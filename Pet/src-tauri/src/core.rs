//! The conversation core, as the pet sees it (Docs/DESIGN_Pet.md 6.1). The core is one long-lived
//! child process that speaks JSON lines on stdin and stdout, and `ChatCore/src/protocol.rs` is its
//! wire. This file restates that wire instead of depending on it, because that crate also serves
//! the VR side and the pet is not its only consumer. `pet_core` pins the restatement to literal
//! lines, and it is the only thing that does.
//!
//! The restatement, as far as the page reads it:
//! - `{"ev":"ready","version"}` once, first.
//! - `{"ev":"utterance","text","tags","voice","part"}` per part of a turn. An answer comes in parts:
//!   `part` is 0-based within the turn, a turn is part 0, then parts 1.., then `turn_end`, and each
//!   part carries its own `text` (its markers in front of the clauses it opens), its own `tags` and
//!   its own `voice`. A line with no `part` is a turn of one part, as before. If a part is silent,
//!   that part and everything after it come as one silent utterance, so a silent part is always the
//!   last. `voice` is either
//!   `{"state":"voiced","samples","pcm_b64","envelope"}`, 16 kHz mono PCM16 in base64 and one
//!   loudness value per 320 samples, the shape of one entry in `voice/fillers.json`; or
//!   `{"state":"silent","why"}`, a sentence the bubble can show as it is.
//! - `{"ev":"turn_end"}` after each turn, and `{"ev":"error","message"}` when a turn failed.
//!
//! The pet writes three lines: `{"op":"say","text"}`, `{"op":"character","name","persona","voice"?}`
//! once as each core starts and again on each change of character, and `{"op":"greet","text"}`,
//! which the core voices like a turn of one part with no tags.
//!
//! A voiced line is a few hundred kilobytes (about 430 KB of base64 for ten seconds of speech). It
//! is relayed like any other line, whole, with one field added on the way (`for_page`): a voiced
//! utterance gets `plan`, what the body does while it is said, as `{actions, dispositions}`, or
//! `{error}` when the audio could not be read. The text with its `[tag]` markers and the PCM are
//! all the planner needs, so the core sends nothing new for it (Docs/DESIGN_Pet.md 4.1). Each part
//! is planned on its own, from its own text and its own audio, with its `at` counted from its own
//! first sample; the page puts the parts of a turn on one clock. A silent turn has no audio and so
//! no timing, and gets no plan (Docs/DESIGN_Voice.md 3.2).
//!
//! The first half is pure on purpose, like `hit.rs` and `side.rs`: no tauri type and no process
//! appears in it, so `--selftest=pet_core` drives it with byte buffers. The second half is the
//! part that cannot be tested that way, the process and the two threads that read it, and it is
//! kept thin enough to read in one go.
//!
//! The webview has no way to start a process. The core is started here with `std::process`, and
//! the shell plugin is not registered, so there is no permission in `capabilities/default.json`
//! that could be opened too wide.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdout, Command, Stdio};
use std::sync::Mutex;

use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager};
use vrcue::Bindings;

use crate::cast;
use crate::child::{self, Piped, Reaper};
use crate::cue::{plan, profile};
use crate::settings;
use crate::voice;

/// One line the core printed, forwarded to the page exactly as the core wrote it.
pub const EVENT: &str = "core-event";

/// A change in `CoreState`. It is not a core line because the core cannot print that it died.
pub const STATE_EVENT: &str = "core-state";

/// Names the core's executable outright. When it is set it is the only place looked, so a path
/// that is wrong fails loudly instead of quietly running some other build of the core.
pub const EXE_ENV: &str = "VRCHATCORE";

// ---------------------------------------------------------------------------------------------
// The pure half

