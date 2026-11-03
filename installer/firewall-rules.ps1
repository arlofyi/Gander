# Gander installer: create or remove the Windows Firewall allow rules (WP-1A).
#
#   powershell.exe -NoProfile -ExecutionPolicy Bypass -File firewall-rules.ps1 -Action Install
#   powershell.exe -NoProfile -ExecutionPolicy Bypass -File firewall-rules.ps1 -Action Remove
#   powershell.exe -NoProfile -ExecutionPolicy Bypass -File firewall-rules.ps1 -Action Plan
#
# Install creates two inbound ALLOW rules for gander-host.exe, both in the rule group "Gander":
#   Gander-In-TCP  the listener port (listener.port in config.toml, default 42637)
#   Gander-In-UDP  the WebRTC host-candidate range (firewall.udp_ports, default 50000-50099)
# RemoteAddress is ONLY the tailnet ranges (100.64.0.0/10, fd7a:115c:a1e0::/48) plus
# listener.generic.cidrs from config.toml. Profiles: Private and Domain; Public only when
# firewall.allow_public_profile = true (or -IncludePublic): Tailscale's adapter is sometimes
# classified Public, and a rule that skips Public then never matches. The source scope stays the
# same either way. The script refuses RemoteAddress Any, an empty scope, /0 and anything broader
# than a private network, before it touches the firewall.
#
# Remove deletes every rule in the group "Gander" (and nothing else). Both actions are idempotent.
# Plan prints what Install would create and changes nothing; it needs no elevation.
#
# Changing firewall rules needs administrator rights, and a per-user MSI runs without them. So
# Install and Remove re-launch themselves elevated (one UAC prompt) unless -NoElevate is given
# (for a shell that is already elevated). See installer/README.md, "Firewall rules".
#
# Exit codes: 0 done, 1 failed, 2 bad input or configuration, 3 refused (unsafe scope),
#             5 not elevated (the prompt was declined or not answered within two minutes).
#
# Windows PowerShell 5.1 compatible (no ternary, ??, ?., && or ||); ASCII only.

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('Install', 'Remove', 'Plan')]
    [string]$Action,

    # gander-host.exe the rules are tied to. Default: the copy next to this script.
    [string]$ProgramPath,

    # config.toml to read. Default: %APPDATA%\Gander\config.toml of the current user.
    [string]$ConfigPath,

    # Also open the rules on Public-profile networks (same as firewall.allow_public_profile).
    [switch]$IncludePublic,

    # Do not re-launch elevated; fail with exit 5 when not already elevated.
    [switch]$NoElevate,

    # Run the elevated copy in a hidden window (the MSI uses this).
    [switch]$Quiet,

    # Append the progress lines to this file as well (the elevated copy's output is otherwise
    # lost with its window). Default: %LOCALAPPDATA%\Gander\logs\firewall-setup.log.
    [string]$LogPath
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

$script:ExitOk = 0
$script:ExitFailed = 1
$script:ExitBadInput = 2
$script:ExitRefused = 3
$script:ExitNotElevated = 5
$script:ElevationTimeoutMs = 120000

. (Join-Path $PSScriptRoot 'firewall-common.ps1')

function Write-GanderLog {
    param([string]$Message)

    $line = '{0} {1}' -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $Message
    Write-Information -MessageData $line -InformationAction Continue
    if (-not [string]::IsNullOrEmpty($script:LogFile)) {
        try {
            $dir = Split-Path -Parent $script:LogFile
            if (-not (Test-Path -LiteralPath $dir)) {
                New-Item -ItemType Directory -Path $dir -Force | Out-Null
            }
            Add-Content -LiteralPath $script:LogFile -Value $line -Encoding UTF8
        }
        catch {
            Write-Warning ('cannot write the log file: {0}' -f $_.Exception.Message)
            $script:LogFile = $null
        }
    }
}

