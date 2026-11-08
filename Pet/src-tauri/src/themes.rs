//! The interface's look, as packs the user can switch between and drop in (Docs/DESIGN_Themes.md).
//!
//! A theme is a folder: `theme.json` gives its name, five colours and a corner radius, and may name
//! an icon and a paper texture beside it. They come from two places, the way characters do
//! (`cast.rs`): the bundle lists its own in `ui/themes/index.json`, and the user's are folders
//! dropped into `<home>/themes/`.
//!
//! A theme carries no CSS. The page maps the parsed colours and radius onto custom properties of its
//! own (index.html `applyTheme`), so a theme can recolour the dialogue and nothing else: it cannot
//! move a box, hide a button, or reach anything outside the page. Its two images are used only as
//! CSS backgrounds and `<img>`, where an SVG runs no script and loads nothing from elsewhere.
//!
//! The user's folders are untrusted input, checked when scanned and again on every request through
//! the `theme` scheme (`Themes::serve`), with the same file checks as a character's
//! (`cast::files_in`, `cast::read_in`). A folder that fails is listed with the reason.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::cast::{self, Id, Refusal, Refused, Source};

pub const INDEX: &str = "themes/index.json";
/// The folder under the pet's home that the user drops themes into.
pub const USER_DIR: &str = "themes";
pub const FILE: &str = "theme.json";
/// The theme a fresh pet shows, and the one a missing choice falls back to. `pet_settings` checks
/// the bundle's index names the same one.
pub const DEFAULT: &str = "paper";

/// The URI scheme user themes are served through, at `ORIGIN` (for the reason `cast::ORIGIN` gives).
pub const SCHEME: &str = "theme";
#[cfg(windows)]
pub const ORIGIN: &str = "http://theme.localhost/";
#[cfg(not(windows))]
pub const ORIGIN: &str = "theme://localhost/";

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
/// `theme.json` is a dozen short fields, about 300 bytes. 64 KiB is two hundred times that and
/// still bounds what the scan parses for every folder at every start.
pub const JSON_CAP: u64 = 64 * KIB;
/// The bundled icon is 0.4 KB and the paper 0.7 KB, and a 256 px PNG tile is about 200 KB. 2 MiB is
/// ten times the tile, and bounds what the page decodes for a picture drawn at 16 px or tiled.
pub const IMAGE_CAP: u64 = 2 * MIB;
/// The whole folder, every file counted: `theme.json` and two images at their caps, with room for a
/// README or a preview beside them that nothing reads.
pub const TOTAL_CAP: u64 = 8 * MIB;
/// The radius's range, in CSS pixels: 0 is square, and past 28 the 40 px text field and the 22 px
/// buttons, whose radius is twice this, are pills already and the boxes' corners eat their text.
pub const RADIUS_MAX: u8 = 28;

// ---------------------------------------------------------------------------------------------
// The data

/// `#rrggbb`, kept in lower case. Six hex digits and nothing else, so a colour can be written into a
/// CSS custom property without being able to close it or add a second declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Color(String);

impl TryFrom<String> for Color {
    type Error = String;
    fn try_from(text: String) -> Result<Color, String> {
        let hex = text.strip_prefix('#').filter(|h| h.len() == 6 && h.bytes().all(|b| b.is_ascii_hexdigit()));
        match hex {
            Some(_) => Ok(Color(text.to_ascii_lowercase())),
            None => Err(format!("顏色 {text:?} 要寫成 #rrggbb，六個十六進位數字")),
        }
    }
}

impl From<Color> for String {
    fn from(c: Color) -> String {
        c.0
    }
}

/// The five colours, each a role on the page and not a selector in it (Docs/DESIGN_Themes.md 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Colors {
    /// Hover and focus borders, and her name, mixed toward `ink`.
    pub accent: Color,
    /// The dialogue box, the choices and the history.
    pub paper: Color,
    /// Text on the paper, and the faint marks mixed from it toward `paper`.
    pub ink: Color,
    pub border: Color,
    /// The buttons in the strip and the text field.
    pub controls: Color,
}

