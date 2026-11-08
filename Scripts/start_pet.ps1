# Starts the desktop pet, and the local voice service first when it is not already up. The desktop
# shortcut runs this; `-Shortcut` (re)creates that shortcut. The pet's 結束 button closes the pet,
# its core and its speech input; the voice service stays up, so the next start skips the model load.
#
# usage: powershell -File Scripts\start_pet.ps1 [-Shortcut]
param([switch]$Shortcut)

$ErrorActionPreference = "Stop"
$repo = Split-Path $PSScriptRoot -Parent
$pet = Join-Path $repo "Pet\src-tauri\target\debug\vrpet.exe"
# The Python that has VoxCPM2 installed: VRPET_PYTHON when set, else the repo's own .venv.
# Without either the pet still starts, and each turn shows its text without a voice.
$python = if ($env:VRPET_PYTHON) { $env:VRPET_PYTHON } else { Join-Path $repo ".venv\Scripts\python.exe" }
$voiceDir = Join-Path $PSScriptRoot "tts_engines"
$voicePort = 8880
# The pet's home (Pet/src-tauri/src/home.rs). Not LOCALAPPDATA: the Claude desktop app redirects it.
$vrpetHome = if ($env:VRPET_HOME) { $env:VRPET_HOME } else { Join-Path $env:USERPROFILE ".vrpet" }
$logs = Join-Path $vrpetHome "logs"

if ($Shortcut) {
    $lnk = Join-Path ([Environment]::GetFolderPath("Desktop")) "桌寵.lnk"
    $s = (New-Object -ComObject WScript.Shell).CreateShortcut($lnk)
    $s.TargetPath = Join-Path $env:SystemRoot "System32\WindowsPowerShell\v1.0\powershell.exe"
    $s.Arguments = "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$PSCommandPath`""
    $s.WorkingDirectory = $repo
    $s.WindowStyle = 7
    $s.IconLocation = "$pet,0"
    $s.Description = "Start the desktop pet and its voice service"
    $s.Save()
    Write-Output $lnk
    exit 0
}

# Runs `$exe` with no console window and its output in `$log`, detached from this script.
function Start-Hidden([string]$exe, [string]$arguments, [string]$dir, [string]$log) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $env:ComSpec
    $psi.Arguments = "/d /c `"`"$exe`" $arguments > `"$log`" 2>&1`""
    $psi.WorkingDirectory = $dir
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.EnvironmentVariables["PYTHONIOENCODING"] = "utf-8"
    [System.Diagnostics.Process]::Start($psi) | Out-Null
}

function Test-VoiceUp {
    if (Get-NetTCPConnection -LocalPort $voicePort -State Listen -ErrorAction SilentlyContinue) { return $true }
    # The model loads for about a minute before the port opens. A second start in that minute must
    # not load a second copy: the first alone takes 7.6 GB of VRAM.
    return [bool](Get-CimInstance Win32_Process -Filter "Name='python.exe'" |
        Where-Object { $_.CommandLine -like "*serve.py*" })
}

New-Item -ItemType Directory -Force $logs | Out-Null

if (-not (Test-Path $python)) {
    "no Python at $python, so the voice service is not started. Set VRPET_PYTHON or create .venv (README.md)." |
        Set-Content -Encoding utf8 (Join-Path $logs "voice.log")
} elseif (-not (Test-VoiceUp)) {
    Start-Hidden $python "-u serve.py --port $voicePort" $voiceDir (Join-Path $logs "voice.log")
}

# The pet does not wait for the voice: each turn asks the service afresh, and a turn before it is up
# is silent with the reason in the bubble (ChatCore/src/voice.rs).
if (-not (Get-Process vrpet -ErrorAction SilentlyContinue)) {
    Start-Hidden $pet "" (Split-Path $pet -Parent) (Join-Path $logs "pet.log")
}
