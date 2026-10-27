using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Windows.Automation;
using System.Windows.Forms;

public static class PropertiesDiscovery {
    private delegate bool WindowCallback(IntPtr window, IntPtr parameter);
    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    private static extern bool SHObjectProperties(IntPtr owner, uint kind, string path, string page);
    [DllImport("user32.dll")] private static extern bool EnumWindows(WindowCallback callback, IntPtr parameter);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll")] private static extern IntPtr GetWindow(IntPtr window, uint command);
    [DllImport("user32.dll")] private static extern bool IsWindow(IntPtr window);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetWindowText(IntPtr window, StringBuilder text, int count);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetClassName(IntPtr window, StringBuilder text, int count);
    [DllImport("user32.dll")] private static extern bool PostMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
    [DllImport("version.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern uint GetFileVersionInfoSize(string path, out uint handle);
    [DllImport("version.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool GetFileVersionInfo(string path, uint handle, uint length, byte[] data);
    [DllImport("version.dll", CharSet = CharSet.Unicode)]
    private static extern bool VerQueryValue(byte[] block, string name, out IntPtr value, out uint length);

    private static Dictionary<string, object> Resource(string path) {
        uint ignored, length;
        uint size = GetFileVersionInfoSize(path, out ignored);
        if (size == 0 || size > 16 * 1024 * 1024) throw new Exception("Version resource size is invalid");
        byte[] data = new byte[size];
        if (!GetFileVersionInfo(path, 0, size, data)) throw new Exception("Cannot read version resource");
        var pin = GCHandle.Alloc(data, GCHandleType.Pinned);
        try {
        IntPtr value;
        if (!VerQueryValue(data, "\\VarFileInfo\\Translation", out value, out length) || length == 0 || length % 4 != 0)
            throw new Exception("Version translation table is invalid");
        var translations = new List<string>();
        for (int offset = 0; offset < length; offset += 4) {
            translations.Add(((ushort)Marshal.ReadInt16(value, offset)).ToString("X4") + "/" +
                ((ushort)Marshal.ReadInt16(value, offset + 2)).ToString(CultureInfo.InvariantCulture));
        }
        if (!VerQueryValue(data, "\\StringFileInfo\\040904E4\\LegalCopyright", out value, out length) || length == 0)
            throw new Exception("Expected copyright string block is missing");
        return new Dictionary<string, object> { {"translations", translations},
            {"copyright", Marshal.PtrToStringUni(value, checked((int)length - 1))} };
        } finally { pin.Free(); }
    }

    private sealed class WindowState {
        public readonly IntPtr Handle, Owner;
        public readonly uint Pid, Thread;
        public readonly bool Visible;
        public readonly string Kind, Title;
        public WindowState(IntPtr handle, uint pid, uint thread, IntPtr owner, bool visible, string kind, string title) {
            Handle = handle; Pid = pid; Thread = thread; Owner = owner; Visible = visible; Kind = kind; Title = title;
        }
    }

    private sealed class PropertiesIdentity {
        public readonly WindowState Dialog, Stub;
        public PropertiesIdentity(WindowState dialog, WindowState stub) { Dialog = dialog; Stub = stub; }
    }

    private static WindowState ReadWindow(IntPtr window, uint expectedPid) {
        uint pid;
        uint thread = GetWindowThreadProcessId(window, out pid);
        if (!IsWindow(window) || pid != expectedPid || thread == 0) return null;
        var kind = new StringBuilder(256);
        var title = new StringBuilder(512);
        if (GetClassName(window, kind, kind.Capacity) == 0) return null;
        GetWindowText(window, title, title.Capacity);
        return new WindowState(window, pid, thread, GetWindow(window, 4), IsWindowVisible(window), kind.ToString(), title.ToString());
    }

    private static bool FreshPair(WindowState dialog, WindowState stub, HashSet<IntPtr> baseline, uint pid, string title) {
        return dialog != null && stub != null && dialog.Handle != IntPtr.Zero && stub.Handle != IntPtr.Zero && dialog.Handle != stub.Handle
            && !baseline.Contains(dialog.Handle) && !baseline.Contains(stub.Handle)
            && dialog.Pid == pid && stub.Pid == pid && dialog.Thread != 0 && dialog.Thread == stub.Thread
            && dialog.Visible && !stub.Visible && dialog.Kind == "#32770" && dialog.Title == title
            && dialog.Owner == stub.Handle && stub.Kind == "StubWindow32" && stub.Title == "" && stub.Owner == IntPtr.Zero;
    }

    private static bool Same(WindowState first, WindowState second) {
        return first != null && second != null && first.Handle == second.Handle && first.Pid == second.Pid
            && first.Thread == second.Thread && first.Owner == second.Owner && first.Visible == second.Visible
            && first.Kind == second.Kind && first.Title == second.Title;
    }

    private static bool Same(PropertiesIdentity first, PropertiesIdentity second) {
        return first != null && second != null && Same(first.Dialog, second.Dialog) && Same(first.Stub, second.Stub);
    }

    private static void RequireIdentity(PropertiesIdentity identity, HashSet<IntPtr> baseline, uint pid, string title) {
        var dialog = ReadWindow(identity.Dialog.Handle, pid);
        var stub = ReadWindow(identity.Stub.Handle, pid);
        if (!FreshPair(dialog, stub, baseline, pid, title) || !Same(identity, new PropertiesIdentity(dialog, stub)))
            throw new Exception("Properties dialog/stub identity changed");
    }

    private static AutomationElement RootFor(PropertiesIdentity identity, HashSet<IntPtr> baseline, uint pid, string title) {
        RequireIdentity(identity, baseline, pid, title);
        var root = AutomationElement.FromHandle(identity.Dialog.Handle);
        var current = root.Current;
        if (current.ProcessId != pid || current.NativeWindowHandle != unchecked((int)identity.Dialog.Handle.ToInt64())
            || current.ClassName != "#32770" || current.Name != title) throw new Exception("Properties UIA root identity differs");
        RequireIdentity(identity, baseline, pid, title);
        return root;
    }

    public static void VerifyIdentityPolicy() {
        Func<long, uint, uint, long, bool, string, string, WindowState> state = (handle, pid, thread, owner, visible, kind, title) =>
            new WindowState(new IntPtr(handle), pid, thread, new IntPtr(owner), visible, kind, title);
        var dialog = state(66072, 1588, 6436, 66070, true, "#32770", "soul2d Properties");
        var stub = state(66070, 1588, 6436, 0, false, "StubWindow32", "");
        var baseline = new HashSet<IntPtr> { new IntPtr(524402) };
        if (!FreshPair(dialog, stub, baseline, 1588, "soul2d Properties")) throw new Exception("Observed fresh Properties pair was refused");
        foreach (var changed in new[] {
            state(0, 1588, 6436, 66070, true, "#32770", "soul2d Properties"),
            state(66072, 1589, 6436, 66070, true, "#32770", "soul2d Properties"),
            state(66072, 1588, 0, 66070, true, "#32770", "soul2d Properties"),
            state(66072, 1588, 6436, 524402, true, "#32770", "soul2d Properties"),
            state(66072, 1588, 6436, 66070, false, "#32770", "soul2d Properties"),
            state(66072, 1588, 6436, 66070, true, "STATIC", "soul2d Properties"),
            state(66072, 1588, 6436, 66070, true, "#32770", "other Properties"), null }) {
            if (FreshPair(changed, stub, baseline, 1588, "soul2d Properties") || Same(dialog, changed))
                throw new Exception("Invalid or replaced Properties dialog was accepted");
        }
        foreach (var changed in new[] {
            state(0, 1588, 6436, 0, false, "StubWindow32", ""),
            state(66070, 1589, 6436, 0, false, "StubWindow32", ""),
            state(66070, 1588, 8488, 0, false, "StubWindow32", ""),
            state(66070, 1588, 6436, 524402, false, "StubWindow32", ""),
            state(66070, 1588, 6436, 0, true, "StubWindow32", ""),
            state(66070, 1588, 6436, 0, false, "STATIC", ""),
            state(66070, 1588, 6436, 0, false, "StubWindow32", "other"), null }) {
            if (FreshPair(dialog, changed, baseline, 1588, "soul2d Properties") || Same(stub, changed))
                throw new Exception("Invalid or replaced Properties stub was accepted");
        }
        foreach (var handle in new[] { dialog.Handle, stub.Handle }) {
            var old = new HashSet<IntPtr>(baseline) { handle };
            if (FreshPair(dialog, stub, old, 1588, "soul2d Properties")) throw new Exception("Baseline Properties window was accepted");
        }
    }

    private static string Cell(string text) {
        return (text ?? "").Replace("\\", "\\\\").Replace("\t", "\\t").Replace("\r", "\\r").Replace("\n", "\\n");
    }

    private static List<Dictionary<string, object>> Tree(AutomationElement root, out AutomationElement details, string evidence) {
        var rows = new List<Dictionary<string, object>>();
        var queue = new Queue<Tuple<AutomationElement, int>>();
        var walker = TreeWalker.RawViewWalker;
        queue.Enqueue(Tuple.Create(root, 0));
        File.WriteAllText(evidence, "depth\tname\ttype\tautomationId\tpid\thwnd\toffscreen\tbounds\tvalue\tpatterns\n");
        details = null;
        int detailsCount = 0;
        while (queue.Count > 0) {
            if (rows.Count >= 200) throw new Exception("Properties tree exceeded 200 elements");
            var item = queue.Dequeue();
            AutomationElement element = item.Item1;
            var current = element.Current;
            var patterns = new List<string>();
            foreach (var pattern in element.GetSupportedPatterns()) patterns.Add(pattern.ProgrammaticName);
            var row = new Dictionary<string, object> { {"depth", item.Item2}, {"name", current.Name},
                {"type", current.ControlType.ProgrammaticName}, {"automationId", current.AutomationId},
                {"pid", current.ProcessId}, {"hwnd", current.NativeWindowHandle},
                {"offscreen", current.IsOffscreen}, {"bounds", current.BoundingRectangle.ToString(CultureInfo.InvariantCulture)},
                {"patterns", patterns} };
            object supported;
            if (element.TryGetCurrentPattern(ValuePattern.Pattern, out supported))
                row["value"] = ((ValuePattern)supported).Current.Value;
            File.AppendAllText(evidence, item.Item2 + "\t" + Cell(current.Name) + "\t" + current.ControlType.ProgrammaticName + "\t" +
                Cell(current.AutomationId) + "\t" + current.ProcessId + "\t" + current.NativeWindowHandle + "\t" + current.IsOffscreen + "\t" +
                Cell((string)row["bounds"]) + "\t" + Cell(row.ContainsKey("value") ? (string)row["value"] : "") + "\t" +
                Cell(String.Join(",", patterns)) + "\n");
            rows.Add(row);
            if (current.ControlType == ControlType.TabItem && current.Name == "Details") {
                details = element;
                detailsCount++;
            }
            if (item.Item2 >= 8) continue;
            for (var child = walker.GetFirstChild(element); child != null; child = walker.GetNextSibling(child)) {
                if (queue.Count + rows.Count >= 200) throw new Exception("Properties tree exceeded 200 elements");
                queue.Enqueue(Tuple.Create(child, item.Item2 + 1));
            }
        }
        if (detailsCount != 1) throw new Exception("Properties does not expose exactly one English Details tab");
        return rows;
    }

    public static Dictionary<string, object> Run(string path, string evidence) {
        var report = new Dictionary<string, object> { {"status", "FAIL"}, {"scope", "Properties discovery only; Issue acceptance NOT_EVALUATED"},
            {"path", path}, {"pid", Process.GetCurrentProcess().Id}, {"uiLanguage", CultureInfo.CurrentUICulture.Name},
            {"resource", Resource(path)}, {"dialogClosed", false} };
        uint pid = (uint)Process.GetCurrentProcess().Id;
        using (var owner = new Form()) {
            owner.Text = "Soul2D Properties discovery " + Guid.NewGuid().ToString("N");
            owner.ShowInTaskbar = false;
            IntPtr ownerHwnd = owner.Handle;
            report["ownerHwnd"] = ownerHwnd.ToInt64();
            var baseline = new HashSet<IntPtr>();
            if (!EnumWindows((window, unused) => { baseline.Add(window); return true; }, IntPtr.Zero))
                throw new Exception("Cannot capture complete Properties baseline");
            string expectedTitle = Path.GetFileNameWithoutExtension(path) + " Properties";
            var observer = new Thread(() => {
                PropertiesIdentity identity = null;
                try {
                    var clock = Stopwatch.StartNew();
                    PropertiesIdentity previous = null;
                    do {
                        var candidates = new List<PropertiesIdentity>();
                        var topology = new List<string> { "hwnd\tpid\towner\tvisible\tbaseline\tthread\tclass\ttitle" };
                        if (!EnumWindows((window, unused) => {
                            uint observedPid;
                            uint thread = GetWindowThreadProcessId(window, out observedPid);
                            if (!baseline.Contains(window) || observedPid == pid) {
                                if (topology.Count < 50) {
                                    var title = new StringBuilder(512);
                                    var name = new StringBuilder(256);
                                    GetWindowText(window, title, title.Capacity);
                                    GetClassName(window, name, name.Capacity);
                                    topology.Add(window.ToInt64() + "\t" + observedPid + "\t" +
                                        GetWindow(window, 4).ToInt64() + "\t" + IsWindowVisible(window) + "\t" +
                                        baseline.Contains(window) + "\t" + thread + "\t" + Cell(name.ToString()) + "\t" + Cell(title.ToString()));
                                }
                            }
                            var candidate = ReadWindow(window, pid);
                            var stub = candidate == null ? null : ReadWindow(candidate.Owner, pid);
                            if (FreshPair(candidate, stub, baseline, pid, expectedTitle)) candidates.Add(new PropertiesIdentity(candidate, stub));
                            return true;
                        }, IntPtr.Zero)) throw new Exception("Cannot enumerate complete Properties candidates");
                        File.WriteAllLines(Path.Combine(evidence, "window-topology.tsv"), topology);
                        if (candidates.Count > 1) throw new Exception("Multiple owned Properties windows");
                        if (candidates.Count == 1 && Same(previous, candidates[0])) { identity = candidates[0]; break; }
                        previous = candidates.Count == 1 ? candidates[0] : null;
                        Thread.Sleep(100);
                    } while (clock.ElapsedMilliseconds < 10000);
                    if (identity == null) throw new Exception("No stable fresh Properties dialog/stub pair in the exact helper");
                    IntPtr dialog = identity.Dialog.Handle;
                    report["dialogHwnd"] = dialog.ToInt64();
                    report["dialogOwnerHwnd"] = identity.Stub.Handle.ToInt64();
                    report["dialogThread"] = identity.Dialog.Thread;
                    report["dialogClass"] = identity.Dialog.Kind;
                    report["dialogTitle"] = identity.Dialog.Title;
                    report["stubOwnerThread"] = identity.Stub.Thread;
                    report["stubOwnerClass"] = identity.Stub.Kind;
                    report["stubOwnerTitle"] = identity.Stub.Title;
                    report["stubOwnerOwnerHwnd"] = identity.Stub.Owner.ToInt64();
                    report["stableSnapshots"] = 2;
                    report["identityScope"] = "Fresh helper-owned dialog/stub pair; exact displayed file path NOT_EVALUATED";
                    RequireIdentity(identity, baseline, pid, expectedTitle);
                    EditorDesktop.ShowWindow(dialog, 9);
                    RequireIdentity(identity, baseline, pid, expectedTitle);
                    report["placement"] = EditorDesktop.PlaceForCapture(dialog, pid);
                    AutomationElement details;
                    var root = RootFor(identity, baseline, pid, expectedTitle);
                    report["beforeTree"] = Tree(root, out details, Path.Combine(evidence, "before-tree.tsv"));
                    object selection;
                    if (!details.TryGetCurrentPattern(SelectionItemPattern.Pattern, out selection))
                        throw new Exception("Details tab lacks selection pattern");
                    RequireIdentity(identity, baseline, pid, expectedTitle);
                    ((SelectionItemPattern)selection).Select();
                    Thread.Sleep(300);
                    RequireIdentity(identity, baseline, pid, expectedTitle);
                    root = RootFor(identity, baseline, pid, expectedTitle);
                    report["detailsTree"] = Tree(root, out details, Path.Combine(evidence, "details-tree.tsv"));
                    if (!details.TryGetCurrentPattern(SelectionItemPattern.Pattern, out selection) ||
                        !((SelectionItemPattern)selection).Current.IsSelected) throw new Exception("Details is not selected");
                    report["detailsSelected"] = true;
                    RequireIdentity(identity, baseline, pid, expectedTitle);
                    EditorDesktop.SetForegroundWindow(dialog);
                    Thread.Sleep(200);
                    RequireIdentity(identity, baseline, pid, expectedTitle);
                    var frame = EditorDesktop.Capture(dialog, pid);
                    frame.Save(Path.Combine(evidence, "properties-details.bmp"));
                    RequireIdentity(identity, baseline, pid, expectedTitle);
                    report["capture"] = "properties-details.bmp";
                    report["status"] = "OBSERVED";
                } catch (Exception error) { report["failure"] = error.ToString(); }
                finally {
                    try {
                        if (identity != null) {
                            RequireIdentity(identity, baseline, pid, expectedTitle);
                            if (!PostMessage(identity.Dialog.Handle, 0x0010, IntPtr.Zero, IntPtr.Zero)) throw new Exception("Properties WM_CLOSE failed");
                            var clock = Stopwatch.StartNew();
                            while (Same(identity.Dialog, ReadWindow(identity.Dialog.Handle, pid)) && clock.ElapsedMilliseconds < 3000) Thread.Sleep(50);
                            report["dialogClosed"] = !Same(identity.Dialog, ReadWindow(identity.Dialog.Handle, pid));
                        }
                    } catch (Exception error) { report["cleanupFailure"] = error.ToString(); }
                    if (!(bool)report["dialogClosed"]) report["status"] = "FAIL";
                }
            });
            observer.IsBackground = true;
            observer.SetApartmentState(ApartmentState.MTA);
            observer.Start();
            bool invoked = SHObjectProperties(ownerHwnd, 2, path, null);
            var end = Stopwatch.StartNew();
            while (!observer.Join(25) && end.ElapsedMilliseconds < 15000) Application.DoEvents();
            if (observer.IsAlive) throw new Exception("Properties observer did not finish");
            report["invoked"] = invoked;
            if (!invoked) report["status"] = "FAIL";
        }
        return report;
    }
}
