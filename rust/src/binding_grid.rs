//! Typed view of one parameter binding grid, the part of `ParameterBindingImpl` (soul2d-live
//! core/param/binding.d) that the parameter panel's keypoint features need: interpolation in the three
//! modes, `setValue` / `unset` / `reset` with the re-interpolation each of them runs, axis-aware negation and
//! keypoint insertion.
//!
//! A cell is a flat list of components: one number for a value or parameter-link binding, `x0, y0, x1, y1, ...`
//! for a deformation. Every algorithm is linear in the components, so the deformation case runs the scalar
//! code once per component, as `Deformation`'s operators do in D.

use crate::evaluation;
use serde_json::{json, Value};

pub(crate) type Cell = Vec<f64>;

const MAX_AXIS_POINTS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Nearest,
    Linear,
    Cubic,
}

impl Mode {
    pub(crate) fn of(binding: &Value) -> Result<Mode, String> {
        match binding.get("interpolate_mode") {
            None | Some(Value::Null) => Ok(Mode::Linear),
            Some(Value::String(text)) => match text.to_ascii_lowercase().as_str() {
                "nearest" => Ok(Mode::Nearest),
                "linear" => Ok(Mode::Linear),
                "cubic" => Ok(Mode::Cubic),
                _ => Err(format!("binding interpolation {text:?} is unsupported")),
            },
            Some(_) => Err("binding.interpolate_mode must be a string".into()),
        }
    }
}

/// The normalized (0..1) axis points of a parameter.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Axes {
    pub x: Vec<f64>,
    pub y: Vec<f64>,
    pub is_vec2: bool,
    pub repeat: bool,
}

impl Axes {
    pub(crate) fn of(parameter: &Value) -> Result<Axes, String> {
        let is_vec2 = parameter.get("is_vec2").and_then(Value::as_bool).unwrap_or(false);
        let raw = parameter
            .get("axis_points")
            .and_then(Value::as_array)
            .filter(|axes| axes.len() == 2)
            .ok_or("parameter.axis_points must contain two axis arrays")?;
        let read = |value: &Value, name: &str| -> Result<Vec<f64>, String> {
            let points = value
                .as_array()
                .ok_or_else(|| format!("{name} must be an array"))?
                .iter()
                .map(|point| crate::finite_number(point, name))
                .collect::<Result<Vec<_>, _>>()?;
            if points.len() > MAX_AXIS_POINTS {
                return Err(format!("{name} exceeds the supported axis size"));
            }
            Ok(points)
        };
        let x = read(&raw[0], "parameter.axis_points[0]")?;
        let y = read(&raw[1], "parameter.axis_points[1]")?;
        if x.len() < 2 || y.is_empty() || (is_vec2 && y.len() < 2) {
            return Err("parameter axis points are incomplete".into());
        }
        Ok(Axes { x, y, is_vec2, repeat: evaluation::parameter_repeats(parameter) })
    }

    pub(crate) fn counts(&self) -> (usize, usize) {
        (self.x.len(), self.y.len())
    }

    /// `Parameter.getKeypointOffset`.
    pub(crate) fn offset(&self, index: [usize; 2]) -> Result<[f64; 2], String> {
        match (self.x.get(index[0]), self.y.get(index[1])) {
            (Some(x), Some(y)) => Ok([*x, *y]),
            _ => Err("keypoint is outside the parameter axis points".into()),
        }
    }

    /// `Parameter.findOffset`: the keypoint left of `offset` and the position inside that cell.
    pub(crate) fn find_offset(&self, offset: [f64; 2]) -> ([usize; 2], [f64; 2]) {
        fn cell(points: &[f64], value: f64) -> (usize, f64) {
            for i in 0..points.len().saturating_sub(1) {
                if points[i + 1] > value || i == points.len() - 2 {
                    return (i, (value - points[i]) / (points[i + 1] - points[i]));
                }
            }
            (0, 0.0)
        }
        let (x, ox) = cell(&self.x, offset[0]);
        if self.is_vec2 {
            let (y, oy) = cell(&self.y, offset[1]);
            ([x, y], [ox, oy])
        } else {
            ([x, 0], [ox, 0.0])
        }
    }

