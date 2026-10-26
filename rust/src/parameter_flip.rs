//! Parameter panel features that need more than one binding: parameter links, the flip-pair based
//! mirroring (Mirrored Autofill, Set from mirror, Flip / Symmetrize Deform, Paste and Duplicate with flip),
//! the binding and parameter clipboards, "Copy to" / "Swap with" and "To 2D".
//!
//! Port of commands/parameter/{base,paramedit}.d, commands/binding/{base,binding}.d and
//! utils/transform.d (`incBindingGetPairFor`, `incBindingAutoFlip`). Every function edits a candidate
//! model; the session commits it as one undo step.
//!
//! Flip pairs are pairs of node UUIDs stored in the document's extension data under [`FLIP_CONFIG_KEY`].
//!
//! Not ported: the deformation transfer of a PathDeformer (its Bezier / spline `closestPoint` has no Rust
//! counterpart): those bindings are skipped and reported in `skipped`.

use crate::binding_grid::{Axes, Binding, Cell};
use crate::parameter_edit::{self, container_of, locate_plain, slot_index, BindingKey, Slot};
use crate::{deformers, geometry, nodes};
use serde_json::{json, Value};
use std::collections::HashMap;

pub(crate) type Pair = (u32, u32);
pub(crate) type ExtensionEntry = (Vec<u8>, Vec<u8>);

/// D stores flip pairs in the puppet's extension data under this name.
pub const FLIP_CONFIG_KEY: &str = "com.inochi2d.creator.FlipConfig";

/// The pairs that hold two existing nodes (`incGetFlipPairFor` ignores the others).
pub(crate) fn read_pairs(entries: &[ExtensionEntry], model: &Value) -> Result<Vec<Pair>, String> {
    let Some((_, payload)) = entries.iter().find(|(name, _)| name == FLIP_CONFIG_KEY.as_bytes()) else {
        return Ok(Vec::new());
    };
    if payload.is_empty() {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_slice(payload).map_err(|error| format!("FlipConfig is not JSON: {error}"))?;
    let items = value.as_array().ok_or("FlipConfig is not an array")?;
    let mut pairs = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let uuid = |key: &str| {
            item.get(key)
                .and_then(Value::as_u64)
                .and_then(|uuid| u32::try_from(uuid).ok())
                .ok_or_else(|| format!("FlipConfig entry {index} has no valid {key}"))
        };
        let (a, b) = (uuid("uuid1")?, uuid("uuid2")?);
        if nodes::find_node(model, a).is_some() && nodes::find_node(model, b).is_some() {
            pairs.push((a, b));
        }
    }
    Ok(pairs)
}

/// The node paired with `node` by the first pair that holds it.
fn partner(pairs: &[Pair], node: u32) -> Option<u32> {
    pairs.iter().find_map(|&(a, b)| {
        if a == node {
            Some(b)
        } else if b == node {
            Some(a)
        } else {
            None
        }
    })
}

fn is_node(model: &Value, id: u32) -> bool {
    nodes::find_node(model, id).is_some()
}

// ------------------------------------------------------------------------------------------ geometry

/// What a deformation binding of a node moves.
enum Geometry {
    Mesh { vertices: Vec<[f64; 2]>, indices: Vec<u32> },
    Grid { points: Vec<[f64; 2]> },
    Path { count: usize },
}

impl Geometry {
    fn count(&self) -> usize {
        match self {
            Geometry::Mesh { vertices, .. } => vertices.len(),
            Geometry::Grid { points } => points.len(),
            Geometry::Path { count } => *count,
        }
    }
}

const PART_TYPES: [&str; 5] = ["Part", "Composite", "DynamicComposite", "Mask", "AnimatedPart"];

fn node_type(node: &Value) -> &str {
    node.get("type").and_then(Value::as_str).unwrap_or("Node")
}

/// The geometry of a `Deformable` node; `None` for any other node or an unknown id.
fn geometry_of(model: &Value, id: u32) -> Result<Option<Geometry>, String> {
    let Some(node) = nodes::find_node(model, id) else { return Ok(None) };
    let kind = node_type(node);
    Ok(if PART_TYPES.contains(&kind) || kind == "MeshGroup" {
        let mesh = deformers::parse_part_mesh(node)?;
        let indices = mesh.indices.iter().map(|index| *index as u32).collect();
        Some(Geometry::Mesh { vertices: mesh.vertices, indices })
    } else if kind == "GridDeformer" {
        Some(Geometry::Grid { points: deformers::node_geometry_points(node) })
    } else if kind == "PathDeformer" {
        Some(Geometry::Path { count: deformers::node_geometry_points(node).len() })
    } else {
        None
    })
}

fn pairs_of(flat: &[f64]) -> Vec<[f64; 2]> {
    flat.chunks(2).map(|pair| [pair[0], pair[1]]).collect()
}

fn is_close(a: f64, b: f64) -> bool {
    (a - b).abs() <= (1e-4 * a.abs().max(b.abs())).max(1e-5)
}

/// `locateInterval` of the GridDeformer branch of `deformByDeformationBinding`.
fn locate_interval(axis: &[f64], value: f64) -> Option<(usize, f64)> {
    if axis.len() < 2 {
        return None;
    }
    let last = axis[axis.len() - 1];
    if value <= axis[0] {
        return is_close(value, axis[0]).then_some((0, 0.0));
    }
    if value >= last {
        return is_close(value, last).then_some((axis.len() - 2, 1.0));
    }
    for i in 0..axis.len() - 1 {
        let (a, b) = (axis[i], axis[i + 1]);
        if value >= a && value <= b {
            let span = b - a;
            let weight = if span > 0.0 { ((value - a) / span).clamp(0.0, 1.0) } else { 0.0 };
            return Some((i, weight));
        }
    }
    None
}

