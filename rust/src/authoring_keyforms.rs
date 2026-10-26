use crate::authoring_graph::Graph;
use crate::authoring_values::{float_array, Budget};
use roxmltree::Node;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KeyformGrid {
    pub(crate) axes: Vec<KeyformAxis>,
    pub(crate) cells: Vec<KeyformCell>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KeyformWeight {
    pub(crate) cell_index: usize,
    pub(crate) weight: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KeyformAxis {
    pub(crate) binding_ref: String,
    pub(crate) parameter_guid: String,
    pub(crate) parameter_id: Option<String>,
    pub(crate) keys: Vec<f64>,
    pub(crate) interpolation: String,
    pub(crate) extended_interpolation: String,
    pub(crate) insert_point_count: u32,
    pub(crate) extended_interpolation_scale: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KeyformCell {
    pub(crate) indices: Vec<usize>,
    pub(crate) form_guid: String,
}

impl KeyformGrid {
    pub(crate) fn linear_weights(
        &self,
        axis_values: &[f64],
        budget: &mut Budget,
    ) -> Result<Vec<KeyformWeight>, String> {
        self.repeating_weights(axis_values, &[], budget)
    }

    /// `linear_weights` for axes that may repeat: `periods[axis]` is the span of a repeating parameter. A sample
    /// outside the keys of such an axis lies between its last key and its first, one period on.
    pub(crate) fn repeating_weights(
        &self,
        axis_values: &[f64],
        periods: &[Option<f64>],
        budget: &mut Budget,
    ) -> Result<Vec<KeyformWeight>, String> {
        if axis_values.len() != self.axes.len() {
            return Err("authoring keyform sample has the wrong axis count".into());
        }
        if axis_values.iter().any(|value| !value.is_finite()) {
            return Err("authoring keyform sample has a non-finite value".into());
        }
        if self.cells.is_empty() {
            return Err("authoring keyform grid has no cells".into());
        }
        if self.axes.is_empty() && self.cells.len() != 1 {
            return Err("zero-axis authoring keyform grid must have one cell".into());
        }

        let work = self
            .axes
            .len()
            .checked_mul(self.cells.len())
            .ok_or("authoring keyform work exceeds scalar budget")?;
        budget.consume(work)?;
        let key_work = self.axes.iter().try_fold(0usize, |total, axis| {
            total
                .checked_add(axis.keys.len())
                .ok_or("authoring keyform key validation exceeds scalar budget")
        })?;
        budget.consume(key_work)?;
        budget.consume(self.axes.len())?;
        let mut brackets = Vec::with_capacity(self.axes.len());
        let mut expected_corners = 1usize;
        for (axis_index, (axis, value)) in self.axes.iter().zip(axis_values).enumerate() {
            if axis.interpolation != "LINEAR" || axis.extended_interpolation != "LINEAR" {
                return Err("authoring keyform interpolation is unsupported".into());
            }
            if axis.keys.is_empty() || axis.keys.iter().any(|key| !key.is_finite()) {
                return Err("authoring keyform axis keys are invalid".into());
            }
            if axis.keys.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err("authoring keyform axis keys are invalid".into());
            }
            let first = *axis
                .keys
                .first()
                .ok_or("authoring keyform axis keys are invalid")?;
            let last = *axis
                .keys
                .last()
                .ok_or("authoring keyform axis keys are invalid")?;
            let outside = *value < first || *value > last;
            let period = periods.get(axis_index).copied().flatten();
            let bracket = if let (true, Some(period)) = (outside, period) {
                let offset = if *value > last { *value - last } else { *value + period - last };
                let upper_weight = offset / (first + period - last);
                let last_index = axis.keys.len() - 1;
                // A sample on either end of the seam, exactly or after rounding, is on that key.
                let on = |index| AxisBracket { lower: index, upper: index, lower_weight: 1.0, upper_weight: 0.0 };
                if !upper_weight.is_finite() {
                    return Err("authoring keyform axis weights are not representable".into());
                } else if upper_weight >= 1.0 {
                    on(0)
                } else if upper_weight <= 0.0 || 1.0 - upper_weight <= 0.0 {
                    on(last_index)
                } else {
                    AxisBracket { lower: last_index, upper: 0, lower_weight: 1.0 - upper_weight, upper_weight }
                }
            } else if outside {
                return Err("authoring keyform sample is outside the key range".into());
            } else { match axis.keys.binary_search_by(|key| {
                if *key < *value {
                    std::cmp::Ordering::Less
                } else if *key > *value {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Equal
                }
            }) {
                Ok(index) => AxisBracket {
                    lower: index,
                    upper: index,
                    lower_weight: 1.0,
                    upper_weight: 0.0,
                },
                Err(upper) => {
                    let lower = upper
                        .checked_sub(1)
                        .ok_or("authoring keyform axis bracket is invalid")?;
                    let lower_key = *axis
                        .keys
                        .get(lower)
                        .ok_or("authoring keyform axis bracket is invalid")?;
                    let upper_key = *axis
                        .keys
                        .get(upper)
                        .ok_or("authoring keyform axis bracket is invalid")?;
                    let (lower_weight, upper_weight) =
                        axis_fractions(lower_key, upper_key, *value)?;
                    AxisBracket {
                        lower,
                        upper,
                        lower_weight,
                        upper_weight,
                    }
                }
            } };
            if bracket.lower != bracket.upper {
                expected_corners = expected_corners
                    .checked_mul(2)
                    .ok_or("authoring keyform corner count overflow")?;
            }
            brackets.push(bracket);
        }
        if expected_corners > self.cells.len() {
            return Err("authoring keyform grid is missing positive-weight corners".into());
        }

        budget.consume(self.cells.len())?;
        let mut coordinates = HashSet::with_capacity(self.cells.len());
        for cell in &self.cells {
            if cell.indices.len() != self.axes.len()
                || cell
                    .indices
                    .iter()
                    .zip(&self.axes)
                    .any(|(index, axis)| *index >= axis.keys.len())
            {
                return Err("authoring keyform cell coordinates are invalid".into());
            }
            if !coordinates.insert(cell.indices.as_slice()) {
                return Err("duplicate authoring keyform cell coordinate".into());
            }
        }

        budget.consume(expected_corners)?;
        let mut weights = Vec::with_capacity(expected_corners);
        for (cell_index, cell) in self.cells.iter().enumerate() {
            let mut weight = 1.0;
            for (index, bracket) in cell.indices.iter().zip(&brackets) {
                let factor = if bracket.lower == bracket.upper {
                    if *index != bracket.lower {
                        0.0
                    } else {
                        1.0
                    }
                } else if *index == bracket.lower {
                    bracket.lower_weight
                } else if *index == bracket.upper {
                    bracket.upper_weight
                } else {
                    0.0
                };
                if factor == 0.0 {
                    weight = 0.0;
                    break;
                }
                weight *= factor;
                if !weight.is_finite() || weight == 0.0 {
                    return Err("authoring keyform weight is not representable".into());
                }
            }
            if weight > 0.0 {
                weights.push(KeyformWeight { cell_index, weight });
            }
        }
        if weights.len() != expected_corners {
            return Err("authoring keyform grid is missing positive-weight corners".into());
        }
        Ok(weights)
    }
}

#[derive(Clone, Copy)]
struct AxisBracket {
    lower: usize,
    upper: usize,
    lower_weight: f64,
    upper_weight: f64,
}

fn axis_fractions(lower: f64, upper: f64, value: f64) -> Result<(f64, f64), String> {
    let span = upper - lower;
    let (lower_offset, upper_offset, span) = if span.is_finite() {
        (upper - value, value - lower, span)
    } else {
        (
            upper / 2.0 - value / 2.0,
            value / 2.0 - lower / 2.0,
            upper / 2.0 - lower / 2.0,
        )
    };
    let lower_weight = lower_offset / span;
    let upper_weight = upper_offset / span;
    if lower_weight.is_finite()
        && lower_weight > 0.0
        && upper_weight.is_finite()
        && upper_weight > 0.0
    {
        Ok((lower_weight, upper_weight))
    } else {
        Err("authoring keyform axis weights are not representable".into())
    }
}

pub(crate) fn read_grid<'d, 'i>(
    graph: &Graph<'d, 'i>,
    source: Node<'d, 'i>,
    field: &str,
    parameters: &HashMap<String, String>,
    allow_unresolved: bool,
    budget: &mut Budget,
) -> Result<Option<KeyformGrid>, String> {
    let Some(grid) = graph.field(source, field)? else {
        return Ok(None);
    };
    let grid = graph.resolve(grid)?;
    if grid.has_tag_name("null") {
        return Ok(None);
    }
    if !grid.has_tag_name("KeyformGridSource") {
        return Err("authoring grid field is not a KeyformGridSource".into());
    }

    let (binding_list, axis_count) = array_list(graph, grid, "keyformBindings")?;
    budget.consume(axis_count)?;
    let mut axes = Vec::with_capacity(axis_count);
    let mut axis_by_ref = HashMap::with_capacity(axis_count);
    let mut seen_bindings = HashSet::with_capacity(axis_count);
    for child in binding_list.children().filter(|child| child.is_element()) {
        let binding = graph.resolve(child)?;
        if !binding.has_tag_name("KeyformBindingSource") {
            return Err("authoring keyformBindings contains a non-binding source".into());
        }
        let binding_ref = binding_ref(graph, binding)?;
        if !seen_bindings.insert(binding_ref) {
            return Err("duplicate authoring keyform binding".into());
        }

        let parameter_guid = guid_field(graph, binding, "parameterGuid")?;
        let parameter_id = parameters
            .get(parameter_guid)
            .filter(|id| !id.trim().is_empty())
            .map(String::as_str);
        // Extended grids can keep bindings to parameters deleted in the editor; the GUID is recorded, not repaired.
        if parameter_id.is_none() && !allow_unresolved {
            return Err("unknown authoring parameter GUID".into());
        }
        let keys_node = required_field(graph, binding, "keys")?;
        let keys = float_array(graph, keys_node, budget)?;
        if keys.is_empty() {
            return Err("authoring keyform binding has no keys".into());
        }
        if keys.iter().any(|key| !key.is_finite()) {
            return Err("authoring keyform binding has a non-finite key".into());
        }
        if keys.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err("authoring keyform binding keys are not strictly increasing".into());
        }

        let interpolation = enum_field(graph, binding, "interpolationType")?;
        let extended_interpolation = enum_field(graph, binding, "extendedInterpolationType")?;
        let insert_point_count = text_field(graph, binding, "insertPointCount")?
            .parse::<u32>()
            .map_err(|_| "invalid authoring keyform insertPointCount".to_owned())?;
        let extended_interpolation_scale =
            text_field(graph, binding, "extendedInterpolationScale")?
                .parse::<f64>()
                .map_err(|_| "invalid authoring keyform extendedInterpolationScale".to_owned())?;
        if !extended_interpolation_scale.is_finite() {
            return Err("non-finite authoring keyform extendedInterpolationScale".into());
        }

        axis_by_ref.insert(binding_ref, axes.len());
        axes.push(KeyformAxis {
            binding_ref: budget.text(binding_ref)?,
            parameter_guid: budget.text(parameter_guid)?,
            parameter_id: parameter_id.map(|id| budget.text(id)).transpose()?,
            keys,
            interpolation: budget.text(interpolation)?,
            extended_interpolation: budget.text(extended_interpolation)?,
            insert_point_count,
            extended_interpolation_scale,
        });
    }

    let (cell_list, cell_count) = array_list(graph, grid, "keyformsOnGrid")?;
    if cell_count == 0 || (axes.is_empty() && cell_count != 1) {
        return Err("invalid authoring keyform grid cell count".into());
    }
    budget.consume(cell_count)?;
    let mut cells = Vec::with_capacity(cell_count);
    let mut seen_coordinates = HashSet::with_capacity(cell_count);
    for child in cell_list.children().filter(|child| child.is_element()) {
        let cell = graph.resolve(child)?;
        if !cell.has_tag_name("KeyformOnGrid") {
            return Err("authoring keyformsOnGrid contains a non-cell".into());
        }
        let access_key = required_field(graph, cell, "accessKey")?;
        let (coordinate_list, coordinate_count) =
            array_list(graph, access_key, "_keyOnParameterList")?;
        if coordinate_count != axes.len() {
            return Err("authoring keyform cell has the wrong coordinate count".into());
        }
        budget.consume(axes.len())?;
        let mut indices = vec![usize::MAX; axes.len()];
        for coordinate in coordinate_list
            .children()
            .filter(|coordinate| coordinate.is_element())
        {
            let coordinate = graph.resolve(coordinate)?;
            if !coordinate.has_tag_name("KeyOnParameter") {
                return Err("authoring keyform access key contains a non-coordinate".into());
            }
            let binding = required_field(graph, coordinate, "binding")?;
            let binding_ref = binding_ref(graph, binding)?;
            let axis_index = *axis_by_ref.get(binding_ref).ok_or_else(|| {
                format!("authoring keyform cell refers to unknown binding {binding_ref:?}")
            })?;
            let key_index = text_field(graph, coordinate, "keyIndex")?
                .parse::<usize>()
                .map_err(|_| "invalid authoring keyform keyIndex".to_owned())?;
            if key_index >= axes[axis_index].keys.len() {
                return Err("authoring keyform keyIndex is out of range".into());
            }
            if indices[axis_index] != usize::MAX {
                return Err("duplicate authoring keyform cell coordinate".into());
            }
            indices[axis_index] = key_index;
        }
        if indices.iter().any(|index| *index == usize::MAX) {
            return Err("authoring keyform cell has an incomplete coordinate".into());
        }
        budget.consume(indices.len())?;
        if !seen_coordinates.insert(indices.clone()) {
            return Err("duplicate authoring keyform cell coordinate".into());
        }
        cells.push(KeyformCell {
            indices,
            form_guid: budget.text(guid_field(graph, cell, "keyformGuid")?)?,
        });
    }

    Ok(Some(KeyformGrid { axes, cells }))
}

fn required_field<'d, 'i>(
    graph: &Graph<'d, 'i>,
    node: Node<'d, 'i>,
    name: &str,
) -> Result<Node<'d, 'i>, String> {
    graph
        .field(node, name)?
        .ok_or_else(|| format!("authoring keyform field {name} is missing"))
}

fn array_list<'d, 'i>(
    graph: &Graph<'d, 'i>,
    node: Node<'d, 'i>,
    name: &str,
) -> Result<(Node<'d, 'i>, usize), String> {
    let list = required_field(graph, node, name)?;
    if !list.has_tag_name("array_list") {
        return Err(format!(
            "authoring keyform field {name} is not an array_list"
        ));
    }
    let count = list
        .attribute("count")
        .ok_or_else(|| format!("authoring keyform field {name} lacks count"))?
        .parse::<usize>()
        .map_err(|_| format!("invalid authoring keyform field {name} count"))?;
    let actual = list.children().filter(|child| child.is_element()).count();
    if actual != count {
        return Err(format!("authoring keyform field {name} count mismatch"));
    }
    Ok((list, count))
}

fn binding_ref<'d, 'i>(graph: &Graph<'d, 'i>, node: Node<'d, 'i>) -> Result<&'d str, String> {
    let binding = graph.resolve(node)?;
    if !binding.has_tag_name("KeyformBindingSource") {
        return Err("authoring keyform binding reference has the wrong type".into());
    }
    binding
        .attribute("xs.id")
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| "authoring keyform binding lacks xs.id".into())
}

