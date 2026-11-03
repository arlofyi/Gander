# Helpers for the WP-47 Files-app spike driver. Dot-source this file; it defines functions only.
# Run scripts with: pwsh -NoProfile -File <script.ps1>   (PowerShell 7+).

Set-StrictMode -Version Latest

if (-not $env:ANDROID_HOME) { throw 'Set ANDROID_HOME to the Android SDK folder (for example %LOCALAPPDATA%/Android/Sdk).' }
$script:SdkRoot = $env:ANDROID_HOME
$script:Adb = "$($script:SdkRoot)/platform-tools/adb.exe"
$script:Emulator = "$($script:SdkRoot)/emulator/emulator.exe"
$script:Package = 'fyi.arlo.gander.spike.files'
$script:Authority = 'fyi.arlo.gander.spike.files.documents'
$script:DataDir = "/sdcard/Android/data/$($script:Package)/files"

function Invoke-Adb {
    # Runs adb and returns its stdout lines. Deliberately NOT an advanced function: with
    # [CmdletBinding()] PowerShell would swallow adb's own -a/-d/-t flags as common parameters.
    & $script:Adb @args
}

function Save-AdbScreenshot {
    <# Raw PNG from the emulator screen (Start-Process keeps the bytes intact). #>
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$Path)
    $dir = Split-Path -Parent $Path
    if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    $splat = @{
        FilePath               = $script:Adb
        ArgumentList           = @('exec-out', 'screencap', '-p')
        RedirectStandardOutput = $Path
        Wait                   = $true
        NoNewWindow            = $true
    }
    Start-Process @splat
}

function Get-SpikeEmulatorProcess {
    Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.ProcessName -match '^(qemu-system|emulator)' }
}

function Start-SpikeEmulator {
    <# Boots the `gander` AVD headless and waits for sys.boot_completed. Refuses to start a second one. #>
    [CmdletBinding()]
    param([string]$LogPath, [int]$BootTimeoutSec = 300)
    if (Get-SpikeEmulatorProcess) { throw 'An emulator or qemu process is already running; only one instance may run.' }
    if (-not $env:ANDROID_AVD_HOME) { throw 'Set ANDROID_AVD_HOME to the folder that holds the gander AVD (for example %USERPROFILE%/.android/avd).' }
    $emuArgs = @('-avd', 'gander', '-port', '5554', '-no-window', '-no-audio', '-no-snapshot-save', '-no-snapshot-load', '-gpu', 'swiftshader_indirect')
    $splat = @{
        FilePath               = $script:Emulator
        ArgumentList           = $emuArgs
        PassThru               = $true
        WindowStyle            = 'Hidden'
        RedirectStandardOutput = $LogPath
        RedirectStandardError  = "$LogPath.err"
    }
    $proc = Start-Process @splat
    Write-Information "emulator started, pid $($proc.Id)" -InformationAction Continue
    Invoke-Adb wait-for-device | Out-Null
    $deadline = (Get-Date).AddSeconds($BootTimeoutSec)
    while ((Get-Date) -lt $deadline) {
        $boot = (Invoke-Adb shell getprop sys.boot_completed 2>$null | Out-String).Trim()
        if ($boot -eq '1') { return }
        Start-Sleep -Seconds 3
    }
    throw "Emulator did not finish booting within $BootTimeoutSec s."
}

