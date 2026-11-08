# Runs every self-test the pet binary carries. `run_selftests.ps1` is the Unreal one: it drives
# UnrealEditor.exe -VRSelfTest=<name> and its list has never had a pet_* in it, so until now
# nothing ran these five together and the only way to check them was to remember all the names.
#
# usage: pwsh -File Scripts\run_pet_selftests.ps1 [-Tests page,hit] [-Release]
param(
    [string[]]$Tests = @("pet_vocab", "pet_page", "pet_hit", "pet_asking", "pet_cues", "pet_core", "pet_filler", "pet_mouth", "pet_gesture", "pet_mic", "pet_characters", "pet_settings", "pet_themes"),
    [switch]$Release
)

$ErrorActionPreference = "Continue"
# -File hands a comma list in as one string, the same trap run_selftests.ps1 documents.
$Tests = $Tests -split ',' | ForEach-Object { $_.Trim() } | Where-Object { $_ }

$profileDir = if ($Release) { "release" } else { "debug" }
$exe = Join-Path $PSScriptRoot "..\Pet\src-tauri\target\$profileDir\vrpet.exe"
if (-not (Test-Path $exe)) {
    Write-Output "SelfTest: FAIL run_pet_selftests: no binary at $exe. Build it with: cd Pet\src-tauri; cargo build"
    exit 2
}

$failed = @()
foreach ($test in $Tests) {
    $name = if ($test -like "pet_*") { $test } else { "pet_$test" }
    # The binary prints its own SelfTest line and decides pass or fail itself (DESIGN_Pet.md 8):
    # a runner that judged the output would be a second opinion nobody asked for.
    & $exe "--selftest=$name"
    if ($LASTEXITCODE -ne 0) { $failed += $name }
}

if ($failed.Count -gt 0) {
    Write-Output ("SelfTest: FAIL run_pet_selftests: {0} of {1} failed: {2}" -f $failed.Count, $Tests.Count, ($failed -join ", "))
    exit 1
}
Write-Output ("SelfTest: PASS run_pet_selftests: {0} tests" -f $Tests.Count)
