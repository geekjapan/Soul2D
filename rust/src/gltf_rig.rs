//! Rig-only import of a humanoid skeleton from glTF 2.0 and VRM (`rig.import_gltf`, issue #243).
//!
//! Only the JSON of a `.gltf`, `.glb` or `.vrm` file is read: node transforms, the VRM humanoid table and what the
//! loss report counts. No buffer, image or external URI is opened. The conventions and the bone table are in
//! `doc/3d-conversion-conventions.md`, the outcomes in `doc/3d-conversion-loss-map.md`.

use serde_json::{json, Value};
use std::array::from_fn;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

/// The size other JSON readers of the session allow (motion3, expressions).
pub(crate) const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;
const MAX_NODES: usize = 65_536;
/// Loss items listed for one feature. The counts are never cut.
const MAX_ITEMS: usize = 256;
/// Humanoid bones without a table row that are kept. Both VRM versions define 55 names.
const MAX_EXTRA_BONES: usize = 256;
pub(crate) const JSON_CHUNK: u32 = 0x4E4F_534A;

pub(crate) const VRM_META_KEY: &str = "io.github.geekjapan.soul2d.VrmMeta";
pub(crate) const VRM_HUMANOID_KEY: &str = "io.github.geekjapan.soul2d.VrmHumanoid";
pub(crate) const GLTF_ASSET_KEY: &str = "io.github.geekjapan.soul2d.GltfAsset";

/// Standard bone, VRM humanoid name (0.x and 1.0), Mixamo-style node name. The character's left is the right of the
/// picture, so a source "left" bone is a `.R` bone.
pub(crate) const HUMANOID: [(&str, &str, &str); 19] = [
    ("Pelvis", "hips", "Hips"),
    ("Spine", "spine", "Spine"),
    ("Chest", "chest", "Spine2"),
    ("Neck", "neck", "Neck"),
    ("Head", "head", "Head"),
    ("Clavicle.R", "leftShoulder", "LeftShoulder"),
    ("UpperArm.R", "leftUpperArm", "LeftArm"),
    ("Forearm.R", "leftLowerArm", "LeftForeArm"),
    ("Hand.R", "leftHand", "LeftHand"),
    ("Clavicle.L", "rightShoulder", "RightShoulder"),
    ("UpperArm.L", "rightUpperArm", "RightArm"),
    ("Forearm.L", "rightLowerArm", "RightForeArm"),
    ("Hand.L", "rightHand", "RightHand"),
    ("Thigh.R", "leftUpperLeg", "LeftUpLeg"),
    ("Shin.R", "leftLowerLeg", "LeftLeg"),
    ("Foot.R", "leftFoot", "LeftFoot"),
    ("Thigh.L", "rightUpperLeg", "RightUpLeg"),
    ("Shin.L", "rightLowerLeg", "RightLeg"),
    ("Foot.L", "rightFoot", "RightFoot"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Convention {
    Gltf2,
    Vrm1,
    Vrm0,
}

impl Convention {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Convention::Gltf2 => "gltf2",
            Convention::Vrm1 => "vrm1",
            Convention::Vrm0 => "vrm0",
        }
    }

    fn format(self) -> &'static str {
        match self {
            Convention::Gltf2 => "glTF 2.0",
            Convention::Vrm1 => "VRM 1.0",
            Convention::Vrm0 => "VRM 0.x",
        }
    }

    /// The signs that take a source point to the rig frame (x screen right, y down, z toward the viewer).
    pub(crate) fn signs(self) -> [f64; 3] {
        match self {
            Convention::Vrm0 => [-1.0, -1.0, -1.0],
            Convention::Gltf2 | Convention::Vrm1 => [1.0, -1.0, 1.0],
        }
    }

    /// Standard bones a file of this kind may leave out. They are placed between their neighbours.
    fn optional(self) -> &'static [&'static str] {
        match self {
            Convention::Vrm0 => &["Clavicle.L", "Clavicle.R"],
            Convention::Gltf2 | Convention::Vrm1 => &["Chest", "Neck", "Clavicle.L", "Clavicle.R"],
        }
    }
}

