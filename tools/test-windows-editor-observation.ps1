$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Add-Type -Path (Join-Path $PSScriptRoot 'windows-editor-observation.cs')
function Frame([int[]]$Color) {
    $frame = [EditorFrame]::new()
    $frame.Width = 50; $frame.Height = 40; $frame.Left = 20; $frame.Top = 30
    $frame.Pixels = [byte[]]::new(50 * 40 * 4)
    for ($offset = 0; $offset -lt $frame.Pixels.Length; $offset += 4) {
        $frame.Pixels[$offset] = $Color[2]
        $frame.Pixels[$offset + 1] = $Color[1]
        $frame.Pixels[$offset + 2] = $Color[0]
    }
    return $frame
}
$a = @(239, 31, 151); $b = @(23, 211, 67)
$first = Frame $a; $second = Frame $b
if ([EditorDesktop]::CountTransitions($first, $second, $a, $b) -ne 2000) { throw 'Changed pixels were not counted.' }
if ([EditorDesktop]::CountTransitions($first, $first, $a, $b) -ne 0) { throw 'Unchanged pixels passed.' }
if ([EditorDesktop]::CountTransitions($first, $second, $a, @(0, 0, 255)) -ne 0) { throw 'Wrong colour passed.' }
foreach ($case in @('origin', 'size', 'buffer', 'same-colour')) {
    $changed = Frame $b
    switch ($case) {
        'origin' { $changed.Left++ }
        'size' { $changed.Width++ }
        'buffer' { $changed.Pixels = [byte[]]::new(4) }
    }
    $rejected = $false
    try { [void][EditorDesktop]::CountTransitions($first, $changed, $a, $(if ($case -eq 'same-colour') { $a } else { $b })) }
    catch { $rejected = $true }
    if (-not $rejected) { throw "Invalid $case frame passed." }
}
$temporary = Join-Path ([IO.Path]::GetTempPath()) "soul2d-frame-$([guid]::NewGuid()).bmp"
try {
    $first.Save($temporary)
    $bytes = [IO.File]::ReadAllBytes($temporary)
    if ($bytes.Length -ne 8054 -or [BitConverter]::ToUInt16($bytes, 0) -ne 0x4d42 `
        -or [BitConverter]::ToInt32($bytes, 22) -ne -40) { throw 'Invalid saved client bitmap.' }
} finally { Remove-Item -LiteralPath $temporary -ErrorAction SilentlyContinue }
Write-Output 'PASS: native-pixel comparison and bitmap encoding checks.'
