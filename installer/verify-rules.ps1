# Gander installer: check that the firewall rules are the ones the installer should have made.
#
#   powershell.exe -NoProfile -File verify-rules.ps1
#   powershell.exe -NoProfile -File verify-rules.ps1 -SelfTest
#
# READ-ONLY. It never creates, changes or removes a rule and needs no elevation. It computes the
# rule plan from config.toml exactly as firewall-rules.ps1 would, then compares it with the rules
# in the firewall group "Gander": every planned rule exists and is enabled, inbound, Allow, on the
# planned profiles, protocol, port(s) and program, with exactly the planned RemoteAddress list; no
# rule in the group has RemoteAddress Any; no stray rule in the group. Prints PASS or FAIL per
# check and exits 0 only when every check passed.
#
# -SelfTest does not look at the firewall at all. It runs the pure functions of
# firewall-common.ps1 (CIDR and port validation, config.toml reading, the plan, refusing Any)
# against built-in cases, so `cargo xtask installer-check` can test the logic on any machine
# without touching its rules.
#
# Exit codes: 0 all checks passed, 1 a check failed, 2 bad input or configuration.
#
# Windows PowerShell 5.1 compatible (no ternary, ??, ?., && or ||); ASCII only.

[CmdletBinding()]
param(
    [string]$ProgramPath,
    [string]$ConfigPath,
    [switch]$IncludePublic,
    [switch]$SelfTest
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'firewall-common.ps1')

$script:Failures = 0
$script:Checks = 0

function Write-Check {
    param([bool]$Ok, [string]$Name, [string]$Detail)

    $script:Checks++
    $label = 'PASS'
    if (-not $Ok) {
        $label = 'FAIL'
        $script:Failures++
    }
    if ([string]::IsNullOrEmpty($Detail)) {
        Write-Output ('{0} {1}' -f $label, $Name)
    }
    else {
        Write-Output ('{0} {1}: {2}' -f $label, $Name, $Detail)
    }
}

function Test-SameSet {
    param([string[]]$Left, [string[]]$Right)

    $a = @($Left | Sort-Object)
    $b = @($Right | Sort-Object)
    return ((($a -join '|')) -ceq (($b -join '|')))
}

