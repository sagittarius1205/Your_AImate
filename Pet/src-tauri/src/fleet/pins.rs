//! Which sessions the user pinned to the top of the list (Docs/DESIGN_Pet.md 4.4.5).
//!
//! One file of its own, `<home>/pins.json`, and this module is the only code that reads or writes
//! it: `{"pinned": ["<session id>", ...]}`, oldest pin first. It is not a key of `settings.json`
//! because the two have different writers. The settings window sends the whole form each time, from a
//! copy it took when it opened, so a pin made in the pet in the meantime would be written over by a
//! stale list. Here the pet's page is the only caller, and every call is one pin.
//!
//! A pin is the session's id, which `claude agents --json` gives and which outlives the process: a
//! session closed tonight and resumed tomorrow is still the one that was pinned. So a pin is never
//! dropped because its session was missing from a poll. The only thing that drops one is the cap
//! (`MAX`), and then only the oldest pin whose session is not running, because a session that is gone
//! cannot be unpinned from a list that no longer shows it.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Value, json};

use crate::Session;

pub const FILE: &str = "pins.json";
/// Every pin the file holds. A pin is a row of a list on a 460 pixel window; 64 is more than anyone
/// scrolls, and a file that holds more is not one this module wrote.
pub const MAX: usize = 64;
/// The file's own size, which 64 ids of 64 characters fit in with room to spare. A bigger file is read
/// as having nothing in it, so a hand-edit that went wrong cannot make every poll parse megabytes.
const FILE_CAP: u64 = 64 * 1024;
const ID_MAX: usize = 64;

/// `^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$`. The CLI's ids are UUIDs today, and this is looser than that on
/// purpose: a session id of another shape must pin as well as this one, and the check is here to keep
/// out what the page could not have got from the list, not to say what a session id is.
pub fn valid_id(id: &str) -> Result<(), String> {
    let mut chars = id.chars();
    let first = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
    if first && id.len() <= ID_MAX && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')) {
        Ok(())
    } else {
        Err(format!("{id:?} 不是 session 的 id（1 到 {ID_MAX} 個英文字母、數字、_ 或 -）"))
    }
}

/// The pins after `id` is pinned or unpinned. Idempotent: pinning what is pinned and unpinning what is
/// not leave the list as it was, so a click that arrives twice is one pin. At the cap a new pin takes
/// the place of the oldest one whose session `is_live` does not know, and with every one running it is
/// refused and says so.
pub fn change(pins: &[String], is_live: impl Fn(&str) -> bool, id: &str, pinned: bool) -> Result<Vec<String>, String> {
    valid_id(id)?;
    let mut next = pins.to_vec();
    let at = next.iter().position(|p| p == id);
    match (pinned, at) {
        (true, Some(_)) | (false, None) => {}
        (false, Some(i)) => {
            next.remove(i);
        }
        (true, None) => {
            if next.len() >= MAX {
                match next.iter().position(|p| !is_live(p)) {
                    Some(gone) => {
                        next.remove(gone);
                    }
                    None => return Err(format!("已經釘了 {MAX} 個，而且每一個都還在執行，先取消幾個")),
                }
            }
            next.push(id.to_string());
        }
    }
    Ok(next)
}

/// The pins a file's bytes hold, and what was wrong with it. Field by field, like `settings.json`: an
/// entry that is not an id is dropped on its own, a repeat is kept once, and past the cap the newest
/// pins stay.
pub fn parse(bytes: &[u8]) -> (Vec<String>, Vec<String>) {
    let Ok(root) = serde_json::from_slice::<Value>(bytes) else {
        return (Vec::new(), vec![format!("{FILE} 不是 JSON，當成沒有釘選")]);
    };
    let Some(list) = root.get("pinned").and_then(Value::as_array) else {
        return (Vec::new(), vec![format!("{FILE} 裡沒有 pinned 這個陣列，當成沒有釘選")]);
    };
    let mut why = Vec::new();
    let mut pins: Vec<String> = Vec::new();
    for item in list {
        match item.as_str() {
            Some(id) => match valid_id(id) {
                Ok(()) if pins.iter().any(|p| p == id) => {}
                Ok(()) => pins.push(id.to_string()),
                Err(e) => why.push(e),
            },
            None => why.push(format!("{item} 不是文字")),
        }
    }
    if pins.len() > MAX {
        pins.drain(..pins.len() - MAX);
        why.push(format!("超過 {MAX} 個，留下最近釘的 {MAX} 個"));
    }
    (pins, why)
}

fn read(path: &Path) -> Vec<String> {
    let bytes = match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Ok(meta) if meta.len() > FILE_CAP => {
            eprintln!("pins: {} is {} bytes, over {FILE_CAP}, so there are no pins", path.display(), meta.len());
            return Vec::new();
        }
        _ => match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("pins: {}: {e}", path.display());
                return Vec::new();
            }
        },
    };
    let (pins, why) = parse(&bytes);
    for w in why {
        eprintln!("pins: {w}");
    }
    pins
}

/// Written to a second file and renamed over the first, as `settings.json` is, so a pet that dies
/// mid-write leaves the old file whole.
fn write(path: &Path, pins: &[String]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = serde_json::to_vec_pretty(&json!({ "pinned": pins })).map_err(|e| e.to_string())?;
    let next = path.with_extension("json.new");
    std::fs::write(&next, text).map_err(|e| format!("{}: {e}", next.display()))?;
    std::fs::rename(&next, path).map_err(|e| format!("{}: {e}", path.display()))
}

struct Inner {
    /// Oldest pin first.
    pins: Vec<String>,
    /// The last list the CLI gave, before the pins were applied. A pin changes how that list is shown
    /// and not what it holds, so the new order can be answered from it without asking the CLI again
    /// (`claude agents --json` takes a moment, and a pin should move the row now).
    polled: Vec<Session>,
}

/// The pins, and the last fleet they were applied to.
pub struct Pins {
    path: Option<PathBuf>,
    inner: Mutex<Inner>,
}

impl Pins {
    /// Reads the file once. With no home there is nowhere to read from or save to: the list shows no
    /// pins, and pinning says why it cannot.
    pub fn open(home: Option<PathBuf>) -> Pins {
        let path = home.map(|h| h.join(FILE));
        let pins = path.as_deref().map(read).unwrap_or_default();
        Pins { path, inner: Mutex::new(Inner { pins, polled: Vec::new() }) }
    }

    /// A poll's rows, kept, and shown as the page lists them. Nothing is dropped from the pins here, so
    /// a session that is missing from one poll is still pinned when it comes back.
    pub fn show(&self, polled: Vec<Session>) -> Vec<Session> {
        let mut inner = self.inner.lock().expect("pins poisoned");
        inner.polled = polled;
        super::shown(&inner.polled, &inner.pins)
    }

    /// Pins or unpins one session, saves the list, and answers the last poll as it is listed now.
    /// The file is written before the list in memory changes, so a write that fails leaves both as
    /// they were and the page is told.
    pub fn set(&self, id: &str, pinned: bool) -> Result<Vec<Session>, String> {
        let mut inner = self.inner.lock().expect("pins poisoned");
        let next = change(&inner.pins, |p| inner.polled.iter().any(|s| s.session_id == p), id, pinned)?;
        if next != inner.pins {
            let path = self.path.as_deref().ok_or("桌寵沒有家目錄，釘選存不下來")?;
            write(path, &next)?;
            inner.pins = next;
        }
        Ok(super::shown(&inner.polled, &inner.pins))
    }

    pub fn pinned(&self) -> Vec<String> {
        self.inner.lock().expect("pins poisoned").pins.clone()
    }
}