/// `deformByDeformationBinding` for a GridDeformer source: `points` are sampled on the deformed grid
/// (mirrored in x when `flip`), zero offsets when the grid is not a full lattice.
fn transfer_grid(points: &[[f64; 2]], grid: &[[f64; 2]], offsets: &[[f64; 2]], flip: bool) -> Vec<[f64; 2]> {
    let zeros = vec![[0.0, 0.0]; points.len()];
    if grid.len() < 4 || offsets.len() != grid.len() {
        return zeros;
    }
    let sorted = |axis: usize| {
        let mut values: Vec<f64> = grid.iter().map(|point| point[axis]).collect();
        values.sort_by(f64::total_cmp);
        values.dedup();
        values
    };
    let (xs, ys) = (sorted(0), sorted(1));
    let (cols, rows) = (xs.len(), ys.len());
    if cols < 2 || rows < 2 || cols * rows != grid.len() {
        return zeros;
    }
    points
        .iter()
        .map(|original| {
            let query = if flip { [-original[0], original[1]] } else { *original };
            let (Some((cx, u)), Some((cy, v))) = (locate_interval(&xs, query[0]), locate_interval(&ys, query[1]))
            else {
                return [0.0, 0.0];
            };
            let corner = |row: usize, col: usize| {
                let slot = row * cols + col;
                [grid[slot][0] + offsets[slot][0], grid[slot][1] + offsets[slot][1]]
            };
            let (p00, p10, p01, p11) = (corner(cy, cx), corner(cy, cx + 1), corner(cy + 1, cx), corner(cy + 1, cx + 1));
            let mut moved = [0, 1].map(|k| {
                let a = p00[k] * (1.0 - u) + p10[k] * u;
                let b = p01[k] * (1.0 - u) + p11[k] * u;
                a * (1.0 - v) + b * v
            });
            if flip {
                moved[0] = -moved[0];
            }
            [moved[0] - original[0], moved[1] - original[1]]
        })
        .collect()
}

/// `deformByDeformationBinding(dest, src, deform, flip)`: the offsets for the vertices of `dest` that follow
/// the deformation `offsets` of `src`. `None` when the pair of node kinds has no transfer.
fn transfer(dest: &Geometry, src: &Geometry, offsets: &[f64], flip: bool) -> Result<Option<Cell>, String> {
    let offsets = pairs_of(offsets);
    let moved = match (dest, src) {
        (Geometry::Mesh { vertices, .. }, Geometry::Mesh { vertices: source, indices }) => {
            geometry::transfer_mesh_offsets(vertices, source, indices, &offsets, flip)?
        }
        (Geometry::Grid { points }, Geometry::Grid { points: source }) => {
            transfer_grid(points, source, &offsets, flip)
        }
        _ => return Ok(None),
    };
    Ok(Some(moved.iter().flatten().copied().collect()))
}

// ------------------------------------------------------------------------------------------ work set

/// One parameter with its bindings in typed form.
struct Work {
    axes: Axes,
    min: [f64; 2],
    max: [f64; 2],
    list: Vec<Binding>,
}

fn pair_field(parameter: &Value, key: &str, fallback: [f64; 2]) -> Result<[f64; 2], String> {
    match parameter.get(key) {
        None => Ok(fallback),
        Some(value) => crate::parse_pair(value, key),
    }
}

impl Work {
    fn load(model: &Value, parameter: &Value) -> Result<Work, String> {
        let axes = Axes::of(parameter)?;
        let defaults = parameter_edit::parameter_defaults(model);
        let raw = parameter
            .get("bindings")
            .and_then(Value::as_array)
            .ok_or("parameter.bindings must be an array")?;
        let mut list = Vec::with_capacity(raw.len());
        for binding in raw {
            let channel =
                binding.get("param_name").and_then(Value::as_str).ok_or("binding.param_name must be a string")?;
            let node = binding.get("node").and_then(Value::as_u64).and_then(|id| u32::try_from(id).ok()).unwrap_or(0);
            let vertices = if channel == "deform" {
                geometry_of(model, node)?.map(|geometry| geometry.count())
            } else {
                None
            };
            let default = parameter_edit::neutral_of(binding, channel, &defaults);
            list.push(Binding::parse(binding, &axes, default, vertices)?);
        }
        Ok(Work {
            axes,
            min: pair_field(parameter, "min", [0.0, 0.0])?,
            max: pair_field(parameter, "max", [1.0, 1.0])?,
            list,
        })
    }

    fn store(&self, parameter: &mut Value) -> Result<(), String> {
        let bindings = self
            .list
            .iter()
            .map(|binding| if binding.touched { binding.to_json() } else { Ok(binding.raw.clone()) })
            .collect::<Result<Vec<_>, _>>()?;
        parameter["bindings"] = Value::Array(bindings);
        Ok(())
    }

    /// `Parameter.mapValue` for one axis, clamped to 0..1.
    fn map_axis(&self, axis: usize, value: f64) -> f64 {
        let range = self.max[axis] - self.min[axis];
        if range == 0.0 {
            0.0
        } else {
            ((value - self.min[axis]) / range).clamp(0.0, 1.0)
        }
    }

    /// `Parameter.unmapValue` for one axis.
    fn unmap_axis(&self, axis: usize, offset: f64) -> f64 {
        (self.max[axis] - self.min[axis]) * offset + self.min[axis]
    }

    /// `Parameter.findClosestKeypoint(value)`.
    fn closest_keypoint(&self, value: [f64; 2]) -> [usize; 2] {
        [
            Axes::closest(&self.axes.x, self.map_axis(0, value[0])),
            Axes::closest(&self.axes.y, self.map_axis(1, value[1])),
        ]
    }

    /// The reset `createBinding` runs at the keypoint nearest the physical origin when that is a real zero.
    fn zero_key(&self, binding: &mut Binding) -> Result<(), String> {
        let index = self.closest_keypoint([0.0, 0.0]);
        let offset = self.axes.offset(index)?;
        let zero = [self.unmap_axis(0, offset[0]), self.unmap_axis(1, offset[1])];
        if zero[0].abs() < 0.001 && zero[1].abs() < 0.001 {
            binding.reset(&self.axes, index)?;
        }
        Ok(())
    }

