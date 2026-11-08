//! The pet's settings (Docs/DESIGN_Settings.md 1): one file, `settings.json` in the pet's home
//! (home.rs), and this module is the only code that reads or writes it. The character choice lives
//! in the same file under `character`, so `cast.rs` reads and saves it through here and has no
//! reader of its own.
//!
//! Two ways in, with two rules. The file is read field by field, and a field that is broken or out
//! of range falls back to its default with the reason recorded, because one hand-edited typo must
//! not cost the user every other setting. A request from the settings window is all or nothing: one
//! bad field refuses the whole request and nothing is written, because the window built it and a
//! bad field there is a bug to see, not a value to guess at.
//!
//! Keys this version does not know are kept on every write, so an older and a newer pet can share
//! the file.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::cast::Id;

pub const FILE: &str = "settings.json";

/// The size the pet's window was laid out at (Docs/DESIGN_Pet.md 4.4.1), which `tauri.conf.json`
/// gives it. `scale.window` multiplies it, and `pet_settings` checks the two agree.
pub const WINDOW_BASE: (f64, f64) = (460.0, 620.0);

/// The speech recognition model the pet ships with, and so the only pack there is today.
pub const DEFAULT_PACK: &str = "sense-voice-zh-en-ja-ko-yue-int8-2024-07-17";

// ---------------------------------------------------------------------------------------------
// The data

/// A number with its range and default. The table below is the one place a range is written: the
/// parser reads it, and `pet_settings` checks the settings page's sliders against it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounded {
    /// The field's path in the file, dotted.
    pub field: &'static str,
    pub low: f64,
    pub high: f64,
    pub default: f64,
    /// Whole numbers only: the font size is a pixel count.
    pub integer: bool,
}

pub const VOLUME: Bounded = Bounded { field: "volume", low: 0.0, high: 1.0, default: 0.8, integer: false };
/// The whole window and everything in it, which is what `scale.figure` did before 2026-11-08.
pub const WINDOW: Bounded = Bounded { field: "scale.window", low: 0.6, high: 1.6, default: 1.0, integer: false };
/// Her alone, inside the window, about the bottom of her box (Docs/DESIGN_Settings.md 1). 1.3 at the
/// top, because there her head is at y 162 and the choices' band above it still holds three one-line
/// cards at the largest `scale.ui`, and her box, 416 wide, still fits the 460 window. 0.75 at the
/// bottom, because her feet stay inside the dialogue box at every size, so a smaller figure has more
/// of her behind it, and below 0.75 the box's top edge passes her middle. `pet_page` does both sums.
pub const FIGURE: Bounded = Bounded { field: "scale.figure", low: 0.75, high: 1.3, default: 1.0, integer: false };
pub const UI: Bounded = Bounded { field: "scale.ui", low: 0.8, high: 1.4, default: 1.0, integer: false };
pub const BUBBLE_OPACITY: Bounded =
    Bounded { field: "look.bubble_opacity", low: 0.4, high: 1.0, default: 0.92, integer: false };
pub const FONT_SIZE: Bounded = Bounded { field: "look.font_size", low: 11.0, high: 20.0, default: 13.0, integer: true };

pub const NUMBERS: [Bounded; 6] = [VOLUME, WINDOW, FIGURE, UI, BUBBLE_OPACITY, FONT_SIZE];

/// The six switches and their defaults, in the order the settings window lists them.
pub const SWITCHES: [(&str, bool); 6] = [
    ("voice", true),
    ("fillers", true),
    ("mic", true),
    ("follow", true),
    ("on_top", true),
    ("autostart", false),
];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Settings {
    /// `None` when nothing has been chosen, or when what was saved is not an id. Not written when
    /// `None`, so a bad id in the file stays there to be reported rather than being erased.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub character: Option<Id>,
    pub volume: f64,
    pub scale: Scale,
    pub switches: Switches,
    pub speech: Speech,
    pub stt: SttPack,
    pub chat_model: ChatModel,
    pub look: Look,
    /// The settings window's tab it was last on, so it opens there again. Here and not in the
    /// webview's storage, which lives under the AppData the Claude desktop app redirects.
    pub settings_tab: Tab,
}

