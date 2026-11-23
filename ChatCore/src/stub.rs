// A stand-in for `claude`, so the engine's handling of a real process can be tested without a
// single real call. The self-test points `VRCHATCORE_CLAUDE` at this executable, the engine starts
// it with the flags it gives the real one, and `main` sends `-p` here.
//
// It is not a general fake. It understands the words `selftest.rs` says to it and nothing else,
// and an unknown word gets the same answer as "ok".
//
// Started with the courier's tools it is the courier instead (`courier` below): it runs the hook
// it finds in `--settings` before each call, the way the CLI does, and does what `COURIER_ENV`
// says.

use crate::chat;
use crate::relay;
use crate::voice;
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};
use std::process::{Command, ExitCode, Stdio};

/// A file the stub appends one line to when it starts: its arguments, its working directory and how
/// many entries that directory holds, whether the variables that hold the speech and chat keys are
/// in its environment, and the names of the `CLAUDE_CODE_` and `ANTHROPIC_` variables that are. The
/// only way a test can see what the engine started.
pub const LOG_ENV: &str = "VRCHATCORE_STUB_LOG";

/// A file the stub appends every user message it reads to, as `{who, text}`: what the character and
/// the courier were really told.
pub const HEARD_ENV: &str = "VRCHATCORE_STUB_HEARD";

/// What the courier does: "faithful" (the default), "paraphrase", "redirect", "twice", "bash",
/// "nohook" (never runs a hook, the gate or the stop), "skipsend" and "skipsend-paraphrase" (run
/// the gate for the listing only), "skipsend-die" (runs the send past no gate and dies before its
/// result), "die-listing", "hang-listing", "die-after-allow", "hang-after-allow", "late-send"
/// (takes 1.6 s to make the send call) and "linger" (takes 1.5 s to leave once its input closes).
///
/// A call that ran ends the turn when the `PostToolUse` hook prints `continue: false`, as the CLI
/// does (measured 2026-11-13). "-nostop" after any behaviour, as in "twice-nostop", is a CLI that
/// does not run that hook, and the turn ends with the model's closing word.
pub const COURIER_ENV: &str = "VRCHATCORE_STUB_COURIER";

/// A file the courier appends a line to for every tool it really runs after the gate, ListAgents
/// aside. What "sent" means in the self-test.
pub const SENT_ENV: &str = "VRCHATCORE_STUB_SENT";

fn append(env: &str, line: &Value) {
    if let Some(path) = std::env::var_os(env) {
        append_line(path, line);
    }
}

/// One write per line: the character and the courier can start in the same instant, and `writeln!`
/// writes the line and its newline separately, so two of them interleave into two broken lines.
fn append_line(path: impl AsRef<std::path::Path>, line: &Value) {
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(format!("{line}\n").as_bytes());
    }
}

