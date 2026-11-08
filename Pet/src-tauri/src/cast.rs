//! Which characters the pet has, and which of them she is (Docs/DESIGN_Characters.md).
//!
//! A character is a folder: `character.json` says who she is, `rig.json` and `layers.json` and the
//! PNGs say how she is drawn. They come from two places. The bundle lists its own in
//! `ui/characters/index.json`, because neither the page nor Tauri's asset resolver can list a
//! folder. The user's are folders dropped into `<home>/characters/`, read once at startup.
//!
//! The user's folders are untrusted input: they may be someone else's work, downloaded. So every one
//! is checked when it is scanned, and every file the page asks for is checked again when it is
//! served (`Roster::serve`), because the page is not trusted to send only the paths it was given.
//! A folder that fails a check is still listed, with the reason, so a character never vanishes
//! without the user being told why.
//!
//! The choice is kept in `settings.json` in the pet's home (home.rs), not in the webview's storage,
//! which lives under the AppData the Claude desktop app redirects. `settings.rs` is the one reader
//! and writer of that file, so the choice is read and saved through it.

use std::fs::{File, Metadata, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::settings;

pub const INDEX: &str = "characters/index.json";
/// The folder under the pet's home that the user drops characters into.
pub const USER_DIR: &str = "characters";

pub const PROFILE: &str = "character.json";
pub const RIG: &str = "rig.json";
pub const ART: &str = "layers.json";
/// The one name a reference voice may have. The speech service looks for `<id>/voice.wav` and for
/// nothing else (Docs/DESIGN_Characters.md 5), so a profile naming any other file would load and
/// then speak in the default voice without a word about why.
pub const VOICE: &str = "voice.wav";
/// Her thinking fillers in her own voice, in the bank format of `voice/fillers.json`, written by
/// `Scripts/tts_engines/render_fillers.py`.
pub const FILLERS: &str = "fillers.json";

/// The URI scheme user characters are served through. The page reaches it at `ORIGIN`.
pub const SCHEME: &str = "character";
/// What the webview calls `SCHEME`. WebView2 cannot register a scheme of its own, so Tauri serves a
/// custom scheme on Windows as an `http://<scheme>.localhost` origin, and as `<scheme>://localhost`
/// everywhere else.
#[cfg(windows)]
pub const ORIGIN: &str = "http://character.localhost/";
#[cfg(not(windows))]
pub const ORIGIN: &str = "character://localhost/";

const MIB: u64 = 1024 * 1024;
/// The whole folder, every file in it counted, not only the ones the page will ask for.
pub const TOTAL_CAP: u64 = 200 * MIB;

// ---------------------------------------------------------------------------------------------
// The data

/// A character's id, which is its folder's name. Checked once, here, so a value of this type can be
/// joined onto a path anywhere without being checked again: the rule admits no separator, no dot and
/// nothing a filesystem treats specially.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Id(String);

impl Id {
    /// `^[a-z0-9][a-z0-9_-]{0,47}$`.
    pub fn parse(text: &str) -> Result<Id, String> {
        let mut chars = text.chars();
        let first_ok = chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        let rest_ok = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
        if first_ok && rest_ok && text.len() <= 48 {
            Ok(Id(text.to_string()))
        } else {
            Err(format!(
                "資料夾名稱 {text:?} 不合規則：只能用小寫英文、數字、- 和 _，第一個字是英文或數字，最長 48 字"
            ))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Id {
    type Error = String;
    fn try_from(text: String) -> Result<Id, String> {
        Id::parse(&text)
    }
}

impl From<Id> for String {
    fn from(id: Id) -> String {
        id.0
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// `character.json`: who she is and how she talks (Docs/DESIGN_Characters.md 2). An unknown field
/// is refused, so a misspelt key is a reason on the settings list and not a setting that silently
/// does nothing.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub name: String,
    /// Which of her looks this folder is, for two folders of one character (小克 and 小克 in a
    /// gown). Shown beside her name in the settings list only: the dialogue and the core take `name`,
    /// because she is the same person in either.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outfit: Option<String>,
    pub persona: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub greeting: Option<String>,
    /// Always `VOICE` when present (`parse` refuses anything else).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// Absent and empty mean the same thing: nothing for `render_fillers.py` to synthesise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fillers: Vec<String>,
}

impl Profile {
    pub fn parse(bytes: &[u8]) -> Result<Profile, String> {
        let profile: Profile = serde_json::from_slice(bytes).map_err(|e| format!("{PROFILE} 讀不懂：{e}"))?;
        profile.check()?;
        Ok(profile)
    }

    /// The file this profile is written as. Made by `parse`'s own rules on the bytes it produces, so
    /// what the settings window saves is exactly what the next scan accepts, and nothing it refuses
    /// is ever written.
    pub fn to_file(&self) -> Result<Vec<u8>, String> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        Profile::parse(&bytes)?;
        Ok(bytes)
    }

    /// The limits are counted in characters, which is what the design's 字 means, and not in bytes:
    /// a name of 32 CJK characters is 96 bytes and is allowed.
    ///
    /// Three rules are stricter than the design's table, because the core refuses these in its
    /// `character` and `greet` lines (ChatCore/src/checks.rs) and a profile the pet accepts must
    /// never be one the core turns away: a greeting that is present cannot be blank, a name holds no
    /// control character (it is spliced into the prompt as one line), and a greeting holds no square
    /// bracket, which the core reserves for gesture markers.
    fn check(&self) -> Result<(), String> {
        within("name", &self.name, true, 32)?;
        if self.name.chars().any(char::is_control) {
            return Err(format!("{PROFILE} 的 name 不能有換行或其他控制字元"));
        }
        if let Some(outfit) = &self.outfit {
            within("outfit", outfit, true, 16)?;
            if outfit.chars().any(char::is_control) || outfit.contains(['[', ']']) {
                return Err(format!("{PROFILE} 的 outfit 不能有換行、其他控制字元或方括號"));
            }
        }
        within("persona", &self.persona, true, 2000)?;
        if let Some(greeting) = &self.greeting {
            within("greeting", greeting, true, 200)?;
            if greeting.contains(['[', ']']) {
                return Err(format!("{PROFILE} 的 greeting 不能有方括號，方括號是留給手勢標記的"));
            }
        }
        if self.fillers.len() > 12 {
            return Err(format!("{PROFILE} 的 fillers 有 {} 句，最多 12 句", self.fillers.len()));
        }
        for filler in &self.fillers {
            within("fillers", filler, true, 40)?;
        }
        match self.voice.as_deref() {
            None | Some(VOICE) => Ok(()),
            Some(other) => Err(format!("{PROFILE} 的 voice 是 {other:?}，只能是 {VOICE:?}，語音服務只找這個檔名")),
        }
    }
}

fn within(field: &str, text: &str, required: bool, max: usize) -> Result<(), String> {
    if required && text.trim().is_empty() {
        return Err(format!("{PROFILE} 的 {field} 不能是空的"));
    }
    let n = text.chars().count();
    if n > max {
        return Err(format!("{PROFILE} 的 {field} 有 {n} 字，最多 {max} 字"));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    Bundled,
    User,
}

/// A character that passed every check and can be shown. One that did not is a `Refused`, which
/// has no profile to read by mistake.
#[derive(Debug, Clone, PartialEq)]
pub struct Character {
    pub id: Id,
    pub source: Source,
    pub profile: Profile,
}

impl Character {
    /// Where the page fetches her rig. The bundle's through the page's own origin, the user's
    /// through the `character` scheme.
    pub fn rig_url(&self) -> String {
        match self.source {
            Source::Bundled => bundled_file(&self.id, RIG),
            Source::User => format!("{ORIGIN}{}/{RIG}", self.id),
        }
    }

    pub fn listed(&self) -> Listed {
        Listed {
            id: self.id.clone(),
            source: self.source,
            name: self.profile.name.clone(),
            outfit: self.profile.outfit.clone(),
            greeting: self.profile.greeting.clone(),
            rig: self.rig_url(),
        }
    }
}

/// A character as the page sees it. The persona is not in it: the page has no use for it, and the
/// core gets it from Rust (`Roster::speaking`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Listed {
    pub id: Id,
    pub source: Source,
    pub name: String,
    /// For the settings list's label only; the pet's page names her by `name`.
    pub outfit: Option<String>,
    pub greeting: Option<String>,
    pub rig: String,
}

/// A folder that could not be loaded, and why. `id` is the folder's name as it was found, which is
/// not an `Id` when the name is the reason.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Refused {
    pub id: String,
    pub source: Source,
    pub why: String,
}

/// The kinds of file a character folder may serve, each with its own cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Png,
    Json,
    Wav,
}

impl Kind {
    /// By extension, in any case, because Windows opens `A.PNG` for `a.png` too.
    pub fn of(file: &str) -> Option<Kind> {
        let (_, ext) = file.rsplit_once('.')?;
        match ext.to_ascii_lowercase().as_str() {
            "png" => Some(Kind::Png),
            "json" => Some(Kind::Json),
            "wav" => Some(Kind::Wav),
            _ => None,
        }
    }

    /// `fillers.json` is the one JSON that holds audio, base64 at about 43.5 KB per second, so the
    /// 1 MiB that fits a rig would refuse twelve lines of a few seconds each, and with them the
    /// whole character.
    pub fn cap(self, file: &str) -> u64 {
        match self {
            Kind::Png => 16 * MIB,
            Kind::Json if file.eq_ignore_ascii_case(FILLERS) => 8 * MIB,
            Kind::Json => MIB,
            Kind::Wav => 10 * MIB,
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Kind::Png => "image/png",
            Kind::Json => "application/json",
            Kind::Wav => "audio/wav",
        }
    }
}

/// Why a file was not served, split the way the page can tell apart: a file that is not there, and
/// one that is there and may not be read.
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    Missing(String),
    Forbidden(String),
}

impl Refusal {
    pub fn status(&self) -> u16 {
        match self {
            Refusal::Missing(_) => 404,
            Refusal::Forbidden(_) => 403,
        }
    }

    pub fn why(&self) -> &str {
        match self {
            Refusal::Missing(why) | Refusal::Forbidden(why) => why,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The bundle

/// `characters/index.json`. Ids only: each character's name is in its own `character.json`, which
/// is the one place a name is written, so the list and the dialogue cannot disagree about it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub default: Id,
    pub characters: Vec<Id>,
}

impl Index {
    pub fn parse(bytes: &[u8]) -> Result<Index, String> {
        let index: Index = serde_json::from_slice(bytes).map_err(|e| format!("{INDEX}: {e}"))?;
        if !index.characters.contains(&index.default) {
            return Err(format!("{INDEX}: the default {:?} is not in the list", index.default.as_str()));
        }
        Ok(index)
    }
}

/// A bundled file's path, as the page and the asset resolver both name it.
pub fn bundled_file(id: &Id, file: &str) -> String {
    format!("characters/{id}/{file}")
}

/// A bundled character, read through `read` (the asset resolver, or the folder in a test). A
/// failure here is a bug in the commit that built the bundle, and it is listed like any other so
/// that it is seen.
fn bundled(read: &dyn Fn(&str) -> Option<Vec<u8>>, id: &Id) -> Result<Character, String> {
    for file in [RIG, ART] {
        read(&bundled_file(id, file)).ok_or_else(|| format!("缺 {file}"))?;
    }
    let bytes = read(&bundled_file(id, PROFILE)).ok_or_else(|| format!("缺 {PROFILE}"))?;
    let profile = Profile::parse(&bytes)?;
    if profile.voice.is_some() && read(&bundled_file(id, VOICE)).is_none() {
        return Err(format!("{PROFILE} 指定了 {VOICE}，但資料夾裡沒有"));
    }
    Ok(Character { id: id.clone(), source: Source::Bundled, profile })
}

// ---------------------------------------------------------------------------------------------
// The user's folders

/// A link of any kind. `is_symlink` covers symlinks and junctions on Windows; the attribute covers
/// every other reparse point as well, none of which a character folder has a reason to hold.
fn is_link(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    meta.file_type().is_symlink()
}

/// A file name the page may ask for: one name, no path, and an extension on the allowlist.
///
/// `:` is here because on Windows `rig.json:x` names a stream inside `rig.json`, and a drive letter
/// begins with one. `..` is refused anywhere in the name and not only as a whole segment, which also
/// covers a name that Windows would trim to something else.
pub fn file_kind(file: &str) -> Result<Kind, String> {
    if file.is_empty() {
        return Err("沒有檔名".to_string());
    }
    if file.contains(['/', '\\', ':']) || file.contains("..") || file.chars().any(char::is_control) {
        return Err(format!("{file:?} 不是單一檔名"));
    }
    Kind::of(file).ok_or_else(|| format!("{file:?} 不是 .png、.json 或 .wav"))
}

/// Reads one file of a user's character with every check the design asks for
/// (Docs/DESIGN_Characters.md 4). The scan, the `character` scheme and the core's gesture binding all
/// read through here, so there is one set of checks and not three that could drift.
pub fn read_user(dir: &Path, id: &Id, file: &str) -> Result<(Vec<u8>, Kind), Refusal> {
    let kind = file_kind(file).map_err(Refusal::Forbidden)?;
    read_in(dir, id, file, kind.cap(file)).map(|bytes| (bytes, kind))
}

/// One file of the user's folder `<dir>/<id>`, at most `cap` bytes, for any kind of folder the user
/// drops in: a character here, a theme in `themes.rs`. `file` must already be known to be a single
/// name with an allowed extension, which each kind of folder decides for itself.
///
/// The file is opened without following a reparse point and its handle is what gets checked, so a
/// file swapped for a link between a check and the open is still refused. The folder above it is
/// checked by path, which leaves a race on the user's own disk that only a local process could run.
pub fn read_in(dir: &Path, id: &Id, file: &str, cap: u64) -> Result<Vec<u8>, Refusal> {
    let folder = dir.join(id.as_str());
    let meta = std::fs::symlink_metadata(&folder).map_err(|_| Refusal::Missing(format!("沒有 {id} 這個資料夾")))?;
    if is_link(&meta) {
        return Err(Refusal::Forbidden(format!("{id} 是捷徑，不跟隨")));
    }
    if !meta.is_dir() {
        return Err(Refusal::Missing(format!("{id} 不是資料夾")));
    }

    let path = folder.join(file);
    let mut open = OpenOptions::new();
    open.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        open.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let handle: File = open.open(&path).map_err(|_| Refusal::Missing(format!("{id} 沒有 {file}")))?;
    let meta = handle.metadata().map_err(|e| Refusal::Missing(format!("{id}/{file}: {e}")))?;
    if is_link(&meta) {
        return Err(Refusal::Forbidden(format!("{id}/{file} 是捷徑，不跟隨")));
    }
    if !meta.is_file() {
        return Err(Refusal::Forbidden(format!("{id}/{file} 不是一般檔案")));
    }
    let too_big = || Refusal::Forbidden(over(&format!("{id}/{file}"), meta.len(), cap));
    if meta.len() > cap {
        return Err(too_big());
    }
    // Bounded again while reading, because the length above was true when it was read and a file
    // can grow after that.
    let mut bytes = Vec::new();
    handle.take(cap + 1).read_to_end(&mut bytes).map_err(|e| Refusal::Missing(format!("{id}/{file}: {e}")))?;
    if bytes.len() as u64 > cap {
        return Err(too_big());
    }
    Ok(bytes)
}

/// A size over its cap, in MB with one decimal, or in KB when the cap is under a megabyte, so a
/// theme's 64 KB cap does not read as "0 MB".
pub fn over(what: &str, len: u64, cap: u64) -> String {
    if cap < MIB {
        format!("{what} 有 {:.1} KB，超過 {} KB 的上限", len as f64 / 1024.0, cap / 1024)
    } else {
        format!("{what} 有 {:.1} MB，超過 {} MB 的上限", len as f64 / MIB as f64, cap / MIB)
    }
}

/// Every folder in `dir`, in name order, each either a character or the reason it is not one. A
/// plain file beside the folders is not a character and is passed over without a word; a link is
/// listed, because it may well be a character the user expects to see.
pub fn scan(dir: &Path, bundled: &[Id]) -> (Vec<Character>, Vec<Refused>) {
    scan_with(dir, bundled, "角色", inspect)
}

/// `scan` for any kind of folder the user drops in, `what` naming the kind in the reasons and
/// `inspect` deciding whether one folder, whose name is an id and which is a real folder, is one.
pub fn scan_with<T>(dir: &Path, bundled: &[Id], what: &str, inspect: impl Fn(&Path, Id) -> Result<T, String>) -> (Vec<T>, Vec<Refused>) {
    let mut found = Vec::new();
    let mut refused = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (found, refused);
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        let link = is_link(&meta);
        if !link && !meta.is_dir() {
            continue;
        }
        let result = Id::parse(&name).and_then(|id| {
            if link {
                Err("這個資料夾是捷徑（symlink 或 junction），不跟隨".to_string())
            } else if bundled.contains(&id) {
                Err(format!("和內建{what}同名，不載入"))
            } else {
                inspect(dir, id)
            }
        });
        match result {
            Ok(one) => found.push(one),
            Err(why) => refused.push(Refused { id: name, source: Source::User, why }),
        }
    }
    (found, refused)
}

/// The regular files at the top of the user's folder `<dir>/<id>`, in lower case, because Windows
/// opens `Rig.json` when the page asks for `rig.json`. Refused when any entry there is a link, when
/// a file is over the cap `cap_of` gives its name (`None` is a file the folder may hold and nothing
/// will read), or when all of them together are over `total`; `whole` names the folder in that
/// reason.
pub fn files_in(dir: &Path, id: &Id, cap_of: impl Fn(&str) -> Option<u64>, total: u64, whole: &str) -> Result<Vec<String>, String> {
    let folder = dir.join(id.as_str());
    let entries = std::fs::read_dir(&folder).map_err(|e| format!("讀不到資料夾：{e}"))?;
    let mut sum = 0u64;
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let meta = std::fs::symlink_metadata(entry.path()).map_err(|e| format!("{name}: {e}"))?;
        if is_link(&meta) {
            return Err(format!("{name} 是捷徑（symlink 或 junction），不跟隨"));
        }
        if !meta.is_file() {
            continue;
        }
        sum += meta.len();
        if let Some(cap) = cap_of(&name)
            && meta.len() > cap
        {
            return Err(over(&name, meta.len(), cap));
        }
        files.push(name.to_lowercase());
    }
    if sum > total {
        return Err(over(whole, sum, total));
    }
    Ok(files)
}

/// One user folder, whose id has passed and which is a real folder.
fn inspect(dir: &Path, id: Id) -> Result<Character, String> {
    let files = files_in(dir, &id, |name| Kind::of(name).map(|k| k.cap(name)), TOTAL_CAP, "整個角色")?;
    for required in [PROFILE, RIG, ART] {
        if !files.iter().any(|f| f == required) {
            return Err(format!("缺 {required}"));
        }
    }
    let (bytes, _) = read_user(dir, &id, PROFILE).map_err(|r| r.why().to_string())?;
    let profile = Profile::parse(&bytes)?;
    if profile.voice.is_some() && !files.iter().any(|f| f == VOICE) {
        return Err(format!("{PROFILE} 指定了 {VOICE}，但資料夾裡沒有"));
    }
    Ok(Character { id, source: Source::User, profile })
}

// ---------------------------------------------------------------------------------------------
// Writing the user's folders (Docs/DESIGN_Settings.md 3)

/// What the settings window may change in a profile. Her voice is not in it: it names a file in her
/// folder, which the window cannot put there, so it is kept as the folder has it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    pub name: String,
    /// Empty is no outfit.
    pub outfit: String,
    pub persona: String,
    /// Empty is no greeting.
    pub greeting: String,
    pub fillers: Vec<String>,
}

