use crate::authoring_graph::Graph;
use crate::authoring_values::Budget;
use crate::caff::CaffDocument;
use roxmltree::{Document, Node, ParsingOptions};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};

pub(crate) fn inspect(container: &CaffDocument) -> Result<Value, String> {
    let xml = container.main_xml();
    let text = std::str::from_utf8(&xml)
        .map_err(|error| format!("authoring XML is not UTF-8: {error}"))?;
    let mut result = inspect_xml(text)?;
    result["container"] = json!({"format": "CAFF", "profile": 45,
        "entries": container.entries().iter().map(|entry| json!({
            "name": entry.name, "alias": entry.alias, "bytes": entry.size,
            "flags": entry.flags,
        })).collect::<Vec<_>>(), "xmlBytes": xml.len()});
    Ok(result)
}

/// The display information Cubism writes beside a model as `.cdi3.json`: IDs and names of parameters, parameter
/// groups and Parts, and the parameter pairs shown as one two-axis control.
///
/// The order is the one the vendor file has: parameters and groups as a walk of the parameter group tree meets them,
/// Parts level by level from the root Part. A parameter with `combined` set pairs with the next member of its group.
pub(crate) fn display_info(container: &CaffDocument) -> Result<Value, String> {
    let xml = container.main_xml();
    let text = std::str::from_utf8(&xml)
        .map_err(|error| format!("authoring XML is not UTF-8: {error}"))?;
    display_info_xml(text)
}

fn index_by_guid<'d, 'i>(
    graph: &Graph<'d, 'i>,
    nodes: &[Node<'d, 'i>],
    kind: &str,
) -> Result<HashMap<&'d str, usize>, String> {
    let mut map = HashMap::new();
    for (index, node) in nodes.iter().enumerate() {
        let guid = graph.guid(*node)?.ok_or_else(|| format!("authoring {kind} lacks a GUID"))?;
        if map.insert(guid, index).is_some() {
            return Err(format!("duplicate authoring {kind} GUID"));
        }
    }
    Ok(map)
}

fn display_info_xml(text: &str) -> Result<Value, String> {
    use crate::authoring_values::{bool_field, guid_field, list, required_field, text_field};
    let document = Document::parse_with_options(
        text,
        ParsingOptions { allow_dtd: false, nodes_limit: 1_000_000, ..Default::default() },
    )
    .map_err(|error| format!("invalid authoring XML: {error}"))?;
    let graph = Graph::new(&document)?;
    let model = graph.model()?.ok_or("authoring source contains no model")?;
    let mut budget = Budget::new(5_000_000);
    let id = |node, kind: &str| {
        graph.field(node, "id")?
            .and_then(|id| id.attribute("idstr"))
            .filter(|id| !id.is_empty())
            .ok_or_else(|| format!("authoring {kind} lacks an ID"))
    };

    let parameters = graph.source_list(model, "parameterSourceSet")?;
    let groups = list(&graph, required_field(&graph, required_field(&graph, model, "parameterGroupSet")?, "_groups")?,
        &mut budget)?;
    budget.consume(parameters.len())?;
    let parameter_index = index_by_guid(&graph, &parameters, "parameter")?;
    let group_index = index_by_guid(&graph, &groups, "parameter group")?;
    let mut roots = Vec::new();
    for (index, group) in groups.iter().enumerate() {
        if guid_field(&graph, *group, "parentGroupGuid")?.is_none() {
            roots.push(index);
        }
    }
    let [root] = roots[..] else {
        return Err("authoring parameter groups must have exactly one root".into());
    };
    // A member of the root group has no group ID in the vendor file.
    let group_id = |node, listed_in: usize| -> Result<&str, String> {
        if guid_field(&graph, node, "parentGroupGuid")?.and_then(|guid| group_index.get(guid)) != Some(&listed_in) {
            return Err("authoring parameter group member names another parent than the group listing it".into());
        }
        if listed_in == root { Ok("") } else { id(groups[listed_in], "parameter group") }
    };

    let (mut parameter_rows, mut group_rows, mut pairs) = (Vec::new(), Vec::new(), Vec::new());
    let mut pending = None;
    let mut seen = HashSet::new();
    let top = list(&graph, required_field(&graph, groups[root], "_childGuids")?, &mut budget)?;
    let mut stack = vec![(root, top.into_iter())];
    while let Some((listed_in, members)) = stack.last_mut() {
        let listed_in = *listed_in;
        // No sample pairs a parameter with anything but the next parameter of its own group.
        let member = members.next().filter(|member| pending.is_none() || member.has_tag_name("CParameterGuid"));
        let Some(member) = member else {
            if pending.is_some() {
                return Err("authoring combined parameter has no partner".into());
            }
            stack.pop();
            continue;
        };
        let guid = member.attribute("uuid").ok_or("authoring parameter group member lacks uuid")?;
        if !seen.insert(guid) {
            return Err("authoring parameter group tree names a member twice".into());
        }
        if let Some(index) = member.has_tag_name("CParameterGuid").then(|| parameter_index.get(guid)).flatten() {
            let parameter = parameters[*index];
            let parameter_id = budget.text(id(parameter, "parameter")?)?;
            if let Some(first) = pending.take() {
                pairs.push(json!([first, parameter_id]));
            }
            if bool_field(&graph, parameter, "combined", false)? {
                if pairs.last().is_some_and(|pair| pair[1] == parameter_id.as_str()) {
                    return Err("authoring combined parameters overlap".into());
                }
                pending = Some(parameter_id.clone());
            }
            parameter_rows.push(json!({"Id": parameter_id, "GroupId": budget.text(group_id(parameter, listed_in)?)?,
                "Name": budget.text(text_field(&graph, parameter, "name")?.unwrap_or(""))?}));
        } else if let Some(index) = member.has_tag_name("CParameterGroupGuid").then(|| group_index.get(guid)).flatten() {
            let group = groups[*index];
            group_rows.push(json!({"Id": budget.text(id(group, "parameter group")?)?,
                "GroupId": budget.text(group_id(group, listed_in)?)?,
                "Name": budget.text(text_field(&graph, group, "name")?.unwrap_or(""))?}));
            stack.push((*index, list(&graph, required_field(&graph, group, "_childGuids")?, &mut budget)?.into_iter()));
        } else {
            return Err("authoring parameter group holds an unknown member".into());
        }
    }
    if parameter_rows.len() != parameters.len() || group_rows.len() + 1 != groups.len() {
        return Err("authoring parameter or group is outside the parameter group tree".into());
    }

    let parts = graph.source_list(model, "partSourceSet")?;
    budget.consume(parts.len())?;
    let part_index = index_by_guid(&graph, &parts, "Part")?;
    let root_part = graph.guid(required_field(&graph, model, "rootPart")?)?
        .and_then(|guid| part_index.get(guid))
        .ok_or("authoring rootPart is absent from the part source set")?;
    let mut part_rows = Vec::new();
    let mut queue = VecDeque::from([*root_part]);
    let mut seen = HashSet::from([*root_part]);
    while let Some(part) = queue.pop_front() {
        for child in list(&graph, required_field(&graph, parts[part], "_childGuids")?, &mut budget)? {
            if !child.has_tag_name("CPartGuid") {
                continue;
            }
            let index = *child.attribute("uuid").and_then(|guid| part_index.get(guid))
                .ok_or("authoring Part names a child Part outside the part source set")?;
            if !seen.insert(index) {
                return Err("authoring Part tree names a Part twice".into());
            }
            part_rows.push(json!({"Id": budget.text(id(parts[index], "Part")?)?,
                "Name": budget.text(text_field(&graph, parts[index], "localName")?.unwrap_or(""))?}));
            queue.push_back(index);
        }
    }
    if part_rows.len() + 1 != parts.len() {
        return Err("authoring Part is outside the Part tree".into());
    }

    let mut result = json!({"Version": 3, "Parameters": parameter_rows, "ParameterGroups": group_rows, "Parts": part_rows});
    // The vendor file has no such key when no parameter is combined.
    if !pairs.is_empty() {
        result["CombinedParameters"] = json!(pairs);
    }
    Ok(result)
}