/// What a file gives a rig-only import.
pub(crate) struct Skeleton {
    pub(crate) convention: Convention,
    /// Rest head and tail of each standard bone, in rig axes and source units.
    pub(crate) rests: BTreeMap<&'static str, ([f64; 3], [f64; 3])>,
    /// VRM humanoid bones without a table row: name and position, in rig axes and source units.
    pub(crate) extra: Vec<(String, [f64; 3])>,
    /// What the file states about rights, kept unchanged: the `extdata` key and the JSON.
    pub(crate) rights: Option<(&'static str, Value)>,
    /// The standard bone of each node that is one.
    pub(crate) joints: BTreeMap<usize, &'static str>,
    report: Report,
}

impl Skeleton {
    /// True when the import approximates or drops something, which needs the caller's `lossy: true`.
    pub(crate) fn lossy(&self) -> bool {
        self.report.lossy()
    }

    /// The loss report of `doc/3d-conversion-loss-map.md`, without the fields the session adds.
    pub(crate) fn report(&self) -> Value {
        let rights = self.rights.as_ref().map(|(_, rights)| (shown(rights), rights));
        // The view of the rights is not all the file states: something was cut, replaced or not shown.
        let mut report = self.report.fields(rights.as_ref().is_some_and(|(view, stated)| view != *stated));
        report["direction"] = json!("gltf-to-soul2d");
        report["source"] = json!({ "format": self.convention.format(), "convention": self.convention.name() });
        report["rights"] = json!(rights.map(|(view, _)| view));
        report
    }
}

/// The items of a loss report (`doc/3d-conversion-loss-map.md`).
#[derive(Default)]
pub(crate) struct Report {
    counts: BTreeMap<&'static str, usize>,
    listed: BTreeMap<&'static str, usize>,
    truncated: BTreeSet<&'static str>,
    items: Vec<Value>,
}

impl Report {
    /// True when something is approximated or dropped.
    pub(crate) fn lossy(&self) -> bool {
        ["approximated", "dropped"].iter().any(|outcome| self.counts.get(outcome).is_some_and(|count| *count > 0))
    }

    /// `counts`, `items` and `truncated` of the report. `rights_cut` names `rights` under `truncated`.
    pub(crate) fn fields(&self, rights_cut: bool) -> Value {
        let count = |outcome: &str| self.counts.get(outcome).copied().unwrap_or(0);
        let mut truncated = self.truncated.clone();
        if rights_cut {
            truncated.insert("rights");
        }
        json!({
            "counts": {
                "converted": count("converted"), "approximated": count("approximated"), "kept": count("kept"),
                "dropped": count("dropped"), "refused": 0,
            },
            "items": self.items,
            "truncated": truncated,
        })
    }

    /// `add` for `times` things of one source that are alike. They are one item, and the counts hold them all.
    pub(crate) fn add_times(
        &mut self,
        times: usize,
        feature: &'static str,
        source: &str,
        outcome: &'static str,
        note: &str,
    ) {
        if times > 0 {
            *self.counts.entry(outcome).or_default() += times - 1;
            self.add(feature, source, outcome, note);
        }
    }