/// Writes `profile` as `<dir>/<id>/character.json`. Checked by `Profile::to_file` first, then
/// written to a second file and renamed over the first, so a pet that dies mid-write leaves her old
/// profile whole. The folder must be a real folder and not a link, for the reason `read_user` gives.
pub fn write_profile(dir: &Path, id: &Id, profile: &Profile) -> Result<(), String> {
    let bytes = profile.to_file()?;
    let folder = dir.join(id.as_str());
    let meta = std::fs::symlink_metadata(&folder).map_err(|_| format!("沒有 {id} 這個資料夾"))?;
    if is_link(&meta) || !meta.is_dir() {
        return Err(format!("{id} 不是一般的資料夾，不寫進去"));
    }
    let next = folder.join(format!("{PROFILE}.new"));
    std::fs::write(&next, &bytes).map_err(|e| format!("{}: {e}", next.display()))?;
    std::fs::rename(&next, folder.join(PROFILE)).map_err(|e| format!("{}: {e}", folder.join(PROFILE).display()))
}

/// Every file of the bundled character `id`, read through `read`: the three every character has,
/// every image its `layers.json` names, and her voice and fillers when she has them. The bundle
/// cannot be listed, so this is the list, and an image the art names and the bundle lacks is an
/// error rather than a copy that will not draw.
fn bundled_files(read: &dyn Fn(&str) -> Option<Vec<u8>>, id: &Id) -> Result<Vec<(String, Vec<u8>)>, String> {
    let need = |file: &str| read(&bundled_file(id, file)).ok_or_else(|| format!("內建的 {id} 缺 {file}"));
    let art = need(ART)?;
    let layers: serde_json::Value = serde_json::from_slice(&art).map_err(|e| format!("{ART}: {e}"))?;
    let mut names: Vec<String> = vec![PROFILE.to_string(), RIG.to_string()];
    for layer in layers.get("layers").and_then(serde_json::Value::as_array).into_iter().flatten() {
        if let Some(file) = layer.get("file").and_then(serde_json::Value::as_str)
            && !names.iter().any(|n| n == file)
        {
            names.push(file.to_string());
        }
    }
    let mut files = vec![(ART.to_string(), art)];
    for name in names {
        file_kind(&name).map_err(|why| format!("{ART} 裡的 {why}"))?;
        files.push((name.clone(), need(&name)?));
    }
    for optional in [VOICE, FILLERS] {
        if let Some(bytes) = read(&bundled_file(id, optional)) {
            files.push((optional.to_string(), bytes));
        }
    }
    Ok(files)
}