    /// `getOrAddBinding`: the index of the binding for `node` / `channel`, created when missing. A node
    /// binding of "deform" needs a `Deformable` node; `None` is returned (and nothing added) otherwise.
    fn ensure(
        &mut self,
        model: &Value,
        defaults: &HashMap<u32, [f64; 2]>,
        node: u32,
        channel: &str,
    ) -> Result<Option<usize>, String> {
        if let Some(found) = self.list.iter().position(|b| b.node == node && b.channel == channel) {
            return Ok(Some(found));
        }
        let mut binding = if let Some(target) = defaults.get(&node).filter(|_| !is_node(model, node)) {
            let axis = match channel {
                "X" => 0,
                "Y" => 1,
                _ => return Err(format!("{channel:?} is not a parameter link channel")),
            };
            Binding::blank(node, channel, &self.axes, target[axis], 0)
        } else {
            if !is_node(model, node) {
                return Err(format!("node {node} was not found"));
            }
            let vertices = if channel == "deform" {
                match geometry_of(model, node)? {
                    Some(geometry) => geometry.count(),
                    None => return Ok(None),
                }
            } else {
                0
            };
            Binding::blank(node, channel, &self.axes, parameter_edit::channel_default(channel), vertices)
        };
        self.zero_key(&mut binding)?;
        self.list.push(binding);
        Ok(Some(self.list.len() - 1))
    }

    /// `incBindingGetPairFor` with `forceCreate`: the binding of the partner node (the node itself without
    /// a pair) for `channel`.
    fn pair_binding(
        &mut self,
        model: &Value,
        defaults: &HashMap<u32, [f64; 2]>,
        partner: Option<u32>,
        node: u32,
        channel: &str,
    ) -> Result<Option<usize>, String> {
        self.ensure(model, defaults, partner.unwrap_or(node), channel)
    }

    /// The bindings an action applies to: all of them, or the listed ones.
    fn select(&self, only: Option<&[BindingKey]>) -> Result<Vec<usize>, String> {
        let indexes: Vec<usize> = match only {
            None => (0..self.list.len()).collect(),
            Some(keys) => {
                for (node, channel) in keys {
                    if !self.list.iter().any(|b| b.node == *node && &b.channel == channel) {
                        return Err(format!("parameter has no {channel} binding on node {node}"));
                    }
                }
                (0..self.list.len())
                    .filter(|i| {
                        let b = &self.list[*i];
                        keys.iter().any(|(node, channel)| b.node == *node && &b.channel == channel)
                    })
                    .collect()
            }
        };
        if indexes.is_empty() {
            return Err("parameter has no bindings to edit".into());
        }
        Ok(indexes)
    }

    fn check_key(&self, key: [usize; 2]) -> Result<(), String> {
        let (x, y) = self.axes.counts();
        if key[0] >= x || key[1] >= y {
            return Err("key must identify an existing parameter keypoint index".into());
        }
        Ok(())
    }
}

struct Opened {
    slot: Slot,
    parameter: Value,
}

fn open(model: &mut Value, id: u32) -> Result<Opened, String> {
    let slot = locate_plain(model, id)?;
    let parameter = container_of(model, slot)?[slot_index(slot)].clone();
    Ok(Opened { slot, parameter })
}

fn close(model: &mut Value, opened: &Opened, work: &Work) -> Result<Value, String> {
    let target = &mut container_of(model, opened.slot)?[slot_index(opened.slot)];
    work.store(target)?;
    Ok(target.clone())
}

// ------------------------------------------------------------------------------------------ flipping

/// `incBindingAutoFlip`'s computation: the cell `dest` receives at `index` from `src`. `axis` is -1
/// (diagonal), 0 or 1 (mirrored over that axis) or 2 (as is). With `extrapolation` the source is read at the
/// mirrored keypoint, without it at the same keypoint of the source parameter. `None`: nothing to do.
fn flipped_cell(
    model: &Value,
    dest: &Binding,
    src: &Binding,
    src_axes: &Axes,
    index: [usize; 2],
    axis: i32,
    extrapolation: bool,
) -> Result<Option<Cell>, String> {
    if dest.deform != src.deform {
        return Ok(None);
    }
    let mut offset = src_axes.offset(index)?;
    if extrapolation {
        match axis {
            -1 => offset = [1.0 - offset[0], 1.0 - offset[1]],
            0 => offset[0] = 1.0 - offset[0],
            1 => offset[1] = 1.0 - offset[1],
            2 => {}
            _ => return Err("bad axis".into()),
        }
    }
    let (left, sub) = src_axes.find_offset(offset);
    let cell = src.interpolate(src_axes, left, sub)?;
    if !dest.deform {
        return Ok(Some(cell));
    }
    // D mirrors the mesh in x whenever it extrapolates, whatever the axis.
    let flip = extrapolation || axis < 1;
    match (geometry_of(model, dest.node)?, geometry_of(model, src.node)?) {
        (Some(to), Some(from)) => transfer(&to, &from, &cell, flip),
        _ => Ok(None),
    }
}

fn apply_flip(dest: &mut Binding, axes: &Axes, cell: Cell, index: [usize; 2], axis: i32) -> Result<(), String> {
    dest.set_value(axes, index, cell)?;
    if !dest.deform && axis < 2 {
        let negated = dest.negated(index, axis);
        dest.set_value(axes, index, negated)?;
    }
    Ok(())
}

fn note_skipped(skipped: &mut Vec<Value>, binding: &Binding) {
    let entry = json!({"node": binding.node, "param_name": binding.channel});
    if !skipped.contains(&entry) {
        skipped.push(entry);
    }
}