function Stop-SpikeEmulator {
    <# adb emu kill, then confirm no emulator/qemu process is left. Throws if one survives. #>
    [CmdletBinding()]
    param([int]$TimeoutSec = 60)
    Invoke-Adb emu kill 2>$null | Out-Null
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        if (-not (Get-SpikeEmulatorProcess)) { break }
        Start-Sleep -Seconds 2
    }
    $left = @(Get-SpikeEmulatorProcess)
    if ($left.Count -gt 0) { throw "Emulator processes still running: $(($left | ForEach-Object { $_.ProcessName }) -join ', ')" }
    $devices = (Invoke-Adb devices | Out-String)
    Write-Information "emulator stopped; adb devices: $(($devices -split "`n" | Select-Object -Skip 1 | Where-Object { $_.Trim() }) -join '; ')" -InformationAction Continue
}

function Set-SpikeConf {
    <# Writes <externalFilesDir>/spike.conf on the device (empty hashtable clears it). #>
    [CmdletBinding()]
    param([hashtable]$Conf = @{})
    if ($Conf.Count -eq 0) {
        Invoke-Adb shell "rm -f $($script:DataDir)/spike.conf" | Out-Null
        return
    }
    $text = ($Conf.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join '\n'
    # chmod 666: a file created by the shell user is 0660 and the app (another uid) gets EACCES.
    Invoke-Adb shell "printf '$text\n' > $($script:DataDir)/spike.conf; chmod 666 $($script:DataDir)/spike.conf" | Out-Null
}

function Get-UiNode {
    <# Dumps the UI hierarchy and returns one object per node with text, desc, id and a tap centre. #>
    [CmdletBinding()]
    param()
    Invoke-Adb shell rm -f /sdcard/ui.xml | Out-Null # a failed dump must not return the previous screen
    Invoke-Adb shell uiautomator dump /sdcard/ui.xml 2>$null | Out-Null
    $raw = (Invoke-Adb exec-out cat /sdcard/ui.xml | Out-String)
    if (-not $raw.Contains('<hierarchy')) { return @() }
    $xml = [xml]$raw.Substring($raw.IndexOf('<?xml'))
    foreach ($n in $xml.SelectNodes('//node')) {
        $m = [regex]::Match($n.bounds, '\[(\d+),(\d+)\]\[(\d+),(\d+)\]')
        if (-not $m.Success) { continue }
        [pscustomobject]@{
            Text      = $n.text
            Desc      = $n.'content-desc'
            Id        = $n.'resource-id'
            Clickable = ($n.clickable -eq 'true')
            X         = [int](([int]$m.Groups[1].Value + [int]$m.Groups[3].Value) / 2)
            Y         = [int](([int]$m.Groups[2].Value + [int]$m.Groups[4].Value) / 2)
        }
    }
}

function Find-UiNode {
    <# First node whose text, content-desc or id matches the regex. $null when none. #>
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$Pattern)
    Get-UiNode | Where-Object { $_.Text -match $Pattern -or $_.Desc -match $Pattern -or $_.Id -match $Pattern } | Select-Object -First 1
}

function Wait-UiNode {
    <# Polls the UI until a node matches; returns it, or $null on timeout. #>
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$Pattern, [int]$TimeoutSec = 20)
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    do {
        $node = Find-UiNode -Pattern $Pattern
        if ($node) { return $node }
        Start-Sleep -Milliseconds 700
    } while ((Get-Date) -lt $deadline)
    return $null
}

function Invoke-UiTap {
    <# Waits for a node and taps its centre. Throws when it never appears (unless -Optional). #>
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$Pattern, [int]$TimeoutSec = 20, [switch]$Optional, [switch]$LongPress)
    $node = Wait-UiNode -Pattern $Pattern -TimeoutSec $TimeoutSec
    if (-not $node) {
        if ($Optional) { return $false }
        throw "UI node matching '$Pattern' not found within $TimeoutSec s."
    }
    if ($LongPress) {
        Invoke-Adb shell input swipe $node.X $node.Y $node.X $node.Y 1000 | Out-Null
    } else {
        Invoke-Adb shell input tap $node.X $node.Y | Out-Null
    }
    return $true
}

function Save-SpikeLog {
    <# Saves the GanderSpike logcat lines since the last `logcat -c`, trimmed to `HH:mm:ss.fff EVENT k=v ...`. #>
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$Path, [switch]$IncludeFetch)
    $dir = Split-Path -Parent $Path
    if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    $lines = Invoke-Adb logcat -d -v threadtime -s 'GanderSpike:V'
    $keep = foreach ($l in $lines) {
        $m = [regex]::Match($l, '^\d\d-\d\d (\d\d:\d\d:\d\d\.\d+)\s+\d+\s+(\d+)\s+\w\s+GanderSpike:\s+([A-Z_]+)\s*(.*)$')
        if (-not $m.Success) { continue }
        $event = $m.Groups[3].Value
        if ($event -eq 'FETCH' -and -not $IncludeFetch) { continue }
        if ($event -eq 'HTTP') { continue } # server side lines duplicate FETCH and HEAD
        "$($m.Groups[1].Value) tid=$($m.Groups[2].Value) $event $($m.Groups[4].Value)"
    }
    Set-Content -Path $Path -Value $keep -Encoding utf8
    return @($keep).Count
}

function Clear-SpikeLog {
    [CmdletBinding()]
    param()
    Invoke-Adb logcat -c | Out-Null
}

function Wait-SpikeLogLine {
    <# Polls logcat until a GanderSpike line matches the regex. Returns the line, or $null on timeout. #>
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$Pattern, [int]$TimeoutSec = 60)
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    do {
        $hit = Invoke-Adb logcat -d -v threadtime -s 'GanderSpike:V' | Where-Object { $_ -match $Pattern } | Select-Object -First 1
        if ($hit) { return $hit }
        Start-Sleep -Seconds 1
    } while ((Get-Date) -lt $deadline)
    return $null
}
