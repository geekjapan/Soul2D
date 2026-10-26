use serde_json::{json, Value};
use soul2d_core::selector::{find, find_response};

/// Root(1) > Body(2) > { Face(3) > Eye(8, Part), Grid(4, GridDeformer), PartA(5, Part) }, Other(6) > Part(7).
/// ParamA(9) sits in group Faces(20) and drives Face; ParamB(10) is ungrouped and drives Grid.
fn model() -> Value {
    json!({
        "nodes": {
            "uuid": 1, "name": "Root", "type": "Node", "enabled": true,
            "children": [
                {"uuid": 2, "name": "Body", "type": "Node", "children": [
                    {"uuid": 3, "name": "Face", "type": "Node", "children": [
                        {"uuid": 8, "name": "瞳", "type": "Part"},
                    ]},
                    {"uuid": 4, "name": "Grid", "type": "GridDeformer"},
                    {"uuid": 5, "name": "PartA", "type": "Part"},
                ]},
                {"uuid": 6, "name": "Other", "type": "Node", "children": [
                    {"uuid": 7, "name": "目::R::MG", "type": "Part"},
                ]},
            ],
        },
        "param": [
            {"groupUUID": 20, "name": "Faces", "children": [
                {"uuid": 9, "name": "ParamA", "bindings": [
                    {"node": 3, "param_name": "transform.t.x", "values": []},
                ]},
            ]},
            {"uuid": 10, "name": "ParamB", "bindings": [
                {"node": 4, "param_name": "a b", "values": []},
            ]},
        ],
    })
}

fn run(selector: &str) -> Vec<Value> {
    find(&model(), selector).unwrap_or_else(|e| panic!("{selector:?} failed: {e}"))
}

fn uuids(selector: &str) -> Vec<u64> {
    run(selector).iter().map(|r| r["uuid"].as_u64().unwrap()).collect()
}

fn names(selector: &str) -> Vec<String> {
    run(selector).iter().map(|r| r["name"].as_str().unwrap().to_string()).collect()
}

fn rejected(selector: &str) -> String {
    match find(&model(), selector) {
        Ok(found) => panic!("{selector:?} should be rejected but matched {found:?}"),
        Err(message) => message,
    }
}

// Cases from source/soul2d_tests/regression.d `testSelectorQueryAndTreeStore`.

#[test]
fn star_lists_groups_parameters_then_nodes_without_the_root() {
    assert_eq!(
        names("*"),
        ["Faces", "ParamA", "ParamB", "Body", "Face", "瞳", "Grid", "PartA", "Other", "目::R::MG"]
    );
}

#[test]
fn direct_child_combinator() {
    let found = run("Node.Body > GridDeformer.Grid");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["name"], "Grid");
    assert_eq!(found[0]["typeId"], "GridDeformer");
    assert_eq!(found[0]["kind"], "node");
}

#[test]
fn attribute_name_query() {
    assert_eq!(uuids(r#"*[name="Face"]"#), [3]);
}

#[test]
fn node_to_bound_parameter() {
    let found = run("Node.Face Parameter");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["name"], "ParamA");
    assert_eq!(found[0]["kind"], "parameter");
    assert_eq!(found[0]["uri"], "resource://soul2d/resources/9");
}

#[test]
fn node_to_bound_binding() {
    let found = run("Node.Face Binding");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["kind"], "binding");
    assert_eq!(found[0]["typeId"], "Binding");
    assert_eq!(found[0]["name"], "transform.t.x");
    assert_eq!(found[0]["bindingName"], "transform.t.x");
    assert_eq!(found[0]["parameter"], json!({"uuid": 9, "name": "ParamA"}));
    assert_eq!(found[0]["target"], json!({"uuid": 3, "name": "Face", "typeId": "Node"}));
    assert_eq!(found[0]["uri"], "resource://soul2d/bindings/get?parameter=9&target=3&name=transform.t.x");
}

#[test]
fn parameter_to_binding() {
    let found = run("Parameter.ParamA Binding");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["kind"], "binding");
    assert_eq!(found[0]["name"], "transform.t.x");
}

#[test]
fn binding_name_is_percent_encoded_in_its_uri() {
    let found = run("Parameter.ParamB Binding");
    assert_eq!(found[0]["uri"], "resource://soul2d/bindings/get?parameter=10&target=4&name=a%20b");
}