/// `mirroredAutofill` for the four directions of the Mirrored Autofill menu: 1 and 2 fill the low and high
/// half of the X axis, 3 and 4 the high and low half of the Y axis, each from the mirrored half.
pub(crate) fn mirrored_autofill(
    model: &mut Value,
    pairs: &[Pair],
    id: u32,
    direction: u8,
) -> Result<Value, String> {
    let (axis, low, high) = match direction {
        1 => (0, 0.0, 0.4999),
        2 => (0, 0.5001, 1.0),
        3 => (1, 0.5001, 1.0),
        4 => (1, 0.0, 0.4999),
        _ => return Err("direction must be 1, 2, 3 or 4".into()),
    };
    let opened = open(model, id)?;
    let defaults = parameter_edit::parameter_defaults(model);
    let mut work = Work::load(model, &opened.parameter)?;
    if axis == 1 && !work.axes.is_vec2 {
        return Err("directions 3 and 4 need a 2D parameter".into());
    }
    let axes = work.axes.clone();
    let (x_count, y_count) = axes.counts();
    let initial = work.list.len();
    let (mut filled, mut skipped) = (0, Vec::new());
    for source in 0..initial {
        let (node, channel) = (work.list[source].node, work.list[source].channel.clone());
        if !is_node(model, node) {
            continue;
        }
        let Some(target) = work.pair_binding(model, &defaults, partner(pairs, node), node, &channel)? else {
            continue;
        };
        for x in 0..x_count {
            if axis == 0 && (axes.x[x] < low || axes.x[x] > high) {
                continue;
            }
            for y in 0..y_count {
                if axis == 1 && (axes.y[y] < low || axes.y[y] > high) {
                    continue;
                }
                if work.list[target].is_set([x, y]) {
                    continue;
                }
                match flipped_cell(model, &work.list[target], &work.list[source], &axes, [x, y], axis, true)? {
                    Some(cell) => {
                        apply_flip(&mut work.list[target], &axes, cell, [x, y], axis)?;
                        filled += 1;
                    }
                    None => note_skipped(&mut skipped, &work.list[source]),
                }
            }
        }
    }
    close(model, &opened, &work)?;
    Ok(json!({"id": id, "direction": direction, "filled": filled, "skipped": skipped}))
}

/// Four-corner form synthesis (issue #180, PAR15) on 2D parameter `id`: every key off the two lines through the
/// reference key (the key nearest the parameter's default) becomes the keys on those lines in its column and row
/// minus the reference, `cell(x, y) = cell(x, ry) + cell(rx, y) - cell(rx, ry)`, overwriting what it held. The
/// manual gives no formula, so this rule is inferred, not compared. A binding without every key on the two lines
/// set is skipped.
pub(crate) fn synthesize_corners(model: &mut Value, id: u32) -> Result<Value, String> {
    let opened = open(model, id)?;
    let mut work = Work::load(model, &opened.parameter)?;
    if !work.axes.is_vec2 {
        return Err("corner synthesis needs a 2D parameter".into());
    }
    let [rx, ry] = work.closest_keypoint(pair_field(&opened.parameter, "defaults", [0.0, 0.0])?);
    let axes = work.axes.clone();
    let (x_count, y_count) = axes.counts();
    let (mut written, mut skipped) = (0, Vec::new());
    for binding in &mut work.list {
        if !(0..x_count).all(|x| binding.is_set([x, ry])) || !(0..y_count).all(|y| binding.is_set([rx, y])) {
            note_skipped(&mut skipped, binding);
            continue;
        }
        let mut cells = Vec::new();
        for x in (0..x_count).filter(|x| *x != rx) {
            for y in (0..y_count).filter(|y| *y != ry) {
                let (column, row, reference) = (&binding.values[x][ry], &binding.values[rx][y], &binding.values[rx][ry]);
                let sum = column.iter().zip(row).zip(reference).map(|((a, b), c)| a + b - c).collect();
                cells.push(([x, y], sum));
            }
        }
        for (index, cell) in cells {
            binding.set_value(&axes, index, cell)?;
            written += 1;
        }
        binding.touched = true;
    }
    close(model, &opened, &work)?;
    Ok(json!({"id": id, "reference": [rx, ry], "written": written, "skipped": skipped}))
}

/// `pasteParameter`: the bindings of `source` (a stored parameter, not necessarily in the model) are
/// written to every keypoint of parameter `id`. `axis` 0 pastes onto the partner nodes mirrored, 2 as is.
pub(crate) fn paste_parameter(
    model: &mut Value,
    pairs: &[Pair],
    id: u32,
    source: &Value,
    axis: i32,
) -> Result<Value, String> {
    let opened = open(model, id)?;
    let defaults = parameter_edit::parameter_defaults(model);
    let mut work = Work::load(model, &opened.parameter)?;
    let source_work = Work::load(model, source)?;
    let axes = work.axes.clone();
    let (x_count, y_count) = axes.counts();
    let mut skipped = Vec::new();
    for src in &source_work.list {
        if !is_node(model, src.node) {
            continue;
        }
        let pair = if axis != 2 { partner(pairs, src.node) } else { None };
        let Some(target) = work.pair_binding(model, &defaults, pair, src.node, &src.channel)? else { continue };
        for x in 0..x_count {
            for y in 0..y_count {
                match flipped_cell(model, &work.list[target], src, &source_work.axes, [x, y], axis, false)? {
                    Some(cell) => apply_flip(&mut work.list[target], &axes, cell, [x, y], axis)?,
                    None => note_skipped(&mut skipped, src),
                }
            }
        }
    }
    let mut result = close(model, &opened, &work)?;
    result["skipped"] = json!(skipped);
    Ok(result)
}

/// `DuplicateParameterWithFlip`: a copy of the parameter without bindings, filled by pasting the original
/// with the horizontal flip.
pub(crate) fn duplicate_with_flip(
    model: &mut Value,
    pairs: &[Pair],
    id: u32,
    new_id: u32,
) -> Result<Value, String> {
    let source = open(model, id)?;
    let base = source.parameter.get("name").and_then(Value::as_str).unwrap_or("").to_owned();
    let name = parameter_edit::free_name(model, &format!("{base} (Copy)"), |n| format!("{base} (Copy {n})"));
    let mut copy = source.parameter.clone();
    copy["uuid"] = json!(new_id);
    copy["name"] = json!(name);
    copy["bindings"] = json!([]);
    container_of(model, source.slot)?.push(copy);
    paste_parameter(model, pairs, new_id, &source.parameter, 0)
}

/// What a "Set from mirror" menu entry does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MirrorSource {
    Horizontal,
    Vertical,
    Diagonal,
    OneD,
}