/// What the pet knows about the core. Three values, because "not up yet" and "cannot come up" are
/// different: the first changes and the second does not, and the user is told different things.
///
/// `Down` is the character unable to talk. The roster, the panel and the hit regions go on working
/// (Docs/DESIGN_Pet.md 1.2), so nothing here is allowed to stop the app.
///
/// `Restarting` is the core stopped on purpose so it can start again with new settings
/// (Docs/DESIGN_Settings.md 4), with the reason the page says while she is quiet. It ends the way
/// `Starting` does, with the new core's `ready`.
///
/// This is also the page's wire: `state` names the variant and the other key is its payload.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum CoreState {
    #[default]
    Starting,
    Ready {
        version: String,
    },
    Restarting {
        why: String,
    },
    Down {
        why: String,
    },
}

impl CoreState {
    /// The state after the core printed `event`, or `None` when the line changes nothing.
    ///
    /// Only `ready` moves anything. `Down` is final for the core that printed it: it is set after
    /// that core's stdout has closed, so no further line can arrive, and the rule is written here
    /// anyway so it does not depend on the order two threads happen to run in. Only a restart,
    /// which starts another core, leaves it (`restart`).
    pub fn after(&self, event: &Value) -> Option<CoreState> {
        if matches!(self, CoreState::Down { .. }) || event.get("ev").and_then(Value::as_str) != Some("ready") {
            return None;
        }
        let version = event.get("version").and_then(Value::as_str).unwrap_or("?");
        let next = CoreState::Ready { version: version.to_string() };
        (next != *self).then_some(next)
    }
}

/// The one line that hands the core a sentence. `to_string` on a `Value` escapes every newline and
/// quote in `text`, so the framing (one object per line) cannot be broken by what the user typed.
pub fn say_line(text: &str) -> String {
    framed(json!({ "op": "say", "text": text }))
}

/// The line that tells the core who she is (Docs/DESIGN_Characters.md 5). `voice` is left out, and
/// not sent as null, when she has no voice of her own: the core reads both as the default voice,
/// and leaving it out is the form that cannot be mistaken for "keep the voice you had".
pub fn character_line(speaking: &cast::Speaking) -> String {
    let mut line = json!({ "op": "character", "name": speaking.profile.name, "persona": speaking.profile.persona });
    if let Some(voice) = &speaking.voice {
        line["voice"] = json!(voice.as_str());
    }
    framed(line)
}

/// The line that has her say `text` in her current voice without asking the model.
pub fn greet_line(text: &str) -> String {
    framed(json!({ "op": "greet", "text": text }))
}

fn framed(line: Value) -> String {
    let mut line = line.to_string();
    line.push('\n');
    line
}

/// One line of the core's stdout as an event. Anything with a string `ev` is an event, including
/// one this file has never heard of: the page decides what to do with it, so a new kind of event
/// is ignored there and not eaten here.
///
/// The payload is the object the core printed, whole. It is not parsed into a type and printed
/// again, which would drop every field this file does not know about.
pub fn parse_line(line: &[u8]) -> Result<Value, String> {
    let text = std::str::from_utf8(line).map_err(|e| format!("it is not valid UTF-8: {e}"))?;
    let value: Value = serde_json::from_str(text).map_err(|e| format!("it is not JSON: {e}"))?;
    match value.get("ev").and_then(Value::as_str) {
        Some(_) => Ok(value),
        None => Err("it is not an event, which has a string `ev`".to_string()),
    }
}

/// The event as the page receives it. A voiced utterance gains `plan`; every other line is the line.
///
/// The plan is made here and not on the page because the page does not arbitrate
/// (Docs/DESIGN_Pet.md 4): it gets the actions already scheduled, resolved for this model and clear
/// of each other, and the dispositions that say what happened to every cue that did not play.
pub fn for_page(mut event: Value, bindings: &Bindings) -> Value {
    let voiced = event.get("ev").and_then(Value::as_str) == Some("utterance")
        && event.pointer("/voice/state").and_then(Value::as_str) == Some("voiced");
    if voiced {
        event["plan"] = match planned(&event, bindings) {
            Ok(plan) => json!(plan),
            Err(why) => json!({ "error": why }),
        };
    }
    event
}

