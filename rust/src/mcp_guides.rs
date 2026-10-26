//! Guide texts of the D MCP server (`api/mcp/server.d`): the three `resource://soul2d/guides/*` documents
//! and the five prompts. The wording is D's, so clients that quote it keep working.

pub const GUIDE_URIS: [(&str, &str); 3] = [
    ("resource://soul2d/guides/resources", "Resources Guide"),
    ("resource://soul2d/guides/selectors", "Selectors Guide"),
    ("resource://soul2d/guides/find", "Find Guide"),
];

/// The operating skill (`.cursor/skills/operate-soul2d`), served as resources so an ACP agent reaches it without a
/// checkout: `SKILL_URI` is its `SKILL.md`, `SKILL_FAMILY_PREFIX<family>` one family map.
pub const SKILL_URI: &str = "resource://soul2d/skill/operate-soul2d";
pub const SKILL_FAMILY_PREFIX: &str = "resource://soul2d/skill/operate-soul2d/families/";
const SKILL_MD: &str = include_str!("../../.cursor/skills/operate-soul2d/SKILL.md");
/// (family, map text), the files of `families/`; the test below checks the table against the checkout.
pub const SKILL_FAMILIES: [(&str, &str); 23] = [
    ("animation", include_str!("../../.cursor/skills/operate-soul2d/families/animation.md")),
    ("atlas-export", include_str!("../../.cursor/skills/operate-soul2d/families/atlas-export.md")),
    ("automesh", include_str!("../../.cursor/skills/operate-soul2d/families/automesh.md")),
    ("deformers", include_str!("../../.cursor/skills/operate-soul2d/families/deformers.md")),
    ("depth-bones-rig", include_str!("../../.cursor/skills/operate-soul2d/families/depth-bones-rig.md")),
    ("depth-map-draw", include_str!("../../.cursor/skills/operate-soul2d/families/depth-map-draw.md")),
    ("editor-state", include_str!("../../.cursor/skills/operate-soul2d/families/editor-state.md")),
    ("history", include_str!("../../.cursor/skills/operate-soul2d/families/history.md")),
    ("import-merge", include_str!("../../.cursor/skills/operate-soul2d/families/import-merge.md")),
    ("live2d-animation", include_str!("../../.cursor/skills/operate-soul2d/families/live2d-animation.md")),
    ("live2d-authoring", include_str!("../../.cursor/skills/operate-soul2d/families/live2d-authoring.md")),
    ("live2d-expression", include_str!("../../.cursor/skills/operate-soul2d/families/live2d-expression.md")),
    ("live2d-physics", include_str!("../../.cursor/skills/operate-soul2d/families/live2d-physics.md")),
    ("live2d-pose", include_str!("../../.cursor/skills/operate-soul2d/families/live2d-pose.md")),
    ("mesh-deform", include_str!("../../.cursor/skills/operate-soul2d/families/mesh-deform.md")),
    ("motion3", include_str!("../../.cursor/skills/operate-soul2d/families/motion3.md")),
    ("nodes-inspector", include_str!("../../.cursor/skills/operate-soul2d/families/nodes-inspector.md")),
    ("parameters-bindings", include_str!("../../.cursor/skills/operate-soul2d/families/parameters-bindings.md")),
    ("physics", include_str!("../../.cursor/skills/operate-soul2d/families/physics.md")),
    ("render-scene", include_str!("../../.cursor/skills/operate-soul2d/families/render-scene.md")),
    ("session", include_str!("../../.cursor/skills/operate-soul2d/families/session.md")),
    ("split", include_str!("../../.cursor/skills/operate-soul2d/families/split.md")),
    ("textures", include_str!("../../.cursor/skills/operate-soul2d/families/textures.md")),
];

/// The skill text behind a `resource://soul2d/skill/...` URI.
pub fn skill(uri: &str) -> Option<&'static str> {
    if uri == SKILL_URI {
        return Some(SKILL_MD);
    }
    let family = uri.strip_prefix(SKILL_FAMILY_PREFIX)?;
    SKILL_FAMILIES.iter().find(|(name, _)| *name == family).map(|(_, text)| *text)
}