/// The settings window's tabs, in the order it shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tab {
    Character,
    Voice,
    Look,
    Switches,
    Models,
}

impl Tab {
    pub const ALL: [Tab; 5] = [Tab::Character, Tab::Voice, Tab::Look, Tab::Switches, Tab::Models];

    /// Its name in the file and on the page's `data-tab`.
    pub fn name(self) -> &'static str {
        match self {
            Tab::Character => "character",
            Tab::Voice => "voice",
            Tab::Look => "look",
            Tab::Switches => "switches",
            Tab::Models => "models",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Scale {
    pub window: f64,
    pub figure: f64,
    pub ui: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Switches {
    pub voice: bool,
    pub fillers: bool,
    pub mic: bool,
    pub follow: bool,
    pub on_top: bool,
    pub autostart: bool,
}

/// Where her voice is synthesised (Docs/DESIGN_Settings.md 4). An enum and not a kind string with
/// optional fields, so a custom service without a URL cannot be built.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Speech {
    Local,
    Custom {
        url: String,
        model: String,
        voice: String,
        /// The NAME of the environment variable that holds the key, never the key. `null` when
        /// written, rather than left out, so a write replaces a name saved before.
        key_env: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SttPack {
    pub pack: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatModel {
    Haiku,
    Sonnet,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Look {
    pub bubble_opacity: f64,
    /// Pixels, whole.
    pub font_size: u32,
    /// The interface theme's folder name (Docs/DESIGN_Themes.md). Whether that theme can be shown
    /// is `themes.rs`'s to say, and it falls back with a reason, so a theme deleted since is not a
    /// fault in this file.
    pub theme: Id,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            character: None,
            volume: VOLUME.default,
            scale: Scale { window: WINDOW.default, figure: FIGURE.default, ui: UI.default },
            switches: Switches { voice: true, fillers: true, mic: true, follow: true, on_top: true, autostart: false },
            speech: Speech::Local,
            stt: SttPack { pack: DEFAULT_PACK.to_string() },
            chat_model: ChatModel::Haiku,
            look: Look { bubble_opacity: BUBBLE_OPACITY.default, font_size: FONT_SIZE.default as u32, theme: default_theme() },
            settings_tab: Tab::Character,
        }
    }
}

fn default_theme() -> Id {
    Id::parse(crate::themes::DEFAULT).expect("the default theme's id follows the folder rule")
}

/// A field that did not parse, and why, for the settings window to show.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Problem {
    pub field: String,
    pub why: String,
}

/// The settings in effect and what was wrong with the file they came from.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Loaded {
    pub settings: Settings,
    pub problems: Vec<Problem>,
}

// ---------------------------------------------------------------------------------------------
// Parsing

/// Whether a missing field is the default (the file: an older pet wrote fewer keys) or a fault (a
/// request: the window always sends every field, so one missing is a bug in the window).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    File,
    Request,
}

struct Reader<'a> {
    root: &'a Value,
    mode: Mode,
    problems: Vec<Problem>,
}

impl<'a> Reader<'a> {
    fn problem(&mut self, field: &str, why: impl Into<String>) {
        self.problems.push(Problem { field: field.to_string(), why: why.into() });
    }