/// Copies the bundled character `from` to `<dir>/<to>/`, so the user has a copy of her own to edit.
/// The new folder is made with `create_dir`, which fails when anything is already there, so an
/// existing folder is refused and never written into. A copy that fails partway is removed, since
/// this made it and a half copy would only be listed as broken.
pub fn copy_bundled(read: &dyn Fn(&str) -> Option<Vec<u8>>, from: &Id, dir: &Path, to: &Id) -> Result<(), String> {
    let files = bundled_files(read, from)?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let folder = dir.join(to.as_str());
    std::fs::create_dir(&folder).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => format!("{} 已經存在，換一個名稱", folder.display()),
        _ => format!("{}: {e}", folder.display()),
    })?;
    let written = files.iter().try_for_each(|(name, bytes)| {
        let path = folder.join(name);
        std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))
    });
    if written.is_err() {
        let _ = std::fs::remove_dir_all(&folder);
    }
    written
}

/// Turns `%XX` back into the byte it stands for. The page's URLs are built from file names in
/// `layers.json`, and the webview escapes a space or a CJK character in them before the request
/// reaches here. A decoded `/` or `\` is refused afterwards like any other.
pub fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = bytes.get(at + 1..at + 3).filter(|h| h.iter().all(u8::is_ascii_hexdigit))?;
            out.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

