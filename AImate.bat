@echo off
rem Double-click to start AImate: the desktop pet, and the local voice service when it is not
rem already up. Scripts\start_pet.ps1 does the work; this file exists because Windows opens a
rem .ps1 in an editor instead of running it.
setlocal
set "PET=%~dp0Pet\src-tauri\target\debug\vrpet.exe"
if not exist "%PET%" (
    echo AImate is not built yet. Missing: %PET%
    echo Build it first:  cd Pet\src-tauri ^&^& cargo build
    echo Details are in the README.
    pause
    exit /b 1
)
start "" powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "%~dp0Scripts\start_pet.ps1"
