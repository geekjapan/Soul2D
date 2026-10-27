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

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class EditorPlacementFixture {
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct MonitorInfo { public uint Size; public Rect Monitor, Work; public uint Flags; }
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern IntPtr CreateWindowEx(uint extendedStyle, string kind, string title, uint style,
        int x, int y, int width, int height, IntPtr parent, IntPtr menu, IntPtr instance, IntPtr parameter);
    [DllImport("user32.dll")] public static extern bool DestroyWindow(IntPtr window);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll")] public static extern int GetSystemMetrics(int index);
    [DllImport("user32.dll")] public static extern IntPtr MonitorFromWindow(IntPtr window, uint flags);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern bool GetMonitorInfo(IntPtr monitor, ref MonitorInfo info);
}
'@
if ($IsWindows) {
    $desktopRight = [EditorPlacementFixture]::GetSystemMetrics(76) + [EditorPlacementFixture]::GetSystemMetrics(78)
    $desktopBottom = [EditorPlacementFixture]::GetSystemMetrics(77) + [EditorPlacementFixture]::GetSystemMetrics(79)
    $placementWindow = [EditorPlacementFixture]::CreateWindowEx(0x08000080, 'STATIC', 'Soul2D placement self-check',
        0x90000000u, ($desktopRight + 200), ($desktopBottom + 200), 160, 100,
        [IntPtr]::Zero, [IntPtr]::Zero, [IntPtr]::Zero, [IntPtr]::Zero)
    if ($placementWindow -eq [IntPtr]::Zero) { throw 'Cannot create the owned placement fixture.' }
    try {
        $before = [EditorPlacementFixture+Rect]::new()
        if (-not [EditorPlacementFixture]::GetWindowRect($placementWindow, [ref]$before) `
            -or $before.Left -lt $desktopRight -or $before.Top -lt $desktopBottom) {
            throw 'Placement fixture did not start outside the desktop.'
        }
        $rejected = $false
        try { [void][EditorDesktop]::PlaceForCapture($placementWindow, [uint32]($PID + 1)) } catch { $rejected = $true }
        $unchanged = [EditorPlacementFixture+Rect]::new()
        if (-not $rejected -or -not [EditorPlacementFixture]::GetWindowRect($placementWindow, [ref]$unchanged) `
            -or ($before | ConvertTo-Json -Compress) -cne ($unchanged | ConvertTo-Json -Compress)) {
            throw 'Wrong-owner placement did not preserve the window.'
        }
        $placement = [EditorDesktop]::PlaceForCapture($placementWindow, [uint32]$PID)
        $after = [EditorPlacementFixture+Rect]::new()
        $info = [EditorPlacementFixture+MonitorInfo]::new()
        $info.Size = [Runtime.InteropServices.Marshal]::SizeOf($info)
        if (-not [EditorPlacementFixture]::GetWindowRect($placementWindow, [ref]$after) `
            -or -not [EditorPlacementFixture]::GetMonitorInfo([EditorPlacementFixture]::MonitorFromWindow($placementWindow, 2), [ref]$info) `
            -or $after.Left -lt $info.Work.Left -or $after.Top -lt $info.Work.Top `
            -or $after.Right -gt $info.Work.Right -or $after.Bottom -gt $info.Work.Bottom `
            -or ($after.Right - $after.Left) -ne ($before.Right - $before.Left) `
            -or ($after.Bottom - $after.Top) -ne ($before.Bottom - $before.Top)) {
            throw 'Owned window was not placed inside the work area with its size preserved.'
        }
        $rejected = $false
        try { [void][EditorDesktop]::PlaceForCapture([IntPtr]::Zero, [uint32]$PID) } catch { $rejected = $true }
        if (-not $rejected) { throw 'Invalid window placement passed.' }
        Write-Output "PASS: owned offscreen window placement, unchanged size, wrong-owner and invalid-window refusal. $placement"
    } finally {
        if (-not [EditorPlacementFixture]::DestroyWindow($placementWindow)) { throw 'Owned placement fixture cleanup failed.' }
    }
} else { Write-Output 'Windows owned-window placement runtime check NOT_RUN on this host.' }