// ---------------------------------------------------------------------------------------------
// The choice

/// Every character that can be shown, bundled first, and every folder that could not be.
#[derive(Debug, Clone)]
pub struct Found {
    pub default: Id,
    pub characters: Vec<Character>,
    pub refused: Vec<Refused>,
}

impl Found {
    /// `read` is the bundle; `user_dir` is `<home>/characters`, or `None` with no home.
    pub fn load(read: &dyn Fn(&str) -> Option<Vec<u8>>, user_dir: Option<&Path>) -> Result<Found, String> {
        let index = Index::parse(&read(INDEX).ok_or_else(|| format!("{INDEX} is not in the bundle"))?)?;
        let mut characters = Vec::new();
        let mut refused = Vec::new();
        for id in &index.characters {
            match bundled(read, id) {
                Ok(c) => characters.push(c),
                Err(why) => refused.push(Refused { id: id.to_string(), source: Source::Bundled, why }),
            }
        }
        if !characters.iter().any(|c| c.id == index.default) {
            let why = refused.iter().find(|r| r.id == index.default.as_str()).map_or("", |r| r.why.as_str());
            return Err(format!("{INDEX}: the default {} cannot be shown: {why}", index.default));
        }
        if let Some(dir) = user_dir {
            let (users, bad) = scan(dir, &index.characters);
            characters.extend(users);
            refused.extend(bad);
        }
        Ok(Found { default: index.default, characters, refused })
    }