    /// Counts one item. What is not `converted` is also listed, up to `MAX_ITEMS` for one feature.
    pub(crate) fn add(&mut self, feature: &'static str, source: &str, outcome: &'static str, note: &str) {
        *self.counts.entry(outcome).or_default() += 1;
        if outcome == "converted" {
            return;
        }
        let listed = self.listed.entry(feature).or_default();
        if *listed == MAX_ITEMS {
            self.truncated.insert(feature);
            return;
        }
        *listed += 1;
        let key = if outcome == "kept" { "target" } else { "reason" };
        self.items.push(json!({ "feature": feature, "source": label(source, 64), "outcome": outcome, key: note }));
    }
}

/// Text of the file as an answer repeats it: cut, with control characters replaced.
pub(crate) fn label(text: &str, limit: usize) -> String {
    text.chars().take(limit).map(|c| if c.is_control() { '?' } else { c }).collect()
}

/// The rights fields as the report shows them: 64 fields, names cut to 64 characters, text to 1,024 and lists to
/// 16 texts. A number is shown up to 32 characters, and a value of another kind is not shown.
pub(crate) fn shown(rights: &Value) -> Value {
    let field = |value: &Value| match value {
        Value::String(text) => Some(json!(label(text, 1024))),
        Value::Bool(_) => Some(value.clone()),
        // The crate keeps the text of a number, which a file can make as long as it likes.
        Value::Number(number) => (number.to_string().len() <= 32).then(|| value.clone()),
        Value::Array(items) => {
            let texts = items.iter().take(16).filter_map(Value::as_str).map(|text| label(text, 1024));
            Some(json!(texts.collect::<Vec<_>>()))
        }
        _ => None,
    };
    let fields = rights.as_object().into_iter().flatten().take(64);
    Value::Object(fields.filter_map(|(name, value)| Some((label(name, 64), field(value)?))).collect())
}

/// Reads the JSON of a glTF file and nothing after it, so the size of a binary chunk does not matter.
pub(crate) fn read_json(mut file: impl Read) -> Result<Vec<u8>, String> {
    let failed = |error: std::io::Error| format!("could not read glTF file: {error}");
    let mut text = Vec::new();
    file.by_ref().take(20).read_to_end(&mut text).map_err(failed)?;
    if !text.starts_with(b"glTF") {
        file.take((MAX_JSON_BYTES - text.len()) as u64 + 1).read_to_end(&mut text).map_err(failed)?;
        return if text.len() > MAX_JSON_BYTES { Err("glTF JSON is too large".into()) } else { Ok(text) };
    }
    let word =
        |at: usize| text.get(at..at + 4).map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
    let length = match (word(4), word(12), word(16)) {
        (Some(2), Some(length), Some(JSON_CHUNK)) => length as usize,
        _ => return Err("binary glTF must be version 2 and start with a JSON chunk".into()),
    };
    if length > MAX_JSON_BYTES {
        return Err("glTF JSON is too large".into());
    }
    let mut json = Vec::new();
    file.take(length as u64).read_to_end(&mut json).map_err(failed)?;
    if json.len() < length {
        return Err("binary glTF JSON chunk is cut short".into());
    }
    Ok(json)
}

#[derive(Clone, Copy)]
pub(crate) struct Affine {
    linear: [[f64; 3]; 3],
    offset: [f64; 3],
}

impl Affine {
    pub(crate) const IDENTITY: Affine =
        Affine { linear: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], offset: [0.0; 3] };

    /// The transform a column-major 4x4 matrix states with its upper three rows.
    pub(crate) fn from_matrix(m: [f64; 16]) -> Affine {
        Affine { linear: from_fn(|r| from_fn(|c| m[c * 4 + r])), offset: [m[12], m[13], m[14]] }
    }

    pub(crate) fn apply(&self, point: [f64; 3]) -> [f64; 3] {
        from_fn(|r| (0..3).map(|k| self.linear[r][k] * point[k]).sum::<f64>() + self.offset[r])
    }

    /// True when the transform turns a shape into its mirror image, which turns a triangle the other way round.
    pub(crate) fn mirrors(&self) -> bool {
        let [a, b, c] = self.linear;
        let determinant = a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
            + a[2] * (b[0] * c[1] - b[1] * c[0]);
        determinant < 0.0
    }

    /// The largest number of the transform, without its sign.
    pub(crate) fn size(&self) -> f64 {
        let numbers = self.linear.as_flattened().iter().chain(&self.offset);
        numbers.fold(0.0, |most, number| most.max(number.abs()))
    }

