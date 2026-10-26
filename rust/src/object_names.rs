//! `object.find` and `object.replace_name`: look objects up by a part of their name and rename many at once.
//! `object.export_csv` and `object.import_csv`: the same names as a CSV text that a spreadsheet edits.
//! An object is a node of any type, a parameter or a parameter group. An INX object has a name and a numeric
//! UUID but no separate ID string, so only names are covered.

use crate::mcp::echoed;
use crate::{check_parameter_name, find_parameter, find_parameter_mut, nodes, required_string, valid_id};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// Most entries of `ids` and `types`, and most rows of a CSV after its header.
const MAX_LIST: usize = 4096;
/// Most characters of `text`, `find` and `replace`.
const MAX_TEXT_CHARS: usize = 256;
/// Most characters of a name after a replace. A short `find` and a long `replace` grow a name up to 256 times
/// per call, so a few calls without this bound could fill the memory.
const MAX_NAME_CHARS: usize = 4096;
/// Most bytes of a CSV text that is read. A larger list is imported in parts.
pub(crate) const MAX_CSV_BYTES: usize = 1024 * 1024;
const CSV_HEADER: [&str; 3] = ["uuid", "type", "name"];
/// What a spreadsheet takes for the start of a formula. A type or a name that starts with one is written after an
/// apostrophe, and so is one that starts with an apostrophe, so that reading takes exactly one away.
const FORMULA_STARTS: [char; 7] = ['=', '+', '-', '@', '\t', '\r', '\''];

struct Object<'a> {
    kind: &'static str,
    type_id: &'a str,
    uuid: u32,
    name: &'a str,
}

/// Nodes depth first, then parameters and groups in list order, each group before its members.
fn objects(model: &Value) -> Vec<Object<'_>> {
    fn name(item: &Value) -> &str {
        item.get("name").and_then(Value::as_str).unwrap_or("")
    }
    fn children(item: &Value) -> &[Value] {
        item.get("children").and_then(Value::as_array).map_or(&[], Vec::as_slice)
    }
    fn walk_nodes<'a>(node: &'a Value, out: &mut Vec<Object<'a>>) {
        if let Some(uuid) = node.get("uuid").and_then(Value::as_u64).and_then(valid_id) {
            let type_id = node.get("type").and_then(Value::as_str).unwrap_or("Node");
            out.push(Object { kind: "node", type_id, uuid, name: name(node) });
        }
        children(node).iter().for_each(|child| walk_nodes(child, out));
    }
    fn walk_parameters<'a>(list: &'a [Value], out: &mut Vec<Object<'a>>) {
        for item in list {
            if let Some(uuid) = item.get("uuid").or_else(|| item.get("groupUUID")).and_then(Value::as_u64).and_then(valid_id) {
                out.push(Object { kind: "parameter", type_id: "Parameter", uuid, name: name(item) });
            }
            walk_parameters(children(item), out);
        }
    }
    let mut out = Vec::new();
    if let Some(root) = model.get("nodes") {
        walk_nodes(root, &mut out);
    }
    if let Some(list) = model.get("param").and_then(Value::as_array) {
        walk_parameters(list, &mut out);
    }
    out
}

fn bounded<'a>(request: &'a Value, key: &str, allow_empty: bool) -> Result<&'a str, String> {
    let text = required_string(request, key)?;
    if text.is_empty() && !allow_empty {
        return Err(format!("{key} must not be empty"));
    }
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(format!("{key} is longer than {MAX_TEXT_CHARS} characters"));
    }
    Ok(text)
}

/// `{"matches": [{kind, typeId, uuid, name}]}` for every object whose name contains `text`.
pub fn find(model: &Value, request: &Value) -> Result<Value, String> {
    let text = bounded(request, "text", false)?;
    let case_sensitive = match request.get("caseSensitive") {
        None => true,
        Some(value) => value.as_bool().ok_or("caseSensitive must be a boolean")?,
    };
    let types: Option<Vec<&str>> = match request.get("types") {
        None => None,
        Some(value) => {
            let list = value.as_array().filter(|list| !list.is_empty() && list.len() <= MAX_LIST);
            let list = list.ok_or_else(|| format!("types must be an array of 1 to {MAX_LIST} strings"))?;
            Some(list.iter().map(Value::as_str).collect::<Option<_>>().ok_or("types must contain only strings")?)
        }
    };
    let needle = if case_sensitive { text.to_owned() } else { text.to_lowercase() };
    let matches: Vec<Value> = objects(model)
        .into_iter()
        .filter(|object| types.as_ref().is_none_or(|types| types.contains(&object.type_id)))
        .filter(|object| {
            if case_sensitive { object.name.contains(&needle) } else { object.name.to_lowercase().contains(&needle) }
        })
        .map(|o| json!({"kind": o.kind, "typeId": o.type_id, "uuid": o.uuid, "name": o.name}))
        .collect();
    Ok(json!({ "matches": matches }))
}

