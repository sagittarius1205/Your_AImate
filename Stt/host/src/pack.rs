//! The model files and their checked-in hashes (Docs/DESIGN_STT.md 7.1 `Asset`, reduced to this
//! slice). The manifest is compiled in, so the hashes a build trusts are the ones in git. The files
//! are compared by content, so a half-finished download or a swapped model fails by name instead of
//! loading.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const MANIFEST: &str = include_str!("../../models.json");

/// Overrides where the models are looked for. Unset, it is `stt` in the pet's home.
pub const ROOT_ENV: &str = "VRSTT_MODELS";

/// Names the pack `run` must load, which is how a host's choice reaches this process (the pet's
/// settings, Docs/DESIGN_Settings.md 4). Unset is the manifest's own. A build knows the one pack its
/// manifest describes, so any other name is a model error by name, and not that pack loaded quietly
/// in place of the one chosen.
pub const PACK_ENV: &str = "VRSTT_PACK";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    AsrModel,
    AsrTokens,
    Vad,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Asset {
    pub role: Role,
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackError {
    Unparsable(String),
    UnknownVersion { found: u32 },
    /// A path that is absolute or climbs out of the model directory.
    PathEscapes { path: String },
    MissingRole { role: Role },
    Missing { file: PathBuf },
    Size { file: PathBuf, want: u64, got: u64 },
    Checksum { file: PathBuf, want: String, got: String },
    Unreadable { file: PathBuf, why: String },
    UnknownPack { wanted: String, known: String },
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackError::Unparsable(why) => write!(f, "models.json does not parse: {why}"),
            PackError::UnknownVersion { found } => write!(f, "models.json version {found} is not 1"),
            PackError::PathEscapes { path } => write!(f, "models.json path {path} leaves the model directory"),
            PackError::MissingRole { role } => write!(f, "models.json names no {role:?} file"),
            PackError::Missing { file } => write!(f, "model file missing: {}", file.display()),
            PackError::Size { file, want, got } => {
                write!(f, "model file {} is {got} bytes, the manifest says {want}", file.display())
            }
            PackError::Checksum { file, want, got } => {
                write!(f, "model file {} has SHA-256 {got}, the manifest says {want}", file.display())
            }
            PackError::Unreadable { file, why } => write!(f, "model file {} cannot be read: {why}", file.display()),
            PackError::UnknownPack { wanted, known } => {
                write!(f, "model pack {wanted} is not one this build knows; it knows {known}")
            }
        }
    }
}

/// The files, verified. Only `verify` builds one.
#[derive(Debug, Clone)]
pub struct Verified {
    pub asr_model: PathBuf,
    pub asr_tokens: PathBuf,
    pub vad: PathBuf,
}

pub fn default_root() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(ROOT_ENV).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    // The pet's home, the rule `Pet/src-tauri/src/home.rs` owns: `VRPET_HOME`, else `.vrpet` in the
    // profile. Not `%LOCALAPPDATA%`, which the Claude desktop app's MSIX package redirects, so a
    // service it started and one started from the desktop looked in two places.
    let set = |name: &str| std::env::var_os(name).filter(|p| !p.is_empty()).map(PathBuf::from);
    let home = set("VRPET_HOME").or_else(|| set("USERPROFILE").map(|p| p.join(".vrpet")))?;
    Some(home.join("stt"))
}

pub fn parse(text: &str) -> Result<Manifest, PackError> {
    let m: Manifest = serde_json::from_str(text).map_err(|e| PackError::Unparsable(e.to_string()))?;
    if m.version != 1 {
        return Err(PackError::UnknownVersion { found: m.version });
    }
    for a in &m.assets {
        let p = Path::new(&a.path);
        let climbs = p.components().any(|c| !matches!(c, std::path::Component::Normal(_)));
        if climbs || p.is_absolute() {
            return Err(PackError::PathEscapes { path: a.path.clone() });
        }
    }
    Ok(m)
}

/// The pack the manifest describes: the folder under `packs/` that holds its recognition model.
pub fn pack_id(manifest: &Manifest) -> Option<&str> {
    let model = manifest.assets.iter().find(|a| a.role == Role::AsrModel)?;
    let mut parts = model.path.split('/');
    (parts.next() == Some("packs")).then(|| parts.next()).flatten().filter(|id| !id.is_empty())
}

/// `Ok` when `wanted` is absent or is the manifest's pack.
pub fn check_wanted(manifest: &Manifest, wanted: Option<&str>) -> Result<(), PackError> {
    let known = pack_id(manifest).unwrap_or("");
    match wanted.filter(|w| !w.is_empty()) {
        Some(w) if w != known => Err(PackError::UnknownPack { wanted: w.to_string(), known: known.to_string() }),
        _ => Ok(()),
    }
}

pub fn verify(manifest: &Manifest, root: &Path) -> Result<Verified, PackError> {
    let found = |role: Role| -> Result<PathBuf, PackError> {
        let a = manifest.assets.iter().find(|a| a.role == role).ok_or(PackError::MissingRole { role })?;
        let file = root.join(&a.path);
        check(a, &file)?;
        Ok(file)
    };
    Ok(Verified { asr_model: found(Role::AsrModel)?, asr_tokens: found(Role::AsrTokens)?, vad: found(Role::Vad)? })
}

fn check(a: &Asset, file: &Path) -> Result<(), PackError> {
    let meta = std::fs::metadata(file).map_err(|_| PackError::Missing { file: file.to_path_buf() })?;
    if meta.len() != a.bytes {
        return Err(PackError::Size { file: file.to_path_buf(), want: a.bytes, got: meta.len() });
    }
    let got = sha256_file(file).map_err(|why| PackError::Unreadable { file: file.to_path_buf(), why })?;
    if !got.eq_ignore_ascii_case(&a.sha256) {
        return Err(PackError::Checksum { file: file.to_path_buf(), want: a.sha256.clone(), got });
    }
    Ok(())
}

pub fn sha256_file(file: &Path) -> Result<String, String> {
    let mut f = std::fs::File::open(file).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
