# WP-44 emulator run: boots the `gander` AVD headless, installs the spike app, starts the host
# (`spike-input serve`), launches the app and lets the host drive taps with `adb shell input tap`.
#
# Run with PowerShell 7+:  pwsh -NoProfile -File spikes/stream/android/scripts/Invoke-StreamSpike.ps1
# Build first:  cargo build --release -p spike-input  and  gradlew assembleDebug (this folder's parent).
#
# Emulator coordination: only one emulator may run (the WP-29 e2e suite uses the same AVD). If a
# qemu or emulator process exists the script polls every 2 minutes (up to -WaitMinutes) and boots
# only when none is left. It always kills the emulator it booted and confirms no qemu remains.
#
# Network: the host binds 127.0.0.1 only. The emulator reaches the host's loopback as 10.0.2.2, so
# the offer advertises 10.0.2.2 (-Signalling direct, the app connects to ws://10.0.2.2:<port>/), or
# the signalling TCP port is forwarded with `adb reverse` (-Signalling reverse, the app connects to
# ws://127.0.0.1:<port>/). The media UDP path is the emulator's NAT either way.

[CmdletBinding()]
param(
    [string]$Label = 'emu',
    [int]$Taps = 12,
    [ValidateSet('postmessage', 'sendinput', 'alternate', 'none')][string]$Inject = 'postmessage',
    [ValidateSet('direct', 'reverse')][string]$Signalling = 'direct',
    [int]$Port = 47440,
    [int]$RunSeconds = 240,
    [int]$WaitMinutes = 90,
    [string]$Size = '1080x2280',
    [ValidateSet('intel', 'nvidia')][string]$Vendor = 'intel',
    [switch]$StripAnswerCandidates,
    [switch]$TextureDecode,
    [switch]$KeepEmulator,
    [switch]$NoEmulatorBoot,
    # Seconds to let a freshly booted emulator settle before the test (post-boot work skews timing).
    [int]$SettleSeconds = 0,
    # C14: what the host sees when the phone app goes to the background, the screen turns off, or
    # the app is killed (no taps are driven in these scenarios).
    [ValidateSet('none', 'background', 'screenoff', 'forcestop')][string]$Scenario = 'none'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repo = (Resolve-Path "$PSScriptRoot/../../../..").Path -replace '\\', '/'
if (-not $env:ANDROID_HOME) { throw 'Set ANDROID_HOME to the Android SDK folder (for example %LOCALAPPDATA%/Android/Sdk).' }
$sdk = $env:ANDROID_HOME -replace '\\', '/'
$adb = "$sdk/platform-tools/adb.exe"
$emulatorExe = "$sdk/emulator/emulator.exe"
$apk = "$repo/spikes/stream/android/app/build/outputs/apk/debug/app-debug.apk"
$hostExe = "$repo/target/release/spike-input.exe"
$results = "$repo/spikes/stream/android/results"
$package = 'fyi.arlo.gander.spike.stream'

function Invoke-Adb {
    # Not an advanced function: [CmdletBinding()] would swallow adb's own flags as common parameters.
    & $adb @args
}

function Get-EmulatorProcess {
    Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.ProcessName -match '^(qemu-system|emulator)' }
}

function Wait-EmulatorFree {
    $deadline = (Get-Date).AddMinutes($WaitMinutes)
    while (Get-EmulatorProcess) {
        if ((Get-Date) -gt $deadline) { throw "An emulator was still running after $WaitMinutes minutes; emulator part deferred." }
        Write-Information "emulator/qemu running (another worker); polling again in 2 minutes" -InformationAction Continue
        Start-Sleep -Seconds 120
    }
}

function Start-Emulator {
    if (-not $env:ANDROID_AVD_HOME) { throw 'Set ANDROID_AVD_HOME to the folder that holds the gander AVD (for example %USERPROFILE%/.android/avd).' }
    $log = "$results/$Label-emulator.log"
    $emuArgs = @('-avd', 'gander', '-port', '5554', '-no-window', '-no-audio', '-no-snapshot-save', '-no-snapshot-load', '-gpu', 'swiftshader_indirect')
    $splat = @{
        FilePath               = $emulatorExe
        ArgumentList           = $emuArgs
        PassThru               = $true
        WindowStyle            = 'Hidden'
        RedirectStandardOutput = $log
        RedirectStandardError  = "$log.err"
    }
    $proc = Start-Process @splat
    Write-Information "emulator started, pid $($proc.Id)" -InformationAction Continue
    Invoke-Adb wait-for-device | Out-Null
    $deadline = (Get-Date).AddSeconds(300)
    while ((Get-Date) -lt $deadline) {
        $boot = (Invoke-Adb shell getprop sys.boot_completed 2>$null | Out-String).Trim()
        if ($boot -eq '1') { return }
        Start-Sleep -Seconds 3
    }
    throw 'Emulator did not finish booting within 300 s.'
}