/// `theme.json`. Unknown fields are refused, as `character.json`'s are, so a misspelt key is a
/// reason on the list and not a setting that silently does nothing.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeFile {
    pub name: String,
    pub colors: Colors,
    #[serde(deserialize_with = "radius")]
    pub radius: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paper_texture: Option<String>,
}

/// Read as any number and then checked, so a radius of -1, 4.5 or 300 is refused with a reason that
/// names the field, rather than serde's "expected u8", which does not.
fn radius<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
    let v = f64::deserialize(d)?;
    if v.fract() == 0.0 && (0.0..=f64::from(RADIUS_MAX)).contains(&v) {
        Ok(v as u8)
    } else {
        Err(serde::de::Error::custom(format!("radius 是 {v}，只能是 0 到 {RADIUS_MAX} 的整數")))
    }
}

impl ThemeFile {
    pub fn parse(bytes: &[u8]) -> Result<ThemeFile, String> {
        let file: ThemeFile = serde_json::from_slice(bytes).map_err(|e| format!("{FILE} 讀不懂：{e}"))?;
        let n = file.name.chars().count();
        if file.name.trim().is_empty() || n > 32 || file.name.chars().any(char::is_control) {
            return Err(format!("{FILE} 的 name 要有 1 到 32 字，不能有換行或其他控制字元"));
        }
        for (field, image) in [("icon", &file.icon), ("paper_texture", &file.paper_texture)] {
            if let Some(name) = image {
                image_kind(name).map_err(|why| format!("{FILE} 的 {field}：{why}"))?;
            }
        }
        Ok(file)
    }

    /// The images this file names, which the folder must hold.
    fn images(&self) -> impl Iterator<Item = &str> {
        [self.icon.as_deref(), self.paper_texture.as_deref()].into_iter().flatten()
    }
}

/// The kinds of file a theme folder may serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Json,
    Svg,
    Png,
}

impl Kind {
    pub fn cap(self) -> u64 {
        match self {
            Kind::Json => JSON_CAP,
            Kind::Svg | Kind::Png => IMAGE_CAP,
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Kind::Json => "application/json",
            Kind::Svg => "image/svg+xml",
            Kind::Png => "image/png",
        }
    }

    fn of(file: &str) -> Option<Kind> {
        let (_, ext) = file.rsplit_once('.')?;
        match ext.to_ascii_lowercase().as_str() {
            "json" => Some(Kind::Json),
            "svg" => Some(Kind::Svg),
            "png" => Some(Kind::Png),
            _ => None,
        }
    }
}

/// An image a theme may name: `^[a-z0-9][a-z0-9_.-]{0,63}\.(svg|png)$`, no `..`. Stricter than a
/// character's file names, because this one is written into a CSS `url("…")` on the page, and a
/// quote, a bracket, a space or a backslash there could end the URL and start another.
pub fn image_kind(file: &str) -> Result<Kind, String> {
    let mut chars = file.chars();
    let first = chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'));
    match Kind::of(file) {
        Some(kind @ (Kind::Svg | Kind::Png)) if first && rest && file.len() <= 68 && !file.contains("..") => Ok(kind),
        _ => Err(format!("{file:?} 要是資料夾裡的一個 .svg 或 .png，檔名只能用小寫英文、數字、_ . -")),
    }
}

/// A file the `theme` scheme may serve: `theme.json` or an image by `image_kind`'s rule.
fn file_kind(file: &str) -> Result<Kind, String> {
    if file == FILE { Ok(Kind::Json) } else { image_kind(file) }
}

/// A theme that passed every check.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub id: Id,
    pub source: Source,
    pub file: ThemeFile,
}

impl Theme {
    /// Where the page fetches one of its files: the bundle's through the page's own origin, the
    /// user's through the `theme` scheme.
    pub fn url(&self, file: &str) -> String {
        match self.source {
            Source::Bundled => bundled_file(&self.id, file),
            Source::User => format!("{ORIGIN}{}/{file}", self.id),
        }
    }

