# Gander installer: shared firewall plan logic (WP-1A).
#
# Dot-sourced by firewall-rules.ps1 (install/remove, elevated) and verify-rules.ps1 (read-only).
# Windows PowerShell 5.1 compatible: no ternary, no ??, no ?., no && or ||, ASCII only
# (`cargo xtask installer-check` parses every installer script with the 5.1 parser).
#
# Two kinds of function live here:
#   - PURE functions (CIDR and port validation, config.toml reading, the rule plan). They never
#     call a NetSecurity cmdlet; `verify-rules.ps1 -SelfTest` exercises exactly these.
#   - READERS (Get-GanderInstalledRule): read-only Get-NetFirewall* calls.
# Nothing in this file creates, changes or removes a firewall rule.

Set-StrictMode -Version 2.0

$script:GanderRuleGroup = 'Gander'
$script:GanderTailnetCidrs = @('100.64.0.0/10', 'fd7a:115c:a1e0::/48')
$script:GanderDefaultTcpPort = 42637
$script:GanderDefaultUdpPorts = '50000-50099'
$script:GanderMinUdpPort = 1024
$script:GanderMaxUdpSpan = 1000
$script:GanderMinPrefixV4 = 8
$script:GanderMinPrefixV6 = 16

# --------------------------------------------------------------------------------------------
# PURE: validation
# --------------------------------------------------------------------------------------------

function Test-GanderCidr {
    <#
    .SYNOPSIS
        Returns $null when $Cidr is an acceptable RemoteAddress scope, otherwise the reason.
    .DESCRIPTION
        Only address/prefix notation is accepted. Any, *, LocalSubnet and the other firewall
        keywords, a zero prefix, the unspecified address, set host bits and networks broader than
        a private network (IPv4 shorter than /8, IPv6 shorter than /16) are all refused, so a
        rule can never end up open to the whole internet.
    #>
    param([AllowNull()][AllowEmptyString()][string]$Cidr)

    if ($null -eq $Cidr) {
        return 'empty address'
    }
    $text = $Cidr.Trim()
    if ($text.Length -eq 0) {
        return 'empty address'
    }
    if ($text -match '^(?i:any|\*)$') {
        return 'RemoteAddress Any is refused'
    }
    if (-not ($text -match '^([^/\s]+)/(\d{1,3})$')) {
        return ('"{0}" is not address/prefix notation' -f $text)
    }
    $addressText = $Matches[1]
    $prefix = [int]$Matches[2]

    $isV4 = ($addressText -match '^\d{1,3}(\.\d{1,3}){3}$')
    $isV6 = ($addressText.Contains(':') -and -not $addressText.Contains('%'))
    if (-not ($isV4 -or $isV6)) {
        return ('"{0}" is not a plain IPv4 or IPv6 address' -f $addressText)
    }
    $ip = $null
    if (-not [System.Net.IPAddress]::TryParse($addressText, [ref]$ip)) {
        return ('"{0}" is not a valid address' -f $addressText)
    }

    $bytes = $ip.GetAddressBytes()
    $width = $bytes.Length * 8
    if ($prefix -lt 1 -or $prefix -gt $width) {
        return ('prefix /{0} is out of range (1 to {1}); /0 would match every address' -f $prefix, $width)
    }
    $allZero = $true
    foreach ($b in $bytes) {
        if ($b -ne 0) {
            $allZero = $false
        }
    }
    if ($allZero) {
        return 'the unspecified address (0.0.0.0 or ::) is refused'
    }
    $minimum = $script:GanderMinPrefixV6
    if ($isV4) {
        $minimum = $script:GanderMinPrefixV4
    }
    if ($prefix -lt $minimum) {
        return ('/{0} is broader than a private network (minimum /{1})' -f $prefix, $minimum)
    }
    # Host bits (everything after the prefix) must be zero.
    for ($i = 0; $i -lt $bytes.Length; $i++) {
        $bitsInByte = [Math]::Max(0, [Math]::Min(8, $prefix - (8 * $i)))
        $hostMask = 255 - ((255 -shl (8 - $bitsInByte)) -band 255)
        if (($bytes[$i] -band $hostMask) -ne 0) {
            return ('"{0}" has host bits set after the /{1} prefix' -f $text, $prefix)
        }
    }
    return $null
}

