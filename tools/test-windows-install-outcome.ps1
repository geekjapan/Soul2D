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
    installation = 'PASS'; shortcut = 'FAIL'; association = 'NOT_RUN'
    failure = 'start-menu did not produce an installed editor window and owned MCP listener before the deadline.'
    cleanup = @{ uninstallExitCode = 0; errors = @(); productAbsent = $true; installDirectoryAbsent = $true;
        shortcutAbsent = $true; registrationsAbsent = $true; associationAbsent = $true }
    launches = @(@{ kind = 'start-menu'; accepted = $false; directDiagnostic = @{
        exited = $true; exitCode = 1; hwnd = 0
        stderr = 'Error: OpenGL(PainterError("egui_glow requires opengl 2.0+. "))'
    } })
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
        $r.failure = $null; $r.shortcut = 'PASS'; $r.association = 'PASS'
        $r.launches = @(@{ kind = 'start-menu'; accepted = $true }, @{ kind = 'association'; accepted = $true })
    } }
)
foreach ($case in $cases) {
    $receipt = $known | ConvertTo-Json -Depth 10 | ConvertFrom-Json -AsHashtable
    & $case.change $receipt
    $actual = Get-VerificationExitCode $receipt
    if ($actual -ne $case.expected) { throw "$($case.name): expected $($case.expected), got $actual" }
}
Write-Output "PASS: $($cases.Count) Windows verification outcome cases."