fn planned(event: &Value, bindings: &Bindings) -> Result<plan::Plan, String> {
    let text = event.get("text").and_then(Value::as_str).ok_or("the utterance has no text")?;
    let b64 = event.pointer("/voice/pcm_b64").and_then(Value::as_str).ok_or("the voice has no pcm_b64")?;
    let bytes = crate::base64_decode(b64).map_err(|why| format!("the voice's pcm_b64 is not base64: {why}"))?;
    let samples: Vec<f32> =
        bytes.chunks_exact(2).map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32768.0).collect();
    // The text is the model's, so the planner sees input nobody chose. A panic here would end the
    // thread that reads the core and the character would stop answering without a word; caught, the
    // sentence still plays, without gestures, and the plan says why.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| plan::plan(text, &samples, voice::SAMPLE_RATE, bindings)))
        .map_err(|_| "the gesture planner panicked on this sentence, so it plays without gestures".to_string())
}

/// What the page is told when the core printed a line that is not an event. The same shape as the
/// core's own `error`, so the page needs no second way to say it. Dropping the line quietly would
/// leave a user who just spoke with no answer and no reason (Docs/DESIGN_Pet.md 6.1).
pub fn bad_line_event(why: &str) -> Value {
    json!({ "ev": "error", "message": format!("核心送來一行讀不懂的東西：{why}") })
}

/// Calls `on` with each non-blank line of `input`, without its terminator, until the input ends.
///
/// Bytes and not `lines()`: that iterator stops at the first line that is not UTF-8, and a reader
/// that stops is a pipe nobody drains, which stalls the core on its next write.
pub fn each_line(mut input: impl BufRead, mut on: impl FnMut(&[u8])) -> std::io::Result<()> {
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if input.read_until(b'\n', &mut buf)? == 0 {
            return Ok(());
        }
        let line = buf.trim_ascii();
        if !line.is_empty() {
            on(line);
        }
    }
}

/// Reads the core's stdout to its end, handing over each line as an event or as the reason it is
/// not one. A bad line is one more item and not the end of the loop: a core with a bug must not be
/// able to silence itself by printing something odd. `Err` is only for a broken pipe.
pub fn pump(input: impl BufRead, mut on: impl FnMut(Result<Value, String>)) -> std::io::Result<()> {
    each_line(input, |line| on(parse_line(line)))
}

/// Where to look for the core, best first.
///
/// Next to the pet comes first, which is where a packaged app keeps it. Then the build output of
/// the sibling crate: the pet is `Pet/src-tauri/target/<profile>/vrpet.exe`, so the repo root is
/// four levels above its directory. A debug pet prefers the debug core and a release pet the
/// release one, so a stale build of the other kind is only a fallback.
pub fn candidates(explicit: Option<OsString>, exe_dir: Option<&Path>, prefer_release: bool) -> Vec<PathBuf> {
    sibling_candidates(explicit, exe_dir, prefer_release, "ChatCore", "vrchatcore")
}

/// The same search for any sibling process this repo builds: `workspace` is its directory at the
/// repo root and `stem` its executable's name. The speech input service is found this way too.
pub fn sibling_candidates(
    explicit: Option<OsString>,
    exe_dir: Option<&Path>,
    prefer_release: bool,
    workspace: &str,
    stem: &str,
) -> Vec<PathBuf> {
    if let Some(path) = explicit.filter(|p| !p.is_empty()) {
        return vec![PathBuf::from(path)];
    }
    let Some(dir) = exe_dir else {
        return Vec::new();
    };
    let file = format!("{stem}{}", std::env::consts::EXE_SUFFIX);
    let mut found = vec![dir.join(&file)];
    if let Some(root) = dir.ancestors().nth(4) {
        let profiles = if prefer_release { ["release", "debug"] } else { ["debug", "release"] };
        for profile in profiles {
            found.push(root.join(workspace).join("target").join(profile).join(&file));
        }
    }
    found
}