/// `SetFromHorizontalMirror` and friends: each chosen binding takes the value of its partner's binding
/// at the mirrored keypoint (`OneD` with explicit bindings pushes the other way, as D does).
pub(crate) fn set_from_mirror(
    model: &mut Value,
    pairs: &[Pair],
    id: u32,
    key: [usize; 2],
    mode: MirrorSource,
    only: Option<&[BindingKey]>,
) -> Result<usize, String> {
    let opened = open(model, id)?;
    let defaults = parameter_edit::parameter_defaults(model);
    let mut work = Work::load(model, &opened.parameter)?;
    work.check_key(key)?;
    let axes = work.axes.clone();
    let axis = match mode {
        MirrorSource::Horizontal | MirrorSource::OneD => 0,
        MirrorSource::Vertical => {
            if !axes.is_vec2 {
                return Err("vertical mirroring needs a 2D parameter".into());
            }
            1
        }
        MirrorSource::Diagonal => {
            if !axes.is_vec2 {
                return Err("diagonal mirroring needs a 2D parameter".into());
            }
            -1
        }
    };
    let chosen = work.select(only)?;
    let mut edited = 0;
    for selected in chosen {
        let (node, channel) = (work.list[selected].node, work.list[selected].channel.clone());
        if !is_node(model, node) {
            continue;
        }
        let other = work.pair_binding(model, &defaults, partner(pairs, node), node, &channel)?;
        let (dest, src) = match (mode, other) {
            (MirrorSource::OneD, Some(other)) if only.is_some() => (other, selected),
            (MirrorSource::OneD, None) if only.is_some() => continue,
            (_, other) => (selected, other.unwrap_or(selected)),
        };
        if let Some(cell) = flipped_cell(model, &work.list[dest], &work.list[src], &axes, key, axis, true)? {
            apply_flip(&mut work.list[dest], &axes, cell, key, axis)?;
            edited += 1;
        }
    }
    close(model, &opened, &work)?;
    Ok(edited)
}

/// `FlipDeformCommand` (`symmetrize` false) and `SymmetrizeDeformCommand` (`symmetrize` true): the stored
/// deformation of the key is mirrored in x through the node's own mesh, or averaged with its mirror.
pub(crate) fn flip_deform(
    model: &mut Value,
    id: u32,
    key: [usize; 2],
    symmetrize: bool,
    only: Option<&[BindingKey]>,
) -> Result<(usize, Vec<Value>), String> {
    let opened = open(model, id)?;
    let mut work = Work::load(model, &opened.parameter)?;
    work.check_key(key)?;
    let axes = work.axes.clone();
    let chosen = work.select(only)?;
    let (mut edited, mut skipped) = (0, Vec::new());
    for index in chosen {
        if !work.list[index].deform {
            continue;
        }
        let Some(geometry) = geometry_of(model, work.list[index].node)? else { continue };
        let original = work.list[index].value(key).clone();
        let Some(mut flipped) = transfer(&geometry, &geometry, &original, true)? else {
            note_skipped(&mut skipped, &work.list[index]);
            continue;
        };
        if symmetrize {
            if flipped.len() != original.len() {
                continue;
            }
            for (new, old) in flipped.iter_mut().zip(&original) {
                *new = (*old + *new) / 2.0;
            }
        }
        work.list[index].set_value(&axes, key, flipped)?;
        edited += 1;
    }
    close(model, &opened, &work)?;
    Ok((edited, skipped))
}

// ------------------------------------------------------------------------------------------ clipboards

/// One binding at the copied keypoint: `cClipboardBindings` keeps the live binding, the session keeps what it
/// held when copied.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ClipEntry {
    pub node: u32,
    pub channel: String,
    pub set: bool,
    pub cell: Cell,
    pub deform: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BindingClipboard {
    pub parameter: u32,
    pub key: [usize; 2],
    pub entries: Vec<ClipEntry>,
}

impl BindingClipboard {
    pub(crate) fn summary(&self) -> Value {
        json!({
            "parameter": self.parameter,
            "key": self.key,
            "bindings": self.entries.iter()
                .map(|e| json!({"node": e.node, "param_name": e.channel, "set": e.set}))
                .collect::<Vec<_>>(),
        })
    }
}

/// `CopyBindingCommand`.
pub(crate) fn copy_bindings(
    model: &mut Value,
    id: u32,
    key: [usize; 2],
    only: Option<&[BindingKey]>,
) -> Result<BindingClipboard, String> {
    let opened = open(model, id)?;
    let work = Work::load(model, &opened.parameter)?;
    work.check_key(key)?;
    let entries = work
        .select(only)?
        .into_iter()
        .map(|i| {
            let b = &work.list[i];
            ClipEntry {
                node: b.node,
                channel: b.channel.clone(),
                set: b.is_set(key),
                cell: b.value(key).clone(),
                deform: b.deform,
            }
        })
        .collect();
    Ok(BindingClipboard { parameter: id, key, entries })
}

/// Paste Form options. `ratio` (Blend Form, #178) mixes the copied form into the current one: current + ratio
/// × (copied − current). `invert` (Paste Form Special, #179) mirrors a pasted vertex shape about a vertical
/// (axis 0) or horizontal (axis 1) line, by default through the centre of the node's rest bounds, negates a
/// pasted `transform.r.z` angle, and reflects a pasted `transform.t.x` (or `t.y`) position about `origin` in the
/// parent's space (default 0), which is Reflect's "flip the rotation deformer position" (#173).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PasteOptions {
    pub ratio: f64,
    pub invert: Option<(usize, Option<f64>)>,
}

impl Default for PasteOptions {
    fn default() -> Self {
        PasteOptions { ratio: 1.0, invert: None }
    }
}

fn apply_entry(
    model: &Value,
    binding: &mut Binding,
    axes: &Axes,
    key: [usize; 2],
    set: bool,
    cell: Cell,
    options: PasteOptions,
) -> Result<(), String> {
    if !set && options == PasteOptions::default() {
        return binding.unset(axes, key);
    }
    let mut cell = cell;
    if let Some((axis, origin)) = options.invert {
        if binding.deform {
            let node = nodes::find_node(model, binding.node).ok_or("the target node was not found")?;
            let rest = deformers::node_geometry_points(node);
            if rest.len() * 2 != cell.len() {
                return Err(format!("node {} has {} vertices and the form {}", binding.node, rest.len(), cell.len() / 2));
            }
            let origin = origin.unwrap_or_else(|| {
                let (low, high) = rest.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), point| {
                    (low.min(point[axis]), high.max(point[axis]))
                });
                (low + high) / 2.0
            });
            for (index, point) in rest.iter().enumerate() {
                let slot = index * 2 + axis;
                cell[slot] = 2.0 * origin - 2.0 * point[axis] - cell[slot];
            }
        } else if binding.channel == "transform.r.z" {
            cell.iter_mut().for_each(|value| *value = -*value);
        } else if binding.channel == ["transform.t.x", "transform.t.y"][axis] {
            let node = nodes::find_node(model, binding.node).ok_or("the target node was not found")?;
            let rest = node["transform"]["trans"][axis].as_f64().unwrap_or(0.0);
            let origin = origin.unwrap_or(0.0);
            cell.iter_mut().for_each(|value| *value = 2.0 * origin - 2.0 * rest - *value);
        }
    }
    if options.ratio != 1.0 {
        let current = &binding.values[key[0]][key[1]];
        cell = current.iter().zip(&cell).map(|(now, copied)| now + options.ratio * (copied - now)).collect();
    }
    binding.set_value(axes, key, cell)
}

