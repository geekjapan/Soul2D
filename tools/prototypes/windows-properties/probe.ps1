param([Parameter(Mandatory)][string]$FilePath, [Parameter(Mandatory)][string]$EvidenceDirectory,
    [Parameter(Mandatory)][string]$CaptureSource)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows -or $env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'Properties discovery requires an owned disposable Windows Actions runner.'
}
$expected = Join-Path $env:ProgramFiles 'Soul2D\soul2d.exe'
if (-not [String]::Equals([IO.Path]::GetFullPath($FilePath), $expected, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Properties prototype accepts only the installed Soul2D editor.'
}
if (Test-Path -LiteralPath $EvidenceDirectory) { throw 'Properties evidence directory must be new.' }
New-Item -ItemType Directory $EvidenceDirectory | Out-Null
$before = (Get-FileHash -LiteralPath $FilePath).Hash
$result = [ordered]@{ status = 'FAIL'; scope = 'Properties discovery only; Issue acceptance NOT_EVALUATED';
    file = $FilePath; sha256Before = $before; helperExited = $false; failure = $null; cleanupErrors = @() }
$helper = $null
try {
    $shell = Join-Path $env:WINDIR 'System32\WindowsPowerShell\v1.0\powershell.exe'
    $script = Join-Path $PSScriptRoot 'observe.ps1'
    $helper = Start-Process -FilePath $shell -PassThru -NoNewWindow -ArgumentList @('-NoProfile', '-NonInteractive', '-STA',
        '-File', "`"$script`"", '-FilePath', "`"$FilePath`"", '-EvidenceDirectory', "`"$EvidenceDirectory`"",
        '-CaptureSource', "`"$CaptureSource`"") -RedirectStandardOutput (Join-Path $EvidenceDirectory 'stdout.log') `
        -RedirectStandardError (Join-Path $EvidenceDirectory 'stderr.log')
    if (-not $helper.WaitForExit(20000)) { throw 'Properties helper exceeded its outer deadline.' }
    $result.helperExited = $true
    $result.helperExitCode = $helper.get_ExitCode()
    if ($result.helperExitCode -ne 0) { throw 'Properties helper failed.' }
    $discovery = Get-Content -Raw -LiteralPath (Join-Path $EvidenceDirectory 'discovery.json') | ConvertFrom-Json
    if ($discovery.status -ne 'OBSERVED' -or $discovery.dialogClosed -ne $true -or $discovery.detailsSelected -ne $true) {
        throw 'Incomplete Properties discovery or cleanup.'
    }
    $result.status = 'OBSERVED'
} catch { $result.failure = $_.Exception.Message }
finally {
    if ($null -ne $helper) {
        try {
            if (-not $helper.HasExited) {
                try { $helper.Kill() } catch { if (-not $helper.HasExited) { throw } }
                if (-not $helper.WaitForExit(5000)) { throw 'Properties helper termination was not confirmed.' }
            }
            $result.helperExited = $helper.HasExited
        } catch { $result.cleanupErrors += $_.Exception.Message }
        finally {
            try { $helper.Dispose() } catch { $result.cleanupErrors += $_.Exception.Message }
        }
    }
    try {
        $result.sha256After = (Get-FileHash -LiteralPath $FilePath).Hash
        if ($result.sha256After -cne $before) { throw 'Installed executable changed.' }
    } catch { $result.cleanupErrors += $_.Exception.Message }
    if ($result.cleanupErrors.Count -ne 0 -or -not $result.helperExited) {
        $result.status = 'FAIL'
    }
    $result | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $EvidenceDirectory 'result.json') -Encoding utf8NoBOM
}
if ($result.status -ne 'OBSERVED') { throw 'Properties discovery failed; see owned evidence.' }
