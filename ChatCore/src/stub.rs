// A stand-in for `claude`, so the engine's handling of a real process can be tested without a
// single real call. The self-test points `VRCHATCORE_CLAUDE` at this executable, the engine starts
// it with the flags it gives the real one, and `main` sends `-p` here.
//
// It is not a general fake. It understands the words `selftest.rs` says to it and nothing else,
// and an unknown word gets the same answer as "ok".

use crate::voice;
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::process::ExitCode;

/// A file the stub appends one line to when it starts: its arguments, its working directory and how
/// many entries that directory holds, and whether the variable that holds the speech key is in its
/// environment. The only way a test can see what the engine started.
pub const LOG_ENV: &str = "VRCHATCORE_STUB_LOG";

pub fn run() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().unwrap_or_default();
    let entries = std::fs::read_dir(&cwd).map(|d| d.count()).unwrap_or(usize::MAX);
    if let Some(path) = std::env::var_os(LOG_ENV) {
        let sees_key = std::env::var_os(voice::KEY_ENV_ENV).and_then(|name| name.to_str().and_then(std::env::var_os)).is_some();
        let line = json!({ "pid": std::process::id(), "args": args, "cwd": cwd, "entries": entries, "sees_key": sees_key });
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{line}");
        }
    }

    let model = args.iter().position(|a| a == "--model").and_then(|i| args.get(i + 1)).cloned().unwrap_or_default();
    send(&json!({ "type": "system", "subtype": "init", "cwd": cwd, "model": model }));

    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
        let text = message["message"]["content"][0]["text"].as_str().unwrap_or("").to_string();
        // Something that is not the answer comes first, as it does from the real thing.
        send(&json!({ "type": "assistant", "message": { "content": [] } }));
        match text.as_str() {
            "die" => {
                // A lot of stderr first, so the last line is still being read when the exit is
                // seen: the last line is the one that explains the death.
                for n in 0..300 {
                    eprintln!("noise {n} {}", "x".repeat(1000));
                }
                eprintln!("boom: the stub died on purpose");
                return ExitCode::from(3);
            }
            "garbage" => println!("this is not json"),
            // Never answers, and does not leave when its input closes. Only being killed ends it.
            "hang" => loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            },
            "noise" => {
                for n in 0..300 {
                    eprintln!("noise {n} {}", "x".repeat(1000));
                }
                for n in 0..40 {
                    send(&json!({ "type": "rate_limit_event", "n": n }));
                }
                send(&success(&clauses(&[("吵歸吵", "none")])));
            }
            "bye" => {
                send(&success(&clauses(&[("再見", "nod")])));
                return ExitCode::SUCCESS;
            }
            "bracket" => send(&success(&clauses(&[("好的", "none"), ("[happy] hi", "none")]))),
            "badgesture" => send(&success(&clauses(&[("好的", "none"), ("hi", "happy")]))),
            "empty" => send(&success(&clauses(&[]))),
            "notjson" => send(&success("this is not json")),
            "array" => send(&success("[1, 2]")),
            "error" => send(&json!({
                "type": "result", "subtype": "error_during_execution", "is_error": true, "result": "the stub failed on purpose",
            })),
            "many" => send(&success(&clauses(&[("一", "nod"), ("二", "none"), ("三", "nod"), ("四", "lookAway")]))),
            // Long enough that the starting pace cuts it into parts.
            "long" => send(&success(&clauses(&[
                ("好熱喔，我懂那種不想動的感覺。", "selfRef"),
                ("要不要先待在家吹冷氣，喝點冰的，", "offer"),
                ("等傍晚涼一點再出去走走？", "nod"),
            ]))),
            other => match other.strip_prefix("echo:") {
                Some(said) => send(&success(&clauses(&[(said, "none")]))),
                None => send(&success(&clauses(&[("收到", "nod"), ("我在這裡", "offer")]))),
            },
        }
    }
    ExitCode::SUCCESS
}

fn clauses(list: &[(&str, &str)]) -> String {
    let clauses: Vec<Value> = list.iter().map(|(text, gesture)| json!({ "text": text, "gesture": gesture })).collect();
    json!({ "clauses": clauses, "language": "zh-TW", "affect": "calm" }).to_string()
}

/// The `result` message of a good turn. The answer is a string that holds JSON, as it is from the
/// real thing.
fn success(result: &str) -> Value {
    json!({ "type": "result", "subtype": "success", "is_error": false, "result": result })
}

fn send(value: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}
