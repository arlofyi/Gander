# Summarises the READ lines of a WP-47 spike log, one row per proxy file descriptor.
#
# Input: a log saved by Save-SpikeLog (`HH:mm:ss.fff tid=N EVENT k=v ...`) or raw `adb logcat -v threadtime`.
# Output: markdown on stdout (and to -OutPath when given).
#
#   pwsh -NoProfile -File Analyze-ReadLog.ps1 -LogPath results/logs/08-player-sample-faststart.txt [-OutPath x.md]
#
# Per fd: reads, bytes requested vs unique bytes (interval union), re-read share, sequential / forward /
# backward counts against the previous read's end, size histogram, forward gap and big-jump list,
# callback latency percentiles, threads.

[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$LogPath,
    [string]$OutPath,
    [string]$Title,
    [long]$BigJumpBytes = 1048576
)

Set-StrictMode -Version Latest

function ConvertTo-KeyValue {
    param([string]$Text)
    $h = @{}
    foreach ($m in [regex]::Matches($Text, '(\w+)=(\S+)')) { $h[$m.Groups[1].Value] = $m.Groups[2].Value }
    return $h
}

function Get-Percentile {
    param([double[]]$Values, [double]$P)
    if ($Values.Count -eq 0) { return 0 }
    $sorted = $Values | Sort-Object
    $i = [math]::Min($sorted.Count - 1, [math]::Max(0, [int][math]::Ceiling($P / 100.0 * $sorted.Count) - 1))
    return $sorted[$i]
}

function Format-Bytes {
    param([double]$N)
    if ($N -ge 1MB) { return ('{0:N2} MiB' -f ($N / 1MB)) }
    if ($N -ge 1KB) { return ('{0:N1} KiB' -f ($N / 1KB)) }
    return "$([long]$N) B"
}

$fds = [ordered]@{}
$timeline = New-Object System.Collections.Generic.List[object]
foreach ($line in Get-Content -Path $LogPath) {
    $m = [regex]::Match($line, '(?:GanderSpike:\s+|^\d\d:\d\d:\d\d\.\d+\s+tid=\d+\s+)([A-Z_]+)\s*(.*)$')
    if (-not $m.Success) { continue }
    $kv = ConvertTo-KeyValue -Text $m.Groups[2].Value
    $ev = $m.Groups[1].Value
    if ($ev -eq 'READ' -or $ev -like 'PLAYER_*') {
        $tm = [regex]::Match($line, '^(\d\d):(\d\d):(\d\d)\.(\d+)')
        $t = if ($tm.Success) { (([int]$tm.Groups[1].Value * 3600 + [int]$tm.Groups[2].Value * 60 + [int]$tm.Groups[3].Value) * 1000) + [int]$tm.Groups[4].Value } else { -1 }
        $timeline.Add([pscustomobject]@{ Ev = $ev; T = $t; Kv = $kv })
    }
    if (@('OPEN', 'READ', 'RELEASE', 'GETSIZE') -notcontains $ev) { continue }
    if (-not $kv.ContainsKey('fd')) { continue }
    $key = $kv['fd']
    if (-not $fds.Contains($key)) {
        $fds[$key] = [pscustomobject]@{
            Fd = [int]$key; Opener = '?'; Doc = '?'; FileSize = 0L
            Reads = New-Object System.Collections.Generic.List[object]; Release = $null
        }
    }
    $f = $fds[$key]
    switch ($ev) {
        'OPEN' { $f.Opener = $kv['pkg']; $f.Doc = $kv['doc'] }
        'GETSIZE' { $f.FileSize = [long]$kv['size'] }
        'READ' {
            $f.Reads.Add([pscustomobject]@{
                    Off = [long]$kv['off']; Size = [int]$kv['size']; Ret = [int]$kv['ret']
                    CbMs = [double]$kv['cb_ms']; Thr = $kv['thr']
                })
        }
        'RELEASE' { $f.Release = $kv }
    }
}

