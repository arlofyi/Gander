# Gander installer: parse PowerShell scripts without running them (used by `cargo xtask installer-check`).
#
#   powershell.exe -NoProfile -File parse-check.ps1 a.ps1 b.ps1     (Windows PowerShell 5.1)
#   pwsh -NoProfile -File parse-check.ps1 a.ps1 b.ps1               (PowerShell 7)
#
# Uses the language parser of whichever shell runs it, so the 5.1 run rejects 7-only syntax (ternary,
# ??, ?., && and ||). Prints the shell version, then OK or one FAIL line per parse error per file.
# Exit 0 when every file parses, 1 otherwise.

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0, ValueFromRemainingArguments = $true)]
    [string[]]$Path
)

Set-StrictMode -Version 2.0

Write-Output ('shell: PowerShell {0} ({1})' -f $PSVersionTable.PSVersion, $PSVersionTable.PSEdition)
$failed = 0
# `-File` passes "a,b" as one string, so split on commas as well as on spaces.
foreach ($item in @($Path | ForEach-Object { $_ -split ',' } | Where-Object { $_.Length -gt 0 })) {
    if (-not (Test-Path -LiteralPath $item -PathType Leaf)) {
        Write-Output ('FAIL {0}: file not found' -f $item)
        $failed++
        continue
    }
    $full = (Resolve-Path -LiteralPath $item).ProviderPath
    $tokens = $null
    $errors = $null
    $null = [System.Management.Automation.Language.Parser]::ParseFile($full, [ref]$tokens, [ref]$errors)
    if ($null -ne $errors -and @($errors).Count -gt 0) {
        foreach ($e in @($errors)) {
            Write-Output ('FAIL {0}:{1}:{2}: {3}' -f $item, $e.Extent.StartLineNumber, $e.Extent.StartColumnNumber, $e.Message)
        }
        $failed++
    }
    else {
        Write-Output ('OK   {0}' -f $item)
    }
}
if ($failed -gt 0) {
    exit 1
}
exit 0
