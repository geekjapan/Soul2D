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

    private static bool Owned(IntPtr window, IntPtr owner, uint expectedPid) {
        uint pid;
        GetWindowThreadProcessId(window, out pid);
        return IsWindow(window) && pid == expectedPid && GetWindow(window, 4) == owner;
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
            EnumWindows((window, unused) => { baseline.Add(window); return true; }, IntPtr.Zero);
            var observer = new Thread(() => {
                IntPtr dialog = IntPtr.Zero;
                try {
                    DateTime deadline = DateTime.UtcNow.AddSeconds(10);
                    do {
                        var candidates = new List<IntPtr>();
                        var topology = new List<string> { "hwnd\tpid\towner\tvisible\tbaseline\tthread\tclass\ttitle" };
                        EnumWindows((window, unused) => {
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
                            if (Owned(window, ownerHwnd, pid) && IsWindowVisible(window)) candidates.Add(window);
                            return true;
                        }, IntPtr.Zero);
                        File.WriteAllLines(Path.Combine(evidence, "window-topology.tsv"), topology);
                        if (candidates.Count > 1) throw new Exception("Multiple owned Properties windows");
                        if (candidates.Count == 1) { dialog = candidates[0]; break; }
                        Thread.Sleep(100);
                    } while (DateTime.UtcNow < deadline);
                    if (dialog == IntPtr.Zero) throw new Exception("No visible Properties dialog with the exact helper PID and owner");
                    report["dialogHwnd"] = dialog.ToInt64();
                    report["dialogOwnerHwnd"] = GetWindow(dialog, 4).ToInt64();
                    EditorDesktop.ShowWindow(dialog, 9);
                    report["placement"] = EditorDesktop.PlaceForCapture(dialog, pid);
                    AutomationElement details;
                    var root = AutomationElement.FromHandle(dialog);
                    report["beforeTree"] = Tree(root, out details, Path.Combine(evidence, "before-tree.tsv"));
                    object selection;
                    if (!details.TryGetCurrentPattern(SelectionItemPattern.Pattern, out selection))
                        throw new Exception("Details tab lacks selection pattern");
                    ((SelectionItemPattern)selection).Select();
                    Thread.Sleep(300);
                    root = AutomationElement.FromHandle(dialog);
                    report["detailsTree"] = Tree(root, out details, Path.Combine(evidence, "details-tree.tsv"));
                    if (!details.TryGetCurrentPattern(SelectionItemPattern.Pattern, out selection) ||
                        !((SelectionItemPattern)selection).Current.IsSelected) throw new Exception("Details is not selected");
                    report["detailsSelected"] = true;
                    EditorDesktop.SetForegroundWindow(dialog);
                    Thread.Sleep(200);
                    var frame = EditorDesktop.Capture(dialog, pid);
                    frame.Save(Path.Combine(evidence, "properties-details.bmp"));
                    report["capture"] = "properties-details.bmp";
                    report["status"] = "OBSERVED";
                } catch (Exception error) { report["failure"] = error.ToString(); }
                finally {
                    if (dialog != IntPtr.Zero && Owned(dialog, ownerHwnd, pid)) {
                        PostMessage(dialog, 0x0010, IntPtr.Zero, IntPtr.Zero);
                        DateTime deadline = DateTime.UtcNow.AddSeconds(3);
                        while (IsWindow(dialog) && DateTime.UtcNow < deadline) Thread.Sleep(50);
                        report["dialogClosed"] = !IsWindow(dialog);
                    }
                    if (!(bool)report["dialogClosed"]) report["status"] = "FAIL";
                }
            });
            observer.IsBackground = true;
            observer.SetApartmentState(ApartmentState.MTA);
            observer.Start();
            bool invoked = SHObjectProperties(ownerHwnd, 2, path, null);
            DateTime end = DateTime.UtcNow.AddSeconds(15);
            while (!observer.Join(25) && DateTime.UtcNow < end) Application.DoEvents();
            if (observer.IsAlive) throw new Exception("Properties observer did not finish");
            report["invoked"] = invoked;
            if (!invoked) report["status"] = "FAIL";
        }
        return report;
    }
}