    /// True when `other` turns and scales as this transform does, within a ten-thousandth of its largest number,
    /// and puts the origin within a ten-thousandth of `reach` of where this one puts it. Inverse bind matrices are
    /// stored as `f32`, so two that mean the same differ a little.
    pub(crate) fn near(&self, other: &Affine, reach: f64) -> bool {
        let turn = self.linear.as_flattened().iter().fold(0.0f64, |most, number| most.max(number.abs()));
        let close = |a: &[f64], b: &[f64], size: f64| a.iter().zip(b).all(|(a, b)| (a - b).abs() <= 1e-4 * size);
        close(self.linear.as_flattened(), other.linear.as_flattened(), turn)
            && close(&self.offset, &other.offset, reach)
    }

    /// The transform of a child whose local transform is `inner`, in the frame `self` is given in.
    pub(crate) fn then(&self, inner: &Affine) -> Result<Affine, String> {
        let row = |r: usize, column: [f64; 3]| (0..3).map(|k| self.linear[r][k] * column[k]).sum::<f64>();
        let linear: [[f64; 3]; 3] = from_fn(|r| from_fn(|c| row(r, from_fn(|k| inner.linear[k][c]))));
        let offset: [f64; 3] = from_fn(|r| row(r, inner.offset) + self.offset[r]);
        if linear.iter().flatten().chain(&offset).all(|number| number.is_finite()) {
            Ok(Affine { linear, offset })
        } else {
            Err("a node transform is not finite".into())
        }
    }
}

fn numbers<const N: usize>(value: &Value, name: &str) -> Result<[f64; N], String> {
    let refusal = || format!("a node {name} must hold {N} finite numbers");
    let items = value.as_array().filter(|items| items.len() == N).ok_or_else(refusal)?;
    let mut out = [0.0; N];
    for (slot, item) in out.iter_mut().zip(items) {
        *slot = item.as_f64().filter(|number| number.is_finite()).ok_or_else(refusal)?;
    }
    Ok(out)
}

/// The local transform of a node: its `matrix` (column-major), or translation, rotation (x, y, z, w) and scale.
fn local(node: &Value) -> Result<Affine, String> {
    let part = |name: &str| node.get(name).filter(|value| !value.is_null());
    if let Some(matrix) = part("matrix") {
        return Ok(Affine::from_matrix(numbers(matrix, "matrix")?));
    }
    let offset = part("translation").map_or(Ok([0.0; 3]), |value| numbers(value, "translation"))?;
    let scale = part("scale").map_or(Ok([1.0; 3]), |value| numbers(value, "scale"))?;
    let turn: [f64; 4] = part("rotation").map_or(Ok([0.0, 0.0, 0.0, 1.0]), |value| numbers(value, "rotation"))?;
    let length = turn.iter().map(|part| part * part).sum::<f64>().sqrt();
    if !(length > 1e-6 && length.is_finite()) {
        return Err("a node rotation has no length".into());
    }
    let [x, y, z, w] = turn.map(|part| part / length);
    let rotation = [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w)],
        [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w)],
        [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y)],
    ];
    Ok(Affine { linear: from_fn(|r| from_fn(|c| rotation[r][c] * scale[c])), offset })
}

pub(crate) fn index(value: &Value, count: usize) -> Option<usize> {
    value.as_u64().and_then(|index| usize::try_from(index).ok()).filter(|index| *index < count)
}

