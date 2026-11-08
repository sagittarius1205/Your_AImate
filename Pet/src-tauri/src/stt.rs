//! The speech input service, as the pet sees it (Docs/DESIGN_STT.md). `vrstt run` is one long-lived
//! child process that speaks JSON lines on stdin and stdout; `Stt/host/src/proto.rs` is its wire.
//! This file restates the part of that wire the pet uses instead of depending on the crate, for the
//! reason `core.rs` restates the core's: the service is meant to ship on its own, and the pet is one
//! consumer. `pet_mic` pins the restatement to literal lines.
//!
//! The restatement:
//! - in: `{"op":"press"}` and `{"op":"release"}`, one per line.
//! - out: `{"ev":"ready","model","capture":{"device","native_rate","native_channels","resampled"}}`,
//!   or `"capture":null` with `capture_error`; `{"ev":"capture_started","session"}`;
//!   `{"ev":"transcript","session","seq","text","timings"}`; `{"ev":"nothing","session","why"}`;
//!   `{"ev":"error","session","kind","message"}`, where `kind: "model"` is followed by the exit.
//!
//! Every line goes to the page as the service wrote it. The page owns what a line means for the
//! button and the dialogue; Rust owns the state the page asks for when it loads.
//!
//! Started with `std::process` like the core, so the webview has no way to start a process.

use std::ffi::OsString;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdout, Command, Stdio};
use std::sync::Mutex;

use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager};

use crate::child::{Piped, Reaper};
use crate::core::{each_line, parse_line, sibling_candidates};
use crate::settings;

/// One line the service printed, forwarded as it was written.
pub const EVENT: &str = "stt-event";
/// A change in `SttState`.
pub const STATE_EVENT: &str = "stt-state";
/// Names the service's executable outright, like `VRCHATCORE` for the core.
pub const EXE_ENV: &str = "VRSTT";

// ---------------------------------------------------------------------------------------------
// The pure half

/// Starting, ready, or down with the words the bubble will say. `Ready` with a `device_error` is a
/// service whose models loaded and that found no microphone when it started: a press still tries,
/// because a microphone can be plugged in later, and the press says why if it fails. `Restarting`
/// is the service stopped on purpose to load another pack, until the new one's `ready`.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(tag = "state", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SttState {
    #[default]
    Starting,
    Ready {
        model: String,
        device: Option<String>,
        resampled: Option<bool>,
        device_error: Option<String>,
    },
    Restarting {
        why: String,
    },
    Down {
        why: String,
    },
}

impl SttState {
    /// The state after the service printed `event`, or `None` when the line changes nothing.
    pub fn after(&self, event: &Value) -> Option<SttState> {
        if matches!(self, SttState::Down { .. }) || event.get("ev").and_then(Value::as_str) != Some("ready") {
            return None;
        }
        let text = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_string);
        let next = SttState::Ready {
            model: text(event.get("model")).unwrap_or_else(|| "?".to_string()),
            device: text(event.pointer("/capture/device")),
            resampled: event.pointer("/capture/resampled").and_then(Value::as_bool),
            device_error: text(event.get("capture_error")),
        };
        (next != *self).then_some(next)
    }
}

