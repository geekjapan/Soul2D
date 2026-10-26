use serde_json::{json, Value};
use soul2d_core::atlas::{Rect, TexturePacker};
use soul2d_core::Session;

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn result(response: &Value) -> &Value {
    assert_eq!(response["succeeded"], true, "request failed: {response}");
    &response["result"]
}

fn rect(x: u32, y: u32, width: u32, height: u32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

fn in_bounds(rect: Rect, width: u32, height: u32) -> bool {
    rect.width > 0
        && rect.height > 0
        && u64::from(rect.x) + u64::from(rect.width) <= u64::from(width)
        && u64::from(rect.y) + u64::from(rect.height) <= u64::from(height)
}

fn disjoint(a: Rect, b: Rect) -> bool {
    u64::from(a.x) + u64::from(a.width) <= u64::from(b.x)
        || u64::from(b.x) + u64::from(b.width) <= u64::from(a.x)
        || u64::from(a.y) + u64::from(a.height) <= u64::from(b.y)
        || u64::from(b.y) + u64::from(b.height) <= u64::from(a.y)
}

#[test]
fn four_squares_pack_in_split_order_and_fifth_fails_cleanly() {
    let mut packer = TexturePacker::new(4, 4).unwrap();
    assert_eq!(packer.size(), [4, 4]);
    assert_eq!(packer.pack(2, 2), Some(rect(0, 0, 2, 2)));
    assert_eq!(packer.occupancy(), 0.25);
    assert_eq!(packer.pack(2, 2), Some(rect(0, 2, 2, 2)));
    assert_eq!(packer.occupancy(), 0.5);
    assert_eq!(packer.pack(2, 2), Some(rect(2, 0, 2, 2)));
    assert_eq!(packer.pack(2, 2), Some(rect(2, 2, 2, 2)));
    assert_eq!(packer.occupancy(), 1.0);
    assert_eq!(packer.pack(2, 2), None);
    assert_eq!(packer.pack(1, 1), None);
    assert_eq!(packer.occupancy(), 1.0);
}

#[test]
fn rotation_is_never_attempted() {
    let mut packer = TexturePacker::new(2, 3).unwrap();
    assert_eq!(packer.pack(3, 2), None);
    assert_eq!(packer.occupancy(), 0.0);
    assert_eq!(packer.pack(2, 3), Some(rect(0, 0, 2, 3)));
}

#[test]
fn zero_dimensions_are_rejected_without_state_changes() {
    assert!(TexturePacker::new(0, 4).is_err());
    assert!(TexturePacker::new(4, 0).is_err());
    assert!(TexturePacker::new(0, 0).is_err());
    let mut packer = TexturePacker::new(4, 4).unwrap();
    assert_eq!(packer.pack(0, 2), None);
    assert_eq!(packer.pack(2, 0), None);
    assert_eq!(packer.occupancy(), 0.0);
    assert_eq!(packer.pack(4, 4), Some(rect(0, 0, 4, 4)));
    assert_eq!(packer.occupancy(), 1.0);
}

#[test]
fn u32_max_bin_uses_wide_area_math() {
    let mut giant = TexturePacker::new(u32::MAX, u32::MAX).unwrap();
    assert_eq!(giant.size(), [u32::MAX, u32::MAX]);
    assert_eq!(
        giant.pack(u32::MAX, u32::MAX),
        Some(rect(0, 0, u32::MAX, u32::MAX))
    );
    assert_eq!(giant.occupancy(), 1.0);
    let mut sparse = TexturePacker::new(u32::MAX, u32::MAX).unwrap();
    assert!(sparse.pack(1, 1).is_some());
    let coverage = sparse.occupancy();
    assert!(coverage > 0.0 && coverage < 1e-18, "{coverage}");
}

#[test]
fn clear_restores_the_empty_bin() {
    let mut packer = TexturePacker::new(4, 4).unwrap();
    for _ in 0..4 {
        packer.pack(2, 2);
    }
    assert_eq!(packer.pack(1, 1), None);
    packer.clear();
    assert_eq!(packer.occupancy(), 0.0);
    assert_eq!(packer.pack(2, 2), Some(rect(0, 0, 2, 2)));
}

#[test]
fn remove_frees_only_live_rects_and_repacks_reuse_them() {
    let mut packer = TexturePacker::new(4, 4).unwrap();
    let first = packer.pack(2, 2).unwrap();
    let second = packer.pack(2, 2).unwrap();
    let third = packer.pack(2, 2).unwrap();
    let fourth = packer.pack(2, 2).unwrap();
    assert_eq!(packer.remove(rect(9, 9, 1, 1)), false);
    assert_eq!(packer.remove(rect(0, 0, 2, 3)), false);
    assert_eq!(packer.occupancy(), 1.0);
    assert_eq!(packer.remove(first), true);
    assert_eq!(packer.occupancy(), 0.75);
    assert_eq!(packer.remove(first), false);
    let reused = packer.pack(2, 2).unwrap();
    assert_eq!(reused, first);
    assert!(disjoint(reused, second) && disjoint(reused, third) && disjoint(reused, fourth));
    assert_eq!(packer.occupancy(), 1.0);
}

#[test]
fn varied_sequence_stays_in_bounds_and_disjoint_through_removals() {
    let mut packer = TexturePacker::new(32, 32).unwrap();
    let sizes = [
        (8, 8),
        (16, 4),
        (4, 16),
        (8, 4),
        (4, 4),
        (10, 6),
        (6, 10),
        (12, 3),
        (3, 12),
        (5, 5),
        (7, 2),
        (2, 7),
    ];
    let mut live = Vec::new();
    for (width, height) in sizes {
        if let Some(rect) = packer.pack(width, height) {
            assert!(in_bounds(rect, 32, 32), "{rect:?}");
            for other in &live {
                assert!(disjoint(rect, *other), "{rect:?} overlaps {other:?}");
            }
            live.push(rect);
        }
    }
    assert!(live.len() >= 6, "{live:?}");
    let removed: Vec<Rect> = live.drain(2..5).collect();
    for rect in removed {
        assert!(packer.remove(rect));
        assert!(!packer.remove(rect));
    }
    for (width, height) in [(4, 4), (2, 2), (6, 3), (3, 6), (1, 8), (8, 1)] {
        if let Some(rect) = packer.pack(width, height) {
            assert!(in_bounds(rect, 32, 32), "{rect:?}");
            for other in &live {
                assert!(disjoint(rect, *other), "{rect:?} overlaps {other:?}");
            }
            live.push(rect);
        }
    }
    for rect in &live {
        for other in &live {
            if rect != other {
                assert!(disjoint(*rect, *other), "{rect:?} overlaps {other:?}");
            }
        }
    }
    packer.clear();
    assert_eq!(packer.pack(32, 32), Some(rect(0, 0, 32, 32)));
}

#[test]
fn occupancy_reports_fractional_coverage() {
    let mut packer = TexturePacker::new(8, 4).unwrap();
    assert_eq!(packer.pack(4, 2), Some(rect(0, 0, 4, 2)));
    assert_eq!(packer.occupancy(), 0.25);
    assert_eq!(packer.pack(4, 2), Some(rect(4, 0, 4, 2)));
    assert_eq!(packer.occupancy(), 0.5);
    assert_eq!(packer.pack(2, 2), Some(rect(0, 2, 2, 2)));
    assert_eq!(packer.occupancy(), 0.625);
}

#[test]
fn session_atlas_pack_is_readonly_and_validates_everything_first() {
    let mut session = Session::new();
    let before = session.snapshot();
    let response = request(
        &mut session,
        json!({"op":"atlas.pack","size":[4,4],
            "rectangles":[[2,2],[2,2],[2,2],[2,2],[2,2]]}),
    );
    assert_eq!(result(&response)["size"], json!([4, 4]));
    assert_eq!(
        result(&response)["placements"],
        json!([[0, 0, 2, 2], [0, 2, 2, 2], [2, 0, 2, 2], [2, 2, 2, 2], null])
    );
    assert_eq!(result(&response)["occupancy"], 1.0);
    assert_eq!(session.snapshot(), before);

    let response = request(
        &mut session,
        json!({"op":"atlas.pack","size":[4,4],"rectangles":[[2,2],[8,8]]}),
    );
    assert_eq!(
        result(&response)["placements"],
        json!([[0, 0, 2, 2], null])
    );
    assert_eq!(session.snapshot(), before);

    for invalid in [
        json!({"op":"atlas.pack"}),
        json!({"op":"atlas.pack","size":[4,4]}),
        json!({"op":"atlas.pack","size":[4],"rectangles":[[2,2]]}),
        json!({"op":"atlas.pack","size":[4,4],"rectangles":[]}),
        json!({"op":"atlas.pack","size":[4,4],"rectangles":{}}),
        json!({"op":"atlas.pack","size":[0,4],"rectangles":[[2,2]]}),
        json!({"op":"atlas.pack","size":[40000,4],"rectangles":[[2,2]]}),
        json!({"op":"atlas.pack","size":[4.5,4],"rectangles":[[2,2]]}),
        json!({"op":"atlas.pack","size":[4,4],"rectangles":[[2,2],"bad"]}),
        json!({"op":"atlas.pack","size":[4,4],"rectangles":[[2,2],[0,2]]}),
        json!({"op":"atlas.pack","size":[4,4],"rectangles":[[2,2],[2]]}),
        json!({"op":"atlas.pack","size":[4,4],"rectangles":[[2,2],[2,-1]]}),
        json!({"op":"atlas.pack","size":[4,4],"rectangles":[[2,2],[4294967296u64,1]]}),
    ] {
        assert_eq!(request(&mut session, invalid.clone())["succeeded"], false, "{invalid}");
        assert_eq!(session.snapshot(), before);
    }
}

#[test]
fn session_atlas_pack_enforces_request_caps() {
    let mut session = Session::new();
    let before = session.snapshot();

    let over = vec![json!([1, 1]); 4097];
    let response = request(
        &mut session,
        json!({"op":"atlas.pack","size":[4,4],"rectangles":over}),
    );
    assert_eq!(response["succeeded"], false);
    assert_eq!(session.snapshot(), before);

    let full = vec![json!([1, 1]); 4096];
    let response = request(
        &mut session,
        json!({"op":"atlas.pack","size":[4,4],"rectangles":full}),
    );
    let placements = result(&response)["placements"].as_array().unwrap();
    assert_eq!(placements.len(), 4096);
    assert_eq!(placements[0], json!([0, 0, 1, 1]));
    assert!(placements[..16].iter().all(|placement| !placement.is_null()));
    assert!(placements[16..].iter().all(Value::is_null));
    assert_eq!(result(&response)["occupancy"], 1.0);
    assert_eq!(session.snapshot(), before);

    let mut capped = vec![json!([1, 1]); 4096];
    capped[4095] = json!([4294967296u64, 1]);
    let response = request(
        &mut session,
        json!({"op":"atlas.pack","size":[4,4],"rectangles":capped}),
    );
    assert_eq!(response["succeeded"], false);
    assert_eq!(session.snapshot(), before);
}

#[test]
fn mcp_atlas_pack_reports_schema_and_shared_state() {
    let mut session = Session::new();
    let listed = session
        .handle_mcp(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .unwrap();
    let tools = listed["result"]["tools"].as_array().unwrap();
    let tool = tools
        .iter()
        .find(|tool| tool["name"] == "atlas_pack")
        .expect("atlas_pack is not listed");
    assert_eq!(
        tool["inputSchema"]["required"],
        json!(["size", "rectangles"])
    );
    assert_eq!(
        tool["inputSchema"]["properties"]["size"],
        json!({"type":"array","items":{"type":"integer","minimum":1,"maximum":32768},
            "minItems":2,"maxItems":2})
    );
    assert_eq!(
        tool["inputSchema"]["properties"]["rectangles"],
        json!({"type":"array","minItems":1,"maxItems":4096,
            "items":{"type":"array","minItems":2,"maxItems":2,
                "items":{"type":"integer","minimum":1,"maximum":4294967295u64}}})
    );

    let before = session.snapshot();
    let called = session
        .handle_mcp(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
            "name":"atlas_pack",
            "arguments":{"size":[4,4],"rectangles":[[2,2],[2,2],[2,2],[2,2],[2,2]]},
        }}))
        .unwrap();
    assert_eq!(called["result"]["isError"], false);
    assert_eq!(
        called["result"]["structuredContent"]["placements"],
        json!([[0, 0, 2, 2], [0, 2, 2, 2], [2, 0, 2, 2], [2, 2, 2, 2], null])
    );
    assert_eq!(
        called["result"]["structuredContent"]["occupancy"],
        json!(1.0)
    );
    assert_eq!(session.snapshot(), before);

    let read = session
        .handle_mcp(&json!({"jsonrpc":"2.0","id":3,"method":"resources/read","params":{
            "uri":"resource://vsoul/snapshot",
        }}))
        .unwrap();
    let text = read["result"]["contents"][0]["text"].as_str().unwrap();
    let snapshot: Value = serde_json::from_str(text).unwrap();
    assert_eq!(snapshot, session.snapshot());
}
