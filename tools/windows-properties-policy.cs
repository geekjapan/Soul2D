using System;
using System.Collections.Generic;
using System.Linq;

public sealed class FilePropertyNode {
    public int Id, ParentId, Depth, Pid, Hwnd;
    public string Name, Type, AutomationId, Value;
    public bool Offscreen;
    public double[] Bounds;
    public string[] Patterns;
}

public static class FilePropertiesPolicy {
    private static FilePropertyNode One(IEnumerable<FilePropertyNode> nodes, string description) {
        var found = nodes.ToList();
        if (found.Count != 1) throw new Exception("Expected one " + description);
        return found[0];
    }

    private static bool Rectangle(double[] bounds) {
        return bounds != null && bounds.Length == 4 && bounds.All(value => !Double.IsNaN(value) && !Double.IsInfinity(value))
            && bounds[2] > 0 && bounds[3] > 0;
    }

    private static void Within(double[] bounds, double[] parent) {
        if (!Rectangle(bounds) || !Rectangle(parent) || bounds[0] < parent[0] || bounds[1] < parent[1]
            || bounds[0] + bounds[2] > parent[0] + parent[2] || bounds[1] + bounds[3] > parent[1] + parent[3])
            throw new Exception("Properties evidence has invalid or clipped bounds");
    }

    private static void Visible(FilePropertyNode node, int pid) {
        if (pid <= 0 || node.Pid != pid || node.Offscreen || !Rectangle(node.Bounds))
            throw new Exception("Properties evidence is not visible in the expected helper");
    }

    private static void Tree(IList<FilePropertyNode> nodes, int pid) {
        if (nodes == null || nodes.Count == 0 || nodes.Count > 200) throw new Exception("Invalid Properties snapshot size");
        for (int index = 0; index < nodes.Count; index++) {
            var node = nodes[index];
            if (node == null || node.Id != index || node.Depth < 0 || node.Depth > 8 || node.Name == null
                || node.Type == null || node.AutomationId == null || node.Patterns == null)
                throw new Exception("Malformed Properties snapshot node");
            if (index == 0) {
                if (node.ParentId != -1 || node.Depth != 0 || node.Type != "ControlType.Window")
                    throw new Exception("Invalid Properties snapshot root");
            } else if (node.ParentId < 0 || node.ParentId >= index || node.Depth != nodes[node.ParentId].Depth + 1) {
                throw new Exception("Invalid Properties snapshot parent");
            }
        }
        Visible(nodes[0], pid);
    }

    private static bool Descendant(FilePropertyNode node, FilePropertyNode ancestor, IList<FilePropertyNode> nodes) {
        for (int parent = node.ParentId; parent >= 0; parent = nodes[parent].ParentId)
            if (parent == ancestor.Id) return true;
        return false;
    }

    public static Dictionary<string, object> General(IList<FilePropertyNode> nodes, int pid, string directory, string filename) {
        Tree(nodes, pid);
        if (String.IsNullOrEmpty(directory) || String.IsNullOrEmpty(filename) || !filename.EndsWith(".exe", StringComparison.Ordinal))
            throw new Exception("Invalid expected Properties target");
        var pane = One(nodes.Where(node => node.Type == "ControlType.Pane" && node.Name == "General"), "General pane");
        Visible(pane, pid);
        Within(pane.Bounds, nodes[0].Bounds);
        var values = new List<string>();
        foreach (var field in new[] { new[] { "13156", "File name:" }, new[] { "13059", "Type of file:" }, new[] { "13065", "Location:" } }) {
            var node = One(nodes.Where(row => row.ParentId == pane.Id && row.Type == "ControlType.Edit"
                && row.AutomationId == field[0] && row.Name == field[1]), "General " + field[1]);
            Visible(node, pid);
            Within(node.Bounds, pane.Bounds);
            if (node.Value == null || !node.Patterns.Contains("ValuePatternIdentifiers.Pattern"))
                throw new Exception("General target field lacks its observed value pattern");
            values.Add(node.Value);
        }
        if (values[0] != filename && values[0] != filename.Substring(0, filename.Length - 4))
            throw new Exception("Properties displays another filename");
        if (values[1] != "Application (.exe)" || !String.Equals(values[2].TrimEnd('\\'), directory.TrimEnd('\\'), StringComparison.OrdinalIgnoreCase))
            throw new Exception("Properties displays another file type or directory");
        return new Dictionary<string, object> { {"filename", values[0]}, {"type", values[1]}, {"directory", values[2]},
            {"expectedFilename", filename}, {"expectedDirectory", directory} };
    }

    public static Dictionary<string, object> Copyright(IList<FilePropertyNode> nodes, int pid, string expected, double[] capture) {
        Tree(nodes, pid);
        if (String.IsNullOrEmpty(expected) || !expected.Contains("\u00a9")) throw new Exception("Expected copyright lacks its glyph");
        var pane = One(nodes.Where(node => node.Type == "ControlType.Pane" && node.Name == "Details"), "Details pane");
        var grid = One(nodes.Where(node => node.Type == "ControlType.DataGrid" && node.Name == "Properties"
            && node.AutomationId == "271" && Descendant(node, pane, nodes)), "Details Properties grid");
        var group = One(nodes.Where(node => node.ParentId == grid.Id && node.Type == "ControlType.Group" && node.Name == "Description"), "Description group");
        var row = One(nodes.Where(node => node.ParentId == group.Id && node.Type == "ControlType.DataItem" && node.Name == "Copyright"), "Copyright row");
        var cells = nodes.Where(node => node.ParentId == row.Id).ToList();
        if (cells.Count != 2 || cells.Any(node => node.Type != "ControlType.Text") || cells[0].Name != "Copyright" || cells[1].Name != expected)
            throw new Exception("Copyright row label/value association differs");
        foreach (var node in new[] { pane, grid, group, row, cells[0], cells[1] }) Visible(node, pid);
        Within(pane.Bounds, nodes[0].Bounds);
        Within(grid.Bounds, pane.Bounds);
        Within(group.Bounds, grid.Bounds);
        Within(row.Bounds, group.Bounds);
        Within(row.Bounds, grid.Bounds);
        Within(grid.Bounds, capture);
        foreach (var cell in cells) {
            if (!cell.Patterns.Contains("GridItemPatternIdentifiers.Pattern") || !cell.Patterns.Contains("TableItemPatternIdentifiers.Pattern"))
                throw new Exception("Copyright cell lacks its observed grid patterns");
            Within(cell.Bounds, row.Bounds);
            Within(cell.Bounds, capture);
        }
        var relative = new[] { cells[1].Bounds[0] - capture[0], cells[1].Bounds[1] - capture[1], cells[1].Bounds[2], cells[1].Bounds[3] };
        return new Dictionary<string, object> { {"label", cells[0].Name}, {"value", cells[1].Name},
            {"rowBounds", row.Bounds}, {"valueBounds", cells[1].Bounds}, {"valueCaptureBounds", relative},
            {"glyphInspection", "REQUIRES_CAPTURE_REVIEW"} };
    }
}