/// What a model failure says, kept so the exit that follows it can say it too.
pub fn fatal_why(event: &Value) -> Option<String> {
    (event.get("ev").and_then(Value::as_str) == Some("error") && event.get("kind").and_then(Value::as_str) == Some("model"))
        .then(|| {
            let message = event.get("message").and_then(Value::as_str).unwrap_or("它沒說是什麼");
            format!("語音輸入的模型載入失敗：{message}")
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Press,
    Release,
}

pub fn command_line(op: Op) -> String {
    let name = match op {
        Op::Press => "press",
        Op::Release => "release",
    };
    let mut line = json!({ "op": name }).to_string();
    line.push('\n');
    line
}

pub fn candidates(explicit: Option<OsString>, exe_dir: Option<&Path>, prefer_release: bool) -> Vec<PathBuf> {
    sibling_candidates(explicit, exe_dir, prefer_release, "Stt", "vrstt")
}

pub fn locate(tried: &[PathBuf], exists: impl Fn(&Path) -> bool) -> Result<PathBuf, String> {
    tried.iter().find(|p| exists(p)).cloned().ok_or_else(|| {
        let places = tried.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("、");
        format!("找不到語音輸入服務，找過：{places}。先在 Stt 目錄跑 cargo build，或用環境變數 {EXE_ENV} 指到它")
    })
}

pub fn exit_why(code: Option<i32>, fatal: Option<String>) -> String {
    fatal.unwrap_or_else(|| match code {
        Some(code) => format!("語音輸入服務結束了（結束碼 {code}）"),
        None => "語音輸入服務結束了（沒有結束碼）".to_string(),
    })
}

/// Names the pack the service loads (`PACK_ENV` in Stt/host/src/pack.rs).
pub const PACK_ENV: &str = "VRSTT_PACK";

/// A pack the settings window may offer: one the service knows and found installed and whole.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pack {
    pub id: String,
}

/// The packs in `vrstt packs`'s one line that are `ok`, or why there are none to offer. A pack that
/// is not installed or fails its hashes is not offered: chosen, it would leave the microphone down.
pub fn packs_of(line: &[u8]) -> Result<Vec<Pack>, String> {
    let v: Value = serde_json::from_slice(line.trim_ascii()).map_err(|e| format!("vrstt packs 的回答讀不懂：{e}"))?;
    if v.get("ev").and_then(Value::as_str) == Some("error") {
        let message = v.get("message").and_then(Value::as_str).unwrap_or("它沒說是什麼");
        return Err(format!("語音辨識的模型清單讀不到：{message}"));
    }
    let listed = v.get("packs").and_then(Value::as_array).ok_or("vrstt packs 的回答沒有 packs")?;
    let mut ok = Vec::new();
    let mut not = Vec::new();
    for p in listed {
        let id = p.get("id").and_then(Value::as_str).unwrap_or("?");
        if p.get("ok") == Some(&Value::Bool(true)) {
            ok.push(Pack { id: id.to_string() });
        } else {
            not.push(format!("{id}：{}", p.get("why").and_then(Value::as_str).unwrap_or("沒說原因")));
        }
    }
    if ok.is_empty() {
        return Err(format!("沒有裝好的語音辨識模型。{}", not.join("；")));
    }
    Ok(ok)
}

// ---------------------------------------------------------------------------------------------
// The process half

#[derive(Default)]
pub struct Stt {
    state: Mutex<SttState>,
    piped: Piped,
    /// Held for the whole of a restart, for the reason `core::Core` gives.
    restarting: Mutex<()>,
}

impl Stt {
    pub fn state(&self) -> SttState {
        self.state.lock().expect("stt state poisoned").clone()
    }

    pub fn send(&self, op: Op) -> Result<(), String> {
        let mut pipe = self.piped.stdin();
        let Some(stdin) = pipe.as_mut() else {
            drop(pipe);
            return Err(match self.state() {
                SttState::Down { why } | SttState::Restarting { why } => why,
                _ => "語音輸入還沒起來".to_string(),
            });
        };
        stdin
            .write_all(command_line(op).as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("寫不進語音輸入服務：{e}"))
    }
}

fn set_state(app: &AppHandle, next: SttState) {
    *app.state::<Stt>().state.lock().expect("stt state poisoned") = next.clone();
    if let Err(e) = app.emit(STATE_EVENT, &next) {
        eprintln!("emit {STATE_EVENT}: {e}");
    }
}

/// Starts the service. A service that cannot start is `Down` with the reason, and the rest of the
/// pet goes on: typing still works.
pub fn start(app: &AppHandle) {
    if let Err(why) = launch(app) {
        eprintln!("stt: {why}");
        set_state(app, SttState::Down { why });
    }
}

fn exe() -> Result<PathBuf, String> {
    let exe_dir = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf));
    let tried = candidates(std::env::var_os(EXE_ENV), exe_dir.as_deref(), !cfg!(debug_assertions));
    locate(&tried, Path::is_file)
}