function Invoke-GanderSelfTest {
    # Expect-Refused: the call must throw. Expect-Value: the call must return the value.
    $accepted = @('100.64.0.0/10', 'fd7a:115c:a1e0::/48', '10.147.17.0/24', '192.168.0.0/16',
        '10.0.0.0/8', 'fd00:1::/64', '172.16.0.0/12', '169.254.0.0/16')
    foreach ($cidr in $accepted) {
        Write-Check -Ok ($null -eq (Test-GanderCidr -Cidr $cidr)) -Name ('accepts {0}' -f $cidr) -Detail ''
    }
    $refused = @('Any', 'any', '*', 'LocalSubnet', 'DefaultGateway', '', '   ', '0.0.0.0/0', '::/0',
        '0.0.0.0/8', '::/48', '10.0.0.1/8', '10.0.0.0/33', '10.0.0.0/7', '10.0.0.0', '10',
        '1/8', '300.1.1.1/8', '10.0.0.0/0', 'fe80::1%12/64', '10.0.0.0/8,0.0.0.0/0',
        '100.64.0.0/10-100.127.255.255', '128.0.0.0/1', '8000::/1', 'fd00::/15', '10.0.0.0/-1')
    foreach ($cidr in $refused) {
        Write-Check -Ok ($null -ne (Test-GanderCidr -Cidr $cidr)) -Name ('refuses "{0}"' -f $cidr) -Detail ''
    }
    Write-Check -Ok ($null -ne (Test-GanderCidr -Cidr $null)) -Name 'refuses $null' -Detail ''

    $threw = $false
    try {
        Assert-GanderRemoteAddress -Address @('100.64.0.0/10', 'Any')
    }
    catch {
        $threw = $_.Exception.Message -like 'refusing*'
    }
    Write-Check -Ok $threw -Name 'a list containing Any is refused' -Detail ''
    $threw = $false
    try {
        Assert-GanderRemoteAddress -Address @()
    }
    catch {
        $threw = $_.Exception.Message -like 'refusing*'
    }
    Write-Check -Ok $threw -Name 'an empty list is refused (it would mean Any)' -Detail ''

    foreach ($range in @('50000-50099', '1024-1024', '49152-49500', '2000 - 2999')) {
        Write-Check -Ok ($null -eq (Test-GanderPortRange -Range $range)) -Name ('accepts ports {0}' -f $range) -Detail ''
    }
    foreach ($range in @('', '80-90', '50099-50000', '50000', '50000-60000', 'a-b', '1023-2000', '2000-3000', '1-65535', '60000-70000')) {
        Write-Check -Ok ($null -ne (Test-GanderPortRange -Range $range)) -Name ('refuses ports "{0}"' -f $range) -Detail ''
    }

    Write-Check -Ok ((ConvertTo-GanderCidr -Address '100.64.0.0/255.192.0.0') -ceq '100.64.0.0/10') -Name 'normalises a dotted mask' -Detail ''
    Write-Check -Ok ((ConvertTo-GanderCidr -Address '10.147.17.0/255.255.255.0') -ceq '10.147.17.0/24') -Name 'normalises a /24 mask' -Detail ''
    Write-Check -Ok ((ConvertTo-GanderCidr -Address 'FD7A:115C:A1E0::/48') -ceq 'fd7a:115c:a1e0::/48') -Name 'normalises IPv6 case' -Detail ''
    Write-Check -Ok ((ConvertTo-GanderCidr -Address 'Any') -ceq 'Any') -Name 'leaves keywords alone' -Detail ''
    Write-Check -Ok ((ConvertTo-GanderCidr -Address '10.0.0.0/255.0.255.0') -ceq '10.0.0.0/255.0.255.0') -Name 'leaves a non-contiguous mask alone' -Detail ''

    # config.toml reading: the layout the host writes, plus comments, a multi-line array and
    # decoys in [[share]] tables that must be ignored.
    $sample = @'
version = 1

[listener]
port = 42700            # not the default
adapter = "generic"

[listener.generic]
interface = "ZeroTier One [8bd5124fd6]"   # brackets in a string
cidrs = [
    "10.147.17.0/24",   # ZeroTier
    'fd00:1::/64',
]

[firewall]
udp_ports = "49200-49299"
allow_public_profile = false

[[share]]
id = "shr_a"
name = "port = 1 # not a comment"
root = 'D:\Projects'
cidrs = ["0.0.0.0/0"]

[[share.grant]]
device = "dev_phone"
permissions = ["list"]
'@
    $settings = Read-GanderConfigText -Text $sample
    Write-Check -Ok ($settings.TcpPort -eq 42700) -Name 'reads listener.port' -Detail ([string]$settings.TcpPort)
    Write-Check -Ok ($settings.UdpPorts -ceq '49200-49299') -Name 'reads firewall.udp_ports' -Detail $settings.UdpPorts
    Write-Check -Ok (-not $settings.AllowPublicProfile) -Name 'reads firewall.allow_public_profile' -Detail ''
    Write-Check -Ok (Test-SameSet -Left $settings.GenericCidrs -Right @('10.147.17.0/24', 'fd00:1::/64')) -Name 'reads a multi-line listener.generic.cidrs' -Detail ($settings.GenericCidrs -join ',')

    $default = Read-GanderConfigText -Text ''
    Write-Check -Ok (($default.TcpPort -eq 42637) -and ($default.UdpPorts -ceq '50000-50099') -and (-not $default.AllowPublicProfile) -and (@($default.GenericCidrs).Count -eq 0)) -Name 'an empty config is the default' -Detail ''

    $bom = Read-GanderConfigText -Text ([string][char]0xFEFF + "version = 1`r`n[listener]`r`nport = 42800`r`n[firewall]`r`nallow_public_profile = true`r`n")
    Write-Check -Ok (($bom.TcpPort -eq 42800) -and $bom.AllowPublicProfile) -Name 'reads a BOM and CRLF file, allow_public_profile = true' -Detail ''

    $dotted = Read-GanderConfigText -Text "[listener]`ngeneric.cidrs = [`"192.168.50.0/24`"]`n"
    Write-Check -Ok (Test-SameSet -Left $dotted.GenericCidrs -Right @('192.168.50.0/24')) -Name 'reads a dotted key under a header' -Detail ''

    foreach ($bad in @("[listener]`nport = `"abc`"`n", "[listener]`nport = 0`n", "[listener]`nport = 70000`n",
            "[firewall]`nudp_ports = 50000`n", "[firewall]`nallow_public_profile = yes`n",
            "[listener.generic]`ncidrs = `"10.0.0.0/8`"`n", "[listener.generic]`ncidrs = [10.0.0.0/8]`n",
            "[listener.generic]`ncidrs = [`"10.0.0.0/8`"`n")) {
        $threw = $false
        try {
            $null = Read-GanderConfigText -Text $bad
        }
        catch {
            $threw = $true
        }
        Write-Check -Ok $threw -Name 'refuses an unreadable value' -Detail (($bad -replace "`n", ' ').Trim())
    }

    # The plan.
    $exe = 'C:\Users\Owner\AppData\Local\Programs\Gander\gander-host.exe'
    $plan = Get-GanderFirewallPlan -ProgramPath $exe -Settings $settings
    Write-Check -Ok (@($plan).Count -eq 2) -Name 'the plan has a TCP and a UDP rule' -Detail ''
    $tcp = $plan[0]
    $udp = $plan[1]
    Write-Check -Ok (($tcp.Protocol -ceq 'TCP') -and ($tcp.LocalPort -ceq '42700') -and ($udp.Protocol -ceq 'UDP') -and ($udp.LocalPort -ceq '49200-49299')) -Name 'the plan uses the configured ports' -Detail ''
    Write-Check -Ok (Test-SameSet -Left $tcp.RemoteAddress -Right @('100.64.0.0/10', 'fd7a:115c:a1e0::/48', '10.147.17.0/24', 'fd00:1::/64')) -Name 'RemoteAddress is the tailnet plus the generic CIDRs' -Detail ($tcp.RemoteAddress -join ',')
    Write-Check -Ok (Test-SameSet -Left $udp.RemoteAddress -Right $tcp.RemoteAddress) -Name 'both rules have the same RemoteAddress' -Detail ''
    Write-Check -Ok (Test-SameSet -Left $tcp.Profile -Right @('Private', 'Domain')) -Name 'the profiles are Private and Domain by default' -Detail ($tcp.Profile -join ',')
    Write-Check -Ok (($tcp.Group -ceq 'Gander') -and ($udp.Group -ceq 'Gander') -and ($tcp.Program -ceq $exe) -and ($tcp.Direction -ceq 'Inbound') -and ($tcp.Action -ceq 'Allow')) -Name 'rules are inbound Allow for the program in the group Gander' -Detail ''

    $withPublic = Get-GanderFirewallPlan -ProgramPath $exe -Settings $settings -IncludePublic
    Write-Check -Ok (Test-SameSet -Left $withPublic[0].Profile -Right @('Private', 'Domain', 'Public')) -Name '-IncludePublic adds Public and nothing else' -Detail ''
    $configuredPublic = Get-GanderFirewallPlan -ProgramPath $exe -Settings (Read-GanderConfigText -Text "[firewall]`nallow_public_profile = true`n")
    Write-Check -Ok (Test-SameSet -Left $configuredPublic[0].Profile -Right @('Private', 'Domain', 'Public')) -Name 'allow_public_profile = true adds Public' -Detail ''
    $defaultPlan = Get-GanderFirewallPlan -ProgramPath $exe -Settings $default
    Write-Check -Ok ((Test-SameSet -Left $defaultPlan[0].RemoteAddress -Right $script:GanderTailnetCidrs) -and ($defaultPlan[0].LocalPort -ceq '42637')) -Name 'the default plan is the tailnet only on port 42637' -Detail ''

    # A configuration that would open the rules wide produces no plan at all.
    foreach ($wide in @('0.0.0.0/0', '::/0', 'Any', '*', '10.0.0.0/7')) {
        $hostile = Read-GanderConfigText -Text ("[listener.generic]`ncidrs = [`"{0}`"]`n" -f $wide)
        $threw = $false
        try {
            $null = Get-GanderFirewallPlan -ProgramPath $exe -Settings $hostile
        }
        catch {
            $threw = $_.Exception.Message -like 'refusing*'
        }
        Write-Check -Ok $threw -Name ('a config with generic CIDR "{0}" produces no plan' -f $wide) -Detail ''
    }
    $threw = $false
    try {
        $null = Get-GanderFirewallPlan -ProgramPath 'gander-host.exe' -Settings $default
    }
    catch {
        $threw = $true
    }
    Write-Check -Ok $threw -Name 'a relative program path is refused' -Detail ''
    $threw = $false
    try {
        $null = Get-GanderFirewallPlan -ProgramPath $exe -Settings (Read-GanderConfigText -Text "[firewall]`nudp_ports = `"80-90`"`n")
    }
    catch {
        $threw = $true
    }
    Write-Check -Ok $threw -Name 'a UDP range below 1024 is refused' -Detail ''
}

function Invoke-GanderVerify {
    $program = $ProgramPath
    if ([string]::IsNullOrEmpty($program)) {
        $program = Join-Path $PSScriptRoot 'gander-host.exe'
    }
    $config = Get-GanderConfigPath -ConfigPath $ConfigPath
    Write-Output ('Config: {0}' -f $config)
    Write-Output ('Program: {0}' -f $program)
    $settings = Read-GanderConfig -ConfigPath $config
    $plan = Get-GanderFirewallPlan -ProgramPath $program -Settings $settings -IncludePublic:$IncludePublic

    $installed = @(Get-GanderInstalledRule)
    Write-Check -Ok ($installed.Count -eq @($plan).Count) -Name 'rule count in the group "Gander"' -Detail ('{0} found, {1} planned' -f $installed.Count, @($plan).Count)
    $plannedNames = @($plan | ForEach-Object { $_.Name })
    foreach ($rule in $installed) {
        Write-Check -Ok ($plannedNames -contains $rule.Name) -Name ('no stray rule {0}' -f $rule.Name) -Detail ''
        $any = $false
        foreach ($address in @($rule.RemoteAddress)) {
            $normalised = ConvertTo-GanderCidr -Address $address
            if ($null -ne (Test-GanderCidr -Cidr $normalised)) {
                $any = $true
            }
        }
        Write-Check -Ok (-not $any) -Name ('{0} has no unscoped RemoteAddress (Any, a keyword or a range)' -f $rule.Name) -Detail (@($rule.RemoteAddress) -join ',')
    }
    foreach ($want in $plan) {
        $have = $null
        foreach ($rule in $installed) {
            if ($rule.Name -ceq $want.Name) {
                $have = $rule
            }
        }
        if ($null -eq $have) {
            Write-Check -Ok $false -Name ('{0} exists' -f $want.Name) -Detail 'missing'
            continue
        }
        Write-Check -Ok $true -Name ('{0} exists' -f $want.Name) -Detail ''
        Write-Check -Ok (($have.Enabled -eq 'True') -and ($have.Direction -eq 'Inbound') -and ($have.Action -eq 'Allow')) -Name ('{0} is enabled, inbound, Allow' -f $want.Name) -Detail ('{0}/{1}/{2}' -f $have.Enabled, $have.Direction, $have.Action)
        $haveProfiles = @(($have.Profile -split ',') | ForEach-Object { $_.Trim() })
        Write-Check -Ok (Test-SameSet -Left $haveProfiles -Right $want.Profile) -Name ('{0} profiles' -f $want.Name) -Detail ('{0}, planned {1}' -f $have.Profile, ($want.Profile -join ','))
        Write-Check -Ok (($have.Protocol -eq $want.Protocol) -and ($have.LocalPort -eq $want.LocalPort)) -Name ('{0} protocol and port' -f $want.Name) -Detail ('{0} {1}, planned {2} {3}' -f $have.Protocol, $have.LocalPort, $want.Protocol, $want.LocalPort)
        Write-Check -Ok ($have.Program -ieq $want.Program) -Name ('{0} program' -f $want.Name) -Detail ('{0}, planned {1}' -f $have.Program, $want.Program)
        $haveAddresses = @(@($have.RemoteAddress) | ForEach-Object { ConvertTo-GanderCidr -Address $_ })
        Write-Check -Ok (Test-SameSet -Left $haveAddresses -Right $want.RemoteAddress) -Name ('{0} RemoteAddress' -f $want.Name) -Detail ('{0}; planned {1}' -f ($haveAddresses -join ','), ($want.RemoteAddress -join ','))
    }
}

try {
    if ($SelfTest) {
        Invoke-GanderSelfTest
        Write-Output ('selftest: {0} check(s), {1} failure(s)' -f $script:Checks, $script:Failures)
    }
    else {
        Invoke-GanderVerify
        Write-Output ('verify: {0} check(s), {1} failure(s)' -f $script:Checks, $script:Failures)
    }
}
catch {
    Write-Output ('ERROR {0}' -f $_.Exception.Message)
    exit 2
}
if ($script:Failures -gt 0) {
    exit 1
}
exit 0
