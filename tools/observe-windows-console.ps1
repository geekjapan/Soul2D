param([Parameter(Mandatory)][int]$EditorPid, [Parameter(Mandatory)][string]$OutputPath)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class ConsoleObservation {
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool FreeConsole();
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool AttachConsole(uint processId);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern uint GetConsoleProcessList([Out] uint[] processes, uint count);
    [DllImport("kernel32.dll")]
    public static extern IntPtr GetConsoleWindow();
    [DllImport("user32.dll")]
    public static extern bool IsWindowVisible(IntPtr window);
}
'@
$result = [ordered]@{ editorPid = $EditorPid; attached = $false; attachError = $null;
    processIds = @(); hwnd = 0; visible = $false; detached = $false }
try {
    [void][ConsoleObservation]::FreeConsole()
    $result.attached = [ConsoleObservation]::AttachConsole([uint32]$EditorPid)
    if (-not $result.attached) {
        $result.attachError = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
        throw "Cannot attach console for editor PID $EditorPid"
    }
    $processes = [uint32[]]::new(1024)
    $count = [ConsoleObservation]::GetConsoleProcessList($processes, $processes.Length)
    if ($count -eq 0 -or $count -gt $processes.Length) { throw 'Cannot enumerate the complete editor console membership.' }
    $result.processIds = @($processes[0..($count - 1)])
    $window = [ConsoleObservation]::GetConsoleWindow()
    $result.hwnd = $window.ToInt64()
    $result.visible = [ConsoleObservation]::IsWindowVisible($window)
    if ($result.processIds -notcontains $EditorPid) { throw 'Editor is absent from its console membership.' }
} finally {
    if ($result.attached) { $result.detached = [ConsoleObservation]::FreeConsole() }
    $result | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $OutputPath -Encoding utf8NoBOM
}
if (-not $result.detached) { throw 'Console observer failed to detach.' }
