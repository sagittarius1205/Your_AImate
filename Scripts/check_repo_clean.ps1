# Fails if the repository tracks a file that must never be published.
# usage: powershell -File Scripts\check_repo_clean.ps1
# Prints one "SelfTest: PASS|FAIL check_repo_clean: ..." line and exits 0 on pass, 1 on fail.
#
# This exists because the pet is going open source (Docs/PROGRAM.md section 3E). Before that,
# a stray commit of a vendored file was untidy. After it, publishing one is redistributing
# somebody else's proprietary file, which is the licence breach the Live2D terms forbid.
#
# It checks what git TRACKS, not what is in the working tree. .gitignore keeps files out by
# convention; it does not stop `git add -f`, and the working tree is not what gets published.

$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..")
Push-Location $root
try {
    $tracked = @(git ls-files 2>$null)
    if ($LASTEXITCODE -ne 0 -or $tracked.Count -eq 0) {
        # A check that cannot see the file list must not report success: that is how a green
        # test ends up proving nothing.
        "SelfTest: FAIL check_repo_clean: git ls-files returned $($tracked.Count) files (exit $LASTEXITCODE); cannot tell what is tracked"
        exit 1
    }

    # Group 1 is a licence breach if published. Cubism Core is Live2D's proprietary library, and
    # the model formats carry whatever licence their author gave them, which is not ours to pass on.
    $licence = @(
        @{ Pattern = "live2dcubismcore";  Why = "Cubism Core is Live2D proprietary; the user downloads it themselves" }
        @{ Pattern = "\.moc3$";           Why = "a Cubism model binary" }
        @{ Pattern = "\.model3\.json$";   Why = "a Cubism model definition" }
        @{ Pattern = "\.motion3\.json$";  Why = "a Cubism motion" }
        @{ Pattern = "\.physics3\.json$"; Why = "a Cubism physics setting" }
        @{ Pattern = "\.cdi3\.json$";     Why = "a Cubism display-info file" }
        @{ Pattern = "^Pet/vendor/";      Why = "Pet/vendor holds third-party runtimes and sample models" }
        # DESIGN_Pet.md section 9 puts the renderer's vendor directory under ui/, because the page
        # has to load those scripts and frontendDist points at ../ui. The rule above is anchored to
        # the path the spike happened to use, so without this one the files become trackable the
        # moment D1 moves them where the design says they go.
        @{ Pattern = "^Pet/ui/vendor/";   Why = "the page's vendor directory holds the same third-party runtimes" }
        # pixi-live2d-display's cubism4 bundle is NOT only MIT, whatever its own LICENSE file says.
        # Its .gitmodules points `cubism` at guansss/CubismWebFramework, a fork of Live2D's own
        # framework, its README says it "has rewritten" the official framework, and the minified
        # bundle carries the framework's class set (CubismPhysics 25 times, CubismMotionJson,
        # CubismPose, CubismRenderer_WebGL and the rest). That fork's LICENSE.md puts Cubism Web
        # Framework under the Live2D Open Software License, whose 5.7 forbids releasing their code
        # under a licence that is not Live2D's. So this file must not sit in an MIT repository, and
        # the name is matched directly because no path rule catches a file someone relocates.
        @{ Pattern = "pixi-live2d-display"; Why = "embeds a rewritten Cubism Web Framework, which is under the Live2D Open Software License, not MIT" }
    )

    # Group 2 is build output. Already gitignored, and a stray one bloats a repo that is about
    # to be public and is already 1605 MB of .git.
    $output = @(
        @{ Pattern = "^Binaries/";          Why = "build output" }
        @{ Pattern = "^Intermediate/";      Why = "build output" }
        @{ Pattern = "^Saved/";             Why = "runtime output" }
        @{ Pattern = "^DerivedDataCache/";  Why = "engine cache" }
        @{ Pattern = "^Pet/src-tauri/target/"; Why = "cargo output" }
        @{ Pattern = "^Pet/src-tauri/gen/";    Why = "tauri generated" }
    )

    $problems = @()
    foreach ($rule in $licence) {
        $hits = @($tracked | Where-Object { $_ -match $rule.Pattern })
        if ($hits.Count -gt 0) {
            $problems += "LICENCE: $($hits.Count) file(s) matching '$($rule.Pattern)' ($($rule.Why)): $($hits -join ', ')"
        }
    }
    foreach ($rule in $output) {
        $hits = @($tracked | Where-Object { $_ -match $rule.Pattern })
        if ($hits.Count -gt 0) {
            $problems += "OUTPUT: $($hits.Count) file(s) matching '$($rule.Pattern)' ($($rule.Why)): $($hits[0..([Math]::Min(2, $hits.Count - 1))] -join ', ')"
        }
    }

    $detail = "$($tracked.Count) tracked files, $($licence.Count) licence rules, $($output.Count) output rules"
    if ($problems.Count -eq 0) {
        "SelfTest: PASS check_repo_clean: $detail"
        exit 0
    }
    "SelfTest: FAIL check_repo_clean: $($problems -join '; ') -- $detail"
    exit 1
}
finally {
    Pop-Location
}
