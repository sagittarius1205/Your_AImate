// Spike for the Live2D desktop assistant's shell. It exists to answer three questions that the
// design rests on and that no source I found answers (Docs/PROGRAM.md section 3D):
//
//   1. Can a Tauri window be transparent, always on top, and pass clicks through everywhere
//      except the character?
//   2. Does WebGL render on that window, and does a real Live2D model draw on it?
//   3. Can the fleet be observed with `claude agents --json`, and what status values does it
//      actually emit?
//
// Question 3 is why this prints the distinct status values it saw rather than classifying them.
// Only 'idle' has been observed so far, so any rule for "this session needs you" would be a
// guess dressed as code.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};

mod autostart;
mod cast;
mod child;
mod core;
mod cue;
mod fleet;
mod hit;
mod home;
mod press;
mod relay;
mod selftest;
mod settings;
mod stt;
mod themes;
mod voice;


/// One row of `claude agents --json`. Unknown fields are dropped on purpose: the CLI owns this
/// shape, and a spike that refuses to parse a field added next week teaches nothing.
/// `rename_all`, not a rename per field. The page read `waitingSince` and `waitingMessage` while
/// these serialized as `waiting_since` and `waiting_message`, so both arrived as `undefined` and
/// fell back silently: the row showed the session's age instead of how long it had been waiting,
/// and the message never appeared at all. Nothing errored, which is why it survived. One attribute
/// for the whole struct leaves no per-field spelling to forget, and `pet_asking` pins the key set.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    pid: u32,
    cwd: String,
    kind: String,
    started_at: i64,
    session_id: String,
    /// Absent for a session the user never named.
    #[serde(default)]
    name: Option<String>,
    status: String,

    /// Not from the CLI. `status` is only ever 'idle' or 'busy' (measured), so it cannot tell a
    /// session stopped at a permission prompt from one that finished. These two come from the
    /// Notification hook instead, via `vrpet-hook`.
    #[serde(default, skip_deserializing)]
    waiting: bool,
    #[serde(default, skip_deserializing)]
    waiting_since: Option<u128>,
    #[serde(default, skip_deserializing)]
    waiting_message: Option<String>,
    /// Not from the CLI either: the user's pin (`fleet::pins`). Set when the rows are listed.
    #[serde(default, skip_deserializing)]
    pinned: bool,
}

/// What `vrpet-hook set` left behind, keyed by session id.
fn read_alerts() -> std::collections::HashMap<String, (u128, Option<String>)> {
    let mut out = std::collections::HashMap::new();
    let Ok(dir) = home::dir().map(|h| h.join("alerts")) else {
        return out;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(id) = v.get("session_id").and_then(|x| x.as_str()) else {
            continue;
        };
        let at = v.get("at").and_then(|x| x.as_u64()).unwrap_or(0) as u128;
        let msg = v
            .get("message")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());
        out.insert(id.to_string(), (at, msg));
    }
    out
}

/// Where the `claude` executable is. PATH first, because that is what a user's own shell uses.
/// The explicit path is the fallback for a packaged app launched from Explorer, which does not
/// always inherit the same PATH (`which claude` found it at ~/.local/bin on this machine).
fn claude_candidates() -> Vec<String> {
    let mut v = vec!["claude".to_string()];
    if let Some(home) = std::env::var_os("USERPROFILE") {
        let p = std::path::Path::new(&home)
            .join(".local")
            .join("bin")
            .join("claude.exe");
        v.push(p.to_string_lossy().into_owned());
    }
    v
}