function Test-GanderElevated {
    $identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object System.Security.Principal.WindowsPrincipal($identity)
    return $principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Get-GanderProgramPath {
    param([string]$Path)

    if (-not [string]::IsNullOrEmpty($Path)) {
        return $Path
    }
    return (Join-Path $PSScriptRoot 'gander-host.exe')
}

function Remove-GanderRuleGroup {
    # Deletes every rule in the group. Returns how many there were.
    $existing = @(Get-NetFirewallRule -Group $script:GanderRuleGroup -ErrorAction SilentlyContinue)
    if ($existing.Count -gt 0) {
        $existing | Remove-NetFirewallRule -ErrorAction Stop
    }
    return $existing.Count
}

function Invoke-GanderElevated {
    # Re-runs this script elevated with already-resolved paths (the elevated user may be a
    # different account, so %APPDATA% must not be re-read there). Returns the exit code.
    param([string]$Program, [string]$Config, [string]$Log)

    $powershell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    $parts = New-Object System.Collections.Generic.List[string]
    $parts.Add('-NoProfile')
    $parts.Add('-NonInteractive')
    $parts.Add('-ExecutionPolicy')
    $parts.Add('Bypass')
    $parts.Add('-File')
    $parts.Add('"' + $PSCommandPath + '"')
    $parts.Add('-Action')
    $parts.Add($Action)
    $parts.Add('-NoElevate')
    $parts.Add('-ProgramPath')
    $parts.Add('"' + $Program + '"')
    $parts.Add('-ConfigPath')
    $parts.Add('"' + $Config + '"')
    $parts.Add('-LogPath')
    $parts.Add('"' + $Log + '"')
    if ($IncludePublic) {
        $parts.Add('-IncludePublic')
    }
    $style = 'Normal'
    if ($Quiet) {
        $style = 'Hidden'
    }
    Write-GanderLog 'Asking Windows for administrator permission (UAC prompt)...'
    try {
        $process = Start-Process -FilePath $powershell -ArgumentList ($parts.ToArray() -join ' ') -Verb RunAs -PassThru -WindowStyle $style
    }
    catch {
        Write-GanderLog ('Administrator permission was not given: {0}' -f $_.Exception.Message)
        return $script:ExitNotElevated
    }
    # Do not wait forever on an unanswered UAC prompt (a silent uninstall must not hang on it).
    if (-not $process.WaitForExit($script:ElevationTimeoutMs)) {
        Write-GanderLog 'No answer to the administrator prompt within two minutes; giving up.'
        return $script:ExitNotElevated
    }
    if (Test-Path -LiteralPath $Log) {
        Write-GanderLog 'Output of the elevated step:'
        Get-Content -LiteralPath $Log -Tail 12 | ForEach-Object { Write-Information -MessageData ('  ' + $_) -InformationAction Continue }
    }
    return [int]$process.ExitCode
}

function Invoke-GanderAction {
    # Returns the exit code.
    $program = Get-GanderProgramPath -Path $ProgramPath
    $config = Get-GanderConfigPath -ConfigPath $ConfigPath
    $settings = Read-GanderConfig -ConfigPath $config

    $plan = $null
    if ($Action -ne 'Remove') {
        try {
            $plan = Get-GanderFirewallPlan -ProgramPath $program -Settings $settings -IncludePublic:$IncludePublic
        }
        catch {
            $message = $_.Exception.Message
            Write-GanderLog ('Refused: {0}' -f $message)
            if ($message -like 'refusing*') {
                return $script:ExitRefused
            }
            return $script:ExitBadInput
        }
    }

    if ($Action -eq 'Plan') {
        Write-GanderLog ('Config: {0}' -f $config)
        foreach ($rule in $plan) {
            Write-GanderLog ('{0}: {1} {2} from {3} on {4} for {5}' -f $rule.Name, $rule.Protocol, $rule.LocalPort, ($rule.RemoteAddress -join ','), ($rule.Profile -join ','), $rule.Program)
        }
        Write-GanderLog 'Plan only: nothing was changed.'
        return $script:ExitOk
    }

    if (-not $NoElevate -and -not (Test-GanderElevated)) {
        $log = $script:LogFile
        if ([string]::IsNullOrEmpty($log)) {
            $log = Join-Path $env:TEMP 'gander-firewall-setup.log'
        }
        return (Invoke-GanderElevated -Program $program -Config $config -Log $log)
    }
    if (-not (Test-GanderElevated)) {
        Write-GanderLog 'Changing firewall rules needs an elevated (administrator) shell.'
        return $script:ExitNotElevated
    }

    if ($Action -eq 'Remove') {
        $count = Remove-GanderRuleGroup
        Write-GanderLog ('Removed {0} firewall rule(s) from the group "{1}".' -f $count, $script:GanderRuleGroup)
        return $script:ExitOk
    }

    # Install. The program must exist: a rule for a missing file would silently match nothing.
    if (-not (Test-Path -LiteralPath $program -PathType Leaf)) {
        Write-GanderLog ('gander-host.exe not found at {0}' -f $program)
        return $script:ExitBadInput
    }
    try {
        # Re-creating the group makes Install idempotent: no duplicates, and an edited or
        # half-removed rule set is put back as planned.
        $removed = Remove-GanderRuleGroup
        if ($removed -gt 0) {
            Write-GanderLog ('Replacing {0} existing Gander rule(s).' -f $removed)
        }
        foreach ($rule in $plan) {
            Assert-GanderRemoteAddress -Address $rule.RemoteAddress
            New-NetFirewallRule @rule -Enabled True -EdgeTraversalPolicy Block -ErrorAction Stop | Out-Null
            Write-GanderLog ('Created {0}: {1} {2} from {3} on {4}' -f $rule.Name, $rule.Protocol, $rule.LocalPort, ($rule.RemoteAddress -join ','), ($rule.Profile -join ','))
        }
    }
    catch {
        Write-GanderLog ('Install failed: {0}' -f $_.Exception.Message)
        # No half-installed state: take back whatever was created.
        try {
            $null = Remove-GanderRuleGroup
        }
        catch {
            Write-GanderLog ('Rollback failed: {0}' -f $_.Exception.Message)
        }
        return $script:ExitFailed
    }
    Write-GanderLog 'Done. Check any time with verify-rules.ps1.'
    return $script:ExitOk
}

$script:LogFile = $LogPath
if ([string]::IsNullOrEmpty($script:LogFile) -and -not [string]::IsNullOrEmpty($env:LOCALAPPDATA)) {
    $script:LogFile = Join-Path (Join-Path (Join-Path $env:LOCALAPPDATA 'Gander') 'logs') 'firewall-setup.log'
}
if ($Action -eq 'Plan') {
    # Plan changes nothing and writes no log.
    $script:LogFile = $null
}

$code = $script:ExitFailed
try {
    $code = Invoke-GanderAction
}
catch {
    Write-GanderLog ('Failed: {0}' -f $_.Exception.Message)
    $code = $script:ExitFailed
}
exit $code
