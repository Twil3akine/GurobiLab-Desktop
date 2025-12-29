use regex::Regex;
use reqwest::Client;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use tauri::{command, Emitter, State, Window};

struct OptimizationState {
    child: Mutex<Option<Child>>,
}

// ユーザー表示用（ノイズ除去のみ、スペースは残す）
fn clean_gurobi_log(raw_log: &str) -> String {
    raw_log
        .lines()
        .filter(|line| {
            !line.contains("Set parameter")
                && !line.contains("Academic license")
                && !line.contains("Gurobi Optimizer version")
                && !line.contains("CPU model")
                && !line.contains("Thread count")
                && !line.contains("Model fingerprint")
        })
        .collect::<Vec<&str>>()
        .join("\n")
}

// JSONの中身を再帰的に探索して、長い配列をカットする関数
fn prune_json_recursively(v: &mut Value) {
    match v {
        Value::Array(arr) => {
            // 配列の長さが「3」を超えていたらカットする
            const MAX_ITEMS: usize = 3;
            if arr.len() > MAX_ITEMS {
                let original_len = arr.len();
                // 最初の3個だけ残す
                arr.truncate(MAX_ITEMS);
                // 末尾に「省略しました」という情報を追加
                arr.push(json!(format!(
                    "... (truncated {} items) ...",
                    original_len - MAX_ITEMS
                )));
            }
            // 配列の中身もさらにチェック（ネスト対応）
            for item in arr {
                prune_json_recursively(item);
            }
        }
        Value::Object(map) => {
            // オブジェクトの場合は、すべての値をチェック
            for (_, val) in map {
                prune_json_recursively(val);
            }
        }
        _ => {} // 数値、文字列、Booleanは何もしない（そのまま残す）
    }
}

// ログの間引き機能を追加した圧縮関数
fn compress_log_for_ai(full_log: &str) -> String {
    let parts: Vec<&str> = full_log.split("---JSON_START---").collect();

    let mut log_part = parts[0].to_string();
    let mut json_part = String::new();

    if parts.len() > 1 {
        let raw_json = parts[1].split("---JSON_END---").next().unwrap_or("{}");
        if let Ok(mut parsed) = serde_json::from_str::<Value>(raw_json) {
            prune_json_recursively(&mut parsed);
            json_part = parsed.to_string();
        } else {
            json_part = raw_json.to_string();
        }
    }

    // 1. まずスペースを詰める
    let re = Regex::new(r" +").unwrap();
    log_part = re.replace_all(&log_part, " ").to_string();

    // 2. 行ごとの間引き処理 (Sampling)
    let mut numeric_row_count = 0;

    log_part = log_part
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return false;
            }

            // 最初の文字を確認
            let first_char = trimmed.chars().next().unwrap();

            // 条件分岐
            if first_char.is_ascii_digit() {
                // 数字で始まる行（通常のログ行）
                numeric_row_count += 1;
                // 最初の15行、以降は15行おきに残す
                if numeric_row_count < 15 || numeric_row_count % 15 == 0 {
                    return true;
                }
                return false; // それ以外は捨てる
            } else {
                // 'H' (Heuristic), '*' (New solution), 文字列ヘッダーなどは全て残す
                return true;
            }
        })
        .collect::<Vec<&str>>()
        .join("\n");

    if json_part.is_empty() {
        log_part
    } else {
        format!("{}\n[JSON_DATA]:{}", log_part, json_part)
    }
}

