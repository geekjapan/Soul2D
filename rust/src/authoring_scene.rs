use crate::authoring_graph::Graph;
use crate::authoring_keyforms::{read_grid, KeyformGrid};
use crate::authoring_values::{self as values, Budget};
use crate::caff::CaffDocument;
use roxmltree::{Document, Node, ParsingOptions};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

const MAX_SCALARS: usize = 5_000_000;

#[derive(Serialize)]
struct PrimaryWeights {
    format: &'static str,
    sources: Vec<SourceWeights>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceWeights {
    guid: String,
    id: String,
    axis_values: Vec<f64>,
    weights: Option<Vec<FormWeight>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FormWeight {
    pub(crate) cell_index: usize,
    pub(crate) form_guid: String,
    pub(crate) weight: f64,
}

#[derive(Serialize)]
struct PrimaryForms {
    format: &'static str,
    sources: Vec<SourceForms>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceForms {
    guid: String,
    id: String,
    parent_part_guid: Option<String>,
    target_deformer_guid: Option<String>,
    axis_values: Vec<f64>,
    weights: Option<Vec<FormWeight>>,
    evaluated_forms: Option<Vec<crate::authoring_forms::PrimaryForm>>,
}

#[derive(Serialize)]
struct MorphForms {
    format: &'static str,
    sources: Vec<MorphSourceForms>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MorphSourceForms {
    guid: String,
    id: String,
    parent_part_guid: Option<String>,
    target_deformer_guid: Option<String>,
    axis_values: Vec<f64>,
    weights: Option<Vec<FormWeight>>,
    evaluated_forms: Option<Vec<crate::authoring_forms::PrimaryForm>>,
    base_form_guid: Option<String>,
    morph_weights: Vec<MorphWeight>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    extended_morph_extensions: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MorphWeight {
    pub(crate) parameter_id: String,
    pub(crate) key_value: f64,
    pub(crate) form_guid: String,
    pub(crate) weight: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Scene {
    pub(crate) format: &'static str,
    pub(crate) file_format_version: Option<String>,
    pub(crate) name: String,
    pub(crate) canvas: [u32; 2],
    pub(crate) root_part_guid: String,
    pub(crate) parameters: Vec<Parameter>,
    pub(crate) sources: Vec<Source>,
    pub(crate) textures: Vec<Texture>,
    /// The ArtPath brush list, read only. Empty when the file stores no brush.
    pub(crate) brushes: Vec<Brush>,
    /// The `physicsSettingsSourceSet` element's verbatim XML: the canonical store of PHY01 physics edits. Kept
    /// byte-exact so unknown markup and full-precision numbers round-trip through save.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) physics_source: Option<String>,
}

pub(crate) struct ParsedScene {
    scene: Scene,
    post_parse_budget: Budget,
}

impl ParsedScene {
    /// A scene built in memory (a runtime `.moc3` read by `moc3::runtime_scene`) with a fresh evaluation budget.
    pub(crate) fn from_scene(scene: Scene) -> Self {
        Self { scene, post_parse_budget: Budget::new(MAX_SCALARS) }
    }

    pub(crate) fn source_value(&self) -> Result<Value, String> {
        serde_json::to_value(&self.scene)
            .map_err(|error| format!("could not serialize authoring source: {error}"))
    }

    pub(crate) fn linear_weights(&self, request: &Value) -> Result<Value, String> {
        let mut budget = self.post_parse_budget.clone();
        weights_for_scene(&self.scene, request, &mut budget)
    }

    pub(crate) fn primary_forms(&self, request: &Value) -> Result<Value, String> {
        let mut budget = self.post_parse_budget.clone();
        primary_forms_for_scene(&self.scene, request, &mut budget)
    }

    pub(crate) fn morph_forms(&self, request: &Value) -> Result<Value, String> {
        let mut budget = self.post_parse_budget.clone();
        morph_forms_for_scene(&self.scene, request, &mut budget)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Parameter {
    pub(crate) guid: String,
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) minimum: f64,
    pub(crate) maximum: f64,
    pub(crate) default: f64,
    pub(crate) repeat: bool,
    pub(crate) parameter_type: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Source {
    pub(crate) guid: String,
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) parent_part_guid: Option<String>,
    pub(crate) target_deformer_guid: Option<String>,
    pub(crate) visible: bool,
    pub(crate) locked: bool,
    /// An ArtMesh's user data text, written out as `.userdata3.json`; absent when empty.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) user_data: String,
    pub(crate) forms: Vec<Form>,
    pub(crate) grid: Option<KeyformGrid>,
    pub(crate) morph_targets: Option<MorphTargets>,
    pub(crate) extended_interpolations: Vec<ExtendedInterpolation>,
    pub(crate) data: SourceData,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExtendedInterpolation {
    guid: String,
    grid: Option<KeyformGrid>,
    normal_forms: Vec<Form>,
    morph_forms: Vec<Form>,
    morph_targets: Option<ExtendedMorphTargets>,
    /// Cell form GUIDs the file never stored (generated inserted points); recorded, not repaired.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    missing_form_guids: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExtendedMorphTargets {
    targets: Vec<ExtendedMorphTarget>,
    settings: Vec<ExtendedInterpolationSetting>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExtendedMorphTarget {
    parameter_guid: String,
    parameter_id: String,
    key_value: f64,
    form_guid: String,
    form_available: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExtendedInterpolationSetting {
    parameter_guid: String,
    parameter_id: String,
    interpolation: String,
    insert_point_count: u32,
    extended_interpolation_scale: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MorphTargets {
    pub(crate) targets: Vec<MorphTarget>,
    pub(crate) blend_weight_constraints: Vec<BlendWeightConstraint>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MorphTarget {
    pub(crate) parameter_guid: String,
    pub(crate) parameter_id: String,
    pub(crate) key_value: f64,
    pub(crate) form_guid: String,
    pub(crate) edit_base_parameter_map: (),
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BlendWeightConstraint {
    pub(crate) morph_target_parameter_guid: String,
    pub(crate) morph_target_parameter_id: String,
    pub(crate) constraint_parameter_guid: String,
    pub(crate) constraint_parameter_id: String,
    pub(crate) constraint_parameter_value: f64,
    pub(crate) blend_weight: f64,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub(crate) enum SourceData {
    ArtMesh {
        positions: Vec<f64>,
        uvs: Vec<f64>,
        indices: Vec<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        vertex_uids: Option<Vec<u32>>,
        texture_guid: String,
        texture_state: String,
        culling: bool,
        color_composition: String,
        alpha_composition: String,
        clip_guids: Vec<String>,
        invert_clipping_mask: bool,
    },
    Warp {
        columns: u32,
        rows: u32,
        quad_transform: bool,
        bezier_settings: Vec<[i32; 3]>,
    },
    Rotation {
        base_angle: f64,
    },
    Part {
        child_guids: Vec<String>,
        draw_order_group: bool,
        default_draw_order: i32,
        sketch: bool,
        use_offscreen: bool,
        clip_guids: Vec<String>,
        invert_clipping_mask: bool,
        color_composition: String,
        alpha_composition: String,
    },
    Glue {
        target_art_mesh_a_guid: String,
        target_art_mesh_b_guid: String,
        bind_vertex_uids: Vec<u64>,
        weights: Vec<f64>,
        /// Pair indices whose UID no longer exists in the target mesh; recorded, not repaired.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        stale_bind_pairs: Vec<usize>,
    },
    /// A stroke along a spline. It is read, not evaluated and not drawn: its brush and its tessellation are unread.
    ArtPath {
        closed: bool,
        division_count: u32,
        line_algorithm: String,
        line_alignment: String,
        brush_guid: String,
        clip_guids: Vec<String>,
        invert_clipping_mask: bool,
    },
}

impl SourceData {
    /// An ArtPath's keyforms are read as they are stored. How Cubism interpolates them is not known here, so it gets
    /// no weights and no forms, and nothing in its keying can refuse the evaluation of the sources that are drawn.
    fn is_evaluated(&self) -> bool {
        !matches!(self, SourceData::ArtPath { .. })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Form {
    pub(crate) guid: String,
    pub(crate) coordinate_type: Option<String>,
    pub(crate) animated: bool,
    pub(crate) local_animated: bool,
    pub(crate) opacity: f64,
    pub(crate) draw_order: Option<i32>,
    pub(crate) multiply_color: [f64; 4],
    pub(crate) screen_color: [f64; 4],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) vertex_count_matches_source: Option<bool>,
    pub(crate) data: FormData,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub(crate) enum FormData {
    ArtMesh {
        positions: Vec<f64>,
    },
    Warp {
        positions: Vec<f64>,
    },
    Rotation {
        angle: f64,
        origin: [f64; 2],
        scale: f64,
        reflect_x: bool,
        reflect_y: bool,
    },
    Part,
    Glue {
        intensity: f64,
    },
    ArtPath {
        points: Vec<ArtPathPoint>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArtPathPoint {
    guid: String,
    position: [f64; 2],
    start_velocity: [f64; 2],
    end_velocity: [f64; 2],
    width: f64,
    opacity: f64,
    color: [f64; 4],
    corner: bool,
}

#[derive(Serialize)]
pub(crate) struct Brush {
    guid: String,
    id: String,
    name: String,
    /// The drawing settings the file stores for the brush, under their field names (`BRUSH_SETTINGS`).
    #[serde(flatten)]
    settings: Map<String, Value>,
}

/// The drawing settings of an ArtPath brush that are read: the drawing curve type, the width coefficient, and
/// the numbers of three groups. A setting the file does not store is left out.
pub(crate) const BRUSH_GROUPS: [(&str, &[&str]); 3] = [
    ("textureUvData", &["up", "upInter", "center", "downInter", "down", "inStart", "repeatStart", "repeatEnd", "outEnd"]),
    ("fixWidthData", &["upFixedWidth", "downFixedWidth"]),
    ("spriteData", &["density", "useTextureMagnification"]),
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Texture {
    pub(crate) guid: String,
    pub(crate) name: String,
    pub(crate) image_entry: String,
    pub(crate) image_size: [u32; 2],
    pub(crate) image_to_logical: Option<[f64; 6]>,
    pub(crate) wrap_mode: String,
    pub(crate) min_filter: String,
    pub(crate) mag_filter: String,
    pub(crate) premultiplied: bool,
}

pub(crate) fn read(container: &CaffDocument) -> Result<Value, String> {
    let scene = parse_scene(container)?;
    serde_json::to_value(&scene.scene)
        .map_err(|error| format!("could not serialize authoring source: {error}"))
}

pub(crate) fn linear_weights(container: &CaffDocument, request: &Value) -> Result<Value, String> {
    parse_scene(container)?.linear_weights(request)
}

pub(crate) fn primary_forms(container: &CaffDocument, request: &Value) -> Result<Value, String> {
    parse_scene(container)?.primary_forms(request)
}

fn primary_forms_for_scene(
    scene: &Scene,
    request: &Value,
    budget: &mut Budget,
) -> Result<Value, String> {
    let primary = primary_weights(scene, request, budget)?;
    budget.consume(scene.sources.len())?;
    let owners: HashMap<_, _> = scene
        .sources
        .iter()
        .map(|source| (source.guid.as_str(), source))
        .collect();
    budget.consume(primary.sources.len())?;
    let mut sources = Vec::with_capacity(primary.sources.len());
    for source in primary.sources {
        let owner = owners
            .get(source.guid.as_str())
            .ok_or("authoring primary form owner was not found")?;
        let evaluated_forms = source
            .weights
            .as_ref()
            .map(|weights| {
                crate::authoring_forms::blend_primary_forms(&owner.forms, weights, budget)
            })
            .transpose()?;
        sources.push(SourceForms {
            guid: source.guid,
            id: source.id,
            parent_part_guid: owner
                .parent_part_guid
                .as_deref()
                .map(|s| budget.text(s))
                .transpose()?,
            target_deformer_guid: owner
                .target_deformer_guid
                .as_deref()
                .map(|s| budget.text(s))
                .transpose()?,
            axis_values: source.axis_values,
            weights: source.weights,
            evaluated_forms,
        });
    }
    serde_json::to_value(PrimaryForms {
        format: "live2d.authoring.primary-forms.v1",
        sources,
    })
    .map_err(|error| format!("could not serialize primary forms: {error}"))
}

pub(crate) fn morph_forms(container: &CaffDocument, request: &Value) -> Result<Value, String> {
    parse_scene(container)?.morph_forms(request)
}

fn morph_forms_for_scene(
    scene: &Scene,
    request: &Value,
    budget: &mut Budget,
) -> Result<Value, String> {
    let (source_guid, values_by_guid) = weight_request(scene, request, budget)?;
    let clamp_to_keys = match request.get("clampToKeys") {
        None => false,
        Some(Value::Bool(clamp)) => *clamp,
        Some(_) => return Err("clampToKeys must be a boolean".into()),
    };
    let primary = primary_weights_for_values(scene, source_guid, &values_by_guid, clamp_to_keys, budget)?;
    budget.consume(scene.sources.len())?;
    let owners: HashMap<_, _> = scene
        .sources
        .iter()
        .map(|source| (source.guid.as_str(), source))
        .collect();
    budget.consume(primary.sources.len())?;
    let mut sources = Vec::with_capacity(primary.sources.len());
    for source in primary.sources {
        let owner = owners
            .get(source.guid.as_str())
            .ok_or("authoring morph form owner was not found")?;
        let target_set = owner.morph_targets.as_ref();
        let evaluated = owner.data.is_evaluated();
        let has_targets = evaluated && target_set.is_some_and(|set| !set.targets.is_empty());
        let (extended_targets, extended_forms, extended_morph_extensions) =
            if evaluated { extended_morph_targets(owner, budget)? } else { Default::default() };
        let (base_form_guid, all_morph_weights, morph_weights) = if has_targets {
            let target_set = target_set.ok_or("authoring morph target set is missing")?;
            let grid = owner
                .grid
                .as_ref()
                .ok_or("authoring morph owner has no primary keyform grid")?;
            reject_primary_morph_overlap(grid, target_set, budget)?;
            let base_form_guid = default_form_guid(scene, grid, budget)?;
            let all_weights = morph_target_weights(
                scene, target_set, &extended_targets, &values_by_guid, budget,
            )?;
            let mut active_weights = Vec::new();
            for weight in &all_weights {
                if weight.weight > 0.0 {
                    budget.consume(1)?;
                    active_weights.push(MorphWeight {
                        parameter_id: budget.text(&weight.parameter_id)?,
                        key_value: weight.key_value,
                        form_guid: budget.text(&weight.form_guid)?,
                        weight: weight.weight,
                    });
                }
            }
            (Some(base_form_guid), all_weights, active_weights)
        } else {
            (None, Vec::new(), Vec::new())
        };
        let evaluated_forms = match &source.weights {
            Some(weights) if has_targets => Some(
                crate::authoring_forms::blend_morph_forms(
                    &owner.forms,
                    &extended_forms,
                    weights,
                    base_form_guid
                        .as_deref()
                        .ok_or("authoring morph default form is missing")?,
                    &all_morph_weights,
                    budget,
                )?),
            Some(weights) => Some(crate::authoring_forms::blend_primary_forms(
                &owner.forms,
                weights,
                budget,
            )?),
            None if has_targets => {
                return Err("authoring morph owner has no primary keyform weights".into())
            }
            None => None,
        };
        sources.push(MorphSourceForms {
            guid: source.guid,
            id: source.id,
            parent_part_guid: owner
                .parent_part_guid
                .as_deref()
                .map(|value| budget.text(value))
                .transpose()?,
            target_deformer_guid: owner
                .target_deformer_guid
                .as_deref()
                .map(|value| budget.text(value))
                .transpose()?,
            axis_values: source.axis_values,
            weights: source.weights,
            evaluated_forms,
            base_form_guid,
            morph_weights,
            extended_morph_extensions,
        });
    }
    serde_json::to_value(MorphForms {
        format: "live2d.authoring.morph-forms.v1",
        sources,
    })
    .map_err(|error| format!("could not serialize morph forms: {error}"))
}

fn weights_for_scene(scene: &Scene, request: &Value, budget: &mut Budget) -> Result<Value, String> {
    serde_json::to_value(primary_weights(scene, request, budget)?)
        .map_err(|error| format!("could not serialize authoring weights: {error}"))
}

fn primary_weights(
    scene: &Scene,
    request: &Value,
    budget: &mut Budget,
) -> Result<PrimaryWeights, String> {
    let (source_guid, values_by_guid) = weight_request(scene, request, budget)?;
    primary_weights_for_values(scene, source_guid, &values_by_guid, false, budget)
}

fn weight_request<'scene, 'request>(
    scene: &'scene Scene,
    request: &'request Value,
    budget: &mut Budget,
) -> Result<(Option<&'request str>, HashMap<&'scene str, f64>), String> {
    let overrides = match request.get("parameterValues") {
        None => None,
        Some(value) => Some(
            value
                .as_object()
                .ok_or("parameterValues must be an object")?,
        ),
    };
    let source_guid = match request.get("sourceGuid") {
        None => None,
        Some(value) => Some(
            value
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .ok_or("sourceGuid must be a non-empty string")?,
        ),
    };
    if source_guid.is_some_and(|guid| !scene.sources.iter().any(|source| source.guid == guid)) {
        return Err("authoring weights source GUID was not found".into());
    }
    if let Some(overrides) = overrides {
        budget.consume(scene.parameters.len())?;
        let parameter_ids: HashSet<_> = scene
            .parameters
            .iter()
            .map(|parameter| parameter.id.as_str())
            .collect();
        for (id, value) in overrides {
            if !parameter_ids.contains(id.as_str()) {
                return Err("authoring weights reference an unknown parameter ID".into());
            }
            if !value.as_f64().is_some_and(f64::is_finite) {
                return Err("authoring parameter value must be a finite number".into());
            }
        }
    }
    budget.consume(scene.parameters.len())?;
    let values_by_guid: HashMap<_, _> = scene
        .parameters
        .iter()
        .map(|parameter| {
            let value = overrides
                .and_then(|values| values.get(&parameter.id))
                .and_then(Value::as_f64)
                .unwrap_or(parameter.default);
            (parameter.guid.as_str(), value)
        })
        .collect();
    Ok((source_guid, values_by_guid))
}

/// Cubism Core 6.0.1 evaluates a parameter value closer than this to a key as that key: 0.0009 from a key gives the
/// keyform and 0.0011 does not, with keys 1 apart and with keys 30 apart.
const KEY_SNAP: f64 = 0.001;

fn primary_weights_for_values(
    scene: &Scene,
    source_guid: Option<&str>,
    values_by_guid: &HashMap<&str, f64>,
    clamp_to_keys: bool,
    budget: &mut Budget,
) -> Result<PrimaryWeights, String> {
    budget.consume(scene.sources.len())?;
    budget.consume(scene.parameters.len())?;
    let repeating: HashMap<&str, &Parameter> = scene.parameters.iter()
        .filter(|parameter| parameter.repeat && parameter.maximum > parameter.minimum)
        .map(|parameter| (parameter.guid.as_str(), parameter))
        .collect();
    let mut sources = Vec::new();
    for source in &scene.sources {
        if source_guid.is_some_and(|guid| source.guid != guid) {
            continue;
        }
        let mut axis_values = Vec::new();
        let weights = if let Some(grid) = source.grid.as_ref().filter(|_| source.data.is_evaluated()) {
            budget.consume(grid.axes.len())?;
            let mut periods = Vec::with_capacity(grid.axes.len());
            for axis in &grid.axes {
                let value = *values_by_guid
                    .get(axis.parameter_guid.as_str())
                    .ok_or("authoring grid references an unknown parameter")?;
                let repeating = repeating.get(axis.parameter_guid.as_str()).copied();
                periods.push(repeating.map(|parameter| parameter.maximum - parameter.minimum));
                axis_values.push(match (repeating, clamp_to_keys, axis.keys.first(), axis.keys.last()) {
                    // A repeating parameter turns: past its keys it is between the last key and the first, and its
                    // maximum is its minimum one period on, as Cubism Core evaluates it.
                    (Some(parameter), ..) => {
                        let turned = crate::native_range_value(value, parameter.minimum, parameter.maximum, true);
                        if turned >= parameter.maximum { parameter.minimum } else { turned }
                    }
                    // A preview holds the end keyform outside the keyed span instead of refusing the frame, and takes
                    // a value closer than `KEY_SNAP` to a key as that key.
                    (None, true, Some(first), Some(last)) if first <= last => {
                        let held = value.clamp(*first, *last);
                        axis.keys.iter().copied().find(|key| (key - held).abs() < KEY_SNAP).unwrap_or(held)
                    }
                    _ => value,
                });
            }
            let weights = grid.repeating_weights(&axis_values, &periods, budget)?;
            budget.consume(weights.len())?;
            let mut forms = Vec::with_capacity(weights.len());
            for weight in weights {
                let cell = grid
                    .cells
                    .get(weight.cell_index)
                    .ok_or("authoring weight cell index is invalid")?;
                forms.push(FormWeight {
                    cell_index: weight.cell_index,
                    form_guid: budget.text(&cell.form_guid)?,
                    weight: weight.weight,
                });
            }
            Some(forms)
        } else {
            None
        };
        sources.push(SourceWeights {
            guid: budget.text(&source.guid)?,
            id: budget.text(&source.id)?,
            axis_values,
            weights,
        });
    }
    Ok(PrimaryWeights {
        format: "live2d.authoring.primary-weights.v1",
        sources,
    })
}

fn default_form_guid(
    scene: &Scene,
    grid: &KeyformGrid,
    budget: &mut Budget,
) -> Result<String, String> {
    budget.consume(grid.axes.len())?;
    let mut default_values = Vec::with_capacity(grid.axes.len());
    let mut default_indices = Vec::with_capacity(grid.axes.len());
    for axis in &grid.axes {
        budget.consume(scene.parameters.len())?;
        let parameter = scene
            .parameters
            .iter()
            .find(|parameter| parameter.guid == axis.parameter_guid)
            .ok_or("authoring default keyform references an unknown parameter")?;
        let index = axis
            .keys
            .binary_search_by(|key| {
                key.partial_cmp(&parameter.default)
                    .unwrap_or(std::cmp::Ordering::Less)
            })
            .map_err(|_| "authoring morph default is not an exact primary keyform corner")?;
        default_values.push(parameter.default);
        default_indices.push(index);
    }
    budget.consume(grid.cells.len())?;
    let mut matching_cells = grid
        .cells
        .iter()
        .filter(|cell| cell.indices == default_indices);
    let cell = matching_cells
        .next()
        .ok_or("authoring morph default has no primary keyform cell")?;
    if matching_cells.next().is_some() {
        return Err("authoring morph default has ambiguous primary keyform cells".into());
    }
    let weights = grid.linear_weights(&default_values, budget)?;
    if weights.len() != 1 || weights[0].weight != 1.0 {
        return Err("authoring morph default is not an exact primary keyform corner".into());
    }
    let weighted_cell = grid
        .cells
        .get(weights[0].cell_index)
        .ok_or("authoring morph default cell index is invalid")?;
    if weighted_cell.form_guid != cell.form_guid {
        return Err("authoring morph default cell is ambiguous".into());
    }
    budget.text(&cell.form_guid)
}

fn reject_primary_morph_overlap(
    grid: &KeyformGrid,
    targets: &MorphTargets,
    budget: &mut Budget,
) -> Result<(), String> {
    budget.consume(grid.cells.len())?;
    let primary_guids: HashSet<_> = grid
        .cells
        .iter()
        .map(|cell| cell.form_guid.as_str())
        .collect();
    budget.consume(targets.targets.len())?;
    if targets
        .targets
        .iter()
        .any(|target| primary_guids.contains(target.form_guid.as_str()))
    {
        return Err("authoring morph target form overlaps a primary grid form".into());
    }
    Ok(())
}

fn extended_morph_targets<'a>(
    owner: &'a Source,
    budget: &mut Budget,
) -> Result<(Vec<MorphTarget>, Vec<&'a Form>, Vec<String>), String> {
    let Some(primary) = &owner.morph_targets else {
        return Ok((Vec::new(), Vec::new(), Vec::new()));
    };
    let mut targets = Vec::new();
    let mut forms = Vec::new();
    let mut extensions = Vec::new();
    let mut active_parameters = HashSet::new();
    budget.consume(owner.extended_interpolations.len())?;
    for extension in &owner.extended_interpolations {
        let Some(generated) = &extension.morph_targets else { continue;
        };
        let mut active = false;
        budget.consume(generated.settings.len())?;
        for setting in &generated.settings {
            budget.consume(primary.targets.len())?;
            let endpoints: Vec<_> = primary.targets.iter()
                .filter(|target| target.parameter_guid == setting.parameter_guid).collect();
            if endpoints.is_empty() || setting.interpolation == "LINEAR" { continue; }
            // shortcut: consume stored SNS keys; curve regeneration needs a verified Cubism contract.
            if setting.interpolation != "SNS_CURVE" {
                return Err("authoring extended morph interpolation is unsupported".into());
            }
            if !active_parameters.insert(setting.parameter_guid.as_str()) {
                return Err("authoring extended morph parameter has conflicting extensions".into());
            }
            active = true;
            budget.consume(endpoints.len().saturating_add(1))?;
            let mut keys: Vec<_> = endpoints.iter().map(|target| target.key_value).collect();
            keys.push(0.0);
            keys.sort_by(f64::total_cmp);
            budget.consume(generated.targets.len())?;
            let inserted: Vec<_> = generated.targets.iter()
                .filter(|target| target.parameter_guid == setting.parameter_guid).collect();
            budget.consume(keys.len().saturating_mul(inserted.len()))?;
            let expected_count = (setting.insert_point_count as usize)
                .checked_mul(keys.len().saturating_sub(1))
                .ok_or("authoring extended morph key count overflows")?;
            if inserted.len() != expected_count
                || keys.windows(2).any(|pair| {
                    inserted.iter()
                    .filter(|target| pair[0] < target.key_value && target.key_value < pair[1]).count()
                    != setting.insert_point_count as usize
                })
            {
                return Err("authoring extended morph keys are incomplete or overlap primary keys".into(),
                );
            }
            for target in inserted {
                budget.consume(extension.morph_forms.len())?;
                let representations: Vec<_> = extension.morph_forms.iter()
                    .filter(|form| form.guid == target.form_guid).collect();
                budget.consume(owner.forms.len())?;
                if !target.form_available || representations.is_empty()
                    || representations.iter().any(|form| form.vertex_count_matches_source == Some(false))
                    || owner.forms.iter().any(|form| form.guid == target.form_guid)
                {
                    return Err("authoring extended morph form is missing or stale".into());
                }
                budget.consume(representations.len())?;
                forms.extend(representations);
                targets.push(MorphTarget {
                    parameter_guid: budget.text(&target.parameter_guid)?,
                    parameter_id: budget.text(&target.parameter_id)?,
                    key_value: target.key_value,
                    form_guid: budget.text(&target.form_guid)?,
                    edit_base_parameter_map: (),
                });
            }
        }
        if active { extensions.push(budget.text(&extension.guid)?); }
    }
    Ok((targets, forms, extensions))
}

fn morph_target_weights(
    scene: &Scene,
    targets: &MorphTargets,
    extended_targets: &[MorphTarget],
    values_by_guid: &HashMap<&str, f64>,
    budget: &mut Budget,
) -> Result<Vec<MorphWeight>, String> {
    let limits = morph_constraint_limits(scene, targets, values_by_guid, budget)?;
    budget.consume(targets.targets.len().saturating_add(extended_targets.len()))?;
    let mut by_parameter: BTreeMap<&str, Vec<&MorphTarget>> = BTreeMap::new();
    for target in targets.targets.iter().chain(extended_targets) {
        if !target.key_value.is_finite() || target.key_value == 0.0 {
            return Err("authoring morph target keys must be finite and nonzero".into());
        }
        by_parameter
            .entry(target.parameter_id.as_str())
            .or_default()
            .push(target);
    }
    let mut output = Vec::with_capacity(targets.targets.len());
    for (parameter_id, mut group) in by_parameter {
        budget.consume(group.len())?;
        group.sort_by(|left, right| left.key_value.total_cmp(&right.key_value));
        let first = group
            .first()
            .ok_or("authoring morph parameter has no targets")?;
        budget.consume(scene.parameters.len())?;
        let parameter = scene
            .parameters
            .iter()
            .find(|parameter| parameter.id == parameter_id)
            .filter(|parameter| parameter.guid == first.parameter_guid)
            .ok_or("authoring morph target parameter is missing")?;
        if parameter.parameter_type != "MORPH_TARGET" {
            return Err("authoring morph target parameter is not MORPH_TARGET".into());
        }
        if parameter.repeat {
            return Err("authoring morph target parameter cannot repeat".into());
        }
        if parameter.minimum > 0.0 || parameter.maximum < 0.0 {
            return Err("authoring morph target parameter range must include the neutral zero key".into(),
            );
        }
        if group.iter().any(|target| target.parameter_guid != parameter.guid) {
            return Err("authoring morph target parameter IDs are inconsistent".into());
        }
        if group
            .windows(2)
            .any(|pair| pair[0].key_value == pair[1].key_value)
        {
            return Err("authoring morph target keys must be unique".into());
        }
        let value = *values_by_guid
            .get(parameter.guid.as_str())
            .ok_or("authoring morph target parameter value is missing")?;
        if !value.is_finite() || value < parameter.minimum || value > parameter.maximum {
            return Err("authoring morph sample is outside its parameter range".into());
        }
        let group_weights = morph_blend_weights(&group, value, budget)?;
        for (target, weight) in group.iter().zip(group_weights) {
            output.push(MorphWeight {
                parameter_id: budget.text(&target.parameter_id)?,
                key_value: target.key_value,
                form_guid: budget.text(&target.form_guid)?,
                weight: weight * limits.get(parameter_id).copied().unwrap_or(1.0),
            });
        }
    }
    Ok(output)
}

fn morph_constraint_limits<'a>(
    scene: &Scene,
    targets: &'a MorphTargets,
    values_by_guid: &HashMap<&str, f64>,
    budget: &mut Budget,
) -> Result<BTreeMap<&'a str, f64>, String> {
    budget.consume(targets.blend_weight_constraints.len())?;
    let mut curves: BTreeMap<(&str, &str), Vec<&BlendWeightConstraint>> = BTreeMap::new();
    for point in &targets.blend_weight_constraints {
        if point.morph_target_parameter_id == point.constraint_parameter_id {
            return Err("authoring morph parameter cannot constrain itself".into());
        }
        budget.consume(targets.targets.len())?;
        if !targets.targets.iter().any(|target| {
            target.parameter_id == point.morph_target_parameter_id
                && target.parameter_guid == point.morph_target_parameter_guid
        }) {
            return Err("authoring blend constraint references a parameter without a morph target".into(),
            );
        }
        if !point.constraint_parameter_value.is_finite()
            || !point.blend_weight.is_finite()
            || !(0.0..=1.0).contains(&point.blend_weight)
        {
            return Err("authoring blend constraint points must be finite with weights in range".into(),
            );
        }
        curves.entry((point.morph_target_parameter_id.as_str(), point.constraint_parameter_id.as_str(),
            ))
            .or_default().push(point);
    }
    let mut limits = BTreeMap::new();
    for ((target_id, controller_id), mut points) in curves {
        budget.consume(points.len())?;
        points.sort_by(|left, right| {
            left.constraint_parameter_value.total_cmp(&right.constraint_parameter_value)
        });
        let first = points[0];
        budget.consume(scene.parameters.len())?;
        let controller = scene.parameters.iter()
            .find(|parameter| parameter.id == controller_id)
            .filter(|parameter| parameter.guid == first.constraint_parameter_guid)
            .ok_or("authoring blend constraint controller is missing")?;
        // shortcut: ambiguous or partial graphs need a verified Cubism interpolation contract.
        if controller.repeat || points.len() < 2
            || points[0].constraint_parameter_value != controller.minimum
            || points[points.len() - 1].constraint_parameter_value != controller.maximum
            || points.iter().any(|point| {
                point.constraint_parameter_guid != controller.guid
                || point.morph_target_parameter_guid != first.morph_target_parameter_guid
            })
            || points.windows(2).any(|pair| {
                pair[0].constraint_parameter_value >= pair[1].constraint_parameter_value
            })
        {
            return Err("authoring blend constraints require unique full-range points and a non-repeating controller".into());
        }
        let value = *values_by_guid.get(controller.guid.as_str())
            .ok_or("authoring blend constraint controller value is missing")?;
        if !value.is_finite() || value < controller.minimum || value > controller.maximum {
            return Err("authoring blend constraint sample is outside its controller range".into());
        }
        let limit = if let Some(point) = points.iter().find(|point| point.constraint_parameter_value == value) {
            point.blend_weight
        } else {
            let pair = points.windows(2).find(|pair| {
                    pair[0].constraint_parameter_value < value
                && value < pair[1].constraint_parameter_value
                })
                .ok_or("authoring blend constraint sample has no interval")?;
            let fraction = morph_fraction(pair[0].constraint_parameter_value, pair[1].constraint_parameter_value, value,
            )?;
            pair[0].blend_weight + fraction * (pair[1].blend_weight - pair[0].blend_weight)
        };
        let smallest = limits.entry(target_id).or_insert(1.0_f64);
        *smallest = smallest.min(limit);
    }
    Ok(limits)
}

fn morph_blend_weights(
    targets: &[&MorphTarget],
    value: f64,
    budget: &mut Budget,
) -> Result<Vec<f64>, String> {
    budget.consume(targets.len().saturating_add(1))?;
    let mut nodes = Vec::with_capacity(targets.len().saturating_add(1));
    nodes.push((0.0, None));
    nodes.extend(
        targets
            .iter()
            .enumerate()
            .map(|(index, target)| (target.key_value, Some(index))),
    );
    nodes.sort_by(|left, right| left.0.total_cmp(&right.0));
    if nodes.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err("authoring morph target keys overlap the neutral key".into());
    }
    let mut weights = vec![0.0; targets.len()];
    if value < nodes[0].0 {
        if let Some(index) = nodes[0].1 {
            weights[index] = 1.0;
        }
        return Ok(weights);
    }
    if value > nodes[nodes.len() - 1].0 {
        if let Some(index) = nodes[nodes.len() - 1].1 {
            weights[index] = 1.0;
        }
        return Ok(weights);
    }
    if let Some((_, Some(index))) = nodes.iter().find(|(key, _)| *key == value) {
        weights[*index] = 1.0;
        return Ok(weights);
    }
    for pair in nodes.windows(2) {
        let (lower, lower_target) = pair[0];
        let (upper, upper_target) = pair[1];
        if lower < value && value < upper {
            let upper_weight = morph_fraction(lower, upper, value)?;
            if let Some(index) = lower_target {
                weights[index] = 1.0 - upper_weight;
            }
            if let Some(index) = upper_target {
                weights[index] = upper_weight;
            }
            return Ok(weights);
        }
    }
    Ok(weights)
}

fn morph_fraction(lower: f64, upper: f64, value: f64) -> Result<f64, String> {
    let span = upper - lower;
    let (offset, span) = if span.is_finite() {
        (value - lower, span)
    } else {
        (value / 2.0 - lower / 2.0, upper / 2.0 - lower / 2.0)
    };
    let fraction = offset / span;
    if fraction.is_finite() && fraction > 0.0 && fraction < 1.0 {
        Ok(fraction)
    } else {
        Err("authoring morph weight is not representable".into())
    }
}

pub(crate) fn parse_scene(container: &CaffDocument) -> Result<ParsedScene, String> {
    let mut post_parse_budget = Budget::new(MAX_SCALARS);
    let scene = parse_container(container, &mut post_parse_budget)?;
    Ok(ParsedScene {
        scene,
        post_parse_budget,
    })
}

fn parse_container(container: &CaffDocument, budget: &mut Budget) -> Result<Scene, String> {
    let xml = container.main_xml();
    let text = std::str::from_utf8(&xml)
        .map_err(|error| format!("authoring XML is not UTF-8: {error}"))?;
    let entries = container
        .entries()
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    parse_xml(text, &entries, budget)
}

#[cfg(test)]
fn read_xml(text: &str, entries: &HashSet<&str>, mut budget: Budget) -> Result<Value, String> {
    let scene = parse_xml(text, entries, &mut budget)?;
    serde_json::to_value(scene)
        .map_err(|error| format!("could not serialize authoring source: {error}"))
}

fn parse_xml(
    text: &str,
    entries: &HashSet<&str>,
    mut budget: &mut Budget,
) -> Result<Scene, String> {
    let document = Document::parse_with_options(
        text,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 1_000_000,
            ..Default::default()
        },
    )
    .map_err(|error| format!("invalid authoring XML: {error}"))?;
    if !document.root_element().has_tag_name("root") {
        return Err("authoring XML root must be root".into());
    }
    let graph = Graph::new(&document)?;
    let model = graph.model()?.ok_or("authoring source contains no model")?;
    let canvas = values::required_field(&graph, model, "canvas")?;
    let width = unsigned_field(&graph, canvas, "pixelWidth")?;
    let height = unsigned_field(&graph, canvas, "pixelHeight")?;
    if width == 0 || height == 0 {
        return Err("authoring canvas dimensions must be positive".into());
    }
    let mut parameters = Vec::new();
    let mut parameter_ids = HashSet::new();
    let mut parameter_map = HashMap::new();
    for node in sources(&graph, model, "parameterSourceSet", &mut budget)? {
        if !node.has_tag_name("CParameterSource") {
            return Err("authoring parameter list contains a non-parameter source".into());
        }
        let guid = budget.text(required_guid(&graph, node, "guid")?)?;
        let id = budget.text(source_id(&graph, node)?)?;
        if !parameter_ids.insert(budget.text(&id)?)
            || parameter_map
                .insert(budget.text(&guid)?, budget.text(&id)?)
                .is_some()
        {
            return Err("duplicate authoring parameter ID or GUID".into());
        }
        let minimum = values::number_field(&graph, node, "minValue", None)?;
        let maximum = values::number_field(&graph, node, "maxValue", None)?;
        let default = values::number_field(&graph, node, "defaultValue", None)?;
        if minimum > default || default > maximum {
            return Err("invalid authoring parameter range".into());
        }
        parameters.push(Parameter {
            guid,
            id,
            name: budget.text(values::text_field(&graph, node, "name")?.unwrap_or(""))?,
            minimum,
            maximum,
            default,
            repeat: values::bool_field(&graph, node, "isRepeat", false)?,
            parameter_type: budget.text(enum_field(&graph, node, "paramType", "NORMAL")?)?,
        });
    }
    let mut source_nodes = Vec::new();
    for set in ["drawableSourceSet", "deformerSourceSet", "partSourceSet"] {
        let nodes = sources(&graph, model, set, &mut budget)?;
        if nodes.iter().any(|node| node.has_tag_name("CGlueSource")) {
            return Err("authoring Glue source is outside affecterSourceSet".into());
        }
        source_nodes.extend(nodes);
    }
    if graph.field(model, "affecterSourceSet")?.is_some() {
        let nodes = sources(&graph, model, "affecterSourceSet", &mut budget)?;
        if nodes.iter().any(|node| !node.has_tag_name("CGlueSource")) {
            return Err("authoring affecter source list contains an unsupported source".into());
        }
        source_nodes.extend(nodes);
    }
    let mut parsed = Vec::new();
    let mut textures = Vec::new();
    let mut texture_nodes = HashMap::new();
    let mut source_guids = HashSet::new();
    for node in source_nodes {
        let guid = budget.text(if node.has_tag_name("CGlueSource") {
            typed_guid(&graph, node, "guid", "CAffecterGuid")?
        } else {
            required_guid(&graph, node, "guid")?
        })?;
        if !source_guids.insert(budget.text(&guid)?) {
            return Err("duplicate authoring source GUID".into());
        }
        let data = match node.tag_name().name() {
            "CArtMeshSource" => {
                let positions = floats(&graph, node, "positions", &mut budget)?;
                let vertex_uids = editable_mesh_vertex_uids(
                    &graph,
                    node,
                    positions.len() / 2,
                    &mut budget,
                )?;
                let uvs = floats(&graph, node, "uvs", &mut budget)?;
                let indices = values::index_array(
                    &graph,
                    values::required_field(&graph, node, "indices")?,
                    &mut budget,
                )?;
                if positions.is_empty()
                    || positions.len() % 2 != 0
                    || positions.len() != uvs.len()
                    || indices.len() % 3 != 0
                    || indices
                        .iter()
                        .any(|&index| index as usize >= positions.len() / 2)
                {
                    return Err("invalid authoring mesh geometry".into());
                }
                let texture = values::required_field(&graph, node, "texture")?;
                let texture_guid = budget.text(required_guid(&graph, texture, "guid")?)?;
                if let Some(existing) =
                    texture_nodes.insert(budget.text(&texture_guid)?, texture.id())
                {
                    if existing != texture.id() {
                        return Err("duplicate authoring texture GUID".into());
                    }
                } else {
                    textures.push(read_texture(&graph, texture, entries, &mut budget)?);
                }
                SourceData::ArtMesh {
                    positions,
                    uvs,
                    indices,
                    vertex_uids,
                    texture_guid,
                    texture_state: budget.text(enum_field(
                        &graph,
                        node,
                        "textureState",
                        "TEXTURE_ATLAS",
                    )?)?,
                    culling: values::bool_field(&graph, node, "culling", false)?,
                    color_composition: budget.text(enum_field(
                        &graph,
                        node,
                        "colorComposition",
                        "NORMAL",
                    )?)?,
                    alpha_composition: budget.text(enum_field(
                        &graph,
                        node,
                        "alphaComposition",
                        "OVER",
                    )?)?,
                    clip_guids: guid_list(&graph, node, "clipGuidList", &mut budget)?,
                    invert_clipping_mask: values::bool_field(
                        &graph,
                        node,
                        "invertClippingMask",
                        false,
                    )?,
                }
            }
            "CWarpDeformerSource" => {
                let columns = unsigned_field(&graph, node, "col")?;
                let rows = unsigned_field(&graph, node, "row")?;
                if columns == 0 || rows == 0 {
                    return Err("invalid authoring warp grid".into());
                }
                let mut bezier_settings = Vec::new();
                if let Some(extensions) = graph.field(node, "_extensions")? {
                    for extension in values::list(&graph, extensions, &mut budget)? {
                        if extension.has_tag_name("CWarpDeformerBezierExtension") {
                            bezier_settings.push([
                                integer_field(&graph, extension, "editLevel", None)?,
                                integer_field(&graph, extension, "bezierCol", None)?,
                                integer_field(&graph, extension, "bezierRow", None)?,
                            ]);
                        }
                    }
                }
                SourceData::Warp {
                    columns,
                    rows,
                    quad_transform: values::bool_field(&graph, node, "isQuadTransform", false)?,
                    bezier_settings,
                }
            }
            "CRotationDeformerSource" => SourceData::Rotation {
                base_angle: values::number_field(&graph, node, "baseAngle", Some(0.0))?,
            },
            "CPartSource" => SourceData::Part {
                child_guids: guid_list(&graph, node, "_childGuids", &mut budget)?,
                draw_order_group: values::bool_field(&graph, node, "enableDrawOrderGroup", false)?,
                default_draw_order: integer_field(
                    &graph,
                    node,
                    "defaultOrder_forEditor",
                    Some(500),
                )?,
                sketch: values::bool_field(&graph, node, "isSketch", false)?,
                use_offscreen: values::bool_field(&graph, node, "useOffscreen", false)?,
                clip_guids: guid_list(&graph, node, "clipGuidList", &mut budget)?,
                invert_clipping_mask: values::bool_field(
                    &graph,
                    node,
                    "invertClippingMask",
                    false,
                )?,
                color_composition: budget.text(enum_field(
                    &graph,
                    node,
                    "colorComposition",
                    "NORMAL",
                )?)?,
                alpha_composition: budget.text(enum_field(
                    &graph,
                    node,
                    "alphaComposition",
                    "OVER",
                )?)?,
            },
            "CGlueSource" => {
                let target_art_mesh_a_guid = budget.text(typed_guid(
                    &graph,
                    node,
                    "targetArtMeshA_guid",
                    "CDrawableGuid",
                )?)?;
                let target_art_mesh_b_guid = budget.text(typed_guid(
                    &graph,
                    node,
                    "targetArtMeshB_guid",
                    "CDrawableGuid",
                )?)?;
                let bind_vertex_uids = unsigned_long_array(
                    &graph,
                    values::required_field(&graph, node, "bindVertexUids")?,
                    &mut budget,
                )?;
                let weights = floats(&graph, node, "weights", &mut budget)?;
                if bind_vertex_uids.len() != weights.len() || bind_vertex_uids.len() % 2 != 0 {
                    return Err("invalid authoring Glue binding array lengths".into());
                }
                if weights.iter().any(|weight| !(0.0..=1.0).contains(weight)) {
                    return Err("authoring Glue weights must be in range".into());
                }
                SourceData::Glue {
                    target_art_mesh_a_guid,
                    target_art_mesh_b_guid,
                    bind_vertex_uids,
                    weights,
                    stale_bind_pairs: Vec::new(),
                }
            }
            "CArtPathSource" => SourceData::ArtPath {
                closed: required_bool(&graph, node, "isClosed")?,
                division_count: values::text_field(&graph, node, "divNum")?
                    .ok_or("authoring source lacks divNum")?.trim().parse::<u32>()
                    .map_err(|_| "invalid authoring unsigned integer divNum".to_owned())?,
                line_algorithm: budget.text(
                    values::required_field(&graph, node, "lineAlgorithm")?
                        .attribute("v")
                        .ok_or("authoring lineAlgorithm lacks enum value")?,
                )?,
                line_alignment: budget.text(
                    values::required_field(&graph, node, "lineAlignment")?
                        .attribute("v")
                        .ok_or("authoring lineAlignment lacks enum value")?,
                )?,
                brush_guid: budget.text(typed_guid(&graph, node, "brushGuid", "CArtPathBrushGuid")?)?,
                clip_guids: guid_list(&graph, node, "clipGuidList", &mut budget)?,
                invert_clipping_mask: values::bool_field(&graph, node, "invertClippingMask", false)?,
            },
            tag => return Err(format!("unsupported authoring source {tag}")),
        };
        let forms = read_owned_forms(
            &graph,
            node,
            values::required_field(&graph, node, "keyforms")?,
            &data,
            false,
            &mut budget,
        )?;
        let grid = read_grid(
            &graph,
            node,
            "keyformGridSource",
            &parameter_map,
            false,
            &mut budget,
        )?;
        if let Some(grid) = &grid {
            let form_guids: HashSet<_> = forms.iter().map(|form| form.guid.as_str()).collect();
            for cell in &grid.cells {
                if !form_guids.contains(cell.form_guid.as_str()) {
                    return Err("authoring grid references a form outside its source".into());
                }
            }
        }
        let morph_targets = read_morph_targets(&graph, node, &forms, &parameter_map, &mut budget)?;
        let extended_interpolations =
            read_extended_interpolations(&graph, node, &data, &forms, &parameter_map, &mut budget)?;
        let (parent_part_guid, target_deformer_guid) = if node.has_tag_name("CGlueSource") {
            (
                optional_typed_guid(&graph, node, "parentGuid", "CPartGuid")?,
                optional_typed_guid(&graph, node, "targetDeformerGuid", "CDeformerGuid")?,
            )
        } else {
            (
                values::guid_field(&graph, node, "parentGuid")?,
                values::guid_field(&graph, node, "targetDeformerGuid")?,
            )
        };
        parsed.push(Source {
            guid,
            id: budget.text(source_id(&graph, node)?)?,
            name: budget.text(values::text_field(&graph, node, "localName")?.unwrap_or(""))?,
            parent_part_guid: parent_part_guid
                .map(|s| budget.text(s))
                .transpose()?,
            target_deformer_guid: target_deformer_guid
                .map(|s| budget.text(s))
                .transpose()?,
            visible: values::bool_field(&graph, node, "isVisible", true)?,
            locked: values::bool_field(&graph, node, "isLocked", false)?,
            user_data: match node.has_tag_name("CArtMeshSource") {
                true => budget.text(values::text_field(&graph, node, "userData")?.unwrap_or(""))?,
                false => String::new(),
            },
            forms,
            grid,
            morph_targets,
            extended_interpolations,
            data,
        });
    }
    let root = values::required_field(&graph, model, "rootPart")?;
    let root_part_guid = budget.text(required_guid(&graph, root, "guid")?)?;
    if !parsed.iter().any(|source| {
        source.guid == root_part_guid && matches!(source.data, SourceData::Part { .. })
    }) {
        return Err("authoring rootPart is absent from the part source set".into());
    }
    budget.consume(parsed.len())?;
    let hierarchy: Vec<_> = parsed
        .iter()
        .map(|source| {
            use crate::authoring_hierarchy::{HierarchyNode, SourceKind};
            HierarchyNode {
                guid: &source.guid,
                kind: match source.data {
                    SourceData::ArtMesh { .. } => SourceKind::ArtMesh,
                    SourceData::Warp { .. } => SourceKind::Warp,
                    SourceData::Rotation { .. } => SourceKind::Rotation,
                    SourceData::Part { .. } => SourceKind::Part,
                    SourceData::Glue { .. } => SourceKind::Glue,
                    SourceData::ArtPath { .. } => SourceKind::ArtPath,
                },
                parent_part_guid: source.parent_part_guid.as_deref(),
                target_deformer_guid: source.target_deformer_guid.as_deref(),
                glue_target_art_mesh_guids: match &source.data {
                    SourceData::Glue {
                        target_art_mesh_a_guid,
                        target_art_mesh_b_guid,
                        ..
                    } => Some((target_art_mesh_a_guid, target_art_mesh_b_guid)),
                    _ => None,
                },
            }
        })
        .collect();
    crate::authoring_hierarchy::validate_hierarchy(&hierarchy, &root_part_guid, budget)?;
    validate_glue_vertex_uids(&mut parsed, budget)?;
    Ok(Scene {
        format: "live2d.authoring.source.v1",
        file_format_version: document
            .root_element()
            .attribute("fileFormatVersion")
            .map(|s| budget.text(s))
            .transpose()?,
        name: budget.text(values::text_field(&graph, model, "name")?.unwrap_or(""))?,
        canvas: [width, height],
        root_part_guid,
        parameters,
        sources: parsed,
        textures,
        brushes: read_brushes(&graph, model, entries, budget)?,
        physics_source: graph
            .field(model, "physicsSettingsSourceSet")?
            .map(|set| set.range())
            .filter(|range| range.end <= text.len())
            .map(|range| budget.text(&text[range]))
            .transpose()?,
    })
}

/// The brush setting is found by its tag, wherever the model stores it; a file without one has no brushes.
fn read_brushes<'d, 'i>(graph: &Graph<'d, 'i>, model: Node<'d, 'i>, entries: &HashSet<&str>, budget: &mut Budget) -> Result<Vec<Brush>, String> {
    let Some(setting) = graph.field(model, "artPathBrushesSetting")? else {
        return Ok(Vec::new());
    };
    let Some(list) = graph.field(setting, "brushes")? else {
        return Ok(Vec::new());
    };
    let mut brushes = Vec::new();
    for node in values::list(graph, list, budget)? {
        if !node.has_tag_name("CArtPathBrush") {
            return Err("authoring brush list contains a non-brush".into());
        }
        let mut settings = Map::new();
        if let Some(input) = graph.field(node, "textureInput")? {
            let mut texture_input = Map::new();
            if let Some(texture) = graph.field(input, "texture")?.filter(|n| !n.has_tag_name("null")) {
                texture_input.insert("texture".into(), serde_json::to_value(read_texture(graph, texture, entries, budget)?)
                    .map_err(|error| error.to_string())?);
            }
            for name in ["textureState", "colorComposition"] {
                if let Some(field) = graph.field(input, name)?.filter(|n| !n.has_tag_name("null")) {
                    let value = field.attribute("v").ok_or("authoring brush texture enum lacks v")?;
                    texture_input.insert(name.into(), json!(budget.text(value)?));
                }
            }
            if let Some(current) = graph.field(input, "currentTextureInputData")?.filter(|n| !n.has_tag_name("null")) {
                let mut data = Map::new();
                if let Some(field) = graph.field(current, "_modelImageGuid")?.filter(|n| !n.has_tag_name("null")) {
                    let guid = field.attribute("uuid").ok_or("authoring brush model image guid lacks uuid")?;
                    data.insert("modelImageGuid".into(), json!(budget.text(guid)?));
                }
                if let Some(transform) = graph.field(current, "optionalTransformOnCanvas")? {
                    let mut coefficients = Map::new();
                    for name in ["m00", "m01", "m02", "m10", "m11", "m12"] {
                        if let Some(value) = transform.attribute(name) {
                            budget.consume(1)?;
                            coefficients.insert(name.into(), json!(values::finite(value)?));
                        }
                    }
                    data.insert("optionalTransformOnCanvas".into(), if transform.has_tag_name("null") {
                        Value::Null
                    } else { Value::Object(coefficients) });
                }
                texture_input.insert("currentTextureInputData".into(), Value::Object(data));
            }
            settings.insert("textureInput".into(), if input.has_tag_name("null") {
                Value::Null
            } else { Value::Object(texture_input) });
        }
        if let Some(kind) = graph.field(node, "drawingCurveType")? {
            let kind = kind.attribute("v").ok_or("authoring brush drawingCurveType lacks v")?;
            settings.insert("drawingCurveType".into(), Value::String(budget.text(kind)?));
        }
        if let Some(text) = values::text_field(graph, node, "widthCoefficient")? {
            settings.insert("widthCoefficient".into(), json!(values::finite(text)?));
        }
        for (group, keys) in BRUSH_GROUPS {
            let Some(element) = graph.field(node, group)? else { continue };
            let mut numbers = Map::new();
            for key in keys {
                if let Some(text) = values::text_field(graph, element, key)? {
                    numbers.insert((*key).into(), json!(values::finite(text)?));
                }
            }
            settings.insert(group.into(), Value::Object(numbers));
        }
        brushes.push(Brush {
            guid: budget.text(typed_guid(graph, node, "guid", "CArtPathBrushGuid")?)?,
            id: budget.text(graph.field(node, "id")?.and_then(|id| id.attribute("idstr")).unwrap_or(""))?,
            name: budget.text(values::text_field(graph, node, "name")?.unwrap_or(""))?,
            settings,
        });
    }
    Ok(brushes)
}

fn read_owned_forms(
    graph: &Graph<'_, '_>,
    source: Node<'_, '_>,
    list: Node<'_, '_>,
    data: &SourceData,
    generated: bool,
    budget: &mut Budget,
) -> Result<Vec<Form>, String> {
    let mut forms = Vec::new();
    let mut coordinates = HashSet::new();
    let mut canonical_forms = HashSet::new();
    for node in values::list(graph, list, budget)? {
        if !canonical_forms.insert(node.id()) {
            continue;
        }
        if values::required_field(graph, node, "_source")?.id() != source.id() {
            return Err("authoring form belongs to another source".into());
        }
        let form = read_form(graph, node, data, generated, budget)?;
        if !coordinates.insert((
            budget.text(&form.guid)?,
            form.coordinate_type
                .as_deref()
                .map(|s| budget.text(s))
                .transpose()?,
        )) {
            return Err("duplicate authoring form GUID and coordinate type".into());
        }
        forms.push(form);
    }
    Ok(forms)
}

fn read_extended_interpolations(
    graph: &Graph<'_, '_>,
    source: Node<'_, '_>,
    data: &SourceData,
    primary_forms: &[Form],
    parameters: &HashMap<String, String>,
    budget: &mut Budget,
) -> Result<Vec<ExtendedInterpolation>, String> {
    let Some(list) = graph.field(source, "_extensions")? else {
        return Ok(Vec::new());
    };
    let mut extensions = Vec::new();
    let mut extension_guids = HashSet::new();
    for extension in values::list(graph, list, budget)? {
        if !extension.has_tag_name("CExtendedInterpolationExtension") {
            continue;
        }
        if values::required_field(graph, extension, "_owner")?.id() != source.id() {
            return Err("authoring extended interpolation belongs to another source".into());
        }
        let guid = required_guid(graph, extension, "guid")?;
        if !extension_guids.insert(guid) {
            return Err("duplicate authoring extended interpolation GUID".into());
        }
        let legacy_forms = graph.field(extension, "extendedKeyforms")?;
        let normal_forms = graph.field(extension, "normalExtendedKeyforms")?;
        let morph_forms = graph.field(extension, "morphTargetExtendedKeyforms")?;
        let (normal_forms, morph_forms) = match (legacy_forms, normal_forms, morph_forms) {
            (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
                return Err("authoring extended interpolation mixes legacy and modern keyform lists".into());
            }
            (Some(forms), None, None) => (
                read_owned_forms(graph, source, forms, data, true, budget)?,
                Vec::new(),
            ),
            (None, Some(normal_forms), Some(morph_forms)) => (
                read_owned_forms(graph, source, normal_forms, data, true, budget)?,
                read_owned_forms(graph, source, morph_forms, data, true, budget)?,
            ),
            (None, None, None) => {
                return Err("authoring extended interpolation lacks extended keyform lists".into());
            }
            (None, _, _) => {
                return Err("authoring extended interpolation requires both modern keyform lists".into());
            }
        };
        let form_guids: HashSet<_> = primary_forms
            .iter()
            .chain(&normal_forms)
            .chain(&morph_forms)
            .map(|form| form.guid.as_str())
            .collect();
        let grid = read_grid(
            graph,
            extension,
            "extendedKeyformGridSource",
            parameters,
            true,
            budget,
        )?;
        let mut missing_form_guids = Vec::new();
        if let Some(grid) = &grid {
            let unresolved = grid.axes.iter().any(|axis| axis.parameter_id.is_none());
            for cell in &grid.cells {
                if form_guids.contains(cell.form_guid.as_str()) {
                    continue;
                }
                // Only generated inserted points may lack a stored form; primary positions stay mandatory.
                let inserted = unresolved
                    || cell.indices.iter().zip(&grid.axes).any(|(index, axis)| {
                        axis.parameter_id.is_some() && index % (axis.insert_point_count as usize + 1) != 0
                    });
                if !inserted {
                    return Err("authoring extended grid references a form outside its source".into());
                }
                missing_form_guids.push(budget.text(&cell.form_guid)?);
            }
        }
        let morph_targets = match graph.field(extension, "_extendedKeyformMorphTargetSet")? {
            None => None,
            Some(set) if set.has_tag_name("null") => None,
            Some(set) => {
                if !set.has_tag_name("ExtendedKeyFormMorphTargetSet") {
                    return Err("invalid authoring extended morph target set".into());
                }
                let mut targets = Vec::new();
                for target in values::list(
                    graph,
                    values::required_field(graph, set, "_morphTargets")?,
                    budget,
                )? {
                    if !target.has_tag_name("ExtendedKeyFormMorphTarget") {
                        return Err("invalid authoring extended morph target".into());
                    }
                    let form_guid = required_guid(graph, target, "keyformGuid")?;
                    let (parameter_guid, parameter_id) =
                        morph_parameter(graph, target, "parameterGuid", parameters, budget)?;
                    targets.push(ExtendedMorphTarget {
                        parameter_guid,
                        parameter_id,
                        key_value: values::number_field(graph, target, "keyValue", None)?,
                        form_guid: budget.text(form_guid)?,
                        form_available: form_guids.contains(form_guid),
                    });
                }
                let map = values::required_field(graph, set, "extendedInterpolationSettings")?;
                if !map.has_tag_name("hash_map") {
                    return Err("invalid authoring extended interpolation settings map".into());
                }
                let mut settings = Vec::new();
                let mut setting_parameters = HashSet::new();
                for entry in values::list(graph, map, budget)? {
                    if !entry.has_tag_name("entry") {
                        return Err(
                            "invalid authoring extended interpolation settings entry".into()
                        );
                    }
                    let parameter_guid = required_guid(graph, entry, "key")?;
                    let parameter_id = parameters
                        .get(parameter_guid)
                        .ok_or("authoring extended setting references an unknown parameter")?;
                    if !setting_parameters.insert(parameter_guid) {
                        return Err(
                            "duplicate authoring extended interpolation parameter setting".into(),
                        );
                    }
                    let value = values::required_field(graph, entry, "value")?;
                    if !value.has_tag_name("ExtendedInterpolationSetting") {
                        return Err("invalid authoring extended interpolation setting".into());
                    }
                    let kind = values::required_field(graph, value, "type")?;
                    let interpolation = kind
                        .attribute("v")
                        .filter(|v| !v.trim().is_empty())
                        .ok_or("authoring extended interpolation type lacks value")?;
                    settings.push(ExtendedInterpolationSetting {
                        parameter_guid: budget.text(parameter_guid)?,
                        parameter_id: budget.text(parameter_id)?,
                        interpolation: budget.text(interpolation)?,
                        insert_point_count: unsigned_field(graph, value, "insertPointCount")?,
                        extended_interpolation_scale: values::number_field(
                            graph,
                            value,
                            "extendedInterpolationScale",
                            None,
                        )?,
                    });
                }
                Some(ExtendedMorphTargets { targets, settings })
            }
        };
        extensions.push(ExtendedInterpolation {
            guid: budget.text(guid)?,
            grid,
            normal_forms,
            morph_forms,
            morph_targets,
            missing_form_guids,
        });
    }
    Ok(extensions)
}

fn read_morph_targets(
    graph: &Graph<'_, '_>,
    source: Node<'_, '_>,
    forms: &[Form],
    parameters: &HashMap<String, String>,
    budget: &mut Budget,
) -> Result<Option<MorphTargets>, String> {
    let Some(set) = graph.field(source, "keyformMorphTargetSet")? else {
        return Ok(None);
    };
    if set.has_tag_name("null") {
        return Ok(None);
    }
    if !set.has_tag_name("KeyFormMorphTargetSet") {
        return Err("invalid authoring morph target set".into());
    }
    let form_guids: HashSet<_> = forms.iter().map(|form| form.guid.as_str()).collect();
    let mut targets = Vec::new();
    for target in values::list(
        graph,
        values::required_field(graph, set, "_morphTargets")?,
        budget,
    )? {
        if !target.has_tag_name("KeyFormMorphTarget") {
            return Err("invalid authoring morph target".into());
        }
        if values::required_field(graph, target, "owner")?.id() != source.id() {
            return Err("authoring morph target belongs to another source".into());
        }
        let form_guid = required_guid(graph, target, "keyformGuid")?;
        if !form_guids.contains(form_guid) {
            return Err("authoring morph target references a form outside its source".into());
        }
        let edit_base = values::required_field(graph, target, "editBaseParameterMap")?;
        if !edit_base.has_tag_name("MorphTargetEditBaseParameterMap") {
            return Err("invalid authoring morph edit base parameter map".into());
        }
        let base_map = values::required_field(graph, edit_base, "baseParameterMap")?;
        // shortcut: non-null edit bases need a verified schema before decoding.
        if !base_map.has_tag_name("null") {
            return Err("unsupported authoring morph edit base parameter map".into());
        }
        let (parameter_guid, parameter_id) =
            morph_parameter(graph, target, "parameterGuid", parameters, budget)?;
        targets.push(MorphTarget {
            parameter_guid,
            parameter_id,
            key_value: values::number_field(graph, target, "keyValue", None)?,
            form_guid: budget.text(form_guid)?,
            edit_base_parameter_map: (),
        });
    }
    let constraints = values::required_field(graph, set, "blendWeightConstraintSet")?;
    if !constraints.has_tag_name("MorphTargetBlendWeightConstraintSet") {
        return Err("invalid authoring morph blend weight constraint set".into());
    }
    let mut blend_weight_constraints = Vec::new();
    let target_parameters: HashSet<_> = targets
        .iter()
        .map(|target| target.parameter_guid.as_str())
        .collect();
    for constraint in values::list(
        graph,
        values::required_field(graph, constraints, "_constraints")?,
        budget,
    )? {
        if !constraint.has_tag_name("MorphTargetBlendWeightConstraint") {
            return Err("invalid authoring morph blend weight constraint".into());
        }
        let (morph_target_parameter_guid, morph_target_parameter_id) = morph_parameter(
            graph,
            constraint,
            "morphTargetParameterGuid",
            parameters,
            budget,
        )?;
        if !target_parameters.contains(morph_target_parameter_guid.as_str()) {
            return Err("authoring blend constraint references a parameter without a morph target in its source".into());
        }
        let (constraint_parameter_guid, constraint_parameter_id) = morph_parameter(
            graph,
            constraint,
            "constraintParameterGuid",
            parameters,
            budget,
        )?;
        blend_weight_constraints.push(BlendWeightConstraint {
            morph_target_parameter_guid,
            morph_target_parameter_id,
            constraint_parameter_guid,
            constraint_parameter_id,
            constraint_parameter_value: values::number_field(
                graph,
                constraint,
                "constraintParameterValue",
                None,
            )?,
            blend_weight: values::number_field(graph, constraint, "blendWeight", None)?,
        });
    }
    Ok(Some(MorphTargets {
        targets,
        blend_weight_constraints,
    }))
}

fn morph_parameter(
    graph: &Graph<'_, '_>,
    node: Node<'_, '_>,
    field: &str,
    parameters: &HashMap<String, String>,
    budget: &mut Budget,
) -> Result<(String, String), String> {
    let guid = required_guid(graph, node, field)?;
    let id = parameters
        .get(guid)
        .ok_or("authoring morph references an unknown parameter")?;
    Ok((budget.text(guid)?, budget.text(id)?))
}

fn sources<'d, 'i>(
    graph: &Graph<'d, 'i>,
    model: Node<'d, 'i>,
    name: &str,
    budget: &mut Budget,
) -> Result<Vec<Node<'d, 'i>>, String> {
    let set = values::required_field(graph, model, name)?;
    values::list(
        graph,
        values::required_field(graph, set, "_sources")?,
        budget,
    )
}

fn source_id<'d, 'i>(graph: &Graph<'d, 'i>, node: Node<'d, 'i>) -> Result<&'d str, String> {
    values::required_field(graph, node, "id")?
        .attribute("idstr")
        .filter(|id| !id.trim().is_empty())
        .ok_or("authoring source lacks ID".into())
}

fn required_guid<'d, 'i>(
    graph: &Graph<'d, 'i>,
    node: Node<'d, 'i>,
    name: &str,
) -> Result<&'d str, String> {
    values::guid_field(graph, node, name)?.ok_or_else(|| format!("authoring source lacks {name}"))
}

fn typed_guid<'d, 'i>(
    graph: &Graph<'d, 'i>,
    node: Node<'d, 'i>,
    name: &str,
    expected_tag: &str,
) -> Result<&'d str, String> {
    let field = values::required_field(graph, node, name)?;
    if !field.has_tag_name(expected_tag) {
        return Err(format!("authoring {name} must use {expected_tag}"));
    }
    required_guid(graph, node, name)
}

fn optional_typed_guid<'d, 'i>(
    graph: &Graph<'d, 'i>,
    node: Node<'d, 'i>,
    name: &str,
    expected_tag: &str,
) -> Result<Option<&'d str>, String> {
    let Some(field) = graph.field(node, name)? else {
        return Ok(None);
    };
    if field.has_tag_name("null") {
        return Ok(None);
    }
    if !field.has_tag_name(expected_tag) {
        return Err(format!("authoring {name} must use {expected_tag}"));
    }
    values::guid_field(graph, node, name)
}

fn enum_field<'d, 'i>(
    graph: &Graph<'d, 'i>,
    node: Node<'d, 'i>,
    name: &str,
    default: &'static str,
) -> Result<&'d str, String> {
    Ok(graph
        .field(node, name)?
        .map(|node| {
            node.attribute("v")
                .ok_or_else(|| format!("authoring {name} lacks enum value"))
        })
        .transpose()?
        .unwrap_or(default))
}

fn required_bool(graph: &Graph<'_, '_>, node: Node<'_, '_>, name: &str) -> Result<bool, String> {
    values::bool_text(values::text_field(graph, node, name)?.ok_or_else(|| format!("authoring source lacks {name}"))?)
}

fn integer_field(
    graph: &Graph<'_, '_>,
    node: Node<'_, '_>,
    name: &str,
    default: Option<i32>,
) -> Result<i32, String> {
    match values::text_field(graph, node, name)? {
        Some(text) => text
            .parse()
            .map_err(|_| format!("invalid authoring integer {name}")),
        None => default.ok_or_else(|| format!("authoring source lacks {name}")),
    }
}

fn unsigned_field(graph: &Graph<'_, '_>, node: Node<'_, '_>, name: &str) -> Result<u32, String> {
    values::text_field(graph, node, name)?
        .ok_or_else(|| format!("authoring source lacks {name}"))?
        .parse()
        .map_err(|_| format!("invalid authoring unsigned integer {name}"))
}

fn floats(
    graph: &Graph<'_, '_>,
    node: Node<'_, '_>,
    name: &str,
    budget: &mut Budget,
) -> Result<Vec<f64>, String> {
    values::float_array(graph, values::required_field(graph, node, name)?, budget)
}

fn unsigned_long_array(
    graph: &Graph<'_, '_>,
    node: Node<'_, '_>,
    budget: &mut Budget,
) -> Result<Vec<u64>, String> {
    let node = graph.resolve(node)?;
    if !node.has_tag_name("long-array") {
        return Err("authoring Glue bindVertexUids must be a long-array".into());
    }
    if node.children().any(|child| child.is_element()) {
        return Err("authoring Glue UID array contains nested elements".into());
    }
    let text = node.text().unwrap_or("");
    let count = text.split_whitespace().count();
    if node.attribute("count").is_some_and(|value| value.parse::<usize>().ok() != Some(count)) {
        return Err("authoring array count does not match content".into());
    }
    budget.consume(count)?;
    text.split_whitespace()
        .map(|value| {
            value
                .parse::<i64>()
                .map_err(|_| "invalid authoring Glue vertex UID".into())
                .and_then(|value| {
                    u64::try_from(value)
                        .map_err(|_| "invalid authoring Glue vertex UID".into())
                })
        })
        .collect()
}

fn editable_mesh_vertex_uids(
    graph: &Graph<'_, '_>,
    source: Node<'_, '_>,
    vertex_count: usize,
    budget: &mut Budget,
) -> Result<Option<Vec<u32>>, String> {
    let Some(extensions) = graph.field(source, "_extensions")? else {
        return Ok(None);
    };
    let extensions = values::list(graph, extensions, budget)?;
    let mut editable_extension = None;
    for extension in extensions {
        if extension.has_tag_name("CEditableMeshExtension") {
            if editable_extension.replace(extension).is_some() {
                return Err("authoring ArtMesh has multiple editable mesh extensions".into());
            }
        }
    }
    let Some(extension) = editable_extension else {
        return Ok(None);
    };
    let owner = values::required_field(graph, extension, "_owner")?;
    if !owner.has_tag_name("CArtMeshSource") || owner.id() != source.id() {
        return Err("authoring editable mesh extension belongs to another source".into());
    }
    let mesh = values::required_field(graph, extension, "editableMesh")?;
    let point_uids = values::index_array(
        graph,
        values::required_field(graph, mesh, "pointUid")?,
        budget,
    )?;
    if point_uids.len() != vertex_count {
        return Err("authoring ArtMesh vertex UID count differs from source".into());
    }
    budget.consume(point_uids.len())?;
    let unique: HashSet<_> = point_uids.iter().copied().collect();
    if unique.len() != point_uids.len() {
        return Err("authoring ArtMesh vertex UIDs are not unique".into());
    }
    Ok(Some(point_uids))
}

fn validate_glue_vertex_uids(sources: &mut [Source], budget: &mut Budget) -> Result<(), String> {
    let mut target_guids = HashSet::new();
    budget.consume(sources.len())?;
    for source in sources.iter() {
        if let SourceData::Glue {
            target_art_mesh_a_guid,
            target_art_mesh_b_guid,
            ..
        } = &source.data
        {
            target_guids.insert(target_art_mesh_a_guid.as_str());
            target_guids.insert(target_art_mesh_b_guid.as_str());
        }
    }
    budget.consume(target_guids.len())?;
    let mut target_uids = HashMap::with_capacity(target_guids.len());
    for source in sources.iter() {
        if !target_guids.contains(source.guid.as_str()) {
            continue;
        }
        let SourceData::ArtMesh {
            vertex_uids: Some(vertex_uids),
            ..
        } = &source.data
        else {
            return Err("authoring Glue target ArtMesh lacks vertex UIDs".into());
        };
        budget.consume(vertex_uids.len())?;
        let unique: HashSet<_> = vertex_uids.iter().copied().collect();
        target_uids.insert(source.guid.as_str(), unique);
    }
    let mut stale = Vec::new();
    for (source_index, source) in sources.iter().enumerate() {
        let SourceData::Glue {
            target_art_mesh_a_guid,
            target_art_mesh_b_guid,
            bind_vertex_uids,
            ..
        } = &source.data
        else {
            continue;
        };
        let target_a = target_uids
            .get(target_art_mesh_a_guid.as_str())
            .ok_or("authoring Glue target ArtMesh lacks vertex UIDs")?;
        let target_b = target_uids
            .get(target_art_mesh_b_guid.as_str())
            .ok_or("authoring Glue target ArtMesh lacks vertex UIDs")?;
        let mut stale_pairs = Vec::new();
        for (pair_index, pair) in bind_vertex_uids.chunks_exact(2).enumerate() {
            let uid_a = u32::try_from(pair[0]).map_err(|_| "invalid authoring Glue vertex UID")?;
            let uid_b = u32::try_from(pair[1]).map_err(|_| "invalid authoring Glue vertex UID")?;
            // The editor leaves pairs for deleted vertices behind; record them instead of failing.
            if !target_a.contains(&uid_a) || !target_b.contains(&uid_b) {
                budget.consume(1)?;
                stale_pairs.push(pair_index);
            }
        }
        if !stale_pairs.is_empty() {
            stale.push((source_index, stale_pairs));
        }
    }
    for (source_index, pairs) in stale {
        if let SourceData::Glue { stale_bind_pairs, .. } = &mut sources[source_index].data {
            *stale_bind_pairs = pairs;
        }
    }
    Ok(())
}

fn guid_list(
    graph: &Graph<'_, '_>,
    node: Node<'_, '_>,
    name: &str,
    budget: &mut Budget,
) -> Result<Vec<String>, String> {
    let Some(list) = graph.field(node, name)? else {
        return Ok(Vec::new());
    };
    values::list(graph, list, budget)?
        .into_iter()
        .map(|node| {
            budget.text(
                node.attribute("uuid")
                    .filter(|v| !v.trim().is_empty())
                    .ok_or("authoring GUID list lacks uuid")?,
            )
        })
        .collect()
}

fn color(
    graph: &Graph<'_, '_>,
    node: Node<'_, '_>,
    name: &str,
    default: [f64; 4],
) -> Result<[f64; 4], String> {
    let Some(color) = graph.field(node, name)? else {
        return Ok(default);
    };
    let mut result = [0.0; 4];
    for (index, channel) in ["red", "green", "blue", "alpha"].into_iter().enumerate() {
        result[index] = values::finite(
            color
                .attribute(channel)
                .ok_or("authoring color lacks channel")?,
        )?;
    }
    Ok(result)
}

fn read_form(
    graph: &Graph<'_, '_>,
    node: Node<'_, '_>,
    source: &SourceData,
    generated: bool,
    budget: &mut Budget,
) -> Result<Form, String> {
    let coordinate_type = graph
        .field(node, "coordType")?
        .map(|coord| {
            budget.text(
                values::text_field(graph, coord, "coordName")?
                    .ok_or("authoring coordType lacks coordName")?,
            )
        })
        .transpose()?;
    let mut vertex_count_matches_source = true;
    let data = match (node.tag_name().name(), source) {
        (
            "CArtMeshForm",
            SourceData::ArtMesh {
                positions: base, ..
            },
        ) => {
            let positions = floats(graph, node, "positions", budget)?;
            vertex_count_matches_source = positions.len() == base.len();
            if !generated && !vertex_count_matches_source {
                return Err("authoring mesh form vertex count differs from source".into());
            }
            if positions.is_empty() || positions.len() % 2 != 0 {
                return Err("invalid authoring mesh form position pairs".into());
            }
            FormData::ArtMesh { positions }
        }
        ("CWarpDeformerForm", SourceData::Warp { columns, rows, .. }) => {
            let positions = floats(graph, node, "positions", budget)?;
            let expected = (*columns as usize + 1)
                .checked_mul(*rows as usize + 1)
                .and_then(|v| v.checked_mul(2))
                .ok_or("authoring warp grid overflow")?;
            vertex_count_matches_source = positions.len() == expected;
            if !generated && !vertex_count_matches_source {
                return Err("authoring warp form grid size differs from source".into());
            }
            if positions.is_empty() || positions.len() % 2 != 0 {
                return Err("invalid authoring warp form position pairs".into());
            }
            FormData::Warp { positions }
        }
        ("CRotationDeformerForm", SourceData::Rotation { .. }) => {
            let attribute = |name| {
                values::finite(
                    node.attribute(name)
                        .ok_or_else(|| format!("authoring rotation form lacks {name}"))?,
                )
            };
            FormData::Rotation {
                angle: attribute("angle")?,
                origin: [attribute("originX")?, attribute("originY")?],
                scale: attribute("scale")?,
                reflect_x: values::bool_text(
                    node.attribute("isReflectX")
                        .ok_or("authoring rotation lacks isReflectX")?,
                )?,
                reflect_y: values::bool_text(
                    node.attribute("isReflectY")
                        .ok_or("authoring rotation lacks isReflectY")?,
                )?,
            }
        }
        ("CPartForm", SourceData::Part { .. }) => FormData::Part,
        ("CGlueForm", SourceData::Glue { .. }) => FormData::Glue {
            intensity: values::number_field(graph, node, "intensity", None)?,
        },
        ("CArtPathForm", SourceData::ArtPath { .. }) => {
            let vector = |node, name| -> Result<[f64; 2], String> {
                let vector = values::required_field(graph, node, name)?;
                Ok([
                    values::number_field(graph, vector, "x", None)?,
                    values::number_field(graph, vector, "y", None)?,
                ])
            };
            let mut points = Vec::new();
            for point in values::list(graph, values::required_field(graph, node, "positions")?, budget)? {
                if !point.has_tag_name("CArtPathPoint") {
                    return Err("authoring ArtPath form holds a non-point".into());
                }
                let curve = values::required_field(graph, point, "curvePointPosition")?;
                points.push(ArtPathPoint {
                    guid: budget.text(typed_guid(graph, point, "ctrlPtId", "CArtPathPointGuid")?)?,
                    position: vector(curve, "point")?,
                    start_velocity: vector(curve, "startVelocity")?,
                    end_velocity: vector(curve, "endVelocity")?,
                    width: values::number_field(graph, point, "width", None)?,
                    opacity: values::number_field(graph, point, "opacity", None)?,
                    color: [
                        values::number_field(graph, point, "colorRed", None)?,
                        values::number_field(graph, point, "colorGreen", None)?,
                        values::number_field(graph, point, "colorBlue", None)?,
                        values::number_field(graph, point, "colorAlpha", None)?,
                    ],
                    corner: required_bool(graph, point, "isCorner")?,
                });
            }
            FormData::ArtPath { points }
        }
        _ => return Err("authoring form type does not match source".into()),
    };
    if !matches!(data, FormData::Part | FormData::Glue { .. }) && coordinate_type.is_none() {
        return Err("authoring geometry form lacks coordinate type".into());
    }
    Ok(Form {
        vertex_count_matches_source: generated.then_some(vertex_count_matches_source),
        guid: budget.text(required_guid(graph, node, "guid")?)?,
        coordinate_type,
        animated: values::bool_field(graph, node, "isAnimatedForm", false)?,
        local_animated: values::bool_field(graph, node, "isLocalAnimatedForm", false)?,
        opacity: values::number_field(graph, node, "opacity", Some(1.0))?,
        draw_order: values::text_field(graph, node, "drawOrder")?
            .map(|text| {
                let text = if !generated && matches!(source, SourceData::ArtPath { .. }) {
                    text.trim_matches([' ', '\t', '\r', '\n'])
                } else { text };
                text.parse()
                    .map_err(|_| "invalid authoring draw order".to_owned())
            })
            .transpose()?,
        multiply_color: color(graph, node, "multiplyColor", [1.0; 4])?,
        screen_color: color(graph, node, "screenColor", [0.0, 0.0, 0.0, 1.0])?,
        data,
    })
}

fn read_texture(
    graph: &Graph<'_, '_>,
    node: Node<'_, '_>,
    entries: &HashSet<&str>,
    budget: &mut Budget,
) -> Result<Texture, String> {
    if !node.has_tag_name("GTexture2D") {
        return Err("authoring mesh texture is not GTexture2D".into());
    }
    let image = values::required_field(graph, node, "srcImageResource")?;
    let file = values::required_field(graph, image, "imageFileBuf")?;
    let entry = file
        .attribute("path")
        .ok_or("authoring image lacks entry path")?;
    if !entries.contains(entry) {
        return Err("authoring image entry is missing".into());
    }
    let dimension = |name| {
        image
            .attribute(name)
            .ok_or("authoring image lacks dimensions")?
            .parse::<u32>()
            .map_err(|_| "invalid authoring image dimensions")
    };
    let image_size = [dimension("width")?, dimension("height")?];
    if image_size.contains(&0) {
        return Err("authoring image dimensions must be positive".into());
    }
    let affine = values::required_field(graph, node, "transformImageResource01toLogical01")?;
    let mut coefficients = [0.0; 6];
    for (index, name) in ["m00", "m01", "m02", "m10", "m11", "m12"]
        .into_iter()
        .enumerate()
    {
        if affine.has_tag_name("null") {
            break;
        }
        coefficients[index] = values::finite(
            affine
                .attribute(name)
                .ok_or("authoring image transform lacks coefficient")?,
        )?;
    }
    let image_to_logical = if affine.has_tag_name("null") {
        None
    } else {
        Some(coefficients)
    };
    let filter = values::required_field(graph, node, "filterMode")?;
    Ok(Texture {
        guid: budget.text(required_guid(graph, node, "guid")?)?,
        name: budget.text(values::text_field(graph, node, "name")?.unwrap_or(""))?,
        image_entry: budget.text(entry)?,
        image_size,
        image_to_logical,
        wrap_mode: budget.text(enum_field(graph, node, "wrapMode", "CLAMP_TO_BORDER")?)?,
        min_filter: budget.text(enum_field(graph, filter, "minFilter", "LINEAR")?)?,
        mag_filter: budget.text(enum_field(graph, filter, "magFilter", "LINEAR")?)?,
        premultiplied: values::bool_field(graph, node, "isPremultiplied", true)?,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> String {
        r#"<root fileFormatVersion="401000004"><CModelSource>
          <s xs.n="name">Native source</s><CImageCanvas xs.n="canvas"><i xs.n="pixelWidth">100</i><i xs.n="pixelHeight">200</i></CImageCanvas>
          <CParameterSourceSet xs.n="parameterSourceSet"><carray_list xs.n="_sources" count="0"/></CParameterSourceSet>
          <CDrawableSourceSet xs.n="drawableSourceSet"><carray_list xs.n="_sources" count="1"><CArtMeshSource xs.ref="mesh"/></carray_list></CDrawableSourceSet>
          <CDeformerSourceSet xs.n="deformerSourceSet"><carray_list xs.n="_sources" count="0"/></CDeformerSourceSet>
          <CPartSourceSet xs.n="partSourceSet"><carray_list xs.n="_sources" count="1"><CPartSource xs.ref="part"/></carray_list></CPartSourceSet>
          <CPartSource xs.n="rootPart" xs.ref="part"/>
        </CModelSource>
        <CPartSource xs.id="part"><CPartGuid xs.n="guid" uuid="root-guid"/><CPartId xs.n="id" idstr="root"/><carray_list xs.n="keyforms" count="0"/></CPartSource>
        <CArtMeshSource xs.id="mesh"><ACDrawableSource xs.n="super"><CArtMeshGuid xs.n="guid" uuid="mesh-guid"/><CArtMeshId xs.n="id" idstr="mesh-id"/><CPartGuid xs.n="parentGuid" uuid="root-guid"/></ACDrawableSource>
          <float-array xs.n="positions" count="6">10 20 30 40 50 60</float-array>
          <float-array xs.n="uvs" count="6">-0.1 0 0.5 0.5 1.1 1</float-array>
          <int-array xs.n="indices" count="3">0 1 2</int-array>
          <GTexture2D xs.n="texture" xs.ref="texture"/>
          <carray_list xs.n="keyforms" count="2">
            <CArtMeshForm><ACDrawableForm xs.n="super"><ACForm xs.n="super"><CFormGuid xs.n="guid" uuid="form-guid"/><CArtMeshSource xs.n="_source" xs.ref="mesh"/></ACForm><CoordType xs.n="coordType"><s xs.n="coordName">Canvas</s></CoordType><i xs.n="drawOrder">502</i></ACDrawableForm><float-array xs.n="positions" count="6">11 22 33 44 55 66</float-array></CArtMeshForm>
            <CArtMeshForm><ACDrawableForm xs.n="super"><ACForm xs.n="super"><CFormGuid xs.n="guid" uuid="form-guid"/><CArtMeshSource xs.n="_source" xs.ref="mesh"/></ACForm><CoordType xs.n="coordType"><s xs.n="coordName">DeformerLocal</s></CoordType><f xs.n="opacity">0.75</f><CFloatColor xs.n="multiplyColor" red="1" green="0.5" blue="0.25" alpha="1"/></ACDrawableForm><float-array xs.n="positions" count="6">0.1 0.2 0.3 0.4 0.5 0.6</float-array></CArtMeshForm>
          </carray_list>
        </CArtMeshSource>
        <GTexture2D xs.id="texture"><GTexture xs.n="super"><GTextureGuid xs.n="guid" uuid="texture-guid"/><FilterMode xs.n="filterMode"><MinFilter xs.n="minFilter" v="LINEAR"/><MagFilter xs.n="magFilter" v="LINEAR"/></FilterMode></GTexture>
          <CImageResource xs.n="srcImageResource" width="2" height="3"><file xs.n="imageFileBuf" path="mesh.png"/></CImageResource>
          <null xs.n="transformImageResource01toLogical01"/>
        </GTexture2D></root>"#.into()
    }

    fn primary_form_alias_fixture() -> String {
        let mut xml = fixture();
        let list_start = xml
            .find("<carray_list xs.n=\"keyforms\" count=\"2\">")
            .unwrap();
        let form_start = list_start + xml[list_start..].find("<CArtMeshForm>").unwrap();
        let form_end = form_start
            + xml[form_start..].find("</CArtMeshForm>").unwrap()
            + "</CArtMeshForm>".len();
        let canonical = xml[form_start..form_end]
            .replace("<CArtMeshForm>", "<CArtMeshForm xs.id=\"form-canonical\">");
        let list_end = list_start
            + xml[list_start..].find("</carray_list>").unwrap()
            + "</carray_list>".len();
        xml.replace_range(
            list_start..list_end,
            &format!(
                "<carray_list xs.n=\"keyforms\" count=\"4\">{canonical}{}{}<CArtMeshForm xs.ref=\"form-canonical\"/></carray_list>",
                "<CArtMeshForm xs.id=\"form-alias\" xs.ref=\"form-canonical\"/>",
                "<CArtMeshForm xs.id=\"form-alias-chain\" xs.ref=\"form-alias\"/>"
            ),
        );
        xml
    }

    fn glue_fixture() -> String {
        let mut xml = fixture().replace(
            "<CArtMeshSource xs.id=\"mesh\">",
            concat!(
                "<CArtMeshSource xs.id=\"mesh\"><carray_list xs.n=\"_extensions\" count=\"1\">",
                "<CEditableMeshExtension><ACExtension xs.n=\"super\"><CArtMeshSource xs.n=\"_owner\" xs.ref=\"mesh\"/></ACExtension>",
                "<EditableMesh xs.n=\"editableMesh\"><int-array xs.n=\"pointUid\" count=\"3\">10 11 12</int-array></EditableMesh>",
                "</CEditableMeshExtension></carray_list>"
            ),
        );
        let start = xml.find("<CArtMeshSource xs.id=\"mesh\">").unwrap();
        let end = start + xml[start..].find("</CArtMeshSource>").unwrap() + "</CArtMeshSource>".len();
        let mesh = xml[start..end].to_owned();
        let second_mesh = mesh
            .replace("xs.id=\"mesh\"", "xs.id=\"mesh-b\"")
            .replace("mesh-guid", "mesh-b-guid")
            .replace("mesh-id", "mesh-b-id")
            .replace("xs.ref=\"mesh\"", "xs.ref=\"mesh-b\"")
            .replace("<int-array xs.n=\"pointUid\" count=\"3\">10 11 12", "<int-array xs.n=\"pointUid\" count=\"3\">20 21 22");
        xml = xml.replace(
            "<carray_list xs.n=\"_sources\" count=\"1\"><CArtMeshSource xs.ref=\"mesh\"/></carray_list>",
            "<carray_list xs.n=\"_sources\" count=\"2\"><CArtMeshSource xs.ref=\"mesh\"/><CArtMeshSource xs.ref=\"mesh-b\"/></carray_list>",
        );
        let insertion = xml.find("<GTexture2D xs.id=\"texture\">").unwrap();
        xml.insert_str(insertion, &second_mesh);
        xml = xml.replace(
            "<CPartSource xs.n=\"rootPart\"",
            concat!(
                "<CAffecterSourceSet xs.n=\"affecterSourceSet\"><carray_list xs.n=\"_sources\" count=\"1\">",
                "<CGlueSource xs.ref=\"glue\"/></carray_list></CAffecterSourceSet>",
                "<CPartSource xs.n=\"rootPart\""
            ),
        );
        let glue_source = r#"<CGlueSource xs.id="glue"><ACAffecterSource xs.n="super">
          <CAffecterGuid xs.n="guid" uuid="glue-guid"/><CAffecterId xs.n="id" idstr="glue-id"/>
          <CPartGuid xs.n="parentGuid" uuid="root-guid"/></ACAffecterSource>
          <CDrawableGuid xs.n="targetArtMeshA_guid" uuid="mesh-guid"/><CDrawableGuid xs.n="targetArtMeshB_guid" uuid="mesh-b-guid"/>
          <long-array xs.n="bindVertexUids" count="4">11 21 12 20</long-array>
          <float-array xs.n="weights" count="4">0.2 0.8 0.3 0.7</float-array>
          <carray_list xs.n="keyforms" count="1"><CGlueForm><ACAffecterForm xs.n="super"><ACForm xs.n="super">
            <CFormGuid xs.n="guid" uuid="glue-form-guid"/><CGlueSource xs.n="_source" xs.ref="glue"/>
          </ACForm></ACAffecterForm><f xs.n="intensity">1.5</f></CGlueForm></carray_list>
        </CGlueSource>"#;
        let insertion = xml.find("<GTexture2D xs.id=\"texture\">").unwrap();
        xml.insert_str(insertion, glue_source);
        xml
    }

    #[test]
    fn preview_weights_hold_the_end_key_outside_the_keyed_span() {
        let mut scene = morph_evaluation_scene();
        let source = scene.sources.iter_mut().find(|source| source.guid == "mesh-guid").unwrap();
        source.grid = Some(KeyformGrid {
            axes: vec![crate::authoring_keyforms::KeyformAxis {
                binding_ref: "binding".into(),
                parameter_guid: "parameter-2-guid".into(),
                parameter_id: "ParamMorph2".to_owned().into(),
                keys: vec![0.0, 1.0],
                interpolation: "LINEAR".into(),
                extended_interpolation: "LINEAR".into(),
                insert_point_count: 0,
                extended_interpolation_scale: 1.0,
            }],
            cells: vec![
                crate::authoring_keyforms::KeyformCell { indices: vec![0], form_guid: "form-guid".into() },
                crate::authoring_keyforms::KeyformCell { indices: vec![1], form_guid: "end-form".into() },
            ],
        });
        let values = HashMap::from([("parameter-2-guid", 1.5), ("parameter-guid", 0.0)]);
        let weights = |clamp| {
            primary_weights_for_values(&scene, Some("mesh-guid"), &values, clamp, &mut Budget::new(MAX_SCALARS))
        };
        assert_eq!(weights(false).err().unwrap(), "authoring keyform sample is outside the key range");
        let held = serde_json::to_value(weights(true).unwrap()).unwrap();
        assert_eq!(held["sources"][0]["axisValues"], json!([1.0]));
        assert_eq!(held["sources"][0]["weights"], json!([{"cellIndex":1, "formGuid":"end-form", "weight":1.0}]));

        // Closer than 0.001 to a key is that key, as Cubism Core evaluates it; 0.0011 away is not.
        let near = |value: f64, clamp| {
            let values = HashMap::from([("parameter-2-guid", value), ("parameter-guid", 0.0)]);
            serde_json::to_value(
                primary_weights_for_values(&scene, Some("mesh-guid"), &values, clamp, &mut Budget::new(MAX_SCALARS))
                    .unwrap(),
            )
            .unwrap()["sources"][0]
                .clone()
        };
        assert_eq!(near(0.0009, true)["axisValues"], json!([0.0]));
        assert_eq!(near(0.0009, true)["weights"], json!([{"cellIndex":0, "formGuid":"form-guid", "weight":1.0}]));
        assert_eq!(near(0.9991, true)["weights"], json!([{"cellIndex":1, "formGuid":"end-form", "weight":1.0}]));
        assert_eq!(near(0.0011, true)["axisValues"], json!([0.0011]));
        assert_eq!(near(0.0011, true)["weights"].as_array().unwrap().len(), 2);
        assert_eq!(near(0.0009, false)["axisValues"], json!([0.0009]));
        assert_eq!(near(0.0009, false)["weights"].as_array().unwrap().len(), 2);

        // The parameter runs -2..2. Repeating, 2.5 is -1.5: half way from the key at 1 round to the key at 0.
        scene.parameters.iter_mut().find(|parameter| parameter.guid == "parameter-2-guid").unwrap().repeat = true;
        for clamp in [false, true] {
            let turned = serde_json::to_value(primary_weights_for_values(
                &scene, Some("mesh-guid"), &HashMap::from([("parameter-2-guid", 2.5), ("parameter-guid", 0.0)]),
                clamp, &mut Budget::new(MAX_SCALARS)).unwrap()).unwrap();
            assert_eq!(turned["sources"][0]["axisValues"], json!([-1.5]));
            assert_eq!(turned["sources"][0]["weights"], json!([
                {"cellIndex":0, "formGuid":"form-guid", "weight":0.5},
                {"cellIndex":1, "formGuid":"end-form", "weight":0.5}]));
        }
        // At its maximum a repeating parameter is at its minimum, one period on.
        let at = |value: f64| serde_json::to_value(primary_weights_for_values(
            &scene, Some("mesh-guid"), &HashMap::from([("parameter-2-guid", value), ("parameter-guid", 0.0)]),
            true, &mut Budget::new(MAX_SCALARS)).unwrap()).unwrap();
        assert_eq!(at(2.0)["sources"][0]["axisValues"], json!([-2.0]));
        assert_eq!(at(2.0)["sources"][0]["weights"], at(-2.0)["sources"][0]["weights"]);
    }

    #[test]
    fn parsed_scene_evaluation_clones_exact_post_parse_budgets() {
        let mut post_parse_budget = Budget::new(MAX_SCALARS);
        let scene = parse_xml(
            &fixture(),
            &HashSet::from(["mesh.png"]),
            &mut post_parse_budget,
        )
        .unwrap();
        let request = json!({"sourceGuid":"root-guid"});
        let mut measured_budget = post_parse_budget.clone();
        let expected = weights_for_scene(&scene, &request, &mut measured_budget).unwrap();
        let scalar_cost = post_parse_budget.remaining() - measured_budget.remaining();
        let text_cost = post_parse_budget.text_remaining() - measured_budget.text_remaining();
        assert!(scalar_cost > 0);
        assert!(text_cost > 0);

        let parsed_with_budget = |post_parse_budget| {
            let mut parse_budget = Budget::new(MAX_SCALARS);
            let scene = parse_xml(
                &fixture(),
                &HashSet::from(["mesh.png"]),
                &mut parse_budget)
            .unwrap();
            ParsedScene {
                scene,
                post_parse_budget,
            }
        };

        let exact_budget = parsed_with_budget(
            Budget::new(scalar_cost).with_text_limit(text_cost));
        assert_eq!(exact_budget.linear_weights(&request).unwrap(), expected);
        assert_eq!(exact_budget.linear_weights(&request).unwrap(), expected);

        let scalar_short = parsed_with_budget(
            Budget::new(scalar_cost - 1).with_text_limit(text_cost));
        assert!(scalar_short.linear_weights(&request).is_err());
        assert!(scalar_short.linear_weights(&request).is_err());

        let text_short = parsed_with_budget(
            Budget::new(scalar_cost).with_text_limit(text_cost - 1));
        assert!(text_short.linear_weights(&request).is_err());
        assert!(text_short.linear_weights(&request).is_err());
    }

    #[test]
    fn weight_request_resolves_defaults_filters_owners_and_validates_inputs() {
        use crate::authoring_keyforms::{KeyformAxis, KeyformCell, KeyformGrid};
        let evaluate = |request: Value, limit| {
            let mut scene = parse_xml(
                &morph_fixture(),
                &HashSet::from(["mesh.png"]),
                &mut Budget::new(MAX_SCALARS),
            )
            .unwrap();
            scene
                .sources
                .iter_mut()
                .find(|source| source.guid == "mesh-guid")
                .unwrap()
                .grid = Some(KeyformGrid {
                axes: vec![KeyformAxis {
                    binding_ref: "binding".into(),
                    parameter_guid: "parameter-guid".into(),
                    parameter_id: Some("ParamMorph".into()),
                    keys: vec![-1.0, 1.0],
                    interpolation: "LINEAR".into(),
                    extended_interpolation: "LINEAR".into(),
                    insert_point_count: 0,
                    extended_interpolation_scale: 1.0,
                }],
                cells: vec![
                    KeyformCell {
                        indices: vec![0],
                        form_guid: "lower".into(),
                    },
                    KeyformCell {
                        indices: vec![1],
                        form_guid: "upper".into(),
                    },
                ],
            });
            weights_for_scene(&scene, &request, &mut Budget::new(limit))
        };
        let result = evaluate(json!({"sourceGuid":"mesh-guid"}), MAX_SCALARS).unwrap();
        assert_eq!(result["sources"].as_array().unwrap().len(), 1);
        assert_eq!(result["sources"][0]["axisValues"], json!([0.0]));
        assert_eq!(
            result["sources"][0]["weights"],
            json!([
                {"cellIndex":0,"formGuid":"lower","weight":0.5},
                {"cellIndex":1,"formGuid":"upper","weight":0.5}
            ])
        );
        let overridden = evaluate(
            json!({"sourceGuid":"mesh-guid", "parameterValues":{"ParamMorph":1}}),
            MAX_SCALARS,
        )
        .unwrap();
        assert_eq!(
            overridden["sources"][0]["weights"],
            json!([
                {"cellIndex":1,"formGuid":"upper","weight":1.0}
            ])
        );
        let absent = evaluate(json!({"sourceGuid":"root-guid"}), MAX_SCALARS).unwrap();
        assert_eq!(absent["sources"][0]["axisValues"], json!([]));
        assert_eq!(absent["sources"][0]["weights"], Value::Null);
        for request in [
            json!({"parameterValues":null}),
            json!({"parameterValues":[]}),
            json!({"parameterValues":{"unknown":0}}),
            json!({"parameterValues":{"ParamMorph":"0"}}),
            json!({"parameterValues":{"ParamMorph":2}}),
            json!({"sourceGuid":"unknown"}),
            json!({"sourceGuid":""}),
            json!({"sourceGuid":4}),
        ] {
            assert!(evaluate(request.clone(), MAX_SCALARS).is_err(), "{request}");
        }
        assert!(evaluate(json!({"parameterValues":{"ParamMorph":0}}), 1).is_err());
    }

    #[test]
    fn retains_native_spaces_duplicate_guid_representations_and_unclamped_uvs() {
        let scene = read_xml(
            &fixture(),
            &HashSet::from(["mesh.png"]),
            Budget::new(MAX_SCALARS),
        )
        .unwrap();
        assert_eq!(scene["canvas"], json!([100, 200]));
        assert_eq!(
            scene["sources"][0]["data"]["positions"],
            json!([10.0, 20.0, 30.0, 40.0, 50.0, 60.0])
        );
        assert_eq!(
            scene["sources"][0]["data"]["uvs"],
            json!([-0.1, 0.0, 0.5, 0.5, 1.1, 1.0])
        );
        let forms = &scene["sources"][0]["forms"];
        assert_eq!(forms[0]["guid"], forms[1]["guid"]);
        assert_eq!(forms[0]["coordinateType"], "Canvas");
        assert_eq!(forms[1]["coordinateType"], "DeformerLocal");
        assert_eq!(
            forms[1]["data"]["positions"],
            json!([0.1, 0.2, 0.3, 0.4, 0.5, 0.6])
        );
        assert_eq!(forms[0]["drawOrder"], 502);
        assert_eq!(forms[1]["opacity"], 0.75);
        assert_eq!(forms[1]["multiplyColor"], json!([1.0, 0.5, 0.25, 1.0]));
        assert_eq!(scene["textures"][0]["imageToLogical"], Value::Null);
    }

    #[test]
    fn canonical_form_aliases_are_projected_once_for_primary_and_generated_lists() {
        let entries = HashSet::from(["mesh.png"]);
        let primary = read_xml(
            &primary_form_alias_fixture(),
            &entries,
            Budget::new(MAX_SCALARS),
        )
        .unwrap();
        assert_eq!(primary["sources"][0]["forms"].as_array().unwrap().len(), 1);
        assert_eq!(primary["sources"][0]["forms"][0]["coordinateType"], "Canvas");

        let generated_fixture = extended_fixture()
            .replace(
                concat!(
                    "<carray_list xs.n=\"normalExtendedKeyforms\"",
                    " count=\"1\"><CArtMeshForm>"
                ),
                concat!(
                    "<carray_list xs.n=\"normalExtendedKeyforms\"",
                    " count=\"3\"><CArtMeshForm xs.id=\"generated-form-canonical\">"
                ),
            )
            .replace(
                concat!(
                    "</float-array>\n            </CArtMeshForm></carray_list>\n",
                    "            <carray_list xs.n=\"morphTargetExtendedKeyforms\""
                ),
                concat!(
                    "</float-array>\n            </CArtMeshForm>",
                    "<CArtMeshForm xs.id=\"generated-form-alias\"",
                    " xs.ref=\"generated-form-canonical\"/>",
                    "<CArtMeshForm xs.id=\"generated-form-alias-chain\"",
                    " xs.ref=\"generated-form-alias\"/></carray_list>\n",
                    "            <carray_list xs.n=\"morphTargetExtendedKeyforms\""
                ),
            );
        assert!(generated_fixture.contains("generated-form-alias-chain"));
        let generated = read_xml(&generated_fixture, &entries, Budget::new(MAX_SCALARS)).unwrap();
        let forms = &generated["sources"][0]["extendedInterpolations"][0]["normalForms"];
        assert_eq!(forms.as_array().unwrap().len(), 1);
        assert_eq!(forms[0]["guid"], "generated-form");
    }

    #[test]
    fn canonical_form_aliases_preserve_owner_count_budget_and_duplicate_guards() {
        let entries = HashSet::from(["mesh.png"]);
        let aliases = primary_form_alias_fixture();
        let wrong_owner = aliases.replace(
            "CArtMeshSource xs.n=\"_source\" xs.ref=\"mesh\"",
            "CArtMeshSource xs.n=\"_source\" xs.ref=\"part\"",
        );
        assert!(read_xml(&wrong_owner, &entries, Budget::new(MAX_SCALARS))
            .unwrap_err()
            .contains("form belongs to another source"));

        let wrong_count = aliases.replace(
            "<carray_list xs.n=\"keyforms\" count=\"4\">",
            "<carray_list xs.n=\"keyforms\" count=\"3\">",
        );
        assert!(read_xml(&wrong_count, &entries, Budget::new(MAX_SCALARS))
            .unwrap_err()
            .contains("array count does not match content"));

        let document = Document::parse(&aliases).unwrap();
        let graph = Graph::new(&document).unwrap();
        let source = document
            .descendants()
            .find(|node| node.attribute("xs.id") == Some("mesh"))
            .unwrap();
        let list = source
            .children()
            .find(|node| node.attribute("xs.n") == Some("keyforms"))
            .unwrap();
        let data = SourceData::Part {
            child_guids: Vec::new(),
            draw_order_group: false,
            default_draw_order: 0,
            sketch: false,
            use_offscreen: false,
            clip_guids: Vec::new(),
            invert_clipping_mask: false,
            color_composition: String::new(),
            alpha_composition: String::new(),
        };
        assert!(read_owned_forms(
            &graph,
            source,
            list,
            &data,
            false,
            &mut Budget::new(3)
        )
        .is_err());

        let duplicate_coordinates = fixture().replace(
            "<s xs.n=\"coordName\">DeformerLocal</s>",
            "<s xs.n=\"coordName\">Canvas</s>",
        );
        assert!(read_xml(&duplicate_coordinates, &entries, Budget::new(MAX_SCALARS))
            .unwrap_err()
            .contains("duplicate authoring form GUID and coordinate type"));
    }

    fn art_path_fixture() -> String {
        let point = |guid: &str, x: f64, width: f64, corner: bool| format!(
            "<CArtPathPoint><CArtPathPointGuid xs.n=\"ctrlPtId\" uuid=\"{guid}\"/>\
             <CSplineCurvePoint xs.n=\"curvePointPosition\"><GVector2 xs.n=\"point\"><f xs.n=\"x\">{x}</f><f xs.n=\"y\">2</f></GVector2>\
             <GVector2 xs.n=\"startVelocity\"><f xs.n=\"x\">0.5</f><f xs.n=\"y\">-0.5</f></GVector2>\
             <GVector2 xs.n=\"endVelocity\"><f xs.n=\"x\">0.25</f><f xs.n=\"y\">0.75</f></GVector2></CSplineCurvePoint>\
             <b xs.n=\"isCorner\">{corner}</b><f xs.n=\"colorRed\">1</f><f xs.n=\"colorGreen\">0.5</f><f xs.n=\"colorBlue\">0.25</f>\
             <f xs.n=\"colorAlpha\">1</f><f xs.n=\"opacity\">0.8</f><f xs.n=\"width\">{width}</f></CArtPathPoint>");
        let path = format!(
            "<CArtPathSource xs.id=\"path\"><ACDrawableSource xs.n=\"super\"><CDrawableGuid xs.n=\"guid\" uuid=\"path-guid\"/>\
             <CDrawableId xs.n=\"id\" idstr=\"path-id\"/><CPartGuid xs.n=\"parentGuid\" uuid=\"root-guid\"/></ACDrawableSource>\
             <CArtPathBrushGuid xs.n=\"brushGuid\" uuid=\"brush-guid\"/><LineAlgorithm xs.n=\"lineAlgorithm\" v=\"LCNS\"/>\
             <LineAlignment xs.n=\"lineAlignment\" v=\"CENTER\"/><b xs.n=\"isClosed\">true</b><i xs.n=\"divNum\">20</i>\
             <carray_list xs.n=\"keyforms\" count=\"1\"><CArtPathForm><ACDrawableForm xs.n=\"super\"><ACForm xs.n=\"super\">\
             <CFormGuid xs.n=\"guid\" uuid=\"path-form\"/><CArtPathSource xs.n=\"_source\" xs.ref=\"path\"/></ACForm>\
             <CoordType xs.n=\"coordType\"><s xs.n=\"coordName\">Canvas</s></CoordType></ACDrawableForm>\
             <carray_list xs.n=\"positions\" count=\"2\">{}{}</carray_list></CArtPathForm></carray_list></CArtPathSource>",
            point("point-a", 1.0, 3.0, false), point("point-b", 5.0, 4.5, true));
        let mut xml = fixture().replace(
            "<carray_list xs.n=\"_sources\" count=\"1\"><CArtMeshSource xs.ref=\"mesh\"/></carray_list>",
            "<carray_list xs.n=\"_sources\" count=\"2\"><CArtMeshSource xs.ref=\"mesh\"/><CArtPathSource xs.ref=\"path\"/></carray_list>",
        );
        let insertion = xml.find("<GTexture2D xs.id=\"texture\">").unwrap();
        xml.insert_str(insertion, &path);
        xml
    }

    #[test]
    fn artpath_order_reader_trims_xml_whitespace_only_for_primary_artpath_forms() {
        let entries = HashSet::from(["mesh.png"]);
        let plain = art_path_fixture().replace("<CArtPathForm>", "<CArtPathForm><i xs.n='drawOrder'>950</i>");
        let scene = parse_xml(&plain, &entries, &mut Budget::new(MAX_SCALARS)).unwrap();
        let source = &scene.sources.iter().find(|source| source.guid == "path-guid").unwrap().data;
        for whitespace in ["", " ", "\t", "\n", "\r", "\r\n"] {
            for order in [i32::MIN, 950, i32::MAX] {
                let xml = plain.replace("<i xs.n='drawOrder'>950</i>",
                    &format!("<i xs.n='drawOrder'>{whitespace}{order}{whitespace}</i>"));
                let read = read_xml(&xml, &entries, Budget::new(MAX_SCALARS)).unwrap();
                let path = read["sources"].as_array().unwrap().iter().find(|source| source["guid"] == "path-guid").unwrap();
                assert_eq!(path["forms"][0]["drawOrder"], order);
                let document = Document::parse(&xml).unwrap();
                let graph = Graph::new(&document).unwrap();
                let form = document.descendants().find(|node| node.has_tag_name("CArtPathForm")).unwrap();
                assert_eq!(read_form(&graph, form, source, true, &mut Budget::new(MAX_SCALARS)).is_ok(), whitespace.is_empty());
            }
            let mesh = art_path_fixture().replace("<CArtMeshForm>",
                &format!("<CArtMeshForm><i xs.n='drawOrder'>{whitespace}950{whitespace}</i>"));
            assert_ne!(mesh, art_path_fixture());
            assert_eq!(read_xml(&mesh, &entries, Budget::new(MAX_SCALARS)).is_ok(), whitespace.is_empty());
        }
        for bad in ["2147483648", "-2147483649", "950.5", "nine", "\u{a0}950\u{a0}"] {
            let xml = plain.replace(">950</i>", &format!(">{bad}</i>"));
            assert!(read_xml(&xml, &entries, Budget::new(MAX_SCALARS)).is_err(), "{bad}");
        }
        let missing = read_xml(&art_path_fixture(), &entries, Budget::new(MAX_SCALARS)).unwrap();
        let path = missing["sources"].as_array().unwrap().iter().find(|source| source["guid"] == "path-guid").unwrap();
        assert!(path["forms"][0]["drawOrder"].is_null());
        let empty = read_xml(&plain.replace(">950</i>", "></i>"), &entries, Budget::new(MAX_SCALARS)).unwrap();
        let path = empty["sources"].as_array().unwrap().iter().find(|source| source["guid"] == "path-guid").unwrap();
        assert!(path["forms"][0]["drawOrder"].is_null());
    }

    #[test]
    fn artpath_settings_division_reader_accepts_surrounding_whitespace_only_for_division() {
        let entries = HashSet::from(["mesh.png"]);
        for count in [0_u32, 20, u32::MAX] {
            for whitespace in [" ", "\t", "\n", "\r", "\r\n"] {
                let xml = art_path_fixture().replace("<i xs.n=\"divNum\">20</i>",
                    &format!("<i xs.n=\"divNum\">{whitespace}{count}{whitespace}</i>"));
                let read = read_xml(&xml, &entries, Budget::new(MAX_SCALARS)).unwrap();
                let path = read["sources"].as_array().unwrap().iter().find(|source| source["data"]["type"] == "ArtPath").unwrap();
                assert_eq!(path["data"]["divisionCount"], count);
            }
        }
        for bad in ["", "4294967296", "-1", "20.5", "twenty"] {
            let xml = art_path_fixture().replace("<i xs.n=\"divNum\">20</i>", &format!("<i xs.n=\"divNum\">{bad}</i>"));
            assert!(read_xml(&xml, &entries, Budget::new(MAX_SCALARS)).is_err(), "{bad}");
        }
        let missing = art_path_fixture().replace("<i xs.n=\"divNum\">20</i>", "");
        assert!(read_xml(&missing, &entries, Budget::new(MAX_SCALARS)).is_err());
        for field in ["pixelWidth", "pixelHeight", "col", "row", "insertPointCount"] {
            for (value, accepted) in [("20", true), (" 20 ", false), ("\r\n20\r\n", false)] {
                let xml = format!("<holder><i xs.n='{field}'>{value}</i></holder>");
                let document = Document::parse(&xml).unwrap();
                let graph = Graph::new(&document).unwrap();
                assert_eq!(unsigned_field(&graph, document.root_element(), field).is_ok(), accepted, "{field}: {value:?}");
            }
        }
    }

    #[test]
    fn reads_art_path_brushes_and_nothing_when_the_file_stores_none() {
        let entries = HashSet::from(["mesh.png"]);
        let read = |xml: &str| read_xml(xml, &entries, Budget::new(MAX_SCALARS));
        assert_eq!(read(&fixture()).unwrap()["brushes"], json!([]));
        let with_setting = |setting: &str| fixture().replace(
            "<CPartSource xs.n=\"rootPart\" xs.ref=\"part\"/>",
            &format!("<CPartSource xs.n=\"rootPart\" xs.ref=\"part\"/>{setting}"),
        );
        assert_eq!(read(&with_setting("<CArtPathBrushSetting xs.n=\"artPathBrushesSetting\"/>")).unwrap()["brushes"], json!([]));
        let brush = |guid: &str, extra: &str| format!(
            "<CArtPathBrush><CArtPathBrushGuid xs.n=\"guid\" uuid=\"{guid}\"/>{extra}\
             <CArtPathBrush_TextureInput xs.n=\"textureInput\"><s xs.n=\"unread\">x</s></CArtPathBrush_TextureInput></CArtPathBrush>");
        let setting = format!(
            "<CArtPathBrushSetting xs.n=\"artPathBrushesSetting\"><carray_list xs.n=\"brushes\" count=\"2\">{}{}</carray_list></CArtPathBrushSetting>",
            brush("brush-a", "<CArtPathBrushId xs.n=\"id\" idstr=\"BrushA\"/><s xs.n=\"name\">Round</s>"),
            brush("brush-b", ""));
        assert_eq!(read(&with_setting(&setting)).unwrap()["brushes"], json!([
            {"guid":"brush-a", "id":"BrushA", "name":"Round", "textureInput":{}},
            {"guid":"brush-b", "id":"", "name":"", "textureInput":{}}
        ]));
        let no_guid = setting.replace("<CArtPathBrushGuid xs.n=\"guid\" uuid=\"brush-b\"/>", "");
        assert!(read(&with_setting(&no_guid)).unwrap_err().contains("lacks guid"));
        let wrong_count = setting.replace("count=\"2\"", "count=\"3\"");
        assert!(read(&with_setting(&wrong_count)).unwrap_err().contains("array count"));
        let foreign = setting.replace("<CArtPathBrush>", "<CFoo>").replace("</CArtPathBrush>", "</CFoo>");
        assert!(read(&with_setting(&foreign)).unwrap_err().contains("non-brush"));
        assert!(read(&with_setting(&format!("{setting}{setting}"))).unwrap_err().contains("duplicate authoring field"));
    }

    #[test]
    fn reads_an_art_path_with_its_points_and_does_not_evaluate_it() {
        let entries = HashSet::from(["mesh.png"]);
        let scene = read_xml(&art_path_fixture(), &entries, Budget::new(MAX_SCALARS)).unwrap();
        assert_eq!(scene["sources"][0]["guid"], "mesh-guid");
        let path = &scene["sources"][1];
        assert_eq!(path["guid"], "path-guid");
        assert_eq!(path["id"], "path-id");
        assert_eq!(path["parentPartGuid"], "root-guid");
        assert_eq!(path["data"], json!({
            "type":"ArtPath", "closed":true, "divisionCount":20, "lineAlgorithm":"LCNS", "lineAlignment":"CENTER",
            "brushGuid":"brush-guid", "clipGuids":[], "invertClippingMask":false
        }));
        assert_eq!(path["forms"][0]["coordinateType"], "Canvas");
        assert_eq!(path["forms"][0]["data"], json!({"type":"ArtPath", "points":[
            {"guid":"point-a", "position":[1.0, 2.0], "startVelocity":[0.5, -0.5], "endVelocity":[0.25, 0.75],
                "width":3.0, "opacity":0.8, "color":[1.0, 0.5, 0.25, 1.0], "corner":false},
            {"guid":"point-b", "position":[5.0, 2.0], "startVelocity":[0.5, -0.5], "endVelocity":[0.25, 0.75],
                "width":4.5, "opacity":0.8, "color":[1.0, 0.5, 0.25, 1.0], "corner":true}
        ]}));

        let not_a_point = art_path_fixture().replace("<CArtPathPoint>", "<CArtMeshForm>").replace("</CArtPathPoint>", "</CArtMeshForm>");
        assert_eq!(read_xml(&not_a_point, &entries, Budget::new(MAX_SCALARS)).unwrap_err(),
            "authoring ArtPath form holds a non-point");
        let no_width = art_path_fixture().replace("<f xs.n=\"width\">3</f>", "");
        assert_eq!(read_xml(&no_width, &entries, Budget::new(MAX_SCALARS)).unwrap_err(),
            "authoring field width lacks number");
        let no_closed = art_path_fixture().replace("<b xs.n=\"isClosed\">true</b>", "");
        assert_eq!(read_xml(&no_closed, &entries, Budget::new(MAX_SCALARS)).unwrap_err(),
            "authoring source lacks isClosed");
        let other_brush_tag = art_path_fixture().replace("<CArtPathBrushGuid xs.n", "<CDrawableGuid xs.n");
        assert_eq!(read_xml(&other_brush_tag, &entries, Budget::new(MAX_SCALARS)).unwrap_err(),
            "authoring brushGuid must use CArtPathBrushGuid");

        // A keyed ArtMesh is evaluated, and morph targets without a grid refuse the query. The same source as an
        // ArtPath gets no weights and no forms, and refuses nothing.
        let mut keyed = morph_evaluation_scene();
        let request = json!({"parameterValues": {}});
        let mesh = morph_forms_for_scene(&keyed, &request, &mut Budget::new(MAX_SCALARS)).unwrap();
        assert!(mesh["sources"][0]["weights"].is_array() && mesh["sources"][0]["evaluatedForms"].is_array());
        let grid = keyed.sources[0].grid.take();
        assert_eq!(morph_forms_for_scene(&keyed, &request, &mut Budget::new(MAX_SCALARS)).unwrap_err(),
            "authoring morph owner has no primary keyform grid");
        keyed.sources[0].grid = grid;
        keyed.sources[0].data = SourceData::ArtPath {
            closed: false, division_count: 20, line_algorithm: "LCNS".into(), line_alignment: "CENTER".into(),
            brush_guid: "brush-guid".into(), clip_guids: Vec::new(), invert_clipping_mask: false,
        };
        for form in &mut keyed.sources[0].forms {
            form.data = FormData::ArtPath { points: Vec::new() };
        }
        assert!(keyed.sources[0].morph_targets.as_ref().is_some_and(|set| !set.targets.is_empty()));
        let morph = morph_forms_for_scene(&keyed, &request, &mut Budget::new(MAX_SCALARS)).unwrap();
        assert_eq!(morph["sources"][0]["weights"], Value::Null);
        assert_eq!(morph["sources"][0]["evaluatedForms"], Value::Null);
        assert_eq!(morph["sources"][0]["morphWeights"], json!([]));
        let primary = primary_forms_for_scene(&keyed, &request, &mut Budget::new(MAX_SCALARS)).unwrap();
        assert_eq!(primary["sources"][0]["weights"], Value::Null);
        assert_eq!(primary["sources"][0]["evaluatedForms"], Value::Null);
        keyed.sources[0].grid = None;
        let morph = morph_forms_for_scene(&keyed, &request, &mut Budget::new(MAX_SCALARS)).unwrap();
        assert_eq!(morph["sources"][0]["evaluatedForms"], Value::Null);
    }

    #[test]
    fn parses_glue_sources_uids_and_raw_intensity_without_reordering_existing_sources() {
        let scene = read_xml(
            &glue_fixture(),
            &HashSet::from(["mesh.png"]),
            Budget::new(MAX_SCALARS),
        )
        .unwrap();
        assert_eq!(scene["sources"][0]["guid"], "mesh-guid");
        assert_eq!(scene["sources"][1]["guid"], "mesh-b-guid");
        assert_eq!(scene["sources"][2]["guid"], "root-guid");
        assert_eq!(scene["sources"][3]["guid"], "glue-guid");
        assert_eq!(scene["sources"][0]["data"]["vertexUids"], json!([10, 11, 12]));
        assert_eq!(scene["sources"][1]["data"]["vertexUids"], json!([20, 21, 22]));
        assert_eq!(scene["sources"][3]["data"], json!({
            "type":"Glue", "targetArtMeshAGuid":"mesh-guid",
            "targetArtMeshBGuid":"mesh-b-guid", "bindVertexUids":[11, 21, 12, 20],
            "weights":[0.2, 0.8, 0.3, 0.7]
        }));
        assert_eq!(scene["sources"][3]["forms"][0]["coordinateType"], Value::Null);
        assert_eq!(scene["sources"][3]["forms"][0]["data"], json!({"type":"Glue", "intensity":1.5}));
        let legacy = read_xml(
            &fixture(),
            &HashSet::from(["mesh.png"]),
            Budget::new(MAX_SCALARS),
        )
        .unwrap();
        assert_eq!(legacy["sources"][0]["data"].get("vertexUids"), None);

        let before = scene.clone();
        let edit = crate::authoring_edit::set_vertex(
            &scene,
            &json!({"sourceGuid":"glue-guid", "formGuid":"glue-form-guid",
                "coordinateType":"Canvas", "vertexIndex":0, "position":[12.0, 34.0]}),
        );
        assert!(edit.is_err());
        assert_eq!(scene, before);
    }

    #[test]
    fn records_glue_pairs_whose_vertex_uid_was_deleted() {
        let xml = glue_fixture().replace("11 21 12 20</long-array>", "19 21 12 20</long-array>");
        let scene = read_xml(&xml, &HashSet::from(["mesh.png"]), Budget::new(MAX_SCALARS)).unwrap();
        let glue = scene["sources"].as_array().unwrap().iter().find(|source| source["data"]["type"] == "Glue").unwrap();
        assert_eq!(glue["data"]["staleBindPairs"], json!([0]));
        assert_eq!(glue["data"]["bindVertexUids"], json!([19, 21, 12, 20]));
        let clean = read_xml(&glue_fixture(), &HashSet::from(["mesh.png"]), Budget::new(MAX_SCALARS)).unwrap();
        assert!(clean.to_string().find("staleBindPairs").is_none());
    }

    #[test]
    fn rejects_malformed_glue_guid_arrays_extensions_and_uid_references() {
        let fixture = glue_fixture();
        let missing_target_extension = fixture.replace(
            "<carray_list xs.n=\"_extensions\" count=\"1\"><CEditableMeshExtension><ACExtension xs.n=\"super\"><CArtMeshSource xs.n=\"_owner\" xs.ref=\"mesh-b\"/></ACExtension><EditableMesh xs.n=\"editableMesh\"><int-array xs.n=\"pointUid\" count=\"3\">20 21 22</int-array></EditableMesh></CEditableMeshExtension></carray_list>",
            "",
        );
        for (xml, reason) in [
            (
                fixture.replace("CAffecterGuid xs.n=\"guid\"", "CGlueGuid xs.n=\"guid\""),
                "must use CAffecterGuid",
            ),
            (
                fixture.replace(
                    "CDrawableGuid xs.n=\"targetArtMeshA_guid\"",
                    "CArtMeshGuid xs.n=\"targetArtMeshA_guid\"",
                ),
                "must use CDrawableGuid",
            ),
            (
                fixture
                    .replace("long-array xs.n=\"bindVertexUids\"", "int-array xs.n=\"bindVertexUids\"")
                    .replace("</long-array>", "</int-array>"),
                "must be a long-array",
            ),
            (
                fixture.replace("11 21 12 20</long-array>", "-1 21 12 20</long-array>"),
                "invalid authoring Glue vertex UID",
            ),
            (
                fixture.replace("11 21 12 20</long-array>", "4294967296 21 12 20</long-array>"),
                "invalid authoring Glue vertex UID",
            ),
            (
                fixture.replace("10 11 12</int-array>", "10 10 12</int-array>"),
                "vertex UIDs are not unique",
            ),
            (
                fixture.replace(
                    "<CArtMeshSource xs.n=\"_owner\" xs.ref=\"mesh\"/></ACExtension><EditableMesh",
                    "<CArtMeshSource xs.n=\"_owner\" xs.ref=\"part\"/></ACExtension><EditableMesh",
                ),
                "belongs to another source",
            ),
            (missing_target_extension, "target ArtMesh lacks vertex UIDs"),
            (
                fixture.replace(
                    "CDrawableGuid xs.n=\"targetArtMeshB_guid\" uuid=\"mesh-b-guid\"",
                    "CDrawableGuid xs.n=\"targetArtMeshB_guid\" uuid=\"mesh-guid\"",
                ),
                "targets must be distinct",
            ),
        ] {
            let error = read_xml(
                &xml,
                &HashSet::from(["mesh.png"]),
                Budget::new(MAX_SCALARS),
            )
            .unwrap_err();
            assert!(error.contains(reason), "{reason}: {error}");
        }
    }

    #[test]
    fn rejects_invalid_geometry_owner_assets_counts_and_scalar_budget() {
        let entries = HashSet::from(["mesh.png"]);
        for xml in [
            fixture().replace("0 1 2</int-array>", "0 1 3</int-array>"),
            fixture().replace("count=\"6\">0.1", "count=\"5\">0.1"),
            fixture().replace(
                "xs.n=\"_source\" xs.ref=\"mesh\"",
                "xs.n=\"_source\" xs.ref=\"part\"",
            ),
            fixture().replace(
                "<s xs.n=\"coordName\">DeformerLocal</s>",
                "<s xs.n=\"coordName\">Canvas</s>",
            ),
            fixture().replace("10 20 30 40 50 60", "NaN 20 30 40 50 60"),
        ] {
            assert!(read_xml(&xml, &entries, Budget::new(MAX_SCALARS)).is_err());
        }
        assert!(read_xml(&fixture(), &HashSet::new(), Budget::new(MAX_SCALARS)).is_err());
        assert!(read_xml(&fixture(), &entries, Budget::new(10)).is_err());
        assert!(read_xml("<root/>", &entries, Budget::new(MAX_SCALARS)).is_err());
    }

    #[test]
    fn identical_form_guids_in_different_sources_keep_their_own_payloads() {
        let mut xml = fixture();
        let start = xml.find("<CArtMeshSource xs.id=\"mesh\">").unwrap();
        let end =
            start + xml[start..].find("</CArtMeshSource>").unwrap() + "</CArtMeshSource>".len();
        let second = xml[start..end]
            .replace("xs.id=\"mesh\"", "xs.id=\"mesh-2\"")
            .replace("xs.ref=\"mesh\"", "xs.ref=\"mesh-2\"")
            .replace("uuid=\"mesh-guid\"", "uuid=\"mesh-guid-2\"")
            .replace("idstr=\"mesh-id\"", "idstr=\"mesh-id-2\"")
            .replace("11 22 33 44 55 66", "77 22 33 44 55 66");
        xml = xml.replace("<carray_list xs.n=\"_sources\" count=\"1\"><CArtMeshSource xs.ref=\"mesh\"/>",
            "<carray_list xs.n=\"_sources\" count=\"2\"><CArtMeshSource xs.ref=\"mesh\"/><CArtMeshSource xs.ref=\"mesh-2\"/>");
        xml = xml.replace("</root>", &format!("{second}</root>"));
        let scene = read_xml(&xml, &HashSet::from(["mesh.png"]), Budget::new(MAX_SCALARS)).unwrap();
        assert_eq!(
            scene["sources"][0]["forms"][0]["guid"],
            scene["sources"][1]["forms"][0]["guid"]
        );
        assert_eq!(
            scene["sources"][0]["forms"][0]["data"]["positions"][0],
            11.0
        );
        assert_eq!(
            scene["sources"][1]["forms"][0]["data"]["positions"][0],
            77.0
        );
    }

    #[test]
    fn shared_text_aliases_are_charged_before_each_copy() {
        let xml = fixture()
            .replace(
                "<CArtMeshSource xs.id=\"mesh\">",
                "<CArtMeshSource xs.id=\"mesh\"><s xs.n=\"localName\" xs.ref=\"shared-name\"/>",
            )
            .replace(
                "<CPartSource xs.id=\"part\">",
                "<CPartSource xs.id=\"part\"><s xs.n=\"localName\" xs.ref=\"shared-name\"/>",
            )
            .replace(
                "</root>",
                &format!("<s xs.id=\"shared-name\">{}</s></root>", "a".repeat(1024)),
            );
        let entries = HashSet::from(["mesh.png"]);
        let scene = read_xml(
            &xml,
            &entries,
            Budget::new(MAX_SCALARS).with_text_limit(4096),
        )
        .unwrap();
        assert_eq!(scene["sources"][0]["name"].as_str().unwrap().len(), 1024);
        assert_eq!(scene["sources"][1]["name"], scene["sources"][0]["name"]);
        let error = read_xml(
            &xml,
            &entries,
            Budget::new(MAX_SCALARS).with_text_limit(1500),
        )
        .unwrap_err();
        assert!(error.contains("copied-text byte budget"), "{error}");
    }

    #[test]
    fn preserves_nondefault_artmesh_alpha_composition() {
        let xml = fixture().replace("<CArtMeshSource xs.id=\"mesh\">", "<CArtMeshSource xs.id=\"mesh\"><AlphaComposition xs.n=\"alphaComposition\" v=\"ATOP\"/>");
        let scene = read_xml(&xml, &HashSet::from(["mesh.png"]), Budget::new(MAX_SCALARS)).unwrap();
        assert_eq!(scene["sources"][0]["data"]["alphaComposition"], "ATOP");
    }

    fn extended_fixture() -> String {
        morph_fixture().replace("<CArtMeshSource xs.id=\"mesh\">", r#"<CArtMeshSource xs.id="mesh">
          <carray_list xs.n="_extensions" count="1"><CExtendedInterpolationExtension>
            <ACExtension xs.n="super"><CExtensionGuid xs.n="guid" uuid="extension-guid"/><CArtMeshSource xs.n="_owner" xs.ref="mesh"/></ACExtension>
            <KeyformGridSource xs.n="extendedKeyformGridSource">
              <array_list xs.n="keyformBindings" count="0"/>
              <array_list xs.n="keyformsOnGrid" count="1"><KeyformOnGrid>
                <KeyformGridAccessKey xs.n="accessKey"><array_list xs.n="_keyOnParameterList" count="0"/></KeyformGridAccessKey>
                <CFormGuid xs.n="keyformGuid" uuid="generated-form"/>
              </KeyformOnGrid></array_list>
            </KeyformGridSource>
            <carray_list xs.n="normalExtendedKeyforms" count="1"><CArtMeshForm>
              <ACDrawableForm xs.n="super"><ACForm xs.n="super"><CFormGuid xs.n="guid" uuid="generated-form"/><CArtMeshSource xs.n="_source" xs.ref="mesh" xs.id="generated-owner"/></ACForm><CoordType xs.n="coordType"><s xs.n="coordName">DeformerLocal</s></CoordType></ACDrawableForm>
              <float-array xs.n="positions" count="4">0.1 0.2 0.3 0.4</float-array>
            </CArtMeshForm></carray_list>
            <carray_list xs.n="morphTargetExtendedKeyforms" count="0"/>
            <ExtendedKeyFormMorphTargetSet xs.n="_extendedKeyformMorphTargetSet">
              <carray_list xs.n="_morphTargets" count="2">
                <ExtendedKeyFormMorphTarget><CFormGuid xs.n="keyformGuid" uuid="generated-form"/><CParameterGuid xs.n="parameterGuid" uuid="parameter-guid"/><f xs.n="keyValue">0.25</f></ExtendedKeyFormMorphTarget>
                <ExtendedKeyFormMorphTarget><CFormGuid xs.n="keyformGuid" uuid="missing-cached-form"/><CParameterGuid xs.n="parameterGuid" uuid="parameter-guid"/><f xs.n="keyValue">0.5</f></ExtendedKeyFormMorphTarget>
              </carray_list>
              <hash_map xs.n="extendedInterpolationSettings" count="1"><entry>
                <CParameterGuid xs.n="key" uuid="parameter-guid"/>
                <ExtendedInterpolationSetting xs.n="value"><ExtendedInterpolationType xs.n="type" v="SNS_CURVE"/><i xs.n="insertPointCount">5</i><f xs.n="extendedInterpolationScale">1</f></ExtendedInterpolationSetting>
              </entry></hash_map>
            </ExtendedKeyFormMorphTargetSet>
          </CExtendedInterpolationExtension></carray_list>"#)
    }

    /// Replaces the zero-axis extended grid with a one-axis grid whose second cell uses a form that was never stored.
    fn stale_extended_grid(parameter_guid: &str, insert_point_count: u32, absent_index: usize) -> String {
        let cell = |index: usize| {
            let form = if index == absent_index { "absent-form" } else { "generated-form" };
            format!(r#"<KeyformOnGrid><KeyformGridAccessKey xs.n="accessKey"><array_list xs.n="_keyOnParameterList" count="1">
                <KeyOnParameter><KeyformBindingSource xs.n="binding" xs.ref="axis"/><i xs.n="keyIndex">{index}</i></KeyOnParameter>
              </array_list></KeyformGridAccessKey><CFormGuid xs.n="keyformGuid" uuid="{form}"/></KeyformOnGrid>"#)
        };
        let grid = format!(r#"<KeyformGridSource xs.n="extendedKeyformGridSource">
              <array_list xs.n="keyformBindings" count="1"><KeyformBindingSource xs.id="axis">
                <CParameterGuid xs.n="parameterGuid" uuid="{parameter_guid}"/>
                <array_list xs.n="keys" count="2"><f>0</f><f>1</f></array_list>
                <InterpolationType xs.n="interpolationType" v="LINEAR"/>
                <ExtendedInterpolationType xs.n="extendedInterpolationType" v="ELLIPSE"/>
                <i xs.n="insertPointCount">{insert_point_count}</i><f xs.n="extendedInterpolationScale">1</f>
              </KeyformBindingSource></array_list>
              <array_list xs.n="keyformsOnGrid" count="2">{}{}</array_list>
            </KeyformGridSource>"#, cell(0), cell(1));
        let fixture = extended_fixture();
        let start = fixture.find("<KeyformGridSource xs.n=\"extendedKeyformGridSource\">").unwrap();
        let end = start + fixture[start..].find("</KeyformGridSource>").unwrap() + "</KeyformGridSource>".len();
        format!("{}{}{}", &fixture[..start], grid, &fixture[end..])
    }

    #[test]
    fn records_extended_cells_without_a_stored_form_only_at_generated_positions() {
        let entries = HashSet::from(["mesh.png"]);
        let read = |xml: &str| read_xml(xml, &entries, Budget::new(MAX_SCALARS));
        let extension = |scene: &Value| scene["sources"][0]["extendedInterpolations"][0].clone();

        // Unresolved axis: the GUID is kept, parameterId is null and the missing form is recorded.
        let scene = read(&stale_extended_grid("deleted-parameter", 0, 1)).unwrap();
        assert_eq!(extension(&scene)["grid"]["axes"][0]["parameterId"], Value::Null);
        assert_eq!(extension(&scene)["grid"]["axes"][0]["parameterGuid"], "deleted-parameter");
        assert_eq!(extension(&scene)["missingFormGuids"], json!(["absent-form"]));

        // Resolved axis: index 1 is an inserted point when insertPointCount is 1.
        let scene = read(&stale_extended_grid("parameter-guid", 1, 1)).unwrap();
        assert_eq!(extension(&scene)["grid"]["axes"][0]["parameterId"], "ParamMorph");
        assert_eq!(extension(&scene)["missingFormGuids"], json!(["absent-form"]));

        // Primary positions stay mandatory, and a resolved axis without inserted points has none.
        for xml in [stale_extended_grid("parameter-guid", 1, 0), stale_extended_grid("parameter-guid", 0, 1)] {
            assert!(read(&xml).unwrap_err().contains("extended grid references"));
        }
    }

    fn remove_form_list(xml: &mut String, name: &str) {
        let marker = format!("<carray_list xs.n=\"{name}\"");
        let start = xml.find(&marker).unwrap();
        let open_end = start + xml[start..].find('>').unwrap();
        let end = if xml[start..=open_end].ends_with("/>") {
            open_end + 1
        } else {
            start + xml[start..].find("</carray_list>").unwrap() + "</carray_list>".len()
        };
        xml.replace_range(start..end, "");
    }

    #[test]
    fn extended_forms_retain_stale_geometry_and_unavailable_target_references() {
        let scene = read_xml(
            &extended_fixture(),
            &HashSet::from(["mesh.png"]),
            Budget::new(MAX_SCALARS),
        )
        .unwrap();
        let source = &scene["sources"][0];
        let extension = &source["extendedInterpolations"][0];
        assert_eq!(extension["guid"], "extension-guid");
        assert_eq!(
            extension["normalForms"][0]["vertexCountMatchesSource"],
            false
        );
        assert_eq!(
            extension["normalForms"][0]["data"]["positions"],
            json!([0.1, 0.2, 0.3, 0.4])
        );
        assert!(source["forms"][0].get("vertexCountMatchesSource").is_none());
        assert_eq!(extension["grid"]["cells"][0]["formGuid"], "generated-form");
        assert_eq!(
            extension["morphTargets"]["targets"][0]["formAvailable"],
            true
        );
        assert_eq!(
            extension["morphTargets"]["targets"][1]["formAvailable"],
            false
        );
        assert_eq!(
            extension["morphTargets"]["targets"][1]["formGuid"],
            "missing-cached-form"
        );
        assert_eq!(
            extension["morphTargets"]["settings"],
            json!([{
                "parameterGuid":"parameter-guid", "parameterId":"ParamMorph", "interpolation":"SNS_CURVE",
                "insertPointCount":5, "extendedInterpolationScale":1.0
            }])
        );
    }

    #[test]
    fn legacy_extended_forms_match_modern_normalized_response() {
        let mut legacy = extended_fixture();
        remove_form_list(&mut legacy, "morphTargetExtendedKeyforms");
        let marker = "<carray_list xs.n=\"normalExtendedKeyforms\"";
        let start = legacy.find(marker).unwrap();
        let end = start
            + legacy[start..].find("</carray_list>").unwrap()
            + "</carray_list>".len();
        let forms = legacy[start..end].replace("normalExtendedKeyforms", "extendedKeyforms");
        legacy.replace_range(start..end, &forms);

        let entries = HashSet::from(["mesh.png"]);
        let modern = read_xml(&extended_fixture(), &entries, Budget::new(MAX_SCALARS)).unwrap();
        let legacy = read_xml(&legacy, &entries, Budget::new(MAX_SCALARS)).unwrap();
        assert_eq!(
            legacy["sources"][0]["extendedInterpolations"][0],
            modern["sources"][0]["extendedInterpolations"][0]
        );
    }

    #[test]
    fn rejects_missing_or_mixed_extended_form_lists() {
        let entries = HashSet::from(["mesh.png"]);
        let mut missing = extended_fixture();
        remove_form_list(&mut missing, "normalExtendedKeyforms");
        remove_form_list(&mut missing, "morphTargetExtendedKeyforms");
        let error = read_xml(&missing, &entries, Budget::new(MAX_SCALARS)).unwrap_err();
        assert!(error.contains("lacks extended keyform lists"), "{error}");

        let mut partial = extended_fixture();
        remove_form_list(&mut partial, "morphTargetExtendedKeyforms");
        let error = read_xml(&partial, &entries, Budget::new(MAX_SCALARS)).unwrap_err();
        assert!(error.contains("requires both modern keyform lists"), "{error}");

        let mixed = extended_fixture().replace(
            "<carray_list xs.n=\"morphTargetExtendedKeyforms\" count=\"0\"/>",
            concat!(
                "<carray_list xs.n=\"morphTargetExtendedKeyforms\" count=\"0\"/>",
                "<carray_list xs.n=\"extendedKeyforms\" count=\"0\"/>"
            ),
        );
        let error = read_xml(&mixed, &entries, Budget::new(MAX_SCALARS)).unwrap_err();
        assert!(error.contains("mixes legacy and modern keyform lists"), "{error}");
    }

    #[test]
    fn extended_targets_do_not_require_generated_forms_or_a_grid() {
        let mut xml = extended_fixture();
        let start = xml
            .find("<KeyformGridSource xs.n=\"extendedKeyformGridSource\">")
            .unwrap();
        let end = start
            + xml[start..].find("</KeyformGridSource>").unwrap()
            + "</KeyformGridSource>".len();
        xml.replace_range(start..end, "<null xs.n=\"extendedKeyformGridSource\"/>");
        let start = xml
            .find("<carray_list xs.n=\"normalExtendedKeyforms\"")
            .unwrap();
        let end = start + xml[start..].find("</carray_list>").unwrap() + "</carray_list>".len();
        xml.replace_range(
            start..end,
            "<carray_list xs.n=\"normalExtendedKeyforms\" count=\"0\"/>",
        );
        let scene = read_xml(&xml, &HashSet::from(["mesh.png"]), Budget::new(MAX_SCALARS)).unwrap();
        let extension = &scene["sources"][0]["extendedInterpolations"][0];
        assert!(extension["grid"].is_null());
        assert!(extension["normalForms"].as_array().unwrap().is_empty());
        assert!(extension["morphForms"].as_array().unwrap().is_empty());
        assert_eq!(
            extension["morphTargets"]["targets"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            extension["morphTargets"]["settings"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        for target in extension["morphTargets"]["targets"].as_array().unwrap() {
            assert_eq!(target["formAvailable"], false);
        }
    }

    #[test]
    fn rejects_extended_owner_grid_parameter_numeric_and_position_pair_errors() {
        let fixture = extended_fixture();
        for (xml, reason) in [
            (
                fixture.replace(
                    "xs.n=\"_owner\" xs.ref=\"mesh\"",
                    "xs.n=\"_owner\" xs.ref=\"part\"",
                ),
                "extended interpolation belongs",
            ),
            (
                fixture.replace(
                    "xs.ref=\"mesh\" xs.id=\"generated-owner\"",
                    "xs.ref=\"part\" xs.id=\"generated-owner\"",
                ),
                "form belongs",
            ),
            (
                fixture.replace(
                    "xs.n=\"keyformGuid\" uuid=\"generated-form\"",
                    "xs.n=\"keyformGuid\" uuid=\"foreign-grid-form\"",
                ),
                "extended grid references",
            ),
            (
                fixture.replace(
                    "xs.n=\"key\" uuid=\"parameter-guid\"",
                    "xs.n=\"key\" uuid=\"unknown-parameter\"",
                ),
                "extended setting references",
            ),
            (
                fixture.replace(
                    "xs.n=\"extendedInterpolationScale\">1",
                    "xs.n=\"extendedInterpolationScale\">NaN",
                ),
                "invalid finite",
            ),
            (
                fixture.replace(
                    "xs.n=\"insertPointCount\">5",
                    "xs.n=\"insertPointCount\">-1",
                ),
                "invalid authoring unsigned",
            ),
            (
                fixture.replace("count=\"4\">0.1 0.2 0.3 0.4", "count=\"3\">0.1 0.2 0.3"),
                "position pairs",
            ),
        ] {
            let error =
                read_xml(&xml, &HashSet::from(["mesh.png"]), Budget::new(MAX_SCALARS)).unwrap_err();
            assert!(error.contains(reason), "{reason}: {error}");
        }
    }

    fn morph_fixture() -> String {
        fixture()
            .replace(
                "<CParameterSourceSet xs.n=\"parameterSourceSet\"><carray_list xs.n=\"_sources\" count=\"0\"/>",
                r#"<CParameterSourceSet xs.n="parameterSourceSet"><carray_list xs.n="_sources" count="1"><CParameterSource><CParameterGuid xs.n="guid" uuid="parameter-guid"/><CParameterId xs.n="id" idstr="ParamMorph"/><f xs.n="minValue">-1</f><f xs.n="maxValue">1</f><f xs.n="defaultValue">0</f></CParameterSource></carray_list>"#,
            )
            .replace(
                "<CArtMeshSource xs.id=\"mesh\">",
                r#"<CArtMeshSource xs.id="mesh"><KeyFormMorphTargetSet xs.n="keyformMorphTargetSet">
                  <carray_list xs.n="_morphTargets" count="1"><KeyFormMorphTarget>
                    <CParameterGuid xs.n="parameterGuid" uuid="parameter-guid" note="StaleParameterId"/><f xs.n="keyValue">-1</f>
                    <CFormGuid xs.n="keyformGuid" uuid="form-guid"/><CArtMeshSource xs.n="owner" xs.ref="mesh"/>
                    <MorphTargetEditBaseParameterMap xs.n="editBaseParameterMap"><null xs.n="baseParameterMap"/></MorphTargetEditBaseParameterMap>
                  </KeyFormMorphTarget></carray_list>
                  <MorphTargetBlendWeightConstraintSet xs.n="blendWeightConstraintSet"><carray_list xs.n="_constraints" count="1"><MorphTargetBlendWeightConstraint>
                    <CParameterGuid xs.n="morphTargetParameterGuid" uuid="parameter-guid"/>
                    <CParameterGuid xs.n="constraintParameterGuid" uuid="parameter-guid"/>
                    <f xs.n="constraintParameterValue">0.5</f><f xs.n="blendWeight">0.25</f>
                  </MorphTargetBlendWeightConstraint></carray_list></MorphTargetBlendWeightConstraintSet>
                </KeyFormMorphTargetSet>"#,
            )
    }

    fn morph_evaluation_scene() -> Scene {
        use crate::authoring_keyforms::{KeyformCell, KeyformGrid};

        let mut scene = parse_xml(
            &morph_fixture(),
            &HashSet::from(["mesh.png"]),
            &mut Budget::new(MAX_SCALARS),
        )
        .unwrap();
        scene.parameters[0].parameter_type = "MORPH_TARGET".into();
        scene.parameters.push(Parameter {
            guid: "parameter-2-guid".into(),
            id: "ParamMorph2".into(),
            name: "Morph two".into(),
            minimum: -2.0,
            maximum: 2.0,
            default: 0.0,
            repeat: false,
            parameter_type: "MORPH_TARGET".into(),
        });
        let source = scene
            .sources
            .iter_mut()
            .find(|source| source.guid == "mesh-guid")
            .unwrap();
        source.grid = Some(KeyformGrid {
            axes: Vec::new(),
            cells: vec![KeyformCell {
                indices: Vec::new(),
                form_guid: "form-guid".into(),
            }],
        });
        let bases = source.forms.iter().map(|form| {
            let positions = match &form.data {
                FormData::ArtMesh { positions } => positions,
                _ => panic!("fixture should be an ArtMesh"),
            };
            [
                ("negative", 2.0),
                ("positive", 4.0),
                ("second-channel", 8.0),
            ]
            .into_iter()
            .map(|(guid_suffix, delta)| Form {
                guid: format!("target-{guid_suffix}"),
                coordinate_type: form.coordinate_type.clone(),
                animated: false,
                local_animated: false,
                opacity: form.opacity,
                draw_order: form.draw_order,
                multiply_color: form.multiply_color,
                screen_color: form.screen_color,
                vertex_count_matches_source: None,
                data: FormData::ArtMesh {
                    positions: positions.iter().map(|value| value + delta).collect(),
                },
            })
            .collect::<Vec<_>>()
        }).flatten().collect::<Vec<_>>();
        source.forms.extend(bases);
        source.morph_targets = Some(MorphTargets {
            targets: vec![
                MorphTarget {
                    parameter_guid: "parameter-guid".into(),
                    parameter_id: "ParamMorph".into(),
                    key_value: -1.0,
                    form_guid: "target-negative".into(),
                    edit_base_parameter_map: (),
                },
                MorphTarget {
                    parameter_guid: "parameter-guid".into(),
                    parameter_id: "ParamMorph".into(),
                    key_value: 1.0,
                    form_guid: "target-positive".into(),
                    edit_base_parameter_map: (),
                },
                MorphTarget {
                    parameter_guid: "parameter-2-guid".into(),
                    parameter_id: "ParamMorph2".into(),
                    key_value: 2.0,
                    form_guid: "target-second-channel".into(),
                    edit_base_parameter_map: (),
                },
            ],
            blend_weight_constraints: Vec::new(),
        });
        scene
    }

    pub(crate) fn native_preview_scene() -> ParsedScene {
        ParsedScene {
            scene: morph_evaluation_scene(),
            post_parse_budget: Budget::new(MAX_SCALARS),
        }
    }

    pub(crate) fn overlapping_native_preview_scene() -> ParsedScene {
        let mut scene = morph_evaluation_scene();
        let mut back = morph_evaluation_scene().sources.remove(0);
        back.guid = "back-mesh-guid".into();
        for form in &mut scene.sources[0].forms {
            form.multiply_color = [1.0, 0.0, 0.0, 1.0];
            if let FormData::ArtMesh { positions } = &mut form.data {
                *positions = vec![8.0, 8.0, 40.0, 8.0, 8.0, 40.0];
            }
        }
        for form in &mut back.forms {
            form.multiply_color = [0.0, 0.0, 1.0, 1.0];
            if let FormData::ArtMesh { positions } = &mut form.data {
                *positions = vec![8.0, 8.0, 40.0, 8.0, 8.0, 40.0];
            }
        }
        scene.sources.push(back);
        ParsedScene { scene, post_parse_budget: Budget::new(MAX_SCALARS) }
    }

    fn sns_evaluation_scene() -> Scene {
        let mut scene = morph_evaluation_scene();
        let owner = &mut scene.sources[0];
        let mut forms = Vec::new();
        let mut targets = Vec::new();
        for (key, guid, delta) in [(-0.5, "sns-negative", 6.0), (0.5, "sns-positive", 10.0)] {
            for base in owner.forms.iter().filter(|form| form.guid == "form-guid") {
                let FormData::ArtMesh { positions } = &base.data else { panic!("mesh fixture") };
                forms.push(Form {
                    guid: guid.into(), coordinate_type: base.coordinate_type.clone(),
                    animated: false, local_animated: false, opacity: base.opacity,
                    draw_order: base.draw_order, multiply_color: base.multiply_color,
                    screen_color: base.screen_color, vertex_count_matches_source: Some(true),
                    data: FormData::ArtMesh { positions: positions.iter().map(|value| value + delta).collect(),
                    },
                });
            }
            targets.push(ExtendedMorphTarget {
                parameter_guid: "parameter-guid".into(), parameter_id: "ParamMorph".into(),
                key_value: key, form_guid: guid.into(), form_available: true,
            });
        }
        owner.extended_interpolations.push(ExtendedInterpolation {
            guid: "sns-extension".into(), grid: None, normal_forms: Vec::new(), morph_forms: forms,
            morph_targets: Some(ExtendedMorphTargets {
                targets, settings: vec![ExtendedInterpolationSetting {
                    parameter_guid: "parameter-guid".into(), parameter_id: "ParamMorph".into(),
                    interpolation: "SNS_CURVE".into(), insert_point_count: 1, extended_interpolation_scale: 1.0,
                }],
            }),
            missing_form_guids: Vec::new(),
        });
        scene
    }

    #[test]
    fn stored_sns_keys_replace_linear_morph_intervals_and_preserve_constraints() {
        let mut scene = sns_evaluation_scene();
        for (sample, delta, guid) in [(-1.0, 2.0, "target-negative"), (-0.5, 6.0, "sns-negative"),
            (-0.25, 3.0, "sns-negative"), (0.0, 0.0, ""), (0.5, 10.0, "sns-positive"), (1.0, 4.0, "target-positive"),
        ] {
            let response = morph_forms_for_scene(&scene,
                &json!({"sourceGuid":"mesh-guid", "parameterValues":{"ParamMorph":sample,"ParamMorph2":1.0}}),
                &mut Budget::new(MAX_SCALARS)).unwrap();
            let row = &response["sources"][0];
            assert_eq!(row["evaluatedForms"][0]["data"]["positions"][0], 11.0 + delta + 4.0);
            assert_eq!(row["extendedMorphExtensions"], json!(["sns-extension"]));
            if !guid.is_empty() { assert_eq!(row["morphWeights"][0]["formGuid"], guid); }
        }
        scene.sources[0].morph_targets.as_mut().unwrap().blend_weight_constraints = [-2.0, 2.0].map(|value|
            BlendWeightConstraint {
                morph_target_parameter_guid: "parameter-guid".into(), morph_target_parameter_id: "ParamMorph".into(),
                constraint_parameter_guid: "parameter-2-guid".into(), constraint_parameter_id: "ParamMorph2".into(),
                constraint_parameter_value: value, blend_weight: 0.5,
            }).to_vec();
        let response = morph_forms_for_scene(&scene,
            &json!({"sourceGuid":"mesh-guid", "parameterValues":{"ParamMorph":-0.5}}),
            &mut Budget::new(MAX_SCALARS),
        ).unwrap();
        assert_eq!(response["sources"][0]["evaluatedForms"][0]["data"]["positions"][0], 14.0);
    }

    #[test]
    fn selected_sns_cache_is_validated_at_neutral_and_inactive_cache_is_ignored() {
        let request = json!({"sourceGuid":"mesh-guid"});
        for case in 0..6 {
            let mut scene = sns_evaluation_scene();
            let extension = &mut scene.sources[0].extended_interpolations[0];
            let generated = extension.morph_targets.as_mut().unwrap();
            match case {
                0 => generated.targets[0].form_available = false,
                1 => generated.settings[0].interpolation = "UNKNOWN".into(),
                2 => generated.targets[0].key_value = generated.targets[1].key_value,
                3 => generated.settings[0].insert_point_count = 2,
                4 => extension.morph_forms[0].vertex_count_matches_source = Some(false),
                5 => generated.settings.push(ExtendedInterpolationSetting {
                    parameter_guid: "parameter-guid".into(), parameter_id: "ParamMorph".into(),
                    interpolation: "SNS_CURVE".into(), insert_point_count: 1, extended_interpolation_scale: 1.0,
                }),
                _ => unreachable!(),
            }
            assert!(morph_forms_for_scene(&scene, &request, &mut Budget::new(MAX_SCALARS)).is_err(), "case {case}");
            scene.sources[0].extended_interpolations[0].morph_targets.as_mut().unwrap().settings.clear();
            let response = morph_forms_for_scene(&scene, &request, &mut Budget::new(MAX_SCALARS)).unwrap();
            assert!(response["sources"][0].get("extendedMorphExtensions").is_none());
            assert_eq!(response["sources"][0]["evaluatedForms"][0]["data"]["positions"][0], 11.0);
        }
        assert!(morph_forms_for_scene(&sns_evaluation_scene(), &request, &mut Budget::new(1)).is_err());
    }

    #[test]
    fn morph_forms_add_multiple_parameter_deltas_and_report_only_active_weights() {
        let scene = morph_evaluation_scene();
        let result = morph_forms_for_scene(
            &scene,
            &json!({"sourceGuid":"mesh-guid", "parameterValues":{"ParamMorph":-0.5,"ParamMorph2":1.0}}),
            &mut Budget::new(MAX_SCALARS),
        )
        .unwrap();
        assert_eq!(result["format"], "live2d.authoring.morph-forms.v1");
        let source = &result["sources"][0];
        assert_eq!(source["baseFormGuid"], "form-guid");
        assert_eq!(
            source["morphWeights"],
            json!([
                {"parameterId":"ParamMorph","keyValue":-1.0,"formGuid":"target-negative","weight":0.5},
                {"parameterId":"ParamMorph2","keyValue":2.0,"formGuid":"target-second-channel","weight":0.5}
            ])
        );
        assert_eq!(source["evaluatedForms"][0]["data"]["positions"][0], 16.0);

        let mut shuffled = morph_evaluation_scene();
        shuffled.sources[0]
            .morph_targets
            .as_mut()
            .unwrap()
            .targets
            .swap(0, 1);
        let reordered = morph_forms_for_scene(
            &shuffled,
            &json!({"sourceGuid":"mesh-guid", "parameterValues":{"ParamMorph":-0.5,"ParamMorph2":1.0}}),
            &mut Budget::new(MAX_SCALARS),
        )
        .unwrap();
        assert_eq!(reordered["sources"][0]["morphWeights"], source["morphWeights"]);

        let neutral = morph_forms_for_scene(
            &scene,
            &json!({"sourceGuid":"mesh-guid", "parameterValues":{"ParamMorph":0,"ParamMorph2":0}}),
            &mut Budget::new(MAX_SCALARS),
        )
        .unwrap();
        assert_eq!(neutral["sources"][0]["morphWeights"], json!([]));
        assert_eq!(neutral["sources"][0]["baseFormGuid"], "form-guid");
        assert_eq!(neutral["sources"][0]["evaluatedForms"][0]["data"]["positions"][0], 11.0);
    }

    #[test]
    fn morph_forms_return_unchanged_primary_fields_when_no_targets_exist() {
        use crate::authoring_keyforms::{KeyformCell, KeyformGrid};

        let scene = parse_xml(
            &fixture(),
            &HashSet::from(["mesh.png"]),
            &mut Budget::new(MAX_SCALARS),
        )
        .unwrap();
        let mut scene = scene;
        scene.sources[0].grid = Some(KeyformGrid {
            axes: Vec::new(),
            cells: vec![KeyformCell {
                indices: Vec::new(),
                form_guid: "form-guid".into(),
            }],
        });
        let result = morph_forms_for_scene(
            &scene,
            &json!({"sourceGuid":"mesh-guid"}),
            &mut Budget::new(MAX_SCALARS),
        )
        .unwrap();
        assert_eq!(result["sources"][0]["baseFormGuid"], Value::Null);
        assert_eq!(result["sources"][0]["morphWeights"], json!([]));
        assert_eq!(result["sources"][0]["evaluatedForms"][0]["data"]["positions"], json!([11.0,22.0,33.0,44.0,55.0,66.0]));
    }

    #[test]
    fn morph_forms_reject_invalid_constraints_parameters_ranges_bases_and_budgets() {
        let request = json!({"sourceGuid":"mesh-guid", "parameterValues":{"ParamMorph":0,"ParamMorph2":0}});
        let mut scene = morph_evaluation_scene();
        scene.sources[0].morph_targets.as_mut().unwrap().blend_weight_constraints.push(
            BlendWeightConstraint {
                morph_target_parameter_guid: "parameter-guid".into(),
                morph_target_parameter_id: "ParamMorph".into(),
                constraint_parameter_guid: "parameter-guid".into(),
                constraint_parameter_id: "ParamMorph".into(),
                constraint_parameter_value: 0.5,
                blend_weight: 0.5,
            });
        assert!(morph_forms_for_scene(&scene, &request, &mut Budget::new(MAX_SCALARS)).is_err());

        for mutate in [
            0usize, // wrong target parameter type
            1, // repeating target parameter
            2, // neutral key outside parameter range
            3, // sampled target parameter outside its model range
            4, // non-finite target key
            5, // duplicated keys
            6, // zero target key
            7, // no exact normal default key
            8, // ambiguous normal default cell
            9, // no primary grid
            10, // target GUID overlaps a primary grid cell
        ] {
            let mut scene = morph_evaluation_scene();
            match mutate {
                0 => scene.parameters[0].parameter_type = "NORMAL".into(),
                1 => scene.parameters[0].repeat = true,
                2 => scene.parameters[0].minimum = 0.25,
                3 => {
                    scene.parameters[0].minimum = -0.25;
                    scene.parameters[0].maximum = 0.25;
                }
                4 => {
                    scene.sources[0].morph_targets.as_mut().unwrap().targets[0].key_value = f64::NAN
                }
                5 => scene.sources[0].morph_targets.as_mut().unwrap().targets[1].key_value = -1.0,
                6 => scene.sources[0].morph_targets.as_mut().unwrap().targets[0].key_value = 0.0,
                7 => {
                    scene.sources[0].grid.as_mut().unwrap().axes = vec![
                    crate::authoring_keyforms::KeyformAxis {
                        binding_ref: "axis".into(),
                        parameter_guid: "parameter-2-guid".into(),
                        parameter_id: Some("ParamMorph2".into()),
                        keys: vec![-2.0, 2.0],
                        interpolation: "LINEAR".into(),
                        extended_interpolation: "LINEAR".into(),
                        insert_point_count: 0,
                        extended_interpolation_scale: 1.0,
                    }]
                }
                8 => scene.sources[0].grid.as_mut().unwrap().cells.push(
                    crate::authoring_keyforms::KeyformCell {
                        indices: Vec::new(),
                        form_guid: "other-default".into(),
                    },
                ),
                9 => scene.sources[0].grid = None,
                10 => {
                    scene.sources[0]
                        .morph_targets
                        .as_mut()
                        .unwrap()
                        .targets[0]
                        .form_guid = "form-guid".into();
                }
                _ => unreachable!(),
            }
            let case_request = if mutate == 3 {
                json!({"sourceGuid":"mesh-guid", "parameterValues":{"ParamMorph":0.5,"ParamMorph2":0}})
            } else {
                request.clone()
            };
            let error = morph_forms_for_scene(&scene, &case_request, &mut Budget::new(MAX_SCALARS))
                .unwrap_err();
            assert!(!error.is_empty(), "case {mutate} should fail");
        }
        let scene = morph_evaluation_scene();
        assert!(morph_forms_for_scene(&scene, &request, &mut Budget::new(1)).is_err());
    }

    #[test]
    fn morph_constraints_interpolate_and_choose_the_smallest_controller_limit() {
        let mut scene = morph_evaluation_scene();
        scene.parameters.push(Parameter {
            guid: "controller-guid".into(), id: "Controller".into(), name: "Controller".into(),
            minimum: -1.0, maximum: 1.0, default: 1.0, repeat: false, parameter_type: "NORMAL".into(),
        });
        let set = scene.sources[0].morph_targets.as_mut().unwrap();
        for (controller, controller_guid, points) in [
            ("ParamMorph2", "parameter-2-guid", vec![(-2.0, 1.0), (0.0, 0.8), (2.0, 0.0)],
            ),
            ("Controller", "controller-guid", vec![(-1.0, 1.0), (1.0, 0.5)],
            ),
        ] {
            for (value, limit) in points {
                set.blend_weight_constraints.push(BlendWeightConstraint {
                    morph_target_parameter_guid: "parameter-guid".into(),
                    morph_target_parameter_id: "ParamMorph".into(),
                    constraint_parameter_guid: controller_guid.into(),
                    constraint_parameter_id: controller.into(),
                    constraint_parameter_value: value,
                    blend_weight: limit,
                });
            }
        }
        for (controller_value, expected) in [(-2.0, 0.5), (-1.0, 0.5), (0.0, 0.5), (1.0, 0.4), (2.0, 0.0)] {
            let response = morph_forms_for_scene(&scene,
                &json!({"sourceGuid":"mesh-guid", "parameterValues":{"ParamMorph":1,"ParamMorph2":controller_value}}),
                &mut Budget::new(MAX_SCALARS)).unwrap();
            let row = &response["sources"][0];
            let actual = row["morphWeights"].as_array().unwrap().iter()
                .find(|weight| weight["parameterId"] == "ParamMorph")
                .map_or(0.0, |weight| weight["weight"].as_f64().unwrap());
            assert!((actual - expected).abs() < 1e-12);
        }
        scene.sources[0].morph_targets.as_mut().unwrap().blend_weight_constraints.reverse();
        let request = json!({"sourceGuid":"mesh-guid", "parameterValues":{"ParamMorph":1,"ParamMorph2":0}});
        let response = morph_forms_for_scene(&scene, &request, &mut Budget::new(MAX_SCALARS)).unwrap();
        assert_eq!(response["sources"][0]["morphWeights"][0]["weight"], 0.5);
        for case in 0..10 {
            let mut malformed = morph_evaluation_scene();
            malformed.parameters.push(Parameter {
                guid: "controller-guid".into(), id: "Controller".into(), name: "Controller".into(),
                minimum: -1.0, maximum: 1.0, default: 1.0, repeat: false, parameter_type: "NORMAL".into(),
            });
            malformed.sources[0].morph_targets.as_mut().unwrap().blend_weight_constraints =
                scene.sources[0].morph_targets.as_ref().unwrap().blend_weight_constraints.clone();
            let points = &mut malformed.sources[0].morph_targets.as_mut().unwrap().blend_weight_constraints;
            match case {
                0 => points[0].blend_weight = 1.1,
                1 => points[0].blend_weight = f64::NAN,
                2 => points[0].constraint_parameter_value = f64::INFINITY,
                3 => points[0].constraint_parameter_value = 0.5,
                4 => points.push(points[0].clone()),
                5 => points[0].constraint_parameter_guid = "missing".into(),
                6 => points[0].morph_target_parameter_guid = "missing".into(),
                7 => malformed.parameters[0].repeat = true,
                8 => points[0].constraint_parameter_id = "missing".into(),
                9 => {
                    points[0].constraint_parameter_id = "ParamMorph".into();
                    points[0].constraint_parameter_guid = "parameter-guid".into();
                }
                _ => unreachable!(),
            }
            let neutral = json!({"sourceGuid":"mesh-guid", "parameterValues":{"ParamMorph":0,"ParamMorph2":0}});
            assert!(morph_forms_for_scene(&malformed, &neutral, &mut Budget::new(MAX_SCALARS)).is_err(), "case {case}");
        }
    }

    #[test]
    fn morph_defaults_can_differ_from_the_neutral_zero_key() {
        let mut scene = morph_evaluation_scene();
        scene.parameters[0].default = 1.0;
        let response = morph_forms_for_scene(&scene, &json!({"sourceGuid":"mesh-guid"}), &mut Budget::new(MAX_SCALARS),
        ).unwrap();
        assert_eq!(response["sources"][0]["morphWeights"][0]["weight"], 1.0);
        assert_eq!(response["sources"][0]["morphWeights"][0]["keyValue"], 1.0);
        let neutral = morph_forms_for_scene(&scene, &json!({"sourceGuid":"mesh-guid", "parameterValues":{"ParamMorph":0}}),
            &mut Budget::new(MAX_SCALARS),
        ).unwrap();
        assert_eq!(neutral["sources"][0]["morphWeights"], json!([]));
        scene.parameters[0].minimum = 0.0;
        let response = morph_forms_for_scene(&scene, &json!({"sourceGuid":"mesh-guid"}), &mut Budget::new(MAX_SCALARS),
        ).unwrap();
        assert_eq!(response["sources"][0]["morphWeights"][0]["formGuid"], "target-positive");
    }

    #[test]
    fn morph_metadata_lookups_charge_parameter_scans() {
        use crate::authoring_keyforms::{KeyformAxis, KeyformCell, KeyformGrid};
        let mut scene = morph_evaluation_scene();
        for index in 0..100 {
            scene.parameters.push(Parameter {
                guid: format!("unused-{index}"), id: format!("Unused{index}"), name: String::new(),
                minimum: 0.0, maximum: 1.0, default: 0.0, repeat: false,
                parameter_type: "NORMAL".into(),
            });
        }
        let values = scene.parameters.iter().map(|p| (p.guid.as_str(), p.default)).collect();
        let targets = scene.sources[0].morph_targets.as_ref().unwrap();
        assert!(morph_target_weights(&scene, targets, &[], &values, &mut Budget::new(64)).is_err());
        assert!(morph_target_weights(&scene, targets, &[], &values, &mut Budget::new(1000)).is_ok());
        let grid = KeyformGrid {
            axes: vec![KeyformAxis {
                binding_ref: "axis".into(), parameter_guid: scene.parameters[0].guid.clone(),
                parameter_id: Some(scene.parameters[0].id.clone()), keys: vec![0.0],
                interpolation: "LINEAR".into(), extended_interpolation: "LINEAR".into(),
                insert_point_count: 0, extended_interpolation_scale: 1.0,
            }],
            cells: vec![KeyformCell { indices: vec![0], form_guid: "form-guid".into(),
            }],
        };
        assert!(default_form_guid(&scene, &grid, &mut Budget::new(64)).is_err());
        assert_eq!(default_form_guid(&scene, &grid, &mut Budget::new(1000)).unwrap(), "form-guid");
    }

    #[test]
    fn morph_default_base_requires_one_exact_stored_normal_corner() {
        use crate::authoring_keyforms::{KeyformAxis, KeyformCell, KeyformGrid};

        let scene = morph_evaluation_scene();
        let off_key = KeyformGrid {
            axes: vec![KeyformAxis {
                binding_ref: "axis".into(),
                parameter_guid: "parameter-2-guid".into(),
                parameter_id: Some("ParamMorph2".into()),
                keys: vec![-2.0, 2.0],
                interpolation: "LINEAR".into(),
                extended_interpolation: "LINEAR".into(),
                insert_point_count: 0,
                extended_interpolation_scale: 1.0,
            }],
            cells: vec![KeyformCell {
                indices: vec![0],
                form_guid: "form-guid".into(),
            }],
        };
        let error = default_form_guid(&scene, &off_key, &mut Budget::new(MAX_SCALARS))
            .unwrap_err();
        assert!(error.contains("not an exact"), "{error}");

        let ambiguous = KeyformGrid {
            axes: Vec::new(),
            cells: vec![
                KeyformCell {
                    indices: Vec::new(),
                    form_guid: "form-guid".into(),
                },
                KeyformCell {
                    indices: Vec::new(),
                    form_guid: "other-default".into(),
                },
            ],
        };
        let error = default_form_guid(&scene, &ambiguous, &mut Budget::new(MAX_SCALARS))
            .unwrap_err();
        assert!(error.contains("ambiguous"), "{error}");
    }

    #[test]
    fn reads_morph_targets_and_parameter_bound_blend_constraints() {
        let scene = read_xml(
            &morph_fixture(),
            &HashSet::from(["mesh.png"]),
            Budget::new(MAX_SCALARS),
        )
        .unwrap();
        assert_eq!(
            scene["sources"][0]["morphTargets"],
            json!({
                "targets": [{"parameterGuid": "parameter-guid", "parameterId": "ParamMorph",
                    "keyValue": -1.0, "formGuid": "form-guid", "editBaseParameterMap": null}],
                "blendWeightConstraints": [{"morphTargetParameterGuid": "parameter-guid",
                    "morphTargetParameterId": "ParamMorph", "constraintParameterGuid": "parameter-guid",
                    "constraintParameterId": "ParamMorph", "constraintParameterValue": 0.5, "blendWeight": 0.25}]
            })
        );
        assert!(scene["sources"][1]["morphTargets"].is_null());
    }

    #[test]
    fn rejects_morph_owner_form_parameter_count_nonfinite_and_unknown_edit_bases() {
        let original = morph_fixture();
        for xml in [
            original.replace(
                "xs.n=\"owner\" xs.ref=\"mesh\"",
                "xs.n=\"owner\" xs.ref=\"part\"",
            ),
            original.replace(
                "xs.n=\"keyformGuid\" uuid=\"form-guid\"",
                "xs.n=\"keyformGuid\" uuid=\"foreign-form\"",
            ),
            original.replace(
                "xs.n=\"parameterGuid\" uuid=\"parameter-guid\"",
                "xs.n=\"parameterGuid\" uuid=\"unknown-param\"",
            ),
            original.replace(
                "xs.n=\"constraintParameterGuid\" uuid=\"parameter-guid\"",
                "xs.n=\"constraintParameterGuid\" uuid=\"unknown-param\"",
            ),
            original.replace(
                "xs.n=\"_morphTargets\" count=\"1\"",
                "xs.n=\"_morphTargets\" count=\"2\"",
            ),
            original.replace("xs.n=\"blendWeight\">0.25", "xs.n=\"blendWeight\">NaN"),
            original.replace(
                "<null xs.n=\"baseParameterMap\"/>",
                "<hash_map xs.n=\"baseParameterMap\" count=\"0\"/>",
            ),
        ] {
            assert!(
                read_xml(&xml, &HashSet::from(["mesh.png"]), Budget::new(MAX_SCALARS)).is_err()
            );
        }
        let start = original.find("<KeyFormMorphTarget>").unwrap();
        let end = original.find("</KeyFormMorphTarget>").unwrap() + "</KeyFormMorphTarget>".len();
        let mut no_target = original.clone();
        no_target.replace_range(start..end, "");
        let no_target = no_target.replace(
            "xs.n=\"_morphTargets\" count=\"1\"",
            "xs.n=\"_morphTargets\" count=\"0\"",
        );
        let error = read_xml(
            &no_target,
            &HashSet::from(["mesh.png"]),
            Budget::new(MAX_SCALARS),
        )
        .unwrap_err();
        assert!(
            error.contains("without a morph target in its source"),
            "{error}"
        );
    }
}