function Assert-GanderRemoteAddress {
    <#
    .SYNOPSIS
        Throws unless every entry of $Address is an acceptable scope. An empty list is refused
        too: a rule with no RemoteAddress means Any.
    #>
    param([AllowNull()][AllowEmptyCollection()][string[]]$Address)

    $list = @($Address)
    if ($list.Count -eq 0) {
        throw 'refusing a rule with no RemoteAddress (that means Any)'
    }
    foreach ($entry in $list) {
        $problem = Test-GanderCidr -Cidr $entry
        if ($null -ne $problem) {
            throw ('refusing RemoteAddress "{0}": {1}' -f $entry, $problem)
        }
    }
}

function Test-GanderPortRange {
    <#
    .SYNOPSIS
        Returns $null when $Range is "first-last" (or one port) inside 1024-65535 spanning at
        most 1000 ports, otherwise the reason. Mirrors gander-core's UdpPortRange.
    #>
    param([AllowNull()][AllowEmptyString()][string]$Range)

    if ($null -eq $Range -or $Range.Trim().Length -eq 0) {
        return 'empty port range'
    }
    if (-not ($Range.Trim() -match '^(\d{1,5})\s*-\s*(\d{1,5})$')) {
        return ('"{0}" is not first-last, for example 50000-50099' -f $Range)
    }
    $first = [int]$Matches[1]
    $last = [int]$Matches[2]
    if ($first -gt $last) {
        return 'the first port is above the last'
    }
    if ($first -lt $script:GanderMinUdpPort) {
        return ('ports below {0} are not allowed' -f $script:GanderMinUdpPort)
    }
    if ($last -gt 65535) {
        return 'ports above 65535 do not exist'
    }
    if (($last - $first) -ge $script:GanderMaxUdpSpan) {
        return ('the range may span at most {0} ports' -f $script:GanderMaxUdpSpan)
    }
    return $null
}

function ConvertTo-GanderCidr {
    <#
    .SYNOPSIS
        Normalises a RemoteAddress as Windows reports it (IPv4 networks come back with a dotted
        mask, 100.64.0.0/255.192.0.0) to address/prefix with a canonical address. Anything that is
        not a subnet (a keyword such as Any, a single host, a range) is returned unchanged.
    #>
    param([string]$Address)

    $text = $Address.Trim()
    if (-not ($text -match '^([^/\s]+)/(\S+)$')) {
        return $text
    }
    $addressText = $Matches[1]
    $maskText = $Matches[2]
    $ip = $null
    if (-not [System.Net.IPAddress]::TryParse($addressText, [ref]$ip)) {
        return $text
    }
    $prefix = -1
    if ($maskText -match '^\d{1,3}$') {
        $prefix = [int]$maskText
    }
    elseif ($maskText -match '^\d{1,3}(\.\d{1,3}){3}$') {
        $maskIp = $null
        if ([System.Net.IPAddress]::TryParse($maskText, [ref]$maskIp)) {
            $count = 0
            $seenZero = $false
            foreach ($b in $maskIp.GetAddressBytes()) {
                for ($bit = 7; $bit -ge 0; $bit--) {
                    if (($b -band (1 -shl $bit)) -ne 0) {
                        if ($seenZero) {
                            return $text
                        }
                        $count++
                    }
                    else {
                        $seenZero = $true
                    }
                }
            }
            $prefix = $count
        }
    }
    if ($prefix -lt 0) {
        return $text
    }
    return ('{0}/{1}' -f $ip.ToString().ToLowerInvariant(), $prefix)
}

# --------------------------------------------------------------------------------------------
# PURE: config.toml
# --------------------------------------------------------------------------------------------