// ★修正: コマンド実行部分（cmdのハードコードを廃止、stdinを閉じる処理を追加）
#[command]
async fn run_optimization(
    window: Window,
    _state: State<'_, OptimizationState>,
    script_path: String,
    args_str: String,
    command_prefix: String,
) -> Result<String, String> {
    println!(
        "実行: {} Args: [{}] Prefix: [{}]",
        script_path, args_str, command_prefix
    );

    // 1. プレフィックスを空白で分割
    let mut parts = command_prefix.split_whitespace();

    // 2. 最初の単語をプログラム名として取得 (例: "uv" や "python")
    let program = parts.next().ok_or("Command prefix is empty")?;

    // 3. 残りの単語を引数として収集
    let mut cmd_args: Vec<&str> = parts.collect();

    // 4. スクリプトパスを追加
    cmd_args.push(&script_path);

    // 5. ユーザー引数を追加
    for arg in args_str.split_whitespace() {
        cmd_args.push(arg);
    }

    // ★重要: program変数を使い、stdinをnullにする
    let mut child = Command::new(program)
        .args(&cmd_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null()) // ★追加: 入力待ちフリーズを防止
        .spawn()
        .map_err(|e| {
            format!(
                "コマンド起動エラー: {}\n(設定のCommand Prefixを確認してください)",
                e
            )
        })?;

    let stdout = child.stdout.take().ok_or("stdout取得失敗")?;
    let stderr = child.stderr.take().ok_or("stderr取得失敗")?;

    let window_clone = window.clone();
    let stdout_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut full_log = String::new();
        for line in reader.lines() {
            if let Ok(l) = line {
                let _ = window_clone.emit("log-output", &l);
                full_log.push_str(&l);
                full_log.push('\n');
            }
        }
        full_log
    });

    let window_clone2 = window.clone();
    let stderr_handle = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        let mut full_err = String::new();
        for line in reader.lines() {
            if let Ok(l) = line {
                let _ = window_clone2.emit("log-output", &l);
                full_err.push_str(&l);
                full_err.push('\n');
            }
        }
        full_err
    });

    let pid = child.id();
    window.emit("process-pid", pid).unwrap_or(());

    let status = child.wait().map_err(|e| format!("{}", e))?;

    let full_stdout = stdout_handle.join().unwrap_or_default();
    let full_stderr = stderr_handle.join().unwrap_or_default();

    if status.success() {
        Ok(clean_gurobi_log(&full_stdout))
    } else {
        Err(format!("Exit Code: {:?}\n{}", status.code(), full_stderr))
    }
}

#[command]
fn kill_process(pid: u32) -> Result<(), String> {
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F", "/T"])
        .output()
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ★修正: 引数を整理 (system_instruction と focus_point を正しく受け取る)
fn build_prompt_string(log: &str, focus_point: &str, system_instruction: &str) -> String {
    // 1. 強力圧縮
    let compressed_log = compress_log_for_ai(log);

    // 2. 長さ制限
    let final_log = if compressed_log.len() > 15000 {
        format!(
            "... (snip) ...\n{}",
            &compressed_log[compressed_log.len() - 15000..]
        )
    } else {
        compressed_log
    };

    // 3. 設定されたシステム指示を使用
    // 設定が空ならデフォルトを使用する安全策
    let base_prompt = if system_instruction.trim().is_empty() {
        "あなたはデータサイエンティストです。以下の最適化計算ログを解析し、Markdown形式のレポートを作成してください。\n# 制約\n- 挨拶や前置きは不可。即座に見出し(#)から開始すること。\n- ログの引用は不可。"
    } else {
        system_instruction
    };

    let mut user_focus = "特に、結果サマリと計算プロセスの健全性について記述すること。".to_string();

    if !focus_point.trim().is_empty() {
        user_focus
            .push_str(format!("追加指示:「{}」について深く考察すること。", focus_point).as_str());
    }

    // 4. 結合
    format!("{}\n{}\n[LOG]\n{}", base_prompt, user_focus, final_log)
}

// デバッグ用コマンド
// ★注意: Svelte側が system_instruction を送っていない場合のためにデフォルト値で対応
#[command]
fn debug_prompt(log: String, focus_point: String) -> String {
    // プレビュー用にデフォルトのシステム指示を使用
    let default_system = "あなたはデータサイエンティストです。(以下略...)";
    let prompt = build_prompt_string(&log, &focus_point, default_system);
    prompt
}

#[command]
async fn analyze_log(
    log: String,
    focus_point: String,
    api_key: String,
    model_name: String,
    system_instruction: String, // ←これを受け取る
) -> Result<String, String> {
    if api_key.is_empty() {
        return Err("APIキーが設定されていません。".to_string());
    }

    // ★修正: 引数の順番と渡し方を正しく
    let prompt = build_prompt_string(&log, &focus_point, &system_instruction);

    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        model_name, api_key
    );

    let client = Client::new();
    let body = json!({ "contents": [{ "parts": [{"text": prompt}] }] });

    let res = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let res_text = res.text().await.map_err(|e| e.to_string())?;

    // エラーハンドリング強化
    let json: serde_json::Value = serde_json::from_str(&res_text)
        .map_err(|_| format!("Google API returned invalid JSON: {}", res_text))?;

    if let Some(content) = json["candidates"][0]["content"]["parts"][0]["text"].as_str() {
        Ok(content.to_string())
    } else {
        // エラー詳細を表示
        Err(format!("API Error: {}", res_text))
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(OptimizationState {
            child: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            run_optimization,
            analyze_log,
            kill_process,
            debug_prompt
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