pub(crate) fn list(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

/// The transform of every node into the frame of the file. The node graph must be a forest: a child that does not
/// exist, a node with two parents and a cycle are refused.
pub(crate) fn world_transforms(nodes: &[Value]) -> Result<Vec<Affine>, String> {
    let mut children = vec![Vec::new(); nodes.len()];
    let mut has_parent = vec![false; nodes.len()];
    for (parent, node) in nodes.iter().enumerate() {
        for child in list(&node["children"]) {
            let child = index(child, nodes.len()).ok_or("a node names a child that does not exist")?;
            if std::mem::replace(&mut has_parent[child], true) {
                return Err("a node has two parents".into());
            }
            children[parent].push(child);
        }
    }
    let mut world = vec![None; nodes.len()];
    let mut stack: Vec<(usize, Affine)> =
        (0..nodes.len()).filter(|node| !has_parent[*node]).map(|node| (node, Affine::IDENTITY)).collect();
    while let Some((node, above)) = stack.pop() {
        let here = above.then(&local(&nodes[node])?)?;
        world[node] = Some(here);
        stack.extend(children[node].iter().map(|child| (*child, here)));
    }
    // A node on a cycle has a parent and is reached from no root.
    world.into_iter().collect::<Option<Vec<_>>>().ok_or_else(|| "the node graph has a cycle".into())
}

/// The name of an item of the file as a report shows it. It is cut here, because a name may be as long as the
/// file and a caller may ask for it in a loop.
pub(crate) fn named(item: &Value, kind: &str, at: usize) -> String {
    item["name"].as_str().map_or_else(|| format!("{kind} {at}"), |name| label(name, 64))
}

/// The humanoid skeleton of the glTF JSON `text` and the loss report of a rig-only import.
pub(crate) fn skeleton(text: &[u8]) -> Result<Skeleton, String> {
    let root: Value = serde_json::from_slice(text).map_err(|error| format!("glTF JSON is not valid: {error}"))?;
    if !root["asset"]["version"].as_str().is_some_and(|version| version.starts_with("2.")) {
        return Err("only glTF 2 is read".into());
    }
    let extensions = &root["extensions"];
    let convention = match (extensions.get("VRM"), extensions.get("VRMC_vrm")) {
        (Some(_), Some(_)) => return Err("the file has both the VRM 0.x and the VRM 1.0 extension".into()),
        (Some(_), None) => Convention::Vrm0,
        (None, Some(_)) => Convention::Vrm1,
        (None, None) => Convention::Gltf2,
    };
    // ponytail: a required extension is accepted by name when its family cannot move a node. Widen the list when a
    // real file needs it.
    let families = ["VRMC_", "KHR_materials_", "KHR_texture_"];
    let harmless = |name: &str| name == "VRM" || families.iter().any(|family| name.starts_with(family));
    let required = list(&root["extensionsRequired"]).iter().map(|name| name.as_str().unwrap_or("?"));
    if let Some(name) = required.into_iter().find(|name| !harmless(name)) {
        return Err(format!("the file requires the extension {}, which is not read", label(name, 64)));
    }
    let nodes = match &root["nodes"] {
        Value::Array(nodes) if nodes.len() <= MAX_NODES => nodes,
        Value::Array(_) => return Err(format!("the file has more than {MAX_NODES} nodes")),
        _ => return Err("the file has no nodes".into()),
    };
    let world = world_transforms(nodes)?;
    let signs = convention.signs();
    let position = |node: usize| -> [f64; 3] { from_fn(|axis| signs[axis] * world[node].offset[axis]) };

    // Humanoid name and node, as the file states them.
    let mut stated: Vec<(String, usize)> = Vec::new();
    let node_of =
        |bone: &Value| index(&bone["node"], nodes.len()).ok_or("a humanoid bone names a node that does not exist");
    match convention {
        Convention::Vrm1 => {
            for (name, bone) in extensions["VRMC_vrm"]["humanoid"]["humanBones"].as_object().into_iter().flatten() {
                stated.push((name.clone(), node_of(bone)?));
            }
        }
        Convention::Vrm0 => {
            for bone in list(&extensions["VRM"]["humanoid"]["humanBones"]) {
                let name = bone["bone"].as_str().ok_or("a humanoid bone has no name")?;
                stated.push((name.to_owned(), node_of(bone)?));
            }
        }
        Convention::Gltf2 => {
            // A file `export.gltf` wrote names its nodes after the standard bones. In a file of another writer such
            // a name says nothing: its `.L` may be the left of the character.
            let own = root["asset"]["generator"].as_str().is_some_and(|generator| generator.starts_with("Soul2D "));
            // A node that draws a mesh is not a bone, whatever its name.
            for (node, item) in nodes.iter().enumerate().filter(|(_, item)| item.get("mesh").is_none()) {
                // A namespace such as `mixamorig:` is not part of the name.
                let name = item["name"].as_str().and_then(|name| name.rsplit(':').next()).unwrap_or("");
                if let Some(row) = HUMANOID.iter().find(|row| row.2 == name || (own && row.0 == name)) {
                    stated.push((row.1.to_owned(), node));
                }
            }
        }
    }
    let mut heads = BTreeMap::<&'static str, [f64; 3]>::new();
    let mut extra = Vec::new();
    let mut used = BTreeSet::new();
    let mut standard = BTreeMap::new();
    let mut report = Report::default();
    // The name a row has in this kind of file.
    let stated_as =
        |row: &(&str, &'static str, &'static str)| if convention == Convention::Gltf2 { row.2 } else { row.1 };
    for (name, node) in stated {
        if !used.insert(node) {
            return Err("two humanoid bones name the same node".into());
        }
        match HUMANOID.iter().find(|row| row.1 == name) {
            Some(row) => {
                if heads.insert(row.0, position(node)).is_some() {
                    return Err(format!("the humanoid bone {} is stated twice", stated_as(row)));
                }
                standard.insert(node, row.0);
                report.add("skeleton.bone", row.1, "converted", "");
            }
            None if extra.len() == MAX_EXTRA_BONES => return Err("the file states too many humanoid bones".into()),
            None => {
                // The kept table is keyed by the shortened name.
                let name = label(&name, 64);
                if extra.iter().any(|(kept, _)| *kept == name) {
                    return Err(format!("the humanoid bone {name} is stated twice"));
                }
                report.add("skeleton.bone", &name, "kept", &format!("extdata:{VRM_HUMANOID_KEY}"));
                extra.push((name, position(node)));
            }
        }
    }
    for row in &HUMANOID {
        if !heads.contains_key(row.0) && !convention.optional().contains(&row.0) {
            return Err(format!("the file lacks the humanoid bone {}", stated_as(row)));
        }
    }
    let middle = |a: [f64; 3], b: [f64; 3]| -> [f64; 3] { from_fn(|axis| (a[axis] + b[axis]) / 2.0) };
    let between = [
        ("Chest", "Spine", if heads.contains_key("Neck") { "Neck" } else { "Head" }),
        ("Neck", "Chest", "Head"),
        ("Clavicle.L", "Neck", "UpperArm.L"),
        ("Clavicle.R", "Neck", "UpperArm.R"),
    ];
    for (bone, a, b) in between {
        if !heads.contains_key(bone) {
            heads.insert(bone, middle(heads[a], heads[b]));
            let reason = "the file has no such bone; it is placed between its neighbours";
            report.add("skeleton.bone", bone, "approximated", reason);
        }
    }

    // A bone ends where the next bone of its chain starts. A bone that ends a chain continues the bone before it
    // with the proportion of the standard skeleton, and a foot points toward the viewer.
    let rests = HUMANOID
        .iter()
        .map(|row| {
            let (bone, head) = (row.0, heads[row.0]);
            let (base, side) = bone.rfind('.').map_or((bone, ""), |at| bone.split_at(at));
            let sided = |name: &str| heads[format!("{name}{side}").as_str()];
            let onward = |from: [f64; 3], ratio: f64| -> [f64; 3] {
                from_fn(|axis| head[axis] + (head[axis] - from[axis]) * ratio)
            };
            let tail = match base {
                "Pelvis" => heads["Spine"],
                "Spine" => heads["Chest"],
                "Chest" => heads["Neck"],
                "Neck" => heads["Head"],
                "Head" => onward(heads["Neck"], 10.0 / 6.0),
                "Clavicle" => sided("UpperArm"),
                "UpperArm" => sided("Forearm"),
                "Forearm" => sided("Hand"),
                "Hand" => onward(sided("Forearm"), 0.377),
                "Thigh" => sided("Shin"),
                "Shin" => sided("Foot"),
                _ => {
                    let shin = sided("Shin");
                    let length = (0..3).map(|axis| (head[axis] - shin[axis]).powi(2)).sum::<f64>().sqrt();
                    [head[0], head[1], head[2] + 0.4 * length]
                }
            };
            // A point between two bones, or past one, can leave the range although every node is inside it.
            if !head.iter().chain(&tail).all(|axis| axis.is_finite()) {
                return Err(format!("the bone {bone} is outside the finite range"));
            }
            if head == tail {
                return Err(format!("the bone {bone} would have no length"));
            }
            Ok((bone, (head, tail)))
        })
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    report.add("skeleton.restPose", "", "approximated", "rest orientations are the rig's own");

    let joints = list(&root["skins"]).iter().flat_map(|skin| list(&skin["joints"]));
    let joints: BTreeSet<usize> = joints.filter_map(|joint| index(joint, nodes.len())).collect();
    for joint in joints.difference(&used) {
        let name = named(&nodes[*joint], "node", *joint);
        report.add("skeleton.bone", &name, "dropped", "not a humanoid bone with a table row");
    }
    let mut drop_each = |feature: &'static str, kind: &str, items: &Value, reason: &str| {
        for (at, item) in list(items).iter().enumerate() {
            report.add(feature, &named(item, kind, at), "dropped", reason);
        }
    };
    drop_each("mesh.geometry", "mesh", &root["meshes"], "a rig-only import reads no mesh");
    drop_each("mesh.skinWeights", "skin", &root["skins"], "a rig-only import reads no mesh");
    drop_each("mesh.texture", "image", &root["images"], "a rig-only import reads no mesh");
    drop_each("material", "material", &root["materials"], "a rig-only import reads no mesh");
    drop_each("animation", "animation", &root["animations"], "motion is not imported");
    drop_each("camera", "camera", &root["cameras"], "Soul2D has no 3D camera");
    let (no_physics, no_morphs) = ("not turned into SimplePhysics", "it binds morph targets, which are not imported");
    drop_each("physics.springBone", "spring", &extensions["VRMC_springBone"]["springs"], no_physics);
    drop_each("physics.springBone", "spring", &extensions["VRM"]["secondaryAnimation"]["boneGroups"], no_physics);
    drop_each("expression", "expression", &extensions["VRM"]["blendShapeMaster"]["blendShapeGroups"], no_morphs);
    for (at, mesh) in list(&root["meshes"]).iter().enumerate() {
        for _ in list(&mesh["primitives"][0]["targets"]) {
            report.add("morph.target", &named(mesh, "mesh", at), "dropped", "a rig-only import reads no mesh");
        }
    }
    for group in ["preset", "custom"] {
        for (name, _) in extensions["VRMC_vrm"]["expressions"][group].as_object().into_iter().flatten() {
            report.add("expression", name, "dropped", no_morphs);
        }
    }
    for name in list(&root["extensionsUsed"]).iter().filter_map(Value::as_str) {
        if !["VRM", "VRMC_vrm", "VRMC_springBone"].contains(&name) {
            report.add("extension", name, "dropped", "not read");
        }
    }

    let rights = match convention {
        Convention::Vrm1 => {
            let meta = &extensions["VRMC_vrm"]["meta"];
            // VRM 1.0 prohibits modification unless the file allows it with one of these two values.
            if !matches!(meta["modification"].as_str(), Some("allowModification" | "allowModificationRedistribution")) {
                return Err("the VRM meta does not allow modification, so the model is not converted".into());
            }
            Some((VRM_META_KEY, meta.clone()))
        }
        Convention::Vrm0 => {
            let meta = &extensions["VRM"]["meta"];
            if matches!(meta["licenseName"].as_str(), Some("Redistribution_Prohibited" | "CC_BY_ND" | "CC_BY_NC_ND")) {
                return Err("the VRM licence allows no derived work, so the model is not converted".into());
            }
            Some((VRM_META_KEY, if meta.is_object() { meta.clone() } else { json!({}) }))
        }
        Convention::Gltf2 => root["asset"]["copyright"].is_string().then(|| (GLTF_ASSET_KEY, root["asset"].clone())),
    };
    if let Some((key, _)) = rights {
        report.add("rights", "", "kept", &format!("extdata:{key}"));
    }
    Ok(Skeleton { convention, rests, extra, rights, joints: standard, report })
}