/// The candidate document and the reply of `object.replace_name`. Every name is judged against the finished
/// document, so one call may hand a name from one listed object to another.
pub fn replace(model: &Value, request: &Value) -> Result<(Value, Value), String> {
    let find = bounded(request, "find", false)?;
    let replace = bounded(request, "replace", true)?;
    let list = request.get("ids").and_then(Value::as_array).filter(|list| !list.is_empty() && list.len() <= MAX_LIST);
    let list = list.ok_or_else(|| format!("ids must be an array of 1 to {MAX_LIST} UUIDs"))?;
    let ids = list.iter().map(|value| value.as_u64().and_then(valid_id).ok_or("ids must contain nonzero u32 UUIDs"));
    rename_all(model, ids, |_, from| from.replace(find, replace))
}

/// Renames the objects of `ids`, each to what `to` gives for its UUID and its present name. Answers the candidate
/// document and `{renamed, unchanged}`.
fn rename_all<'a>(
    model: &Value,
    ids: impl Iterator<Item = Result<u32, &'a str>>,
    to: impl Fn(u32, &str) -> String,
) -> Result<(Value, Value), String> {
    let mut seen = HashSet::new();
    let (mut node_renames, mut parameter_renames) = (Vec::new(), Vec::new());
    let (mut renamed, mut unchanged) = (Vec::new(), Vec::new());
    for id in ids {
        let id = id?;
        if !seen.insert(id) {
            return Err(format!("ids lists {id} more than once"));
        }
        let (kind, object) = match (nodes::find_node(model, id), find_parameter(model, id)) {
            (Some(_), Some(_)) => return Err(format!("{id} names more than one object")),
            (Some(node), None) => ("node", node),
            (None, Some(parameter)) => ("parameter", parameter),
            (None, None) => return Err(format!("{id} names no node, parameter or parameter group")),
        };
        let from = object.get("name").and_then(Value::as_str).unwrap_or("");
        let to = to(id, from);
        if to == from {
            unchanged.push(id);
            continue;
        }
        if to.chars().count() > MAX_NAME_CHARS {
            return Err(format!("{kind} {id} would get a name longer than {MAX_NAME_CHARS} characters"));
        }
        renamed.push(json!({"kind": kind, "uuid": id, "from": from, "to": to}));
        if kind == "node" { &mut node_renames } else { &mut parameter_renames }.push((id, to));
    }
    let mut candidate = if node_renames.is_empty() { model.clone() } else { nodes::rename_nodes(model, &node_renames)? };
    for (id, name) in &parameter_renames {
        let refusal = |why: &str| format!("parameter {id} cannot be named {:?}: {why}", echoed([name.as_str()]));
        if name.is_empty() {
            return Err(refusal("the name would be empty"));
        }
        let parameter = find_parameter_mut(&mut candidate, *id).ok_or_else(|| format!("parameter {id} was not found"))?;
        parameter["name"] = Value::String(name.clone());
    }
    for (id, name) in &parameter_renames {
        check_parameter_name(&candidate, name, Some(*id))
            .map_err(|why| format!("parameter {id} cannot be named {:?}: {why}", echoed([name.as_str()])))?;
    }
    Ok((candidate, json!({"renamed": renamed, "unchanged": unchanged})))
}

/// One cell of a CSV row for a text of the document, which may come from a file of somebody else: after an
/// apostrophe when a spreadsheet would take it for a formula, and as RFC 4180 writes it, in double quotes when it
/// holds a comma, a quote or a line break, with a quote inside written twice. A semicolon and a tab are quoted too,
/// since some spreadsheets split a row on them and the text after one would start a cell.
fn cell(text: &str) -> String {
    let guard = if text.starts_with(FORMULA_STARTS) { "'" } else { "" };
    if text.contains([',', '"', '\n', '\r', ';', '\t']) {
        format!("\"{guard}{}\"", text.replace('"', "\"\""))
    } else {
        format!("{guard}{text}")
    }
}