/// `PasteBindingCommand`: with explicit bindings every selected binding that is on the clipboard receives its
/// copy (or, for one binding against one clipboard entry, that entry); without, every clipboard entry is
/// pasted and missing bindings are created.
pub(crate) fn paste_bindings(
    model: &mut Value,
    clipboard: &BindingClipboard,
    id: u32,
    key: [usize; 2],
    only: Option<&[BindingKey]>,
    options: PasteOptions,
) -> Result<usize, String> {
    if !(0.0..=1.0).contains(&options.ratio) {
        return Err("ratio must be between 0 and 1".into());
    }
    let opened = open(model, id)?;
    let defaults = parameter_edit::parameter_defaults(model);
    let mut work = Work::load(model, &opened.parameter)?;
    work.check_key(key)?;
    let axes = work.axes.clone();
    let mut applied = 0;
    if only.is_some() {
        let chosen = work.select(only)?;
        for &index in &chosen {
            let (node, channel) = (work.list[index].node, work.list[index].channel.clone());
            if let Some(entry) = clipboard.entries.iter().find(|e| e.node == node && e.channel == channel) {
                apply_entry(model, &mut work.list[index], &axes, key, entry.set, entry.cell.clone(), options)?;
                applied += 1;
            }
        }
        if applied == 0 && chosen.len() == 1 && clipboard.entries.len() == 1 {
            let entry = &clipboard.entries[0];
            let dest = &mut work.list[chosen[0]];
            if dest.deform && entry.deform {
                // D reads the source at the paste keypoint here; the clipboard only holds the copied one.
                let (Some(to), Some(from)) = (geometry_of(model, dest.node)?, geometry_of(model, entry.node)?)
                else {
                    return Err("the clipboard node or the target node was not found".into());
                };
                if let Some(cell) = transfer(&to, &from, &entry.cell, false)? {
                    apply_entry(model, dest, &axes, key, true, cell, options)?;
                    applied += 1;
                }
            } else if !dest.deform && !entry.deform {
                apply_entry(model, dest, &axes, key, true, entry.cell.clone(), options)?;
                applied += 1;
            }
        }
    } else {
        for entry in &clipboard.entries {
            let Some(index) = work.ensure(model, &defaults, entry.node, &entry.channel)? else { continue };
            apply_entry(model, &mut work.list[index], &axes, key, entry.set, entry.cell.clone(), options)?;
            applied += 1;
        }
    }
    close(model, &opened, &work)?;
    Ok(applied)
}

pub(crate) fn node_has_param(kind: &str, key: &str) -> bool {
    let base = matches!(
        key,
        "zSort" | "transform.t.x" | "transform.t.y" | "transform.t.z"
            | "transform.r.x" | "transform.r.y" | "transform.r.z" | "transform.s.x" | "transform.s.y"
    );
    let part = matches!(
        key,
        "alphaThreshold" | "opacity" | "tint.r" | "tint.g" | "tint.b" | "screenTint.r" | "screenTint.g"
            | "screenTint.b" | "emissionStrength"
    );
    let physics = matches!(
        key,
        "gravity" | "length" | "frequency" | "angleDamping" | "lengthDamping" | "outputScale.x" | "outputScale.y"
    );
    base || (part && PART_TYPES.contains(&kind)) || (physics && kind == "SimplePhysics")
}

/// `isCompatibleWithNode`: a value binding fits nodes that have its channel, a deformation fits a deformable
/// node with the same vertex count, a parameter link fits no node.
fn compatible(model: &Value, binding: &Binding, target: u32) -> Result<bool, String> {
    let Some(node) = nodes::find_node(model, target) else { return Ok(false) };
    if binding.deform {
        return Ok(geometry_of(model, target)?.is_some_and(|geometry| geometry.count() * 2 == binding.width));
    }
    Ok(node_has_param(node_type(node), &binding.channel))
}

/// The nodes "Copy to" / "Swap with" offer (`getCompatibleNodes`): nodes bound in the parameter, other than
/// the one the selection is on, that every selected binding fits. Unlike D, whose loop never appends, a node
/// that fits is listed.
pub fn compatible_nodes(model: &Value, parameter: u32, bindings: &[(u32, String)]) -> Vec<u32> {
    let mut found = Vec::new();
    let mut inner = || -> Result<(), String> {
        let parameter = crate::find_parameter(model, parameter)
            .filter(|item| item.get("groupUUID").is_none())
            .ok_or("parameter was not found")?;
        let work = Work::load(model, parameter)?;
        let chosen = work.select(Some(bindings))?;
        let current = work.list[chosen[0]].node;
        if chosen.iter().any(|i| work.list[*i].node != current) {
            return Ok(());
        }
        let mut seen = Vec::new();
        for binding in &work.list {
            if binding.node == current || seen.contains(&binding.node) || !is_node(model, binding.node) {
                continue;
            }
            seen.push(binding.node);
            let mut all = true;
            for i in &chosen {
                all &= compatible(model, &work.list[*i], binding.node)?;
            }
            if all {
                found.push(binding.node);
            }
        }
        Ok(())
    };
    if inner().is_err() {
        found.clear();
    }
    found
}

