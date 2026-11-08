//! The wire, and the only code in this process that writes to stdout (Docs/DESIGN_STT.md 6.1).
//! JSON lines both ways. The host on the other end is the pet today; nothing here knows that.
//!
//! In, one object per line on stdin: `{"op":"press"}`, `{"op":"release"}`, `{"op":"cancel"}`,
//! `{"op":"shutdown"}`.
//!
//! Out, one object per line on stdout: `ready`, `capture_started`, `transcript`, `nothing`, `error`.
//! The exact shapes are the functions below, and `wire` in the self-test pins them to literals.

use std::io::Write;

use serde_json::{Value, json};
use stt_core::audio::CaptureFormat;
use stt_core::engine::{Command, Event, FailureKind, Timings, Why};

pub const VERSION: u32 = 1;

pub fn parse_command(line: &[u8]) -> Result<Command, String> {
    let text = std::str::from_utf8(line).map_err(|e| format!("not UTF-8: {e}"))?;
    let v: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    match v.get("op").and_then(Value::as_str) {
        Some("press") => Ok(Command::Press),
        Some("release") => Ok(Command::Release),
        Some("cancel") => Ok(Command::Cancel),
        Some("shutdown") => Ok(Command::Shutdown),
        Some(other) => Err(format!("unknown op {other:?}")),
        None => Err("no string `op`".to_string()),
    }
}

pub fn ready(model: &str, vad: &str, capture: &Result<CaptureFormat, String>, load_ms: u64) -> Value {
    let mut v = json!({ "ev": "ready", "version": VERSION, "model": model, "vad": vad, "load_ms": load_ms });
    match capture {
        Ok(c) => v["capture"] = capture_json(c),
        Err(why) => {
            v["capture"] = Value::Null;
            v["capture_error"] = json!(why);
        }
    }
    v
}

pub fn capture_json(c: &CaptureFormat) -> Value {
    json!({
        "device": c.device,
        "native_rate": c.native.rate,
        "native_channels": c.native.channels,
        "resampled": c.resampled,
    })
}

pub fn event(e: &Event) -> Value {
    match e {
        Event::CaptureStarted { session } => json!({ "ev": "capture_started", "session": session }),
        Event::Transcript { session, seq, text, timings } => json!({
            "ev": "transcript",
            "session": session,
            "seq": seq,
            "text": text.as_str(),
            "timings": timings_json(timings),
        }),
        Event::Nothing { session, why, timings } => json!({
            "ev": "nothing",
            "session": session,
            "why": why_name(*why),
            "timings": timings_json(timings),
        }),
        Event::Failed { session, kind, detail } => error(*session, kind_name(*kind), detail),
    }
}

pub fn error(session: Option<u32>, kind: &str, message: &str) -> Value {
    json!({ "ev": "error", "session": session, "kind": kind, "message": message })
}

pub fn why_name(why: Why) -> &'static str {
    match why {
        Why::NoSpeech => "no_speech",
        Why::TooShort => "too_short",
        Why::NoWords => "no_words",
        Why::Cancelled => "cancelled",
        Why::DeviceLost => "device_lost",
    }
}

fn kind_name(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::Device => "device",
        FailureKind::Recognizer => "recognizer",
    }
}

fn timings_json(t: &Timings) -> Value {
    json!({
        "open_ms": t.open_ms,
        "held_ms": t.held_ms,
        "speech_ms": t.speech_ms,
        "recognize_ms": t.recognize_ms,
        "release_to_text_ms": t.release_to_text_ms,
        "dropped_samples": t.dropped_samples,
        "glitches": t.glitches,
    })
}

/// One line on stdout, flushed, so the host reads it the moment it is written. A failed write is a
/// host that went away, and there is nobody left to tell.
pub fn send(v: &Value) {
    line(&v.to_string());
}

pub fn line(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}