    pub fn find(&self, id: &str) -> Option<&Character> {
        self.characters.iter().find(|c| c.id.as_str() == id)
    }

    /// The saved choice while it can still be shown, and the default otherwise, so a character
    /// removed from a later build, or a user folder deleted or broken since, falls back instead of
    /// leaving her blank.
    pub fn pick(&self, saved: Option<&str>) -> &Character {
        saved.and_then(|id| self.find(id)).or_else(|| self.find(self.default.as_str())).expect("load checked the default can be shown")
    }
}

/// What the core needs to be her: the profile, and the id the speech service clones a voice from
/// when she has one of her own (Docs/DESIGN_Characters.md 5).
#[derive(Debug, Clone, PartialEq)]
pub struct Speaking {
    pub profile: Profile,
    /// `None` without a `voice.wav`, and then the core is sent no voice at all, which it reads as
    /// the default voice. Sending the id anyway would ask the speech service for a file that is not
    /// there, and it answers that with a 400.
    pub voice: Option<Id>,
}

/// The characters and the one shown now. `shown` changes when the page chooses another, and the
/// core reads it to bind the gestures to the rig on screen. `found` changes when the settings window
/// writes a profile or copies a character, which scans the folders again (`rescan`).
pub struct Roster {
    found: Mutex<Result<Found, String>>,
    shown: Mutex<Option<Id>>,
    home: Option<PathBuf>,
}

