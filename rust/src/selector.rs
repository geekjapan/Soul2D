//! Resource selector language (`resource://soul2d/resources/find?selector=`), ported from
//! `soul2d.core.selector`. Selectors run over the JSON document model: nodes from `model["nodes"]`,
//! parameters, parameter groups and bindings from `model["param"]`.
//!
//! Grammar: `element+` joined by `,`. An element is `[>] [Type|*] (#uuid|.name)* [:active] ([key=value])*`.
//! Whitespace between elements is the descendant combinator, `>` the direct-child combinator.
//! `Type` is compared with the exact `typeId` of a resource (nodes: their `type`, parameters and groups:
//! `Parameter`, bindings: `Binding`), so `Node` does not match a `Part`.

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

const MAX_SELECTOR_BYTES: usize = 4096;
const MAX_ELEMENTS: usize = 128;
const MAX_PSEUDO_ARGS: usize = 32;

/// Node types registered by the D editor. Types present in the document are accepted as well.
const NODE_TYPES: [&str; 14] = [
    "Node", "Tmp", "Part", "AnimatedPart", "Composite", "DynamicComposite", "MeshGroup", "Mask",
    "GridDeformer", "PathDeformer", "SimplePhysics", "Camera", "DepthBone", "DepthRigRoot",
];

/// Two-character operators come first so they win over their one-character prefixes.
const PUNCT: [&str; 24] = [
    "==", "!=", ">=", "<=", ">", "<", "=", "[", "]", "{", "}", "(", ")", ",", ".", ":", "+", "-", "*", "/",
    "#", "|", "$", "?",
];

/// One record per matched resource, in evaluation order, without duplicates. Fields: `kind`
/// (`node`/`parameter`/`binding`), `typeId`, `uuid`, `name`, `uri`; nodes add `data`, bindings add
/// `parameter`, `target` and `bindingName`.
///
/// `Binding:active` needs the armed parameter, which is editor state outside the JSON model, so it is an `Err`.
pub fn find(model: &Value, selector: &str) -> Result<Vec<Value>, String> {
    let (run, ids) = execute(model, selector)?;
    Ok(ids
        .iter()
        .map(|&id| {
            let mut record = run.record(id);
            record["kind"] = json!(match run.arena[id].kind {
                Kind::Node => "node",
                Kind::Parameter => "parameter",
                Kind::Binding => "binding",
            });
            record
        })
        .collect())
}

/// The `resources/find` response of the D server: matches nested under their nearest matched ancestor in
/// `items[*].children`, plus the discovery fields.
pub fn find_response(model: &Value, selector: &str) -> Result<Value, String> {
    let (run, ids) = execute(model, selector)?;
    Ok(json!({
        "items": run.tree(&ids),
        "selectorSyntax": "soul2d.core.selector",
        "recommendedDiscoveryUri": "resource://soul2d/resources/find?selector=*",
        "guideUris": [
            "resource://soul2d/guides/find",
            "resource://soul2d/guides/selectors",
            "resource://soul2d/guides/resources",
        ],
    }))
}

fn execute<'a>(model: &'a Value, selector: &str) -> Result<(Run<'a>, Vec<usize>), String> {
    if selector.len() > MAX_SELECTOR_BYTES {
        return Err(format!("selector is longer than {MAX_SELECTOR_BYTES} bytes"));
    }
    let pipelines = parse(&tokenize(selector)?)?;
    let mut run = Run::new(model);
    let mut ids = Vec::new();
    for pipeline in &pipelines {
        ids.extend(run.pipeline(pipeline)?);
    }
    let ids = run.dedupe(ids);
    Ok((run, ids))
}

// ---- tokenizer ----

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Digits(String),
    Str(String),
    Punct(&'static str),
}

