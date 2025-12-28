use dotenv::dotenv;
use reqwest::Client;
use serde_json::json;
use std::env;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::thread;
use tauri::{command, Emitter, Window};

// Gurobiのログからノイズを消す関数（そのまま）
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

// ★大幅変更: Windowを受け取り、イベントを発火しながら実行する
#[command]
async fn run_optimization(
    window: Window, // これでフロントエンドにイベントを送れます
    script_path: String,
    args_str: String,
) -> Result<String, String> {
    println!("実行: {} Args: [{}]", script_path, args_str);

    let mut cmd_args = vec!["/C", "uv", "run", "python", "-u"]; // ★ -u (Unbuffered) が重要！
    cmd_args.push(&script_path);

    // args_str の寿命問題回避のため、分割したStringをベクターにする
    let split_args: Vec<String> = args_str.split_whitespace().map(String::from).collect();
    for arg in &split_args {
        cmd_args.push(arg);
    }

    let mut child = Command::new("cmd")
        .args(&cmd_args)
        .stdout(Stdio::piped()) // パイプをつなぐ
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("コマンド起動エラー: {}", e))?;

    let stdout = child.stdout.take().ok_or("stdoutの取得に失敗")?;
    let stderr = child.stderr.take().ok_or("stderrの取得に失敗")?;

    // 標準出力(stdout)を読み取るスレッド
    let window_clone = window.clone();
    let stdout_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut full_log = String::new();
        for line in reader.lines() {
            if let Ok(l) = line {
                // 画面に1行ずつ送信イベントを送る
                let _ = window_clone.emit("log-output", &l);
                full_log.push_str(&l);
                full_log.push('\n');
            }
        }
        full_log
    });

    // 標準エラー(stderr)を読み取るスレッド
    let window_clone2 = window.clone();
    let stderr_handle = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        let mut full_err = String::new();
        for line in reader.lines() {
            if let Ok(l) = line {
                let _ = window_clone2.emit("log-output", &l); // エラーも同じログとして流す
                full_err.push_str(&l);
                full_err.push('\n');
            }
        }
        full_err
    });

    // プロセスの終了を待つ
    let status = child
        .wait()
        .map_err(|e| format!("プロセス待機エラー: {}", e))?;

    // スレッドの合流（全ログ回収）
    let full_stdout = stdout_handle.join().unwrap_or_default();
    let full_stderr = stderr_handle.join().unwrap_or_default();

    if status.success() {
        // 最終的なきれいなログを返す（AI解析用）
        Ok(clean_gurobi_log(&full_stdout))
    } else {
        Err(format!(
            "Exit Code: {:?}\nError:\n{}",
            status.code(),
            full_stderr
        ))
    }
}

// analyze_log は変更なし（そのまま）
#[command]
async fn analyze_log(log: String, focus_point: String) -> Result<String, String> {
    dotenv().ok();
    let api_key = env::var("GOOGLE_API_KEY")
        .map_err(|_| "環境変数 GOOGLE_API_KEY が見つかりません。".to_string())?;

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

    let user_forcus =
        "特に、結果のサマリと、計算プロセスの健全性についてコメントしてください。".to_string();
    if !focus_point.trim().is_empty() {
        user_forcus.push_str(
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

    let body = json!({
        "contents": [{ "parts": [{"text": prompt}] }]
    });

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
        .invoke_handler(tauri::generate_handler![run_optimization, analyze_log])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
