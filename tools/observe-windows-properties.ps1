param([Parameter(Mandatory)][string]$FilePath, [Parameter(Mandatory)][string]$EvidenceDirectory,
    [Parameter(Mandatory)][string]$ExpectedCopyright)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($PSVersionTable.PSEdition -ne 'Desktop' -or -not [Environment]::Is64BitProcess `
    -or $env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'Properties verification requires owned Windows x64 GitHub Actions and Windows PowerShell 5.1.'
}
$directory = Join-Path $env:ProgramFiles 'Soul2D'
$allowed = @('soul2d.exe', 'soul2d-core.exe', 'njc.exe') | ForEach-Object { Join-Path $directory $_ }
if (@($allowed | Where-Object { [String]::Equals($_, [IO.Path]::GetFullPath($FilePath), [StringComparison]::OrdinalIgnoreCase) }).Count -ne 1) {
    throw 'Properties verification accepts only the three installed Soul2D executables.'
}
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, WindowsBase, System.Windows.Forms
$references = @([System.Windows.Automation.AutomationElement].Assembly.Location,
    [System.Windows.Automation.ControlType].Assembly.Location, [System.Windows.Rect].Assembly.Location,
    [System.Windows.Forms.Form].Assembly.Location, 'System.dll', 'System.Core.dll', 'System.Drawing.dll')
Add-Type -Path @((Join-Path $PSScriptRoot 'windows-editor-observation.cs'),
    (Join-Path $PSScriptRoot 'windows-properties-policy.cs'), (Join-Path $PSScriptRoot 'windows-properties-observation.cs')) -ReferencedAssemblies $references
[PropertiesObservation]::VerifyIdentityPolicy()
Write-Output 'PASS: observed Properties pair, invalid/replaced dialog and stub, and baseline refusal checks.'
[PropertiesObservation]::VerifyPngEncoding($EvidenceDirectory)
Write-Output 'PASS: lossless BGRX-to-RGB PNG conversion ignores the unused fourth byte.'
$result = [PropertiesObservation]::Run($FilePath, $EvidenceDirectory, $ExpectedCopyright)
$result | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath (Join-Path $EvidenceDirectory 'observation.json') -Encoding UTF8
if ($result.status -ne 'PASS') { throw 'Properties verification did not complete; see observation.json.' }
