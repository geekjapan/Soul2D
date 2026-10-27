param([Parameter(Mandatory)][string]$FilePath, [Parameter(Mandatory)][string]$EvidenceDirectory,
    [Parameter(Mandatory)][string]$CaptureSource)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($PSVersionTable.PSEdition -ne 'Desktop' -or -not [Environment]::Is64BitProcess `
    -or $env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'Properties discovery requires owned Windows x64 GitHub Actions and Windows PowerShell 5.1.'
}
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, WindowsBase, System.Windows.Forms
$references = @([System.Windows.Automation.AutomationElement].Assembly.Location,
    [System.Windows.Automation.ControlType].Assembly.Location, [System.Windows.Rect].Assembly.Location,
    [System.Windows.Forms.Form].Assembly.Location, 'System.dll', 'System.Core.dll', 'System.Drawing.dll')
Add-Type -Path @($CaptureSource, (Join-Path $PSScriptRoot 'PropertiesDiscovery.cs')) -ReferencedAssemblies $references
$result = [PropertiesDiscovery]::Run($FilePath, $EvidenceDirectory)
$result | ConvertTo-Json -Depth 15 | Set-Content -LiteralPath (Join-Path $EvidenceDirectory 'discovery.json') -Encoding UTF8
if ($result.status -ne 'OBSERVED') { throw 'Properties discovery did not complete; see discovery.json.' }
