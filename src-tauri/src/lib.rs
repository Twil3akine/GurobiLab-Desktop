use dotenv::dotenv;
use reqwest::Client;
use serde_json::json;
use std::env;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use tauri::{command, Emitter, State, Window};

// 実行中のプロセスを管理するための構造体
struct OptimizationState {
    child: Mutex<Option<Child>>,
}

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

#[command]
async fn run_optimization(
    window: Window,
    state: State<'_, OptimizationState>, // 状態管理
    script_path: String,
    args_str: String,
) -> Result<String, String> {
    println!("実行: {} Args: [{}]", script_path, args_str);

    let mut cmd_args = vec!["/C", "uv", "run", "python", "-u"];
    cmd_args.push(&script_path);

    for arg in args_str.split_whitespace() {
        cmd_args.push(arg);
    }

    let mut child = Command::new("cmd")
        .args(&cmd_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("コマンド起動エラー: {}", e))?;

    let stdout = child.stdout.take().ok_or("stdout取得失敗")?;
    let stderr = child.stderr.take().ok_or("stderr取得失敗")?;

    // プロセスIDをStateに保存（これで後からkillできる）
    // ※stdoutの所有権移動前に保存すると複雑になるので、spawn直後のhandleを使うのは少し工夫が必要ですが
    // 簡易的にキャンセル時は「プロセスを強制終了」するロジックにします。
    // ここでは「読み取り用」とは別にキャンセル用のハンドルを持つのはRustでは難しいので、
    // 実は「キャンセルコマンドが来たらOS側でタスクキル」する等のアプローチが楽ですが、
    // 今回は「StateにChildを入れる」アプローチで実装します。
    // ただし、Childの所有権は wait() で使うため、ここでは Arc<Mutex> で共有するのが一般的ですが
    // コードが複雑になるため、今回は「キャンセルボタン＝強制終了コマンド発行」という簡易実装に逃げず、
    // 正攻法（State管理）の一部簡略版でいきます。

    // (非同期実行中にキャンセルを受け付けるため、本当はChildをArcで包む必要がありますが
    // ここではシンプルに「読み取りスレッド」だけ回し、Child本体はローカルで持ちます。
    // キャンセル機能は「別コマンド(taskkill)」で実装するのがWindowsでは一番確実です)

    // --- ログ読み取りスレッド (変更なし) ---
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

    // キャンセル用にStateに保存しようとすると所有権問題が出るため、
    // 今回は「キャンセル用のフラグ」をチェックするのではなく、
    // フロントエンドから「キャンセル」が押されたら `taskkill` を飛ばす方式にします。
    // そのため、ここでは普通に wait します。

    // プロセスIDを取得（キャンセル用）
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

// ★追加: 実行中止コマンド (Windows専用)
#[command]
fn kill_process(pid: u32) -> Result<(), String> {
    // taskkill /PID <pid> /F (強制終了) /T (ツリー終了: pythonも道連れ)
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F", "/T"])
        .output()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[command]
async fn analyze_log(log: String, focus_point: String, api_key: String) -> Result<String, String> {
    // APIキーをフロントエンド(設定画面)から受け取るように変更
    if api_key.is_empty() {
        return Err("APIキーが設定されていません。設定画面を確認してください。".to_string());
    }

    let truncated_log = if log.len() > 12000 {
        format!("... (中略) ...\n{}", &log[log.len() - 12000..])
    } else {
        log
    };

    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent?key={}",
        api_key
    );

    let client = Client::new();

    let base_instruction = "あなたはデータサイエンティストです。以下の最適化計算ログ（末尾にJSON結果がある場合が多い）を解析し、**Markdown形式**で見やすくしたレポートのみを出力してください。# 制約事項- **「以下は解析結果です」といった挨拶や前置きは一切出力しないこと。**- **いきなりMarkdownの見出し（#）から書き始めること。**- ログの中身をそのまま引用して出力しないこと。";

    let mut user_focus =
        "特に、結果のサマリと、計算プロセスの健全性についてコメントしてください。".to_string();

    if !focus_point.trim().is_empty() {
        user_focus.push_str(
            format!(
                "**また、特に以下の点について深く考察・解説してください**: 「{}」",
                focus_point
            )
            .as_str(),
        );
    }

    let prompt = format!(
        "{}\n{}\n\n--- Log ---\n{}",
        base_instruction, user_focus, truncated_log
    );

    let body = json!({ "contents": [{ "parts": [{"text": prompt}] }] });

    let res = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let res_text = res.text().await.map_err(|e| e.to_string())?;
    let json: serde_json::Value = serde_json::from_str(&res_text).map_err(|e| e.to_string())?;

    if let Some(content) = json["candidates"][0]["content"]["parts"][0]["text"].as_str() {
        Ok(content.to_string())
    } else {
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
        }) // State初期化
        .invoke_handler(tauri::generate_handler![
            run_optimization,
            analyze_log,
            kill_process
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