function Remove-GanderTomlComment {
    # Drops a trailing "# comment" that is not inside a quoted string.
    param([string]$Line)

    if (-not $Line.Contains('#')) {
        return $Line
    }
    $quote = [char]0
    $escaped = $false
    for ($i = 0; $i -lt $Line.Length; $i++) {
        $c = $Line[$i]
        if ($quote -ne [char]0) {
            if ($quote -eq '"' -and $escaped) {
                $escaped = $false
            }
            elseif ($quote -eq '"' -and $c -eq '\') {
                $escaped = $true
            }
            elseif ($c -eq $quote) {
                $quote = [char]0
            }
        }
        elseif ($c -eq '"' -or $c -eq "'") {
            $quote = $c
        }
        elseif ($c -eq '#') {
            return $Line.Substring(0, $i)
        }
    }
    return $Line
}

function Get-GanderTomlBracketDepth {
    # [ minus ] outside quoted strings (for arrays that continue on the next line).
    param([string]$Text)

    $depth = 0
    $quote = [char]0
    foreach ($c in $Text.ToCharArray()) {
        if ($quote -ne [char]0) {
            if ($c -eq $quote) {
                $quote = [char]0
            }
        }
        elseif ($c -eq '"' -or $c -eq "'") {
            $quote = $c
        }
        elseif ($c -eq '[') {
            $depth++
        }
        elseif ($c -eq ']') {
            $depth--
        }
    }
    return $depth
}

function Read-GanderConfigText {
    <#
    .SYNOPSIS
        Reads the four settings the firewall rules depend on from the text of config.toml.
    .DESCRIPTION
        listener.port (default 42637), firewall.udp_ports (default 50000-50099),
        firewall.allow_public_profile (default false) and listener.generic.cidrs (default none).
        Windows PowerShell 5.1 has no TOML parser, so this reads the layout the host writes:
        [section] headers, key = value lines (dotted keys under a header work), # comments and
        arrays that span lines. Everything else in the file is ignored, so [[share]] tables
        cannot influence the rules. A value of interest that cannot be read is an error, never
        a silent default. The host validates the whole file when it starts; this is only the
        installer's view of it.
    #>
    param([AllowNull()][AllowEmptyString()][string]$Text)

    $values = @{}
    $section = ''
    $pendingKey = $null
    $pendingValue = ''

    $lines = @()
    if ($null -ne $Text) {
        $lines = $Text.TrimStart([char]0xFEFF) -split "\r?\n"
    }
    foreach ($rawLine in $lines) {
        $line = (Remove-GanderTomlComment -Line $rawLine).Trim()
        if ($null -ne $pendingKey) {
            $pendingValue = $pendingValue + ' ' + $line
            if ((Get-GanderTomlBracketDepth -Text $pendingValue) -le 0) {
                $values[$pendingKey] = $pendingValue
                $pendingKey = $null
                $pendingValue = ''
            }
            continue
        }
        if ($line.Length -eq 0) {
            continue
        }
        if ($line -match '^\[\[\s*(.+?)\s*\]\]$') {
            $section = '[[' + $Matches[1] + ']]'
            continue
        }
        if ($line -match '^\[\s*([^\[\]]+?)\s*\]$') {
            $section = $Matches[1]
            continue
        }
        if ($line -match '^([A-Za-z0-9_.\-]+)\s*=\s*(.*)$') {
            $key = $Matches[1]
            $value = $Matches[2]
            if ($section.Length -gt 0) {
                $key = $section + '.' + $key
            }
            if ($value.StartsWith('[') -and ((Get-GanderTomlBracketDepth -Text $value) -gt 0)) {
                $pendingKey = $key
                $pendingValue = $value
            }
            else {
                $values[$key] = $value
            }
        }
    }
    if ($null -ne $pendingKey) {
        throw ('config.toml: the array "{0}" is not closed' -f $pendingKey)
    }

    $settings = New-Object psobject -Property @{
        TcpPort            = $script:GanderDefaultTcpPort
        UdpPorts           = $script:GanderDefaultUdpPorts
        AllowPublicProfile = $false
        GenericCidrs       = [string[]]@()
    }

    if ($values.ContainsKey('listener.port')) {
        $raw = $values['listener.port'].Trim()
        if (-not ($raw -match '^\d{1,5}$') -or [int]$raw -lt 1 -or [int]$raw -gt 65535) {
            throw ('config.toml: listener.port "{0}" is not a port number' -f $raw)
        }
        $settings.TcpPort = [int]$raw
    }
    if ($values.ContainsKey('firewall.udp_ports')) {
        $raw = $values['firewall.udp_ports'].Trim()
        $quoted = $null
        if ($raw -match '^"([^"]*)"$') {
            $quoted = $Matches[1]
        }
        elseif ($raw -match "^'([^']*)'$") {
            $quoted = $Matches[1]
        }
        if ($null -eq $quoted) {
            throw ('config.toml: firewall.udp_ports must be a string such as "50000-50099", not {0}' -f $raw)
        }
        $settings.UdpPorts = $quoted.Trim()
    }
    if ($values.ContainsKey('firewall.allow_public_profile')) {
        $raw = $values['firewall.allow_public_profile'].Trim()
        if ($raw -ceq 'true') {
            $settings.AllowPublicProfile = $true
        }
        elseif ($raw -ceq 'false') {
            $settings.AllowPublicProfile = $false
        }
        else {
            throw ('config.toml: firewall.allow_public_profile must be true or false, not {0}' -f $raw)
        }
    }
    if ($values.ContainsKey('listener.generic.cidrs')) {
        $raw = $values['listener.generic.cidrs'].Trim()
        if (-not ($raw.StartsWith('[') -and $raw.EndsWith(']'))) {
            throw 'config.toml: listener.generic.cidrs must be an array of strings'
        }
        $found = New-Object System.Collections.Generic.List[string]
        $stripped = $raw
        foreach ($m in [regex]::Matches($raw, '"([^"]*)"|''([^'']*)''')) {
            if ($m.Groups[1].Success) {
                $found.Add($m.Groups[1].Value)
            }
            else {
                $found.Add($m.Groups[2].Value)
            }
            $stripped = $stripped.Replace($m.Value, '')
        }
        if ($stripped -notmatch '^\[[\s,]*\]$') {
            throw 'config.toml: listener.generic.cidrs holds something other than quoted strings'
        }
        $settings.GenericCidrs = [string[]]$found.ToArray()
    }
    return $settings
}