/// The first candidate that exists, or a message that names every place that was tried. Taking
/// `exists` as a parameter is what lets a test say which paths exist without touching the disk.
pub fn locate(tried: &[PathBuf], exists: impl Fn(&Path) -> bool) -> Result<PathBuf, String> {
    tried.iter().find(|p| exists(p)).cloned().ok_or_else(|| {
        let places = if tried.is_empty() {
            "（沒有任何位置可找）".to_string()
        } else {
            tried.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("、")
        };
        format!("找不到對話核心，找過：{places}。先在 ChatCore 目錄跑 cargo build，或用環境變數 {EXE_ENV} 指到它")
    })
}

/// The model `claude` runs on (ChatCore/src/serve.rs).
pub const MODEL_ENV: &str = "VRCHATCORE_MODEL";
/// Her voice service (ChatCore/src/voice.rs). `KEY_ENV` carries the name of the variable that holds
/// the key and never the key: the core reads the value itself, from the environment it inherits.
pub const SPEECH_URL_ENV: &str = "VRCHATCORE_SPEECH_URL";
pub const SPEECH_MODEL_ENV: &str = "VRCHATCORE_SPEECH_MODEL";
pub const SPEECH_VOICE_ENV: &str = "VRCHATCORE_SPEECH_VOICE";
pub const SPEECH_KEY_ENV_ENV: &str = "VRCHATCORE_SPEECH_KEY_ENV";

/// The environment a core starts with under `settings`: each variable set to a value, or `None` to
/// remove it. Removed and not left alone, so a local voice is local whatever the shell that started
/// the pet had set, and switching back from a custom service leaves nothing of it behind.
pub fn env(settings: &settings::Settings) -> [(&'static str, Option<String>); 5] {
    let (url, model, voice, key_env) = match &settings.speech {
        settings::Speech::Local => (None, None, None, None),
        settings::Speech::Custom { url, model, voice, key_env } => {
            (Some(url.clone()), Some(model.clone()), Some(voice.clone()), key_env.clone())
        }
    };
    [
        (MODEL_ENV, Some(settings.chat_model.name().to_string())),
        (SPEECH_URL_ENV, url),
        (SPEECH_MODEL_ENV, model),
        (SPEECH_VOICE_ENV, voice),
        (SPEECH_KEY_ENV_ENV, key_env),
    ]
}

/// Why the core is gone, in the words the bubble will say.
pub fn exit_why(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("對話核心結束了（結束碼 {code}）"),
        None => "對話核心結束了（沒有結束碼）".to_string(),
    }
}

// ---------------------------------------------------------------------------------------------
// The process half

/// The core as the app holds it, managed by tauri. The state has one writer, the thread that reads
/// the core's stdout (or `start`, before that thread exists), so a read followed by a write cannot
/// race with another write.
#[derive(Default)]
pub struct Core {
    state: Mutex<CoreState>,
    /// The running core, whose stdin is `None` until it is spawned and again once it is gone.
    piped: Piped,
    /// Held for the whole of a restart, so two changes made quickly restart one after the other and
    /// the second starts the core with the settings in effect by then.
    restarting: Mutex<()>,
}

impl Core {
    pub fn state(&self) -> CoreState {
        self.state.lock().expect("core state poisoned").clone()
    }

    /// Writes one `say` to the core. An `Err` reaches the page, which says it in the bubble.
    pub fn say(&self, text: &str) -> Result<(), String> {
        self.write(&say_line(text))
    }

    /// Tells the running core who she is now. A core that is not running is not told and nothing is
    /// kept for later: `launch` tells every core it starts who is shown at that moment, so the next
    /// one learns it from the roster, which already holds the choice.
    pub fn tell(&self, speaking: &cast::Speaking) -> Result<(), String> {
        self.write(&character_line(speaking))
    }

    pub fn greet(&self, text: &str) -> Result<(), String> {
        self.write(&greet_line(text))
    }