    pub fn listed(&self) -> Listed {
        Listed {
            id: self.id.clone(),
            source: self.source,
            name: self.file.name.clone(),
            colors: self.file.colors.clone(),
            radius: self.file.radius,
            icon: self.file.icon.as_deref().map(|f| self.url(f)),
            paper_texture: self.file.paper_texture.as_deref().map(|f| self.url(f)),
        }
    }
}

/// A theme as the pages see it: its values, and URLs in place of its file names.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Listed {
    pub id: Id,
    pub source: Source,
    pub name: String,
    pub colors: Colors,
    pub radius: u8,
    pub icon: Option<String>,
    pub paper_texture: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// The two sources

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub default: Id,
    pub themes: Vec<Id>,
}

pub fn bundled_file(id: &Id, file: &str) -> String {
    format!("themes/{id}/{file}")
}

/// A bundled theme, read through `read` (the asset resolver, or the folder in a test). A failure
/// is a bug in the commit that built the bundle, listed like any other so it is seen.
fn bundled(read: &dyn Fn(&str) -> Option<Vec<u8>>, id: &Id) -> Result<Theme, String> {
    let bytes = read(&bundled_file(id, FILE)).ok_or_else(|| format!("缺 {FILE}"))?;
    let file = ThemeFile::parse(&bytes)?;
    for image in file.images() {
        read(&bundled_file(id, image)).ok_or_else(|| format!("{FILE} 指定了 {image}，但資料夾裡沒有"))?;
    }
    Ok(Theme { id: id.clone(), source: Source::Bundled, file })
}

/// One user folder, whose id has passed and which is a real folder: no link in it, every file
/// under its cap and all of them under `TOTAL_CAP`, a `theme.json` that parses, and every image it
/// names there.
fn inspect(dir: &Path, id: Id) -> Result<Theme, String> {
    let files = cast::files_in(dir, &id, |name| Kind::of(name).map(Kind::cap), TOTAL_CAP, "整個主題")?;
    if !files.iter().any(|f| f == FILE) {
        return Err(format!("缺 {FILE}"));
    }
    let bytes = cast::read_in(dir, &id, FILE, JSON_CAP).map_err(|r| r.why().to_string())?;
    let file = ThemeFile::parse(&bytes)?;
    for image in file.images() {
        if !files.iter().any(|f| f == image) {
            return Err(format!("{FILE} 指定了 {image}，但資料夾裡沒有"));
        }
    }
    Ok(Theme { id, source: Source::User, file })
}

pub fn scan(dir: &Path, bundled: &[Id]) -> (Vec<Theme>, Vec<Refused>) {
    cast::scan_with(dir, bundled, "主題", inspect)
}

/// Every theme that can be shown, bundled first, and every folder that could not be.
#[derive(Debug, Clone)]
pub struct Found {
    pub default: Id,
    pub themes: Vec<Theme>,
    pub refused: Vec<Refused>,
}

impl Found {
    pub fn load(read: &dyn Fn(&str) -> Option<Vec<u8>>, user_dir: Option<&Path>) -> Result<Found, String> {
        let bytes = read(INDEX).ok_or_else(|| format!("{INDEX} is not in the bundle"))?;
        let index: Index = serde_json::from_slice(&bytes).map_err(|e| format!("{INDEX}: {e}"))?;
        let mut themes = Vec::new();
        let mut refused = Vec::new();
        for id in &index.themes {
            match bundled(read, id) {
                Ok(t) => themes.push(t),
                Err(why) => refused.push(Refused { id: id.to_string(), source: Source::Bundled, why }),
            }
        }
        if !themes.iter().any(|t| t.id == index.default) {
            return Err(format!("{INDEX}: the default {} cannot be shown", index.default));
        }
        if let Some(dir) = user_dir {
            let (users, bad) = scan(dir, &index.themes);
            themes.extend(users);
            refused.extend(bad);
        }
        Ok(Found { default: index.default, themes, refused })
    }

