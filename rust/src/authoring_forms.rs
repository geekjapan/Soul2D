use crate::authoring_scene::{Form, FormData, FormWeight, MorphWeight};
use crate::authoring_values::Budget;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PrimaryForm {
    coordinate_type: Option<String>,
    opacity: f64,
    draw_order: Option<f64>,
    multiply_color: [f64; 4],
    screen_color: [f64; 4],
    data: FormData,
}

struct Corner<'a> {
    weight: f64,
    forms: HashMap<Option<&'a str>, &'a Form>,
}

pub(crate) fn blend_primary_forms(
    forms: &[Form],
    weights: &[FormWeight],
    budget: &mut Budget,
) -> Result<Vec<PrimaryForm>, String> {
    if weights.is_empty() {
        return Err("authoring primary forms have no active weights".into());
    }

    budget.consume(weights.len())?;
    for weight in weights {
        if !weight.weight.is_finite() || weight.weight <= 0.0 {
            return Err("authoring primary form weights must be finite and positive".into());
        }
    }

    budget.consume(forms.len())?;
    let mut forms_by_guid: HashMap<&str, Vec<usize>> = HashMap::with_capacity(forms.len());
    for (index, form) in forms.iter().enumerate() {
        forms_by_guid
            .entry(form.guid.as_str())
            .or_default()
            .push(index);
    }

    budget.consume(weights.len())?;
    let mut corners = Vec::with_capacity(weights.len());
    let mut first_indices = None;
    for weight in weights {
        let indices = forms_by_guid
            .get(weight.form_guid.as_str())
            .ok_or("authoring primary form GUID was not found in its owner")?;
        budget.consume(indices.len())?;
        let mut corner_forms = HashMap::with_capacity(indices.len());
        for &index in indices {
            let form = &forms[index];
            if corner_forms
                .insert(form.coordinate_type.as_deref(), form)
                .is_some()
            {
                return Err("duplicate authoring primary form representation".into());
            }
        }
        if first_indices.is_none() {
            first_indices = Some(indices.as_slice());
        }
        corners.push(Corner {
            weight: weight.weight,
            forms: corner_forms,
        });
    }

    let first_indices = first_indices.ok_or("authoring primary forms have no active weights")?;
    let representation_count = first_indices.len();
    if corners
        .iter()
        .any(|corner| corner.forms.len() != representation_count)
    {
        return Err("authoring primary form coordinate types differ between corners".into());
    }

    let lookup_count = representation_count
        .checked_mul(corners.len())
        .ok_or("authoring primary form work exceeds scalar budget")?;
    budget.consume(lookup_count)?;
    let mut representations = Vec::with_capacity(representation_count);
    for &index in first_indices {
        let first = &forms[index];
        let coordinate_type = first.coordinate_type.as_deref();
        let mut corner_forms: Vec<&Form> = Vec::with_capacity(corners.len());
        for corner in &corners {
            let form = corner
                .forms
                .get(&coordinate_type)
                .copied()
                .ok_or("authoring primary form coordinate types differ between corners")?;
            if let Some(base) = corner_forms.first() {
                compatible(base, form)?;
            }
            corner_forms.push(form);
        }
        representations.push(corner_forms);
    }

    let mut output_scalars = representation_count;
    let mut blend_terms = 0usize;
    for representation in &representations {
        let scalar_count = scalar_count(representation[0])?;
        output_scalars = output_scalars
            .checked_add(scalar_count)
            .ok_or("authoring primary form work exceeds scalar budget")?;
        blend_terms = blend_terms
            .checked_add(
                scalar_count
                    .checked_mul(corners.len())
                    .ok_or("authoring primary form work exceeds scalar budget")?,
            )
            .ok_or("authoring primary form work exceeds scalar budget")?;
    }
    budget.consume(output_scalars)?;
    budget.consume(blend_terms)?;

    let mut output = Vec::with_capacity(representation_count);
    for (first_index, representation) in first_indices.iter().zip(&representations) {
        let first = &forms[*first_index];
        let coordinate_type = first
            .coordinate_type
            .as_deref()
            .map(|value| budget.text(value))
            .transpose()?;
        let mut multiply_color = [0.0; 4];
        let mut screen_color = [0.0; 4];
        for component in 0..4 {
            multiply_color[component] = blend_scalar(&corners, representation, |form| {
                Ok(form.multiply_color[component])
            })?;
            screen_color[component] = blend_scalar(&corners, representation, |form| {
                Ok(form.screen_color[component])
            })?;
        }
        let draw_order = if first.draw_order.is_some() {
            Some(blend_scalar(&corners, representation, |form| {
                form.draw_order
                    .map(|value| value as f64)
                    .ok_or_else(|| "authoring primary form drawOrder presence differs".into())
            })?)
        } else {
            None
        };

        output.push(PrimaryForm {
            coordinate_type,
            opacity: blend_scalar(&corners, representation, |form| Ok(form.opacity))?,
            draw_order,
            multiply_color,
            screen_color,
            data: blend_data(&corners, representation)?,
        });
    }
    Ok(output)
}