fn tokenize(text: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '"' {
            let end = chars[i + 1..]
                .iter()
                .position(|&ch| ch == '"')
                .ok_or("unterminated string literal")?;
            tokens.push(Tok::Str(chars[i + 1..i + 1 + end].iter().collect()));
            i += end + 2;
        } else if let Some(p) = PUNCT
            .iter()
            .find(|p| p.chars().enumerate().all(|(k, pc)| chars.get(i + k) == Some(&pc)))
        {
            tokens.push(Tok::Punct(p));
            i += p.len();
        } else if c.is_alphabetic() {
            let len = chars[i..]
                .iter()
                .take_while(|&&ch| ch.is_alphanumeric() || ch == '-' || ch == '_')
                .count();
            tokens.push(Tok::Ident(chars[i..i + len].iter().collect()));
            i += len;
        } else if c.is_ascii_digit() {
            let mut end = i + chars[i..].iter().take_while(|ch| ch.is_ascii_digit()).count();
            // A fraction needs a digit after the dot, so `#12.Face` stays an id followed by a name.
            if chars.get(end) == Some(&'.') && chars.get(end + 1).is_some_and(char::is_ascii_digit) {
                end += 1 + chars[end + 1..].iter().take_while(|ch| ch.is_ascii_digit()).count();
            }
            tokens.push(Tok::Digits(chars[i..end].iter().collect()));
            i = end;
        } else {
            return Err(format!("unexpected character '{c}' in selector"));
        }
    }
    Ok(tokens)
}

// ---- parser ----

#[derive(Debug)]
enum Filter {
    Name(String),
    Uuid(u32),
    TypeId(String),
}

#[derive(Debug)]
struct Element {
    child: bool,
    type_id: Option<String>,
    filters: Vec<Filter>,
    active: bool,
}

struct Cursor<'t> {
    tokens: &'t [Tok],
    at: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.at)
    }

    fn eat(&mut self, punct: &str) -> bool {
        let hit = matches!(self.peek(), Some(Tok::Punct(p)) if *p == punct);
        self.at += usize::from(hit);
        hit
    }

    fn expect(&mut self, punct: &str) -> Result<(), String> {
        if self.eat(punct) {
            Ok(())
        } else {
            Err(format!("expected '{punct}' but found {}", self.describe()))
        }
    }

    fn describe(&self) -> String {
        match self.peek() {
            None => "end of selector".into(),
            Some(Tok::Ident(s) | Tok::Digits(s)) => format!("'{s}'"),
            Some(Tok::Str(s)) => format!("\"{s}\""),
            Some(Tok::Punct(p)) => format!("'{p}'"),
        }
    }

    /// An identifier, number or quoted string.
    fn value(&mut self) -> Result<String, String> {
        match self.peek() {
            Some(Tok::Ident(s) | Tok::Digits(s) | Tok::Str(s)) => {
                let s = s.clone();
                self.at += 1;
                Ok(s)
            }
            _ => Err(format!("expected a name or value but found {}", self.describe())),
        }
    }
}

fn parse_u32(text: &str, what: &str) -> Result<u32, String> {
    text.parse::<u32>()
        .map_err(|_| format!("{what} '{text}' is not an unsigned 32-bit integer"))
}

fn parse(tokens: &[Tok]) -> Result<Vec<Vec<Element>>, String> {
    if tokens.is_empty() {
        return Err("selector is empty".into());
    }
    let mut cur = Cursor { tokens, at: 0 };
    let mut pipelines = Vec::new();
    let mut pipeline: Vec<Element> = Vec::new();
    let mut total = 0;
    while cur.peek().is_some() {
        let child = cur.eat(">");
        if child && pipeline.is_empty() {
            return Err("combinator '>' needs a selector on its left".into());
        }
        if child && cur.peek().is_none() {
            return Err("combinator '>' needs a selector on its right".into());
        }
        total += 1;
        if total > MAX_ELEMENTS {
            return Err(format!("selector has more than {MAX_ELEMENTS} elements"));
        }
        pipeline.push(parse_element(&mut cur, child)?);
        // The D grammar allows a comma after every query, including the last one.
        if cur.eat(",") {
            pipelines.push(std::mem::take(&mut pipeline));
        }
    }
    if !pipeline.is_empty() {
        pipelines.push(pipeline);
    }
    Ok(pipelines)
}