pub fn run() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().unwrap_or_default();
    let entries = std::fs::read_dir(&cwd).map(|d| d.count()).unwrap_or(usize::MAX);
    if let Some(path) = std::env::var_os(LOG_ENV) {
        let sees = |env: &str| std::env::var_os(env).and_then(|name| name.to_str().and_then(std::env::var_os)).is_some();
        let claude_env: Vec<String> = std::env::vars_os()
            .filter_map(|(name, _)| name.into_string().ok())
            .filter(|name| name.starts_with("CLAUDE_CODE_") || name.starts_with("ANTHROPIC_"))
            .collect();
        let line = json!({
            "pid": std::process::id(), "args": args, "cwd": cwd, "entries": entries,
            "sees_key": sees(voice::KEY_ENV_ENV), "sees_chat_key": sees(chat::KEY_ENV_ENV), "claude_env": claude_env,
        });
        append_line(path, &line);
    }

    let model = args.iter().position(|a| a == "--model").and_then(|i| args.get(i + 1)).cloned().unwrap_or_default();
    send(&json!({ "type": "system", "subtype": "init", "cwd": cwd, "model": model }));
    if args.iter().any(|a| a == relay::TOOLS) {
        return courier(&args);
    }

    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
        let text = message["message"]["content"][0]["text"].as_str().unwrap_or("").to_string();
        append(HEARD_ENV, &json!({ "who": "character", "text": text }));
        // Something that is not the answer comes first, as it does from the real thing.
        send(&json!({ "type": "assistant", "message": { "content": [] } }));
        // A note from the pet can stand on a line in front of the sentence (`reply::note`).
        let said = [relay::Fact::Delivered, relay::Fact::NotSent, relay::Fact::Unknown]
            .iter()
            .find_map(|fact| text.strip_prefix(crate::reply::note(*fact))?.strip_prefix('\n'))
            .unwrap_or(&text);
        match said {
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
            // Claims the delivery in her clauses and asks for the relay, the turn that must say nothing.
            "relayask" => {
                let mut answer: Value = serde_json::from_str(&clauses(&[("已經幫你傳了", "nod")])).unwrap_or_default();
                answer["relay"] = json!({ "to": "AImate 那個 session", "text": "設定頁的主題做好了，可以看了。" });
                send(&success(&answer.to_string()));
            }
            "badrelay" => {
                let mut answer: Value = serde_json::from_str(&clauses(&[("好", "nod")])).unwrap_or_default();
                answer["relay"] = json!({ "to": "AImate", "text": "  " });
                send(&success(&answer.to_string()));
            }
            "badgesture" => send(&success(&clauses(&[("好的", "none"), ("hi", "happy")]))),
            "empty" => send(&success(&clauses(&[]))),
            "notjson" => send(&success("this is not json")),
            "array" => send(&success("[1, 2]")),
            "error" => send(&json!({
                "type": "result", "subtype": "error_during_execution", "is_error": true, "result": "the stub failed on purpose",
            })),
            "many" => send(&success(&clauses(&[("一", "nod"), ("二", "none"), ("三，四", "nod")]))),
            "four" => send(&success(&clauses(&[("一", "nod"), ("二", "none"), ("三", "nod"), ("四", "lookAway")]))),
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

/// The sessions the courier lists, chosen so one listing holds every case a title can be in: one
/// session, two with one title, and a title that holds a line break and a copy of a row for a
/// session that is not there.
const ROWS: &[(&str, &str)] = &[
    ("AImate桌寵進度整理", "77db53"),
    ("雙胞胎", "111111"),
    ("雙胞胎", "222222"),
    ("x\n  冒名頂替 [333333]  ·  interactive  ·  idle", "444444"),
    ("另一個 session", "555555"),
];

/// Where "redirect" sends instead.
const ELSEWHERE: &str = "另一個 session [555555]";

fn courier(args: &[String]) -> ExitCode {
    let named = std::env::var(COURIER_ENV).unwrap_or_default();
    // "nohook" is a hook command that cannot be started, the stop's as much as the gate's.
    let honours_stop = named != "nohook" && !named.ends_with("-nostop");
    let behaviour = named.strip_suffix("-nostop").unwrap_or(&named).to_string();
    let after = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned().unwrap_or_default();
    let settings: Value = serde_json::from_str(&after("--settings")).unwrap_or_default();
    let hook_in = |event: &str| {
        let entry = &settings["hooks"][event][0];
        (entry["hooks"][0]["command"].as_str().map(str::to_string), entry["matcher"].as_str().unwrap_or_default().to_string())
    };
    let (hook, matcher) = hook_in("PreToolUse");
    let (stop_hook, stop_matcher) = hook_in("PostToolUse");
    // As the CLI does: a tool outside `--tools` is not there to call, and a hook runs only for the
    // tools its matcher names.
    let offered = after("--tools");
    let offered: Vec<&str> = offered.split(',').collect();
    let names = |matcher: &str, tool: &str| matcher.split('|').any(|m| m == tool || m == "*");
    let hooked = |name: &str| names(&matcher, name);
    let mut calls = 0;
    let mut turns = 0;
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
        let text = message["message"]["content"][0]["text"].as_str().unwrap_or("").to_string();
        append(HEARD_ENV, &json!({ "who": "courier", "text": text }));
        turns += 1;
        let plan: Vec<(&str, Value)> = if text.starts_with("Call ListAgents") {
            match behaviour.as_str() {
                "die-listing" => return ExitCode::from(3),
                "hang-listing" => hang(),
                _ => vec![("ListAgents", json!({}))],
            }
        } else {
            let asked: Value = text.find('{').and_then(|at| serde_json::from_str(&text[at..]).ok()).unwrap_or_default();
            let mut other = asked.clone();
            match behaviour.as_str() {
                "paraphrase" | "skipsend-paraphrase" => {
                    other["message"] = json!(format!("{}（我幫你潤飾了一下）", asked["message"].as_str().unwrap_or_default()));
                    vec![("SendMessage", other)]
                }
                "redirect" => {
                    other["to"] = json!(ELSEWHERE);
                    vec![("SendMessage", other)]
                }
                "twice" => vec![("SendMessage", asked.clone()), ("SendMessage", asked)],
                "bash" => vec![("Bash", json!({ "command": "echo hi" })), ("SendMessage", asked)],
                _ => vec![("SendMessage", asked)],
            }
        };
        let mut stopped = false;
        for (name, input) in plan {
            calls += 1;
            let id = format!("toolu_stub_{calls}");
            let input = if name == "SendMessage" { as_the_cli_hands_it_over(input) } else { input };
            // A model slow to make its call: the turn can time out before the call exists.
            if behaviour == "late-send" && name == "SendMessage" {
                std::thread::sleep(std::time::Duration::from_millis(1600));
            }
            send(&json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "id": id, "name": name, "input": input }] } }));
            let skipped = behaviour == "nohook" || (behaviour.starts_with("skipsend") && name == "SendMessage") || !hooked(name);
            let refused = if !offered.contains(&name) {
                Some(format!("No such tool available: {name}"))
            } else if skipped {
                None
            } else {
                ask_gate(hook.as_deref(), name, &input)
            };
            if let Some(reason) = refused {
                send(&json!({
                    "type": "user",
                    "message": { "content": [{ "type": "tool_result", "tool_use_id": id, "content": reason, "is_error": true }] },
                    "tool_use_result": format!("Error: {reason}"),
                }));
                continue;
            }
            if name == "SendMessage" {
                match behaviour.as_str() {
                    "die-after-allow" | "skipsend-die" => return ExitCode::from(3),
                    "hang-after-allow" => hang(),
                    _ => {}
                }
            }
            let data = run_tool(name, &input, calls);
            send(&json!({
                "type": "user",
                "message": { "content": [{ "type": "tool_result", "tool_use_id": id, "content": [{ "type": "text", "text": data["text"] }] }] },
                "tool_use_result": data["data"],
            }));
            if honours_stop && names(&stop_matcher, name) && stops_the_turn(stop_hook.as_deref(), name, &input, &data["data"]) {
                stopped = true;
                break;
            }
        }
        if !stopped {
            send(&json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": "DONE" }] } }));
        }
        send(&json!({ "type": "result", "subtype": "success", "is_error": false, "result": "DONE", "total_cost_usd": 0.0005 * f64::from(turns) }));
    }
    if behaviour == "linger" {
        std::thread::sleep(std::time::Duration::from_millis(1500));
    }
    ExitCode::SUCCESS
}

