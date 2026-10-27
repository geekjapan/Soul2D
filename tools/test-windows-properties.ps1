param([string]$ProgressChildDirectory)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$source = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'windows-properties-observation.cs')
$parts = foreach ($pattern in @(
    '(?m)^    private enum ProgressActor \{[^\r\n]+\}',
    '(?ms)^    private sealed class CallProgress : IDisposable \{.*?^    \}',
    '(?ms)^    private static string Cell\(string text\) \{.*?^    \}'
)) {
    $found = [regex]::Matches($source, $pattern)
    if ($found.Count -ne 1) { throw 'Expected one production progress member.' }
    $found[0].Value
}
Add-Type -TypeDefinition (@'
using System;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Text;
using System.Threading;
public static class PropertiesProgressTests {
'@ + ($parts -join "`n") + @'
    private static string[][] Read(string path) {
        using (var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite))
        using (var reader = new StreamReader(stream)) {
            string text = reader.ReadToEnd();
            int complete = text.LastIndexOf('\n');
            if (complete < 0) return new string[0][];
            var rows = text.Substring(0, complete).Split('\n').Select(line => line.TrimEnd('\r').Split('\t')).ToArray();
            if (rows[0].Length != 13 || rows[0][0] != "sequence") throw new Exception("Missing progress header");
            foreach (var row in rows.Skip(1)) if (row.Length != 13) throw new Exception("Incomplete progress record");
            return rows.Skip(1).ToArray();
        }
    }

    public static bool Entered(string directory, string operation) {
        string path = Path.Combine(directory, "progress-observer.tsv");
        if (!File.Exists(path)) return false;
        try { return Read(path).Any(row => row[8] == operation && row[9] == "entered"); }
        catch (IOException) { return false; }
    }

    public static void Pending(string directory) {
        using (var progress = new CallProgress(directory, ProgressActor.Observer, 333))
            progress.Call("child.pending", new IntPtr(9), () => Thread.Sleep(Timeout.Infinite));
    }

    public static void VerifyPending(string directory) {
        var rows = Read(Path.Combine(directory, "progress-observer.tsv"));
        if (rows.Last()[8] != "child.pending" || rows.Last()[9] != "entered"
            || rows.Any(row => row[8] == "child.pending" && row[9] != "entered"))
            throw new Exception("Terminated helper lost its outstanding entry");
    }

    public static void Verify(string directory) {
        string path = Path.Combine(directory, "progress-sta.tsv");
        var sentinel = new InvalidOperationException("sentinel");
        int calls = 0;
        using (var progress = new CallProgress(directory, ProgressActor.Sta, 111)) {
            if (progress.Call("normal", new IntPtr(7), () => { calls++; return 42; }) != 42 || calls != 1)
                throw new Exception("Progress changed the invocation or return value");
            if (progress.Call("false", IntPtr.Zero, () => false)
                || progress.Call<object>("null", IntPtr.Zero, () => null) != null)
                throw new Exception("Progress changed false or null return semantics");
            bool caught = false;
            try { progress.Call("throws", IntPtr.Zero, () => { throw sentinel; }); }
            catch (Exception error) { if (!Object.ReferenceEquals(error, sentinel)) throw; caught = true; }
            if (!caught) throw new Exception("Progress swallowed the original exception");
            progress.Call("outer", IntPtr.Zero, () => progress.Call("inner", IntPtr.Zero, () => 5));
            progress.Call("pending.read", IntPtr.Zero, () => {
                var current = Read(path);
                if (current.Last()[8] != "pending.read" || current.Last()[9] != "entered")
                    throw new Exception("Entry was unavailable while the delegate was pending");
            });
            progress.Call("after.throw", IntPtr.Zero, () => { });
        }
        var rows = Read(path);
        long sequence = 0, ticks = 0;
        foreach (var row in rows) {
            long observed = Int64.Parse(row[2], CultureInfo.InvariantCulture);
            if (Int64.Parse(row[0], CultureInfo.InvariantCulture) != ++sequence || observed < ticks
                || row[5] != "111") throw new Exception("Progress ordering or actor metadata differs");
            ticks = observed;
        }
        foreach (var entry in rows.Where(row => row[9] == "entered")) {
            var terminal = rows.Where(row => row[6] == entry[6] && (row[9] == "returned" || row[9] == "threw")).ToArray();
            if (terminal.Length != 1 || terminal[0][7] != entry[7] || terminal[0][8] != entry[8]
                || terminal[0][10] != entry[10] || terminal[0][9] != (entry[8] == "throws" ? "threw" : "returned"))
                throw new Exception("Progress call pairing differs");
        }
        var outer = rows.Single(row => row[8] == "outer" && row[9] == "entered");
        var inner = rows.Single(row => row[8] == "inner" && row[9] == "entered");
        if (inner[7] != outer[6] || rows.Single(row => row[8] == "after.throw" && row[9] == "entered")[7] != "0")
            throw new Exception("Progress nesting or parent restoration differs");
        Exception workerError = null;
        var observer = new Thread(() => {
            try {
                using (var progress = new CallProgress(directory, ProgressActor.Observer, 222))
                    progress.Call("observer", IntPtr.Zero, () => { });
            } catch (Exception error) { workerError = error; }
        });
        observer.IsBackground = true;
        observer.Start();
        if (!observer.Join(5000)) throw new Exception("Progress actor test did not finish");
        if (workerError != null) throw workerError;
        var other = Read(Path.Combine(directory, "progress-observer.tsv"));
        if (other.Any(row => row[5] != "222") || other[0][4] == rows[0][4] || Read(path).Length != rows.Length)
            throw new Exception("Actor journals interfered");
        string blocked = Path.Combine(directory, "not-a-directory");
        File.WriteAllText(blocked, "owned fixture");
        using (var progress = new CallProgress(blocked, ProgressActor.Sta, 0)) {
            if (progress.Call("unavailable", IntPtr.Zero, () => ++calls) != 2)
                throw new Exception("Unavailable diagnostics skipped the delegate");
            bool caught = false;
            try { progress.Call("unavailable.throw", IntPtr.Zero, () => { throw sentinel; }); }
            catch (Exception error) { if (!Object.ReferenceEquals(error, sentinel)) throw; caught = true; }
            if (!caught) throw new Exception("Unavailable diagnostics swallowed the exception");
        }
    }
}
'@)
if ($ProgressChildDirectory) {
    [PropertiesProgressTests]::Pending($ProgressChildDirectory)
    throw 'Pending progress child unexpectedly returned.'
}
$progressDirectory = Join-Path ([IO.Path]::GetTempPath()) ('soul2d-properties-progress-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $progressDirectory | Out-Null
$child = $null
try {
    [PropertiesProgressTests]::Verify($progressDirectory)
    $childDirectory = Join-Path $progressDirectory 'child'
    New-Item -ItemType Directory $childDirectory | Out-Null
    $child = Start-Process -FilePath (Get-Process -Id $PID).Path -PassThru -ArgumentList @('-NoProfile', '-File',
        "`"$PSCommandPath`"", '-ProgressChildDirectory', "`"$childDirectory`"") `
        -RedirectStandardOutput (Join-Path $progressDirectory 'child.stdout') -RedirectStandardError (Join-Path $progressDirectory 'child.stderr')
    $wait = [Diagnostics.Stopwatch]::StartNew()
    while (-not [PropertiesProgressTests]::Entered($childDirectory, 'child.pending')) {
        if ($child.HasExited) { throw 'Progress child exited before its flushed entry.' }
        if ($wait.ElapsedMilliseconds -ge 15000) { throw 'Progress child did not reach its pending delegate.' }
        Start-Sleep -Milliseconds 50
    }
    $child.Kill()
    if (-not $child.WaitForExit(5000)) { throw 'Owned progress child termination was not confirmed.' }
    [PropertiesProgressTests]::VerifyPending($childDirectory)
    Write-Output 'PASS: production progress writer preserves returns, exceptions, nesting, actor isolation, live entries, unavailable-I/O semantics and an entry after owned child termination.'
} finally {
    if ($null -ne $child) {
        if (-not $child.HasExited) { $child.Kill(); if (-not $child.WaitForExit(5000)) { throw 'Progress child cleanup failed.' } }
        $child.Dispose()
    }
    Remove-Item -LiteralPath $progressDirectory -Recurse -Force
}
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