/// The ArtMesh user data Cubism writes beside a model as `.userdata3.json`, in source order. An ArtMesh without user
/// data has no row. `TotalUserDataSize` counts the UTF-8 bytes of the texts, as in the vendor sample files (they hold
/// ASCII only, so a byte count and a character count agree there).
pub(crate) fn user_data(container: &CaffDocument) -> Result<Value, String> {
    let scene = crate::authoring_scene::read(container)?;
    let rows: Vec<Value> = scene["sources"].as_array().into_iter().flatten()
        .filter_map(|source| Some(json!({"Target": "ArtMesh", "Id": source["id"], "Value": source["userData"].as_str()?})))
        .collect();
    let size: usize = rows.iter().map(|row| row["Value"].as_str().map_or(0, str::len)).sum();
    Ok(json!({"Version": 3, "Meta": {"UserDataCount": rows.len(), "TotalUserDataSize": size}, "UserData": rows}))
}

/// The physics settings Cubism writes beside a model as `.physics3.json`.
///
/// Settings keep the order of the CMO3 and are numbered `PhysicsSetting1..N`, as the vendor file numbers them whatever
/// the IDs in the CMO3 are. Gravity and wind are the one value every sampled vendor file has; the CMO3 holds neither.
pub(crate) fn physics(container: &CaffDocument) -> Result<Value, String> {
    let xml = container.main_xml();
    let text = std::str::from_utf8(&xml)
        .map_err(|error| format!("authoring XML is not UTF-8: {error}"))?;
    physics_xml(text)
}

/// Three decimals, and an integer when nothing follows the point: both are what the sampled vendor files hold.
fn rounded(value: f64) -> Result<Value, String> {
    let value = (value * 1000.0).round() / 1000.0;
    if value.abs() >= 1e15 {
        return Err("authoring physics number is out of range".into());
    }
    Ok(if value.fract() == 0.0 { json!(value as i64) } else { json!(value) })
}