    /// One whole line, under the lock, so two lines written at once cannot interleave.
    fn write(&self, line: &str) -> Result<(), String> {
        let mut pipe = self.piped.stdin();
        let Some(stdin) = pipe.as_mut() else {
            drop(pipe);
            return Err(match self.state() {
                CoreState::Down { why } | CoreState::Restarting { why } => why,
                _ => "對話核心還沒起來".to_string(),
            });
        };
        stdin.write_all(line.as_bytes()).and_then(|()| stdin.flush()).map_err(|e| format!("寫不進對話核心：{e}"))
    }
}

/// Records the state and tells the page. The lock is gone before the emit, like `follow_bubble_side`.
fn set_state(app: &AppHandle, next: CoreState) {
    *app.state::<Core>().state.lock().expect("core state poisoned") = next.clone();
    if let Err(e) = app.emit(STATE_EVENT, &next) {
        eprintln!("emit {STATE_EVENT}: {e}");
    }
}

/// Starts the core. It never fails the app: a core that cannot start is `Down` with the reason, and
/// the page says so in the bubble.
pub fn start(app: &AppHandle) {
    if let Err(why) = launch(app) {
        eprintln!("core: {why}");
        set_state(app, CoreState::Down { why });
    }
}

fn launch(app: &AppHandle) -> Result<(), String> {
    let exe_dir = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf));
    let tried = candidates(std::env::var_os(EXE_ENV), exe_dir.as_deref(), !cfg!(debug_assertions));
    let exe = locate(&tried, Path::is_file)?;

    let mut cmd = Command::new(&exe);
    cmd.arg("serve").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // Without this a GUI build flashes a console window when the core starts.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    // The settings as they are now, read at each start: this is the only way a change of model or
    // voice service reaches the core. The log names the variables and not their values beyond the
    // model and the address, and no value here is ever a key.
    let settings = app.state::<settings::Store>().current();
    let mut set = Vec::new();
    for (name, value) in env(&settings) {
        match value {
            Some(value) => {
                cmd.env(name, value);
                set.push(name);
            }
            None => {
                cmd.env_remove(name);
            }
        }
    }
    let speech = match &settings.speech {
        settings::Speech::Local => "local".to_string(),
        settings::Speech::Custom { url, .. } => url.clone(),
    };
    eprintln!("core: starting with model {}, speech {speech}, setting {}", settings.chat_model.name(), set.join(" "));
    let mut child = cmd.spawn().map_err(|e| format!("對話核心 {} 起不來：{e}", exe.display()))?;

    let (Some(mut stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        let _ = child.kill();
        return Err("對話核心的管線沒有接上".to_string());
    };
    // Written before the pipe is shared, so no `say` and no greeting can reach the core ahead of who
    // she is. A failed write is left to the stdout reader, which sees the core go and says why.
    if let Some(speaking) = app.state::<cast::Roster>().speaking() {
        match stdin.write_all(character_line(&speaking).as_bytes()).and_then(|()| stdin.flush()) {
            Ok(()) => eprintln!("core: told it she is {}", speaking.profile.name),
            Err(e) => eprintln!("core: could not tell it who she is: {e}"),
        }
    }
    let reaper = app.state::<Core>().piped.adopt(stdin, child);

    // Both pipes need a reader for the whole life of the core. A pipe that fills stalls the writer,
    // and for stderr that would be a core that hangs on a log line.
    std::thread::spawn(move || {
        if let Err(e) = each_line(BufReader::new(stderr), |line| eprintln!("core: {}", String::from_utf8_lossy(line))) {
            eprintln!("core: reading stderr: {e}");
        }
    });
    let app = app.clone();
    std::thread::spawn(move || read_stdout(&app, stdout, reaper));
    Ok(())
}

