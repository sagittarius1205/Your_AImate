// vrpet-hook: the alert half of the gatekeeper.
//
// `claude agents --json` reports status 'idle' or 'busy' and nothing else (measured 2026-11-02),
// so it cannot tell a session stopped at a permission prompt from one that has finished. Both
// read 'idle'. This binary is the missing signal. Claude Code runs it on Notification (set) and
// on Stop (clear), and it leaves one small file per waiting session for the pet to read.
//
// It runs inside every one of the user's sessions, so the rules it lives by are:
//
//   - Never slow a session down. It is registered async, it writes one small file, it exits.
//   - Never make noise. It always exits 0 and prints nothing, unless VRPET_HOOK_DEBUG is set.
//     A broken alert is worth less than a session that prints hook errors all day.
//   - Never trust the payload. `session_id` becomes a filename, so it is validated first.
//
// Every failure is a named Err rather than a silent None, because the first version returned
// Option and a pipe test that wrote nothing gave no way at all to find out why.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// One file per session, so twenty sessions writing at once never contend.
fn alerts_dir() -> Result<PathBuf, String> {
    Ok(state_dir()?.join("alerts"))
}

#[path = "../home.rs"]
mod home;

fn state_dir() -> Result<PathBuf, String> {
    home::dir()
}

/// Cap for the notification log, past which it is started over. A hook that runs for months
/// must not grow a file without a bound.
const LOG_MAX_BYTES: u64 = 1_000_000;

/// Append one notification to a log the `clear` path never touches.
///
/// Two reasons it exists. The alert file is deleted the moment the session moves on, so without
/// this there is no way to see what asked for attention after the fact, and the product wants
/// that history anyway. And it is how the payload shape gets measured: Notification's fields
/// beyond session_id are not documented, and the event cannot be triggered headlessly, so the
/// first real permission prompt in ordinary use is what records them.
fn append_log(record: &serde_json::Value) -> Result<(), String> {
    let dir = state_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let path = dir.join("notifications.jsonl");

    // Rotate by renaming, never by truncating an open file. The first version checked the size
    // and then opened with truncate(true), which is a check-then-act race: twenty sessions can
    // all see it oversized and truncate over each other, or one can truncate while another is
    // mid-append. A rename loses the race harmlessly, because whoever loses just appends to the
    // file the winner created.
    if std::fs::metadata(&path).map(|m| m.len() > LOG_MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&path, dir.join("notifications.1.jsonl"));
    }

    // Append only. One write_all of one line, so a concurrent writer cannot interleave within
    // the line; `concurrent` in the test suite is what actually holds this down.
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("opening {}: {e}", path.display()))?;

    let mut line = record.to_string();
    line.push('\n');
    f.write_all(line.as_bytes())
        .map_err(|e| format!("appending {}: {e}", path.display()))
}

/// Claude Code's session ids are UUIDs. Anything else does not become a path.
fn safe_session_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn main() {
    if let Err(e) = run() {
        if std::env::var_os("VRPET_HOOK_DEBUG").is_some() {
            eprintln!("vrpet-hook: {e}");
        }
    }
    // Always exit 0. A hook that fails loudly in every session is worse than no alert.
}

fn run() -> Result<(), String> {
    let mode = std::env::args()
        .nth(1)
        .ok_or("no mode argument; expected 'set' or 'clear'")?;

    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| format!("reading stdin: {e}"))?;

    let payload: serde_json::Value =
        serde_json::from_str(&buf).map_err(|e| format!("parsing stdin as JSON: {e}"))?;

    let session_id = payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or("payload has no string session_id")?;
    if !safe_session_id(session_id) {
        return Err(format!("session_id is not a safe filename: {session_id:?}"));
    }

    let dir = alerts_dir()?;
    let path = dir.join(format!("{session_id}.json"));

    match mode.as_str() {
        "clear" => {
            // The session finished its turn, so whatever it was waiting for is resolved.
            match std::fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("removing {}: {e}", path.display())),
            }
        }
        "set" => {
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("creating {}: {e}", dir.display()))?;

            // `raw` is here on purpose and only for now: the documented stdin fields are
            // session_id, tool_name, tool_input and tool_response, and what Notification
            // actually sends beyond that is not documented. Keeping the payload lets the spike
            // measure the real shape instead of guessing it. Trim this to named fields once
            // Pet/README.md records what the shape is.
            let record = serde_json::json!({
                "session_id": session_id,
                "at": now_millis(),
                "message": payload.get("message").and_then(|m| m.as_str()),
                "cwd": payload.get("cwd").and_then(|c| c.as_str()),
                "raw": payload,
            });

            // Write and rename, so the pet never reads half a file. The temp name carries the
            // pid because the first version did not: two notifications for the same session are
            // two processes writing the same `<id>.json.tmp`, and one would rename a file the
            // other was still writing.
            let tmp = dir.join(format!("{session_id}.{}.json.tmp", std::process::id()));
            {
                let mut f = std::fs::File::create(&tmp)
                    .map_err(|e| format!("creating {}: {e}", tmp.display()))?;
                f.write_all(record.to_string().as_bytes())
                    .map_err(|e| format!("writing {}: {e}", tmp.display()))?;
            }
            std::fs::rename(&tmp, &path)
                .map_err(|e| format!("renaming to {}: {e}", path.display()))?;

            // The alert is what the pet shows; the log is what survives `clear`. A failure here
            // must not lose the alert that already landed, so it is reported, not propagated.
            if let Err(e) = append_log(&record) {
                if std::env::var_os("VRPET_HOOK_DEBUG").is_some() {
                    eprintln!("vrpet-hook: log: {e}");
                }
            }
            Ok(())
        }
        other => Err(format!("unknown mode {other:?}; expected 'set' or 'clear'")),
    }
}