fn command(exe: &Path, arg: &str) -> Command {
    let mut cmd = Command::new(exe);
    cmd.arg(arg);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn launch(app: &AppHandle) -> Result<(), String> {
    let exe = exe()?;
    let mut cmd = command(&exe, "run");
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // The pack chosen in the settings, read at each start, which is the only way a change of it
    // reaches the service.
    let pack = app.state::<settings::Store>().current().stt.pack;
    cmd.env(PACK_ENV, &pack);
    eprintln!("stt: starting with pack {pack}");
    let mut child = cmd.spawn().map_err(|e| format!("語音輸入服務 {} 起不來：{e}", exe.display()))?;
    let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        let _ = child.kill();
        return Err("語音輸入服務的管線沒有接上".to_string());
    };
    let reaper = app.state::<Stt>().piped.adopt(stdin, child);

    std::thread::spawn(move || {
        if let Err(e) = each_line(BufReader::new(stderr), |line| eprintln!("stt: {}", String::from_utf8_lossy(line))) {
            eprintln!("stt: reading stderr: {e}");
        }
    });
    let app = app.clone();
    std::thread::spawn(move || read_stdout(&app, stdout, reaper));
    Ok(())
}

/// Stops the service and starts another with the pack in effect now, for the reason and in the way
/// `core::restart` does. A press held at that moment is ended first, so the window lets go of the
/// cursor and the page's button does not wait on a service that is going.
pub fn restart(app: &AppHandle, why: &str) {
    let stt = app.state::<Stt>();
    let _one = stt.restarting.lock().expect("stt restart poisoned");
    app.state::<crate::press::Held>().release();
    set_state(app, SttState::Restarting { why: format!("{why}，語音輸入重新啟動中") });
    let stopped = stt.piped.stop(crate::child::STOP_WAIT);
    eprintln!("stt: restarting ({why}), the old one {stopped:?}");
    start(app);
}

/// The packs the settings window may offer, from `vrstt packs`, which checks each one's files
/// against the hashes it was built with. Seconds and not milliseconds: it reads the whole model.
pub fn packs() -> Result<Vec<Pack>, String> {
    let exe = exe()?;
    let out = command(&exe, "packs").output().map_err(|e| format!("{} packs: {e}", exe.display()))?;
    packs_of(&out.stdout)
}

/// Runs until the service's stdout closes. When the pet dies the OS closes the service's stdin, the
/// service reads end of input, ends any held press and exits.
fn read_stdout(app: &AppHandle, stdout: ChildStdout, reaper: Reaper) {
    let mut fatal = None;
    let read = each_line(BufReader::new(stdout), |line| {
        let event = match parse_line(line) {
            Ok(v) => v,
            Err(why) => {
                eprintln!("stt: bad line: {why}");
                json!({ "ev": "error", "session": null, "kind": "protocol", "message": format!("語音輸入服務送來一行讀不懂的東西：{why}") })
            }
        };
        fatal = fatal.take().or_else(|| fatal_why(&event));
        if let Some(next) = app.state::<Stt>().state().after(&event) {
            set_state(app, next);
        }
        if let Err(e) = app.emit(EVENT, &event) {
            eprintln!("emit {EVENT}: {e}");
        }
    });
    if let Err(e) = read {
        eprintln!("stt: reading stdout: {e}");
    }
    let why = exit_why(reaper.wait(), fatal);
    eprintln!("stt: {why}");
    app.state::<Stt>().piped.end(&reaper, || set_state(app, SttState::Down { why }));
}