fn physics_xml(text: &str) -> Result<Value, String> {
    use crate::authoring_values::{bool_field, guid_field, list, number_field, required_field, text_field};
    let document = Document::parse_with_options(
        text,
        ParsingOptions { allow_dtd: false, nodes_limit: 1_000_000, ..Default::default() },
    )
    .map_err(|error| format!("invalid authoring XML: {error}"))?;
    let graph = Graph::new(&document)?;
    let model = graph.model()?.ok_or("authoring source contains no model")?;
    let mut budget = Budget::new(5_000_000);
    let parameters = graph.source_list(model, "parameterSourceSet")?;
    budget.consume(parameters.len())?;
    let parameter_index = index_by_guid(&graph, &parameters, "parameter")?;
    // The caller charges the text budget: one long ID could otherwise be copied once for every input and output.
    let parameter = |node, field: &str| -> Result<&str, String> {
        let index = guid_field(&graph, node, field)?
            .and_then(|guid| parameter_index.get(guid))
            .ok_or("authoring physics names a parameter outside the parameter source set")?;
        graph.field(parameters[*index], "id")?
            .and_then(|id| id.attribute("idstr"))
            .filter(|id| !id.is_empty())
            .ok_or_else(|| "authoring parameter lacks an ID".to_owned())
    };
    let number = |node, name: &str| number_field(&graph, node, name, None).and_then(rounded);
    let kind = |node, known: &[(&str, &'static str)]| -> Result<&'static str, String> {
        let token = graph.field(node, "type")?.and_then(|kind| kind.attribute("v")).unwrap_or("");
        known.iter().find(|(source, _)| *source == token).map(|(_, written)| *written)
            .ok_or_else(|| "unsupported authoring physics type".to_owned())
    };

    let set = required_field(&graph, model, "physicsSettingsSourceSet")?;
    let sources = list(&graph, required_field(&graph, set, "_sourceCubismPhysics")?, &mut budget)?;
    if sources.is_empty() {
        return Err("authoring model has no physics settings".into());
    }
    // The limits of the physics reader of this crate. A list may name one node many times, so they also bound the work.
    if sources.len() > 256 {
        return Err("authoring physics exceeds 256 settings".into());
    }
    let (mut settings, mut dictionary) = (Vec::new(), Vec::new());
    let (mut input_count, mut output_count, mut vertex_count) = (0, 0, 0);
    for (index, source) in sources.iter().enumerate() {
        let id = format!("PhysicsSetting{}", index + 1);
        let mut vertices = Vec::new();
        for vertex in list(&graph, required_field(&graph, *source, "vertices")?, &mut budget)? {
            let position = required_field(&graph, vertex, "position")?;
            vertices.push(json!({"Position": {"X": number(position, "x")?, "Y": number(position, "y")?},
                "Mobility": number(vertex, "mobility")?, "Delay": number(vertex, "delay")?,
                "Acceleration": number(vertex, "acceleration")?, "Radius": number(vertex, "radius")?}));
        }
        let mut inputs = Vec::new();
        for input in list(&graph, required_field(&graph, *source, "inputs")?, &mut budget)? {
            let source = json!({"Target": "Parameter", "Id": budget.text(parameter(input, "source")?)?});
            let token = graph.field(input, "type")?.and_then(|kind| kind.attribute("v")).unwrap_or("");
            let input_type = crate::authoring_physics::input_type_from_token(token)
                .ok_or("unsupported authoring physics type")?;
            inputs.push(json!({"Source": source, "Weight": number(input, "weight")?,
                "Type": input_type,
                "Reflect": bool_field(&graph, input, "isReverse", false)?}));
        }
        let mut outputs = Vec::new();
        for output in list(&graph, required_field(&graph, *source, "outputs")?, &mut budget)? {
            let vertex = text_field(&graph, output, "vertexIndex")?
                .and_then(|text| text.trim().parse::<usize>().ok())
                .filter(|vertex| (1..vertices.len()).contains(vertex))
                .ok_or("authoring physics output names no vertex of its setting")?;
            let destination = json!({"Target": "Parameter", "Id": budget.text(parameter(output, "destination")?)?});
            outputs.push(json!({"Destination": destination, "VertexIndex": vertex,
                "Scale": number(output, "angleScale")?, "Weight": number(output, "weight")?,
                "Type": kind(output, &[("SRC_TO_G_ANGLE", "Angle")])?,
                "Reflect": bool_field(&graph, output, "isReverse", false)?}));
        }
        // The physics reader of this crate asks for both, and no sample setting lacks either.
        if inputs.is_empty() || outputs.is_empty() {
            return Err("authoring physics setting lacks an input or an output".into());
        }
        let range = |name: &str| -> Result<Value, String> {
            Ok(json!({"Minimum": number(*source, &format!("normalized{name}ValueMin"))?,
                "Default": number(*source, &format!("normalized{name}DefaultValue"))?,
                "Maximum": number(*source, &format!("normalized{name}ValueMax"))?}))
        };
        input_count += inputs.len();
        output_count += outputs.len();
        vertex_count += vertices.len();
        if input_count.max(output_count).max(vertex_count) > 16384 {
            return Err("authoring physics exceeds 16384 inputs, outputs or vertices".into());
        }
        dictionary.push(json!({"Id": id, "Name": budget.text(text_field(&graph, *source, "name")?.unwrap_or(""))?}));
        settings.push(json!({"Id": id, "Input": inputs, "Output": outputs, "Vertices": vertices,
            "Normalization": {"Position": range("Position")?, "Angle": range("Angle")?}}));
    }
    let mut meta = json!({"PhysicsSettingCount": settings.len(), "TotalInputCount": input_count,
        "TotalOutputCount": output_count, "VertexCount": vertex_count,
        "EffectiveForces": {"Gravity": {"X": 0, "Y": -1}, "Wind": {"X": 0, "Y": 0}},
        "PhysicsDictionary": dictionary});
    // The vendor file has the key only when the CMO3 sets a rate.
    if let Some(rate) = graph.field(set, "settingFPS")?.filter(|rate| !rate.has_tag_name("null")) {
        let rate = rate.text().unwrap_or("").trim().parse::<u32>().map_err(|_| "invalid authoring physics rate")?;
        meta["Fps"] = json!(rate);
    }
    Ok(json!({"Version": 3, "Meta": meta, "PhysicsSettings": settings}))
}

fn number<'doc, 'input>(
    graph: &Graph<'doc, 'input>,
    node: Node<'doc, 'input>,
    name: &str,
) -> Result<f64, String> {
    let value = graph
        .field(node, name)?
        .and_then(|field| field.text())
        .ok_or_else(|| format!("authoring parameter lacks {name}"))?
        .parse::<f64>()
        .map_err(|_| format!("invalid authoring parameter {name}"))?;
    if !value.is_finite() {
        return Err(format!("non-finite authoring parameter {name}"));
    }
    Ok(value)
}

fn inspect_xml(text: &str) -> Result<Value, String> {
    inspect_xml_with_budget(text, Budget::new(5_000_000))
}

fn inspect_xml_with_budget(text: &str, mut budget: Budget) -> Result<Value, String> {
    let document = Document::parse_with_options(
        text,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 1_000_000,
            ..Default::default()
        },
    )
    .map_err(|error| format!("invalid authoring XML: {error}"))?;
    let root = document.root_element();
    if !root.has_tag_name("root") {
        return Err("authoring XML root must be root".into());
    }
    let graph = Graph::new(&document)?;
    let model = if let Some(model) = graph.model()? {
        let mut ids = HashSet::new();
        let mut guids = HashSet::new();
        let mut parameters = Vec::new();
        for parameter in graph.source_list(model, "parameterSourceSet")? {
            if !parameter.has_tag_name("CParameterSource") {
                return Err("authoring parameterSourceSet contains a non-parameter source".into());
            }
            if let Some(guid) = graph.guid(parameter)? {
                if !guids.insert(guid) {
                    return Err(format!("duplicate authoring parameter GUID {guid:?}"));
                }
            }
            let id = graph
                .field(parameter, "id")?
                .and_then(|node| node.attribute("idstr"))
                .filter(|id| !id.is_empty())
                .ok_or("authoring parameter lacks an ID")?;
            if !ids.insert(id) {
                return Err(format!("duplicate authoring parameter ID {id:?}"));
            }
            let minimum = number(&graph, parameter, "minValue")?;
            let maximum = number(&graph, parameter, "maxValue")?;
            let default = number(&graph, parameter, "defaultValue")?;
            if minimum > default || default > maximum {
                return Err(format!("invalid authoring parameter range for {id:?}"));
            }
            let name = budget.text(
                graph
                    .field(parameter, "name")?
                    .and_then(|node| node.text())
                    .unwrap_or(""),
            )?;
            parameters.push(
                json!({"id": budget.text(id)?, "name": name, "minimum": minimum,
                "maximum": maximum, "default": default}),
            );
        }
        let canvas = if let Some(canvas) = graph.field(model, "canvas")? {
            Some(json!({
                "width": graph.field(canvas, "pixelWidth")?.and_then(|node| node.text()).map(|s| budget.text(s)).transpose()?,
                "height": graph.field(canvas, "pixelHeight")?.and_then(|node| node.text()).map(|s| budget.text(s)).transpose()?,
            }))
        } else {
            None
        };
        Some(
            json!({"name": budget.text(graph.field(model, "name")?.and_then(|node| node.text()).unwrap_or(""))?,
            "canvas": canvas, "parameters": parameters}),
        )
    } else {
        None
    };
    let definitions = |tag: &str| {
        root.descendants()
            .filter(|node| {
                node.has_tag_name(tag) && node.children().any(|child| child.is_element())
            })
            .count()
    };
    Ok(
        json!({"fileFormatVersion": root.attribute("fileFormatVersion").map(|s| budget.text(s)).transpose()?, "model": model,
        "definitions": {"artMeshes": definitions("CArtMeshSource"),
            "warpDeformers": definitions("CWarpDeformerSource"),
            "rotationDeformers": definitions("CRotationDeformerSource"),
            "glues": definitions("CGlueSource"),
            "artPaths": definitions("CArtPathSource")}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(parameter: &str) -> String {
        format!("<root fileFormatVersion='401000004'><main><CModelSource><s xs.n='name'>モデル</s><CParameterSourceSet xs.n='parameterSourceSet'><carray_list xs.n='_sources'>{parameter}</carray_list></CParameterSourceSet></CModelSource></main></root>")
    }

    const PARAMETER: &str = "<CParameterSource><CParameterId xs.n='id' idstr='ParamAngleX'/><s xs.n='name'>角度 X</s><f xs.n='minValue'>-30</f><f xs.n='maxValue'>30</f><f xs.n='defaultValue'>0</f></CParameterSource>";

    #[test]
    fn reads_actual_authoring_fields_without_using_display_info_as_ranges() {
        let result = inspect_xml(&model(PARAMETER)).unwrap();
        assert_eq!(
            result["model"]["parameters"][0],
            json!({"id":"ParamAngleX", "name":"角度 X", "minimum":-30.0, "maximum":30.0, "default":0.0})
        );
    }

    #[test]
    fn rejects_duplicate_ids_bad_ranges_nonfinite_values_and_entities() {
        assert!(inspect_xml(&model(&PARAMETER.repeat(2))).is_err());
        assert!(inspect_xml(&model(&PARAMETER.replace(">0<", ">31<"))).is_err());
        assert!(inspect_xml(&model(&PARAMETER.replace(">0<", ">NaN<"))).is_err());
        assert!(inspect_xml("<!DOCTYPE root [<!ENTITY x 'text'>]><root>&x;</root>").is_err());
    }

    #[test]
    fn reads_referenced_parameters_in_source_order_and_rejects_broken_links() {
        let shared = format!(
            "<root><CModelSource><CParameterSourceSet xs.n='parameterSourceSet'><carray_list xs.n='_sources'><CParameterSource xs.ref='second'/><CParameterSource xs.ref='parameter'/></carray_list></CParameterSourceSet></CModelSource>{}{}</root>",
            PARAMETER.replacen("<CParameterSource>", "<CParameterSource xs.id='parameter'>", 1),
            PARAMETER.replacen("<CParameterSource>", "<CParameterSource xs.id='second'>", 1).replace("ParamAngleX", "ParamAngleY")
        );
        let result = inspect_xml(&shared).unwrap();
        assert_eq!(result["model"]["parameters"][0]["id"], "ParamAngleY");
        assert_eq!(result["model"]["parameters"][1]["id"], "ParamAngleX");
        assert_eq!(result["model"]["parameters"].as_array().unwrap().len(), 2);
        assert!(inspect_xml(&shared.replace("xs.ref='parameter'", "xs.ref='missing'")).is_err());
        assert!(inspect_xml(&shared.replace("xs.n='_sources'", "xs.n='wrong'")).is_err());
    }

    #[test]
    fn inspection_bounds_repeated_shared_parameter_names() {
        let parameter = PARAMETER.replace(
            "<s xs.n='name'>角度 X</s>",
            "<s xs.n='name' xs.ref='shared-name'/>",
        );
        let xml = model(&format!(
            "{}{}",
            parameter,
            parameter.replace("ParamAngleX", "ParamAngleY")
        ))
        .replace(
            "</root>",
            &format!("<s xs.id='shared-name'>{}</s></root>", "a".repeat(1024)),
        );
        assert!(inspect_xml_with_budget(&xml, Budget::new(100).with_text_limit(4096)).is_ok());
        assert!(
            inspect_xml_with_budget(&xml, Budget::new(100).with_text_limit(1500))
                .unwrap_err()
                .contains("copied-text byte budget")
        );
    }

    #[test]
    fn inspection_counts_glue_affecter_and_art_path_definitions() {
        let result = inspect_xml(
            "<root><CModelSource><s xs.n='name'>Model</s></CModelSource>\
             <CGlueSource xs.id='glue'><CAffecterGuid xs.n='guid' uuid='glue-guid'/></CGlueSource>\
             <CArtPathSource xs.id='path'><i xs.n='divNum'>20</i></CArtPathSource><CArtPathSource xs.ref='path'/></root>",
        )
        .unwrap();
        assert_eq!(result["definitions"]["glues"], 1);
        assert_eq!(result["definitions"]["artPaths"], 1);
    }

    /// Parameter group tree: root holds A, group One, D; One holds B (combined), C, group Two; Two holds E.
    /// Part tree: root holds Left, Right; Left holds Deep; Right holds Far. The source sets list both in another order.
    fn display_fixture() -> String {
        let parameter = |guid: &str, id: &str, parent: &str, combined: bool| format!(
            "<CParameterSource><CParameterGuid xs.n='guid' uuid='{guid}'/><CParameterId xs.n='id' idstr='{id}'/>\
             <s xs.n='name'>Name {id}</s><b xs.n='combined'>{combined}</b>\
             <CParameterGroupGuid xs.n='parentGroupGuid' uuid='{parent}'/></CParameterSource>");
        let group = |guid: &str, id: &str, parent: &str, members: &str| format!(
            "<CParameterGroup><CParameterGroupGuid xs.n='guid' uuid='{guid}'/><CParameterGroupId xs.n='id' idstr='{id}'/>\
             <s xs.n='name'>Name {id}</s>{parent}<carray_list xs.n='_childGuids'>{members}</carray_list></CParameterGroup>");
        let inside = |guid: &str| format!("<CParameterGroupGuid xs.n='parentGroupGuid' uuid='{guid}'/>");
        let part = |guid: &str, id: &str, children: &str| format!(
            "<CPartSource xs.id='{guid}'><CPartGuid xs.n='guid' uuid='{guid}'/><CPartId xs.n='id' idstr='{id}'/>\
             <s xs.n='localName'>Name {id}</s><carray_list xs.n='_childGuids'>{children}</carray_list></CPartSource>");
        [
            "<root><CModelSource><CParameterSourceSet xs.n='parameterSourceSet'><carray_list xs.n='_sources'>".to_owned(),
            parameter("d", "ParamD", "root", false), parameter("e", "ParamE", "two", false),
            parameter("c", "ParamC", "one", false), parameter("b", "ParamB", "one", true),
            parameter("a", "ParamA", "root", false),
            "</carray_list></CParameterSourceSet><CParameterGroupSet xs.n='parameterGroupSet'><carray_list xs.n='_groups'>".to_owned(),
            group("two", "GroupTwo", &inside("one"), "<CParameterGuid xs.ref='shared-e'/>"),
            group("root", "GroupRoot", "<null xs.n='parentGroupGuid'/>",
                "<CParameterGuid uuid='a'/><CParameterGroupGuid uuid='one'/><CParameterGuid uuid='d'/>"),
            group("one", "GroupOne", &inside("root"),
                "<CParameterGuid uuid='b'/><CParameterGuid uuid='c'/><CParameterGroupGuid uuid='two'/>"),
            "</carray_list></CParameterGroupSet><CPartSourceSet xs.n='partSourceSet'><carray_list xs.n='_sources'>".to_owned(),
            part("far", "PartFar", ""), part("deep", "PartDeep", ""),
            part("right", "PartRight", "<CPartGuid uuid='far'/>"),
            part("left", "PartLeft", "<CDrawableGuid uuid='mesh'/><CPartGuid xs.ref='shared-deep'/>"),
            part("top", "PartTop", "<CPartGuid uuid='left'/><CPartGuid uuid='right'/>"),
            "</carray_list></CPartSourceSet><CPartSource xs.n='rootPart' xs.ref='top'/></CModelSource>\
             <CParameterGuid xs.id='shared-e' uuid='e'/><CPartGuid xs.id='shared-deep' uuid='deep'/></root>".to_owned(),
        ].concat()
    }

    #[test]
    fn display_info_follows_the_group_tree_and_lists_parts_level_by_level() {
        assert_eq!(display_info_xml(&display_fixture()).unwrap(), json!({
            "Version": 3,
            "Parameters": [
                {"Id": "ParamA", "GroupId": "", "Name": "Name ParamA"},
                {"Id": "ParamB", "GroupId": "GroupOne", "Name": "Name ParamB"},
                {"Id": "ParamC", "GroupId": "GroupOne", "Name": "Name ParamC"},
                {"Id": "ParamE", "GroupId": "GroupTwo", "Name": "Name ParamE"},
                {"Id": "ParamD", "GroupId": "", "Name": "Name ParamD"},
            ],
            "ParameterGroups": [
                {"Id": "GroupOne", "GroupId": "", "Name": "Name GroupOne"},
                {"Id": "GroupTwo", "GroupId": "GroupOne", "Name": "Name GroupTwo"},
            ],
            "Parts": [
                {"Id": "PartLeft", "Name": "Name PartLeft"}, {"Id": "PartRight", "Name": "Name PartRight"},
                {"Id": "PartDeep", "Name": "Name PartDeep"}, {"Id": "PartFar", "Name": "Name PartFar"},
            ],
            "CombinedParameters": [["ParamB", "ParamC"]],
        }));
        let plain = display_info_xml(&display_fixture().replace(">true<", ">false<")).unwrap();
        assert!(plain.get("CombinedParameters").is_none(), "{plain}");
    }

    #[test]
    fn display_info_refuses_a_tree_it_cannot_follow() {
        let refusal = |old: &str, new: &str| {
            let fixture = display_fixture();
            assert!(fixture.contains(old), "{old}");
            display_info_xml(&fixture.replace(old, new)).unwrap_err()
        };
        // D and E end their groups, a group follows A, and C follows the combined B.
        let combined = |id: &str| {
            let plain = format!("idstr='{id}'/><s xs.n='name'>Name {id}</s><b xs.n='combined'>false<");
            refusal(&plain, &plain.replace("false", "true"))
        };
        for id in ["ParamD", "ParamE", "ParamA"] {
            assert_eq!(combined(id), "authoring combined parameter has no partner", "{id}");
        }
        assert_eq!(combined("ParamC"), "authoring combined parameters overlap");
        assert_eq!(refusal("<b xs.n='combined'>false</b><CParameterGroupGuid xs.n='parentGroupGuid' uuid='one'/>",
            "<b xs.n='combined'>false</b><CParameterGroupGuid xs.n='parentGroupGuid' uuid='two'/>"),
            "authoring parameter group member names another parent than the group listing it");
        assert_eq!(refusal("<CPartGuid uuid='far'/>", ""), "authoring Part is outside the Part tree");
        assert_eq!(refusal("<CParameterGroupGuid xs.n='parentGroupGuid' uuid='root'/><carray_list", "<null xs.n='parentGroupGuid'/><carray_list"),
            "authoring parameter groups must have exactly one root");
        assert_eq!(refusal("<CParameterGuid uuid='d'/>", ""), "authoring parameter or group is outside the parameter group tree");
        assert_eq!(refusal("<CParameterGuid uuid='c'/>", "<CParameterGuid uuid='b'/>"),
            "authoring parameter group tree names a member twice");
        assert_eq!(refusal("<CParameterGuid uuid='c'/>", "<CParameterGuid uuid='missing'/>"),
            "authoring parameter group holds an unknown member");
        assert_eq!(refusal("<CPartGuid uuid='far'/>", "<CPartGuid uuid='missing'/>"),
            "authoring Part names a child Part outside the part source set");
        assert_eq!(refusal("<CPartGuid uuid='far'/>", "<CPartGuid uuid='left'/>"), "authoring Part tree names a Part twice");
    }

    /// Two settings whose IDs in the source are not `PhysicsSetting1..N`; the second sets nothing but what it must.
    fn physics_fixture() -> String {
        let parameter = |guid: &str, id: &str| format!(
            "<CParameterSource><CParameterGuid xs.n='guid' uuid='{guid}'/><CParameterId xs.n='id' idstr='{id}'/></CParameterSource>");
        let link = |tag: &str, field: &str, guid: &str, kind: &str, more: &str| format!(
            "<{tag}><CParameterGuid xs.n='{field}' uuid='{guid}'/><CPhysicsSourceType xs.n='type' v='{kind}'/>\
             <f xs.n='angleScale'>12.3456</f><f xs.n='weight'>59.999996</f><b xs.n='isReverse'>true</b>{more}</{tag}>");
        let vertex = |y: &str| format!(
            "<CPhysicsVertex><GVector2 xs.n='position'><f xs.n='x'>0.0</f><f xs.n='y'>{y}</f></GVector2>\
             <f xs.n='mobility'>0.95</f><f xs.n='delay'>0.800001</f><f xs.n='acceleration'>1.5</f><f xs.n='radius'>-0.0004</f></CPhysicsVertex>");
        let setting = |id: &str, name: &str, inputs: &str, outputs: &str, vertices: &str| format!(
            "<CPhysicsSettingsSource><CPhysicsSettingId xs.n='id' idstr='{id}'/><s xs.n='name'>{name}</s>\
             <carray_list xs.n='inputs'>{inputs}</carray_list><carray_list xs.n='outputs'>{outputs}</carray_list>\
             <carray_list xs.n='vertices'>{vertices}</carray_list>\
             <f xs.n='normalizedPositionValueMin'>-10.0</f><f xs.n='normalizedPositionDefaultValue'>0.0</f>\
             <f xs.n='normalizedPositionValueMax'>10.0</f><f xs.n='normalizedAngleValueMin'>-9.999996</f>\
             <f xs.n='normalizedAngleDefaultValue'>1.25</f><f xs.n='normalizedAngleValueMax'>20.5</f></CPhysicsSettingsSource>");
        [
            "<root><CModelSource><CParameterSourceSet xs.n='parameterSourceSet'><carray_list xs.n='_sources'>".to_owned(),
            parameter("x", "ParamX"), parameter("angle", "ParamAngle"), parameter("hair", "ParamHair"),
            "</carray_list></CParameterSourceSet><CPhysicsSettingsSourceSet xs.n='physicsSettingsSourceSet'>\
             <i xs.n='settingFPS'>60</i><carray_list xs.n='_sourceCubismPhysics'>".to_owned(),
            setting("PhysicsSetting7", "Front",
                &[link("CPhysicsInput", "source", "x", "SRC_TO_X", ""),
                    link("CPhysicsInput", "source", "angle", "SRC_TO_G_ANGLE", "").replace("true", "false")].concat(),
                &link("CPhysicsOutput", "destination", "hair", "SRC_TO_G_ANGLE", "<i xs.n='vertexIndex'>1</i>"),
                &[vertex("0.0"), vertex("3.0")].concat()),
            setting("PhysicsSetting7", "Back", &link("CPhysicsInput", "source", "x", "SRC_TO_X", ""),
                &link("CPhysicsOutput", "destination", "hair", "SRC_TO_G_ANGLE", "<i xs.n='vertexIndex'>2</i>"),
                &[vertex("0.0"), vertex("3.0"), vertex("6.5")].concat()),
            "</carray_list></CPhysicsSettingsSourceSet></CModelSource></root>".to_owned(),
        ].concat()
    }

    #[test]
    fn physics_numbers_the_settings_and_rounds_to_three_decimals() {
        let vertex = |y: Value| json!({"Position": {"X": 0, "Y": y}, "Mobility": 0.95, "Delay": 0.8,
            "Acceleration": 1.5, "Radius": 0});
        let input = |id: &str, kind: &str, reflect: bool| json!({"Source": {"Target": "Parameter", "Id": id},
            "Weight": 60, "Type": kind, "Reflect": reflect});
        let output = |vertex: u32| json!({"Destination": {"Target": "Parameter", "Id": "ParamHair"}, "VertexIndex": vertex,
            "Scale": 12.346, "Weight": 60, "Type": "Angle", "Reflect": true});
        let normalization = json!({"Position": {"Minimum": -10, "Default": 0, "Maximum": 10},
            "Angle": {"Minimum": -10, "Default": 1.25, "Maximum": 20.5}});
        let expected = json!({
            "Version": 3,
            "Meta": {
                "PhysicsSettingCount": 2, "TotalInputCount": 3, "TotalOutputCount": 2, "VertexCount": 5, "Fps": 60,
                "EffectiveForces": {"Gravity": {"X": 0, "Y": -1}, "Wind": {"X": 0, "Y": 0}},
                "PhysicsDictionary": [{"Id": "PhysicsSetting1", "Name": "Front"}, {"Id": "PhysicsSetting2", "Name": "Back"}],
            },
            "PhysicsSettings": [
                {"Id": "PhysicsSetting1", "Input": [input("ParamX", "X", true), input("ParamAngle", "Angle", false)],
                    "Output": [output(1)], "Vertices": [vertex(json!(0)), vertex(json!(3))], "Normalization": normalization},
                {"Id": "PhysicsSetting2", "Input": [input("ParamX", "X", true)], "Output": [output(2)],
                    "Vertices": [vertex(json!(0)), vertex(json!(3)), vertex(json!(6.5))], "Normalization": normalization},
            ],
        });
        let exported = physics_xml(&physics_fixture()).unwrap();
        assert_eq!(exported, expected);
        // Whole numbers are written without a fraction, as the vendor file writes them.
        assert!(exported.to_string().contains(r#""Weight":60}"#), "{exported}");

        let unset = physics_xml(&physics_fixture().replace("<i xs.n='settingFPS'>60</i>", "<null xs.n='settingFPS'/>")).unwrap();
        assert!(unset["Meta"].get("Fps").is_none(), "{unset}");
    }

    #[test]
    fn physics_y_inputs_export_without_enabling_translation_outputs() {
        let exported = physics_xml(&physics_fixture().replace("SRC_TO_X", "SRC_TO_Y")).unwrap();
        let types: Vec<_> = exported["PhysicsSettings"].as_array().unwrap().iter()
            .flat_map(|setting| setting["Input"].as_array().unwrap())
            .map(|input| {
                assert!(input.get("Scale").is_none());
                input["Type"].as_str().unwrap()
            }).collect();
        assert_eq!(types, ["Y", "Angle", "Y"]);
        for token in ["SRC_TO_X", "SRC_TO_Y"] {
            let xml = physics_fixture().replace("uuid='hair'/><CPhysicsSourceType xs.n='type' v='SRC_TO_G_ANGLE'",
                &format!("uuid='hair'/><CPhysicsSourceType xs.n='type' v='{token}'"));
            assert_eq!(physics_xml(&xml).unwrap_err(), "unsupported authoring physics type");
        }
    }

    #[test]
    fn physics_refuses_what_it_cannot_write() {
        let refusal = |old: &str, new: &str| {
            let fixture = physics_fixture();
            assert!(fixture.contains(old), "{old}");
            physics_xml(&fixture.replace(old, new)).unwrap_err()
        };
        assert_eq!(refusal("SRC_TO_X", "SRC_TO_UNKNOWN"), "unsupported authoring physics type");
        assert_eq!(refusal("uuid='hair'/><CPhysicsSourceType xs.n='type' v='SRC_TO_G_ANGLE'",
            "uuid='hair'/><CPhysicsSourceType xs.n='type' v='SRC_TO_X'"), "unsupported authoring physics type");
        // The first vertex is the fixed end of the strand, and the last index is one below the count.
        for index in ["0", "3"] {
            assert_eq!(refusal("<i xs.n='vertexIndex'>2</i>", &format!("<i xs.n='vertexIndex'>{index}</i>")),
                "authoring physics output names no vertex of its setting", "{index}");
        }
        assert_eq!(refusal("xs.n='destination' uuid='hair'", "xs.n='destination' uuid='gone'"),
            "authoring physics names a parameter outside the parameter source set");
        for rate in ["<i xs.n='settingFPS'>59.94</i>", "<i xs.n='settingFPS'/>"] {
            assert_eq!(refusal("<i xs.n='settingFPS'>60</i>", rate), "invalid authoring physics rate", "{rate}");
        }
        assert_eq!(refusal("<f xs.n='delay'>0.800001</f>", "<f xs.n='delay'>NaN</f>"), "invalid finite authoring number");
        assert_eq!(refusal("<f xs.n='delay'>0.800001</f>", "<f xs.n='delay'>1e306</f>"),
            "authoring physics number is out of range");
        // One input named 16385 times through xs.ref, and one long ID copied for each of 70 inputs.
        let aliased = |count: usize, id: &str| physics_fixture()
            .replace("<CPhysicsInput><CParameterGuid xs.n='source' uuid='angle'/>",
                "<CPhysicsInput xs.id='shared'><CParameterGuid xs.n='source' uuid='angle'/>")
            .replacen("</CPhysicsInput></carray_list>",
                &format!("</CPhysicsInput>{}</carray_list>", "<CPhysicsInput xs.ref='shared'/>".repeat(count)), 1)
            .replace("idstr='ParamAngle'", &format!("idstr='{id}'"));
        assert_eq!(physics_xml(&aliased(16384, "ParamAngle")).unwrap_err(),
            "authoring physics exceeds 16384 inputs, outputs or vertices");
        assert!(physics_xml(&aliased(70, &"a".repeat(1 << 20))).unwrap_err().contains("copied-text byte budget"));
        assert!(physics_xml(&aliased(70, "ParamAngle")).is_ok());
        let many = physics_fixture().replacen("<CPhysicsSettingsSource>", "<CPhysicsSettingsSource xs.id='one'>", 1)
            .replace("</carray_list></CPhysicsSettingsSourceSet>",
                &format!("{}</carray_list></CPhysicsSettingsSourceSet>", "<CPhysicsSettingsSource xs.ref='one'/>".repeat(255)));
        assert_eq!(physics_xml(&many).unwrap_err(), "authoring physics exceeds 256 settings");
        for list in ["inputs", "outputs"] {
            let fixture = physics_fixture();
            let (head, rest) = fixture.rsplit_once(&format!("<carray_list xs.n='{list}'>")).unwrap();
            let rest = &rest[rest.find("</carray_list>").unwrap()..];
            assert_eq!(physics_xml(&format!("{head}<carray_list xs.n='{list}'>{rest}")).unwrap_err(),
                "authoring physics setting lacks an input or an output", "{list}");
        }
        let fixture = physics_fixture();
        let (head, rest) = fixture.split_once("<CPhysicsSettingsSource>").unwrap();
        let empty = format!("{head}{}", &rest[rest.rfind("</carray_list></CPhysicsSettingsSourceSet>").unwrap()..]);
        assert_eq!(physics_xml(&empty).unwrap_err(), "authoring model has no physics settings");
    }
}