pub(crate) fn blend_morph_forms(
    forms: &[Form],
    extended_forms: &[&Form],
    primary_weights: &[FormWeight],
    base_form_guid: &str,
    morph_weights: &[MorphWeight],
    budget: &mut Budget,
) -> Result<Vec<PrimaryForm>, String> {
    let mut output = blend_primary_forms(forms, primary_weights, budget)?;
    let forms_by_guid = form_indices_by_guid(forms, extended_forms, budget)?;
    let base_indices = forms_by_guid
        .get(base_form_guid)
        .ok_or("authoring morph default form GUID was not found in its owner")?;
    budget.consume(base_indices.len())?;
    let mut base_forms = HashMap::with_capacity(base_indices.len());
    for &form in base_indices {
        if base_forms
            .insert(form.coordinate_type.as_deref(), form)
            .is_some()
        {
            return Err("duplicate authoring morph default form representation".into());
        }
    }
    if base_forms.is_empty() {
        return Err("authoring morph default form has no representations".into());
    }
    validate_neutral_output(&base_forms, &output, budget)?;

    budget.consume(morph_weights.len())?;
    for morph in morph_weights {
        if !morph.weight.is_finite() || !(0.0..=1.0).contains(&morph.weight) {
            return Err("authoring morph weights must be finite and in range".into());
        }
        let target_indices = forms_by_guid
            .get(morph.form_guid.as_str())
            .ok_or("authoring morph target form GUID was not found in its owner")?;
        budget.consume(target_indices.len())?;
        let mut target_forms = HashMap::with_capacity(target_indices.len());
        for &form in target_indices {
            if target_forms
                .insert(form.coordinate_type.as_deref(), form)
                .is_some()
            {
                return Err("duplicate authoring morph target representation".into());
            }
        }
        if target_forms.len() != base_forms.len() {
            return Err("authoring morph target coordinate types differ from its default".into());
        }
        let work = base_forms.values().try_fold(0usize, |total, form| {
            let count = scalar_count(form)?;
            total
                .checked_add(count)
                .ok_or_else(|| "authoring morph work exceeds scalar budget".to_owned())
        })?;
        budget.consume(work)?;
        for (coordinate_type, base) in &base_forms {
            let target = target_forms
                .get(coordinate_type)
                .copied()
                .ok_or("authoring morph target coordinate types differ from its default")?;
            compatible(base, target)?;
            validate_morph_channels(base, target)?;
            if morph.weight == 0.0 {
                continue;
            }
            budget.consume(output.len())?;
            let evaluated = output
                .iter_mut()
                .find(|form| form.coordinate_type.as_deref() == *coordinate_type)
                .ok_or("authoring morph output coordinate types differ from its default")?;
            evaluated.opacity = add_weighted_delta(evaluated.opacity, base.opacity, target.opacity, morph.weight)?;
            // A draw-order difference (Mao's PartRabbit drops 900 on ParamRabbitDraworder) adds like opacity.
            if let (Some(current), Some(base_order), Some(target_order)) = (evaluated.draw_order, base.draw_order, target.draw_order) {
                evaluated.draw_order = Some(add_weighted_delta(current, f64::from(base_order), f64::from(target_order), morph.weight)?);
            }
            add_morph_delta(&mut evaluated.data, &base.data, &target.data, morph.weight)?;
        }
    }
    for form in &mut output {
        form.opacity = form.opacity.clamp(0.0, 1.0);
    }
    Ok(output)
}

fn validate_neutral_output(
    base_forms: &HashMap<Option<&str>, &Form>,
    output: &[PrimaryForm],
    budget: &mut Budget,
) -> Result<(), String> {
    budget.consume(output.len())?;
    if output.len() != base_forms.len() {
        return Err("authoring primary output coordinate types differ from morph default".into());
    }
    let mut seen = HashSet::with_capacity(output.len());
    for evaluated in output {
        let coordinate_type = evaluated.coordinate_type.as_deref();
        if !seen.insert(coordinate_type) {
            return Err("duplicate authoring primary output representation".into());
        }
        let neutral = base_forms
            .get(&coordinate_type)
            .copied()
            .ok_or("authoring primary output coordinate types differ from morph default")?;
        compatible_output(neutral, evaluated)?;
        budget.consume(scalar_count(neutral)?)?;
    }
    Ok(())
}

fn compatible_output(base: &Form, output: &PrimaryForm) -> Result<(), String> {
    match (&base.data, &output.data) {
        (FormData::ArtMesh { positions: base }, FormData::ArtMesh { positions: output })
        | (FormData::Warp { positions: base }, FormData::Warp { positions: output }) => {
            if base.len() != output.len() {
                return Err("authoring primary output position counts differ from morph default".into());
            }
        }
        (
            FormData::Rotation {
                reflect_x: base_x,
                reflect_y: base_y,
                ..
            },
            FormData::Rotation {
                reflect_x: output_x,
                reflect_y: output_y,
                ..
            },
        ) => {
            if base_x != output_x || base_y != output_y {
                return Err("authoring primary output reflection differs from morph default".into());
            }
        }
        (FormData::Part, FormData::Part) => {}
        (FormData::Glue { .. }, FormData::Glue { .. }) => {}
        _ => {
            return Err("authoring primary output data type differs from morph default".into())
        }
    }
    Ok(())
}