impl Roster {
    /// She is the choice `store` holds, or the default when it cannot be shown (`Found::pick`).
    pub fn load(read: &dyn Fn(&str) -> Option<Vec<u8>>, home: Option<PathBuf>, store: &settings::Store) -> Roster {
        let user_dir = home.as_ref().map(|h| h.join(USER_DIR));
        let found = Found::load(read, user_dir.as_deref());
        let saved = store.current().character;
        let shown = found.as_ref().ok().map(|f| f.pick(saved.as_ref().map(Id::as_str)).id.clone());
        Roster { found: Mutex::new(found), shown: Mutex::new(shown), home }
    }

    /// What the last scan found. A copy, so no caller holds the lock while it reads files.
    pub fn found(&self) -> Result<Found, String> {
        self.found.lock().expect("roster poisoned").clone()
    }

    /// Reads both sources again, as a start does. She stays the one shown while she can still be
    /// shown; one whose folder is gone or now refused falls back like a saved choice does at start.
    pub fn rescan(&self, read: &dyn Fn(&str) -> Option<Vec<u8>>) {
        let user_dir = self.home.as_ref().map(|h| h.join(USER_DIR));
        *self.found.lock().expect("roster poisoned") = Found::load(read, user_dir.as_deref());
    }

    pub fn shown(&self) -> Option<Character> {
        let found = self.found().ok()?;
        let id = self.shown.lock().expect("roster poisoned").clone();
        Some(found.pick(id.as_ref().map(Id::as_str)).clone())
    }

