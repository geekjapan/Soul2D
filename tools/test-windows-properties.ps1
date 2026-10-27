$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Add-Type -Path (Join-Path $PSScriptRoot 'windows-properties-policy.cs')
$copyright = "Copyright $([char]0x00a9) 2021 nijigenerate Project"
function Node($Id, $Parent, $Depth, $Type, $Name, $Bounds) {
    $node = [FilePropertyNode]::new()
    $node.Id = $Id; $node.ParentId = $Parent; $node.Depth = $Depth; $node.Pid = 17
    $node.Type = 'ControlType.' + $Type; $node.Name = $Name; $node.AutomationId = ''; $node.Patterns = @()
    $node.Bounds = [double[]]$Bounds
    return $node
}
function GeneralTree([string]$Filename = 'soul2d.exe') {
    $nodes = [Collections.Generic.List[FilePropertyNode]]::new()
    $nodes.Add((Node 0 -1 0 Window 'soul2d Properties' @(302, 73, 419, 573)))
    $nodes.Add((Node 1 0 1 Pane General @(320, 151, 383, 447)))
    $fields = @(@('13156', 'File name:', $Filename.Substring(0, $Filename.Length - 4), 169),
        @('13059', 'Type of file:', 'Application (.exe)', 216), @('13065', 'Location:', 'C:\Program Files\Soul2D', 281))
    foreach ($field in $fields) {
        $node = Node $nodes.Count 1 2 Edit $field[1] @(404, $field[3], 246, 23)
        $node.AutomationId = $field[0]; $node.Value = $field[2]; $node.Patterns = @('ValuePatternIdentifiers.Pattern')
        $nodes.Add($node)
    }
    return ,$nodes
}
function DetailsTree {
    $nodes = [Collections.Generic.List[FilePropertyNode]]::new()
    $nodes.Add((Node 0 -1 0 Window 'soul2d Properties' @(302, 73, 419, 573)))
    $nodes.Add((Node 1 0 1 Pane Details @(320, 151, 383, 447)))
    $grid = Node 2 1 2 DataGrid Properties @(331, 162, 318, 358); $grid.AutomationId = '271'; $nodes.Add($grid)
    $nodes.Add((Node 3 2 3 Group Description @(333, 188, 314, 204)))
    $nodes.Add((Node 4 3 4 DataItem Copyright @(333, 299, 297, 17)))
    $nodes.Add((Node 5 4 5 Text Copyright @(333, 299, 85, 17)))
    $nodes.Add((Node 6 4 5 Text $copyright @(418, 299, 212, 17)))
    foreach ($cell in @($nodes[5], $nodes[6])) { $cell.Patterns = @('GridItemPatternIdentifiers.Pattern', 'TableItemPatternIdentifiers.Pattern') }
    return ,$nodes
}
foreach ($name in @('soul2d.exe', 'soul2d-core.exe', 'njc.exe')) {
    $tree = GeneralTree $name
    [void][FilePropertiesPolicy]::General($tree, 17, 'C:\Program Files\Soul2D', $name)
    $tree[2].Value = $name
    [void][FilePropertiesPolicy]::General($tree, 17, 'C:\Program Files\Soul2D', $name)
}
$capture = [double[]]@(310, 104, 403, 534)
$row = [FilePropertiesPolicy]::Copyright((DetailsTree), 17, $copyright, $capture)
if ($row.value -cne $copyright -or ($row.valueCaptureBounds -join ',') -ne '108,195,212,17' `
    -or $row.glyphInspection -ne 'REQUIRES_CAPTURE_REVIEW') { throw 'Captured-shaped copyright evidence differs.' }
$generalCases = @(
    { param($n) $n[2].Value = 'other' }, { param($n) $n[3].Value = 'Text document (.txt)' },
    { param($n) $n[4].Value = 'C:\other' }, { param($n) $n[2].ParentId = 0; $n[2].Depth = 1 },
    { param($n) $n[2].Pid = 18 }, { param($n) $n[2].Offscreen = $true },
    { param($n) $n[2].Patterns = @() }, { param($n) $n.RemoveAt(4) },
    { param($n) $n[2].Bounds[2] = 1000 }
)
foreach ($change in $generalCases) {
    $tree = GeneralTree; & $change $tree
    $rejected = $false
    try { [void][FilePropertiesPolicy]::General($tree, 17, 'C:\Program Files\Soul2D', 'soul2d.exe') } catch { $rejected = $true }
    if (-not $rejected) { throw 'Invalid General target passed.' }
}
$detailCases = @(
    { param($n) $n[6].Name = 'Copyright (c) 2021 nijigenerate Project' },
    { param($n) $n[6].Name = 'Copyright ?? 2021 nijigenerate Project' },
    { param($n) $n[6].ParentId = 3; $n[6].Depth = 4 },
    { param($n) $n[6].Name = 'wrong'; $n.Add((Node 7 0 1 Text $copyright @(418, 299, 212, 17))) },
    { param($n) $n.Add((Node 7 3 4 DataItem Copyright @(333, 299, 297, 17))) },
    { param($n) $n.Add((Node 7 4 5 Text extra @(418, 299, 212, 17))) },
    { param($n) $n[6].ParentId = 6 }, { param($n) $n.RemoveAt(6) },
    { param($n) $n[6].Pid = 18 }, { param($n) $n[6].Offscreen = $true },
    { param($n) $n[6].Bounds[2] = 300 }, { param($n) $n[6].Bounds[2] = [double]::NaN },
    { param($n) $n[6].Patterns = @('GridItemPatternIdentifiers.Pattern') },
    { param($n) $n[6].Bounds[0] = [double]::PositiveInfinity },
    { param($n) $n[6].Depth = 9 },
    { param($n) for ($i = $n.Count; $i -le 200; $i++) { $n.Add((Node $i 0 1 Text extra @(418, 299, 212, 17))) } }
)
foreach ($change in $detailCases) {
    $tree = DetailsTree; & $change $tree
    $rejected = $false
    try { [void][FilePropertiesPolicy]::Copyright($tree, 17, $copyright, $capture) } catch { $rejected = $true }
    if (-not $rejected) { throw 'Invalid Copyright association passed.' }
}
Write-Output "PASS: six General filename/stem cases, captured Copyright row, $($generalCases.Count + $detailCases.Count) target/association/refusal cases."