fn parse_element(cur: &mut Cursor, child: bool) -> Result<Element, String> {
    let type_id = match cur.peek() {
        Some(Tok::Ident(s)) => Some(s.clone()),
        Some(Tok::Punct("*")) => Some("*".to_string()),
        _ => None,
    };
    cur.at += usize::from(type_id.is_some());
    let mut filters = Vec::new();
    let mut any_selector = false;
    loop {
        let by_id = if cur.eat("#") {
            true
        } else if cur.eat(".") {
            false
        } else {
            break;
        };
        any_selector = true;
        let name = cur.value()?;
        filters.push(if by_id {
            Filter::Uuid(parse_u32(&name, "uuid")?)
        } else {
            Filter::Name(name)
        });
    }
    if type_id.is_none() && !any_selector {
        return Err(format!("expected a type, '*', '#' or '.' but found {}", cur.describe()));
    }
    let mut active = false;
    if cur.eat(":") {
        let name = match cur.peek() {
            Some(Tok::Ident(s)) => s.clone(),
            _ => return Err(format!("expected a pseudo-class name but found {}", cur.describe())),
        };
        cur.at += 1;
        if name != "active" {
            return Err(format!("unknown pseudo-class ':{name}' (supported: ':active')"));
        }
        if cur.eat("(") {
            let mut args = 0;
            while !cur.eat(")") {
                cur.value()?;
                cur.eat(",");
                args += 1;
                if args > MAX_PSEUDO_ARGS {
                    return Err("too many pseudo-class arguments".into());
                }
            }
            return Err("pseudo-class ':active' takes no arguments".into());
        }
        active = true;
    }
    while cur.eat("[") {
        let key = match cur.peek() {
            Some(Tok::Ident(s)) => s.clone(),
            _ => return Err(format!("expected an attribute name but found {}", cur.describe())),
        };
        cur.at += 1;
        cur.expect("=")?;
        let value = cur.value()?;
        cur.expect("]")?;
        filters.push(match key.as_str() {
            "name" => Filter::Name(value),
            "uuid" => Filter::Uuid(parse_u32(&value, "uuid")?),
            "typeId" => Filter::TypeId(value),
            _ => return Err(format!("unsupported attribute '{key}' (supported: name, uuid, typeId)")),
        });
    }
    Ok(Element { child, type_id, filters, active })
}

// ---- evaluation ----

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Node,
    Parameter,
    Binding,
}

struct Res<'a> {
    kind: Kind,
    uuid: u32,
    name: String,
    type_id: String,
    key: String,
    /// The resource this one was reached from; the D `Resource.source`. Always an earlier arena index.
    source: Option<usize>,
    children: Vec<usize>,
    json: &'a Value,
    /// Bindings only: the owning parameter and the node (or parameter) uuid the binding drives.
    owner: Option<&'a Value>,
    target: Option<u32>,
}

struct Group<'a> {
    json: &'a Value,
    members: Vec<&'a Value>,
}

struct Run<'a> {
    arena: Vec<Res<'a>>,
    /// Every node below the root in preorder; nodes are cached so each uuid is one resource.
    nodes: Vec<usize>,
    by_uuid: HashMap<u32, usize>,
    node_types: HashSet<String>,
    groups: Vec<Group<'a>>,
    loose: Vec<&'a Value>,
    /// All non-group parameters in model order.
    leaves: Vec<&'a Value>,
}

fn uuid_of(value: &Value, key: &str) -> Option<u32> {
    value.get(key)?.as_u64()?.try_into().ok()
}