    /// The value at a dotted path, or `None` when it is absent. An absent field is a problem only in
    /// a request. A parent that is there and is not an object was already reported by `objects`.
    fn at(&mut self, field: &str) -> Option<&'a Value> {
        let pointer = format!("/{}", field.replace('.', "/"));
        let found = self.root.pointer(&pointer).filter(|v| !v.is_null());
        if found.is_none() && self.mode == Mode::Request && self.parent_is_object(field) {
            self.problem(field, "沒有給這一項");
        }
        found
    }

    fn parent_is_object(&self, field: &str) -> bool {
        match field.rsplit_once('.') {
            None => true,
            Some((parent, _)) => self.root.pointer(&format!("/{}", parent.replace('.', "/"))).is_some_and(Value::is_object),
        }
    }

    fn number(&mut self, b: Bounded) -> f64 {
        let Some(v) = self.at(b.field) else {
            return b.default;
        };
        let Some(n) = v.as_f64() else {
            self.problem(b.field, format!("{v} 不是數字，改用預設的 {}", b.default));
            return b.default;
        };
        if b.integer && n.fract() != 0.0 {
            self.problem(b.field, format!("{n} 不是整數，改用預設的 {}", b.default));
            return b.default;
        }
        if !(b.low..=b.high).contains(&n) {
            self.problem(b.field, format!("{n} 不在 {} 到 {} 之間，改用預設的 {}", b.low, b.high, b.default));
            return b.default;
        }
        n
    }

    fn switch(&mut self, name: &str, default: bool) -> bool {
        let field = format!("switches.{name}");
        match self.at(&field) {
            None => default,
            Some(Value::Bool(b)) => *b,
            Some(v) => {
                self.problem(&field, format!("{v} 不是 true 或 false，改用預設的 {default}"));
                default
            }
        }
    }

    fn text(&mut self, field: &str) -> Option<&'a str> {
        let v = self.at(field)?;
        match v.as_str() {
            Some(s) => Some(s),
            None => {
                self.problem(field, format!("{v} 不是文字"));
                None
            }
        }
    }

    /// The objects the design nests fields under. One that is there and is not an object is
    /// reported once here, and every field under it then reads as absent.
    fn objects(&mut self) {
        for key in ["scale", "switches", "speech", "stt", "look"] {
            match self.root.get(key).filter(|v| !v.is_null()) {
                Some(v) if !v.is_object() => self.problem(key, format!("{v} 不是物件，裡面每一項都用預設值")),
                None if self.mode == Mode::Request => self.problem(key, "沒有給這一項"),
                _ => {}
            }
        }
    }

    fn speech(&mut self) -> Speech {
        let Some(kind) = self.text("speech.kind") else {
            return Speech::Local;
        };
        match kind {
            "local" => Speech::Local,
            "custom" => {
                let before = self.problems.len();
                let url = self.checked("speech.url", check_url);
                let model = self.checked("speech.model", |s| check_name("模型", s));
                let voice = self.checked("speech.voice", |s| check_name("聲音", s));
                // Optional in both modes: a local service needs no key, and the window sends an
                // empty name or `null` for none.
                let key_env = match self.root.pointer("/speech/key_env").filter(|v| !v.is_null()) {
                    None => None,
                    Some(Value::String(s)) if s.is_empty() => None,
                    Some(Value::String(s)) => match check_env_name(s) {
                        Ok(()) => Some(s.clone()),
                        Err(why) => {
                            self.problem("speech.key_env", why);
                            None
                        }
                    },
                    Some(v) => {
                        self.problem("speech.key_env", format!("{v} 不是文字"));
                        None
                    }
                };
                match (url, model, voice) {
                    (Some(url), Some(model), Some(voice)) if self.problems.len() == before => {
                        Speech::Custom { url, model, voice, key_env }
                    }
                    _ => {
                        // A request is refused whole, so falling back is the file's to say.
                        if self.mode == Mode::File {
                            self.problem("speech", "自訂語音服務的設定不完整，改用本機");
                        }
                        Speech::Local
                    }
                }
            }
            other => {
                self.problem("speech.kind", format!("{other:?} 不是 local 或 custom，改用本機"));
                Speech::Local
            }
        }
    }

    /// A required text field put through `check`, or `None` with the reason recorded.
    fn checked(&mut self, field: &str, check: impl Fn(&str) -> Result<(), String>) -> Option<String> {
        let s = match self.at(field) {
            None => {
                if self.mode == Mode::File {
                    self.problem(field, "沒有填");
                }
                return None;
            }
            Some(v) => match v.as_str() {
                Some(s) => s,
                None => {
                    self.problem(field, format!("{v} 不是文字"));
                    return None;
                }
            },
        };
        match check(s) {
            Ok(()) => Some(s.to_string()),
            Err(why) => {
                self.problem(field, why);
                None
            }
        }
    }

    /// The three sizes. A file with `scale.figure` and no `scale.window` was written before the window
    /// and the figure had a size each, when `scale.figure` sized the window. Read as the window, it
    /// keeps the pet the size the user left it; read as the figure, it would shrink the window to 1.0
    /// and clamp a 1.5 she was at to the figure's default. Only a file is read this way: the settings
    /// window sends both.
    fn scale(&mut self) -> Scale {
        let given = |r: &Reader, field: &str| r.root.pointer(&format!("/{}", field.replace('.', "/"))).is_some_and(|v| !v.is_null());
        if self.mode == Mode::File && given(self, FIGURE.field) && !given(self, WINDOW.field) {
            let window = self.number(Bounded { field: FIGURE.field, ..WINDOW });
            self.problem(
                FIGURE.field,
                format!(
                    "這是舊版寫的：舊版的 scale.figure 是整個視窗的大小，所以讀成 scale.window = {window}，人物大小用預設的 {}。下次存設定時會改成新的寫法",
                    FIGURE.default
                ),
            );
            return Scale { window, figure: FIGURE.default, ui: self.number(UI) };
        }
        Scale { window: self.number(WINDOW), figure: self.number(FIGURE), ui: self.number(UI) }
    }

    fn pack(&mut self) -> String {
        match self.text("stt.pack") {
            None => DEFAULT_PACK.to_string(),
            Some(p) if is_pack_id(p) => p.to_string(),
            Some(p) => {
                self.problem("stt.pack", format!("{p:?} 不是模型包的名稱，改用 SenseVoice"));
                DEFAULT_PACK.to_string()
            }
        }
    }

    fn chat_model(&mut self) -> ChatModel {
        match self.text("chat_model") {
            None | Some("haiku") => ChatModel::Haiku,
            Some("sonnet") => ChatModel::Sonnet,
            Some(other) => {
                self.problem("chat_model", format!("{other:?} 不是 haiku 或 sonnet，改用 haiku"));
                ChatModel::Haiku
            }
        }
    }

    fn theme(&mut self) -> Id {
        match self.text("look.theme").map(Id::parse) {
            None => default_theme(),
            Some(Ok(id)) => id,
            Some(Err(why)) => {
                self.problem("look.theme", format!("{why}，改用預設的 {}", crate::themes::DEFAULT));
                default_theme()
            }
        }
    }

    fn tab(&mut self) -> Tab {
        match self.text("settings_tab") {
            None => Tab::Character,
            Some(name) => Tab::ALL.into_iter().find(|t| t.name() == name).unwrap_or_else(|| {
                self.problem("settings_tab", format!("{name:?} 不是設定視窗的分頁，改用角色"));
                Tab::Character
            }),
        }
    }

    fn character(&mut self) -> Option<Id> {
        // Absent is never a fault: the settings window does not choose her (`Store::set`).
        let v = self.root.get("character").filter(|v| !v.is_null())?;
        let Some(s) = v.as_str() else {
            self.problem("character", format!("{v} 不是文字"));
            return None;
        };
        match Id::parse(s) {
            Ok(id) => Some(id),
            Err(why) => {
                self.problem("character", why);
                None
            }
        }
    }
}