    /// `Parameter.getClosestAxisPointIndex`: the first closest point.
    pub(crate) fn closest(points: &[f64], offset: f64) -> usize {
        let mut best = (0, f64::INFINITY);
        for (index, point) in points.iter().enumerate() {
            let distance = (point - offset).abs();
            if distance < best.1 {
                best = (index, distance);
            }
        }
        best.0
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Binding {
    /// The stored object, so fields this port does not know survive a rewrite.
    pub raw: Value,
    pub node: u32,
    pub channel: String,
    pub mode: Mode,
    pub deform: bool,
    /// Components per cell: 1 for a scalar, twice the vertex count for a deformation.
    pub width: usize,
    pub values: Vec<Vec<Cell>>,
    pub set: Vec<Vec<bool>>,
    /// The neutral scalar (`clearValue`); deformations are neutral at zero.
    pub default: f64,
    pub touched: bool,
}

fn parse_offsets(cell: &Value) -> Result<Vec<f64>, String> {
    match cell {
        Value::Null => Ok(Vec::new()),
        Value::Array(points) => {
            let mut flat = Vec::with_capacity(points.len() * 2);
            for point in points {
                let pair = point
                    .as_array()
                    .filter(|pair| pair.len() >= 2)
                    .ok_or("deform binding offsets must be coordinate pairs")?;
                flat.push(crate::finite_number(&pair[0], "deform binding offset")?);
                flat.push(crate::finite_number(&pair[1], "deform binding offset")?);
            }
            Ok(flat)
        }
        _ => Err("deform binding values must be arrays".into()),
    }
}

impl Binding {
    /// Reads a stored binding for a parameter with `axes`. `vertices` is the vertex count of the target node
    /// when the binding stores none yet.
    pub(crate) fn parse(raw: &Value, axes: &Axes, default: f64, vertices: Option<usize>) -> Result<Binding, String> {
        let node = raw
            .get("node")
            .and_then(Value::as_u64)
            .and_then(crate::valid_id)
            .ok_or("binding.node must be a nonzero u32 UUID")?;
        let channel = match raw.get("param_name") {
            Some(Value::String(name)) => name.clone(),
            // The older integer form of a parameter link.
            Some(Value::Number(_)) => match evaluation::link_axis(raw) {
                Some(0) => "X".to_owned(),
                Some(_) => "Y".to_owned(),
                None => return Err("binding.param_name must be a string".into()),
            },
            _ => return Err("binding.param_name must be a string".into()),
        };
        let deform = channel == "deform";
        let (x_count, y_count) = axes.counts();
        let columns = raw
            .get("values")
            .and_then(Value::as_array)
            .filter(|columns| columns.len() == x_count)
            .ok_or("binding.values has a mismatched X axis")?;
        let flags = raw
            .get("isSet")
            .and_then(Value::as_array)
            .filter(|flags| flags.len() == x_count)
            .ok_or("binding.isSet has a mismatched X axis")?;
        let mut cells: Vec<Vec<&Value>> = Vec::new();
        let mut set = Vec::new();
        for (column, flag_column) in columns.iter().zip(flags) {
            let column = column
                .as_array()
                .filter(|column| column.len() == y_count)
                .ok_or("binding.values has a mismatched Y axis")?;
            let flag_column = flag_column
                .as_array()
                .filter(|column| column.len() == y_count)
                .ok_or("binding.isSet has a mismatched Y axis")?;
            let flag_column = flag_column
                .iter()
                .map(|flag| match flag {
                    Value::Bool(set) => Ok(*set),
                    Value::Null => Ok(false),
                    _ => Err("binding.isSet entries must be booleans".to_string()),
                })
                .collect::<Result<Vec<_>, _>>()?;
            cells.push(column.iter().collect());
            set.push(flag_column);
        }
        let mut binding = Binding {
            raw: raw.clone(),
            node,
            channel,
            mode: Mode::of(raw)?,
            deform,
            width: 1,
            values: Vec::new(),
            set,
            default,
            touched: false,
        };
        if deform {
            let parsed = cells
                .iter()
                .map(|column| column.iter().map(|cell| parse_offsets(cell)).collect::<Result<Vec<_>, _>>())
                .collect::<Result<Vec<_>, _>>()?;
            let known = |wanted_set: Option<bool>| {
                parsed.iter().zip(&binding.set).flat_map(|(column, flags)| column.iter().zip(flags)).find_map(
                    |(cell, flag)| (!cell.is_empty() && wanted_set.is_none_or(|wanted| wanted == *flag)).then_some(cell.len()),
                )
            };
            binding.width = known(Some(true)).or_else(|| known(None)).unwrap_or_else(|| vertices.unwrap_or(0) * 2);
            let width = binding.width;
            let mut values = Vec::new();
            for (column, flags) in parsed.into_iter().zip(&binding.set) {
                let mut out = Vec::new();
                for (cell, flag) in column.into_iter().zip(flags) {
                    if cell.len() == width {
                        out.push(cell);
                    } else if *flag {
                        return Err("deform binding values have inconsistent vertex counts".into());
                    } else {
                        out.push(vec![0.0; width]);
                    }
                }
                values.push(out);
            }
            binding.values = values;
        } else {
            for column in &cells {
                let mut out = Vec::new();
                for cell in column {
                    out.push(vec![crate::finite_number(cell, "binding.values")?]);
                }
                binding.values.push(out);
            }
        }
        Ok(binding)
    }

    /// `createBinding` without the zero key: every cell neutral and unset.
    pub(crate) fn blank(node: u32, channel: &str, axes: &Axes, default: f64, vertices: usize) -> Binding {
        let deform = channel == "deform";
        let width = if deform { vertices * 2 } else { 1 };
        let (x_count, y_count) = axes.counts();
        let mut binding = Binding {
            raw: json!({"node": node, "param_name": channel, "interpolate_mode": "Linear",
                "values": [], "isSet": []}),
            node,
            channel: channel.to_owned(),
            mode: Mode::Linear,
            deform,
            width,
            values: Vec::new(),
            set: vec![vec![false; y_count]; x_count],
            default,
            touched: true,
        };
        binding.values = vec![vec![binding.neutral(); y_count]; x_count];
        binding
    }

    pub(crate) fn neutral(&self) -> Cell {
        if self.deform {
            vec![0.0; self.width]
        } else {
            vec![self.default]
        }
    }

    pub(crate) fn is_set(&self, index: [usize; 2]) -> bool {
        self.set[index[0]][index[1]]
    }

    pub(crate) fn value(&self, index: [usize; 2]) -> &Cell {
        &self.values[index[0]][index[1]]
    }

    /// The stored object with the current values and flags.
    pub(crate) fn to_json(&self) -> Result<Value, String> {
        let mut out = self.raw.clone();
        let mut columns = Vec::with_capacity(self.values.len());
        for column in &self.values {
            let mut cells = Vec::with_capacity(column.len());
            for cell in column {
                if cell.iter().any(|component| !component.is_finite()) {
                    return Err("binding value is not finite".into());
                }
                cells.push(if self.deform {
                    Value::Array(cell.chunks(2).map(|pair| json!([pair[0], pair[1]])).collect())
                } else {
                    json!(cell[0])
                });
            }
            columns.push(Value::Array(cells));
        }
        out["values"] = Value::Array(columns);
        out["isSet"] = json!(self.set);
        Ok(out)
    }

    fn check_width(&self, cell: &[f64]) -> Result<(), String> {
        if cell.len() == self.width {
            Ok(())
        } else {
            Err("deform value does not match the vertex count of the binding".into())
        }
    }

    /// `ParameterBindingImpl.interpolate`.
    pub(crate) fn interpolate(&self, axes: &Axes, left: [usize; 2], offset: [f64; 2]) -> Result<Cell, String> {
        let (x_count, y_count) = axes.counts();
        if left[0] + 1 >= x_count || (axes.is_vec2 && left[1] + 1 >= y_count) {
            return Err("interpolation cell is outside the parameter axis points".into());
        }
        let at = |x: usize, y: usize| &self.values[x][y];
        let lerp = |a: &Cell, b: &Cell, t: f64| -> Cell {
            a.iter().zip(b).map(|(a, b)| a * (1.0 - t) + b * t).collect()
        };
        Ok(match self.mode {
            Mode::Nearest => {
                let px = left[0] + usize::from(offset[0] >= 0.5);
                let py = if axes.is_vec2 { left[1] + usize::from(offset[1] >= 0.5) } else { 0 };
                at(px, py).clone()
            }
            Mode::Linear => {
                let (p0, p1) = if axes.is_vec2 {
                    let (x, y) = (left[0], left[1]);
                    (lerp(at(x, y), at(x, y + 1), offset[1]), lerp(at(x + 1, y), at(x + 1, y + 1), offset[1]))
                } else {
                    (at(left[0], 0).clone(), at(left[0] + 1, 0).clone())
                };
                lerp(&p0, &p1, offset[0])
            }
            Mode::Cubic => {
                let cubic = |p: [&Cell; 4], t: f64| -> Cell {
                    (0..p[1].len())
                        .map(|c| evaluation::cubic_component(p[0][c], p[1][c], p[2][c], p[3][c], t))
                        .collect()
                };
                let xlen = x_count - 1;
                let xk = left[0];
                let column = |y: usize| {
                    cubic(
                        [at(xk.saturating_sub(1), y), at(xk, y), at(xk + 1, y), at((xk + 2).min(xlen), y)],
                        offset[0],
                    )
                };
                if axes.is_vec2 {
                    let ylen = y_count - 1;
                    let rows: Vec<Cell> = (0..4)
                        .map(|y| column((left[1] as isize + y as isize - 1).clamp(0, ylen as isize) as usize))
                        .collect();
                    cubic([&rows[0], &rows[1], &rows[2], &rows[3]], offset[1])
                } else {
                    column(0)
                }
            }
        })
    }

    /// `reInterpolate`: unset cells follow the set ones; with nothing set every cell is neutral again.
    pub(crate) fn reinterpolate(&mut self, axes: &Axes) -> Result<(), String> {
        self.touched = true;
        if !self.set.iter().flatten().any(|set| *set) {
            let neutral = self.neutral();
            self.values.iter_mut().flatten().for_each(|cell| *cell = neutral.clone());
            return Ok(());
        }
        if self.set.iter().flatten().all(|set| *set) {
            return Ok(());
        }
        for component in 0..self.width {
            let mut grid: Vec<Vec<f64>> =
                self.values.iter().map(|column| column.iter().map(|cell| cell[component]).collect()).collect();
            evaluation::reinterpolate(&mut grid, &self.set, &axes.x, &axes.y, axes.repeat)?;
            for (x, column) in grid.iter().enumerate() {
                for (y, value) in column.iter().enumerate() {
                    if !self.set[x][y] {
                        self.values[x][y][component] = *value;
                    }
                }
            }
        }
        Ok(())
    }

    /// `setValue`.
    pub(crate) fn set_value(&mut self, axes: &Axes, index: [usize; 2], cell: Cell) -> Result<(), String> {
        self.check_width(&cell)?;
        self.values[index[0]][index[1]] = cell;
        self.set[index[0]][index[1]] = true;
        self.reinterpolate(axes)
    }

    /// `unset`.
    pub(crate) fn unset(&mut self, axes: &Axes, index: [usize; 2]) -> Result<(), String> {
        self.values[index[0]][index[1]] = self.neutral();
        self.set[index[0]][index[1]] = false;
        self.reinterpolate(axes)
    }

    /// `reset`.
    pub(crate) fn reset(&mut self, axes: &Axes, index: [usize; 2]) -> Result<(), String> {
        self.values[index[0]][index[1]] = self.neutral();
        self.set[index[0]][index[1]] = true;
        self.reinterpolate(axes)
    }

    /// The cell `scaleValueAt(index, axis, -1)` stores: `axis` is -1 (invert), 0 or 1.
    pub(crate) fn negated(&self, index: [usize; 2], axis: i32) -> Cell {
        let cell = self.value(index);
        if self.deform {
            cell.iter()
                .enumerate()
                .map(|(i, v)| {
                    let hit = axis == -1 || (axis == 0 && i % 2 == 0) || (axis == 1 && i % 2 == 1);
                    if hit { -v } else { *v }
                })
                .collect()
        } else {
            let negate = if axis < 0 {
                true
            } else {
                crate::parameter_edit::mirror_negates(&self.channel, Some(axis as usize))
            };
            vec![if negate { -cell[0] } else { cell[0] }]
        }
    }

    /// `insertKeypoints(0, index)` followed by the re-interpolation; `axes` already holds the new point.
    pub(crate) fn insert_x_keypoint(&mut self, axes: &Axes, index: usize) -> Result<(), String> {
        let y_count = axes.y.len();
        self.values.insert(index, vec![self.neutral(); y_count]);
        self.set.insert(index, vec![false; y_count]);
        self.reinterpolate(axes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn axes_1d(points: &[f64]) -> Axes {
        Axes { x: points.to_vec(), y: vec![0.0], is_vec2: false, repeat: false }
    }

    fn scalar(axes: &Axes, values: &[f64], set: &[bool]) -> Binding {
        let raw = json!({"node": 4, "param_name": "transform.t.x",
            "values": values.iter().map(|v| vec![*v]).collect::<Vec<_>>(),
            "isSet": set.iter().map(|s| vec![*s]).collect::<Vec<_>>()});
        Binding::parse(&raw, axes, 0.0, None).unwrap()
    }

    #[test]
    fn find_offset_follows_parameter_find_offset() {
        let axes = axes_1d(&[0.0, 0.25, 1.0]);
        assert_eq!(axes.find_offset([0.0, 0.0]), ([0, 0], [0.0, 0.0]));
        assert_eq!(axes.find_offset([0.25, 0.0]), ([1, 0], [0.0, 0.0]));
        assert_eq!(axes.find_offset([0.625, 0.0]), ([1, 0], [0.5, 0.0]));
        assert_eq!(axes.find_offset([1.0, 0.0]), ([1, 0], [1.0, 0.0]));
        assert_eq!(Axes::closest(&axes.x, 0.7), 2);
        assert_eq!(Axes::closest(&[0.0, 1.0], 0.5), 0);
    }

    #[test]
    fn interpolation_modes_read_neighbours_like_the_engine() {
        let axes = axes_1d(&[0.0, 0.5, 1.0]);
        let mut binding = scalar(&axes, &[0.0, 4.0, 8.0], &[true, true, true]);
        assert_eq!(binding.interpolate(&axes, [0, 0], [0.5, 0.0]).unwrap(), vec![2.0]);
        binding.mode = Mode::Nearest;
        assert_eq!(binding.interpolate(&axes, [0, 0], [0.5, 0.0]).unwrap(), vec![4.0]);
        assert_eq!(binding.interpolate(&axes, [0, 0], [0.25, 0.0]).unwrap(), vec![0.0]);
        binding.mode = Mode::Cubic;
        // Catmull-Rom through 0, 0, 4, 8 at t = 0.5 for the first cell.
        let expected = evaluation::cubic_component(0.0, 0.0, 4.0, 8.0, 0.5);
        assert_eq!(binding.interpolate(&axes, [0, 0], [0.5, 0.0]).unwrap(), vec![expected]);
        assert!(binding.interpolate(&axes, [2, 0], [0.0, 0.0]).is_err());
    }

    #[test]
    fn repeating_bindings_refill_after_set_unset_and_axis_insert() {
        let mut axes = axes_1d(&[0.0, 0.5, 1.0]);
        axes.repeat = true;
        let mut binding = scalar(&axes, &[10.0, 20.0, 99.0], &[true, true, true]);
        binding.unset(&axes, [2, 0]).unwrap();
        assert_eq!(binding.values[2][0], vec![10.0]);
        binding.set_value(&axes, [0, 0], vec![30.0]).unwrap();
        assert_eq!(binding.values[2][0], vec![30.0]);
        axes.x.insert(2, 0.75);
        binding.insert_x_keypoint(&axes, 2).unwrap();
        assert_eq!(binding.values[2][0], vec![25.0]);
        assert_eq!(binding.values[3][0], vec![30.0]);
    }

    #[test]
    fn set_unset_and_reset_reinterpolate_the_other_cells() {
        let axes = axes_1d(&[0.0, 0.5, 1.0]);
        let mut binding = scalar(&axes, &[1.0, 1.0, 1.0], &[true, false, false]);
        binding.set_value(&axes, [2, 0], vec![5.0]).unwrap();
        assert_eq!(binding.values.concat(), vec![vec![1.0], vec![3.0], vec![5.0]]);
        binding.unset(&axes, [2, 0]).unwrap();
        assert_eq!(binding.set.concat(), vec![true, false, false]);
        assert_eq!(binding.values.concat(), vec![vec![1.0], vec![1.0], vec![1.0]]);
        binding.unset(&axes, [0, 0]).unwrap();
        assert_eq!(binding.values.concat(), vec![vec![0.0], vec![0.0], vec![0.0]]);
        binding.reset(&axes, [1, 0]).unwrap();
        assert_eq!(binding.set.concat(), vec![false, true, false]);
        assert!(binding.set_value(&axes, [0, 0], vec![1.0, 2.0]).is_err());
    }

    #[test]
    fn deform_cells_run_the_algorithms_per_component() {
        let axes = axes_1d(&[0.0, 0.5, 1.0]);
        let raw = json!({"node": 5, "param_name": "deform",
            "values": [[[[0.0, 2.0]]], [[[0.0, 0.0]]], [[[4.0, 6.0]]]],
            "isSet": [[true], [false], [true]]});
        let mut binding = Binding::parse(&raw, &axes, 0.0, None).unwrap();
        assert_eq!(binding.width, 2);
        binding.reinterpolate(&axes).unwrap();
        assert_eq!(binding.values[1][0], vec![2.0, 4.0]);
        assert_eq!(binding.negated([2, 0], 0), vec![-4.0, 6.0]);
        assert_eq!(binding.negated([2, 0], 1), vec![4.0, -6.0]);
        assert_eq!(binding.negated([2, 0], -1), vec![-4.0, -6.0]);
        let json = binding.to_json().unwrap();
        assert_eq!(json["values"][1][0], json!([[2.0, 4.0]]));
        assert_eq!(json["isSet"], json!([[true], [false], [true]]));
        // An unset cell stored with the wrong length is taken as neutral; a set one is an error.
        let ragged = json!({"node": 5, "param_name": "deform",
            "values": [[[[1.0, 1.0]]], [null], [[[1.0, 1.0]]]], "isSet": [[true], [false], [true]]});
        assert_eq!(Binding::parse(&ragged, &axes, 0.0, None).unwrap().values[1][0], vec![0.0, 0.0]);
        let bad = json!({"node": 5, "param_name": "deform",
            "values": [[[[1.0, 1.0]]], [[[1.0, 1.0], [2.0, 2.0]]], [[[1.0, 1.0]]]], "isSet": [[true], [true], [true]]});
        assert!(Binding::parse(&bad, &axes, 0.0, None).is_err());
    }

    #[test]
    fn negation_follows_the_channel_rules_of_node_scale_value() {
        let axes = axes_1d(&[0.0, 1.0]);
        let mut binding = scalar(&axes, &[2.0, 3.0], &[true, true]);
        assert_eq!(binding.negated([0, 0], 0), vec![-2.0]);
        assert_eq!(binding.negated([0, 0], 1), vec![2.0]);
        binding.channel = "transform.t.y".into();
        assert_eq!(binding.negated([0, 0], 1), vec![-2.0]);
        assert_eq!(binding.negated([0, 0], -1), vec![-2.0]);
        binding.channel = "X".into();
        assert_eq!(binding.negated([0, 0], 1), vec![-2.0]);
    }

    #[test]
    fn blank_binding_is_neutral_and_inserting_a_keypoint_keeps_the_set_ones() {
        let mut axes = axes_1d(&[0.0, 1.0]);
        let mut binding = Binding::blank(4, "transform.s.x", &axes, 1.0, 0);
        assert_eq!(binding.values.concat(), vec![vec![1.0], vec![1.0]]);
        binding.set_value(&axes, [1, 0], vec![3.0]).unwrap();
        axes.x.insert(1, 0.5);
        binding.insert_x_keypoint(&axes, 1).unwrap();
        assert_eq!(binding.set.concat(), vec![false, false, true]);
        assert_eq!(binding.values.concat(), vec![vec![3.0], vec![3.0], vec![3.0]]);
    }
}
