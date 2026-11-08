# Downloads the speech-input models that Stt/models.json lists and checks each one's SHA-256.
# Files already in place with the right hash are skipped, so running it again is safe.
#
# The models go where vrstt looks for them: VRSTT_MODELS, else $VRPET_HOME\stt, else ~\.vrpet\stt.
#
# usage: pwsh -File Scripts\fetch_models.ps1
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"   # Invoke-WebRequest is many times slower with the progress bar

$repo = Split-Path $PSScriptRoot -Parent
$manifest = Get-Content -Raw -Encoding utf8 (Join-Path $repo "Stt\models.json") | ConvertFrom-Json
$root = if ($env:VRSTT_MODELS) { $env:VRSTT_MODELS }
        elseif ($env:VRPET_HOME) { Join-Path $env:VRPET_HOME "stt" }
        else { Join-Path $env:USERPROFILE ".vrpet\stt" }
$base = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models"

function Test-Asset($asset) {
    $path = Join-Path $root $asset.path
    (Test-Path $path) -and ((Get-FileHash -Algorithm SHA256 $path).Hash -eq $asset.sha256)
}

$missing = @($manifest.assets | Where-Object { -not (Test-Asset $_) })
if ($missing.Count -eq 0) {
    Write-Output "all $($manifest.assets.Count) models are in place under $root"
    exit 0
}

$work = Join-Path ([IO.Path]::GetTempPath()) ("vrpet-models-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force $work | Out-Null
try {
    foreach ($from in ($missing | ForEach-Object { $_.from } | Select-Object -Unique)) {
        $download = Join-Path $work $from
        Write-Output "downloading $from"
        Invoke-WebRequest -Uri "$base/$from" -OutFile $download
        if ($from -like "*.tar.bz2") {
            # Windows 10 and later ship bsdtar, which reads bzip2. Named by path, because a Git Bash
            # PATH puts GNU tar first and that one reads "C:\..." as a remote host.
            & (Join-Path $env:SystemRoot "System32\tar.exe") -xjf $download -C $work
            if ($LASTEXITCODE -ne 0) { throw "tar could not unpack $from" }
        }
    }
    foreach ($asset in $missing) {
        if ($asset.from -like "*.tar.bz2") {
            # Inside the archive the file sits under a folder named after the archive.
            $dir = $asset.from -replace '\.tar\.bz2$', ''
            $source = Join-Path (Join-Path $work $dir) (Split-Path $asset.path -Leaf)
        } else {
            $source = Join-Path $work $asset.from
        }
        $hash = (Get-FileHash -Algorithm SHA256 $source).Hash
        if ($hash -ne $asset.sha256) { throw "$($asset.path): SHA-256 is $hash, models.json expects $($asset.sha256)" }
        $target = Join-Path $root $asset.path
        New-Item -ItemType Directory -Force (Split-Path $target -Parent) | Out-Null
        Move-Item -Force $source $target
        Write-Output "ok $($asset.path)"
    }
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