function Get-GanderConfigPath {
    # The config.toml of the current user, unless a path is given.
    param([string]$ConfigPath)

    if (-not [string]::IsNullOrEmpty($ConfigPath)) {
        return $ConfigPath
    }
    $appData = $env:APPDATA
    if ([string]::IsNullOrEmpty($appData)) {
        throw 'APPDATA is not set; pass -ConfigPath'
    }
    return (Join-Path (Join-Path $appData 'Gander') 'config.toml')
}

function Read-GanderConfig {
    # Reads config.toml; a missing file is the default configuration (no generic CIDRs).
    param([string]$ConfigPath)

    $path = Get-GanderConfigPath -ConfigPath $ConfigPath
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        return (Read-GanderConfigText -Text '')
    }
    $text = [System.IO.File]::ReadAllText($path, [System.Text.Encoding]::UTF8)
    return (Read-GanderConfigText -Text $text)
}

# --------------------------------------------------------------------------------------------
# PURE: the rule plan
# --------------------------------------------------------------------------------------------

function Get-GanderFirewallPlan {
    <#
    .SYNOPSIS
        The inbound allow rules the installer creates, as hashtables ready to splat into
        New-NetFirewallRule: TCP on the listener port and UDP on the WebRTC range, both limited
        to the executable, the tailnet CIDRs plus the configured generic CIDRs, and the Private
        and Domain profiles (Public only when configured or -IncludePublic).
    .DESCRIPTION
        Throws if any address or port fails validation, so a bad configuration produces no rules.
    #>
    param(
        [Parameter(Mandatory = $true)][string]$ProgramPath,
        [Parameter(Mandatory = $true)]$Settings,
        [switch]$IncludePublic
    )

    if (-not [System.IO.Path]::IsPathRooted($ProgramPath)) {
        throw ('the program path must be absolute: {0}' -f $ProgramPath)
    }
    $remote = New-Object System.Collections.Generic.List[string]
    foreach ($cidr in $script:GanderTailnetCidrs) {
        $remote.Add($cidr)
    }
    foreach ($cidr in @($Settings.GenericCidrs)) {
        $trimmed = ([string]$cidr).Trim()
        if (-not $remote.Contains($trimmed)) {
            $remote.Add($trimmed)
        }
    }
    Assert-GanderRemoteAddress -Address ([string[]]$remote.ToArray())

    $tcpPort = [int]$Settings.TcpPort
    if ($tcpPort -lt 1 -or $tcpPort -gt 65535) {
        throw ('the TCP port {0} is not a port number' -f $tcpPort)
    }
    $problem = Test-GanderPortRange -Range ([string]$Settings.UdpPorts)
    if ($null -ne $problem) {
        throw ('firewall.udp_ports: {0}' -f $problem)
    }

    $profiles = @('Private', 'Domain')
    if ($IncludePublic -or [bool]$Settings.AllowPublicProfile) {
        $profiles = @('Private', 'Domain', 'Public')
    }

    $description = 'Created by Gander network setup. Limited to private-network source addresses; remove with firewall-rules.ps1 -Action Remove.'
    $plan = New-Object System.Collections.Generic.List[object]
    $plan.Add(@{
            Name          = 'Gander-In-TCP'
            DisplayName   = ('Gander host (TCP {0}, private network only)' -f $tcpPort)
            Description   = $description
            Group         = $script:GanderRuleGroup
            Direction     = 'Inbound'
            Action        = 'Allow'
            Protocol      = 'TCP'
            LocalPort     = [string]$tcpPort
            RemoteAddress = [string[]]$remote.ToArray()
            Program       = $ProgramPath
            Profile       = [string[]]$profiles
        })
    $plan.Add(@{
            Name          = 'Gander-In-UDP'
            DisplayName   = ('Gander host (UDP {0}, WebRTC, private network only)' -f ([string]$Settings.UdpPorts).Trim())
            Description   = $description
            Group         = $script:GanderRuleGroup
            Direction     = 'Inbound'
            Action        = 'Allow'
            Protocol      = 'UDP'
            LocalPort     = ([string]$Settings.UdpPorts).Trim()
            RemoteAddress = [string[]]$remote.ToArray()
            Program       = $ProgramPath
            Profile       = [string[]]$profiles
        })
    # The unary comma keeps a one-element result a list.
    return , $plan.ToArray()
}