#[test]
fn response_nests_a_match_under_its_matched_ancestor() {
    let response = find_response(&model(), "Node.Body, GridDeformer.Grid").unwrap();
    let items = response["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], "Body");
    assert_eq!(items[0]["uuid"], 2);
    assert_eq!(items[0]["uri"], "resource://soul2d/resources/2");
    assert_eq!(items[0]["data"], json!({"uuid": 2, "name": "Body", "type": "Node"}));
    let children = items[0]["children"].as_array().unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0]["name"], "Grid");
    assert_eq!(children[0]["children"], json!([]));
    assert_eq!(response["selectorSyntax"], "soul2d.core.selector");
    assert_eq!(response["recommendedDiscoveryUri"], "resource://soul2d/resources/find?selector=*");
    assert_eq!(
        response["guideUris"],
        json!([
            "resource://soul2d/guides/find",
            "resource://soul2d/guides/selectors",
            "resource://soul2d/guides/resources",
        ])
    );
}

#[test]
fn response_for_star_nests_parameters_under_group_and_nodes_under_nodes() {
    let response = find_response(&model(), "*").unwrap();
    let items = response["items"].as_array().unwrap();
    let roots: Vec<&str> = items.iter().map(|i| i["name"].as_str().unwrap()).collect();
    assert_eq!(roots, ["Faces", "ParamB", "Body", "Other"]);
    assert_eq!(items[0]["children"][0]["name"], "ParamA");
    assert_eq!(items[2]["children"][0]["name"], "Face");
    assert_eq!(items[2]["children"][0]["children"][0]["name"], "瞳");
}

// Documented examples (selectors/guide and resources/find prompts).

#[test]
fn type_selector_is_an_exact_type_id() {
    assert_eq!(uuids("Node"), [2, 3, 6]);
    assert_eq!(uuids("Part"), [8, 5, 7]);
    assert_eq!(uuids("Parameter"), [20, 9, 10]);
    assert_eq!(uuids("Nothing"), Vec::<u64>::new());
}

#[test]
fn node_child_part() {
    assert_eq!(uuids("Node > Part"), [5, 8, 7]);
}