/// Stops the core and starts another, which reads the settings in effect now (`launch`): the way a
/// change of chat model or voice service takes effect. While it runs the core is `Restarting` with
/// `why`, which the page says, and a line for the core is refused with the same words.
///
/// Blocks for as long as the old core takes to go, up to `child::STOP_WAIT` and a kill, so it is
/// called on a thread of its own and never on the one that runs the windows.
pub fn restart(app: &AppHandle, why: &str) {
    let core = app.state::<Core>();
    let _one = core.restarting.lock().expect("core restart poisoned");
    set_state(app, CoreState::Restarting { why: format!("{why}，我重新開一下對話核心") });
    let stopped = core.piped.stop(child::STOP_WAIT);
    eprintln!("core: restarting ({why}), the old one {stopped:?}");
    start(app);
}

/// Runs until the core's stdout closes, which is the core exiting or the pet killing it. When the
/// pet itself dies the OS closes the core's stdin, the core reads end of input and exits, so no
/// path leaves a core running without a pet.
fn read_stdout(app: &AppHandle, stdout: ChildStdout, reaper: Reaper) {
    // Bound again only when the character changes, which a reply notices on its way through.
    let mut bound: Option<(Option<cast::Id>, Bindings)> = None;
    let read = pump(BufReader::new(stdout), |line| {
        let shown = app.state::<cast::Roster>().shown();
        let id = shown.as_ref().map(|c| c.id.clone());
        if bound.as_ref().is_none_or(|(was, _)| *was != id) {
            bound = Some((id, model_bindings(app, shown.as_ref())));
        }
        let bindings = &bound.as_ref().expect("bound above").1;
        let event = line.unwrap_or_else(|why| {
            eprintln!("core: bad line: {why}");
            bad_line_event(&why)
        });
        let next = app.state::<Core>().state().after(&event);
        if let Some(next) = next {
            set_state(app, next);
        }
        let event = for_page(event, bindings);
        if let Some(why) = event.pointer("/plan/error").and_then(Value::as_str) {
            eprintln!("gestures: no plan for this reply: {why}");
        }
        if let Err(e) = app.emit(EVENT, &event) {
            eprintln!("emit {EVENT}: {e}");
        }
    });
    if let Err(e) = read {
        eprintln!("core: reading stdout: {e}");
    }
    let why = exit_why(reaper.wait());
    eprintln!("core: {why}");
    // Down only while this is still the current core: one a restart stopped is replaced, and its
    // end says nothing about the next.
    app.state::<Core>().piped.end(&reaper, || set_state(app, CoreState::Down { why }));
}

/// The bindings for the figure on screen, read off its rig.json: the bundle's through the asset
/// resolver, a user's through the same checked read the `character` scheme serves it with. A figure
/// the profile was not written for gets every slot `Unbound`: its cues are `Unresolved` in every
/// plan, which the page can count, and nothing moves on a body the numbers were not written for.
/// That is every user character today.
fn model_bindings(app: &AppHandle, shown: Option<&cast::Character>) -> Bindings {
    let Some(shown) = shown else {
        eprintln!("gestures: no character is chosen, so no cue is bound");
        return Bindings::default();
    };
    let rig = match shown.source {
        cast::Source::Bundled => {
            crate::bundle_file(app, &shown.rig_url()).ok_or_else(|| "it is not in the bundle".to_string())
        }
        cast::Source::User => app
            .state::<cast::Roster>()
            .read_user(&shown.id, cast::RIG)
            .map(|(bytes, _)| bytes)
            .map_err(|r| r.why().to_string()),
    };
    let rig = match rig {
        Ok(rig) => rig,
        Err(why) => {
            eprintln!("gestures: the rig of {} cannot be read ({why}), so no cue is bound", shown.id);
            return Bindings::default();
        }
    };
    match profile::bindings_for(&rig) {
        Ok(bindings) => {
            eprintln!("gestures: {} bound by cue/figure.json, {}", shown.id, profile::parameters(&rig).unwrap_or_default());
            bindings.clone()
        }
        Err(why) => {
            eprintln!("gestures: {why}, so no cue is bound");
            Bindings::default()
        }
    }
}