$out = New-Object System.Collections.Generic.List[string]
if ($Title) { $out.Add("### $Title") ; $out.Add('') }
$out.Add("Log: ``$(Split-Path -Leaf $LogPath)``")
$out.Add('')
$out.Add('| fd | client | doc | reads | requested | unique | re-read | seq | fwd | back | sizes min/med/max | cb ms med/p95/max | HTTP reqs |')
$out.Add('| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: |')

$detail = New-Object System.Collections.Generic.List[string]
foreach ($f in $fds.Values) {
    $reads = $f.Reads.ToArray()
    if ($reads.Count -eq 0) {
        $out.Add("| $($f.Fd) | $($f.Opener) | $($f.Doc) | 0 | 0 | 0 | - | - | - | - | - | - | - |")
        continue
    }
    $requested = ($reads | Measure-Object -Property Ret -Sum).Sum
    # Unique bytes: union of [off, off+ret) intervals.
    $iv = $reads | Where-Object { $_.Ret -gt 0 } | Sort-Object Off | ForEach-Object { , @($_.Off, ($_.Off + $_.Ret)) }
    $unique = 0L; $curS = -1L; $curE = -1L
    foreach ($p in $iv) {
        if ($curS -lt 0) { $curS = $p[0]; $curE = $p[1]; continue }
        if ($p[0] -le $curE) { if ($p[1] -gt $curE) { $curE = $p[1] } }
        else { $unique += ($curE - $curS); $curS = $p[0]; $curE = $p[1] }
    }
    if ($curS -ge 0) { $unique += ($curE - $curS) }

    $seq = 0; $fwd = 0; $back = 0
    $gaps = New-Object System.Collections.Generic.List[double]
    $jumps = New-Object System.Collections.Generic.List[string]
    $prevEnd = -1L
    $i = 0
    foreach ($r in $reads) {
        if ($prevEnd -lt 0 -or $r.Off -eq $prevEnd) { $seq++ }
        elseif ($r.Off -gt $prevEnd) {
            $fwd++; $gaps.Add([double]($r.Off - $prevEnd))
            if (($r.Off - $prevEnd) -ge $BigJumpBytes) { $jumps.Add("read #$i forward $(Format-Bytes ($r.Off - $prevEnd)): $prevEnd -> $($r.Off)") }
        } else {
            $back++
            if (($prevEnd - $r.Off) -ge $BigJumpBytes) { $jumps.Add("read #$i backward $(Format-Bytes ($prevEnd - $r.Off)): $prevEnd -> $($r.Off)") }
        }
        $prevEnd = $r.Off + $r.Ret
        $i++
    }
    $sizes = [double[]]($reads | ForEach-Object { $_.Size })
    $cb = [double[]]($reads | ForEach-Object { $_.CbMs })
    $reread = if ($requested -gt 0) { '{0:P0}' -f (($requested - $unique) / $requested) } else { '-' }
    $httpReqs = if ($f.Release -and $f.Release.ContainsKey('http_requests')) { $f.Release['http_requests'] } else { '?' }
    $sz = '{0}/{1}/{2}' -f (Format-Bytes (($sizes | Measure-Object -Minimum).Minimum)), (Format-Bytes (Get-Percentile $sizes 50)), (Format-Bytes (($sizes | Measure-Object -Maximum).Maximum))
    $lat = '{0:N1}/{1:N1}/{2:N1}' -f (Get-Percentile $cb 50), (Get-Percentile $cb 95), (($cb | Measure-Object -Maximum).Maximum)
    $out.Add("| $($f.Fd) | $($f.Opener) | $($f.Doc) | $($reads.Count) | $(Format-Bytes $requested) | $(Format-Bytes $unique) | $reread | $seq | $fwd | $back | $sz | $lat | $httpReqs |")

    $hist = $reads | Group-Object Size | Sort-Object Count -Descending | Select-Object -First 8 | ForEach-Object { "$($_.Name) x$($_.Count)" }
    $d = New-Object System.Collections.Generic.List[string]
    $d.Add("**fd $($f.Fd)** ($($f.Opener), $($f.Doc), file size $($f.FileSize))")
    $d.Add("- requested sizes (top 8 by count): $($hist -join ', ')")
    $d.Add("- offsets: first read at $($reads[0].Off), last read ends at $($prevEnd), unique coverage $('{0:P1}' -f $(if ($f.FileSize -gt 0) { $unique / $f.FileSize } else { 0 }))")
    if ($gaps.Count -gt 0) {
        $d.Add("- forward gaps (bytes skipped): median $(Format-Bytes (Get-Percentile $gaps.ToArray() 50)), p90 $(Format-Bytes (Get-Percentile $gaps.ToArray() 90)), max $(Format-Bytes (($gaps | Measure-Object -Maximum).Maximum))")
    }
    if ($jumps.Count -gt 0) {
        $d.Add("- jumps of at least $(Format-Bytes $BigJumpBytes) ($($jumps.Count)):")
        foreach ($j in ($jumps | Select-Object -First 12)) { $d.Add("  - $j") }
    }
    $thr = ($reads | ForEach-Object { $_.Thr } | Sort-Object -Unique) -join ', '
    $d.Add("- callback threads: $thr")
    if ($f.Release) {
        $d.Add("- release: http_bytes=$($f.Release['http_bytes']) http_requests=$($f.Release['http_requests']) max_in_flight=$($f.Release['max_in_flight']) life_ms=$($f.Release['life_ms'])")
    }
    $detail.AddRange($d)
    $detail.Add('')
}
$out.Add('')
$out.AddRange($detail)

