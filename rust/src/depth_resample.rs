//! Deformer depth resampling (slice D3).
//!
//! `DefineGrid` and `DefineVertices` call this from the same candidate as the
//! vertex write, so one undo restores geometry and its depth payload together.
//!
//! Grid depths and operation bases follow `ExGridDeformer.rebuffer`: rectangular
//! fields use `ngResampleGridDepths` in `float`, and a failed resample keeps the
//! existing prefix (`resizeDepthsToVertices` zero-fills the tail or truncates).
//! Path depths follow `ExPathDeformer.rebuffer`, interpolating by normalized
//! control-point index in `float`. `copyDepths` normalizes samples before either
//! interpolation.
//!
//! Attached points follow `GridDeformerDefineAction.updateNewState`, which
//! replaces the rebuffer remap with `ngRemapGridIndexBoundDepthOperations`
//! and `preserveSameCount = false`. The nearest new vertex within `1e-8`
//! squared distance wins; otherwise that op is dropped. Ring, plane, and
//! unknown op objects are copied unchanged, including unknown fields.
//! Recognized op types must already parse, matching a later `ApplyDepthOps`.
//! When the remapped list is empty, both `depth-ops` and
//! `depth-op-base-depths` are omitted, matching D serialization of an empty
//! operation list.

use crate::depth_ops::non_negative_index;
use crate::depths::finite_depth_or_zero;
use serde_json::{json, Map, Value};

const AXIS_TOLERANCE: f32 = 1e-4;
const MATCH_DISTANCE_SQUARED: f32 = 1.0e-8;
const OPS_KEY: &str = "depth-ops";
const BASE_KEY: &str = "depth-op-base-depths";

pub(crate) fn has_grid_depth_payload(node: &Value) -> bool {
    present(node, "depths") || present(node, OPS_KEY) || present(node, BASE_KEY)
}

pub(crate) fn has_path_depth_payload(node: &Value) -> bool {
    present(node, "depths")
}

pub(crate) fn prepare_grid_depth_rebuffer(node: &Value) -> Result<(), String> {
    if !has_grid_depth_payload(node) {
        return Ok(());
    }
    let old = grid_vertices(node)?;
    let _ = read_number_array(node, "depths")?;
    let _ = read_number_array(node, BASE_KEY)?;
    let _ = validated_ops(node, &old)?;
    Ok(())
}

pub(crate) fn prepare_path_depth_rebuffer(node: &Value) -> Result<(), String> {
    if !has_path_depth_payload(node) {
        return Ok(());
    }
    let _ = path_vertex_count(node.get("vertices"))?;
    let _ = read_number_array(node, "depths")?;
    Ok(())
}

pub(crate) fn path_vertex_count(vertices: Option<&Value>) -> Result<usize, String> {
    let Some(vertices) = vertices else {
        return Err("PathDeformer vertices must be an array".into());
    };
    let values = vertices
        .as_array()
        .ok_or("PathDeformer vertices must be an array")?;
    if values.len() % 2 != 0 {
        return Err("PathDeformer vertices length must be even".into());
    }
    if values
        .iter()
        .any(|value| value.as_f64().is_none_or(|number| !number.is_finite()))
    {
        return Err("PathDeformer vertices must contain finite coordinates".into());
    }
    Ok(values.len() / 2)
}

pub(crate) fn rebuffer_path_depth_payload(
    node: &mut Value,
    old_vertex_count: usize,
) -> Result<(), String> {
    let Some(depths) = resampled_path_depths(node, old_vertex_count)? else {
        return Ok(());
    };
    node.as_object_mut()
        .ok_or("node must be an object")?
        .insert("depths".into(), json!(stored_f64(&depths)));
    Ok(())
}

fn resampled_path_depths(
    node: &Value,
    old_vertex_count: usize,
) -> Result<Option<Vec<f32>>, String> {
    let Some(values) = read_number_array(node, "depths")? else {
        return Ok(None);
    };
    let new_vertex_count = path_vertex_count(node.get("vertices"))?;
    let samples = values
        .iter()
        .map(|value| finite_depth_or_zero(*value) as f32)
        .collect::<Vec<_>>();
    Ok(Some(
        resample_path_depths(&samples, old_vertex_count, new_vertex_count)
            .unwrap_or_else(|| resize_depths(&samples, new_vertex_count)),
    ))
}

