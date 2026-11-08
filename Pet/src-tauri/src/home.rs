// Where the pet keeps what other processes share with it: the hook's alerts and log, the speech
// models, the screenshots. `%USERPROFILE%\.vrpet` and not `%LOCALAPPDATA%`, because the Claude
// desktop app is an MSIX package and redirects AppData writes made under it to a private folder.
// A hook run by a Claude session and a pet started from the desktop then see two different
// directories, and the alert never arrives (Docs/STATUS.md, 2026-11-07). `~\.claude` is not
// redirected, and this sits beside it.
//
// `vrpet-hook` includes this file too, so the writer and the reader cannot drift apart. The speech
// service is its own workspace and restates the rule in `Stt/host/src/pack.rs`.

use std::ffi::OsString;
use std::path::PathBuf;

/// Points the pet, the hook and the speech service at another home, for tests and for anyone who
/// wants it elsewhere.
pub const ENV: &str = "VRPET_HOME";

pub fn dir() -> Result<PathBuf, String> {
    resolve(std::env::var_os(ENV), std::env::var_os("USERPROFILE"))
}

pub fn resolve(explicit: Option<OsString>, profile: Option<OsString>) -> Result<PathBuf, String> {
    if let Some(p) = explicit.filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    profile
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(p).join(".vrpet"))
        .ok_or_else(|| format!("neither {ENV} nor USERPROFILE is set"))
}
