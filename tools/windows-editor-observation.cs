using System;
using System.IO;
using System.Runtime.InteropServices;

public sealed class EditorFrame {
    public int Left, Top, Width, Height;
    public byte[] Pixels;

    public void Save(string path) {
        using (var writer = new BinaryWriter(File.Create(path))) {
            writer.Write((ushort)0x4d42);
            writer.Write(54 + Pixels.Length);
            writer.Write(0);
            writer.Write(54);
            writer.Write(40);
            writer.Write(Width);
            writer.Write(-Height);
            writer.Write((ushort)1);
            writer.Write((ushort)32);
            writer.Write(0);
            writer.Write(Pixels.Length);
            writer.Write(new byte[16]);
            writer.Write(Pixels);
        }
    }
}

public static class EditorDesktop {
    [StructLayout(LayoutKind.Sequential)] private struct Rect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] private struct Point { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)] private struct BitmapInfo {
        public uint Size; public int Width, Height; public ushort Planes, BitCount;
        public uint Compression, SizeImage; public int XPels, YPels; public uint Used, Important;
    }
    [DllImport("user32.dll")] private static extern bool GetClientRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll")] private static extern bool ClientToScreen(IntPtr window, ref Point point);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")] private static extern bool IsIconic(IntPtr window);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll")] private static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr window, int command);
    [DllImport("user32.dll")] private static extern int GetSystemMetrics(int index);
    [DllImport("user32.dll")] private static extern IntPtr GetDC(IntPtr window);
    [DllImport("user32.dll")] private static extern int ReleaseDC(IntPtr window, IntPtr dc);
    [DllImport("gdi32.dll")] private static extern IntPtr CreateCompatibleDC(IntPtr dc);
    [DllImport("gdi32.dll")] private static extern IntPtr CreateCompatibleBitmap(IntPtr dc, int width, int height);
    [DllImport("gdi32.dll")] private static extern IntPtr SelectObject(IntPtr dc, IntPtr obj);
    [DllImport("gdi32.dll")] private static extern bool BitBlt(IntPtr target, int x, int y, int width, int height,
        IntPtr source, int sx, int sy, uint operation);
    [DllImport("gdi32.dll")] private static extern int GetDIBits(IntPtr dc, IntPtr bitmap, uint first, uint count,
        [Out] byte[] pixels, ref BitmapInfo info, uint usage);
    [DllImport("gdi32.dll")] private static extern bool DeleteObject(IntPtr obj);
    [DllImport("gdi32.dll")] private static extern bool DeleteDC(IntPtr dc);

    public static EditorFrame Capture(IntPtr window, uint expectedPid) {
        uint pid;
        GetWindowThreadProcessId(window, out pid);
        if (pid != expectedPid || !IsWindowVisible(window) || IsIconic(window) || GetForegroundWindow() != window)
            throw new Exception("Editor client is not the owned visible foreground window");
        Rect rect;
        var origin = new Point();
        if (!GetClientRect(window, out rect) || !ClientToScreen(window, ref origin))
            throw new Exception("Cannot observe editor client geometry");
        int width = rect.Right - rect.Left, height = rect.Bottom - rect.Top;
        if (width <= 0 || height <= 0 || width > 8192 || height > 8192 || origin.X < GetSystemMetrics(76)
            || origin.Y < GetSystemMetrics(77) || origin.X + width > GetSystemMetrics(76) + GetSystemMetrics(78)
            || origin.Y + height > GetSystemMetrics(77) + GetSystemMetrics(79))
            throw new Exception("Editor client is outside the capturable desktop");
        IntPtr screen = GetDC(IntPtr.Zero), dc = IntPtr.Zero, bitmap = IntPtr.Zero, previous = IntPtr.Zero;
        try {
            if (screen == IntPtr.Zero) throw new Exception("Cannot acquire desktop DC");
            dc = CreateCompatibleDC(screen);
            bitmap = CreateCompatibleBitmap(screen, width, height);
            if (dc == IntPtr.Zero || bitmap == IntPtr.Zero) throw new Exception("Cannot allocate desktop capture");
            previous = SelectObject(dc, bitmap);
            if (previous == IntPtr.Zero || previous == new IntPtr(-1)) throw new Exception("Cannot select desktop bitmap");
            if (!BitBlt(dc, 0, 0, width, height, screen, origin.X, origin.Y, 0x40cc0020))
                throw new Exception("Desktop pixel copy failed");
            SelectObject(dc, previous);
            previous = IntPtr.Zero;
            var frame = new EditorFrame { Left = origin.X, Top = origin.Y, Width = width, Height = height,
                Pixels = new byte[checked(width * height * 4)] };
            var info = new BitmapInfo { Size = 40, Width = width, Height = -height, Planes = 1, BitCount = 32 };
            if (GetDIBits(screen, bitmap, 0, (uint)height, frame.Pixels, ref info, 0) != height)
                throw new Exception("Cannot read complete desktop pixels");
            if (GetForegroundWindow() != window) throw new Exception("Foreground window changed during capture");
            return frame;
        } finally {
            if (previous != IntPtr.Zero) SelectObject(dc, previous);
            if (bitmap != IntPtr.Zero) DeleteObject(bitmap);
            if (dc != IntPtr.Zero) DeleteDC(dc);
            if (screen != IntPtr.Zero) ReleaseDC(IntPtr.Zero, screen);
        }
    }

    private static void Validate(EditorFrame frame) {
        if (frame == null || frame.Width <= 0 || frame.Height <= 0 || frame.Pixels == null
            || frame.Pixels.Length != checked(frame.Width * frame.Height * 4))
            throw new ArgumentException("Invalid client frame");
    }

    private static bool Matches(byte[] pixels, int offset, int[] color) {
        if (color == null || color.Length != 3 || Array.Exists(color, c => c < 0 || c > 255))
            throw new ArgumentException("Invalid RGB colour");
        return Math.Abs(pixels[offset + 2] - color[0]) <= 3 && Math.Abs(pixels[offset + 1] - color[1]) <= 3
            && Math.Abs(pixels[offset] - color[2]) <= 3;
    }

    public static int CountColor(EditorFrame frame, int[] color) {
        Validate(frame);
        int count = 0;
        for (int offset = 0; offset < frame.Pixels.Length; offset += 4)
            if (Matches(frame.Pixels, offset, color)) count++;
        return count;
    }

    public static int CountTransitions(EditorFrame first, EditorFrame second, int[] colorA, int[] colorB) {
        Validate(first); Validate(second);
        if (first.Left != second.Left || first.Top != second.Top || first.Width != second.Width || first.Height != second.Height)
            throw new ArgumentException("Client geometry changed between frames");
        if (colorA == null || colorB == null || colorA.Length != 3 || colorB.Length != 3
            || Math.Abs(colorA[0] - colorB[0]) + Math.Abs(colorA[1] - colorB[1]) + Math.Abs(colorA[2] - colorB[2]) < 64)
            throw new ArgumentException("Paint colours must be distinct");
        int count = 0;
        for (int offset = 0; offset < first.Pixels.Length; offset += 4)
            if (Matches(first.Pixels, offset, colorA) && Matches(second.Pixels, offset, colorB)) count++;
        return count;
    }
}
