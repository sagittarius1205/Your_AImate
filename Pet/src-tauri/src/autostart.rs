//! The 開機時啟動 switch (Docs/DESIGN_Settings.md 1): a shortcut in the user's Startup folder that runs
//! `Scripts/start_pet.ps1` the way the desktop shortcut does, made and removed here and never written
//! to the registry. The switch shows whether that shortcut is there, and not what was last saved, so
//! one the user deleted by hand reads as off.
//!
//! The shortcut is written by Windows' own `WScript.Shell`, through PowerShell, which is how
//! `start_pet.ps1 -Shortcut` writes the desktop one: a `.lnk` is a binary format, and this crate has
//! no COM binding and keeps its dependencies few. Every path reaches the script through the
//! environment and not spliced into its text, so no path can change what the script does.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The shortcut's name in the Startup folder.
pub const LINK: &str = "桌寵.lnk";
/// Points the Startup folder somewhere else. The self-tests and a test pet set it, so neither ever
/// writes into the real Startup folder of the machine it runs on.
pub const DIR_ENV: &str = "VRPET_STARTUP";
/// The script the shortcut runs, from the repo root.
pub const SCRIPT: &str = "Scripts/start_pet.ps1";

/// What the shortcut points at: Windows PowerShell running `start_pet.ps1`, hidden, as the desktop
/// shortcut does (`start_pet.ps1 -Shortcut`).
#[derive(Debug, Clone, PartialEq)]
pub struct Shortcut {
    pub target: PathBuf,
    pub arguments: String,
    pub working_dir: PathBuf,
}

pub fn shortcut(repo: &Path) -> Shortcut {
    let script = repo.join(SCRIPT.replace('/', "\\"));
    Shortcut {
        target: powershell(),
        arguments: format!("-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File \"{}\"", script.display()),
        working_dir: repo.to_path_buf(),
    }
}

fn powershell() -> PathBuf {
    let root = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
}

/// The repo the running pet was built in: the first folder above its executable that has
/// `Scripts/start_pet.ps1`. Searched for and not counted, because the build folder's depth differs
/// with `CARGO_TARGET_DIR`.
pub fn repo_of(exe: &Path) -> Option<PathBuf> {
    exe.ancestors().skip(1).find(|dir| dir.join(SCRIPT).is_file()).map(Path::to_path_buf)
}

/// The Startup folder: `DIR_ENV` when it is set, else the one under the roaming AppData.
pub fn startup_dir() -> Option<PathBuf> {
    let set = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    set(DIR_ENV).or_else(|| set("APPDATA").map(|a| a.join(r"Microsoft\Windows\Start Menu\Programs\Startup")))
}

/// Where the shortcut goes and what it runs, found once when the pet starts.
pub struct Autostart {
    link: Option<PathBuf>,
    shortcut: Result<Shortcut, String>,
}

impl Autostart {
    pub fn new(folder: Option<PathBuf>, repo: Option<PathBuf>) -> Autostart {
        let shortcut = repo.as_deref().map(shortcut).ok_or_else(|| format!("找不到 {SCRIPT}，開機啟動要有它才能做"));
        Autostart { link: folder.map(|f| f.join(LINK)), shortcut }
    }

    /// The real Startup folder and the repo of the running executable.
    pub fn find() -> Autostart {
        let repo = std::env::current_exe().ok().as_deref().and_then(repo_of);
        Autostart::new(startup_dir(), repo)
    }

    pub fn is_on(&self) -> bool {
        self.link.as_deref().is_some_and(Path::is_file)
    }

    /// Makes the shortcut be there or not. On is written again even when one is there, so one that
    /// points somewhere else since the repo moved is put right; off with none there is nothing to do.
    pub fn set(&self, on: bool) -> Result<(), String> {
        let link = self.link.as_deref().ok_or("找不到「啟動」資料夾（沒有 APPDATA）")?;
        if !on {
            return match std::fs::remove_file(link) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("刪不掉 {}：{e}", link.display())),
            };
        }
        let s = self.shortcut.as_ref().map_err(String::clone)?;
        let icon = std::env::current_exe().map(|exe| format!("{},0", exe.display())).unwrap_or_default();
        run(
            "$s = (New-Object -ComObject WScript.Shell).CreateShortcut($env:VRPET_LNK); \
             $s.TargetPath = $env:VRPET_LNK_TARGET; $s.Arguments = $env:VRPET_LNK_ARGS; \
             $s.WorkingDirectory = $env:VRPET_LNK_DIR; $s.WindowStyle = 7; \
             if ($env:VRPET_LNK_ICON) { $s.IconLocation = $env:VRPET_LNK_ICON }; \
             $s.Description = 'Start the desktop pet and its voice service'; $s.Save()",
            &[
                ("VRPET_LNK", link.as_os_str()),
                ("VRPET_LNK_TARGET", s.target.as_os_str()),
                ("VRPET_LNK_ARGS", std::ffi::OsStr::new(&s.arguments)),
                ("VRPET_LNK_DIR", s.working_dir.as_os_str()),
                ("VRPET_LNK_ICON", std::ffi::OsStr::new(&icon)),
            ],
        )?;
        if self.is_on() { Ok(()) } else { Err(format!("PowerShell 說建好了，但 {} 不在", link.display())) }
    }

    /// The switch as it really is: the file's word replaced by whether the shortcut exists.
    pub fn overlay(&self, mut loaded: crate::settings::Loaded) -> crate::settings::Loaded {
        loaded.settings.switches.autostart = self.is_on();
        loaded
    }
}

/// What the shortcut at `link` points at, read back the way it was written. For the self-test.
pub fn read(link: &Path) -> Result<Shortcut, String> {
    let out = run(
        "$s = (New-Object -ComObject WScript.Shell).CreateShortcut($env:VRPET_LNK); \
         [Console]::OutputEncoding = [Text.Encoding]::UTF8; \
         Write-Output $s.TargetPath; Write-Output $s.Arguments; Write-Output $s.WorkingDirectory",
        &[("VRPET_LNK", link.as_os_str())],
    )?;
    let mut lines = out.lines();
    let mut next = || lines.next().unwrap_or("").trim_end().to_string();
    Ok(Shortcut { target: PathBuf::from(next()), arguments: next(), working_dir: PathBuf::from(next()) })
}

fn run(script: &str, env: &[(&str, &std::ffi::OsStr)]) -> Result<String, String> {
    let mut cmd = Command::new(powershell());
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script]);
    for (name, value) in env {
        cmd.env(name, value);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().map_err(|e| format!("PowerShell 起不來：{e}"))?;
    if !out.status.success() {
        return Err(format!("PowerShell 失敗（{:?}）：{}", out.status.code(), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