    /// What `core::character_line` is built from, each time a core starts and each time she changes.
    pub fn speaking(&self) -> Option<Speaking> {
        let c = self.shown()?;
        let voice = c.profile.voice.is_some().then(|| c.id.clone());
        Some(Speaking { profile: c.profile, voice })
    }

    /// Saved first and shown after, so a choice that could not be saved does not stand until the
    /// next start quietly undoes it.
    pub fn choose(&self, id: &str, store: &settings::Store) -> Result<(), String> {
        let found = self.found()?;
        let c = found.find(id).ok_or_else(|| format!("{id:?} is not a character that can be shown"))?;
        store.choose(&c.id)?;
        *self.shown.lock().expect("roster poisoned") = Some(c.id.clone());
        Ok(())
    }

    /// Writes the settings window's edit of the user character `id` over her `character.json`, her
    /// voice kept as it was. A bundled character is refused: the bundle is the build's, and an edit
    /// to it would be lost on the next one (Docs/DESIGN_Settings.md 3). Not rescanned here; the
    /// caller does that once, after.
    pub fn save_profile(&self, id: &str, edit: Edit) -> Result<(), String> {
        let found = self.found()?;
        let c = found.find(id).ok_or_else(|| format!("{id:?} 不是能顯示的角色"))?;
        if c.source != Source::User {
            return Err(format!("{} 是內建角色，不能改。先複製到我的資料夾，再改那一份", c.profile.name));
        }
        let home = self.home.as_ref().ok_or("桌寵沒有家目錄，存不下角色設定")?;
        let profile = Profile {
            name: edit.name,
            outfit: Some(edit.outfit).filter(|o| !o.is_empty()),
            persona: edit.persona,
            greeting: Some(edit.greeting).filter(|g| !g.is_empty()),
            voice: c.profile.voice.clone(),
            fillers: edit.fillers,
        };
        write_profile(&home.join(USER_DIR), &c.id, &profile)
    }