/// Multiple Keys Editing (issue #183): every set key of every binding of `nodes` on `channel` in `parameters`
/// (default all) becomes `value` (overwrite), itself plus `value` (add) or, for a colour, itself multiplied or
/// screened by `value`. `channel` is opacity, zSort, tint or screenTint; a colour takes three values, one per
/// channel. Opacity and colours are clamped to 0..1. Returns the number of keys changed.
pub(crate) fn multi_edit(
    model: &mut Value,
    nodes: &[u32],
    channel: &str,
    value: &[f64],
    method: &str,
    parameters: Option<&[u32]>,
) -> Result<usize, String> {
    let channels: Vec<(String, f64)> = match (channel, value) {
        ("opacity" | "zSort", [value]) => vec![(channel.to_owned(), *value)],
        ("tint" | "screenTint", [r, g, b]) => {
            [("r", r), ("g", g), ("b", b)].iter().map(|(part, value)| (format!("{channel}.{part}"), **value)).collect()
        }
        ("opacity" | "zSort", _) => return Err(format!("{channel} takes one value")),
        ("tint" | "screenTint", _) => return Err(format!("{channel} takes three values")),
        _ => return Err("channel must be opacity, zSort, tint or screenTint".into()),
    };
    if value.iter().any(|value| !value.is_finite()) {
        return Err("values must be finite".into());
    }
    let colour = matches!(channel, "tint" | "screenTint");
    let apply: fn(f64, f64) -> f64 = match (method, colour) {
        ("overwrite", _) => |_, value| value,
        ("add", _) => |old, value| old + value,
        ("multiply", true) => |old, value| old * value,
        ("screen", true) => |old, value| 1.0 - (1.0 - old) * (1.0 - value),
        _ => return Err("method must be overwrite or add, or multiply or screen for a colour".into()),
    };
    let mut ids: Vec<u32> = match parameters {
        Some(ids) => ids.to_vec(),
        None => parameter_edit::parameter_defaults(model).into_keys().collect(),
    };
    ids.sort_unstable();
    let mut changed = 0;
    for id in ids {
        let opened = open(model, id)?;
        let mut work = Work::load(model, &opened.parameter)?;
        let axes = work.axes.clone();
        let mut touched = false;
        for binding in work.list.iter_mut().filter(|binding| nodes.contains(&binding.node)) {
            let Some((_, value)) = channels.iter().find(|(name, _)| *name == binding.channel) else { continue };
            for x in 0..binding.set.len() {
                for y in 0..binding.set[x].len() {
                    if !binding.set[x][y] {
                        continue;
                    }
                    let mut new = apply(binding.values[x][y][0], *value);
                    if channel != "zSort" {
                        new = new.clamp(0.0, 1.0);
                    }
                    binding.set_value(&axes, [x, y], vec![new])?;
                    changed += 1;
                    touched = true;
                }
            }
        }
        if touched {
            close(model, &opened, &work)?;
        }
    }
    Ok(changed)
}

/// `copySelectionToNode` (`swap` false) and `swapSelectionWithNode` (`swap` true): the copied keypoint of the
/// selected bindings goes to (or is exchanged with) the same channel on `target`, creating its binding.
pub(crate) fn move_to_node(
    model: &mut Value,
    id: u32,
    key: [usize; 2],
    only: &[BindingKey],
    target: u32,
    swap: bool,
) -> Result<usize, String> {
    let opened = open(model, id)?;
    let defaults = parameter_edit::parameter_defaults(model);
    let mut work = Work::load(model, &opened.parameter)?;
    work.check_key(key)?;
    let axes = work.axes.clone();
    let chosen = work.select(Some(only))?;
    let source = work.list[chosen[0]].node;
    if chosen.iter().any(|i| work.list[*i].node != source) {
        return Err("the selected bindings belong to different nodes".into());
    }
    if source == target {
        return Err("choose a node other than the one the bindings are on".into());
    }
    for i in &chosen {
        if !compatible(model, &work.list[*i], target)? {
            return Err(format!("node {target} is not compatible with the {} binding", work.list[*i].channel));
        }
    }
    for i in &chosen {
        let channel = work.list[*i].channel.clone();
        let Some(t) = work.ensure(model, &defaults, target, &channel)? else {
            return Err(format!("node {target} cannot carry a {channel} binding"));
        };
        let (set, cell) = (work.list[*i].is_set(key), work.list[*i].value(key).clone());
        if swap {
            let (other_set, other_cell) = (work.list[t].is_set(key), work.list[t].value(key).clone());
            work.list[t].values[key[0]][key[1]] = cell;
            work.list[t].set[key[0]][key[1]] = set;
            work.list[*i].values[key[0]][key[1]] = other_cell;
            work.list[*i].set[key[0]][key[1]] = other_set;
            work.list[*i].reinterpolate(&axes)?;
            work.list[t].reinterpolate(&axes)?;
        } else if set {
            work.list[t].set_value(&axes, key, cell)?;
        } else {
            work.list[t].unset(&axes, key)?;
        }
    }
    close(model, &opened, &work)?;
    Ok(chosen.len())
}

// ------------------------------------------------------------------------------------------ links

/// `addBinding` of LinkToCommand: `id`'s axis `axis` drives axis `target_axis` of parameter `target`, from the
/// target's minimum at the driver's minimum to its maximum at the driver's maximum.
pub(crate) fn link(
    model: &mut Value,
    id: u32,
    axis: usize,
    target: u32,
    target_axis: usize,
) -> Result<Value, String> {
    if id == target {
        return Err("a parameter cannot be linked to itself".into());
    }
    let opened = open(model, id)?;
    let target_slot = locate_plain(model, target)?;
    let target_parameter = container_of(model, target_slot)?[slot_index(target_slot)].clone();
    let mut work = Work::load(model, &opened.parameter)?;
    if axis > 1 || (axis == 1 && !work.axes.is_vec2) {
        return Err("axis must be 0, or 1 for a 2D parameter".into());
    }
    let target_vec2 = target_parameter.get("is_vec2").and_then(Value::as_bool).unwrap_or(false);
    if target_axis > 1 || (target_axis == 1 && !target_vec2) {
        return Err("target_axis must be 0, or 1 for a 2D target".into());
    }
    let channel = if target_axis == 0 { "X" } else { "Y" };
    if work.list.iter().any(|b| b.node == target && b.channel == channel) {
        return Err(format!("parameter {id} already drives axis {channel} of parameter {target}"));
    }
    let defaults = pair_field(&target_parameter, "defaults", [0.0, 0.0])?;
    let (target_min, target_max) = (
        pair_field(&target_parameter, "min", [0.0, 0.0])?,
        pair_field(&target_parameter, "max", [1.0, 1.0])?,
    );
    let mut binding = Binding::blank(target, channel, &work.axes, defaults[target_axis], 0);
    work.zero_key(&mut binding)?;
    for (end, value) in [(work.min[axis], target_min[target_axis]), (work.max[axis], target_max[target_axis])] {
        let mut position = [0.0, 0.0];
        position[axis] = end;
        let index = work.closest_keypoint(position);
        binding.values[index[0]][index[1]] = vec![value];
        binding.set[index[0]][index[1]] = true;
    }
    binding.reinterpolate(&work.axes)?;
    let created = binding.to_json()?;
    work.list.push(binding);
    close(model, &opened, &work)?;
    Ok(created)
}