fn form_indices_by_guid<'a>(
    forms: &'a [Form],
    extended_forms: &[&'a Form],
    budget: &mut Budget,
) -> Result<HashMap<&'a str, Vec<&'a Form>>, String> {
    budget.consume(forms.len().saturating_add(extended_forms.len()))?;
    let mut forms_by_guid: HashMap<&str, Vec<&Form>> = HashMap::with_capacity(forms.len());
    for form in forms.iter().chain(extended_forms.iter().copied()) {
        forms_by_guid
            .entry(form.guid.as_str())
            .or_default()
            .push(form);
    }
    Ok(forms_by_guid)
}

fn validate_morph_channels(base: &Form, target: &Form) -> Result<(), String> {
    if !(0.0..=1.0).contains(&base.opacity) || !(0.0..=1.0).contains(&target.opacity) {
        return Err("authoring morph opacity must be in range".into());
    }
    // shortcut: colour morph domains and units need validation before their deltas are evaluated.
    if base
            .multiply_color
            .iter()
            .zip(target.multiply_color)
            .any(|(left, right)| left.to_bits() != right.to_bits())
        || base
            .screen_color
            .iter()
            .zip(target.screen_color)
            .any(|(left, right)| left.to_bits() != right.to_bits())
    {
        return Err("authoring morph scalar channels are unsupported".into());
    }
    match (&base.data, &target.data) {
        (FormData::ArtMesh { .. }, FormData::ArtMesh { .. })
        | (FormData::Warp { .. }, FormData::Warp { .. })
        | (FormData::Rotation { .. }, FormData::Rotation { .. })
        | (FormData::Glue { .. }, FormData::Glue { .. }) => Ok(()),
        (FormData::Part, FormData::Part) => {
            Ok(())
        }
        _ => Err("authoring morph form data types differ".into()),
    }
}

fn add_morph_delta(
    current: &mut FormData,
    base: &FormData,
    target: &FormData,
    weight: f64,
) -> Result<(), String> {
    match (current, base, target) {
        (
            FormData::ArtMesh { positions: current },
            FormData::ArtMesh { positions: base },
            FormData::ArtMesh { positions: target },
        )
        | (
            FormData::Warp { positions: current },
            FormData::Warp { positions: base },
            FormData::Warp { positions: target },
        ) => {
            for ((current, base), target) in current.iter_mut().zip(base).zip(target) {
                *current = add_weighted_delta(*current, *base, *target, weight)?;
            }
        }
        (
            FormData::Rotation {
                angle: current_angle,
                origin: current_origin,
                scale: current_scale,
                ..
            },
            FormData::Rotation {
                angle: base_angle,
                origin: base_origin,
                scale: base_scale,
                ..
            },
            FormData::Rotation {
                angle: target_angle,
                origin: target_origin,
                scale: target_scale,
                ..
            },
        ) => {
            *current_angle = add_weighted_delta(*current_angle, *base_angle, *target_angle, weight)?;
            *current_scale = add_weighted_delta(*current_scale, *base_scale, *target_scale, weight)?;
            for component in 0..2 {
                current_origin[component] = add_weighted_delta(
                    current_origin[component],
                    base_origin[component],
                    target_origin[component],
                    weight,
                )?;
            }
        }
        (
            FormData::Glue {
                intensity: current,
            },
            FormData::Glue { intensity: base },
            FormData::Glue { intensity: target },
        ) => {
            *current = add_weighted_delta(*current, *base, *target, weight)?;
        }
        // A Part has no geometry; its opacity and draw order are blended by the caller.
        (FormData::Part, FormData::Part, FormData::Part) => {}
        _ => return Err("authoring morph form data types differ".into()),
    }
    Ok(())
}

fn add_weighted_delta(current: f64, base: f64, target: f64, weight: f64) -> Result<f64, String> {
    let delta = target - base;
    let contribution = delta * weight;
    let output = current + contribution;
    if delta.is_finite() && contribution.is_finite() && output.is_finite() {
        Ok(output)
    } else {
        Err("authoring morph geometry is non-finite".into())
    }
}

fn compatible(base: &Form, other: &Form) -> Result<(), String> {
    if base.draw_order.is_some() != other.draw_order.is_some() {
        return Err("authoring primary form drawOrder presence differs".into());
    }
    match (&base.data, &other.data) {
        (FormData::ArtMesh { positions: left }, FormData::ArtMesh { positions: right })
        | (FormData::Warp { positions: left }, FormData::Warp { positions: right }) => {
            if left.len() != right.len() {
                return Err("authoring primary form position counts differ".into());
            }
        }
        (
            FormData::Rotation {
                reflect_x: left_x,
                reflect_y: left_y,
                ..
            },
            FormData::Rotation {
                reflect_x: right_x,
                reflect_y: right_y,
                ..
            },
        ) => {
            if left_x != right_x || left_y != right_y {
                return Err("authoring primary form reflection differs (unsupported)".into());
            }
        }
        (FormData::Part, FormData::Part) => {}
        (FormData::Glue { .. }, FormData::Glue { .. }) => {}
        _ => return Err("authoring primary form data types differ".into()),
    }
    Ok(())
}