pub fn guide(uri: &str) -> Option<&'static str> {
    Some(match uri {
        "resource://soul2d/guides/resources" => GUIDES_RESOURCES,
        "resource://soul2d/guides/selectors" => GUIDES_SELECTORS,
        "resource://soul2d/guides/find" => GUIDES_FIND,
        _ => return None,
    })
}

/// (name, description, response description, text), in D's registration order.
pub const PROMPTS: [(&str, &str, &str, &str); 5] = [
    ("resources/find", "How to use the resources/find resource endpoint", "Guide for resources/find", RES_FIND_PROMPT),
    ("resources/get", "How to use resources/get tool", "Guide for resources/get", RESOURCE_PROMPT),
    (
        "selectors/guide",
        "Selector syntax, examples, and returned tree shape for resources/find",
        "Selector syntax",
        SELECTOR_GUIDE,
    ),
    ("resources/guide", "Resources usage guide", "Resources policy", RESOURCES_GUIDE),
    ("tools/guide", "Tools input and context guide", "Tools usage", TOOLS_GUIDE),
];

const GUIDES_RESOURCES: &str = "# Resources Guide

- Read `resource://soul2d/document/state` for the current Rust revision, format, runtime values and Motion3 preview.
- Read `resource://soul2d/document/statistics` for the rendered node count, mask references and the maximum offscreen depth.
- Runtime values use native Parameter IDs with scalars for CMO3, or numeric UUID strings with vectors for INX. `motion3` is null when inactive.
- For `live2d.cmo3`, use Live2D authoring tools; numeric node UUID endpoints describe INX models.
- For `live2d.can3`, read `resource://soul2d/document/animation-source` for the active stored keys, including modern and legacy-colon PartOpacity keys. The five key operations select a unique curveId across parameterCurves, modern partOpacityCurves, legacyPartOpacityCurves and underscorePartOpacityCurves. For underscore-map channels, read the raw mapKey and pass it as optional partOpacityMapKey; a duplicate curveId requires that exact key. Supplied selectors never fall back. Legacy and underscore curve creation and reordering are unsupported. Individual curve deletion accepts the same exact optional partOpacityMapKey selector and removes its owned map entry, optional member and attribute; shared/nested storage and outside references are refused. Parent track/scene deletion also removes exact owned mapped Part definitions; optional mapped members and inline unshared inheritance are supported, while unowned map/list entries, nested shared IDs and outside references are refused. Underscore playback binds exact Part GUIDs in an explicitly attached CMO3, keyed by scene/track/mapKey; empty PartOpacity curves produce no write and multiple populated writers for one Part are refused. Read underscorePartBindings and unboundUnderscorePartCurves to distinguish matched and unapplied channels. Raw map, attribute and source identities remain independent. Use `Live2DCommand_AnimationKeySet` with the current document revision to edit one existing anchor value, `live2d_animation_handle_set` and `live2d_animation_curve_type_set` to shape a curve, and `live2d_animation_scene_set` or `live2d_animation_scene_add` to rename, retime or add a scene (these four have no `Live2DCommand_*` name). Without an attached CMO3, runtime `values` are empty and `motion3` is null. Use an explicit model attachment for bounded playback; inspect nativePlayback.active poseSignals and partOpacities in the snapshot. This does not establish Cubism playback equivalence.
- Broad discovery: read `resource://soul2d/resources/find?selector=*`
- Narrow discovery: use `resource://soul2d/resources/find?selector=...`
- Read one item: use `resource://soul2d/resources/{uuid}`
- Selector syntax: read `resource://soul2d/guides/selectors`
- Find endpoint usage: read `resource://soul2d/guides/find`
";

const GUIDES_SELECTORS: &str = "# Selectors Guide

Used by `resource://soul2d/resources/find?selector=...`

Basics:
- `*` any resource
- `Node`, `Part`, `Parameter`, `Binding`, `Group`
- `.name` match by name
- `#123` match by UUID
- `[name=\"Eye\"]` attribute match
- `A > B` child combinator
- `A B` descendant combinator
- `A, B` union

Recommended first query:
- `resource://soul2d/resources/find?selector=*`
";