function Stop-Emulator {
    Invoke-Adb emu kill 2>$null | Out-Null
    $deadline = (Get-Date).AddSeconds(90)
    while ((Get-Date) -lt $deadline) {
        if (-not (Get-EmulatorProcess)) { break }
        Start-Sleep -Seconds 2
    }
    $left = @(Get-EmulatorProcess)
    if ($left.Count -gt 0) { throw "Emulator processes still running: $(($left | ForEach-Object { $_.ProcessName }) -join ', ')" }
    Write-Information "emulator stopped; no qemu/emulator process remains" -InformationAction Continue
}

if (-not (Test-Path $apk)) { throw "APK missing: $apk (run gradlew assembleDebug in spikes/stream/android)" }
if (-not (Test-Path $hostExe)) { throw "Host missing: $hostExe (run cargo build --release -p spike-input)" }
New-Item -ItemType Directory -Force -Path $results | Out-Null

$booted = $false
$hostProc = $null
$testwinPids = @()
try {
    if (-not $NoEmulatorBoot) {
        Wait-EmulatorFree
        Start-Emulator
        $booted = $true
        if ($SettleSeconds -gt 0) { Start-Sleep -Seconds $SettleSeconds }
    }
    $serial = (Invoke-Adb devices | Select-String -Pattern '^(emulator-\d+)\s+device' | ForEach-Object { $_.Matches[0].Groups[1].Value } | Select-Object -First 1)
    if (-not $serial) { throw 'no emulator device' }
    Write-Information "device $serial; $((Invoke-Adb -s $serial shell getprop ro.build.fingerprint | Out-String).Trim())" -InformationAction Continue
    Write-Information "wm size: $((Invoke-Adb -s $serial shell wm size | Out-String).Trim())" -InformationAction Continue
    Invoke-Adb -s $serial install -r $apk | Out-Null
    Invoke-Adb -s $serial logcat -c | Out-Null

    $signalUrl = "ws://10.0.2.2:$Port/"
    if ($Signalling -eq 'reverse') {
        Invoke-Adb -s $serial reverse "tcp:$Port" "tcp:$Port" | Out-Null
        $signalUrl = "ws://127.0.0.1:$Port/"
    }

    $hostLog = "$results/$Label-host.log"
    $hostArgs = @(
        'serve', '--port', $Port, '--advertise', '10.0.2.2', '--inject', $Inject,
        '--adb', $adb, '--serial', $serial, '--drive', $Taps, '--results', $results, '--label', $Label,
        '--secs', $RunSeconds, '--size', $Size, '--vendor', $Vendor
    )
    if ($StripAnswerCandidates) { $hostArgs += '--strip-answer-candidates' }
    # Start-Process joins the list with spaces and does not quote: quote what contains a space.
    $hostArgs = $hostArgs | ForEach-Object { $a = [string]$_; if ($a -match '\s') { "`"$a`"" } else { $a } }
    $splat = @{
        FilePath               = $hostExe
        ArgumentList           = $hostArgs
        PassThru               = $true
        WindowStyle            = 'Hidden'
        RedirectStandardOutput = $hostLog
        RedirectStandardError  = "$hostLog.err"
    }
    $hostProc = Start-Process @splat
    Write-Information "host started, pid $($hostProc.Id)" -InformationAction Continue

    $deadline = (Get-Date).AddSeconds(90)
    while (-not (Test-Path $hostLog) -or -not (Select-String -Path $hostLog -Pattern 'spike-input ready' -Quiet)) {
        if ($hostProc.HasExited) { throw "host exited early: $(Get-Content "$hostLog.err" -Raw)" }
        if ((Get-Date) -gt $deadline) { throw 'host did not become ready within 90 s' }
        Start-Sleep -Milliseconds 500
    }
    $testwinPids = @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$($hostProc.Id)" | Where-Object { $_.Name -eq 'gander-testwin.exe' } | ForEach-Object { $_.ProcessId })

    $tex = if ($TextureDecode) { 'true' } else { 'false' }
    Invoke-Adb -s $serial shell am start -n "$package/.MainActivity" --es signal $signalUrl --es client "emulator-$Label" --ez tex $tex | Out-Null
    Write-Information "app launched, signalling $signalUrl" -InformationAction Continue

    $shot = $false
    $limit = (Get-Date).AddSeconds($RunSeconds + 90)
    $scenarioLog = @()
    function Get-EpochMs { [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() }
    if ($Scenario -ne 'none') {
        $by = (Get-Date).AddSeconds(60)
        while (-not (Select-String -Path $hostLog -Pattern 'connected in' -Quiet)) {
            if ((Get-Date) -gt $by -or $hostProc.HasExited) { throw 'no connection for the scenario' }
            Start-Sleep -Milliseconds 500
        }
        Start-Sleep -Seconds 12
        $t0 = Get-EpochMs
        switch ($Scenario) {
            'background' {
                Invoke-Adb -s $serial shell input keyevent KEYCODE_HOME | Out-Null
                $scenarioLog += "epoch_ms=$t0 app sent to the background (HOME)"
                Start-Sleep -Seconds 20
                $t1 = Get-EpochMs
                Invoke-Adb -s $serial shell am start -n "$package/.MainActivity" | Out-Null
                $scenarioLog += "epoch_ms=$t1 app brought back to the foreground"
                Start-Sleep -Seconds 10
            }
            'screenoff' {
                Invoke-Adb -s $serial shell input keyevent KEYCODE_SLEEP | Out-Null
                $scenarioLog += "epoch_ms=$t0 screen off (KEYCODE_SLEEP)"
                Start-Sleep -Seconds 20
                $t1 = Get-EpochMs
                Invoke-Adb -s $serial shell input keyevent KEYCODE_WAKEUP | Out-Null
                $scenarioLog += "epoch_ms=$t1 screen on (KEYCODE_WAKEUP)"
                Start-Sleep -Seconds 10
            }
            'forcestop' {
                Invoke-Adb -s $serial shell am force-stop $package | Out-Null
                $scenarioLog += "epoch_ms=$t0 app force-stopped"
                $by = (Get-Date).AddSeconds(90)
                while (-not $hostProc.HasExited -and (Get-Date) -lt $by) { Start-Sleep -Milliseconds 250 }
                $scenarioLog += "epoch_ms=$(Get-EpochMs) host process exited: $($hostProc.HasExited)"
            }
        }
        Set-Content -Path "$results/$Label-scenario.txt" -Value $scenarioLog -Encoding utf8
        $limit = Get-Date
    }
    while (-not $hostProc.HasExited) {
        if ((Get-Date) -gt $limit) { Write-Warning 'host still running past the limit; killing it'; break }
        if (-not $shot -and (Select-String -Path $hostLog -Pattern 'driver: tap 3 ' -Quiet)) {
            $png = "$results/$Label-screen.png"
            $splat = @{ FilePath = $adb; ArgumentList = @('-s', $serial, 'exec-out', 'screencap', '-p'); RedirectStandardOutput = $png; Wait = $true; NoNewWindow = $true }
            Start-Process @splat
            $shot = $true
        }
        Start-Sleep -Seconds 2
    }
    Invoke-Adb -s $serial logcat -d -s 'GanderSpike:I' | Set-Content -Path "$results/$Label-logcat.txt" -Encoding utf8
    Invoke-Adb -s $serial shell am force-stop $package | Out-Null
}
finally {
    if ($hostProc -and -not $hostProc.HasExited) { Stop-Process -Id $hostProc.Id -Force -ErrorAction SilentlyContinue }
    foreach ($id in $testwinPids) { Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
    if ($Signalling -eq 'reverse') { Invoke-Adb reverse --remove "tcp:$Port" 2>$null | Out-Null }
    if ($booted -and -not $KeepEmulator) { Stop-Emulator }
}
Write-Information "done; results in $results (label $Label)" -InformationAction Continue
