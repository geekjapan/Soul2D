$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$tokens = $null
$parseErrors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $PSScriptRoot 'verify-windows-install.ps1'), [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count -ne 0) { throw 'Windows verifier does not parse.' }
$function = $ast.Find({ param($node)
    $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Get-VerificationExitCode'
}, $false)
if ($null -eq $function) { throw 'Missing verification outcome function.' }
. ([scriptblock]::Create($function.Extent.Text))

$known = @{
    graphicsContext = 'system'; editorStdio = 'NOT_RUN'; graphicsFixture = $null
    installation = 'PASS'; shortcut = 'FAIL'; association = 'NOT_RUN'
    failure = 'start-menu did not produce an installed editor window and owned MCP listener before the deadline.'
    cleanup = @{ uninstallExitCode = 0; errors = @(); productAbsent = $true; installDirectoryAbsent = $true;
        shortcutAbsent = $true; registrationsAbsent = $true; associationAbsent = $true;
        fixtureRemoved = $true; inventoryRestored = $true }
    launches = @(@{ kind = 'start-menu'; accepted = $false; directDiagnostic = @{
        exited = $true; exitCode = 1; hwnd = 0
        stderr = 'Error: OpenGL(PainterError("egui_glow requires opengl 2.0+. "))'
    } })
}
$copyright = "Copyright $([char]0x00a9) 2021 nijigenerate Project"
$known.properties = @{ status = 'PASS'; installDirectory = 'C:\Program Files\Soul2D'; expectedCopyright = $copyright; files = @(
    @('soul2d.exe', 'soul2d-core.exe', 'njc.exe') | ForEach-Object {
        @{ name = $_; file = ('C:\Program Files\Soul2D\' + $_); status = 'PASS'; helperPid = 17; helperExited = $true; helperExitCode = 0; forcedTermination = $false; cleanupErrors = @();
            sha256Before = ('a' * 64); sha256After = ('a' * 64); observation = @{ status = 'PASS'; path = ('C:\Program Files\Soul2D\' + $_); pid = 17; dialogClosed = $true; detailsSelected = $true;
                generalTarget = @{ expectedFilename = $_; expectedDirectory = 'C:\Program Files\Soul2D' }; resource = @{ translations = @('0409/1252'); copyright = $copyright };
                copyrightRow = @{ value = $copyright }; capturedCopyrightRow = @{ value = $copyright };
                capture = @{ bmp = 'properties-details.bmp'; png = 'properties-details.png'; bmpSha256 = ('b' * 64); pngSha256 = ('c' * 64) } } }
    }
) }
function Set-PassingLaunches($Receipt) {
    $Receipt.failure = $null; $Receipt.shortcut = 'PASS'; $Receipt.association = 'PASS'; $Receipt.editorStdio = 'PASS'
    $Receipt.launches = @('start-menu', 'association') | ForEach-Object {
        @{ kind = $_; accepted = $true; pid = 42; exitCode = 0; paint = @{ status = 'PASS'; restored = $true };
            console = @{ attached = $true; detached = $true; processIds = @(42, 43) }; softwareModule = @{ verified = $true } }
    }
}
function Set-SoftwareLaunches($Receipt) {
    Set-PassingLaunches $Receipt
    $Receipt.graphicsContext = 'test-software-wgl'
    $Receipt.graphicsFixture = @{ verified = $true }
}
$cases = @(
    @{ name = 'known hosted OpenGL limitation'; expected = 2; change = {} }
    @{ name = 'unfamiliar renderer error'; expected = 1; change = { param($r) $r.launches[0].directDiagnostic.stderr = 'different error' } }
    @{ name = 'direct launch timeout'; expected = 1; change = { param($r) $r.launches[0].directDiagnostic.exited = $false } }
    @{ name = 'missing diagnostic'; expected = 1; change = { param($r) $r.launches[0].directDiagnostic = $null } }
    @{ name = 'failed installation'; expected = 1; change = { param($r) $r.installation = 'FAIL' } }
    @{ name = 'cleanup exception'; expected = 1; change = { param($r) $r.cleanup.errors = @('failed cleanup') } }
    @{ name = 'remaining registration'; expected = 1; change = { param($r) $r.cleanup.registrationsAbsent = $false } }
    @{ name = 'uninstall requires reboot'; expected = 1; change = { param($r) $r.cleanup.uninstallExitCode = 3010 } }
    @{ name = 'association was attempted'; expected = 1; change = { param($r) $r.association = 'FAIL' } }
    @{ name = 'native flags without observations'; expected = 1; change = { param($r) $r.failure = $null; $r.shortcut = 'PASS'; $r.association = 'PASS' } }
    @{ name = 'both native paths passed'; expected = 0; change = { param($r)
        Set-PassingLaunches $r
    } }
    @{ name = 'missing process exit code'; expected = 1; change = { param($r) Set-PassingLaunches $r; $r.launches[0].exitCode = $null } }
    @{ name = 'unsuccessful process exit'; expected = 1; change = { param($r) Set-PassingLaunches $r; $r.launches[1].exitCode = 7 } }
    @{ name = 'unknown graphics context'; expected = 1; change = { param($r) Set-PassingLaunches $r; $r.graphicsContext = 'unknown' } }
    @{ name = 'missing editor stdio'; expected = 1; change = { param($r) Set-PassingLaunches $r; $r.editorStdio = 'NOT_RUN' } }
    @{ name = 'unproven paint'; expected = 1; change = { param($r) Set-PassingLaunches $r; $r.launches[1].paint.status = 'FAIL' } }
    @{ name = 'view restoration failed'; expected = 1; change = { param($r) Set-PassingLaunches $r; $r.launches[0].paint.restored = $false } }
    @{ name = 'console attach failed'; expected = 1; change = { param($r) Set-PassingLaunches $r; $r.launches[0].console.attached = $false } }
    @{ name = 'wrong console membership'; expected = 1; change = { param($r) Set-PassingLaunches $r; $r.launches[1].console.processIds = @(99) } }
    @{ name = 'both software paths passed'; expected = 0; change = { param($r) Set-SoftwareLaunches $r } }
    @{ name = 'software failure is not unsupported'; expected = 1; change = { param($r) $r.graphicsContext = 'test-software-wgl' } }
    @{ name = 'unverified archive'; expected = 1; change = { param($r) Set-SoftwareLaunches $r; $r.graphicsFixture.verified = $false } }
    @{ name = 'unverified loaded DLL'; expected = 1; change = { param($r) Set-SoftwareLaunches $r; $r.launches[1].softwareModule.verified = $false } }
    @{ name = 'fixture remains'; expected = 1; change = { param($r) Set-SoftwareLaunches $r; $r.cleanup.fixtureRemoved = $false } }
    @{ name = 'installed inventory changed'; expected = 1; change = { param($r) Set-SoftwareLaunches $r; $r.cleanup.inventoryRestored = $false } }
    @{ name = 'missing Properties'; expected = 1; change = { param($r) [void]$r.Remove('properties') } }
    @{ name = 'prototype Properties is not acceptance'; expected = 1; change = { param($r) $r.properties.status = 'OBSERVED' } }
    @{ name = 'missing Properties file'; expected = 1; change = { param($r) $r.properties.files = $r.properties.files[0..1] } }
    @{ name = 'duplicate Properties file'; expected = 1; change = { param($r) $r.properties.files[1].name = 'soul2d.exe' } }
    @{ name = 'wrong Properties file'; expected = 1; change = { param($r) $r.properties.files[2].name = 'other.exe' } }
    @{ name = 'failed Properties helper'; expected = 1; change = { param($r) $r.properties.files[1].helperExitCode = 1 } }
    @{ name = 'forced Properties cleanup'; expected = 1; change = { param($r) $r.properties.files[1].forcedTermination = $true } }
    @{ name = 'Properties cleanup error'; expected = 1; change = { param($r) $r.properties.files[1].cleanupErrors = @('failure') } }
    @{ name = 'Properties changed executable'; expected = 1; change = { param($r) $r.properties.files[1].sha256After = ('f' * 64) } }
    @{ name = 'wrong Properties target'; expected = 1; change = { param($r) $r.properties.files[1].observation.generalTarget.expectedFilename = 'soul2d.exe' } }
    @{ name = 'wrong native codepage'; expected = 1; change = { param($r) $r.properties.files[1].observation.resource.translations[0] = '0409/1200' } }
    @{ name = 'wrong associated copyright'; expected = 1; change = { param($r) $r.properties.files[1].observation.copyrightRow.value = 'wrong' } }
    @{ name = 'missing Properties capture hash'; expected = 1; change = { param($r) $r.properties.files[1].observation.capture.pngSha256 = $null } }
    @{ name = 'wrong Properties helper identity'; expected = 1; change = { param($r) $r.properties.files[1].observation.pid = 99 } }
    @{ name = 'wrong Properties file identity'; expected = 1; change = { param($r) $r.properties.files[1].observation.path = 'C:\other.exe' } }
    @{ name = 'missing intended copyright'; expected = 1; change = { param($r) $r.properties.expectedCopyright = $null } }
    @{ name = 'passing launches cannot bypass Properties'; expected = 1; change = { param($r) Set-PassingLaunches $r; $r.properties.files[2].status = 'FAIL' } }
    @{ name = 'software launches cannot bypass Properties'; expected = 1; change = { param($r) Set-SoftwareLaunches $r; $r.properties = $null } }
)
foreach ($case in $cases) {
    $receipt = $known | ConvertTo-Json -Depth 10 | ConvertFrom-Json -AsHashtable
    & $case.change $receipt
    $actual = Get-VerificationExitCode $receipt
    if ($actual -ne $case.expected) { throw "$($case.name): expected $($case.expected), got $actual" }
}
Write-Output "PASS: $($cases.Count) Windows verification outcome cases."

$function = $ast.Find({ param($node)
    $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Get-ObservedProcess'
}, $false)
if ($null -eq $function) { throw 'Missing observed-process acquisition function.' }
. ([scriptblock]::Create($function.Extent.Text))

foreach ($expected in @(0, 7)) {
    $start = [Diagnostics.ProcessStartInfo]::new([Environment]::ProcessPath)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in @('-NoProfile', '-Command', ('$null = [Console]::ReadLine(); exit ' + $expected))) {
        $start.ArgumentList.Add($argument)
    }
    $launcher = [Diagnostics.Process]::Start($start)
    $observed = $null
    try {
        $observed = Get-ObservedProcess $launcher.Id
        $launcher.StandardInput.WriteLine('exit')
        $launcher.StandardInput.Close()
        if (-not $observed.WaitForExit(15000)) { throw 'Owned process did not exit.' }
        $actual = $observed.get_ExitCode()
        if ($actual -ne $expected) { throw "Expected exit code $expected, got $actual." }
        Write-Output "PASS independently observed process exit code $expected"
    } finally {
        if (-not $launcher.HasExited) { $launcher.Kill(); $launcher.WaitForExit() }
        if ($null -ne $observed) { $observed.Dispose() }
        $launcher.Dispose()
    }
}

$function = $ast.Find({ param($node)
    $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Test-DocumentPath'
}, $false)
if ($null -eq $function) { throw 'Missing document-path comparison function.' }
. ([scriptblock]::Create($function.Extent.Text))
if (-not (Test-DocumentPath $null $null) -or (Test-DocumentPath $null 'missing.inx') `
    -or (Test-DocumentPath 'missing.inx' $null)) { throw 'Document-path null handling is inconsistent.' }
$type = $ast.Find({ param($node)
    $node -is [Management.Automation.Language.StringConstantExpressionAst] -and $node.Value.Contains('public static class InstalledSoul2D')
}, $true)
if ($null -eq $type) { throw 'Missing installed-product native API class.' }
Add-Type -TypeDefinition $type.Value
if ($IsWindows) {
    $directory = Join-Path ([IO.Path]::GetTempPath()) "soul2d-path-$([guid]::NewGuid())"
    New-Item -ItemType Directory $directory | Out-Null
    try {
        $file = Join-Path $directory 'installed project-日本語.inx'
        $other = Join-Path $directory 'other.inx'
        [IO.File]::WriteAllText($file, 'owned fixture')
        [IO.File]::WriteAllText($other, 'owned fixture')
        $extended = '\\?\' + $file
        if (-not (Test-DocumentPath $extended $file) -or -not (Test-DocumentPath $file $extended) `
            -or (Test-DocumentPath $other $file)) { throw 'Canonical document-path comparison failed.' }
        Write-Output 'PASS Windows ordinary/extended Unicode paths match; distinct files remain distinct.'
    } finally { Remove-Item -LiteralPath $directory -Recurse -Force }
} else { Write-Output 'Windows canonical-path runtime check NOT_RUN on this host.' }
