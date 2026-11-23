// The core's temporary directory for one run, and the one owner that removes it.
//
// Today what lives in it is the engine's working directory, and one folder per relay while the
// relay runs. The WAV files the design puts in "the session's temporary directory"
// (Docs/DESIGN_ChatCore.md section 2) will live in it too, and leave with it.

use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

pub struct Scratch {
    root: PathBuf,
}

impl Scratch {
    /// Names the directory and touches nothing: a core that never starts an engine should not
    /// leave a directory behind, or fail because it could not make one.
    pub fn under(parent: &Path) -> Scratch {
        Scratch { root: parent.join(format!("vrchatcore-{}", std::process::id())) }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        remove(&self.root);
    }
}

/// Removes a directory and everything in it. On Windows a directory cannot go while a process still
/// has it as its working directory, and a process that was just killed can take a moment to let
/// go, so a refusal is retried briefly before it is reported.
pub fn remove(dir: &Path) {
    let mut last = None;
    for _ in 0..50 {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => return,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            Err(e) => last = Some(e),
        }
        thread::sleep(Duration::from_millis(20));
    }
    if let Some(e) = last {
        eprintln!("vrchatcore: could not remove {}: {e}", dir.display());
    }
}

/// Makes `dir` new and empty, so whatever an earlier process left there (or a stale directory
/// from a run whose process id this one reuses) is not there when the next one starts.
pub fn fresh(dir: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    std::fs::create_dir_all(dir)
}

/// A new, empty directory for the engine to run in, made fresh each time.
pub fn engine_dir(root: &Path) -> io::Result<PathBuf> {
    let dir = root.join("engine");
    fresh(&dir)?;
    Ok(dir)
}