fn guid_field<'d, 'i>(
    graph: &Graph<'d, 'i>,
    node: Node<'d, 'i>,
    name: &str,
) -> Result<&'d str, String> {
    let guid = graph.resolve(required_field(graph, node, name)?)?;
    guid.attribute("uuid")
        .map(str::trim)
        .filter(|guid| !guid.is_empty())
        .ok_or_else(|| format!("authoring keyform field {name} lacks a GUID"))
}

fn text_field<'d, 'i>(
    graph: &Graph<'d, 'i>,
    node: Node<'d, 'i>,
    name: &str,
) -> Result<&'d str, String> {
    graph
        .field(node, name)?
        .and_then(|field| field.text())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| format!("authoring keyform field {name} is missing text"))
}

fn enum_field<'d, 'i>(
    graph: &Graph<'d, 'i>,
    node: Node<'d, 'i>,
    name: &str,
) -> Result<&'d str, String> {
    let value = required_field(graph, node, name)?
        .attribute("v")
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("authoring keyform field {name} lacks v"))?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use roxmltree::Document;

    fn kernel_axis(keys: Vec<f64>) -> KeyformAxis {
        KeyformAxis {
            binding_ref: String::new(),
            parameter_guid: String::new(),
            parameter_id: Some(String::new()),
            keys,
            interpolation: "LINEAR".into(),
            extended_interpolation: "LINEAR".into(),
            insert_point_count: 0,
            extended_interpolation_scale: 1.0,
        }
    }

    fn kernel_grid(keysets: Vec<Vec<f64>>, coordinates: Vec<Vec<usize>>) -> KeyformGrid {
        KeyformGrid {
            axes: keysets.into_iter().map(kernel_axis).collect(),
            cells: coordinates
                .into_iter()
                .enumerate()
                .map(|(index, indices)| KeyformCell {
                    indices,
                    form_guid: format!("form-{index}"),
                })
                .collect(),
        }
    }

    fn binary_grid(axis_count: usize) -> KeyformGrid {
        let cell_count = 1 << axis_count;
        let coordinates = (0..cell_count)
            .map(|mask| (0..axis_count).map(|axis| (mask >> axis) & 1).collect())
            .collect();
        kernel_grid(vec![vec![0.0, 1.0]; axis_count], coordinates)
    }

    fn assert_weight(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-12, "{actual} != {expected}");
    }

    #[test]
    fn linear_weights_preserve_asymmetric_two_axis_cell_order() {
        let grid = kernel_grid(
            vec![vec![0.0, 10.0], vec![-5.0, 5.0]],
            vec![vec![1, 1], vec![0, 1], vec![1, 0], vec![0, 0]],
        );
        let weights = grid
            .linear_weights(&[2.5, 1.0], &mut Budget::new(100))
            .unwrap();
        assert_eq!(
            weights
                .iter()
                .map(|item| item.cell_index)
                .collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
        for (actual, expected) in weights
            .iter()
            .map(|item| item.weight)
            .zip([0.15, 0.45, 0.10, 0.30])
        {
            assert_weight(actual, expected);
        }
        assert_eq!(
            serde_json::to_value(&weights[0]).unwrap(),
            serde_json::json!({"cellIndex": 0, "weight": weights[0].weight})
        );
    }

    #[test]
    fn a_repeating_axis_joins_its_last_key_to_its_first_across_the_seam() {
        // Keys 0, 1 and 3 on a parameter 0..4: the seam cell runs from 3 round to 4, which is 0 again.
        let grid = kernel_grid(vec![vec![0.0, 1.0, 3.0]], vec![vec![0], vec![1], vec![2]]);
        let weights = |value: f64, period| grid.repeating_weights(&[value], &[period], &mut Budget::new(100));
        let seam = weights(3.25, Some(4.0)).unwrap();
        assert_eq!(seam.iter().map(|item| item.cell_index).collect::<Vec<_>>(), [0, 2]);
        assert_weight(seam[0].weight, 0.25);
        assert_weight(seam[1].weight, 0.75);
        let inside = weights(2.0, Some(4.0)).unwrap();
        assert_eq!(inside.iter().map(|item| item.cell_index).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(weights(3.25, None).unwrap_err(), "authoring keyform sample is outside the key range");
        // Keys 1 and 3: below the first key the sample is on the same seam cell, 2 long.
        let grid = kernel_grid(vec![vec![1.0, 3.0]], vec![vec![0], vec![1]]);
        let below = grid.repeating_weights(&[0.5], &[Some(4.0)], &mut Budget::new(100)).unwrap();
        assert_weight(below[0].weight, 0.75);
        assert_weight(below[1].weight, 0.25);
        // The maximum is the minimum one turn on. A sample there is on the first key, and the mirror on the last.
        let grid = kernel_grid(vec![vec![0.0, 1.0, 3.0]], vec![vec![0], vec![1], vec![2]]);
        let end = grid.repeating_weights(&[4.0], &[Some(4.0)], &mut Budget::new(100)).unwrap();
        assert_eq!((end.len(), end[0].cell_index), (1, 0));
        let grid = kernel_grid(vec![vec![1.0, 3.0, 4.0]], vec![vec![0], vec![1], vec![2]]);
        let start = grid.repeating_weights(&[0.0], &[Some(4.0)], &mut Budget::new(100)).unwrap();
        assert_eq!((start.len(), start[0].cell_index), (1, 2));
        let lone = kernel_grid(vec![vec![1.0]], vec![vec![0]]);
        assert_weight(lone.repeating_weights(&[3.0], &[Some(4.0)], &mut Budget::new(100)).unwrap()[0].weight, 1.0);
    }

    #[test]
    fn linear_weights_collapse_exact_keys_including_endpoints() {
        let grid = kernel_grid(
            vec![vec![0.0, 0.5, 1.0], vec![0.0, 1.0]],
            vec![
                vec![0, 0],
                vec![0, 1],
                vec![1, 0],
                vec![1, 1],
                vec![2, 0],
                vec![2, 1],
            ],
        );
        for (values, index) in [([0.0, 1.0], 1), ([0.5, 1.0], 3), ([1.0, 1.0], 5)] {
            let weights = grid.linear_weights(&values, &mut Budget::new(100)).unwrap();
            assert_eq!(weights.len(), 1);
            assert_eq!(weights[0].cell_index, index);
            assert_eq!(weights[0].weight, 1.0);
        }
    }

    #[test]
    fn linear_weights_preserve_representable_fractions_near_each_endpoint() {
        let grid = kernel_grid(vec![vec![-1.0, 1.0]], vec![vec![0], vec![1]]);
        let near_upper = grid
            .linear_weights(
                &[f64::from_bits(1.0f64.to_bits() - 1)],
                &mut Budget::new(100),
            )
            .unwrap();
        assert_eq!(near_upper[0].weight, 2.0f64.powi(-54));
        assert_eq!(near_upper[1].weight, 1.0);

        let near_lower = grid
            .linear_weights(&[-1.0 + f64::EPSILON / 2.0], &mut Budget::new(100))
            .unwrap();
        assert_eq!(near_lower[0].weight, 1.0);
        assert_eq!(near_lower[1].weight, 2.0f64.powi(-54));
    }

    #[test]
    fn linear_weights_handle_five_axis_uniform_and_asymmetric_products() {
        let grid = binary_grid(5);
        let uniform = grid
            .linear_weights(&[0.5; 5], &mut Budget::new(1_000))
            .unwrap();
        assert_eq!(uniform.len(), 32);
        assert!(uniform
            .iter()
            .all(|item| (item.weight - 1.0 / 32.0).abs() < 1e-12));

        let values = [0.1, 0.2, 0.3, 0.4, 0.5];
        let asymmetric = grid
            .linear_weights(&values, &mut Budget::new(1_000))
            .unwrap();
        assert_eq!(asymmetric.len(), 32);
        for (cell_index, item) in asymmetric.iter().enumerate() {
            assert_eq!(item.cell_index, cell_index);
            let expected = (0..5).fold(1.0, |weight, axis| {
                if (cell_index >> axis) & 1 == 0 {
                    weight * (1.0 - values[axis])
                } else {
                    weight * values[axis]
                }
            });
            assert_weight(item.weight, expected);
        }
    }

    #[test]
    fn linear_weights_reject_missing_corners_but_allow_an_exact_sparse_cell() {
        let grid = kernel_grid(
            vec![vec![0.0, 1.0], vec![0.0, 1.0]],
            vec![vec![0, 0], vec![1, 0], vec![0, 1]],
        );
        assert!(grid
            .linear_weights(&[0.5, 0.5], &mut Budget::new(100))
            .unwrap_err()
            .contains("missing positive-weight corners"));
        let exact = grid
            .linear_weights(&[0.0, 0.0], &mut Budget::new(100))
            .unwrap();
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].cell_index, 0);
        assert_eq!(exact[0].weight, 1.0);
    }

    #[test]
    fn linear_weights_reject_unsupported_modes_invalid_samples_and_bad_cells() {
        let grid = binary_grid(1);
        let mut unsupported = grid.clone();
        unsupported.axes[0].interpolation = "CUBIC".into();
        assert!(unsupported
            .linear_weights(&[0.5], &mut Budget::new(100))
            .unwrap_err()
            .contains("unsupported"));
        unsupported.axes[0].interpolation = "LINEAR".into();
        unsupported.axes[0].extended_interpolation = "ELLIPSE".into();
        assert!(unsupported
            .linear_weights(&[0.5], &mut Budget::new(100))
            .unwrap_err()
            .contains("unsupported"));
        assert!(grid
            .linear_weights(&[-f64::EPSILON], &mut Budget::new(100))
            .is_err());
        assert!(grid
            .linear_weights(&[f64::NAN], &mut Budget::new(100))
            .is_err());
        assert!(grid.linear_weights(&[], &mut Budget::new(100)).is_err());

        let mut malformed = grid.clone();
        malformed.axes[0].keys = vec![1.0, 0.0];
        assert!(malformed
            .linear_weights(&[0.5], &mut Budget::new(100))
            .is_err());
        malformed = grid.clone();
        malformed.cells[0].indices[0] = usize::MAX;
        assert!(malformed
            .linear_weights(&[0.5], &mut Budget::new(100))
            .is_err());
        malformed = grid.clone();
        malformed.cells[0].indices[0] = malformed.cells[1].indices[0];
        assert!(malformed
            .linear_weights(&[0.5], &mut Budget::new(100))
            .unwrap_err()
            .contains("duplicate"));
    }

    #[test]
    fn linear_weights_bound_work_and_handle_constant_and_wide_range_grids() {
        let grid = binary_grid(2);
        assert!(grid
            .linear_weights(&[0.5, 0.5], &mut Budget::new(0))
            .unwrap_err()
            .contains("budget"));
        assert!(binary_grid(1)
            .linear_weights(&[0.5], &mut Budget::new(8))
            .unwrap_err()
            .contains("budget"));

        let singleton = kernel_grid(
            vec![vec![2.0], vec![0.0, 1.0]],
            vec![vec![0, 0], vec![0, 1]],
        );
        let weights = singleton
            .linear_weights(&[2.0, 0.25], &mut Budget::new(100))
            .unwrap();
        assert_eq!(weights.len(), 2);
        assert_weight(weights[0].weight, 0.75);
        assert_weight(weights[1].weight, 0.25);
        assert!(singleton
            .linear_weights(&[2.1, 0.25], &mut Budget::new(100))
            .is_err());

        let constant = kernel_grid(vec![], vec![vec![]]);
        let weights = constant.linear_weights(&[], &mut Budget::new(10)).unwrap();
        assert_eq!(weights.len(), 1);
        assert_eq!(weights[0].cell_index, 0);
        assert_eq!(weights[0].weight, 1.0);

        let wide = kernel_grid(vec![vec![-f64::MAX, f64::MAX]], vec![vec![0], vec![1]]);
        let weights = wide.linear_weights(&[0.0], &mut Budget::new(100)).unwrap();
        assert_eq!(weights.len(), 2);
        assert_weight(weights[0].weight, 0.5);
        assert_weight(weights[1].weight, 0.5);
    }

    fn xml(parameters: HashMap<String, String>, text: &str) -> Result<Option<KeyformGrid>, String> {
        let document = Document::parse(text).map_err(|error| error.to_string())?;
        let graph = Graph::new(&document)?;
        let source = document
            .descendants()
            .find(|node| node.has_tag_name("Owner"))
            .ok_or_else(|| "missing test owner".to_owned())?;
        read_grid(
            &graph,
            source,
            "keyformGridSource",
            &parameters,
            false,
            &mut Budget::new(10_000),
        )
    }

    fn parameters() -> HashMap<String, String> {
        HashMap::from([
            ("parameter-guid-a".into(), "ParamA".into()),
            ("parameter-guid-b".into(), "ParamB".into()),
        ])
    }

    fn two_axis_grid() -> String {
        r#"<root>
            <Owner><KeyformGridSource xs.n="keyformGridSource" xs.ref="grid" /></Owner>
            <KeyformGridSource xs.id="grid">
                <array_list xs.n="keyformsOnGrid" count="2">
                    <KeyformOnGrid>
                        <KeyformGridAccessKey xs.n="accessKey">
                            <array_list xs.n="_keyOnParameterList" count="2">
                                <KeyOnParameter><KeyformBindingSource xs.n="binding" xs.ref="axis-b-alias" /><i xs.n="keyIndex">1</i></KeyOnParameter>
                                <KeyOnParameter><KeyformBindingSource xs.n="binding" xs.ref="axis-a-alias" /><i xs.n="keyIndex">0</i></KeyOnParameter>
                            </array_list>
                        </KeyformGridAccessKey>
                        <CFormGuid xs.n="keyformGuid" xs.ref="form-one" />
                    </KeyformOnGrid>
                    <KeyformOnGrid>
                        <KeyformGridAccessKey xs.n="accessKey">
                            <array_list xs.n="_keyOnParameterList" count="2">
                                <KeyOnParameter><KeyformBindingSource xs.n="binding" xs.ref="axis-b-alias" /><i xs.n="keyIndex">0</i></KeyOnParameter>
                                <KeyOnParameter><KeyformBindingSource xs.n="binding" xs.ref="axis-a-alias" /><i xs.n="keyIndex">1</i></KeyOnParameter>
                            </array_list>
                        </KeyformGridAccessKey>
                        <CFormGuid xs.n="keyformGuid" xs.ref="form-two" />
                    </KeyformOnGrid>
                </array_list>
                <array_list xs.n="keyformBindings" count="2">
                    <KeyformBindingSource xs.ref="axis-a-alias" />
                    <KeyformBindingSource xs.ref="axis-b-alias" />
                </array_list>
            </KeyformGridSource>
            <KeyformBindingSource xs.id="axis-a-alias" xs.ref="axis-a" />
            <KeyformBindingSource xs.id="axis-b-alias" xs.ref="axis-b" />
            <KeyformBindingSource xs.id="axis-a">
                <CParameterGuid xs.n="parameterGuid" xs.ref="parameter-a" />
                <array_list xs.n="keys" count="2"><f>-40.0</f><f>40.0</f></array_list>
                <InterpolationType xs.n="interpolationType" v="LINEAR" />
                <ExtendedInterpolationType xs.n="extendedInterpolationType" v="ELLIPSE" />
                <i xs.n="insertPointCount">2</i>
                <f xs.n="extendedInterpolationScale">1.5</f>
            </KeyformBindingSource>
            <KeyformBindingSource xs.id="axis-b">
                <CParameterGuid xs.n="parameterGuid" xs.ref="parameter-b" />
                <array_list xs.n="keys" count="2"><f>-1.0</f><f>1.0</f></array_list>
                <InterpolationType xs.n="interpolationType" v="LINEAR" />
                <ExtendedInterpolationType xs.n="extendedInterpolationType" v="SNS_CURVE" />
                <i xs.n="insertPointCount">1</i>
                <f xs.n="extendedInterpolationScale">1.0</f>
            </KeyformBindingSource>
            <CParameterGuid xs.id="parameter-a" uuid="parameter-guid-a" />
            <CParameterGuid xs.id="parameter-b" uuid="parameter-guid-b" />
            <CFormGuid xs.id="form-one" uuid="form-guid-one" />
            <CFormGuid xs.id="form-two" uuid="form-guid-two" />
        </root>"#
            .to_owned()
    }

    #[test]
    fn reads_reordered_binding_aliases_and_sparse_cells() {
        let grid = xml(parameters(), &two_axis_grid()).unwrap().unwrap();
        assert_eq!(grid.axes.len(), 2);
        assert_eq!(grid.axes[0].binding_ref, "axis-a");
        assert_eq!(grid.axes[0].parameter_guid, "parameter-guid-a");
        assert_eq!(grid.axes[0].parameter_id.as_deref(), Some("ParamA"));
        assert_eq!(grid.axes[0].keys, [-40.0, 40.0]);
        assert_eq!(grid.axes[0].interpolation, "LINEAR");
        assert_eq!(grid.axes[0].extended_interpolation, "ELLIPSE");
        assert_eq!(grid.axes[1].extended_interpolation, "SNS_CURVE");
        assert_eq!(grid.cells.len(), 2);
        assert_eq!(grid.cells[0].indices, [0, 1]);
        assert_eq!(grid.cells[0].form_guid, "form-guid-one");
        assert_eq!(grid.cells[1].indices, [1, 0]);
    }

    #[test]
    fn rejects_non_finite_keys_out_of_range_indices_and_duplicates() {
        assert!(xml(parameters(), &two_axis_grid().replace("-40.0", "NaN")).is_err());

        let out_of_range = two_axis_grid().replace(
            "xs.ref=\"axis-b-alias\" /><i xs.n=\"keyIndex\">1</i>",
            "xs.ref=\"axis-b-alias\" /><i xs.n=\"keyIndex\">2</i>",
        );
        assert!(xml(parameters(), &out_of_range).is_err());

        let duplicate_axis = two_axis_grid().replace(
            "<KeyformBindingSource xs.ref=\"axis-b-alias\" />\n                </array_list>",
            "<KeyformBindingSource xs.ref=\"axis-a-alias\" />\n                </array_list>",
        );
        assert!(xml(parameters(), &duplicate_axis).is_err());

        let duplicate_cell = two_axis_grid()
            .replace(
                "xs.ref=\"axis-b-alias\" /><i xs.n=\"keyIndex\">0</i>",
                "xs.ref=\"axis-b-alias\" /><i xs.n=\"keyIndex\">1</i>",
            )
            .replace(
                "xs.ref=\"axis-a-alias\" /><i xs.n=\"keyIndex\">1</i>",
                "xs.ref=\"axis-a-alias\" /><i xs.n=\"keyIndex\">0</i>",
            );
        assert!(xml(parameters(), &duplicate_cell).is_err());
    }

    #[test]
    fn unresolved_parameter_is_an_error_unless_the_grid_allows_it() {
        let text = two_axis_grid().replace("keyformGridSource", "extendedKeyformGridSource");
        let document = Document::parse(&text).unwrap();
        let graph = Graph::new(&document).unwrap();
        let owner = document.descendants().find(|node| node.has_tag_name("Owner")).unwrap();
        let known = HashMap::from([("parameter-guid-a".to_owned(), "ParamA".to_owned())]);
        let read = |allow| {
            read_grid(&graph, owner, "extendedKeyformGridSource", &known, allow, &mut Budget::new(10_000))
        };
        assert_eq!(read(false).unwrap_err(), "unknown authoring parameter GUID");
        let grid = read(true).unwrap().unwrap();
        assert_eq!(grid.axes[0].parameter_id.as_deref(), Some("ParamA"));
        assert_eq!(grid.axes[1].parameter_id, None);
        assert_eq!(grid.axes[1].parameter_guid, "parameter-guid-b");
        assert_eq!(serde_json::to_value(&grid.axes[1]).unwrap()["parameterId"], serde_json::Value::Null);
    }

    #[test]
    fn reads_zero_axis_grid_as_one_empty_coordinate_cell() {
        let text = r#"<root>
            <Owner><KeyformGridSource xs.n="keyformGridSource" xs.ref="grid" /></Owner>
            <KeyformGridSource xs.id="grid">
                <array_list xs.n="keyformBindings" count="0" />
                <array_list xs.n="keyformsOnGrid" count="1">
                    <KeyformOnGrid>
                        <KeyformGridAccessKey xs.n="accessKey">
                            <array_list xs.n="_keyOnParameterList" count="0" />
                        </KeyformGridAccessKey>
                        <CFormGuid xs.n="keyformGuid" xs.ref="form" />
                    </KeyformOnGrid>
                </array_list>
            </KeyformGridSource>
            <CFormGuid xs.id="form" uuid="form-guid" />
        </root>"#;
        let grid = xml(HashMap::new(), text).unwrap().unwrap();
        assert!(grid.axes.is_empty());
        assert_eq!(grid.cells.len(), 1);
        assert!(grid.cells[0].indices.is_empty());
    }

    #[test]
    fn grid_text_metadata_is_bounded_before_copying_parameter_ids() {
        let text = two_axis_grid();
        let document = Document::parse(&text).unwrap();
        let graph = Graph::new(&document).unwrap();
        let source = document
            .descendants()
            .find(|node| node.has_tag_name("Owner"))
            .unwrap();
        let mut params = parameters();
        params.insert("parameter-guid-a".into(), "a".repeat(1024));
        let error = read_grid(
            &graph,
            source,
            "keyformGridSource",
            &params,
            false,
            &mut Budget::new(10_000).with_text_limit(1024),
        )
        .unwrap_err();
        assert!(error.contains("copied-text byte budget"), "{error}");
    }

    #[test]
    fn reads_named_extended_grids_and_preserves_null_as_absent() {
        let text = two_axis_grid().replace("keyformGridSource", "extendedKeyformGridSource");
        let document = Document::parse(&text).unwrap();
        let graph = Graph::new(&document).unwrap();
        let owner = document
            .descendants()
            .find(|node| node.has_tag_name("Owner"))
            .unwrap();
        assert!(read_grid(
            &graph,
            owner,
            "keyformGridSource",
            &parameters(),
            false,
            &mut Budget::new(10_000)
        )
        .unwrap()
        .is_none());
        let grid = read_grid(
            &graph,
            owner,
            "extendedKeyformGridSource",
            &parameters(),
            false,
            &mut Budget::new(10_000),
        )
        .unwrap()
        .unwrap();
        assert_eq!(grid.axes.len(), 2);
        assert_eq!(grid.cells.len(), 2);
        let document = Document::parse(
            r#"<root><Owner><null xs.n="extendedKeyformGridSource"/></Owner></root>"#,
        )
        .unwrap();
        let graph = Graph::new(&document).unwrap();
        let owner = document
            .descendants()
            .find(|node| node.has_tag_name("Owner"))
            .unwrap();
        assert!(read_grid(
            &graph,
            owner,
            "extendedKeyformGridSource",
            &parameters(),
            false,
            &mut Budget::new(10_000)
        )
        .unwrap()
        .is_none());
    }
}