fn resample_path_depths(
    old_depths: &[f32],
    old_vertex_count: usize,
    new_vertex_count: usize,
) -> Option<Vec<f32>> {
    if old_vertex_count == 0 || old_depths.len() != old_vertex_count {
        return None;
    }
    if new_vertex_count == 0 {
        return Some(Vec::new());
    }
    if old_vertex_count == 1 {
        return Some(vec![
            finite_depth_or_zero(f64::from(old_depths[0])) as f32;
            new_vertex_count
        ]);
    }

    let mut result = Vec::with_capacity(new_vertex_count);
    for index in 0..new_vertex_count {
        let normalized = if new_vertex_count > 1 {
            index as f32 / (new_vertex_count - 1) as f32
        } else {
            0.0
        };
        let old_position = normalized * (old_depths.len() - 1) as f32;
        let mut segment = old_position as usize;
        if segment >= old_depths.len() - 1 {
            segment = old_depths.len() - 2;
        }
        let weight = old_position - segment as f32;
        let sample = old_depths[segment] * (1.0 - weight) + old_depths[segment + 1] * weight;
        result.push(finite_depth_or_zero(f64::from(sample)) as f32);
    }
    Some(result)
}

pub(crate) fn grid_vertices(node: &Value) -> Result<Vec<[f32; 2]>, String> {
    let xs = axis_f32(node, "grid_axis_x")?;
    let ys = axis_f32(node, "grid_axis_y")?;
    if xs.len() < 2 || ys.len() < 2 {
        return Err("depth payload cannot be resampled".into());
    }
    let mut points = Vec::with_capacity(xs.len().saturating_mul(ys.len()));
    for y in &ys {
        for x in &xs {
            points.push([*x, *y]);
        }
    }
    Ok(points)
}

pub(crate) fn rebuffer_grid_depth_payload(
    node: &mut Value,
    old_vertices: &[[f32; 2]],
) -> Result<(), String> {
    let new_vertices = grid_vertices(node)?;
    let depths = resampled_array(node, "depths", old_vertices, &new_vertices, true)?;
    let base = resampled_array(node, BASE_KEY, old_vertices, &new_vertices, false)?;
    let ops = validated_ops(node, old_vertices)?;
    let object = node.as_object_mut().ok_or("node must be an object")?;
    if let Some(depths) = depths {
        object.insert("depths".into(), json!(stored_f64(&depths)));
    }
    if let Some(ops) = ops {
        let remapped = remap_ops(&ops, old_vertices, &new_vertices, false)?;
        if remapped.is_empty() {
            // D omits both keys when the operation list is empty.
            object.remove(OPS_KEY);
            object.remove(BASE_KEY);
        } else {
            object.insert(OPS_KEY.into(), Value::Array(remapped));
            if let Some(base) = base {
                object.insert(BASE_KEY.into(), json!(stored_f64(&base)));
            }
        }
    } else if let Some(base) = base {
        object.insert(BASE_KEY.into(), json!(stored_f64(&base)));
    }
    Ok(())
}

fn resampled_array(
    node: &Value,
    key: &str,
    old_vertices: &[[f32; 2]],
    new_vertices: &[[f32; 2]],
    normalize_inputs: bool,
) -> Result<Option<Vec<f32>>, String> {
    let Some(values) = read_number_array(node, key)? else {
        return Ok(None);
    };
    let samples = if normalize_inputs {
        values
            .iter()
            .map(|value| finite_depth_or_zero(*value) as f32)
            .collect::<Vec<_>>()
    } else {
        values.iter().map(|value| *value as f32).collect::<Vec<_>>()
    };
    let resampled =
        if let Some(resampled) = resample_grid_depths(old_vertices, &samples, new_vertices) {
            resampled
        } else {
            resize_depths(&samples, new_vertices.len())
        };
    Ok(Some(resampled))
}