fn scalar_count(form: &Form) -> Result<usize, String> {
    let data_count = match &form.data {
        FormData::ArtMesh { positions } | FormData::Warp { positions } => positions.len(),
        FormData::Rotation { .. } => 6,
        FormData::Part => 0,
        FormData::Glue { .. } => 1,
        FormData::ArtPath { points } => points.len(),
    };
    9usize
        .checked_add(usize::from(form.draw_order.is_some()))
        .and_then(|count| count.checked_add(data_count))
        .ok_or_else(|| "authoring primary form work exceeds scalar budget".into())
}

fn blend_scalar(
    corners: &[Corner<'_>],
    forms: &[&Form],
    value: impl Fn(&Form) -> Result<f64, String>,
) -> Result<f64, String> {
    let mut sum = None;
    for (corner, form) in corners.iter().zip(forms) {
        let product = corner.weight * value(form)?;
        if !product.is_finite() {
            return Err("authoring primary form interpolation is non-finite".into());
        }
        let next = sum.map_or(product, |sum| sum + product);
        if !next.is_finite() {
            return Err("authoring primary form interpolation is non-finite".into());
        }
        sum = Some(next);
    }
    sum.ok_or_else(|| "authoring primary forms have no active weights".into())
}

fn blend_data(corners: &[Corner<'_>], forms: &[&Form]) -> Result<FormData, String> {
    match &forms[0].data {
        FormData::ArtMesh { positions } | FormData::Warp { positions } => {
            let art_mesh = matches!(&forms[0].data, FormData::ArtMesh { .. });
            let mut output = Vec::with_capacity(positions.len());
            for index in 0..positions.len() {
                output.push(blend_scalar(corners, forms, |form| {
                    match (&form.data, art_mesh) {
                        (FormData::ArtMesh { positions }, true)
                        | (FormData::Warp { positions }, false) => positions
                            .get(index)
                            .copied()
                            .ok_or_else(|| "authoring primary form position counts differ".into()),
                        _ => Err("authoring primary form data types differ".into()),
                    }
                })?);
            }
            if art_mesh {
                Ok(FormData::ArtMesh { positions: output })
            } else {
                Ok(FormData::Warp { positions: output })
            }
        }
        FormData::Rotation {
            angle: _,
            origin: _,
            scale: _,
            reflect_x,
            reflect_y,
        } => {
            let angle = blend_scalar(corners, forms, |form| match &form.data {
                FormData::Rotation { angle, .. } => Ok(*angle),
                _ => Err("authoring primary form data types differ".into()),
            })?;
            let mut blended_origin = [0.0; 2];
            for component in 0..2 {
                blended_origin[component] =
                    blend_scalar(corners, forms, |form| match &form.data {
                        FormData::Rotation { origin, .. } => Ok(origin[component]),
                        _ => Err("authoring primary form data types differ".into()),
                    })?;
            }
            let scale = blend_scalar(corners, forms, |form| match &form.data {
                FormData::Rotation { scale, .. } => Ok(*scale),
                _ => Err("authoring primary form data types differ".into()),
            })?;
            Ok(FormData::Rotation {
                angle,
                origin: blended_origin,
                scale,
                reflect_x: *reflect_x,
                reflect_y: *reflect_y,
            })
        }
        FormData::Part => Ok(FormData::Part),
        FormData::Glue { .. } => Ok(FormData::Glue {
            intensity: blend_scalar(corners, forms, |form| match &form.data {
                FormData::Glue { intensity } => Ok(*intensity),
                _ => Err("authoring primary form data types differ".into()),
            })?,
        }),
        FormData::ArtPath { .. } => Err("authoring ArtPath forms are not evaluated".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(
        guid: &str,
        coordinate_type: Option<&str>,
        opacity: f64,
        draw_order: Option<i32>,
        data: FormData,
    ) -> Form {
        Form {
            guid: guid.into(),
            coordinate_type: coordinate_type.map(str::to_owned),
            animated: false,
            local_animated: false,
            opacity,
            draw_order,
            multiply_color: [1.0, 2.0, 3.0, 4.0],
            screen_color: [5.0, 6.0, 7.0, 8.0],
            vertex_count_matches_source: None,
            data,
        }
    }

    fn weight(form_guid: &str, weight: f64) -> FormWeight {
        FormWeight {
            cell_index: 0,
            form_guid: form_guid.into(),
            weight,
        }
    }

    fn artmesh(guid: &str, coordinate_type: Option<&str>, positions: Vec<f64>) -> Form {
        form(
            guid,
            coordinate_type,
            1.0,
            Some(0),
            FormData::ArtMesh { positions },
        )
    }

    #[test]
    fn blends_two_and_three_corners_without_normalizing() {
        let forms = vec![
            form(
                "a",
                Some("mesh"),
                2.0,
                Some(1),
                FormData::ArtMesh {
                    positions: vec![10.0],
                },
            ),
            form(
                "b",
                Some("mesh"),
                4.0,
                Some(3),
                FormData::ArtMesh {
                    positions: vec![30.0],
                },
            ),
            form(
                "c",
                Some("mesh"),
                8.0,
                Some(5),
                FormData::ArtMesh {
                    positions: vec![50.0],
                },
            ),
        ];
        let output = blend_primary_forms(
            &forms,
            &[weight("a", 0.25), weight("b", 0.5)],
            &mut Budget::new(100),
        )
        .unwrap();
        assert_eq!(output[0].opacity, 2.5);
        assert_eq!(output[0].draw_order, Some(1.75));
        assert_eq!(output[0].multiply_color, [0.75, 1.5, 2.25, 3.0]);
        assert_eq!(output[0].screen_color, [3.75, 4.5, 5.25, 6.0]);
        match &output[0].data {
            FormData::ArtMesh { positions } => assert_eq!(positions, &[17.5]),
            _ => panic!("expected ArtMesh"),
        }

        let output = blend_primary_forms(
            &forms,
            &[weight("a", 0.2), weight("b", 0.3), weight("c", 0.5)],
            &mut Budget::new(100),
        )
        .unwrap();
        assert_eq!(output[0].opacity, 5.6);
        assert!((output[0].draw_order.unwrap() - 3.6).abs() < 1e-12);
        match &output[0].data {
            FormData::ArtMesh { positions } => assert_eq!(positions, &[36.0]),
            _ => panic!("expected ArtMesh"),
        }
    }

    #[test]
    fn blends_raw_glue_intensity_without_normalizing_or_clamping() {
        let forms = [
            form("a", None, 1.0, None, FormData::Glue { intensity: 2.0 }),
            form("b", None, 1.0, None, FormData::Glue { intensity: 4.0 }),
        ];
        let primary = blend_primary_forms(
            &forms,
            &[weight("a", 0.25), weight("b", 0.5)],
            &mut Budget::new(100),
        )
        .unwrap();
        assert_eq!(primary[0].coordinate_type, None);
        assert!(matches!(&primary[0].data, FormData::Glue { intensity } if *intensity == 2.5));

        let morph_forms = [
            form("base", None, 1.0, None, FormData::Glue { intensity: 2.5 }),
            form("target", None, 1.0, None, FormData::Glue { intensity: 3.5 }),
        ];
        let morphed = blend_morph_forms(
            &morph_forms,
            &[],
            &[weight("base", 1.0)],
            "base",
            &[morph_weight("GlueIntensity", "target", 0.5)],
            &mut Budget::new(100),
        )
        .unwrap();
        assert!(matches!(&morphed[0].data, FormData::Glue { intensity } if *intensity == 3.0));
        assert!(blend_morph_forms(
            &morph_forms,
            &[],
            &[weight("base", 1.0)],
            "base",
            &[morph_weight("GlueIntensity", "target", 1.5)],
            &mut Budget::new(100),
        )
        .is_err());
    }

    #[test]
    fn preserves_coordinate_representations_and_first_corner_order() {
        let forms = vec![
            artmesh("a", Some("mesh"), vec![0.0]),
            artmesh("a", Some("warp"), vec![10.0]),
            artmesh("b", Some("warp"), vec![20.0]),
            artmesh("b", Some("mesh"), vec![30.0]),
        ];
        let output = blend_primary_forms(
            &forms,
            &[weight("a", 0.5), weight("b", 0.5)],
            &mut Budget::new(200),
        )
        .unwrap();
        assert_eq!(output.len(), 2);
        assert_eq!(output[0].coordinate_type.as_deref(), Some("mesh"));
        assert_eq!(output[1].coordinate_type.as_deref(), Some("warp"));
        assert!(matches!(&output[0].data, FormData::ArtMesh { positions } if positions == &[15.0]));
        assert!(matches!(&output[1].data, FormData::ArtMesh { positions } if positions == &[15.0]));

        assert!(blend_primary_forms(
            &[artmesh("other-owner", Some("mesh"), vec![1.0])],
            &[weight("a", 1.0)],
            &mut Budget::new(100),
        )
        .err()
        .unwrap()
        .contains("owner"));
    }

    #[test]
    fn preserves_raw_rotation_angles_and_exact_corners() {
        let forms = vec![
            form(
                "a",
                None,
                0.25,
                None,
                FormData::Rotation {
                    angle: 350.0,
                    origin: [0.0, 4.0],
                    scale: 1.0,
                    reflect_x: false,
                    reflect_y: true,
                },
            ),
            form(
                "b",
                None,
                0.75,
                None,
                FormData::Rotation {
                    angle: 10.0,
                    origin: [10.0, 12.0],
                    scale: 3.0,
                    reflect_x: false,
                    reflect_y: true,
                },
            ),
        ];
        let output = blend_primary_forms(
            &forms,
            &[weight("a", 0.5), weight("b", 0.5)],
            &mut Budget::new(200),
        )
        .unwrap();
        assert_eq!(output[0].opacity, 0.5);
        match &output[0].data {
            FormData::Rotation {
                angle,
                origin,
                scale,
                reflect_x,
                reflect_y,
            } => {
                assert_eq!(*angle, 180.0);
                assert_eq!(*origin, [5.0, 8.0]);
                assert_eq!(*scale, 2.0);
                assert!(!reflect_x);
                assert!(*reflect_y);
            }
            _ => panic!("expected Rotation"),
        }

        let exact =
            blend_primary_forms(&forms, &[weight("a", 1.0)], &mut Budget::new(100)).unwrap();
        match &exact[0].data {
            FormData::Rotation { angle, origin, .. } => {
                assert_eq!(*angle, 350.0);
                assert_eq!(*origin, [0.0, 4.0]);
            }
            _ => panic!("expected Rotation"),
        }
    }

    #[test]
    fn rejects_invalid_corner_shapes_weights_and_local_forms() {
        let mismatch = [
            artmesh("a", Some("mesh"), vec![1.0]),
            artmesh("b", Some("warp"), vec![2.0]),
        ];
        assert!(blend_primary_forms(
            &mismatch,
            &[weight("a", 0.5), weight("b", 0.5)],
            &mut Budget::new(100),
        )
        .err()
        .unwrap()
        .contains("coordinate types"));

        let duplicate = [
            artmesh("a", Some("mesh"), vec![1.0]),
            artmesh("a", Some("mesh"), vec![2.0]),
        ];
        assert!(
            blend_primary_forms(&duplicate, &[weight("a", 1.0)], &mut Budget::new(100),)
                .err()
                .unwrap()
                .contains("duplicate")
        );

        let lengths = [
            artmesh("a", Some("mesh"), vec![1.0]),
            artmesh("b", Some("mesh"), vec![2.0, 3.0]),
        ];
        assert!(blend_primary_forms(
            &lengths,
            &[weight("a", 0.5), weight("b", 0.5)],
            &mut Budget::new(100),
        )
        .err()
        .unwrap()
        .contains("counts"));

        let kinds = [
            artmesh("a", Some("mesh"), vec![1.0]),
            form("b", Some("mesh"), 1.0, Some(0), FormData::Part),
        ];
        assert!(blend_primary_forms(
            &kinds,
            &[weight("a", 0.5), weight("b", 0.5)],
            &mut Budget::new(100),
        )
        .err()
        .unwrap()
        .contains("data types"));

        assert!(blend_primary_forms(&[], &[], &mut Budget::new(100))
            .err()
            .unwrap()
            .contains("no active"));
        for invalid in [0.0, -0.5, f64::NAN, f64::INFINITY] {
            assert!(blend_primary_forms(
                &[artmesh("a", Some("mesh"), vec![1.0])],
                &[weight("a", invalid)],
                &mut Budget::new(100),
            )
            .is_err());
        }
    }

    #[test]
    fn rejects_reflection_draw_order_and_nonfinite_output_mismatches() {
        let reflected = [
            form(
                "a",
                Some("rotation"),
                1.0,
                None,
                FormData::Rotation {
                    angle: 0.0,
                    origin: [0.0, 0.0],
                    scale: 1.0,
                    reflect_x: false,
                    reflect_y: false,
                },
            ),
            form(
                "b",
                Some("rotation"),
                1.0,
                None,
                FormData::Rotation {
                    angle: 1.0,
                    origin: [0.0, 0.0],
                    scale: 1.0,
                    reflect_x: true,
                    reflect_y: false,
                },
            ),
        ];
        assert!(blend_primary_forms(
            &reflected,
            &[weight("a", 0.5), weight("b", 0.5)],
            &mut Budget::new(100),
        )
        .err()
        .unwrap()
        .contains("unsupported"));

        let draw_order = [
            artmesh("a", Some("mesh"), vec![1.0]),
            form(
                "b",
                Some("mesh"),
                1.0,
                None,
                FormData::ArtMesh {
                    positions: vec![2.0],
                },
            ),
        ];
        assert!(blend_primary_forms(
            &draw_order,
            &[weight("a", 0.5), weight("b", 0.5)],
            &mut Budget::new(100),
        )
        .err()
        .unwrap()
        .contains("drawOrder"));

        let huge = [artmesh("a", Some("mesh"), vec![f64::MAX])];
        assert!(
            blend_primary_forms(&huge, &[weight("a", 2.0)], &mut Budget::new(100),)
                .err()
                .unwrap()
                .contains("non-finite")
        );
    }

    #[test]
    fn charges_indexing_output_and_string_copy_against_budget() {
        let forms = [artmesh("a", Some("representation-name"), vec![1.0])];
        assert!(blend_primary_forms(&forms, &[weight("a", 1.0)], &mut Budget::new(16)).is_err());
        assert!(blend_primary_forms(
            &forms,
            &[weight("a", 1.0)],
            &mut Budget::new(100).with_text_limit(4),
        )
        .is_err());
    }

    fn morph_weight(parameter_id: &str, form_guid: &str, weight: f64) -> MorphWeight {
        MorphWeight {
            parameter_id: parameter_id.into(),
            key_value: 1.0,
            form_guid: form_guid.into(),
            weight,
        }
    }

    #[test]
    fn morph_output_representation_lookups_charge_work() {
        let mut forms = Vec::new();
        for index in 0..10 {
            let coordinate = format!("space-{index}");
            forms.push(artmesh("base", Some(&coordinate), vec![1.0, 2.0]));
            forms.push(artmesh("target", Some(&coordinate), vec![3.0, 4.0]));
        }
        assert!(blend_morph_forms(&forms, &[], &[weight("base", 1.0)], "base",
            &[morph_weight("ParamA", "target", 0.5)], &mut Budget::new(600)).is_err());
        let output = blend_morph_forms(&forms, &[], &[weight("base", 1.0)], "base",
            &[morph_weight("ParamA", "target", 0.5)], &mut Budget::new(700)).unwrap();
        assert_eq!(output.len(), 10);
        assert!(output.iter().all(|form|
            matches!(&form.data, FormData::ArtMesh { positions } if positions == &[2.0, 3.0])));
    }

    #[test]
    fn adds_independent_artmesh_deltas_to_primary_geometry() {
        let forms = [
            artmesh("base", Some("mesh"), vec![1.0, 2.0]),
            artmesh("first", Some("mesh"), vec![3.0, 4.0]),
            artmesh("second", Some("mesh"), vec![5.0, 6.0]),
        ];
        let output = blend_morph_forms(
            &forms, &[],
            &[weight("base", 1.0)],
            "base",
            &[
                morph_weight("ParamA", "first", 0.5),
                morph_weight("ParamB", "second", 0.25),
            ],
            &mut Budget::new(1000),
        )
        .unwrap();
        assert!(matches!(
            &output[0].data,
            FormData::ArtMesh { positions } if positions == &[3.0, 4.0]
        ));

        let warp_forms = [
            form(
                "base",
                Some("warp"),
                1.0,
                None,
                FormData::Warp {
                    positions: vec![0.0, 2.0],
                },
            ),
            form(
                "target",
                Some("warp"),
                1.0,
                None,
                FormData::Warp {
                    positions: vec![4.0, 6.0],
                },
            ),
        ];
        let warp_output = blend_morph_forms(
            &warp_forms, &[],
            &[weight("base", 1.0)],
            "base",
            &[morph_weight("ParamA", "target", 0.5)],
            &mut Budget::new(1000),
        )
        .unwrap();
        assert!(matches!(
            &warp_output[0].data,
            FormData::Warp { positions } if positions == &[2.0, 4.0]
        ));
    }

    #[test]
    fn adds_rotation_angle_and_origin_while_preserving_scale_and_reflections() {
        let forms = [
            form(
                "base",
                Some("rotation"),
                1.0,
                None,
                FormData::Rotation {
                    angle: 10.0,
                    origin: [2.0, 4.0],
                    scale: 1.5,
                    reflect_x: false,
                    reflect_y: true,
                },
            ),
            form(
                "target",
                Some("rotation"),
                1.0,
                None,
                FormData::Rotation {
                    angle: 30.0,
                    origin: [6.0, 8.0],
                    scale: 1.5,
                    reflect_x: false,
                    reflect_y: true,
                },
            ),
        ];
        let output = blend_morph_forms(
            &forms, &[],
            &[weight("base", 1.0)],
            "base",
            &[morph_weight("ParamTurn", "target", 0.25)],
            &mut Budget::new(1000),
        )
        .unwrap();
        assert!(matches!(
            &output[0].data,
            FormData::Rotation { angle, origin, scale, reflect_x, reflect_y }
                if *angle == 15.0 && *origin == [3.0, 5.0] && *scale == 1.5
                    && !*reflect_x && *reflect_y
        ));
    }

    #[test]
    fn validates_inactive_morph_targets_and_rejects_unsupported_changes() {
        let base = artmesh("base", Some("mesh"), vec![1.0]);
        let mut scalar_change = artmesh("target", Some("mesh"), vec![2.0]);
        scalar_change.multiply_color[0] = 0.5;
        assert!(blend_morph_forms(
            &[base, scalar_change], &[],
            &[weight("base", 1.0)],
            "base",
            &[morph_weight("ParamA", "target", 0.0)],
            &mut Budget::new(1000),
        )
        .err().unwrap()
        .contains("scalar channels"));

        let base = form(
            "base",
            Some("rotation"),
            1.0,
            None,
            FormData::Rotation {
                angle: 0.0,
                origin: [0.0, 0.0],
                scale: 1.0,
                reflect_x: false,
                reflect_y: false,
            },
        );
        let scaled = form(
            "target",
            Some("rotation"),
            1.0,
            None,
            FormData::Rotation {
                angle: 1.0,
                origin: [0.0, 0.0],
                scale: 2.0,
                reflect_x: false,
                reflect_y: false,
            },
        );
        let blended = blend_morph_forms(
            &[base, scaled], &[],
            &[weight("base", 1.0)],
            "base",
            &[morph_weight("ParamA", "target", 0.5)],
            &mut Budget::new(1000),
        )
        .unwrap();
        assert!(matches!(blended[0].data, FormData::Rotation { angle, scale, .. }
            if (angle - 0.5).abs() < 1e-12 && (scale - 1.5).abs() < 1e-12));

        let part = form("base", Some("part"), 1.0, Some(900), FormData::Part);
        let dropped = form("target", Some("part"), 1.0, Some(0), FormData::Part);
        let blended = blend_morph_forms(
            &[part, dropped], &[],
            &[weight("base", 1.0)],
            "base",
            &[morph_weight("ParamA", "target", 0.5)],
            &mut Budget::new(1000),
        )
        .unwrap();
        assert_eq!(blended[0].draw_order, Some(450.0));
    }

    #[test]
    fn morph_opacity_sums_all_deltas_before_clamping() {
        let mut base = artmesh("base", Some("mesh"), vec![0.0]);
        base.opacity = 0.5;
        let mut positive = artmesh("positive", Some("mesh"), vec![1.0]);
        positive.opacity = 1.0;
        let mut negative = artmesh("negative", Some("mesh"), vec![2.0]);
        negative.opacity = 0.0;
        let forms = [base, positive, negative];
        for (weights, expected) in [
            (vec![morph_weight("A", "positive", 1.0), morph_weight("B", "positive", 1.0), morph_weight("C", "negative", 1.0)], 1.0),
            (vec![morph_weight("A", "positive", 1.0), morph_weight("B", "positive", 1.0), morph_weight("C", "negative", 0.5)], 1.0),
            (vec![morph_weight("A", "negative", 1.0), morph_weight("B", "negative", 1.0)], 0.0),
        ] {
            let output = blend_morph_forms(&forms, &[], &[weight("base", 1.0)], "base", &weights, &mut Budget::new(1000)).unwrap();
            assert_eq!(output[0].opacity, expected);
        }
        let mut invalid = artmesh("target", Some("mesh"), vec![1.0]);
        invalid.opacity = 1.1;
        assert!(blend_morph_forms(&[artmesh("base", Some("mesh"), vec![0.0]), invalid], &[], &[weight("base", 1.0)], "base",
            &[morph_weight("A", "target", 0.0)], &mut Budget::new(1000)).is_err());
    }

    #[test]
    fn morph_geometry_checks_lengths_reflections_and_scalar_budget() {
        let base = artmesh("base", Some("mesh"), vec![1.0]);
        let wrong_length = artmesh("target", Some("mesh"), vec![1.0, 2.0]);
        assert!(blend_morph_forms(
            &[base, wrong_length], &[],
            &[weight("base", 1.0)],
            "base",
            &[morph_weight("ParamA", "target", 0.0)],
            &mut Budget::new(1000),
        )
        .is_err());

        let reflected = [
            form(
                "base",
                Some("rotation"),
                1.0,
                None,
                FormData::Rotation {
                    angle: 0.0,
                    origin: [0.0, 0.0],
                    scale: 1.0,
                    reflect_x: false,
                    reflect_y: false,
                },
            ),
            form(
                "target",
                Some("rotation"),
                1.0,
                None,
                FormData::Rotation {
                    angle: 1.0,
                    origin: [0.0, 0.0],
                    scale: 1.0,
                    reflect_x: true,
                    reflect_y: false,
                },
            ),
        ];
        assert!(blend_morph_forms(
            &reflected, &[],
            &[weight("base", 1.0)],
            "base",
            &[morph_weight("ParamA", "target", 0.0)],
            &mut Budget::new(1000),
        )
        .is_err());
        assert!(blend_morph_forms(
            &[artmesh("base", Some("mesh"), vec![1.0])], &[],
            &[weight("base", 1.0)],
            "base",
            &[],
            &mut Budget::new(1),
        )
        .is_err());
    }

    #[test]
    fn morph_output_must_match_the_neutral_primary_shape_even_when_inactive() {
        let length_mismatch = [
            artmesh("base", Some("mesh"), vec![1.0]),
            artmesh("current", Some("mesh"), vec![1.0, 2.0]),
        ];
        assert!(blend_morph_forms(
            &length_mismatch, &[],
            &[weight("current", 1.0)],
            "base",
            &[],
            &mut Budget::new(1000),
        )
        .err().unwrap()
        .contains("position counts"));

        let representation_mismatch = [
            artmesh("base", Some("neutral-space"), vec![1.0]),
            artmesh("current", Some("sample-space"), vec![1.0]),
        ];
        assert!(blend_morph_forms(
            &representation_mismatch, &[],
            &[weight("current", 1.0)],
            "base",
            &[],
            &mut Budget::new(1000),
        )
        .err().unwrap()
        .contains("coordinate types"));
    }
}