/// A field of a row without the apostrophe `cell` may have put before it.
fn plain(cell: &str) -> &str {
    cell.strip_prefix('\'').unwrap_or(cell)
}

/// The names of the document as CSV text, and the number of its objects: the header `uuid,type,name`, then one
/// row for each node, parameter and parameter group in the order of `object.find`.
pub fn export_csv(model: &Value) -> (String, usize) {
    let objects = objects(model);
    let mut text = CSV_HEADER.join(",") + "\n";
    for object in &objects {
        text += &format!("{},{},{}\n", object.uuid, cell(object.type_id), cell(object.name));
    }
    (text, objects.len())
}

/// The records of a CSV text as RFC 4180 writes them. A line ends with LF or CRLF, and an empty line is no record.
/// A text with more records or more fields in one than an import reads is refused while it is read.
fn records(text: &str) -> Result<Vec<Vec<String>>, String> {
    let mut records: Vec<Vec<String>> = Vec::new();
    let (mut record, mut field) = (Vec::new(), String::new());
    // Inside the quotes of a field, and after the quotes of a field have closed.
    let (mut quoted, mut closed) = (false, false);
    let mut chars = text.chars().chain(['\n']).peekable();
    while let Some(next) = chars.next() {
        let row = records.len() + 1;
        match next {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            '"' if quoted => (quoted, closed) = (false, true),
            _ if quoted => field.push(next),
            '"' if field.is_empty() && !closed => quoted = true,
            ',' if record.len() + 1 == CSV_HEADER.len() => {
                return Err(format!("row {row} has more than {} fields", CSV_HEADER.len()));
            }
            ',' => {
                record.push(std::mem::take(&mut field));
                closed = false;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                let empty = record.is_empty() && field.is_empty() && !closed;
                record.push(std::mem::take(&mut field));
                closed = false;
                if empty {
                    record.clear();
                } else if records.len() > MAX_LIST {
                    return Err(format!("the CSV has more than {MAX_LIST} rows after its header"));
                } else {
                    records.push(std::mem::take(&mut record));
                }
            }
            _ if closed => return Err(format!("row {row} has text after the closing quote of a field")),
            '"' => return Err(format!("row {row} has a quote inside a field that is not quoted")),
            _ => field.push(next),
        }
    }
    if quoted {
        return Err("a quoted field does not end".into());
    }
    Ok(records)
}

/// The candidate document and the reply of `object.import_csv` for a text `export_csv` wrote and somebody edited.
/// A row names its object by UUID and gives it the name of the row. Objects without a row keep theirs.
pub fn import_csv(model: &Value, text: &str) -> Result<(Value, Value), String> {
    if text.len() > MAX_CSV_BYTES {
        return Err(format!("the CSV text is larger than {MAX_CSV_BYTES} bytes"));
    }
    // A spreadsheet writes a byte order mark before UTF-8.
    let mut rows = records(text.strip_prefix('\u{feff}').unwrap_or(text))?.into_iter();
    if rows.next().is_none_or(|header| header != CSV_HEADER) {
        return Err("the first row of the CSV must be uuid,type,name".into());
    }
    let types: HashMap<u32, &str> = objects(model).iter().map(|object| (object.uuid, object.type_id)).collect();
    let mut names = HashMap::new();
    let mut ids = Vec::new();
    for (at, fields) in rows.enumerate() {
        // The header is row 1.
        let row = at + 2;
        let [uuid, type_id, name] = <[String; 3]>::try_from(fields)
            .map_err(|fields| format!("row {row} has {} fields, and 3 are needed", fields.len()))?;
        let id = uuid.parse::<u64>().ok().and_then(valid_id);
        let id = id.ok_or_else(|| format!("row {row}: the uuid must be a nonzero u32"))?;
        let Some(known) = types.get(&id) else {
            return Err(format!("row {row}: {id} names no node, parameter or parameter group"));
        };
        if *known != plain(&type_id) {
            return Err(format!("row {row}: {id} is not of the type the row says"));
        }
        if names.insert(id, plain(&name).to_owned()).is_some() {
            return Err(format!("row {row}: {id} has a row already"));
        }
        ids.push(id);
    }
    rename_all(model, ids.into_iter().map(Ok), |id, _| names[&id].clone())
}
