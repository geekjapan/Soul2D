using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.Drawing.Imaging;
using System.Globalization;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using System.Threading;
using System.Windows.Automation;
using System.Windows.Forms;

public static class PropertiesObservation {
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
    [DllImport("kernel32.dll")] private static extern uint GetCurrentThreadId();
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

    private enum ProgressActor { Sta, Observer }

    private sealed class CallProgress : IDisposable {
        private StreamWriter writer;
        private readonly int pid, managedThreadId;
        private readonly uint nativeThreadId;
        private long sequence, nextCallId, activeCallId;
        private bool unavailable;

        public CallProgress(string evidence, ProgressActor actor, uint nativeThreadId) {
            using (var process = Process.GetCurrentProcess()) pid = process.Id;
            managedThreadId = Thread.CurrentThread.ManagedThreadId;
            this.nativeThreadId = nativeThreadId;
            Write(() => {
                var path = Path.Combine(evidence, "progress-" + actor.ToString().ToLowerInvariant() + ".tsv");
                writer = new StreamWriter(new FileStream(path, FileMode.CreateNew, FileAccess.Write, FileShare.Read), new UTF8Encoding(false));
                writer.AutoFlush = true;
                writer.WriteLine("sequence\tutc\tmonotonicTicks\tpid\tmanagedThreadId\tnativeThreadId\tcallId\tparentCallId\toperation\tphase\thwnd\tfirst\tsecond");
            });
            Fact("clock.frequency", IntPtr.Zero, Stopwatch.Frequency, 0);
        }

        private void Write(Action action) {
            if (unavailable) return;
            try { action(); }
            catch (IOException) { Disable(); }
            catch (UnauthorizedAccessException) { Disable(); }
        }

        private void Disable() {
            unavailable = true;
            try { if (writer != null) writer.Dispose(); } catch { }
            try { Console.Error.WriteLine("Properties progress journal unavailable for managed thread " + managedThreadId); } catch { }
        }

        private void Append(long callId, long parentCallId, string operation, string phase, IntPtr window, long first, long second) {
            Write(() => writer.WriteLine(String.Join("\t", new[] {
                (++sequence).ToString(CultureInfo.InvariantCulture), DateTime.UtcNow.ToString("O", CultureInfo.InvariantCulture),
                Stopwatch.GetTimestamp().ToString(CultureInfo.InvariantCulture), pid.ToString(CultureInfo.InvariantCulture),
                managedThreadId.ToString(CultureInfo.InvariantCulture), nativeThreadId.ToString(CultureInfo.InvariantCulture),
                callId.ToString(CultureInfo.InvariantCulture), parentCallId.ToString(CultureInfo.InvariantCulture), Cell(operation), phase,
                window.ToInt64().ToString(CultureInfo.InvariantCulture), first.ToString(CultureInfo.InvariantCulture), second.ToString(CultureInfo.InvariantCulture)
            })));
        }

        public T Call<T>(string operation, IntPtr window, Func<T> body) {
            long callId = ++nextCallId, parent = activeCallId;
            Append(callId, parent, operation, "entered", window, 0, 0);
            activeCallId = callId;
            try {
                T result = body();
                Append(callId, parent, operation, "returned", window, 0, 0);
                return result;
            } catch {
                Append(callId, parent, operation, "threw", window, 0, 0);
                throw;
            } finally { activeCallId = parent; }
        }

        public void Call(string operation, IntPtr window, Action body) {
            Call<object>(operation, window, () => { body(); return null; });
        }

        public void Fact(string operation, IntPtr window, long first, long second) {
            Append(0, activeCallId, operation, "fact", window, first, second);
        }

        public void Dispose() { Write(() => writer.Dispose()); }
    }