fn str_of<'v>(value: &'v Value, key: &str) -> &'v str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn array_of<'v>(value: &'v Value, key: &str) -> &'v [Value] {
    value.get(key).and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// FNV-1a. The D server derived a binding's pseudo-uuid from a memory address; this one is stable.
fn pseudo_uuid(text: &str) -> u32 {
    text.bytes()
        .fold(0x811c_9dc5_u32, |h, b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193))
}

/// Same unreserved set as D `std.uri.encodeComponent`.
fn encode_component(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl<'a> Run<'a> {
    fn new(model: &'a Value) -> Self {
        let mut run = Run {
            arena: Vec::new(),
            nodes: Vec::new(),
            by_uuid: HashMap::new(),
            node_types: HashSet::new(),
            groups: Vec::new(),
            loose: Vec::new(),
            leaves: Vec::new(),
        };
        if let Some(root) = model.get("nodes") {
            run.build_nodes(root);
        }
        run.collect_parameters(array_of(model, "param"));
        run
    }

    /// Iterative preorder walk, so a very deep document cannot overflow the stack.
    fn build_nodes(&mut self, root: &'a Value) {
        let mut stack: Vec<(&'a Value, Option<usize>)> = Vec::new();
        for child in array_of(root, "children").iter().rev() {
            stack.push((child, None));
        }
        while let Some((json, parent)) = stack.pop() {
            let Some(uuid) = uuid_of(json, "uuid") else {
                continue;
            };
            if self.by_uuid.contains_key(&uuid) {
                continue;
            }
            let type_id = json.get("type").and_then(Value::as_str).unwrap_or("Node").to_string();
            self.node_types.insert(type_id.clone());
            let id = self.arena.len();
            self.arena.push(Res {
                kind: Kind::Node,
                uuid,
                name: str_of(json, "name").to_string(),
                type_id,
                key: format!("n{uuid}"),
                source: parent,
                children: Vec::new(),
                json,
                owner: None,
                target: None,
            });
            self.by_uuid.insert(uuid, id);
            self.nodes.push(id);
            if let Some(parent) = parent {
                self.arena[parent].children.push(id);
            }
            for child in array_of(json, "children").iter().rev() {
                stack.push((child, Some(id)));
            }
        }
    }

    fn collect_parameters(&mut self, list: &'a [Value]) {
        for item in list.iter().filter(|item| item.is_object()) {
            if item.get("groupUUID").is_some() {
                let mut members = Vec::new();
                let mut stack: Vec<&'a Value> = array_of(item, "children").iter().rev().collect();
                while let Some(member) = stack.pop() {
                    if member.get("groupUUID").is_some() {
                        stack.extend(array_of(member, "children").iter().rev());
                    } else if uuid_of(member, "uuid").is_some() {
                        members.push(member);
                    }
                }
                self.leaves.extend(members.iter().copied());
                self.groups.push(Group { json: item, members });
            } else if uuid_of(item, "uuid").is_some() {
                self.loose.push(item);
                self.leaves.push(item);
            }
        }
    }

    fn is_node_type(&self, type_id: &str) -> bool {
        NODE_TYPES.contains(&type_id) || self.node_types.contains(type_id)
    }

    /// Parameters and groups are not cached: every creation is a fresh resource, as in D.
    fn parameter(&mut self, json: &'a Value, uuid_key: &str, source: Option<usize>) -> usize {
        let uuid = uuid_of(json, uuid_key).unwrap_or(0);
        self.arena.push(Res {
            kind: Kind::Parameter,
            uuid,
            name: str_of(json, "name").to_string(),
            type_id: "Parameter".into(),
            key: format!("p{uuid}"),
            source,
            children: Vec::new(),
            json,
            owner: None,
            target: None,
        });
        self.arena.len() - 1
    }

    fn binding(&mut self, owner: &'a Value, json: &'a Value, source: Option<usize>) -> usize {
        let name = str_of(json, "param_name").to_string();
        let target = uuid_of(json, "node");
        let key = format!("b{}/{}/{name}", uuid_of(owner, "uuid").unwrap_or(0), target.unwrap_or(0));
        self.arena.push(Res {
            kind: Kind::Binding,
            uuid: pseudo_uuid(&key),
            name,
            type_id: "Binding".into(),
            key,
            source,
            children: Vec::new(),
            json,
            owner: Some(owner),
            target,
        });
        self.arena.len() - 1
    }

    /// Everything addressable: groups with their parameters, remaining parameters, then all nodes.
    fn puppet_walk(&mut self) -> Vec<usize> {
        let mut out = Vec::new();
        let groups: Vec<(&'a Value, Vec<&'a Value>)> =
            self.groups.iter().map(|g| (g.json, g.members.clone())).collect();
        for (group, members) in groups {
            let group_id = self.parameter(group, "groupUUID", None);
            out.push(group_id);
            for member in members {
                out.push(self.parameter(member, "uuid", Some(group_id)));
            }
        }
        for param in self.loose.clone() {
            out.push(self.parameter(param, "uuid", None));
        }
        out.extend(self.nodes.iter().copied());
        out
    }

    fn of_kind(&self, targets: &[usize], kind: Kind) -> Vec<usize> {
        targets.iter().copied().filter(|&t| self.arena[t].kind == kind).collect()
    }

    fn node_children(&self, targets: &[usize]) -> Vec<usize> {
        let nodes = targets.iter().filter(|&&t| self.arena[t].kind == Kind::Node);
        nodes.flat_map(|&t| self.arena[t].children.iter().copied()).collect()
    }

    fn node_descendants(&self, targets: &[usize]) -> Vec<usize> {
        let mut out = Vec::new();
        // A node seen before had its whole subtree emitted, so overlapping targets cost nothing extra.
        let mut seen = HashSet::new();
        for t in self.of_kind(targets, Kind::Node) {
            let mut stack: Vec<usize> = self.arena[t].children.iter().rev().copied().collect();
            while let Some(id) = stack.pop() {
                if seen.insert(id) {
                    out.push(id);
                    stack.extend(self.arena[id].children.iter().rev());
                }
            }
        }
        out
    }

    fn drives(binding: &Value, node: u32) -> bool {
        uuid_of(binding, "node") == Some(node)
    }

    fn node_to_parameters(&mut self, targets: &[usize]) -> Vec<usize> {
        let mut out = Vec::new();
        for t in self.of_kind(targets, Kind::Node) {
            let node = self.arena[t].uuid;
            for param in self.leaves.clone() {
                if array_of(param, "bindings").iter().any(|b| Self::drives(b, node)) {
                    out.push(self.parameter(param, "uuid", Some(t)));
                }
            }
        }
        out
    }

    fn node_to_bindings(&mut self, targets: &[usize]) -> Vec<usize> {
        let mut out = Vec::new();
        for t in self.of_kind(targets, Kind::Node) {
            let node = self.arena[t].uuid;
            for param in self.leaves.clone() {
                for binding in array_of(param, "bindings").iter().filter(|b| Self::drives(b, node)) {
                    out.push(self.binding(param, binding, Some(t)));
                }
            }
        }
        out
    }

    /// Bindings of parameters; when the parameter was reached from a node only that node's bindings count.
    fn parameter_to_bindings(&mut self, targets: &[usize]) -> Vec<usize> {
        let mut out = Vec::new();
        for t in self.of_kind(targets, Kind::Parameter) {
            let mut from = self.arena[t].source;
            let mut node = None;
            while let Some(s) = from {
                if self.arena[s].kind == Kind::Node {
                    node = Some(self.arena[s].uuid);
                    break;
                }
                from = self.arena[s].source;
            }
            let param = self.arena[t].json;
            for binding in array_of(param, "bindings") {
                if node.is_none_or(|n| Self::drives(binding, n)) {
                    out.push(self.binding(param, binding, Some(t)));
                }
            }
        }
        out
    }

    fn dedupe(&self, ids: Vec<usize>) -> Vec<usize> {
        let mut seen = HashSet::new();
        ids.into_iter().filter(|&id| seen.insert(self.arena[id].key.clone())).collect()
    }

    fn pipeline(&mut self, elements: &[Element]) -> Result<Vec<usize>, String> {
        let mut cur = self.puppet_walk();
        let mut last = String::new();
        for (i, el) in elements.iter().enumerate() {
            let ty = el.type_id.as_deref().unwrap_or("");
            let prev_node = last.is_empty() || last == "*" || self.is_node_type(&last);
            let is_node = ty.is_empty() || ty == "*" || self.is_node_type(ty);
            if prev_node && is_node {
                if i > 0 {
                    cur = if el.child { self.node_children(&cur) } else { self.node_descendants(&cur) };
                }
            } else if prev_node {
                if i > 0 && ty == "Parameter" {
                    cur = self.node_to_parameters(&cur);
                } else if ty == "Binding" {
                    if el.active {
                        return Err("':active' needs the armed parameter, which the JSON model does not carry".into());
                    }
                    cur = self.node_to_bindings(&cur);
                }
            } else if ty == "Binding" {
                cur = self.parameter_to_bindings(&cur);
            }
            if !ty.is_empty() && ty != "*" {
                cur.retain(|&id| self.arena[id].type_id == ty);
            }
            for filter in &el.filters {
                cur.retain(|&id| {
                    let res = &self.arena[id];
                    match filter {
                        Filter::Name(name) => res.name == *name,
                        Filter::Uuid(uuid) => res.uuid == *uuid,
                        Filter::TypeId(type_id) => res.type_id == *type_id,
                    }
                });
            }
            cur = self.dedupe(cur);
            last = ty.to_string();
        }
        Ok(cur)
    }

    // ---- output ----

    /// The `name`/`typeId` of a binding target, found among the document's nodes and parameters.
    fn target_info(&self, uuid: u32) -> (String, &'static str, String) {
        if let Some(&id) = self.by_uuid.get(&uuid) {
            return (self.arena[id].name.clone(), "node", self.arena[id].type_id.clone());
        }
        let param = self.leaves.iter().find(|p| uuid_of(p, "uuid") == Some(uuid));
        match param {
            Some(p) => (str_of(p, "name").to_string(), "parameter", "Parameter".into()),
            None => (String::new(), "unknown", String::new()),
        }
    }

    fn record(&self, id: usize) -> Value {
        let res = &self.arena[id];
        let mut record = json!({
            "typeId": res.type_id,
            "uuid": res.uuid,
            "name": res.name,
            "uri": format!("resource://soul2d/resources/{}", res.uuid),
        });
        match res.kind {
            Kind::Node => {
                record["data"] = json!({ "uuid": res.uuid, "name": res.name, "type": res.type_id });
            }
            Kind::Parameter => {}
            Kind::Binding => {
                let owner = res.owner.unwrap_or(&Value::Null);
                let owner_uuid = uuid_of(owner, "uuid").unwrap_or(0);
                let target = res.target.unwrap_or(0);
                let (target_name, _, target_type) = self.target_info(target);
                record["uri"] = json!(format!(
                    "resource://soul2d/bindings/get?parameter={owner_uuid}&target={target}&name={}",
                    encode_component(&res.name)
                ));
                record["parameter"] = json!({ "uuid": owner_uuid, "name": str_of(owner, "name") });
                record["target"] = json!({ "uuid": target, "name": target_name, "typeId": target_type });
                record["bindingName"] = json!(res.name);
            }
        }
        record
    }

    /// Nest every match under its nearest matched `source` ancestor (D `TreeStore_!false`). A source always
    /// has a lower arena index than its resource, so building from the highest index down needs no recursion.
    fn tree(&self, ids: &[usize]) -> Vec<Value> {
        let included: HashSet<usize> = ids.iter().copied().collect();
        let mut kids: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut roots = Vec::new();
        for &id in ids {
            let mut from = self.arena[id].source;
            let mut parent = None;
            while let Some(s) = from {
                if included.contains(&s) {
                    parent = Some(s);
                    break;
                }
                from = self.arena[s].source;
            }
            match parent {
                Some(p) => kids.entry(p).or_default().push(id),
                None => roots.push(id),
            }
        }
        let mut order = ids.to_vec();
        order.sort_unstable_by(|a, b| b.cmp(a));
        let mut built: HashMap<usize, Value> = HashMap::new();
        for id in order {
            let mut record = self.record(id);
            let children: Vec<Value> = kids
                .remove(&id)
                .unwrap_or_default()
                .iter()
                .filter_map(|child| built.remove(child))
                .collect();
            record["children"] = Value::Array(children);
            built.insert(id, record);
        }
        roots.iter().filter_map(|root| built.remove(root)).collect()
    }
}