fn resample_grid_depths(
    old_vertices: &[[f32; 2]],
    old_depths: &[f32],
    new_vertices: &[[f32; 2]],
) -> Option<Vec<f32>> {
    if old_vertices.len() < 4 || old_depths.len() != old_vertices.len() {
        return None;
    }
    let mut xs: Vec<f32> = old_vertices.iter().map(|vertex| vertex[0]).collect();
    let mut ys: Vec<f32> = old_vertices.iter().map(|vertex| vertex[1]).collect();
    xs = depth_grid_axis(xs);
    ys = depth_grid_axis(ys);
    let cols = xs.len();
    let rows = ys.len();
    if cols < 2 || rows < 2 || cols.checked_mul(rows) != Some(old_vertices.len()) {
        return None;
    }
    let mut ordered = vec![0.0f32; old_depths.len()];
    let mut assigned = vec![false; old_depths.len()];
    for (vertex, depth) in old_vertices.iter().zip(old_depths.iter()) {
        let x = depth_grid_axis_index(&xs, vertex[0])?;
        let y = depth_grid_axis_index(&ys, vertex[1])?;
        let ordered_index = y.checked_mul(cols)?.checked_add(x)?;
        if assigned.get(ordered_index).copied().unwrap_or(true) {
            return None;
        }
        ordered[ordered_index] = *depth;
        assigned[ordered_index] = true;
    }
    if assigned.iter().any(|was_assigned| !was_assigned) {
        return None;
    }

    let mut result = Vec::with_capacity(new_vertices.len());
    for vertex in new_vertices {
        let Some((cell_x, u)) = locate_interval(&xs, vertex[0]) else {
            result.push(0.0);
            continue;
        };
        let Some((cell_y, v)) = locate_interval(&ys, vertex[1]) else {
            result.push(0.0);
            continue;
        };
        let index00 = cell_y.checked_mul(cols)?.checked_add(cell_x)?;
        let index10 = index00.checked_add(1)?;
        let index01 = cell_y
            .checked_add(1)?
            .checked_mul(cols)?
            .checked_add(cell_x)?;
        let index11 = index01.checked_add(1)?;
        if index11 >= ordered.len() {
            return None;
        }
        let top = ordered[index00] * (1.0 - u) + ordered[index10] * u;
        let bottom = ordered[index01] * (1.0 - u) + ordered[index11] * u;
        let sample = top * (1.0 - v) + bottom * v;
        result.push(finite_depth_or_zero(f64::from(sample)) as f32);
    }
    Some(result)
}

fn resize_depths(old: &[f32], vertex_count: usize) -> Vec<f32> {
    let mut resized = Vec::with_capacity(vertex_count);
    for value in old.iter().take(vertex_count) {
        resized.push(finite_depth_or_zero(f64::from(*value)) as f32);
    }
    resized.resize(vertex_count, 0.0);
    resized
}

fn remap_ops(
    ops: &[Value],
    old_vertices: &[[f32; 2]],
    new_vertices: &[[f32; 2]],
    preserve_same_count: bool,
) -> Result<Vec<Value>, String> {
    let preserve = preserve_same_count && old_vertices.len() == new_vertices.len();
    let mut result = Vec::with_capacity(ops.len());
    for op in ops {
        let object = op.as_object().ok_or("depth operation must be an object")?;
        crate::depth_ops::ensure_known_depth_op(op)?;
        let type_name = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or("operation.type is required")?;
        if type_name != "attached-point" && type_name != "AttachedPoint" {
            result.push(op.clone());
            continue;
        }
        let index = attached_index(object)?;
        if index >= old_vertices.len() || new_vertices.is_empty() {
            continue;
        }
        if preserve {
            result.push(op.clone());
            continue;
        }
        let source = old_vertices[index];
        let mut best_distance = f32::INFINITY;
        let mut best_index = 0usize;
        let mut found = false;
        for (candidate_index, candidate) in new_vertices.iter().enumerate() {
            let dx = candidate[0] - source[0];
            let dy = candidate[1] - source[1];
            let distance = dx * dx + dy * dy;
            if !distance.is_finite() || (found && distance >= best_distance) {
                continue;
            }
            best_distance = distance;
            best_index = candidate_index;
            found = true;
        }
        if !found || best_distance > MATCH_DISTANCE_SQUARED {
            continue;
        }
        let mut updated = op.clone();
        if best_index != index {
            updated
                .as_object_mut()
                .ok_or("depth operation must be an object")?
                .insert("index".into(), json!(best_index));
        }
        result.push(updated);
    }
    Ok(result)
}