impl Settings {
    /// Every field of `raw` that can be read, the default for every one that cannot, and why.
    pub fn parse(raw: &Value, mode: Mode) -> (Settings, Vec<Problem>) {
        let mut r = Reader { root: raw, mode, problems: Vec::new() };
        if !raw.is_object() {
            r.problem("", "整份設定不是 JSON 物件，全部用預設值");
            return (Settings::default(), r.problems);
        }
        r.objects();
        let switch = |r: &mut Reader, i: usize| r.switch(SWITCHES[i].0, SWITCHES[i].1);
        let settings = Settings {
            character: r.character(),
            volume: r.number(VOLUME),
            scale: r.scale(),
            switches: Switches {
                voice: switch(&mut r, 0),
                fillers: switch(&mut r, 1),
                mic: switch(&mut r, 2),
                follow: switch(&mut r, 3),
                on_top: switch(&mut r, 4),
                autostart: switch(&mut r, 5),
            },
            speech: r.speech(),
            stt: SttPack { pack: r.pack() },
            chat_model: r.chat_model(),
            look: Look { bubble_opacity: r.number(BUBBLE_OPACITY), font_size: r.number(FONT_SIZE) as u32, theme: r.theme() },
            settings_tab: r.tab(),
        };
        (settings, r.problems)
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// `https` anywhere, `http` only to this machine (Docs/DESIGN_Settings.md 4): what she says goes to
/// this address, and in the clear only when it never leaves the computer. No `@`, because a URL
/// that carries credentials puts a secret in the file, and `http://127.0.0.1@elsewhere` is not
/// local whatever it starts with.
///
/// The core reads this address again when it starts (`speech_url` in ChatCore/src/voice.rs) and
/// refuses what it does not take, and then every sentence is silent. So these rules are the core's,
/// not looser: a lower-case scheme, the loopback literals and not `localhost`, no query or fragment.
pub fn check_url(url: &str) -> Result<(), String> {
    if url.len() > 2048 || url.chars().any(|c| c.is_whitespace() || c.is_control()) || url.contains(['?', '#']) {
        return Err("網址太長，或有空白、控制字元、? 或 #".to_string());
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return Err("網址要以 https:// 開頭，本機可以用 http://".to_string());
    };
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() {
        return Err("網址沒有主機名稱".to_string());
    }
    if authority.contains('@') {
        return Err("網址裡不能有帳號密碼（@），金鑰請放在環境變數".to_string());
    }
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split_once(']').map(|(h, _)| h).unwrap_or(v6),
        None => authority.rsplit_once(':').map_or(authority, |(h, _)| h),
    };
    match scheme {
        "https" => Ok(()),
        "http" if ["127.0.0.1", "::1"].contains(&host) => Ok(()),
        "http" => Err(format!("{host} 不是本機（127.0.0.1 或 [::1]），遠端位址必須用 https")),
        other => Err(format!("{other}:// 不是 http 或 https（小寫）")),
    }
}

/// A `model` or `voice` name, by the rule the core reads it with (`name_of` in ChatCore/src/voice.rs):
/// 1 to 64 characters of letters, digits and `. _ - : /`. A name the core refuses would leave every
/// sentence silent, so it is refused here, where the user can still see what they typed.
fn check_name(what: &str, s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err(format!("{what}不能是空的"));
    }
    if s.len() > 64 || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b".-_:/".contains(&b)) {
        return Err(format!("{what}只能有英文、數字和 . _ - : /，最多 64 字"));
    }
    Ok(())
}