const GUIDES_FIND: &str = "# Find Guide

Endpoint: `resource://soul2d/resources/find?selector=...`

Recommended first query:
- `resource://soul2d/resources/find?selector=*`

Examples:
- all nodes: `resource://soul2d/resources/find?selector=Node`
- direct child parts: `resource://soul2d/resources/find?selector=Node%20%3E%20Part`
- part named Eye: `resource://soul2d/resources/find?selector=Part.%22Eye%22`

Next step:
- follow each item's `uri` with `resources/read`
";

const RES_FIND_PROMPT: &str = "Resource Endpoint: resources/find
Purpose: Hierarchical exploration ONLY. Use selector to locate resource instances and their UUIDs. For reading content, use resources/{uuid}. Returns Basics + Children tree.

Recommended first step:
- Start broad discovery with resource://soul2d/resources/find?selector=*

Selector syntax guide:
- See resource: resource://soul2d/guides/selectors

Workflow guide:
- See resource: resource://soul2d/guides/resources

Input fields:
- selector (string): soul2d.core.selector query

Examples:
1) List all nodes (basic info)
   resource://soul2d/resources/find?selector=Node
2) By name (Part named \"Eye\")
   resource://soul2d/resources/find?selector=Part.%22Eye%22
3) Direct children Parts of any Node
   resource://soul2d/resources/find?selector=Node%20%3E%20Part
4) Active bindings of selected node
   resource://soul2d/resources/find?selector=Binding%3Aactive

MCP command binding input:
- Do not pass Binding pseudo-UUIDs to tools. For commands such as RemoveBinding, pass context.parameters[0] plus context.bindings=[{target:<Node-or-Parameter UUID>, name:<binding name>}].

Binding value read endpoint:
- resource://soul2d/bindings/get?parameter=<parameter UUID>&target=<target UUID>&name=<binding name>

Official read endpoint:
- resource://soul2d/resources/{uuid} (see resources/get)
";

const RESOURCE_PROMPT: &str = "Resource Endpoint: resources/get
Purpose: OFFICIAL read endpoint. Fetch one resource instance by UUID. Node returns Basics + State + Geometry + Links; Parameter returns Basics.

Binding values use the descriptor endpoint instead of UUID:
- resource://soul2d/bindings/get?parameter=<parameter UUID>&target=<target UUID>&name=<binding name>

Input fields:
- uuid (integer): Numeric UUID of a Node or Parameter.

Examples:
- Get a node by UUID
  uuid=123456
";

const SELECTOR_GUIDE: &str = "Selector Syntax (soul2d.core.selector)

Used by:
- resource://soul2d/resources/find?selector=...

Recommended discovery query:
- resource://soul2d/resources/find?selector=*