# Seek behaviour: what the provider is asked for after each PLAYER_SEEK_TO.
$seekIdx = @(0..($timeline.Count - 1) | Where-Object { $timeline.Count -gt 0 -and $timeline[$_].Ev -eq 'PLAYER_SEEK_TO' })
if ($seekIdx.Count -gt 0) {
    $out.Add('**Seek behaviour** (reads the provider saw between each seek request and the next seek or the end)')
    foreach ($si in $seekIdx) {
        $seek = $timeline[$si]
        $done = $null; $rd = New-Object System.Collections.Generic.List[object]; $doneReads = 0
        $prevEnd = -1L
        for ($k = $si - 1; $k -ge 0; $k--) {
            if ($timeline[$k].Ev -eq 'READ') { $prevEnd = [long]$timeline[$k].Kv['off'] + [long]$timeline[$k].Kv['ret']; break }
        }
        $landing = 'no jump of 1 MiB or more'
        for ($j = $si + 1; $j -lt $timeline.Count; $j++) {
            $e = $timeline[$j]
            if ($e.Ev -eq 'PLAYER_SEEK_TO' -or $e.Ev -eq 'PLAYER_FINISH') { break }
            if ($e.Ev -eq 'PLAYER_SEEK_DONE' -and -not $done) { $done = $e; $doneReads = $rd.Count }
            if ($e.Ev -eq 'READ') {
                $off = [long]$e.Kv['off']
                if ($prevEnd -ge 0 -and [math]::Abs($off - $prevEnd) -ge $BigJumpBytes -and $landing.StartsWith('no jump')) {
                    $landing = "first jump of $(Format-Bytes ([math]::Abs($off - $prevEnd))) ($(if ($off -gt $prevEnd) { 'forward' } else { 'backward' })) lands at offset $off after $($rd.Count) reads"
                }
                $prevEnd = $off + [long]$e.Kv['ret']
                $rd.Add($e)
            }
        }
        $firstOffs = ($rd | Select-Object -First 4 | ForEach-Object { "$($_.Kv['off'])/$($_.Kv['size'])" }) -join ', '
        $bytes = ($rd | ForEach-Object { [long]$_.Kv['ret'] } | Measure-Object -Sum).Sum
        $lat = if ($done -and $done.T -ge 0 -and $seek.T -ge 0) { "$($done.T - $seek.T) ms" } else { '?' }
        $out.Add("- seek to $($seek.Kv['pos_ms']) ms (from $($seek.Kv['from_ms']) ms): done after $lat with $doneReads reads; $landing; first reads (offset/size) $firstOffs; $($rd.Count) reads and $(Format-Bytes $(if ($bytes) { $bytes } else { 0 })) until the next seek")
    }
    $out.Add('')
}

$text = $out -join "`n"
if ($OutPath) { Set-Content -Path $OutPath -Value $text -Encoding utf8 }
Write-Output $text