fn run_json(exe: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new(exe);
    cmd.args(args);
    // Without this a GUI build flashes a console window on every poll.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().map_err(|e| format!("{exe}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{exe} exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The user's sessions, in the order the list shows them (`fleet::shown`): the CLI's rows with the
/// pet's own conversation engine taken out, each marked waiting from the hook's alerts and pinned from
/// `pins.json`.
#[tauri::command]
fn fleet(pins: tauri::State<'_, fleet::pins::Pins>) -> Result<Vec<Session>, String> {
    let mut last = String::new();
    for exe in claude_candidates() {
        match run_json(&exe, &["agents", "--json"]) {
            Ok(text) => {
                let mut sessions =
                    serde_json::from_str::<Vec<Session>>(&text).map_err(|e| format!("parse: {e}"))?;
                sessions.retain(|s| !fleet::is_pet_engine(&s.cwd));
                let alerts = read_alerts();
                for s in &mut sessions {
                    // A busy session is working, so it is not waiting on anyone whatever the
                    // alert file says. That also covers a Stop hook that never fired.
                    if s.status == "busy" {
                        continue;
                    }
                    if let Some((at, msg)) = alerts.get(&s.session_id) {
                        s.waiting = true;
                        s.waiting_since = Some(*at);
                        s.waiting_message = msg.as_deref().and_then(fleet::asking_line);
                    }
                }
                return Ok(pins.show(sessions));
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Pins or unpins one session and answers the list as it stands now, so the row moves at the click
/// and not at the next poll. Idempotent (`fleet::pins::change`). Only the pet's window may ask.
#[tauri::command]
fn pin_session(pins: tauri::State<'_, fleet::pins::Pins>, session_id: String, pinned: bool) -> Result<Vec<Session>, String> {
    pins.set(&session_id, pinned)
}

/// The relay as it stands, for the page to ask once when it loads, for the reason `core_state` gives.
#[tauri::command]
fn relay_state(relays: tauri::State<'_, relay::Relays>) -> relay::View {
    relays.view()
}

/// A row's 轉話: opens the sheet on that session with an empty field. The session list is read again
/// here, so the sheet shows the row as it is now. `async` because reading it runs the CLI.
#[tauri::command]
async fn relay_open(app: tauri::AppHandle, session_id: String) -> Result<(), String> {
    let fleet = fleet(app.state());
    app.state::<relay::Relays>().apply(&app, relay::Input::Opened { session_id, fleet })
}

/// 送出 and 再送一次, and nothing else: the only command that can start a send, and the page calls it
/// only from those two clicks. The text is the field's at the click, so what the user edited is what
/// is framed and sent. The list is read again, so a row that went or was renamed since the sheet
/// opened is refused and redrawn rather than sent to.
#[tauri::command]
async fn relay_approve(app: tauri::AppHandle, revision: u64, session_id: String, text: String) -> Result<(), String> {
    let fleet = fleet(app.state());
    app.state::<relay::Relays>().approve(&app, revision, session_id, text, fleet)
}

/// 取消 on a draft, 關閉 on an outcome.
#[tauri::command]
fn relay_dismiss(app: tauri::AppHandle, relays: tauri::State<'_, relay::Relays>, revision: u64) -> Result<(), String> {
    relays.apply(&app, relay::Input::Dismiss { revision })
}

/// The page's own answers, printed once on stdout. A spike's job is to answer questions, and a
/// window cannot report to a headless run any other way.
#[tauri::command]
fn report(payload: serde_json::Value) {
    println!("SPIKE_REPORT {payload}");
}

/// Decodes standard base64. No dependency, because this crate has three and each one of them
/// is load-bearing; a decoder is twenty lines and a supply chain is not.
fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut value = [u8::MAX; 256];
    for (i, c) in ALPHABET.iter().enumerate() {
        value[*c as usize] = i as u8;
    }

    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for (at, byte) in s.bytes().enumerate() {
        if byte == b'=' {
            break;
        }
        // A data URL from a `toDataURL` has no line breaks, but one that was pasted or logged
        // and read back might, and tolerating them costs nothing.
        if byte == b'\r' || byte == b'\n' {
            continue;
        }
        let six = value[byte as usize];
        if six == u8::MAX {
            return Err(format!("byte {at} is not base64"));
        }
        acc = (acc << 6) | u32::from(six);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

/// Writes a PNG the page produced, and returns the path it landed on.
///
/// This is the only place a WebView hands this process bytes, so the input gets what every
/// other string crossing that line gets: parsed, bounded, and not trusted for having come from
/// our own page. A page cannot write a file and should not be able to.
///
/// It exists because `drawn()`'s pixel count cannot tell a character apart from a smear, and a
/// transparent always-on-top window is not something a screenshot of the desktop captures
/// cleanly. A PNG of the model alone is the evidence a human can actually check.
#[tauri::command]
fn save_shot(data_url: String) -> Result<String, String> {
    const PREFIX: &str = "data:image/png;base64,";
    // About 9 MiB of PNG. A 320x320 character cannot reach it, and with no cap a renderer that
    // had been talked into something could write until the disk filled.
    const MAX_B64: usize = 12 * 1024 * 1024;

    let Some(b64) = data_url.strip_prefix(PREFIX) else {
        return Err(format!("expected a `{PREFIX}` data URL"));
    };
    if b64.len() > MAX_B64 {
        return Err(format!("{} base64 bytes is over the {MAX_B64} cap", b64.len()));
    }
    let bytes = base64_decode(b64)?;
    // The prefix is a claim about the content. These eight bytes are the content. A file named
    // .png that is not a PNG is a trap for whoever opens it next.
    if !bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Err("the decoded bytes are not a PNG".to_string());
    }

    let dir = home::dir()?.join("shots");
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("shot-{stamp}.png"));
    std::fs::write(&path, &bytes).map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(path.display().to_string())
}

/// Takes the page's geometry report, both lists or neither (`Regions::accept`). The page hears
/// about a rejection through the `Err`, so its own log shows it too.
#[tauri::command]
fn set_regions(
    state: tauri::State<'_, Mutex<hit::Regions>>,
    character: Vec<hit::Region>,
    panel: Vec<hit::Region>,
) -> Result<(), String> {
    let mut regions = state.lock().expect("regions poisoned");
    regions.accept(&character, &panel).inspect_err(|why| {
        eprintln!("set_regions rejected, keeping the last valid report: {why}");
    })
}

/// What the OS cursor is over, from the last valid report.
fn cursor_hit(app: &tauri::AppHandle, win: &tauri::WebviewWindow) -> hit::Hit {
    let Ok(cursor) = app.cursor_position() else {
        return hit::Hit::Through;
    };
    let Ok(origin) = win.inner_position() else {
        return hit::Hit::Through;
    };
    let Ok(size) = win.inner_size() else {
        return hit::Hit::Through;
    };

    let state = app.state::<Mutex<hit::Regions>>();
    let regions = state.lock().expect("regions poisoned");
    hit::hit(
        &regions,
        (cursor.x, cursor.y),
        (origin.x as f64, origin.y as f64),
        (size.width as f64, size.height as f64),
    )
}

/// What the conversation core is doing, for the page to ask once when it starts. The core prints
/// `ready` about 10 ms after it is spawned, long before this page has loaded, and an event sent
/// before the page has a listener is gone, so a page that only listened would read `starting` for
/// ever. It also covers a page that reloads, since Rust's state does not change and so sends nothing.
#[tauri::command]
fn core_state(core: tauri::State<'_, core::Core>) -> core::CoreState {
    core.state()
}

/// Hands the core one thing the user said. The answer is not this command's result. It arrives
/// later as a `core-event`, so a core that is slow, or that fails after taking the line, does not
/// leave the page waiting on a promise. An `Err` here means the line never reached the core.
/// `async` so a write that blocks on a full pipe holds a worker thread and not the one that runs
/// the window.
#[tauri::command]
async fn say(core: tauri::State<'_, core::Core>, text: String) -> Result<(), String> {
    core.say(&text)
}

/// The next thinking filler, so the character makes a sound the moment the user stops typing
/// instead of after the measured 7 to 14 seconds the core takes to answer (`voice`).
///
/// It cannot fail, only answer nothing, and it is deliberately separate from `say`: a filler that
/// could not be found must never stop the sentence being sent. The page calls both and lets this
/// one fail quietly.
///
/// The bank is the shown character's (`voice::Bank::of`), read the first time she is asked for a
/// filler and kept until another character is shown, the way `core::read_stdout` keeps her gesture
/// bindings. With no character to show at all, the compiled bank still answers.
///
/// The turn counter lives here rather than in `voice` so that the rule stays a pure function.
#[tauri::command]
fn filler(
    app: tauri::AppHandle,
    turn: tauri::State<'_, AtomicUsize>,
    roster: tauri::State<'_, cast::Roster>,
    banks: tauri::State<'_, voice::Shown>,
) -> Option<voice::Filler> {
    let n = turn.fetch_add(1, Ordering::Relaxed);
    let Some(shown) = roster.shown() else {
        return voice::pick(voice::bank(), n).cloned();
    };
    banks.pick(shown.id.as_str(), n, || {
        let own = roster.own_file(&shown, cast::FILLERS, &|path| bundle_file(&app, path));
        let (bank, why) = voice::Bank::of(own, shown.profile.voice.is_some());
        if let Some(why) = why {
            eprintln!("fillers: the {} of {} is not used: {why}", cast::FILLERS, shown.id);
        }
        eprintln!("fillers: {} plays {bank}", shown.id);
        bank
    })
}

/// What the speech input service is doing, for the page to ask once when it loads, for the reason
/// `core_state` gives: `ready` is printed long before the page has a listener.
#[tauri::command]
fn stt_state(stt: tauri::State<'_, stt::Stt>) -> stt::SttState {
    stt.state()
}

/// The microphone button went down. The lease is taken before the service hears of it, so the
/// window is pinned from the first moment the service could open the device. `async` for the
/// reason `say` gives.
///
/// Refused while the microphone is switched off in the settings. The page hides the button then,
/// and this is what makes the switch hold for a page that did not.
#[tauri::command]
async fn mic_press(
    held: tauri::State<'_, press::Held>,
    stt: tauri::State<'_, stt::Stt>,
    store: tauri::State<'_, settings::Store>,
) -> Result<(), String> {
    if !store.current().switches.mic {
        return Err("麥克風在設定裡關掉了".to_string());
    }
    held.take(std::time::Instant::now());
    stt.send(stt::Op::Press).inspect_err(|_| {
        held.release();
    })
}

/// The button came up, or the page lost the pointer, which is the same thing (Docs/DESIGN_STT.md
/// 4.1.1). The service is told even with no lease held: release is a no-op there too, and a page
/// that reloaded mid-press has no other way to end it.
#[tauri::command]
async fn mic_release(held: tauri::State<'_, press::Held>, stt: tauri::State<'_, stt::Stt>) -> Result<(), String> {
    held.release();
    stt.send(stt::Op::Release)
}

/// Ends the pet. Nothing is stopped by hand: the core and the speech service read their stdin from
/// this process, and both exit when it closes (`core::start`, `stt::start`).
#[tauri::command]
fn quit(app: tauri::AppHandle) {
    app.exit(0);
}

#[derive(Serialize)]
struct Characters {
    shown: cast::Listed,
    characters: Vec<cast::Listed>,
    refused: Vec<cast::Refused>,
}

/// Every character, the one to draw, and every folder that could not be loaded with the reason. The
/// page asks once, before it mounts the figure.
#[tauri::command]
fn characters(roster: tauri::State<'_, cast::Roster>) -> Result<Characters, String> {
    let found = roster.found()?;
    let shown = roster.shown().ok_or("no character can be shown")?;
    Ok(Characters {
        shown: shown.listed(),
        characters: found.characters.iter().map(cast::Character::listed).collect(),
        refused: found.refused.clone(),
    })
}

/// Saves the choice, tells the core, and reloads the pet's page, because the figure is built once
/// per page. The settings window asks for this and the pet is another window, so Rust reloads it.
///
/// The choice stands even when the core cannot be told, since it is saved and the figure is hers:
/// a core that is down is told by `core::launch` when it next starts, from the roster this just set.
#[tauri::command]
fn choose_character(
    app: tauri::AppHandle,
    roster: tauri::State<'_, cast::Roster>,
    core: tauri::State<'_, core::Core>,
    store: tauri::State<'_, settings::Store>,
    id: String,
) -> Result<(), String> {
    roster.choose(&id, &store)?;
    tell_and_reload(&app, &core);
    Ok(())
}

/// Tells the core who is shown now and reloads the pet's page, which builds the figure and asks for
/// her greeting once per load: after a choice, and after a profile was written or copied and scanned.
fn tell_and_reload(app: &tauri::AppHandle, core: &core::Core) {
    if let Some(speaking) = app.state::<cast::Roster>().speaking() {
        match core.tell(&speaking) {
            Ok(()) => eprintln!("core: told it she is now {}", speaking.profile.name),
            Err(why) => eprintln!("core: not told she is now {}, and it will be when it starts: {why}", speaking.profile.name),
        }
    }
    if let Some(win) = app.get_webview_window(OVERLAY)
        && let Err(e) = win.reload()
    {
        eprintln!("reloading the pet: {e}");
    }
}

/// The profile the settings window edits: the shown character's, with where she comes from, since
/// only the user's own may be changed. The persona is in it, unlike `characters`: the window shows it
/// to be edited, and it is the user's own text.
#[derive(Serialize)]
struct Editable {
    id: cast::Id,
    source: cast::Source,
    name: String,
    /// Empty when she has none.
    outfit: String,
    persona: String,
    greeting: String,
    fillers: Vec<String>,
    /// Whether she has a voice of her own, which the window says it keeps and cannot change.
    voice: bool,
}

#[tauri::command]
fn character_profile(roster: tauri::State<'_, cast::Roster>) -> Result<Editable, String> {
    let c = roster.shown().ok_or("沒有能顯示的角色")?;
    Ok(Editable {
        id: c.id,
        source: c.source,
        name: c.profile.name,
        outfit: c.profile.outfit.unwrap_or_default(),
        persona: c.profile.persona,
        greeting: c.profile.greeting.unwrap_or_default(),
        fillers: c.profile.fillers,
        voice: c.profile.voice.is_some(),
    })
}

/// Writes the edit over the user character `id`'s profile (`Roster::save_profile`), scans the
/// folders again so the roster holds what was written, and has the core and the pet's page take her
/// as she is now.
#[tauri::command]
fn save_profile(
    app: tauri::AppHandle,
    roster: tauri::State<'_, cast::Roster>,
    core: tauri::State<'_, core::Core>,
    id: String,
    profile: cast::Edit,
) -> Result<(), String> {
    roster.save_profile(&id, profile)?;
    roster.rescan(&|path| bundle_file(&app, path));
    tell_and_reload(&app, &core);
    Ok(())
}

/// Copies the bundled character `from` into the user's folder as `to`, scans again, and makes the
/// copy the one shown, so the profile editor is on the copy the user can change.
#[tauri::command]
fn copy_character(
    app: tauri::AppHandle,
    roster: tauri::State<'_, cast::Roster>,
    core: tauri::State<'_, core::Core>,
    store: tauri::State<'_, settings::Store>,
    from: String,
    to: String,
) -> Result<String, String> {
    let read = |path: &str| bundle_file(&app, path);
    let id = roster.copy(&read, &from, &to)?;
    roster.rescan(&read);
    roster.choose(id.as_str(), &store)?;
    tell_and_reload(&app, &core);
    Ok(id.to_string())
}

/// The speech recognition packs the settings window may offer (`stt::packs`). `async` because it
/// hashes the whole model, which takes about a second.
#[tauri::command]
async fn stt_packs() -> Result<Vec<stt::Pack>, String> {
    stt::packs()
}

/// The models the chat service at `url` has, for the settings window's model list (`core::list_models`).
/// `async` because the core is started for it and may wait on the service for seconds.
#[tauri::command]
async fn chat_models(url: String, key_env: Option<String>) -> Result<Vec<String>, String> {
    core::list_models(&url, key_env.as_deref().filter(|k| !k.is_empty()))
}

/// The settings in effect and what was wrong with the file they came from. The pet's page asks once
/// when it loads, for the reason `core_state` gives, and the settings window asks to fill its form.
///
/// The autostart switch is the shortcut's and not the file's (`autostart.rs`).
#[tauri::command]
fn get_settings(store: tauri::State<'_, settings::Store>, autostart: tauri::State<'_, autostart::Autostart>) -> settings::Loaded {
    autostart.overlay(store.loaded())
}

/// The settings window's whole form. Checked by `Store::set_with`, which makes the Startup shortcut
/// match the autostart switch before it saves anything, so a shortcut that could not be made refuses
/// the request. Then applied by Rust where Rust owns the thing changed, and sent to the pet's page for
/// the rest (`apply`). `async`, because making the shortcut runs PowerShell for most of a second and
/// would otherwise hold the thread that runs the windows.
///
/// A theme the request moves to must be one the last scan can show. One it keeps is not asked
/// about: a theme deleted since it was chosen falls back with a reason (`themes::Found::pick`), and
/// refusing every other change until the user chose again would hold the whole form hostage to it.
#[tauri::command]
async fn set_settings(
    app: tauri::AppHandle,
    store: tauri::State<'_, settings::Store>,
    autostart: tauri::State<'_, autostart::Autostart>,
    looks: tauri::State<'_, themes::Themes>,
    settings: serde_json::Value,
) -> Result<settings::Loaded, String> {
    let before = store.current();
    let after = store.set_with(&settings, |next| {
        if next.look.theme != before.look.theme && looks.found()?.find(next.look.theme.as_str()).is_none() {
            return Err(format!("look.theme：{} 不是能用的主題", next.look.theme));
        }
        let on = next.switches.autostart;
        if on == autostart.is_on() { Ok(()) } else { autostart.set(on) }
    })?;
    apply(&app, &before, &after.settings);
    Ok(autostart.overlay(after))
}

/// What a change of settings does outside the pet's page: the window's place on top and its size,
/// and a press the microphone switch has just ended. Then the page is sent the whole of what is in
/// effect and applies the rest itself. Only the pet's window is sent it: the settings window is the
/// one that asked, and it has the answer already.
fn apply(app: &tauri::AppHandle, before: &settings::Settings, after: &settings::Settings) {
    if let Some(win) = app.get_webview_window(OVERLAY) {
        if before.switches.on_top != after.switches.on_top
            && let Err(e) = win.set_always_on_top(after.switches.on_top)
        {
            eprintln!("settings: set_always_on_top({}): {e}", after.switches.on_top);
        }
        if before.scale.window != after.scale.window {
            fit_window(&win, after.scale.window);
        }
    }
    if before.switches.mic
        && !after.switches.mic
        && app.state::<press::Held>().release()
        && let Err(why) = app.state::<stt::Stt>().send(stt::Op::Release)
    {
        eprintln!("settings: the microphone was switched off mid-press and the release did not reach the service: {why}");
    }
    if let Err(e) = app.emit_to(OVERLAY, SETTINGS_EVENT, after) {
        eprintln!("emit {SETTINGS_EVENT}: {e}");
    }
    // Each on a thread of its own: a restart waits for the old process to go, for up to seconds.
    let restarts = settings::restarts(before, after);
    if let Some(why) = restarts.core {
        let app = app.clone();
        std::thread::spawn(move || core::restart(&app, &why));
    }
    if let Some(why) = restarts.stt {
        let app = app.clone();
        std::thread::spawn(move || stt::restart(&app, &why));
    }
}

/// Sizes the pet's window at `factor` and zooms its page by the same factor, so the page still lays
/// out at `WINDOW_BASE` CSS pixels and every ratio measured on it holds: the band above her head, the
/// regions the hit test reads as fractions of the viewport, her place on the canvas. The zoom is the
/// webview's and not CSS `zoom`, because the figure works out where her eyes are from the canvas's
/// box in CSS pixels, and CSS zoom puts that box in another unit. Her own size inside the window,
/// `scale.figure`, is the page's (index.html `--figure`).
///
/// Idempotent: run again at the same factor it moves nothing, which is what lets each page load run
/// it (`on_page_load`) without asking whether the zoom survived the reload.
fn fit_window(win: &tauri::WebviewWindow, factor: f64) {
    let placed = (|| -> tauri::Result<()> {
        let at = win.outer_position()?;
        let size = win.inner_size()?;
        let work = win.current_monitor()?.map(|m| {
            let area = m.work_area();
            ((area.position.x, area.position.y), (area.size.width, area.size.height))
        });
        let (to, wanted) = settings::window_rect((at.x, at.y), (size.width, size.height), factor, win.scale_factor()?, work);
        if wanted != (size.width, size.height) {
            win.set_size(tauri::PhysicalSize::new(wanted.0, wanted.1))?;
            win.set_position(tauri::PhysicalPosition::new(to.0, to.1))?;
        }
        win.set_zoom(factor)
    })();
    if let Err(e) = placed {
        eprintln!("settings: sizing the pet for scale.window {factor}: {e}");
    }
}

/// Opens the settings window, or brings it forward when it is open already. `async` because a
/// window built inside a synchronous command deadlocks on Windows (tauri 2, `WebviewWindowBuilder`).
///
/// Above every window while it has the focus, and an ordinary window again when it loses it. The pet
/// is always on top, so an ordinary settings window opened over her went under her as it appeared,
/// with the sliders that size her behind her. On top for its whole life would also cover the browser
/// the user switches to for a service's address; while focused it is above her for as long as it is
/// being used, and a click on it brings it back.
#[tauri::command]
async fn open_settings(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(SETTINGS_WINDOW) {
        let _ = win.unminimize();
        let _ = win.set_always_on_top(true);
        return win.set_focus().map_err(|e| e.to_string());
    }
    let win = tauri::WebviewWindowBuilder::new(&app, SETTINGS_WINDOW, tauri::WebviewUrl::App("settings.html".into()))
        .title("設定")
        .inner_size(460.0, 680.0)
        .min_inner_size(360.0, 420.0)
        .resizable(true)
        .always_on_top(true)
        .build()
        .map_err(|e| e.to_string())?;
    let handle = win.clone();
    win.on_window_event(move |event| {
        if let tauri::WindowEvent::Focused(focused) = event {
            match handle.set_always_on_top(*focused) {
                Ok(()) => eprintln!("settings: on top {focused}, following its focus"),
                Err(e) => eprintln!("settings: set_always_on_top({focused}): {e}"),
            }
        }
    });
    Ok(())
}

/// Opens `<home>/characters` in Explorer, making it first if it is not there. The path is Rust's,
/// so the page can open this one folder and nothing else.
#[tauri::command]
fn open_characters_folder() -> Result<String, String> {
    let dir = settings::characters_dir(&home::dir()?)?;
    Command::new("explorer").arg(&dir).spawn().map_err(|e| format!("explorer {}: {e}", dir.display()))?;
    Ok(dir.display().to_string())
}

/// The interface themes, the one in effect, and why it is not the one chosen when it is not
/// (Docs/DESIGN_Themes.md). Both folders are scanned again first, so a theme dropped into the
/// user's folder is listed the next time a page asks, and the pet's page asks again whenever the
/// settings it is sent name another theme, so a switch applies without reloading her.
#[tauri::command]
fn themes(
    app: tauri::AppHandle,
    looks: tauri::State<'_, themes::Themes>,
    store: tauri::State<'_, settings::Store>,
) -> Result<themes::Listing, String> {
    looks.rescan(&|path| bundle_file(&app, path));
    looks.listing(&store.current().look.theme)
}

/// Opens `<home>/themes` in Explorer, made first if it is not there, as `open_characters_folder` does.
#[tauri::command]
fn open_themes_folder() -> Result<String, String> {
    let dir = themes::dir(&home::dir()?)?;
    Command::new("explorer").arg(&dir).spawn().map_err(|e| format!("explorer {}: {e}", dir.display()))?;
    Ok(dir.display().to_string())
}

/// Has her say her greeting, in her voice, through the core and not the model. The answer is
/// the text sent, so the page can tell the greeting from an answer when it arrives as an
/// `utterance`, or `None` when her profile has none and nothing is coming.
///
/// The page asks, once per load and after its listeners are up, and not Rust at the moment she is
/// chosen: the page reloads after a choice, and an utterance emitted while it loads would reach no
/// listener. The text is her profile's, read here, so the page cannot have her say anything else.
#[tauri::command]
async fn greet(roster: tauri::State<'_, cast::Roster>, core: tauri::State<'_, core::Core>) -> Result<Option<String>, String> {
    let Some(text) = roster.shown().and_then(|c| c.profile.greeting) else {
        return Ok(None);
    };
    core.greet(&text)?;
    Ok(Some(text))
}

/// The label of the pet's own window. `tauri.conf.json` gives the window this label and the poll
/// thread below looks it up by it, so the name is written once and `pet_page` reads the
/// configuration to check the two agree.
const OVERLAY: &str = "main";
/// The settings window, which `open_settings` builds when it is first asked for.
const SETTINGS_WINDOW: &str = "settings";
/// The settings in effect, sent to the pet's page each time they change.
const SETTINGS_EVENT: &str = "settings";

/// Where the cursor is, in the window's CSS pixels (`hit::css`), each time it moves. The page hands
/// it to the figure, which turns toward it.
const CURSOR_EVENT: &str = "cursor";
/// 25 polls of 40 ms: a still cursor is sent again once a second.
const LOOK_RESEND_POLLS: u32 = 25;

/// What each window may ask for (Docs/DESIGN_Settings.md 2). A command in neither list is refused to
/// both, so a command added later is refused until someone decides which window it is for, and
/// `pet_settings` fails on a registered command that no list names.
///
/// Tauri checks its own permission list for a custom command only when the app ships an ACL
/// manifest of its own or the request comes from a remote origin: "This ensures remote content can
/// never reach custom commands unless an explicit `remote` capability has been configured for
/// them" (tauri 2.12.5, webview/mod.rs). This app ships no such manifest, so every local window
/// reaches every command unless this table says otherwise. `say` is what makes that concrete: it
/// hands a sentence to a core that pays for the answer out of the user's subscription, so the
/// settings window, which has no reason to talk to her, must not be able to.
///
/// The `character` scheme is in the pet's list as well. It serves the files of user characters,
/// which only the figure draws. The `theme` scheme is in both: the pet draws a theme's icon and
/// paper, and the settings window shows the icon beside each theme's name.
const ALLOWED: &[(&str, &[&str])] = &[
    (
        OVERLAY,
        &[
            "fleet",
            "pin_session",
            "set_regions",
            "report",
            "save_shot",
            "core_state",
            "say",
            "filler",
            "stt_state",
            "mic_press",
            "mic_release",
            "quit",
            "characters",
            "greet",
            "get_settings",
            "open_settings",
            "themes",
            "relay_state",
            "relay_open",
            "relay_approve",
            "relay_dismiss",
            cast::SCHEME,
            themes::SCHEME,
        ],
    ),
    (
        SETTINGS_WINDOW,
        &[
            "get_settings",
            "set_settings",
            "characters",
            "choose_character",
            "open_characters_folder",
            "character_profile",
            "save_profile",
            "copy_character",
            "stt_packs",
            "chat_models",
            "themes",
            "open_themes_folder",
            themes::SCHEME,
        ],
    ),
];

/// Why a window may not ask, or `None` when it may.
fn refuse(label: &str, command: &str) -> Option<String> {
    match ALLOWED.iter().find(|(window, _)| *window == label) {
        Some((_, commands)) if commands.contains(&command) => None,
        Some(_) => Some(format!("{command} is not one of the commands the window {label:?} may ask for")),
        None => Some(format!("{command} was asked for by {label:?}, which is not a window this app made")),
    }
}

/// A file of the bundle, or `None` when the bundle does not have it.
///
/// The resolver alone cannot say `None`: for a path it lacks it tries `<path>.html`, then
/// `<path>/index.html`, then answers with `index.html` itself (tauri 2.12.5, manager/mod.rs
/// `get_asset`). A missing `rig.json` would arrive as the page's own HTML. Nothing here asks for an
/// HTML file, so an HTML answer is that fallback.
pub fn bundle_file(app: &tauri::AppHandle, path: &str) -> Option<Vec<u8>> {
    app.asset_resolver().get(path.to_string()).filter(|a| !a.mime_type.starts_with("text/html")).map(|a| a.bytes)
}

/// One file of a user's folder, for the `character` or the `theme` scheme. Only a window whose list
/// names the scheme may ask, for the reason `refuse` gives, and only for a GET. The checks
/// themselves are `serve`'s: `Roster::serve` or `Themes::serve`.
///
/// `Access-Control-Allow-Origin` because the page is another origin, and an image drawn from another
/// origin without it taints the canvas: `drawn()` reads pixels back and `shot()` encodes them, and
/// both throw on a tainted canvas.
///
/// `Content-Security-Policy` and `nosniff` because a theme's SVG is the user's file and an SVG is a
/// document that can hold script. The page only ever uses one as an image, where none runs; these
/// make a request that opened one as a document get no script and no outside load either.
fn user_file(
    label: &str,
    scheme: &str,
    request: &tauri::http::Request<Vec<u8>>,
    serve: impl FnOnce(&str) -> Result<(Vec<u8>, &'static str), cast::Refusal>,
) -> tauri::http::Response<Vec<u8>> {
    let path = request.uri().path();
    let served = match refuse(label, scheme) {
        Some(why) => Err(cast::Refusal::Forbidden(why)),
        None if request.method() != tauri::http::Method::GET => {
            Err(cast::Refusal::Forbidden(format!("{} is not GET", request.method())))
        }
        None => serve(path),
    };
    let response = tauri::http::Response::builder()
        .header("Access-Control-Allow-Origin", "*")
        .header("Content-Security-Policy", "default-src 'none'; style-src 'unsafe-inline'")
        .header("X-Content-Type-Options", "nosniff");
    let built = match served {
        Ok((bytes, mime)) => response.status(200).header("Content-Type", mime).body(bytes),
        Err(refusal) => {
            eprintln!("{scheme}: refused {path}: {}", refusal.why());
            response
                .status(refusal.status())
                .header("Content-Type", "text/plain; charset=utf-8")
                .body(refusal.why().as_bytes().to_vec())
        }
    };
    built.unwrap_or_else(|e| {
        eprintln!("{scheme}: {path}: {e}");
        tauri::http::Response::new(Vec::new())
    })
}

fn main() {
    // Before any window exists: a self-test must be able to run, and fail, with nothing on screen.
    if let Some(code) = selftest::dispatch(std::env::args_os().skip(1)) {
        std::process::exit(code);
    }
    tauri::Builder::default()
        .manage(Mutex::new(hit::Regions::default()))
        // Here and not in `setup`, so the page's first poll can never find it missing.
        .manage(fleet::pins::Pins::open(home::dir().ok()))
        .manage(core::Core::default())
        .manage(relay::Relays::default())
        .manage(stt::Stt::default())
        .manage(press::Held::default())
        // Which filler plays next. An integer and not a shuffled bag, because `voice::pick` is
        // round-robin for the reasons written there.
        .manage(AtomicUsize::new(0))
        .manage(voice::Shown::default())
        // A thread per request, because a layer can be 16 MB and the handler otherwise runs on the
        // thread that draws the window.
        .register_asynchronous_uri_scheme_protocol(cast::SCHEME, |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            let label = ctx.webview_label().to_string();
            std::thread::spawn(move || {
                let serve = |path: &str| app.state::<cast::Roster>().serve(path).map(|(bytes, kind)| (bytes, kind.mime()));
                responder.respond(user_file(&label, cast::SCHEME, &request, serve))
            });
        })
        .register_asynchronous_uri_scheme_protocol(themes::SCHEME, |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            let label = ctx.webview_label().to_string();
            std::thread::spawn(move || {
                let serve = |path: &str| app.state::<themes::Themes>().serve(path).map(|(bytes, kind)| (bytes, kind.mime()));
                responder.respond(user_file(&label, themes::SCHEME, &request, serve))
            });
        })
        // Wrapped, and not handed straight to the builder, so the rule is in one place instead of
        // one line at the top of each command. A command added later cannot forget a check it
        // never had to write, which is the only version of this that stays true.
        .invoke_handler({
            // The type is written out because the generated handler is generic over the runtime
            // and nothing else in this expression names which one.
            let commands: Box<dyn Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync> =
                Box::new(tauri::generate_handler![
                    fleet,
                    pin_session,
                    set_regions,
                    report,
                    save_shot,
                    core_state,
                    say,
                    filler,
                    stt_state,
                    mic_press,
                    mic_release,
                    quit,
                    characters,
                    choose_character,
                    greet,
                    get_settings,
                    set_settings,
                    open_settings,
                    open_characters_folder,
                    character_profile,
                    save_profile,
                    copy_character,
                    stt_packs,
                    chat_models,
                    themes,
                    open_themes_folder,
                    relay_state,
                    relay_open,
                    relay_approve,
                    relay_dismiss
                ]);
            move |invoke| match refuse(invoke.message.webview_ref().label(), invoke.message.command()) {
                // `true` is "answered". The refusal reaches the caller as the command's error, and
                // stderr is where it is seen when the caller is not a page anyone is looking at.
                Some(why) => {
                    eprintln!("refused: {why}");
                    invoke.resolver.reject(why);
                    true
                }
                None => commands(invoke),
            }
        })
        // Every load of the pet's page gets the window's size and zoom again, so whether the webview
        // keeps a zoom across a reload is not a question. `try_state`, because the first load can
        // start before `setup` has made the settings, and `setup` sizes the window itself.
        .on_page_load(|webview, payload| {
            let app = webview.app_handle();
            if webview.label() == OVERLAY
                && payload.event() == tauri::webview::PageLoadEvent::Started
                && let (Some(win), Some(store)) = (app.get_webview_window(OVERLAY), app.try_state::<settings::Store>())
            {
                fit_window(&win, store.window());
            }
        })
        .setup(|app| {
            let store = settings::Store::open(home::dir().ok());
            if let Some(win) = app.get_webview_window(OVERLAY) {
                if let Err(e) = win.set_always_on_top(store.current().switches.on_top) {
                    eprintln!("settings: set_always_on_top at start: {e}");
                }
                fit_window(&win, store.window());
            }
            // Before the core, which binds the gestures to the character this names.
            let handle = app.handle().clone();
            app.manage(cast::Roster::load(&|path| bundle_file(&handle, path), home::dir().ok(), &store));
            app.manage(themes::Themes::load(&|path| bundle_file(&handle, path), home::dir().ok()));
            app.manage(store);
            app.manage(autostart::Autostart::find());
            // A core that cannot start leaves the character mute and the rest of the app working, so
            // there is no error here to return.
            core::start(app.handle());
            // The same for the microphone: without it the pet is a pet you type to.
            stt::start(app.handle());
            let handle = app.handle().clone();
            // The window cannot do this hit test itself: while it ignores cursor events it never
            // sees the cursor, so there is nothing to react to. Polling the OS cursor from
            // outside the window is the way out of that circle.
            std::thread::spawn(move || {
                let mut ignoring: Option<bool> = None;
                let mut looked: Option<hit::Css> = None;
                let mut since_look = 0u32;
                loop {
                    std::thread::sleep(Duration::from_millis(40));
                    let Some(win) = handle.get_webview_window(OVERLAY) else {
                        continue;
                    };
                    let held = handle.state::<press::Held>();
                    if held.expire_overdue(std::time::Instant::now()) {
                        eprintln!("mic: a press outlived {:?} and was released", press::MAX_HOLD);
                        if let Err(why) = handle.state::<stt::Stt>().send(stt::Op::Release) {
                            eprintln!("mic: release after expiry: {why}");
                        }
                    }
                    let ignore = press::ignore_cursor(held.pins_window(), cursor_hit(&handle, &win));
                    if ignoring != Some(ignore) {
                        if let Err(e) = win.set_ignore_cursor_events(ignore) {
                            eprintln!("set_ignore_cursor_events({ignore}): {e}");
                        } else {
                            ignoring = Some(ignore);
                        }
                    }
                    // The same poll tells the figure where to look: on every move, and once a second
                    // even when the cursor is still, because the first send comes before the page
                    // has a listener and a still cursor would otherwise never be sent again.
                    // The page's CSS pixel is the monitor's scale times the window's zoom (`fit_window`).
                    if let (Ok(cursor), Ok(origin), Ok(scale)) =
                        (handle.cursor_position(), win.inner_position(), win.scale_factor())
                    {
                        let zoom = handle.state::<settings::Store>().window();
                        let at = hit::css((cursor.x, cursor.y), (origin.x as f64, origin.y as f64), scale * zoom);
                        since_look += 1;
                        if looked != Some(at) || since_look >= LOOK_RESEND_POLLS {
                            if let Err(e) = win.emit(CURSOR_EVENT, at) {
                                eprintln!("emit {CURSOR_EVENT}: {e}");
                            }
                            looked = Some(at);
                            since_look = 0;
                        }
                    }
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("tauri failed to start");
}