/// Removes the link from `id` to axis `target_axis` of `target`.
pub(crate) fn unlink(model: &mut Value, id: u32, target: u32, target_axis: usize) -> Result<usize, String> {
    if target_axis > 1 {
        return Err("target_axis must be 0 or 1".into());
    }
    let channel = if target_axis == 0 { "X" } else { "Y" };
    parameter_edit::remove_bindings(model, id, &[(target, channel.to_owned())])
}

// ------------------------------------------------------------------------------------------ To 2D

/// `convertTo2D`: the parameter becomes a 2D one with the same UUID, X range and X axis points; its Y range
/// repeats the X range and the keys sit on its first Y axis point. D drops the merge mode and defaults of the
/// old parameter; here they are kept, and a grouped parameter moves to the end of its group as in D.
pub(crate) fn convert_to_2d(model: &mut Value, id: u32) -> Result<Value, String> {
    let opened = open(model, id)?;
    let old = &opened.parameter;
    if old.get("is_vec2").and_then(Value::as_bool).unwrap_or(false) {
        return Err("parameter is already 2D".into());
    }
    let defaults = parameter_edit::parameter_defaults(model);
    let source = Work::load(model, old)?;
    let mut work = Work {
        axes: Axes { x: vec![0.0, 1.0], y: vec![0.0, 1.0], is_vec2: true, repeat: false },
        min: [source.min[0], source.min[0]],
        max: [source.max[0], source.max[0]],
        list: Vec::new(),
    };
    for &key in &source.axes.x {
        if key != 0.0 && key != 1.0 {
            let at = work.axes.x.iter().skip(1).position(|point| *point > key).map_or(work.axes.x.len(), |p| p + 1);
            work.axes.x.insert(at, key);
            let axes = work.axes.clone();
            for binding in &mut work.list {
                binding.insert_x_keypoint(&axes, at)?;
            }
        }
        for binding in &source.list {
            let Some(target) = work.ensure(model, &defaults, binding.node, &binding.channel)? else { continue };
            let from = source.closest_keypoint([source.unmap_axis(0, key), source.unmap_axis(1, 0.0)]);
            // `unmapValue(vec2(key, newParam.min.y))` in D feeds the minimum in as a 0..1 offset.
            let to = work.closest_keypoint([work.unmap_axis(0, key), work.unmap_axis(1, work.min[1])]);
            let axes = work.axes.clone();
            if binding.is_set(from) {
                work.list[target].set_value(&axes, to, binding.value(from).clone())?;
            } else {
                work.list[target].unset(&axes, to)?;
            }
        }
    }
    let mut converted = old.clone();
    let default_x = pair_field(old, "defaults", [0.0, 0.0])?[0];
    converted["is_vec2"] = json!(true);
    if let Some(object) = converted.as_object_mut() {
        object.remove("repeat");
    }
    converted["min"] = json!(work.min);
    converted["max"] = json!(work.max);
    converted["defaults"] = json!([default_x, 0.0f64.clamp(work.min[0], work.max[0])]);
    converted["axis_points"] = json!([work.axes.x, work.axes.y]);
    work.store(&mut converted)?;
    let index = slot_index(opened.slot);
    let container = container_of(model, opened.slot)?;
    if opened.slot.group.is_none() {
        container[index] = converted.clone();
    } else {
        container.remove(index);
        container.push(converted.clone());
    }
    // Eye blinking drives 1D parameters only.
    parameter_edit::drop_eye(model, id);
    Ok(converted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partner_and_pair_payload_follow_the_flip_config_format() {
        let model = json!({"nodes": {"uuid": 1, "children": [{"uuid": 4}, {"uuid": 5}]}});
        let payload = br#"[{"uuid1":4,"uuid2":5},{"uuid1":4,"uuid2":99}]"#.to_vec();
        let entries = vec![(FLIP_CONFIG_KEY.as_bytes().to_vec(), payload)];
        let pairs = read_pairs(&entries, &model).unwrap();
        assert_eq!(pairs, vec![(4, 5)]);
        assert_eq!((partner(&pairs, 4), partner(&pairs, 5), partner(&pairs, 1)), (Some(5), Some(4), None));
        assert!(read_pairs(&[(FLIP_CONFIG_KEY.as_bytes().to_vec(), b"{}".to_vec())], &model).is_err());
        assert_eq!(read_pairs(&[], &model).unwrap(), Vec::<Pair>::new());
    }

    #[test]
    fn grid_transfer_mirrors_the_deformed_lattice() {
        // A 2x2 lattice whose right column moved up by 1; dest points sit on the left column.
        let grid = [[-1.0, 0.0], [1.0, 0.0], [-1.0, 2.0], [1.0, 2.0]];
        let offsets = [[0.0, 0.0], [0.0, 1.0], [0.0, 0.0], [0.0, 1.0]];
        let points = [[-1.0, 0.0], [-1.0, 2.0]];
        assert_eq!(transfer_grid(&points, &grid, &offsets, false), vec![[0.0, 0.0], [0.0, 0.0]]);
        assert_eq!(transfer_grid(&points, &grid, &offsets, true), vec![[0.0, 1.0], [0.0, 1.0]]);
        assert_eq!(transfer_grid(&points, &grid[..3], &offsets[..3], false), vec![[0.0, 0.0], [0.0, 0.0]]);
    }
}