    pub fn find(&self, id: &str) -> Option<&Theme> {
        self.themes.iter().find(|t| t.id.as_str() == id)
    }

    /// The saved choice while it can be shown, and the default otherwise, with why, so a theme
    /// whose folder was deleted or broke since falls back instead of leaving the page unstyled.
    pub fn pick(&self, saved: &Id) -> (&Theme, Option<String>) {
        if let Some(t) = self.find(saved.as_str()) {
            return (t, None);
        }
        let default = self.find(self.default.as_str()).expect("load checked the default can be shown");
        let why = match self.refused.iter().find(|r| r.id == saved.as_str()) {
            Some(r) => format!("選的主題 {saved} 載入不了（{}），先用{}", r.why, default.file.name),
            None => format!("找不到選的主題 {saved}，先用{}", default.file.name),
        };
        (default, Some(why))
    }
}

/// What the pages are told: the theme in effect, why it is not the one chosen when it is not, and
/// the list for the settings window.
#[derive(Debug, Clone, Serialize)]
pub struct Listing {
    pub shown: Listed,
    pub fallback: Option<String>,
    pub themes: Vec<Listed>,
    pub refused: Vec<Refused>,
}

/// The themes the pet has. Scanned at start, and again each time a page asks for the list, so a
/// folder dropped in shows up when the settings window is next focused, without a restart.
pub struct Themes {
    found: Mutex<Result<Found, String>>,
    home: Option<PathBuf>,
}

impl Themes {
    pub fn load(read: &dyn Fn(&str) -> Option<Vec<u8>>, home: Option<PathBuf>) -> Themes {
        let found = Found::load(read, home.as_ref().map(|h| h.join(USER_DIR)).as_deref());
        if let Err(why) = &found {
            eprintln!("themes: {why}");
        }
        Themes { found: Mutex::new(found), home }
    }

    pub fn found(&self) -> Result<Found, String> {
        self.found.lock().expect("themes poisoned").clone()
    }

    pub fn rescan(&self, read: &dyn Fn(&str) -> Option<Vec<u8>>) {
        let user_dir = self.home.as_ref().map(|h| h.join(USER_DIR));
        *self.found.lock().expect("themes poisoned") = Found::load(read, user_dir.as_deref());
    }

    pub fn listing(&self, saved: &Id) -> Result<Listing, String> {
        let found = self.found()?;
        let (shown, fallback) = found.pick(saved);
        Ok(Listing {
            shown: shown.listed(),
            fallback,
            themes: found.themes.iter().map(Theme::listed).collect(),
            refused: found.refused.clone(),
        })
    }

    /// One file of a user theme, by the scheme's path, `/<id>/<file>`. Only a theme the last scan
    /// accepted is served, and `cast::read_in` checks the file again now.
    pub fn serve(&self, path: &str) -> Result<(Vec<u8>, Kind), Refusal> {
        let (id, file) = path
            .strip_prefix('/')
            .and_then(|p| p.split_once('/'))
            .ok_or_else(|| Refusal::Forbidden(format!("{path:?} is not /<id>/<file>")))?;
        let file = cast::percent_decode(file).ok_or_else(|| Refusal::Forbidden(format!("{file:?} is not a valid escaped name")))?;
        let kind = file_kind(&file).map_err(Refusal::Forbidden)?;
        let theme = self
            .found()
            .ok()
            .and_then(|f| f.find(id).cloned())
            .filter(|t| t.source == Source::User)
            .ok_or_else(|| Refusal::Missing(format!("{id:?} is not a user theme that was loaded")))?;
        let home = self.home.as_ref().ok_or_else(|| Refusal::Missing("the pet has no home".to_string()))?;
        cast::read_in(&home.join(USER_DIR), &theme.id, &file, kind.cap()).map(|bytes| (bytes, kind))
    }
}

/// `<home>/themes`, made if it is not there yet, for the settings window's button that opens it.
pub fn dir(home: &Path) -> Result<PathBuf, String> {
    let dir = home.join(USER_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}