#[test]
fn quoted_name_selectors_with_non_ascii_text() {
    assert_eq!(uuids(r#"Part."瞳""#), [8]);
    assert_eq!(uuids(r#"Part."目::R::MG""#), [7]);
    assert_eq!(uuids(r#"Part."目::R::MG" Part."瞳""#), Vec::<u64>::new());
}

#[test]
fn uuid_selectors_and_attributes() {
    assert_eq!(uuids("Node#3"), [3]);
    assert_eq!(uuids("#2"), [2]);
    assert_eq!(uuids(r#"Node#3[name="Face"]"#), [3]);
    assert_eq!(uuids(r#"Node#3[name="Nope"]"#), Vec::<u64>::new());
    assert_eq!(uuids("*[uuid=3]"), [3]);
    assert_eq!(uuids("*[typeId=Part]"), [8, 5, 7]);
    assert_eq!(uuids("*[typeId=GridDeformer][name=Grid]"), [4]);
    assert_eq!(uuids(r#".Face"#), [3]);
}

#[test]
fn descendant_combinator_walks_the_whole_subtree() {
    assert_eq!(uuids("Node.Body Part"), [8, 5]);
    assert_eq!(uuids("Node.Body > Part"), [5]);
    assert_eq!(uuids("Node.Body Node.Face > Part"), [8]);
}

#[test]
fn comma_unions_keep_order_and_drop_duplicates() {
    assert_eq!(uuids("Part, Parameter"), [8, 5, 7, 20, 9, 10]);
    assert_eq!(uuids("Node.Body, Node.Body"), [2]);
    assert_eq!(uuids("Node.Body,"), [2]);
}

#[test]
fn bare_binding_lists_bindings_of_every_node() {
    assert_eq!(names("Binding"), ["transform.t.x", "a b"]);
}

#[test]
fn element_after_attributes_starts_a_descendant_element() {
    // The D grammar ends an element at its attributes; `.PartA` is then a new element below Body.
    assert_eq!(uuids(r#"Node[name="Body"] .PartA"#), [5]);
    assert_eq!(uuids(r#"Node[name="Body"].PartA"#), [5]);
}

#[test]
fn id_followed_by_dot_name_is_not_a_fraction() {
    assert_eq!(uuids("Node#3.Face"), [3]);
}

#[test]
fn whitespace_is_free_between_tokens() {
    assert_eq!(uuids("  Node . Body   >   GridDeformer . Grid "), [4]);
}

#[test]
fn missing_document_parts_match_nothing() {
    assert_eq!(find(&json!({}), "*").unwrap(), Vec::<Value>::new());
    assert_eq!(find(&Value::Null, "Node").unwrap(), Vec::<Value>::new());
    assert_eq!(find(&json!({"nodes": {"uuid": 1}}), "*").unwrap(), Vec::<Value>::new());
}

#[test]
fn deep_documents_do_not_overflow_the_stack() {
    // Built by moving values; json! would serialize the nested value recursively.
    let mut node = json!({"uuid": 100_000, "name": "leaf", "type": "Part"});
    for depth in (2..20_000_u64).rev() {
        let mut parent = serde_json::Map::new();
        parent.insert("uuid".into(), json!(depth));
        parent.insert("type".into(), json!("Node"));
        parent.insert("children".into(), Value::Array(vec![node]));
        node = Value::Object(parent);
    }
    let mut root = serde_json::Map::new();
    root.insert("uuid".into(), json!(1));
    root.insert("children".into(), Value::Array(vec![node]));
    let mut model = serde_json::Map::new();
    model.insert("nodes".into(), Value::Object(root));
    let model = Value::Object(model);
    assert_eq!(find(&model, "Node Part").unwrap().len(), 1);
    assert_eq!(find(&model, "Node").unwrap().len(), 19_998);
    // Dropping a value this deep is itself recursive in serde_json, so the test leaks it.
    std::mem::forget(model);
}

// Malformed or unsupported selectors.

#[test]
fn rejects_unterminated_string() {
    assert!(rejected(r#"Part."Eye"#).contains("unterminated"));
}

#[test]
fn rejects_unknown_pseudo_class() {
    assert!(rejected("Node:hover").contains("unknown pseudo-class ':hover'"));
    assert!(rejected("Node:nth-child(10, 0)").contains("unknown pseudo-class"));
    assert!(rejected("Node:").contains("pseudo-class name"));
}

#[test]
fn rejects_empty_selector() {
    assert!(rejected("").contains("empty"));
    assert!(rejected("   ").contains("empty"));
}

#[test]
fn rejects_stray_combinators() {
    assert!(rejected("Node >").contains("right"));
    assert!(rejected("> Node").contains("left"));
    assert!(rejected("Node > > Part").contains("expected a type"));
    assert!(rejected("Node > , Part").contains("expected a type"));
}

#[test]
fn rejects_stray_commas_and_garbage() {
    rejected(",Node");
    rejected("Node,,Part");
    rejected("Node Part)");
    rejected("Node@");
    rejected("Node == Part");
    rejected("[name=x]");
    rejected("Node[name]");
    rejected("Node[name=]");
    rejected("Node[name=x");
    rejected("Node[name=x]:active");
    rejected("Node.");
    rejected("Node#");
}

#[test]
fn rejects_unsupported_attributes_and_bad_uuids() {
    assert!(rejected("Node[enabled=true]").contains("unsupported attribute 'enabled'"));
    assert!(rejected("Node#abc").contains("uuid"));
    assert!(rejected("Node#99999999999").contains("uuid"));
    assert!(rejected("Node[uuid=1.5]").contains("uuid"));
}

#[test]
fn active_needs_the_armed_parameter_which_the_model_lacks() {
    assert!(rejected("Binding:active").contains("armed parameter"));
    assert!(rejected("Node.Face Binding:active").contains("armed parameter"));
    assert!(rejected("Node:active(1)").contains("no arguments"));
    // Outside a Binding element D ignores the pseudo-class, so it is harmless there.
    assert_eq!(uuids("Node.Body:active"), [2]);
}

#[test]
fn caps_selector_size() {
    let long = "Node ".repeat(1000);
    assert!(rejected(&long).contains("longer than"));
    let many = "A ".repeat(129);
    assert!(rejected(&many).contains("more than 128 elements"));
    let fits = "A ".repeat(128);
    assert_eq!(find(&model(), &fits).unwrap().len(), 0);
}