fn validated_ops(node: &Value, old_vertices: &[[f32; 2]]) -> Result<Option<Vec<Value>>, String> {
    let Some(ops) = read_ops(node)? else {
        return Ok(None);
    };
    let _ = remap_ops(&ops, old_vertices, old_vertices, true)?;
    Ok(Some(ops))
}

fn read_ops(node: &Value) -> Result<Option<Vec<Value>>, String> {
    match node.get(OPS_KEY) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(values)) => Ok(Some(values.clone())),
        Some(_) => Err("depth-ops must be an array".into()),
    }
}

fn attached_index(object: &Map<String, Value>) -> Result<usize, String> {
    match object.get("index") {
        None | Some(Value::Null) => Ok(0),
        Some(value) => non_negative_index(value, "index"),
    }
}

fn depth_grid_axis(mut values: Vec<f32>) -> Vec<f32> {
    if values.is_empty() {
        return values;
    }
    values.sort_by(f32::total_cmp);
    let mut write = 1usize;
    for index in 1..values.len() {
        if !same_depth_axis(values[write - 1], values[index]) {
            values[write] = values[index];
            write += 1;
        }
    }
    values.truncate(write);
    values
}

fn depth_grid_axis_index(axis: &[f32], value: f32) -> Option<usize> {
    axis.iter()
        .position(|axis_value| same_depth_axis(value, *axis_value))
}

fn same_depth_axis(left: f32, right: f32) -> bool {
    if left == right {
        return true;
    }
    if left.is_infinite() || right.is_infinite() {
        return false;
    }
    let diff = (left - right).abs();
    diff <= AXIS_TOLERANCE * left.abs()
        || diff <= AXIS_TOLERANCE * right.abs()
        || diff <= AXIS_TOLERANCE
}

fn locate_interval(axis: &[f32], value: f32) -> Option<(usize, f32)> {
    if axis.len() < 2 {
        return None;
    }
    if value <= axis[0] {
        let span = axis[1] - axis[0];
        let weight = if span > 0.0 {
            (value - axis[0]) / span
        } else {
            0.0
        };
        return Some((0, weight));
    }
    let last = axis.len() - 1;
    if value >= axis[last] {
        let span = axis[last] - axis[last - 1];
        let weight = if span > 0.0 {
            (value - axis[last - 1]) / span
        } else {
            1.0
        };
        return Some((last - 1, weight));
    }
    for index in 0..axis.len() - 1 {
        let start = axis[index];
        let end = axis[index + 1];
        if value < start || value > end {
            continue;
        }
        let span = end - start;
        let weight = if span > 0.0 {
            (value - start) / span
        } else {
            0.0
        };
        return Some((index, weight));
    }
    None
}

fn axis_f32(node: &Value, key: &str) -> Result<Vec<f32>, String> {
    let values = node
        .get(key)
        .and_then(Value::as_array)
        .ok_or("depth payload cannot be resampled")?;
    let mut axis = Vec::with_capacity(values.len());
    for value in values {
        let number = value
            .as_f64()
            .filter(|number| number.is_finite())
            .ok_or("depth payload cannot be resampled")?;
        let sample = number as f32;
        if !sample.is_finite() {
            return Err("depth payload cannot be resampled".into());
        }
        axis.push(sample);
    }
    Ok(axis)
}

fn read_number_array(node: &Value, key: &str) -> Result<Option<Vec<f64>>, String> {
    match node.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(values)) => {
            let mut numbers = Vec::with_capacity(values.len());
            for value in values {
                let number = value
                    .as_f64()
                    .filter(|number| number.is_finite())
                    .ok_or_else(|| format!("{key} must be an array of numbers"))?;
                numbers.push(number);
            }
            Ok(Some(numbers))
        }
        Some(_) => Err(format!("{key} must be an array of numbers")),
    }
}

fn present(node: &Value, key: &str) -> bool {
    node.get(key).is_some_and(|value| !value.is_null())
}