/// An environment variable's name: `^[A-Z_][A-Z0-9_]{0,39}$`, the core's rule (`valid_key_env` in
/// ChatCore/src/voice.rs). Strict on purpose, because the likeliest mistake in this field is pasting
/// the key itself, and a key is long and has lower-case letters and a `-` in it.
pub fn check_env_name(s: &str) -> Result<(), String> {
    let mut bytes = s.bytes();
    let first = bytes.next().is_some_and(|b| b.is_ascii_uppercase() || b == b'_');
    if first && bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') && s.len() <= 40 {
        Ok(())
    } else {
        Err("要填的是環境變數的名稱（大寫英文、數字、底線，最多 40 字），不是金鑰本身".to_string())
    }
}

/// `^[a-z0-9][a-z0-9._-]{0,95}$`: a folder name under `stt/packs`. Whether that pack is installed and
/// passes its hash check is the speech service's to say when it starts.
fn is_pack_id(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        && s.len() <= 96
}

// ---------------------------------------------------------------------------------------------
// The file

/// Reads `<home>/settings.json`. A file that is not there is the defaults with nothing to report,
/// because that is every first start.
pub fn read(home: &Path) -> Loaded {
    let path = home.join(FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded::default(),
        Err(e) => {
            let why = format!("讀不到 {}：{e}，全部用預設值", path.display());
            return Loaded { settings: Settings::default(), problems: vec![Problem { field: String::new(), why }] };
        }
    };
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(raw) => {
            let (settings, problems) = Settings::parse(&raw, Mode::File);
            Loaded { settings, problems }
        }
        Err(e) => {
            let why = format!("{FILE} 不是 JSON：{e}，全部用預設值");
            Loaded { settings: Settings::default(), problems: vec![Problem { field: String::new(), why }] }
        }
    }
}