# --------------------------------------------------------------------------------------------
# READERS: read-only firewall queries (never used by -SelfTest)
# --------------------------------------------------------------------------------------------

function Get-GanderInstalledRule {
    <#
    .SYNOPSIS
        The rules in the Gander group as they are in Windows Firewall now, flattened: Name,
        Enabled, Direction, Action, Profile, Protocol, LocalPort, Program, RemoteAddress.
        Read-only.
    #>
    $rules = @(Get-NetFirewallRule -Group $script:GanderRuleGroup -ErrorAction SilentlyContinue)
    $out = New-Object System.Collections.Generic.List[object]
    foreach ($rule in $rules) {
        $address = Get-NetFirewallAddressFilter -AssociatedNetFirewallRule $rule
        $port = Get-NetFirewallPortFilter -AssociatedNetFirewallRule $rule
        $app = Get-NetFirewallApplicationFilter -AssociatedNetFirewallRule $rule
        $out.Add((New-Object psobject -Property @{
                    Name          = [string]$rule.Name
                    Enabled       = [string]$rule.Enabled
                    Direction     = [string]$rule.Direction
                    Action        = [string]$rule.Action
                    Profile       = [string]$rule.Profile
                    Protocol      = [string]$port.Protocol
                    LocalPort     = (@($port.LocalPort) -join ',')
                    Program       = [string]$app.Program
                    RemoteAddress = [string[]]@($address.RemoteAddress)
                }))
    }
    return , $out.ToArray()
}