    /// Copies the bundled character `from` into the user's folder as `to` (`copy_bundled`). `to`
    /// must not be the id of any character there is, bundled or the user's, or of a folder already
    /// in the user's folder. Not rescanned here, for the reason `save_profile` gives.
    pub fn copy(&self, read: &dyn Fn(&str) -> Option<Vec<u8>>, from: &str, to: &str) -> Result<Id, String> {
        let to = Id::parse(to)?;
        let found = self.found()?;
        let c = found.find(from).ok_or_else(|| format!("{from:?} 不是能顯示的角色"))?;
        if c.source != Source::Bundled {
            return Err(format!("{} 已經在你的資料夾裡了，直接改它就好", c.profile.name));
        }
        if found.find(to.as_str()).is_some() || found.refused.iter().any(|r| r.id == to.as_str()) {
            return Err(format!("已經有叫 {to} 的角色了，換一個名稱"));
        }
        let home = self.home.as_ref().ok_or("桌寵沒有家目錄，複製不了")?;
        copy_bundled(read, &c.id, &home.join(USER_DIR), &to)?;
        Ok(to)
    }

    /// One file of a user character, by the `character` scheme's path, `/<id>/<file>`. Only a
    /// character the scan accepted is served, and `read_user` checks the file again now.
    pub fn serve(&self, path: &str) -> Result<(Vec<u8>, Kind), Refusal> {
        let (id, file) = path
            .strip_prefix('/')
            .and_then(|p| p.split_once('/'))
            .ok_or_else(|| Refusal::Forbidden(format!("{path:?} is not /<id>/<file>")))?;
        let file = percent_decode(file).ok_or_else(|| Refusal::Forbidden(format!("{file:?} is not a valid escaped name")))?;
        let c = self
            .found()
            .ok()
            .and_then(|f| f.find(id).cloned())
            .filter(|c| c.source == Source::User)
            .ok_or_else(|| Refusal::Missing(format!("{id:?} is not a user character that was loaded")))?;
        self.read_user(&c.id, &file)
    }

    /// One file of `c`'s folder, wherever she comes from: the bundle's through `bundle` (the asset
    /// resolver), the user's through `read_user`. `Ok(None)` is a file she does not have, which is
    /// ordinary; `Err` is one she has that may not be read, which is worth a line in the log.
    pub fn own_file(&self, c: &Character, file: &str, bundle: &dyn Fn(&str) -> Option<Vec<u8>>) -> Result<Option<Vec<u8>>, String> {
        match c.source {
            Source::Bundled => Ok(bundle(&bundled_file(&c.id, file))),
            Source::User => match self.read_user(&c.id, file) {
                Ok((bytes, _)) => Ok(Some(bytes)),
                Err(Refusal::Missing(_)) => Ok(None),
                Err(refusal) => Err(refusal.why().to_string()),
            },
        }
    }

    pub fn read_user(&self, id: &Id, file: &str) -> Result<(Vec<u8>, Kind), Refusal> {
        let home = self.home.as_ref().ok_or_else(|| Refusal::Missing("the pet has no home".to_string()))?;
        read_user(&home.join(USER_DIR), id, file)
    }
}