/// Writes `settings` over what the file holds, keeping every key it does not know, at any depth.
/// Read again now and not at startup, so a key another pet wrote since is kept too. Written to a
/// second file and renamed over the first, so a pet that dies mid-write leaves the old file whole.
pub fn write(home: &Path, settings: &Settings) -> Result<(), String> {
    std::fs::create_dir_all(home).map_err(|e| format!("{}: {e}", home.display()))?;
    let path = home.join(FILE);
    let mut all = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| Value::Object(Map::new()));
    merge(&mut all, &settings.to_value());
    let text = serde_json::to_vec_pretty(&all).map_err(|e| e.to_string())?;
    let next = home.join(format!("{FILE}.new"));
    std::fs::write(&next, text).map_err(|e| format!("{}: {e}", next.display()))?;
    std::fs::rename(&next, &path).map_err(|e| format!("{}: {e}", path.display()))
}

/// `from` laid over `into`: objects key by key, anything else replaced.
fn merge(into: &mut Value, from: &Value) {
    match (into, from) {
        (Value::Object(old), Value::Object(new)) => {
            for (key, value) in new {
                match old.get_mut(key) {
                    Some(slot) if slot.is_object() && value.is_object() => merge(slot, value),
                    _ => {
                        old.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (slot, value) => *slot = value.clone(),
    }
}

// ---------------------------------------------------------------------------------------------
// The state the app keeps

/// The settings in effect, and the home they are saved in. One lock around both reading and
/// writing, so the settings window and a character choice cannot interleave two writes.
pub struct Store {
    home: Option<PathBuf>,
    now: Mutex<Loaded>,
}

impl Store {
    /// Reads the file once. With no home there is nowhere to read from or save to, and the pet runs
    /// on the defaults.
    pub fn open(home: Option<PathBuf>) -> Store {
        let loaded = home.as_deref().map(read).unwrap_or_default();
        for p in &loaded.problems {
            eprintln!("settings: {} {}", p.field, p.why);
        }
        Store { home, now: Mutex::new(loaded) }
    }

    pub fn loaded(&self) -> Loaded {
        self.now.lock().expect("settings poisoned").clone()
    }

    pub fn current(&self) -> Settings {
        self.loaded().settings
    }

    /// The window's scale alone, for the poll thread, which asks 25 times a second.
    pub fn window(&self) -> f64 {
        self.now.lock().expect("settings poisoned").settings.scale.window
    }

    /// The settings window's request, whole or not at all. Her character is not the window's to set
    /// here: it changes through `choose`, which also tells the roster and the core, so whatever the
    /// request says about her is replaced by the choice in effect.
    pub fn set(&self, request: &Value) -> Result<Loaded, String> {
        self.set_with(request, |_| Ok(()))
    }

    /// `set`, with `effect` run on the checked settings before they are written. An `Err` from it
    /// refuses the request like a bad field does, so a setting whose change lives outside the file
    /// (the Startup shortcut, `autostart.rs`) is never saved as done when it was not.
    pub fn set_with(&self, request: &Value, effect: impl FnOnce(&Settings) -> Result<(), String>) -> Result<Loaded, String> {
        let (mut next, problems) = Settings::parse(request, Mode::Request);
        if !problems.is_empty() {
            let each: Vec<String> = problems.iter().map(|p| format!("{}：{}", p.field, p.why)).collect();
            return Err(each.join("；"));
        }
        let home = self.home.as_deref().ok_or("桌寵沒有家目錄，設定存不下來")?;
        let mut now = self.now.lock().expect("settings poisoned");
        next.character = now.settings.character.clone();
        effect(&next)?;
        write(home, &next)?;
        // What is still wrong with the file is only what this write did not replace.
        now.problems.retain(|p| p.field == "character");
        now.settings = next;
        Ok(now.clone())
    }

    /// Saves her as the character chosen.
    pub fn choose(&self, id: &Id) -> Result<(), String> {
        let home = self.home.as_deref().ok_or("桌寵沒有家目錄，選擇存不下來")?;
        let mut now = self.now.lock().expect("settings poisoned");
        let mut next = now.settings.clone();
        next.character = Some(id.clone());
        write(home, &next)?;
        now.problems.retain(|p| p.field != "character");
        now.settings = next;
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// The pet's window

/// A rectangle in physical pixels: its top-left corner and its size.
pub type Rect = ((i32, i32), (u32, u32));

/// Where the window goes when it is scaled by `factor`, in physical pixels: `WINDOW_BASE` times the
/// factor and the monitor's scale, with the bottom-left corner where it was. Bottom-left and not
/// top-left, because she stands on the bottom edge and a user who put her on the taskbar expects
/// her feet to stay there.
///
/// Then kept inside `work`, the monitor's work area, when there is one: grown from the bottom, a
/// window near the top of the screen went up past its edge, choices and dialogue with it, where
/// nothing could be read or clicked. A window larger than the work area keeps its top-left inside,
/// because that is where the choices are.
pub fn window_rect(position: (i32, i32), size: (u32, u32), factor: f64, monitor: f64, work: Option<Rect>) -> Rect {
    let w = (WINDOW_BASE.0 * factor * monitor).round() as u32;
    let h = (WINDOW_BASE.1 * factor * monitor).round() as u32;
    let bottom = position.1 + size.1 as i32;
    let (mut x, mut y) = (position.0, bottom - h as i32);
    if let Some(((wx, wy), (ww, wh))) = work {
        x = x.min(wx + ww as i32 - w as i32).max(wx);
        y = y.min(wy + wh as i32 - h as i32).max(wy);
    }
    ((x, y), (w, h))
}

/// Why each child process must start again after `before` became `after`, or `None` for one that
/// keeps running (Docs/DESIGN_Settings.md 4). The core reads her voice service and her model only
/// when it starts, and the speech input service its pack, so a change reaches them only this way.
/// The reason is what the page says while she is quiet.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Restarts {
    pub core: Option<String>,
    pub stt: Option<String>,
}

pub fn restarts(before: &Settings, after: &Settings) -> Restarts {
    let mut core = Vec::new();
    if before.chat_model != after.chat_model {
        core.push(format!("對話模型換成 {}", after.chat_model.name()));
    }
    if before.speech != after.speech {
        core.push(match &after.speech {
            Speech::Local => "語音換回本機".to_string(),
            Speech::Custom { url, .. } => format!("語音換成 {url}"),
        });
    }
    let stt = (before.stt != after.stt).then(|| format!("語音辨識換成 {}", after.stt.pack));
    Restarts { core: (!core.is_empty()).then(|| core.join("，")), stt }
}

impl ChatModel {
    /// Its name on the wire, in the file, and in `VRCHATCORE_MODEL`.
    pub fn name(self) -> &'static str {
        match self {
            ChatModel::Haiku => "haiku",
            ChatModel::Sonnet => "sonnet",
        }
    }
}

/// `<home>/characters`, made if it is not there yet, for the settings window's button that opens it.
pub fn characters_dir(home: &Path) -> Result<PathBuf, String> {
    let dir = home.join(crate::cast::USER_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}