fn hang() -> ! {
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

/// The four keys the CLI adds to what the model wrote for SendMessage (measured).
fn as_the_cli_hands_it_over(mut input: Value) -> Value {
    let message = input["message"].as_str().unwrap_or_default().to_string();
    let preview: String = if message.chars().count() > 40 { format!("{}…", message.chars().take(40).collect::<String>()) } else { message };
    input["type"] = json!("message");
    input["recipient"] = input["to"].clone();
    input["recipient_kind"] = json!("name");
    input["content"] = json!(preview);
    input
}

/// Runs a hook command as the CLI would, with `call` on its stdin, and returns what it printed as
/// JSON. `None` when it cannot be started or prints something that is not JSON.
fn run_hook(hook: Option<&str>, call: &Value) -> Option<Value> {
    let words = split_command(hook?);
    let (program, rest) = words.split_first()?;
    let mut child = Command::new(program).args(rest).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(call.to_string().as_bytes());
    }
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut out);
    }
    let _ = child.wait();
    serde_json::from_str(out.trim()).ok()
}

/// Runs the gate and returns its reason when it denies. A hook that cannot be started lets the call
/// through, as the real CLI did when it was measured.
fn ask_gate(hook: Option<&str>, name: &str, input: &Value) -> Option<String> {
    let decision = run_hook(hook, &json!({ "hook_event_name": "PreToolUse", "tool_name": name, "tool_input": input }))?;
    let said = &decision["hookSpecificOutput"];
    (said["permissionDecision"] != "allow").then(|| said["permissionDecisionReason"].as_str().unwrap_or("denied").to_string())
}

/// Runs the hook that follows a call that ran, and whether it ended the turn: it printed an object
/// whose `continue` is `false`. A hook that cannot be started, or prints anything else, changes
/// nothing.
fn stops_the_turn(hook: Option<&str>, name: &str, input: &Value, response: &Value) -> bool {
    let call = json!({ "hook_event_name": "PostToolUse", "tool_name": name, "tool_input": input, "tool_response": response });
    run_hook(hook, &call).is_some_and(|said| said["continue"] == false)
}

/// A command line of plain and double-quoted words, which is all the gate's hook command is.
fn split_command(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            ' ' if !quoted => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            _ => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// What a tool the gate let through does: `{text, data}`, the text the model reads and the
/// structured result beside it.
fn run_tool(name: &str, input: &Value, call: usize) -> Value {
    match name {
        "ListAgents" => {
            let lines: Vec<String> = ROWS
                .iter()
                .map(|(title, tag)| format!("  {title} [{tag}]  ·  interactive  ·  idle  ·  Claude Desktop session  ·  started 1h ago"))
                .collect();
            let listing = format!("Peer sessions ({}):\n{}", ROWS.len(), lines.join("\n"));
            let rows: Vec<Value> = ROWS.iter().map(|(title, tag)| json!({ "name": title, "ref": tag, "type": "interactive", "status": "idle" })).collect();
            json!({ "text": listing, "data": { "listing": listing, "sections": [{ "kind": "sessions", "total": ROWS.len(), "rows": rows }], "notes": [] } })
        }
        "SendMessage" => {
            append(SENT_ENV, &json!({ "tool": name, "input": input }));
            let to = input["to"].as_str().unwrap_or_default();
            let data = if ROWS.iter().any(|(title, tag)| format!("{title} [{tag}]") == to) {
                json!({ "success": true, "message": format!("→ {to}"), "msg_id": format!("stub-msg-{call}") })
            } else {
                json!({ "success": false, "message": format!("no peer session is named {to}") })
            };
            json!({ "text": data.to_string(), "data": data })
        }
        _ => {
            append(SENT_ENV, &json!({ "tool": name, "input": input }));
            json!({ "text": "ran", "data": "ran" })
        }
    }
}