fn stored_f64(values: &[f32]) -> Vec<f64> {
    values.iter().copied().map(f64::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vertices(xs: &[f32], ys: &[f32]) -> Vec<[f32; 2]> {
        let mut points = Vec::new();
        for y in ys {
            for x in xs {
                points.push([*x, *y]);
            }
        }
        points
    }

    #[test]
    fn bilinear_resize_extrapolate_and_descending_axes_match_d() {
        let old = vertices(&[-1.0, 1.0], &[-1.0, 1.0]);
        let inside = vertices(&[-0.5, 0.5], &[-0.5, 0.5]);
        let depths = [0.0, 2.0, 4.0, 6.0];
        assert_eq!(
            resample_grid_depths(&old, &depths, &inside).unwrap(),
            vec![1.5, 2.5, 3.5, 4.5]
        );
        let outside = vertices(&[-2.0, 2.0], &[-2.0, 2.0]);
        assert_eq!(
            resample_grid_depths(&inside, &[1.5, 2.5, 3.5, 4.5], &outside).unwrap(),
            vec![-3.0, 1.0, 5.0, 9.0]
        );

        let coarse = vertices(&[-20.0, 20.0], &[-10.0, 10.0]);
        let fine = vertices(&[-20.0, 0.0, 20.0], &[-10.0, 0.0, 10.0]);
        let samples = [0.0f32, 0.2, 0.6, 0.8];
        let literals = [0.0f32, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8];
        let resampled = resample_grid_depths(&coarse, &samples, &fine).unwrap();
        for (value, literal) in resampled.iter().zip(literals) {
            assert!((value - literal).abs() < 1.0e-4, "{value} vs {literal}");
        }

        let descending = vec![[1.0, 1.0], [-1.0, 1.0], [1.0, -1.0], [-1.0, -1.0]];
        let descending_depths: Vec<f32> = descending
            .iter()
            .map(|vertex| vertex[0] + vertex[1] * 2.0)
            .collect();
        let target = vertices(&[-1.0, 0.0, 1.0], &[-1.0, 0.0, 1.0]);
        let expected: Vec<f32> = target
            .iter()
            .map(|vertex| vertex[0] + vertex[1] * 2.0)
            .collect();
        assert_eq!(
            resample_grid_depths(&descending, &descending_depths, &target).unwrap(),
            expected
        );
    }

    #[test]
    fn failed_resample_keeps_prefix_and_attached_points_follow_define_grid() {
        let old = vertices(&[-1.0, 0.0, 1.0], &[-2.0, 2.0]);
        assert!(resample_grid_depths(&old, &[0.0, 1.0, 0.0], &old).is_none());
        assert_eq!(resize_depths(&[0.0, 1.0, 0.0], 4), vec![0.0, 1.0, 0.0, 0.0]);
        assert_eq!(
            resize_depths(&[0.0, 1.0, 0.0, 4.0, 5.0], 3),
            vec![0.0, 1.0, 0.0]
        );

        let moved = vertices(&[0.0, 2.0], &[0.0, 2.0]);
        let attached =
            json!({"type": "attached-point", "index": 1, "amount": 0.25, "future": "keep"});
        let ring = json!({"type": "ring", "p0": [-1.0, 0.0], "future": "ring"});
        let remapped = remap_ops(&[attached, ring.clone()], &old[..4], &moved, false).unwrap();
        assert_eq!(remapped, vec![ring]);

        let square = vertices(&[-1.0, 1.0], &[-1.0, 1.0]);
        let fine = vertices(&[-1.0, 0.0, 1.0], &[-1.0, 0.0, 1.0]);
        let followed = remap_ops(
            &[json!({"type": "attached-point", "index": 1, "amount": 0.4, "future": "pt"})],
            &square,
            &fine,
            false,
        )
        .unwrap();
        assert_eq!(followed[0]["index"], 2);
        assert_eq!(followed[0]["future"], "pt");
        assert_eq!(followed[0]["amount"], json!(0.4));

        let wide = vertices(&[-1.0, 0.0, 1.0], &[-1.0, 1.0]);
        let tall = vertices(&[-1.0, 1.0], &[-1.0, 0.0, 1.0]);
        assert!(remap_ops(
            &[json!({"type": "attached-point", "index": 1, "amount": 0.2})],
            &wide,
            &tall,
            false,
        )
        .unwrap()
        .is_empty());
        let preserved = remap_ops(
            &[json!({"type": "attached-point", "index": 1, "amount": 0.2})],
            &wide,
            &tall,
            true,
        )
        .unwrap();
        assert_eq!(preserved[0]["index"], 1);

        let origin = vertices(&[0.0, 1.0], &[0.0, 1.0]);
        let nudged = vertices(&[1.0e-5, 1.0], &[0.0, 1.0]);
        let kept = remap_ops(
            &[json!({"type": "attached-point", "index": 0, "amount": 1.0})],
            &origin,
            &nudged,
            false,
        )
        .unwrap();
        assert_eq!(kept[0]["index"], 0);
        let shifted = vertices(&[1.0e-3, 1.0], &[0.0, 1.0]);
        assert!(remap_ops(
            &[json!({"type": "attached-point", "index": 0, "amount": 1.0})],
            &origin,
            &shifted,
            false,
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn path_depths_resample_by_normalized_control_point_index() {
        let mut path = json!({
            "vertices": [0.0, 0.0, 3.0, 0.0, 100.0, 0.0],
            "depths": [0.0, 0.5, 1.0],
        });
        prepare_path_depth_rebuffer(&path).unwrap();
        path["vertices"] = json!([0.0, 0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 100.0, 0.0]);
        rebuffer_path_depth_payload(&mut path, 3).unwrap();
        assert_eq!(path["depths"], json!([0.0, 0.25, 0.5, 0.75, 1.0]));

        let mut single = json!({"vertices": [0.0, 0.0], "depths": [0.1]});
        single["vertices"] = json!([0.0, 0.0, 2.0, 0.0, 4.0, 0.0]);
        rebuffer_path_depth_payload(&mut single, 1).unwrap();
        assert_eq!(
            single["depths"],
            json!([f64::from(0.1f32), f64::from(0.1f32), f64::from(0.1f32)])
        );
    }

    #[test]
    fn path_depth_rebuffer_falls_back_to_prefix_resize() {
        assert!(resample_path_depths(&[1.0, 2.0], 0, 3).is_none());
        assert_eq!(resize_depths(&[1.0, 2.0], 3), vec![1.0, 2.0, 0.0]);
        assert_eq!(resample_path_depths(&[4.0], 1, 0), Some(Vec::new()));
        assert_eq!(resample_path_depths(&[4.0], 1, 3), Some(vec![4.0; 3]));
        assert_eq!(resample_path_depths(&[1.0, 2.0], 2, 1), Some(vec![1.0]));
    }

    #[test]
    fn known_ops_must_parse_and_an_empty_remap_omits_the_base() {
        let mut malformed = json!({
            "grid_axis_x": [-1.0, 1.0],
            "grid_axis_y": [-1.0, 1.0],
            "depths": [0.0, 1.0, 0.0, 1.0],
            "depth-ops": [{"type": "attached-point", "amount": "bad"}],
            "depth-op-base-depths": [0.0, 0.0, 0.0, 0.0],
        });
        let old = grid_vertices(&malformed).unwrap();
        let error = rebuffer_grid_depth_payload(&mut malformed, &old).unwrap_err();
        assert!(error.contains("amount must be a number"), "{error}");
        assert_eq!(malformed["grid_axis_x"], json!([-1.0, 1.0]));
        assert_eq!(malformed["depths"], json!([0.0, 1.0, 0.0, 1.0]));

        let mut ring = json!({
            "grid_axis_x": [-1.0, 1.0],
            "grid_axis_y": [-1.0, 1.0],
            "depth-ops": [{"type": "ring", "p0": 1}],
        });
        let old = grid_vertices(&ring).unwrap();
        let error = rebuffer_grid_depth_payload(&mut ring, &old).unwrap_err();
        assert!(error.contains("p0 must be [x, y]"), "{error}");

        let mut dropped = json!({
            "grid_axis_x": [-1.0, 1.0],
            "grid_axis_y": [-1.0, 1.0],
            "depth-ops": [{"type": "attached-point", "index": 3, "amount": 0.5, "future": "keep"}],
            "depth-op-base-depths": [0.1, 0.2, 0.3, 0.4],
        });
        let old = grid_vertices(&dropped).unwrap();
        dropped["grid_axis_x"] = json!([0.0, 2.0]);
        dropped["grid_axis_y"] = json!([0.0, 2.0]);
        rebuffer_grid_depth_payload(&mut dropped, &old).unwrap();
        assert!(dropped.get("depth-ops").is_none());
        assert!(dropped.get(BASE_KEY).is_none());
    }
}
