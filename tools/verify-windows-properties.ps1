param([Parameter(Mandatory)][string]$InstallDirectory, [Parameter(Mandatory)][string]$EvidenceDirectory,
    [Parameter(Mandatory)][string]$ExpectedCopyright)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows -or -not [Environment]::Is64BitProcess -or $env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'Properties verification requires an owned disposable Windows x64 Actions runner.'
}
$expected = Join-Path $env:ProgramFiles 'Soul2D'
if (-not [String]::Equals([IO.Path]::GetFullPath($InstallDirectory), $expected, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Properties verification accepts only the installed Soul2D directory.'
}
if ([string]::IsNullOrWhiteSpace($ExpectedCopyright) -or $ExpectedCopyright.Contains('"')) { throw 'Invalid expected copyright argument.' }
if (Test-Path -LiteralPath $EvidenceDirectory) { throw 'Properties evidence directory must be new.' }
New-Item -ItemType Directory $EvidenceDirectory | Out-Null
$files = [Collections.Generic.List[object]]::new()
$result = [ordered]@{ status = 'FAIL'; scope = 'Three installed file Properties checks; rendered glyph requires capture review';
    installDirectory = $expected; expectedCopyright = $ExpectedCopyright; files = $files; failure = $null }
try {
    foreach ($name in @('soul2d.exe', 'soul2d-core.exe', 'njc.exe')) {
        $path = Join-Path $InstallDirectory $name
        $evidence = Join-Path $EvidenceDirectory $name
        New-Item -ItemType Directory $evidence | Out-Null
        $before = (Get-FileHash -LiteralPath $path).Hash
        $file = [ordered]@{ name = $name; status = 'FAIL'; file = $path; sha256Before = $before; sha256After = $null;
            helperPid = $null; helperExited = $false; helperExitCode = $null; forcedTermination = $false; cleanupErrors = @(); observation = $null; failure = $null }
        $files.Add($file)
        $helper = $null
        try {
            $shell = Join-Path $env:WINDIR 'System32\WindowsPowerShell\v1.0\powershell.exe'
            $script = Join-Path $PSScriptRoot 'observe-windows-properties.ps1'
            $helper = Start-Process -FilePath $shell -PassThru -NoNewWindow -ArgumentList @('-NoProfile', '-NonInteractive', '-STA',
                '-File', "`"$script`"", '-FilePath', "`"$path`"", '-EvidenceDirectory', "`"$evidence`"",
                '-ExpectedCopyright', "`"$ExpectedCopyright`"") -RedirectStandardOutput (Join-Path $evidence 'stdout.log') `
                -RedirectStandardError (Join-Path $evidence 'stderr.log')
            $file.helperPid = $helper.Id
            if (-not $helper.WaitForExit(20000)) { throw 'Properties helper exceeded its outer deadline.' }
            $file.helperExited = $true
            $file.helperExitCode = $helper.get_ExitCode()
            if ($file.helperExitCode -ne 0) { throw 'Properties helper failed.' }
            $file.observation = Get-Content -Raw -LiteralPath (Join-Path $evidence 'observation.json') | ConvertFrom-Json
            if ($file.observation.status -ne 'PASS' -or $file.observation.dialogClosed -ne $true `
                -or $file.observation.detailsSelected -ne $true -or $file.observation.copyrightRow.value -cne $ExpectedCopyright `
                -or $file.observation.capturedCopyrightRow.value -cne $ExpectedCopyright `
                -or $file.observation.pid -ne $file.helperPid -or $file.observation.path -cne $path `
                -or $file.observation.capture.bmp -cne 'properties-details.bmp' -or $file.observation.capture.png -cne 'properties-details.png') {
                throw 'Incomplete Properties observation or cleanup.'
            }
            foreach ($format in @('bmp', 'png')) {
                $capture = $file.observation.capture
                $hashField = $format + 'Sha256'
                if ((Get-FileHash -LiteralPath (Join-Path $evidence $capture.$format)).Hash -cne $capture.$hashField) {
                    throw 'Properties capture hash differs.'
                }
            }
            $file.status = 'PASS'
        } catch { $file.failure = $_.Exception.Message }
        finally {
            if ($null -ne $helper) {
                try {
                    if (-not $helper.HasExited) {
                        $file.forcedTermination = $true
                        try { $helper.Kill() } catch { if (-not $helper.HasExited) { throw } }
                        if (-not $helper.WaitForExit(5000)) { throw 'Properties helper termination was not confirmed.' }
                    }
                    $file.helperExited = $helper.HasExited
                } catch { $file.cleanupErrors += $_.Exception.Message }
                finally {
                    try { $helper.Dispose() } catch { $file.cleanupErrors += $_.Exception.Message }
                }
            }
            try {
                $file.sha256After = (Get-FileHash -LiteralPath $path).Hash
                if ($file.sha256After -cne $before) { throw 'Installed executable changed.' }
            } catch { $file.cleanupErrors += $_.Exception.Message }
            if ($file.cleanupErrors.Count -ne 0 -or -not $file.helperExited -or $file.forcedTermination) { $file.status = 'FAIL' }
        }
        if ($file.status -ne 'PASS') { throw "Properties verification failed for $name; see owned evidence." }
    }
    $result.status = 'PASS'
} catch { $result.failure = $_.Exception.Message }
finally {
    $result | ConvertTo-Json -Depth 25 | Set-Content -LiteralPath (Join-Path $EvidenceDirectory 'result.json') -Encoding utf8NoBOM
}
if ($result.status -ne 'PASS') { throw $result.failure }