    private static WindowState ReadWindow(IntPtr window, uint expectedPid, CallProgress progress) {
        uint pid;
        uint thread = GetWindowThreadProcessId(window, out pid);
        if (!IsWindow(window) || pid != expectedPid || thread == 0) return null;
        var kind = new StringBuilder(256);
        var title = new StringBuilder(512);
        progress.Fact("window.identity", window, pid, thread);
        if (GetClassName(window, kind, kind.Capacity) == 0) return null;
        progress.Call("window.GetWindowText", window, () => GetWindowText(window, title, title.Capacity));
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

    private static void RequireIdentity(PropertiesIdentity identity, HashSet<IntPtr> baseline, uint pid, string title, CallProgress progress) {
        progress.Call("dialog.RequireIdentity", identity.Dialog.Handle, () => {
            var dialog = ReadWindow(identity.Dialog.Handle, pid, progress);
            var stub = ReadWindow(identity.Stub.Handle, pid, progress);
            if (!FreshPair(dialog, stub, baseline, pid, title) || !Same(identity, new PropertiesIdentity(dialog, stub)))
                throw new Exception("Properties dialog/stub identity changed");
        });
    }

    private static AutomationElement RootFor(PropertiesIdentity identity, HashSet<IntPtr> baseline, uint pid, string title, CallProgress progress) {
        return progress.Call("uia.RootFor", identity.Dialog.Handle, () => {
            RequireIdentity(identity, baseline, pid, title, progress);
            var root = progress.Call("uia.FromHandle", identity.Dialog.Handle, () => AutomationElement.FromHandle(identity.Dialog.Handle));
            var current = progress.Call("uia.Current", identity.Dialog.Handle, () => root.Current);
            if (progress.Call("uia.ProcessId", identity.Dialog.Handle, () => current.ProcessId) != pid
                || progress.Call("uia.NativeWindowHandle", identity.Dialog.Handle, () => current.NativeWindowHandle) != unchecked((int)identity.Dialog.Handle.ToInt64())
                || progress.Call("uia.ClassName", identity.Dialog.Handle, () => current.ClassName) != "#32770"
                || progress.Call("uia.Name", identity.Dialog.Handle, () => current.Name) != title) throw new Exception("Properties UIA root identity differs");
            RequireIdentity(identity, baseline, pid, title, progress);
            return root;
        });
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

    private static List<FilePropertyNode> Tree(AutomationElement root, out AutomationElement general, out AutomationElement details, string evidence, CallProgress progress) {
        var rows = new List<FilePropertyNode>();
        var queue = new Queue<Tuple<AutomationElement, int, int>>();
        var walker = TreeWalker.RawViewWalker;
        queue.Enqueue(Tuple.Create(root, 0, -1));
        progress.Call("tree.header", IntPtr.Zero, () => File.WriteAllText(evidence, "id\tparentId\tdepth\tname\ttype\tautomationId\tpid\thwnd\toffscreen\tbounds\tvalue\tpatterns\n"));
        general = null;
        details = null;
        int generalCount = 0, detailsCount = 0;
        while (queue.Count > 0) {
            if (rows.Count >= 200) throw new Exception("Properties tree exceeded 200 elements");
            var item = queue.Dequeue();
            progress.Fact("tree.node", IntPtr.Zero, rows.Count, item.Item2);
            AutomationElement element = item.Item1;
            var current = progress.Call("tree.Current", IntPtr.Zero, () => element.Current);
            var patterns = new List<string>();
            foreach (var pattern in progress.Call("tree.GetSupportedPatterns", IntPtr.Zero, () => element.GetSupportedPatterns())) patterns.Add(pattern.ProgrammaticName);
            var bounds = current.BoundingRectangle;
            var row = new FilePropertyNode { Id = rows.Count, ParentId = item.Item3, Depth = item.Item2, Name = current.Name,
                Type = current.ControlType.ProgrammaticName, AutomationId = current.AutomationId,
                Pid = current.ProcessId, Hwnd = current.NativeWindowHandle, Offscreen = current.IsOffscreen,
                Bounds = new[] { bounds.Left, bounds.Top, bounds.Width, bounds.Height }, Patterns = patterns.ToArray() };
            object supported = null;
            if (progress.Call("tree.TryGetValuePattern", IntPtr.Zero, () => element.TryGetCurrentPattern(ValuePattern.Pattern, out supported)))
                row.Value = progress.Call("tree.Value", IntPtr.Zero, () => ((ValuePattern)supported).Current.Value);
            File.AppendAllText(evidence, row.Id + "\t" + row.ParentId + "\t" + item.Item2 + "\t" + Cell(current.Name) + "\t" + current.ControlType.ProgrammaticName + "\t" +
                Cell(current.AutomationId) + "\t" + current.ProcessId + "\t" + current.NativeWindowHandle + "\t" + current.IsOffscreen + "\t" +
                Cell(bounds.ToString(CultureInfo.InvariantCulture)) + "\t" + Cell(row.Value) + "\t" +
                Cell(String.Join(",", patterns)) + "\n");
            rows.Add(row);
            if (current.ControlType == ControlType.TabItem && current.Name == "General") {
                general = element;
                generalCount++;
            }
            if (current.ControlType == ControlType.TabItem && current.Name == "Details") {
                details = element;
                detailsCount++;
            }
            var first = progress.Call("tree.GetFirstChild", IntPtr.Zero, () => walker.GetFirstChild(element));
            if (item.Item2 >= 8 && first != null) throw new Exception("Properties tree exceeded depth 8");
            for (var child = first; child != null; child = progress.Call("tree.GetNextSibling", IntPtr.Zero, () => walker.GetNextSibling(child))) {
                if (queue.Count + rows.Count >= 200) throw new Exception("Properties tree exceeded 200 elements");
                queue.Enqueue(Tuple.Create(child, item.Item2 + 1, row.Id));
            }
        }
        if (generalCount != 1 || detailsCount != 1) throw new Exception("Properties lacks unique English General and Details tabs");
        return rows;
    }

    private static void SelectTab(AutomationElement tab) {
        object pattern;
        if (!tab.TryGetCurrentPattern(SelectionItemPattern.Pattern, out pattern)) throw new Exception("Properties tab lacks selection pattern");
        ((SelectionItemPattern)pattern).Select();
    }

    private static void RequireSelected(AutomationElement tab) {
        object pattern;
        if (!tab.TryGetCurrentPattern(SelectionItemPattern.Pattern, out pattern) || !((SelectionItemPattern)pattern).Current.IsSelected)
            throw new Exception("Properties tab selection was not confirmed");
    }

    private static string Hash(string path) {
        using (var sha = SHA256.Create()) return BitConverter.ToString(sha.ComputeHash(File.ReadAllBytes(path))).Replace("-", "");
    }

    private static void SavePng(EditorFrame frame, string path) {
        using (var bitmap = new Bitmap(frame.Width, frame.Height, PixelFormat.Format24bppRgb)) {
            var data = bitmap.LockBits(new Rectangle(0, 0, frame.Width, frame.Height), ImageLockMode.WriteOnly, PixelFormat.Format24bppRgb);
            try {
                var row = new byte[frame.Width * 3];
                for (int y = 0; y < frame.Height; y++) {
                    for (int x = 0; x < frame.Width; x++)
                        Buffer.BlockCopy(frame.Pixels, (y * frame.Width + x) * 4, row, x * 3, 3);
                    Marshal.Copy(row, 0, IntPtr.Add(data.Scan0, y * data.Stride), row.Length);
                }
            } finally { bitmap.UnlockBits(data); }
            bitmap.Save(path, ImageFormat.Png);
        }
    }

    public static void VerifyPngEncoding(string evidence) {
        string path = Path.Combine(evidence, "png-selfcheck.png");
        var frame = new EditorFrame { Width = 2, Height = 1, Pixels = new byte[] { 0, 0, 255, 0, 0, 255, 0, 127 } };
        SavePng(frame, path);
        using (var image = new Bitmap(path)) {
            if (image.Width != 2 || image.Height != 1 || image.GetPixel(0, 0).ToArgb() != Color.Red.ToArgb()
                || image.GetPixel(1, 0).ToArgb() != Color.Lime.ToArgb()) throw new Exception("BGRX PNG conversion changed pixels");
        }
    }

    public static Dictionary<string, object> Run(string path, string evidence, string expectedCopyright) {
        var resource = Resource(path);
        var translations = (List<string>)resource["translations"];
        if (translations.Count != 1 || translations[0] != "0409/1252" || (string)resource["copyright"] != expectedCopyright)
            throw new Exception("Installed version translation/copyright differs from source policy");
        var report = new Dictionary<string, object> { {"status", "FAIL"}, {"scope", "Installed file Properties target, resources and associated copyright row; rendered glyph requires capture review"},
            {"path", path}, {"pid", Process.GetCurrentProcess().Id}, {"uiLanguage", CultureInfo.CurrentUICulture.Name},
            {"resource", resource}, {"dialogClosed", false} };
        uint pid = (uint)Process.GetCurrentProcess().Id;
        using (var sta = new CallProgress(evidence, ProgressActor.Sta, GetCurrentThreadId()))
        using (var owner = new Form()) {
            owner.Text = "Soul2D Properties verification " + Guid.NewGuid().ToString("N");
            owner.ShowInTaskbar = false;
            IntPtr ownerHwnd = owner.Handle;
            report["ownerHwnd"] = ownerHwnd.ToInt64();
            var baseline = new HashSet<IntPtr>();
            if (!sta.Call("baseline.EnumWindows", IntPtr.Zero, () => EnumWindows((window, unused) => { baseline.Add(window); return true; }, IntPtr.Zero)))
                throw new Exception("Cannot capture complete Properties baseline");
            string expectedTitle = Path.GetFileNameWithoutExtension(path) + " Properties";
            var observer = new Thread(() => {
                using (var progress = new CallProgress(evidence, ProgressActor.Observer, GetCurrentThreadId())) {
                    PropertiesIdentity identity = null;
                    try {
                        var clock = Stopwatch.StartNew();
                        PropertiesIdentity previous = null;
                        long iteration = 0;
                        do {
                            progress.Fact("discovery.iteration", IntPtr.Zero, ++iteration, 0);
                            var candidates = new List<PropertiesIdentity>();
                            var topology = new List<string> { "hwnd\tpid\towner\tvisible\tbaseline\tthread\tclass\ttitle" };
                            if (!progress.Call("discovery.EnumWindows", IntPtr.Zero, () => EnumWindows((window, unused) => {
                                uint observedPid;
                                uint thread = GetWindowThreadProcessId(window, out observedPid);
                                if (!baseline.Contains(window) || observedPid == pid) {
                                    if (topology.Count < 50) {
                                        var title = new StringBuilder(512);
                                        var name = new StringBuilder(256);
                                        progress.Fact("topology.window", window, observedPid, thread);
                                        progress.Call("topology.GetWindowText", window, () => GetWindowText(window, title, title.Capacity));
                                        GetClassName(window, name, name.Capacity);
                                        topology.Add(window.ToInt64() + "\t" + observedPid + "\t" +
                                            GetWindow(window, 4).ToInt64() + "\t" + IsWindowVisible(window) + "\t" +
                                            baseline.Contains(window) + "\t" + thread + "\t" + Cell(name.ToString()) + "\t" + Cell(title.ToString()));
                                    }
                                }
                                var candidate = ReadWindow(window, pid, progress);
                                var stub = candidate == null ? null : ReadWindow(candidate.Owner, pid, progress);
                                if (FreshPair(candidate, stub, baseline, pid, expectedTitle)) candidates.Add(new PropertiesIdentity(candidate, stub));
                                return true;
                            }, IntPtr.Zero))) throw new Exception("Cannot enumerate complete Properties candidates");
                            progress.Fact("discovery.candidates", IntPtr.Zero, iteration, candidates.Count);
                            progress.Call("topology.save", IntPtr.Zero, () => File.WriteAllLines(Path.Combine(evidence, "window-topology.tsv"), topology));
                            if (candidates.Count > 1) throw new Exception("Multiple owned Properties windows");
                            if (candidates.Count == 1 && Same(previous, candidates[0])) {
                                identity = candidates[0];
                                progress.Fact("discovery.stablePair", identity.Dialog.Handle, identity.Stub.Handle.ToInt64(), 2);
                                break;
                            }
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
                        report["identityScope"] = "Fresh helper-owned dialog/stub pair";
                        RequireIdentity(identity, baseline, pid, expectedTitle, progress);
                        progress.Call("dialog.ShowWindow", dialog, () => { EditorDesktop.ShowWindow(dialog, 9); });
                        RequireIdentity(identity, baseline, pid, expectedTitle, progress);
                        report["placement"] = progress.Call("dialog.PlaceForCapture", dialog, () => EditorDesktop.PlaceForCapture(dialog, pid));
                        AutomationElement general = null, details = null;
                        var root = RootFor(identity, baseline, pid, expectedTitle, progress);
                        report["initialTree"] = progress.Call("tree.initial", dialog, () => Tree(root, out general, out details, Path.Combine(evidence, "initial-tree.tsv"), progress));
                        RequireIdentity(identity, baseline, pid, expectedTitle, progress);
                        progress.Call("tab.SelectGeneral", dialog, () => SelectTab(general));
                        Thread.Sleep(100);
                        root = RootFor(identity, baseline, pid, expectedTitle, progress);
                        var generalTree = progress.Call("tree.general", dialog, () => Tree(root, out general, out details, Path.Combine(evidence, "general-tree.tsv"), progress));
                        progress.Call("tab.RequireGeneral", dialog, () => RequireSelected(general));
                        report["generalTree"] = generalTree;
                        report["generalTarget"] = FilePropertiesPolicy.General(generalTree, (int)pid, Path.GetDirectoryName(path), Path.GetFileName(path));
                        RequireIdentity(identity, baseline, pid, expectedTitle, progress);
                        progress.Call("tab.SelectDetails", dialog, () => SelectTab(details));
                        Thread.Sleep(300);
                        RequireIdentity(identity, baseline, pid, expectedTitle, progress);
                        root = RootFor(identity, baseline, pid, expectedTitle, progress);
                        var detailsTree = progress.Call("tree.details", dialog, () => Tree(root, out general, out details, Path.Combine(evidence, "details-tree.tsv"), progress));
                        progress.Call("tab.RequireDetails", dialog, () => RequireSelected(details));
                        var bounds = progress.Call("uia.BoundingRectangle", dialog, () => root.Current.BoundingRectangle);
                        FilePropertiesPolicy.Copyright(detailsTree, (int)pid, expectedCopyright, new[] { bounds.Left, bounds.Top, bounds.Width, bounds.Height });
                        report["detailsTree"] = detailsTree;
                        report["detailsSelected"] = true;
                        RequireIdentity(identity, baseline, pid, expectedTitle, progress);
                        progress.Call("dialog.SetForegroundWindow", dialog, () => { EditorDesktop.SetForegroundWindow(dialog); });
                        Thread.Sleep(200);
                        RequireIdentity(identity, baseline, pid, expectedTitle, progress);
                        var frame = progress.Call("dialog.Capture", dialog, () => EditorDesktop.Capture(dialog, pid));
                        var capture = new double[] { frame.Left, frame.Top, frame.Width, frame.Height };
                        report["copyrightRow"] = FilePropertiesPolicy.Copyright(detailsTree, (int)pid, expectedCopyright, capture);
                        string bmp = Path.Combine(evidence, "properties-details.bmp"), png = Path.Combine(evidence, "properties-details.png");
                        progress.Call("capture.SaveBmp", dialog, () => frame.Save(bmp));
                        progress.Call("capture.SavePng", dialog, () => SavePng(frame, png));
                        RequireIdentity(identity, baseline, pid, expectedTitle, progress);
                        root = RootFor(identity, baseline, pid, expectedTitle, progress);
                        var capturedTree = progress.Call("tree.captured", dialog, () => Tree(root, out general, out details, Path.Combine(evidence, "captured-tree.tsv"), progress));
                        progress.Call("tab.RequireCapturedDetails", dialog, () => RequireSelected(details));
                        report["capturedTree"] = capturedTree;
                        report["capturedCopyrightRow"] = FilePropertiesPolicy.Copyright(capturedTree, (int)pid, expectedCopyright, capture);
                        RequireIdentity(identity, baseline, pid, expectedTitle, progress);
                        report["capture"] = new Dictionary<string, object> { {"bmp", "properties-details.bmp"}, {"bmpSha256", Hash(bmp)},
                            {"png", "properties-details.png"}, {"pngSha256", Hash(png)}, {"bounds", capture} };
                        report["status"] = "PASS";
                    } catch (Exception error) { report["failure"] = error.ToString(); progress.Fact("observer.failure", IntPtr.Zero, 0, 0); }
                    finally {
                        progress.Fact("cleanup.entered", IntPtr.Zero, 0, 0);
                        try {
                            if (identity != null) {
                                RequireIdentity(identity, baseline, pid, expectedTitle, progress);
                                if (!progress.Call("dialog.PostClose", identity.Dialog.Handle, () => PostMessage(identity.Dialog.Handle, 0x0010, IntPtr.Zero, IntPtr.Zero))) throw new Exception("Properties WM_CLOSE failed");
                                var clock = Stopwatch.StartNew();
                                while (Same(identity.Dialog, ReadWindow(identity.Dialog.Handle, pid, progress)) && clock.ElapsedMilliseconds < 3000) Thread.Sleep(50);
                                report["dialogClosed"] = !Same(identity.Dialog, ReadWindow(identity.Dialog.Handle, pid, progress));
                            }
                        } catch (Exception error) { report["cleanupFailure"] = error.ToString(); }
                        if (!(bool)report["dialogClosed"]) report["status"] = "FAIL";
                        progress.Fact("cleanup.finished", IntPtr.Zero, (bool)report["dialogClosed"] ? 1 : 0, 0);
                    }
                }
            });
            observer.IsBackground = true;
            observer.SetApartmentState(ApartmentState.MTA);
            sta.Call("observer.Start", IntPtr.Zero, () => observer.Start());
            bool invoked = sta.Call("shell.SHObjectProperties", ownerHwnd, () => SHObjectProperties(ownerHwnd, 2, path, null));
            sta.Call("observer.Join", IntPtr.Zero, () => {
                var end = Stopwatch.StartNew();
                while (!observer.Join(25) && end.ElapsedMilliseconds < 15000) Application.DoEvents();
            });
            if (observer.IsAlive) {
                sta.Fact("observer.timeout", IntPtr.Zero, 15000, 0);
                throw new Exception("Properties observer did not finish");
            }
            report["invoked"] = invoked;
            if (!invoked) report["status"] = "FAIL";
        }
        return report;
    }
}
