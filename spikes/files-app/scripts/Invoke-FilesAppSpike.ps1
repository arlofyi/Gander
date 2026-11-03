# WP-47 Files-app spike driver: builds the throwaway provider, boots the `gander` AVD headless (with
# -Boot), drives the system Files app and a few players against it, and records what they read.
#
#   pwsh -NoProfile -File spikes/files-app/scripts/Invoke-FilesAppSpike.ps1 -Boot
#   pwsh -NoProfile -File spikes/files-app/scripts/Invoke-FilesAppSpike.ps1 -UseRunningEmulator -SkipBuild
#
# Outputs (spikes/files-app/results/): device.txt, analysis.md, logs/NN-<run>.txt, screens/*.png.
# Needs: JDK 17, Android SDK 36 (ANDROID_HOME) + the `gander` AVD (ANDROID_AVD_HOME), ffmpeg on PATH,
# GRADLE_USER_HOME pointing at the Gradle cache (the script stops if one is unset). Nothing here binds a
# non-loopback address; the HTTP server inside the app listens on 127.0.0.1 only.

[CmdletBinding()]
param(
    [switch]$Boot,
    [switch]$UseRunningEmulator,
    [switch]$SkipBuild,
    [switch]$KeepEmulator,
    [string]$ResultsDir = "$PSScriptRoot/../results"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$InformationPreference = 'Continue'

. "$PSScriptRoot/SpikeLib.ps1"

$projectDir = (Resolve-Path "$PSScriptRoot/..").Path
$ResultsDir = [System.IO.Path]::GetFullPath($ResultsDir)
$mediaDir = "$projectDir/build/media"
$apk = "$projectDir/app/build/outputs/apk/debug/app-debug.apk"
$fileA = 'sample-faststart.mp4'
$fileB = 'sample-moovend.mp4'
$analysis = New-Object System.Collections.Generic.List[string]
$runNo = 0

if (-not $Boot -and -not $UseRunningEmulator) { throw 'Pass -Boot (start and stop the emulator) or -UseRunningEmulator.' }
foreach ($d in @("$ResultsDir/logs", "$ResultsDir/screens")) { New-Item -ItemType Directory -Force -Path $d | Out-Null }

function Start-SpikeRun {
    param([Parameter(Mandatory)][string]$Name)
    $script:runNo++
    $script:currentRun = ('{0:D2}-{1}' -f $script:runNo, $Name)
    Write-Information "=== run $($script:currentRun)"
    Clear-SpikeLog
}

function Complete-SpikeRun {
    param([Parameter(Mandatory)][string]$Title, [string]$Notes, [switch]$IncludeFetch)
    $path = "$ResultsDir/logs/$($script:currentRun).txt"
    $lines = Save-SpikeLog -Path $path -IncludeFetch:$IncludeFetch
    Write-Information "    saved $lines log lines"
    $analysis.Add((& "$PSScriptRoot/Analyze-ReadLog.ps1" -LogPath $path -Title "$($script:currentRun): $Title"))
    if ($Notes) { $analysis.Add("`n$Notes`n") }
    $analysis.Add('')
}

function Reset-Apps {
    foreach ($p in @('com.google.android.documentsui', 'com.google.android.apps.photos', $script:Package)) {
        Invoke-Adb shell am force-stop $p | Out-Null
    }
    Invoke-Adb shell input keyevent KEYCODE_HOME | Out-Null
    Start-Sleep -Milliseconds 800
}

function Open-FilesRoot {
    Invoke-Adb shell am start -a android.intent.action.VIEW -d "content://$($script:Authority)/root/spike" -t vnd.android.document/root | Out-Null
    $n = Wait-UiNode -Pattern ([regex]::Escape($fileA)) -TimeoutSec 30
    if (-not $n) { throw 'Files app did not list the spike documents.' }
}

function Get-TopActivity {
    $line = Invoke-Adb shell dumpsys activity activities | Where-Object { $_ -match 'topResumedActivity' } | Select-Object -First 1
    if ($line) { return ($line -replace '^\s*', '') }
    return '?'
}

function Invoke-PlayerRun {
    param([string]$Doc)
    Invoke-Adb shell am start -n "$($script:Package)/.PlayerActivity" -d "content://$($script:Authority)/document/$Doc" | Out-Null
    $done = Wait-SpikeLogLine -Pattern 'PLAYER_DESTROY' -TimeoutSec 60
    if (-not $done) { Write-Warning 'player run did not finish in 60 s' }
    Start-Sleep -Seconds 2 # let the provider's RELEASE line land
}

function Invoke-ProbeRun {
    param([string]$Plan, [string]$Plan2, [string]$Doc = $fileA, [int]$TimeoutSec = 90)
    $amArgs = @('shell', 'am', 'start', '-n', "$($script:Package)/.ProbeActivity", '--es', 'plan', $Plan, '--es', 'doc', $Doc)
    if ($Plan2) { $amArgs += @('--es', 'plan2', $Plan2) }
    Invoke-Adb @amArgs | Out-Null
    $done = Wait-SpikeLogLine -Pattern 'PROBE_ALL_DONE' -TimeoutSec $TimeoutSec
    if (-not $done) { Write-Warning "probe '$Plan' did not finish in $TimeoutSec s" }
    Start-Sleep -Seconds 2
}

$emulatorStarted = $false
try {
    # ---- 1. media and build ----------------------------------------------------------------------
    New-Item -ItemType Directory -Force -Path $mediaDir | Out-Null
    if (-not (Test-Path "$mediaDir/$fileA")) {
        Write-Information 'generating test media with ffmpeg (about 31 MB each, not committed)'
        $ff = @('-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', 'testsrc2=size=1280x720:rate=30,noise=alls=25:allf=t',
            '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=44100', '-t', '40', '-c:v', 'libx264', '-preset', 'veryfast',
            '-profile:v', 'main', '-pix_fmt', 'yuv420p', '-b:v', '6M', '-maxrate', '7M', '-bufsize', '8M', '-g', '60',
            '-c:a', 'aac', '-b:a', '128k', '-movflags', '+faststart', "$mediaDir/$fileA")
        & ffmpeg @ff
        if ($LASTEXITCODE -ne 0) { throw 'ffmpeg failed' }
    }
    if (-not (Test-Path "$mediaDir/$fileB")) {
        # Same bytes of media, but the moov box moved to the end (no faststart): players must seek to the tail first.
        & ffmpeg -hide_banner -loglevel error -y -i "$mediaDir/$fileA" -c copy "$mediaDir/$fileB"
        if ($LASTEXITCODE -ne 0) { throw 'ffmpeg remux failed' }
    }
    $sizeA = (Get-Item "$mediaDir/$fileA").Length

    if (-not $SkipBuild) {
        if (-not $env:GRADLE_USER_HOME) { throw 'Set GRADLE_USER_HOME to the Gradle cache folder (for example %USERPROFILE%/.gradle).' }
        if (-not $env:ANDROID_HOME) { $env:ANDROID_HOME = $script:SdkRoot }
        Push-Location $projectDir
        try {
            # --no-daemon: other workers share GRADLE_USER_HOME; never stop their daemons.
            & "$projectDir/gradlew.bat" --no-daemon assembleDebug
            if ($LASTEXITCODE -ne 0) { throw 'gradle assembleDebug failed' }
        } finally { Pop-Location }
    }
    if (-not (Test-Path $apk)) { throw "APK not found: $apk" }

    # ---- 2. emulator ---------------------------------------------------------------------------
    if ($Boot) {
        Start-SpikeEmulator -LogPath "$projectDir/build/emulator.log"
        $emulatorStarted = $true
    }
    Invoke-Adb shell input keyevent KEYCODE_WAKEUP | Out-Null

    # ---- 3. device facts -------------------------------------------------------------------------
    $pkgXml = "$($script:SdkRoot)/system-images/android-36/google_apis/x86_64/package.xml"
    $imageRev = '?'
    try {
        if (Test-Path $pkgXml) {
            $raw = Get-Content $pkgXml -Raw
            $rev = if ($raw -match '<revision>\s*<major>(\d+)</major>') { $Matches[1] } else { '?' }
            $disp = if ($raw -match '<display-name[^>]*>([^<]+)</display-name>') { $Matches[1] } else { '?' }
            $imageRev = "system-images;android-36;google_apis;x86_64, revision $rev ($disp)"
        }
    } catch { Write-Warning "could not read $pkgXml : $($_.Exception.Message)" }
    $emuVer = (& $script:Emulator -version 2>$null | Select-String 'Android emulator version' | Select-Object -First 1).Line
    $facts = @(
        "date (host): $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')",
        "emulator: $emuVer",
        "system image package: $imageRev",
        "avd: gander (pixel_6 profile)",
        "ro.build.fingerprint: $((Invoke-Adb shell getprop ro.build.fingerprint | Out-String).Trim())",
        "ro.build.version.release: $((Invoke-Adb shell getprop ro.build.version.release | Out-String).Trim())",
        "ro.build.version.sdk: $((Invoke-Adb shell getprop ro.build.version.sdk | Out-String).Trim())",
        "ro.build.version.security_patch: $((Invoke-Adb shell getprop ro.build.version.security_patch | Out-String).Trim())",
        "ro.product.cpu.abi: $((Invoke-Adb shell getprop ro.product.cpu.abi | Out-String).Trim())",
        "kernel: $((Invoke-Adb shell uname -r | Out-String).Trim())",
        "documentsui: $((Invoke-Adb shell dumpsys package com.google.android.documentsui | Select-String 'versionName' | Select-Object -First 1 | ForEach-Object { $_.Line.Trim() }))",
        "documentsui apk: $((Invoke-Adb shell pm path com.google.android.documentsui | Out-String).Trim())",
        "photos: $((Invoke-Adb shell dumpsys package com.google.android.apps.photos | Select-String 'versionName' | Select-Object -First 1 | ForEach-Object { $_.Line.Trim() }))",
        "video VIEW handlers: $((Invoke-Adb shell cmd package query-activities --brief -a android.intent.action.VIEW -t video/mp4 | Where-Object { $_ -match '/' } | ForEach-Object { $_.Trim() }) -join '; ')",
        "test media: $fileA and $fileB, $sizeA bytes each, 1280x720 H.264 Main 30 fps 40 s + AAC; moov at start / at end"
    )
    Set-Content -Path "$ResultsDir/device.txt" -Value $facts -Encoding utf8
    $facts | ForEach-Object { Write-Information "    $_" }

    # ---- 4. install and stage ---------------------------------------------------------------------
    Invoke-Adb uninstall $script:Package 2>$null | Out-Null
    Invoke-Adb install -r $apk | Out-Null
    Invoke-Adb shell mkdir -p $script:DataDir | Out-Null
    Invoke-Adb push "$mediaDir/$fileA" "$($script:DataDir)/" | Out-Null
    Invoke-Adb push "$mediaDir/$fileB" "$($script:DataDir)/" | Out-Null
    Set-SpikeConf @{}
    Invoke-Adb shell rm -f "/sdcard/Download/$fileA" "/sdcard/Download/$fileB" | Out-Null
    Invoke-Adb logcat -G 16M | Out-Null
    Reset-Apps

    # ---- 5. runs ---------------------------------------------------------------------------------
    # R1: the Files app lists the provider's root and both documents.
    Start-SpikeRun 'files-listing'
    Open-FilesRoot
    Save-AdbScreenshot -Path "$ResultsDir/screens/files-app-listing.png"
    Complete-SpikeRun -Title 'Files app browses the root (queries only, no reads)'

    # R2..R5: tap the document in the Files app; the VIEW intent goes to the system video handler (Photos).
    # Two passes over both files: Photos was seen to stall in the prepared state on some launches.
    foreach ($attempt in 1..2) {
        foreach ($doc in @($fileA, $fileB)) {
            $short = $doc -replace '^sample-', '' -replace '\.mp4$', ''
            Reset-Apps
            Start-SpikeRun "files-open-$short-try$attempt"
            Open-FilesRoot
            Invoke-UiTap -Pattern ([regex]::Escape($doc)) | Out-Null
            Start-Sleep -Seconds 4
            if (Invoke-UiTap -Pattern '^Got it$' -Optional -TimeoutSec 4) { Write-Information '    dismissed the Photos first-run tip' }
            Start-Sleep -Seconds 12
            $top = Get-TopActivity
            $mp = (Invoke-Adb shell dumpsys media.player | Select-String 'state\(\d+\)' | Select-Object -First 1 | ForEach-Object { $_.Line.Trim() })
            if ($attempt -eq 1) {
                Save-AdbScreenshot -Path "$ResultsDir/screens/files-open-$short-try$attempt.png"
            }
            Complete-SpikeRun -Title "Files app tap on $doc (attempt $attempt), handed to the system video player" `
                -Notes "Top activity after 16 s: ``$top``. NuPlayer: ``$mp`` (state 4 = prepared, playback not started; state 5 = running)."
        }
    }

    # R4: Files app "Copy to... Downloads": a full sequential copy performed by DocumentsUI.
    Reset-Apps
    Start-SpikeRun 'files-copy-faststart'
    Open-FilesRoot
    Invoke-UiTap -Pattern ([regex]::Escape($fileA)) -LongPress | Out-Null
    Start-Sleep -Seconds 1
    Invoke-UiTap -Pattern '^More options$' | Out-Null
    Start-Sleep -Milliseconds 800
    Invoke-UiTap -Pattern '^Copy to' | Out-Null
    $null = Wait-UiNode -Pattern '^COPY$' -TimeoutSec 15
    Invoke-UiTap -Pattern '^COPY$' | Out-Null
    $rel = Wait-SpikeLogLine -Pattern 'RELEASE .*opener=com.google.android.documentsui' -TimeoutSec 90
    Start-Sleep -Seconds 2
    $copied = (Invoke-Adb shell stat -c %s "/sdcard/Download/$fileA" 2>$null | Out-String).Trim()
    Complete-SpikeRun -Title 'Files app "Copy to... Downloads" (a full copy by DocumentsUI)' `
        -Notes "Copy finished: $([bool]$rel). Size of /sdcard/Download/$fileA after the copy: $copied bytes (source $sizeA)."
    Invoke-Adb shell rm -f "/sdcard/Download/$fileA" | Out-Null

    # R5: Files app "Get info" on a document.
    Reset-Apps
    Start-SpikeRun 'files-getinfo-faststart'
    Open-FilesRoot
    Invoke-UiTap -Pattern ([regex]::Escape($fileA)) -LongPress | Out-Null
    Start-Sleep -Seconds 1
    Invoke-UiTap -Pattern '^More options$' | Out-Null
    Start-Sleep -Milliseconds 800
    $info = Invoke-UiTap -Pattern '^Get info' -Optional -TimeoutSec 5
    Start-Sleep -Seconds 4
    Complete-SpikeRun -Title 'Files app "Get info"' -Notes "Get info tapped: $info."

    # R6/R7: platform MediaPlayer (the engine Photos prepares with) plays the file with scripted seeks.
    foreach ($doc in @($fileA, $fileB)) {
        Reset-Apps
        Start-SpikeRun "player-$($doc -replace '\.mp4$','')"
        Invoke-PlayerRun -Doc $doc
        Complete-SpikeRun -Title "Platform MediaPlayer (VideoView), $doc, scripted seeks to 50 %, 90 %, 10 %"
    }

    # R8: same, with a 1 MiB one-block read-ahead in the callback (HTTP requests vs onRead calls).
    Reset-Apps
    Set-SpikeConf @{ readahead = 1048576 }
    Start-SpikeRun 'player-faststart-readahead1m'
    Invoke-PlayerRun -Doc $fileA
    Complete-SpikeRun -IncludeFetch -Title "Platform MediaPlayer, $fileA, provider read-ahead 1 MiB"
    Set-SpikeConf @{}

    # R9: controlled sequential reads with different client buffer sizes.
    Reset-Apps
    Start-SpikeRun 'probe-seq'
    Invoke-ProbeRun -Plan 'seq:4096,seq:65536,seq:1048576,seq:8388608'
    Complete-SpikeRun -Title 'Sequential client reads, buffers 4 KiB, 64 KiB, 1 MiB, 8 MiB (one fd each, in this order)'

    # R10: controlled random reads.
    Start-SpikeRun 'probe-rand'
    Invoke-ProbeRun -Plan 'rand:4096:200,rand:65536:100,rand:1048576:50'
    Complete-SpikeRun -Title 'Random client preads: 200 x 4 KiB, 100 x 64 KiB, 50 x 1 MiB (one fd each, in this order)'

    # R10b: sequential 1 MiB client reads with a 1 MiB read-ahead in the callback (HTTP requests vs onRead calls).
    Set-SpikeConf @{ readahead = 1048576 }
    Start-SpikeRun 'probe-seq-readahead1m'
    Invoke-ProbeRun -Plan 'seq:1048576'
    Complete-SpikeRun -IncludeFetch -Title 'Sequential 1 MiB client reads, provider read-ahead 1 MiB'
    Set-SpikeConf @{}

    # R11/R12: does a slow read on one fd stall another fd? Shared handler thread versus one thread per fd.
    foreach ($mode in @('shared', 'perfd')) {
        Set-SpikeConf @{ delay_ms = 5000; delay_from = 19000000; handler = $mode }
        Start-SpikeRun "probe-parallel-$mode"
        Invoke-ProbeRun -Plan 'seq:65536:20000000:131072' -Plan2 'sleep:1000,seq:65536:0:1048576'
        Complete-SpikeRun -Title "Two fds in parallel, handler=$($mode): t0 reads at 20,000,000 and is delayed 5 s by the server; t1 starts 1 s later and reads the first 1 MiB"
    }

    # R13: a single read that takes 20 s (is there a platform timeout?).
    Set-SpikeConf @{ delay_ms = 20000; delay_from = 29000000; handler = 'shared' }
    Start-SpikeRun 'probe-slow-read-20s'
    Invoke-ProbeRun -Plan 'seq:65536:30000000:131072' -TimeoutSec 60
    Complete-SpikeRun -Title 'One read delayed 20 s by the server'

    # R14: and one that takes 75 s (past any 60 s limit someone might expect).
    Set-SpikeConf @{ delay_ms = 75000; delay_from = 29000000; handler = 'shared' }
    Start-SpikeRun 'probe-slow-read-75s'
    Invoke-ProbeRun -Plan 'seq:65536:30000000:131072' -TimeoutSec 220
    Complete-SpikeRun -Title 'One read delayed 75 s by the server'
    Set-SpikeConf @{}

    Set-Content -Path "$ResultsDir/analysis.md" -Value ($analysis -join "`n") -Encoding utf8
    Write-Information "done; results in $ResultsDir"
} finally {
    try {
        Set-SpikeConf @{}
        Invoke-Adb shell rm -f "/sdcard/Download/$fileA" "/sdcard/Download/$fileB" | Out-Null
        Invoke-Adb uninstall $script:Package 2>$null | Out-Null
    } catch { Write-Warning "cleanup on device failed: $($_.Exception.Message)" }
    if ($emulatorStarted -and -not $KeepEmulator) {
        try { Stop-SpikeEmulator } catch { Write-Warning $_.Exception.Message; throw }
    }
}