Overview:
- Pattern: [TypeId|*] [ .name | #UUID ] [ [attr=value] ] [ :pseudo(args) ] [ combinators ]
- Combinators: A > B (child), A B (descendant), A, B (union)

Types:
- Common: Root, Node, Part, Parameter, Binding, Group, * (any)

Basic selectors:
- .name              → match by name (string). Use quotes for spaces or non-ASCII.
- #UUID              → match by numeric UUID (decimal).
- [name=\"...\"]     → attribute equals (string).
- [uuid=123]         → attribute equals (numeric).

Quoting & escaping:
- Use double quotes for names with spaces: Part.\"Left Eye\"
- Escape quotes with \\\": Part.\\\"\"Quoted\"\\\"

Pseudoclasses (examples):
- Binding:active     → bindings on the currently armed parameter.

Tool input:
- Binding resources are for inspection. MCP tools identify bindings as {target:<UUID>, name:<binding name>} under context.bindings, together with context.parameters[0].

Binding value read:
- Use the Binding item's URI: resource://soul2d/bindings/get?parameter=<parameter UUID>&target=<target UUID>&name=<binding name>

Combinator examples:
- Node > Part        → direct Part children of any Node.
- Node Part          → any descendant Part under any Node.
- Node, Part         → all Nodes and Parts.

Result shape:
- resources/find returns a tree under 'items'.
- Each returned node includes 'uri' so clients can follow up with resources/read.
- Child combinators and descendant queries preserve hierarchy in the returned tree.

Advanced examples:
- Part.#123          → Part with UUID 123.
- Part[name=\"Eye\"] → Part whose name is \"Eye\".
- Binding:active     → only active bindings for the armed parameter.

Tips:
- Start with selector=* when you need the broad current resource tree.
- Prefer #UUID for precise targeting.
- Build lists with resources/find, then fetch details by UUID.
";

const RESOURCES_GUIDE: &str = "Resources vs Tools (Official Policy)

Policy:
- Exploration: Use resources/find (hierarchical traversal only).
- Reading:    Use resources/{uuid} (official endpoint) to fetch content.
- Listing:    'resources/list' returns currently readable resource instance URIs for the active puppet.
- Tools:      Use Tool commands only to mutate app state or perform actions.

URIs:
- resource://soul2d/resources/find?selector=... (exploration)
- resource://soul2d/resources/{uuid} (read one resource instance)
- resource://soul2d/bindings/get?parameter=...&target=...&name=... (read one binding value grid)
- resources/list (list current resource instances)

Prompt links:
- resource://soul2d/guides/find describes the exploration endpoint and recommends selector=* for first-pass discovery.
- resource://soul2d/guides/selectors describes selector syntax and returned tree shape.
- resource://soul2d/guides/resources describes the overall exploration/read workflow.

Typical flow:
1) Start broad discovery with resources/find?selector=*.
2) Narrow with more specific selectors as needed.
3) Read details via resources/{uuid}.
4) Read binding values via bindings/get descriptor URIs when needed.
5) Optionally call Tools to modify.

Notes:
- In soul2d, direct resource entries are rebuilt when a puppet is loaded so that 'resources/list' reflects current instances.
- 'resources/templates/list' lists parameterized read definitions such as resources/{uuid}.
- See also: 'selectors/guide' and 'tools/guide'.";

const TOOLS_GUIDE: &str = "Tools Input & Context

Role:
- Tools are action endpoints. They may mutate app state or perform side effects.
- Use resources/list, resources/find, and resources/{uuid} for discovery and reading.

Context (recommended null):
- Omit context to use the active app state (same as shortcuts).
- Provide only when needed to override selection.

Context keys:
- parameters: uint[] of Parameter UUIDs.
- armedParameters: uint[] for commands that explicitly use armed-parameter context. Only arm-specific commands change the GUI ArmedParameter.
- nodes: uint[] of Node UUIDs.
- parameterValue: [x] or [x,y] parameter-axis values. Values must exactly match existing key values.
- keyPoint is not part of the MCP input surface. Do not pass key point indexes through MCP.

Other parameters:
- Unless a tool defines additional properties, only 'context' is accepted in this build.
- UUIDs are numeric (decimal). Obtain them via resources/find or resources/{uuid}.

Examples (JSON args):
- Run with default context: {}
- Run for specific nodes: {\"context\": {\"nodes\":[123,456]}}
- Set a parameter key without arming: {\"context\": {\"parameters\":[789], \"parameterValue\":[-0.5,0]}}
- Set and arm a parameter key: {\"context\": {\"armedParameters\":[789], \"parameterValue\":[-0.5,0]}}
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_served_skill_is_the_checkout_s_operate_soul2d() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.cursor/skills/operate-soul2d/families");
        let mut names: Vec<String> = std::fs::read_dir(dir).unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().trim_end_matches(".md").to_owned()).collect();
        names.sort();
        let mut served: Vec<String> = SKILL_FAMILIES.iter().map(|(name, _)| (*name).to_owned()).collect();
        served.sort();
        assert_eq!(served, names, "every family map is served");
        assert!(skill(SKILL_URI).unwrap().contains("name: operate-soul2d"));
        assert!(skill(&format!("{SKILL_FAMILY_PREFIX}editor-state")).unwrap().contains("editor_selection_set"));
        assert_eq!(skill(&format!("{SKILL_FAMILY_PREFIX}nope")), None);
        assert_eq!(skill("resource://soul2d/skill/other"), None);
        assert_eq!(guide(SKILL_URI), None, "the skill is not a D guide");
    }
}
