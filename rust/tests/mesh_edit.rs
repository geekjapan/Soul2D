mod common;

use serde_json::{json, Value};
use soul2d_core::mesh_edit::{
    apply_edits, brush_stroke, build_mesh, combine_selection, deform_positions, extract_triangles, grid_axes,
    normalize_uvs, parse_edit, path_deform, segments_intersect, select_lasso, select_rect, Brush, DeformTool, Edit,
    EditMesh, EditOptions, InflateSample, Mirror, SelectMode, inflate_stroke, MAX_INFLATE_SAMPLES,
};
use soul2d_core::Session;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

const NO_TEXTURE: u32 = u32::MAX;

fn quad() -> EditMesh {
    EditMesh::from_triangles(
        &[[-8.0, -8.0], [8.0, -8.0], [8.0, 8.0], [-8.0, 8.0]],
        &[0, 1, 2, 0, 2, 3],
        [0.0, 0.0],
    )
    .unwrap()
}

fn edit(mesh: &mut EditMesh, edits: Value) {
    let edits: Vec<Edit> = edits
        .as_array()
        .unwrap()
        .iter()
        .map(|e| parse_edit(e).unwrap())
        .collect();
    apply_edits(mesh, &edits, &EditOptions::default()).unwrap();
}

fn triangle_set(mesh: &EditMesh) -> Vec<[usize; 3]> {
    let mut set: Vec<[usize; 3]> = extract_triangles(&mesh.verts)
        .into_iter()
        .map(|mut t| {
            t.sort_unstable();
            t
        })
        .collect();
    set.sort_unstable();
    set
}

fn close(a: [f64; 2], b: [f64; 2], tolerance: f64) -> bool {
    (a[0] - b[0]).abs() <= tolerance && (a[1] - b[1]).abs() <= tolerance
}

// ------------------------------------------------------- IncMesh model

#[test]
fn import_connects_each_triangle_in_order_and_export_returns_the_same_triangles() {
    let mesh = quad();
    let connections: Vec<Vec<usize>> = mesh.verts.iter().map(|v| v.connections.clone()).collect();
    assert_eq!(connections, vec![vec![1, 2, 3], vec![0, 2], vec![1, 0, 3], vec![2, 0]]);
    assert_eq!(extract_triangles(&mesh.verts), vec![[0, 1, 2], [0, 2, 3]]);
}

#[test]
fn a_quad_is_a_two_by_two_grid_so_apply_sorts_it_and_reports_the_axes() {
    let built = build_mesh(&quad(), None).unwrap();
    assert_eq!(built.vertices, vec![[-8.0, -8.0], [8.0, -8.0], [-8.0, 8.0], [8.0, 8.0]]);
    assert_eq!(built.indices, vec![0, 1, 3, 0, 3, 2]);
    assert_eq!(built.grid_axes, Some([vec![-8.0, 8.0], vec![-8.0, 8.0]]));
    assert!(built.permuted);
    assert_eq!(built.uvs, vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]]);
    assert_eq!(grid_axes(&[[0.0, 0.0], [1.0, 0.0], [2.0, 1.0], [3.0, 1.0]]), None);
}

#[test]
fn texture_uvs_are_position_over_size_and_the_fallback_rescales_the_bounds() {
    let points = [[-8.0, -8.0], [8.0, -8.0], [8.0, 8.0]];
    assert_eq!(
        normalize_uvs(&points, Some([64, 32])),
        vec![[0.375, 0.25], [0.625, 0.25], [0.625, 0.75]]
    );
    assert_eq!(normalize_uvs(&points, None), vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]);
    // A zero-size texture counts as missing, a flat extent maps to the middle.
    assert_eq!(
        normalize_uvs(&points, Some([0, 64])),
        vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]
    );
    assert_eq!(
        normalize_uvs(&[[1.0, 1.0], [2.0, 1.0]], None),
        vec![[0.5, 0.5], [0.5, 0.5]]
    );
}

#[test]
fn winding_is_made_counter_clockwise() {
    let mesh = EditMesh::from_triangles(
        &[[0.0, 0.0], [0.0, 4.0], [4.0, 0.0], [9.0, 9.0]],
        &[0, 1, 2],
        [0.0, 0.0],
    )
    .unwrap();
    // (0,0) -> (0,4) -> (4,0) is clockwise, so the last two corners swap.
    assert_eq!(build_mesh(&mesh, None).unwrap().indices, vec![0, 2, 1]);
}

// ------------------------------------------------------ point and edge tools

#[test]
fn add_appends_a_vertex_and_connect_builds_triangles_from_the_graph() {
    let mut mesh = quad();
    edit(
        &mut mesh,
        json!([
            {"op": "disconnect", "a": 0, "b": 2},
            {"op": "add", "pos": [0, 0]},
            {"op": "connect", "a": 4, "b": 0},
            {"op": "connect", "a": 4, "b": 1},
            {"op": "connect", "a": 4, "b": 2},
            {"op": "connect", "a": 4, "b": 3},
        ]),
    );
    assert_eq!(mesh.verts.len(), 5);
    assert_eq!(mesh.verts[4].position, [0.0, 0.0]);
    assert_eq!(triangle_set(&mesh), vec![[0, 1, 4], [0, 3, 4], [1, 2, 4], [2, 3, 4]]);
}

#[test]
fn a_dense_graph_keeps_overlapping_triangles_like_d() {
    // The diagonal stays: D's intersection filter never fires, so the centre also forms 0-2-4.
    let mut mesh = quad();
    edit(
        &mut mesh,
        json!([
            {"op": "add", "pos": [0, 0], "connect": 0},
            {"op": "connect", "a": 4, "b": 1},
            {"op": "connect", "a": 4, "b": 2},
            {"op": "connect", "a": 4, "b": 3},
        ]),
    );
    assert_eq!(triangle_set(&mesh).len(), 7);
}

#[test]
fn edge_cut_removes_only_the_edges_the_segment_crosses_inside() {
    let mut mesh = quad();
    edit(&mut mesh, json!([{"op": "cut", "from": [-10, 10], "to": [10, -10]}]));
    // The segment crosses the diagonal; it only touches the quad corner (8,-8) and (-8,8) at an
    // edge end, which the 1e-5 margin excludes.
    assert!(!mesh.verts[0].connections.contains(&2));
    assert_eq!(mesh.verts[0].connections, vec![1, 3]);
    assert!(extract_triangles(&mesh.verts).is_empty());
    assert!(segments_intersect(
        [-10.0, 10.0],
        [10.0, -10.0],
        [-8.0, -8.0],
        [8.0, 8.0]
    ));
    assert!(!segments_intersect([0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]));
    assert!(!segments_intersect([0.0, 0.0], [2.0, 0.0], [1.0, 0.0], [1.0, 0.0]));
}

#[test]
fn toggle_disconnects_a_connected_pair_and_connects_it_again() {
    let mut mesh = quad();
    edit(&mut mesh, json!([{"op": "toggle", "a": 0, "b": 2}]));
    assert!(!mesh.verts[0].connections.contains(&2));
    edit(&mut mesh, json!([{"op": "toggle", "a": 0, "b": 2}]));
    assert!(mesh.verts[0].connections.contains(&2));
    assert!(mesh.verts[2].connections.contains(&0));
}

#[test]
fn remove_deletes_connections_and_renumbers() {
    let mut mesh = quad();
    edit(
        &mut mesh,
        json!([{"op": "connect", "a": 1, "b": 3}, {"op": "remove", "indices": [2]}]),
    );
    assert_eq!(mesh.positions(), vec![[-8.0, -8.0], [8.0, -8.0], [-8.0, 8.0]]);
    assert_eq!(mesh.verts[1].connections, vec![0, 2]);
    assert_eq!(triangle_set(&mesh), vec![[0, 1, 2]]);
}

#[test]
fn remove_at_takes_the_first_vertex_under_the_point_and_empty_space_is_an_error() {
    // At zoom 1 the radius is 16, so (7,-7) is under vertex 0 first, not the nearer vertex 1.
    let mut mesh = quad();
    edit(&mut mesh, json!([{"op": "remove", "at": [7, -7]}]));
    assert_eq!(mesh.positions(), vec![[8.0, -8.0], [8.0, 8.0], [-8.0, 8.0]]);
    let zoomed = EditOptions {
        zoom: 8.0,
        ..EditOptions::default()
    };
    let mut mesh = quad();
    apply_edits(
        &mut mesh,
        &[parse_edit(&json!({"op": "remove", "at": [7, -7]})).unwrap()],
        &zoomed,
    )
    .unwrap();
    assert_eq!(mesh.positions(), vec![[-8.0, -8.0], [8.0, 8.0], [-8.0, 8.0]]);
    let miss = [parse_edit(&json!({"op": "remove", "at": [100, 100]})).unwrap()];
    assert!(apply_edits(&mut quad(), &miss, &EditOptions::default())
        .unwrap_err()
        .contains("no vertex"));
}

#[test]
fn move_translates_the_listed_vertices() {
    let mut mesh = quad();
    edit(&mut mesh, json!([{"op": "move", "indices": [1, 2], "delta": [2, 3]}]));
    assert_eq!(
        mesh.positions(),
        vec![[-8.0, -8.0], [10.0, -5.0], [10.0, 11.0], [-8.0, 8.0]]
    );
}

#[test]
fn mirror_mode_adds_partners_first_and_snaps_to_the_axis() {
    let mirrored = EditOptions {
        mirror: Mirror {
            horizontal: true,
            ..Mirror::default()
        },
        ..EditOptions::default()
    };
    let mut mesh = EditMesh::from_triangles(&[], &[], [0.0, 0.0]).unwrap();
    let add = [parse_edit(&json!({"op": "add", "pos": [4, 6]})).unwrap()];
    apply_edits(&mut mesh, &add, &mirrored).unwrap();
    assert_eq!(mesh.positions(), vec![[-4.0, 6.0], [4.0, 6.0]]);
    // Within 4 units of the axis one vertex lands exactly on it.
    let near = [parse_edit(&json!({"op": "add", "pos": [2, 9]})).unwrap()];
    apply_edits(&mut mesh, &near, &mirrored).unwrap();
    assert_eq!(mesh.positions()[2], [0.0, 9.0]);
    assert_eq!(mesh.verts.len(), 3);
}

#[test]
fn mirror_mode_moves_the_partner_with_a_mirrored_delta_and_connects_pairs() {
    // Zoom 8 shrinks the select radius to 2 units so partners are found by position.
    let mirrored = EditOptions {
        mirror: Mirror {
            horizontal: true,
            ..Mirror::default()
        },
        zoom: 8.0,
        ..EditOptions::default()
    };
    let mut mesh = EditMesh::from_triangles(
        &[[-8.0, -8.0], [8.0, -8.0], [-8.0, 8.0], [8.0, 8.0]],
        &[0, 1, 3],
        [0.0, 0.0],
    )
    .unwrap();
    // Selecting both a vertex and its partner moves each side once.
    let moves = [parse_edit(&json!({"op": "move", "indices": [1, 0], "delta": [2, 3]})).unwrap()];
    apply_edits(&mut mesh, &moves, &mirrored).unwrap();
    assert_eq!(mesh.positions()[1], [10.0, -5.0]);
    assert_eq!(mesh.positions()[0], [-10.0, -5.0]);
    let connect = [parse_edit(&json!({"op": "connect", "a": 1, "b": 3})).unwrap()];
    apply_edits(&mut mesh, &connect, &mirrored).unwrap();
    assert!(
        mesh.verts[0].connections.contains(&2),
        "the partner pair 0-2 is linked too"
    );
}

#[test]
fn auto_connect_links_each_add_to_the_previous_one() {
    let options = EditOptions {
        auto_connect: true,
        ..EditOptions::default()
    };
    let mut mesh = EditMesh::from_triangles(&[], &[], [0.0, 0.0]).unwrap();
    let adds: Vec<Edit> = [[0, 0], [100, 0], [0, 100]]
        .iter()
        .map(|p| parse_edit(&json!({"op": "add", "pos": p})).unwrap())
        .collect();
    apply_edits(&mut mesh, &adds, &options).unwrap();
    let connections: Vec<Vec<usize>> = mesh.verts.iter().map(|v| v.connections.clone()).collect();
    assert_eq!(connections, vec![vec![1], vec![0, 2], vec![1]]);
}

#[test]
fn flip_negates_one_axis_and_triangulate_rebuilds_connections() {
    let mut mesh = EditMesh::from_triangles(
        &[[-8.0, -8.0], [8.0, -8.0], [8.0, 8.0], [-8.0, 8.0], [0.0, 0.0]],
        &[],
        [0.0, 0.0],
    )
    .unwrap();
    edit(&mut mesh, json!([{"op": "triangulate"}]));
    assert_eq!(triangle_set(&mesh), vec![[0, 1, 4], [0, 3, 4], [1, 2, 4], [2, 3, 4]]);
    edit(&mut mesh, json!([{"op": "flip", "axis": "v"}]));
    assert_eq!(mesh.positions()[0], [-8.0, 8.0]);
    edit(&mut mesh, json!([{"op": "flip", "axis": "h"}]));
    assert_eq!(mesh.positions()[0], [8.0, 8.0]);
}

// -------------------------------------------------------------- grid tool

fn grid3() -> EditMesh {
    let mut mesh = EditMesh::from_triangles(&[], &[], [5.0, 5.0]).unwrap();
    edit(&mut mesh, json!([{"op": "grid_create", "bounds": [30, 20, 0, 0]}]));
    mesh
}

#[test]
fn grid_create_builds_three_cuts_in_d_x_pattern() {
    let mesh = grid3();
    assert_eq!(mesh.origin, [0.0, 0.0], "a fresh grid has no origin");
    assert_eq!(
        mesh.positions(),
        vec![
            [0.0, 0.0],
            [15.0, 0.0],
            [30.0, 0.0],
            [0.0, 10.0],
            [15.0, 10.0],
            [30.0, 10.0],
            [0.0, 20.0],
            [15.0, 20.0],
            [30.0, 20.0],
        ]
    );
    assert_eq!(
        triangle_set(&mesh),
        vec![
            [0, 1, 4],
            [0, 3, 4],
            [1, 2, 4],
            [2, 4, 5],
            [3, 4, 7],
            [3, 6, 7],
            [4, 5, 7],
            [5, 7, 8]
        ]
    );
    let built = build_mesh(&mesh, None).unwrap();
    assert_eq!(built.grid_axes, Some([vec![0.0, 10.0, 20.0], vec![0.0, 15.0, 30.0]]));
    assert!(!built.permuted);
    assert_eq!(built.indices.len(), 24);
    for tri in built.indices.chunks(3) {
        let p = |i: u32| built.vertices[i as usize];
        let (a, b, c) = (p(tri[0]), p(tri[1]), p(tri[2]));
        assert!(
            (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]) > 0.0,
            "{tri:?} is clockwise"
        );
    }
}

#[test]
fn grid_axis_edits_follow_the_grid_tool() {
    // A high zoom shrinks the 16 px hit radius to 2 units.
    let zoomed = EditOptions {
        zoom: 8.0,
        ..EditOptions::default()
    };
    let run = |edit: Value| {
        let mut mesh = grid3();
        apply_edits(&mut mesh, &[parse_edit(&edit).unwrap()], &zoomed).unwrap();
        mesh
    };
    // Add: y goes before 20 and x before 15; a point past the last value is dropped.
    let added = run(json!({"op": "grid_add_axis", "pos": [7, 12]}));
    assert_eq!(
        grid_axes(&added.positions()),
        Some([vec![0.0, 10.0, 12.0, 20.0], vec![0.0, 7.0, 15.0, 30.0]])
    );
    let beyond = run(json!({"op": "grid_add_axis", "pos": [40, 25]}));
    assert_eq!(beyond.verts.len(), 9);
    // Remove: the centre drops both axes, an edge vertex only the crossing axis, a corner nothing.
    assert_eq!(run(json!({"op": "grid_remove_axis", "vertex": 4})).verts.len(), 4);
    let edge = run(json!({"op": "grid_remove_axis", "vertex": 1}));
    assert_eq!(
        grid_axes(&edge.positions()),
        Some([vec![0.0, 10.0, 20.0], vec![0.0, 30.0]])
    );
    assert_eq!(run(json!({"op": "grid_remove_axis", "vertex": 0})).verts.len(), 9);
    // Move: an interior vertex moves both axes, a top/bottom edge vertex only X.
    let free = run(json!({"op": "grid_move", "vertex": 4, "to": [16, 12]}));
    assert_eq!(
        grid_axes(&free.positions()),
        Some([vec![0.0, 12.0, 20.0], vec![0.0, 16.0, 30.0]])
    );
    let along = run(json!({"op": "grid_move", "vertex": 1, "to": [18, 5]}));
    assert_eq!(
        grid_axes(&along.positions()),
        Some([vec![0.0, 10.0, 20.0], vec![0.0, 18.0, 30.0]])
    );
}

// ---------------------------------------------------------------- selection

#[test]
fn rect_and_lasso_selection_and_modes() {
    let points = [[0.0, 0.0], [5.0, 5.0], [15.0, 5.0], [5.0, -1.0], [9.0, 9.0]];
    assert_eq!(select_rect(&points, [10.0, 10.0], [0.0, 0.0]).unwrap(), vec![0, 1, 4]);
    // The last lasso point only closes the polygon: it is replaced by the first.
    let lasso = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0], [0.2, 0.1]];
    assert_eq!(select_lasso(&points[1..], &lasso).unwrap(), vec![0, 3]);
    assert!(select_lasso(&points, &lasso[..2]).is_err());
    assert_eq!(combine_selection(&[1, 2], &[2, 3], SelectMode::Replace), vec![2, 3]);
    assert_eq!(combine_selection(&[1, 2], &[2, 3], SelectMode::Add), vec![1, 2, 3]);
    assert_eq!(combine_selection(&[1, 2], &[2, 3], SelectMode::Remove), vec![1]);
    assert_eq!(combine_selection(&[1, 2], &[2, 3], SelectMode::Toggle), vec![1, 3]);
}

// ------------------------------------------------------------------ brushes

#[test]
fn brush_falloffs_match_d_at_known_distances() {
    let circle = Brush::circle(300.0).unwrap();
    assert_eq!(circle.weight_at([0.0, 0.0], [150.0, 0.0]), 0.5);
    assert_eq!(circle.weight_at([0.0, 0.0], [0.0, 0.0]), 1.0);
    assert_eq!(circle.weight_at([0.0, 0.0], [300.0, 0.0]), 0.0);
    assert_eq!(circle.weight_at([0.0, 0.0], [400.0, 0.0]), 0.0);
    let double = Brush::double_threshold(300.0, 100.0).unwrap();
    assert_eq!(double.weight_at([0.0, 0.0], [100.0, 0.0]), 1.0);
    assert_eq!(double.weight_at([0.0, 0.0], [200.0, 0.0]), 0.5);
    assert_eq!(double.weight_at([0.0, 0.0], [300.0, 0.0]), 0.0);
    // The constructor clamps like D: radius >= 1, inner <= radius.
    assert_eq!(
        Brush::double_threshold(0.5, 9.0).unwrap(),
        Brush::DoubleThreshold {
            radius: 1.0,
            inner: 1.0
        }
    );
    // The rectangle falls off over the full width and height, and takes the smaller ratio.
    let rect = Brush::rectangle(300.0, 300.0).unwrap();
    assert_eq!(rect.weight_at([0.0, 0.0], [150.0, 0.0]), 0.5);
    assert_eq!(rect.weight_at([0.0, 0.0], [0.0, 300.0]), 0.0);
    assert_eq!(rect.weight_at([0.0, 0.0], [150.0, 225.0]), 0.25);
    assert!(Brush::circle(0.0).is_err() && Brush::circle(f64::NAN).is_err());
}

#[test]
fn line_brush_falls_off_only_across_its_rotated_strip() {
    for angle in [0.0, std::f64::consts::FRAC_PI_2, std::f64::consts::FRAC_PI_4] {
        let brush = Brush::line(100.0, angle).unwrap();
        let (sin, cos) = angle.sin_cos();
        let center = [13.0, -27.0];
        for along in [0.0, 1000.0, -1000.0] {
            for (across, expected) in [(0.0, 1.0), (50.0, 0.5), (-50.0, 0.5), (100.0, 0.0), (110.0, 0.0)] {
                let point = [center[0] + along * cos - across * sin, center[1] + along * sin + across * cos];
                assert!((brush.weight_at(center, point) - expected).abs() < 1e-12);
            }
        }
    }
    for radius in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(Brush::line(radius, 0.0).is_err());
    }
    for angle in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(Brush::line(100.0, angle).is_err());
    }
}

#[test]
fn inflate_and_contract_keep_the_center_and_respect_shape_and_selection() {
    for (brush, expanded, contracted) in [
        (Brush::circle(100.0).unwrap(), 62.5, 37.5),
        (Brush::double_threshold(100.0, 50.0).unwrap(), 75.0, 25.0),
        (Brush::rectangle(100.0, 100.0).unwrap(), 62.5, 37.5),
        (Brush::line(100.0, std::f64::consts::FRAC_PI_2).unwrap(), 62.5, 37.5),
    ] {
        for (amount, expected) in [(0.5, expanded), (-0.5, contracted)] {
            let mut points = [[0.0,0.0], [50.0,0.0], [200.0,0.0], [-50.0,0.0]];
            inflate_stroke(&mut points, &[0,1,2], &brush, &[InflateSample { center:[0.0,0.0], amount }]).unwrap();
            assert_eq!(points[0], [0.0,0.0]);
            assert!((points[1][0]-expected).abs()<1e-10);
            assert_eq!(points[2], [200.0,0.0]);
            assert_eq!(points[3], [-50.0,0.0]);
        }
    }
    let mut points = [[50.0,0.0]];
    inflate_stroke(&mut points, &[], &Brush::circle(100.0).unwrap(), &[
        InflateSample { center:[0.0,0.0], amount:0.5 },
        InflateSample { center:[0.0,0.0], amount:-0.5 },
    ]).unwrap();
    assert_eq!(points, [[50.78125,0.0]]);
}

#[test]
fn inflate_rejects_the_whole_invalid_sample_list_before_applying_it() {
    let brush=Brush::circle(100.0).unwrap();
    let valid=InflateSample { center:[0.0,0.0], amount:0.5 };
    for samples in [vec![], vec![valid;MAX_INFLATE_SAMPLES+1],
        vec![valid,InflateSample { center:[f64::NAN,0.0],amount:0.1 }],
        vec![valid,InflateSample { center:[0.0,0.0],amount:f64::INFINITY }],
        vec![valid,InflateSample { center:[0.0,0.0],amount:1.01 }],
        vec![valid,InflateSample { center:[0.0,0.0],amount:-1.01 }],
    ] {
        let mut points=[[50.0,0.0]];
        assert!(inflate_stroke(&mut points,&[],&brush,&samples).is_err());
        assert_eq!(points,[[50.0,0.0]]);
    }
}

#[test]
fn line_brush_drag_flow_and_selection_use_the_shared_stroke() {
    let original = [[0.0, 0.0], [1000.0, 0.0], [0.0, 50.0], [0.0, 100.0]];
    for (flow, offsets) in [(false, [40.0, 40.0, 20.0, 0.0]), (true, [31.2, 31.2, 29.2, 11.2])] {
        let mut points = original;
        brush_stroke(&mut points, &[], &Brush::line(100.0, 0.0).unwrap(), flow, &[[0.0, 0.0], [0.0, 20.0], [0.0, 40.0]], None).unwrap();
        for (i, point) in points.iter().enumerate() {
            assert!((point[1] - original[i][1] - offsets[i]).abs() < 1e-12);
        }
    }
    let mut points = original;
    brush_stroke(&mut points, &[2], &Brush::line(100.0, 0.0).unwrap(), false, &[[0.0, 0.0], [0.0, 20.0]], None).unwrap();
    assert_eq!(points, [[0.0, 0.0], [1000.0, 0.0], [0.0, 60.0], [0.0, 100.0]]);
}

#[test]
fn brush_stroke_moves_by_weight_and_flow_recomputes_the_weights() {
    let start = vec![[0.0, 0.0], [150.0, 0.0], [400.0, 0.0]];
    let circle = Brush::circle(300.0).unwrap();
    let mut fixed = start.clone();
    brush_stroke(&mut fixed, &[], &circle, false, &[[0.0, 0.0], [10.0, 0.0]], None).unwrap();
    assert_eq!(fixed, vec![[10.0, 0.0], [155.0, 0.0], [400.0, 0.0]]);

    let double = Brush::double_threshold(300.0, 100.0).unwrap();
    let mut flow = start.clone();
    brush_stroke(
        &mut flow,
        &[],
        &double,
        true,
        &[[0.0, 0.0], [10.0, 0.0], [20.0, 0.0]],
        None,
    )
    .unwrap();
    assert!(
        close(flow[0], [20.0, 0.0], 1e-9) && close(flow[1], [166.1, 0.0], 1e-9) && flow[2] == [400.0, 0.0],
        "{flow:?}"
    );
    let mut held = start.clone();
    brush_stroke(
        &mut held,
        &[],
        &double,
        false,
        &[[0.0, 0.0], [10.0, 0.0], [20.0, 0.0]],
        None,
    )
    .unwrap();
    // Fixed weights from the press: 0.75 at distance 150, for the whole 20 unit drag.
    assert!(close(held[1], [165.0, 0.0], 1e-9), "{held:?}");

    // A selection restricts the vertices; Shift locks the stroke to its dominant axis.
    let mut selected = start.clone();
    brush_stroke(
        &mut selected,
        &[1],
        &circle,
        false,
        &[[0.0, 0.0], [40.0, 10.0]],
        Some(1.0),
    )
    .unwrap();
    assert_eq!(selected, vec![[0.0, 0.0], [170.0, 0.0], [400.0, 0.0]]);
    assert!(brush_stroke(&mut selected, &[], &circle, false, &[[0.0, 0.0]], None).is_err());
    assert!(brush_stroke(&mut selected, &[7], &circle, false, &[[0.0, 0.0], [1.0, 0.0]], None).is_err());
    assert!(brush_stroke(&mut selected, &[], &circle, false, &[[0.0, 0.0], [f64::NAN, 0.0]], None).is_err());
}

// ------------------------------------------------------------- path deform

#[test]
fn path_deform_follows_a_translated_path_and_leaves_the_path_start_alone() {
    let path = [[0.0, 0.0], [10.0, 0.0], [20.0, 0.0]];
    let lifted: Vec<[f64; 2]> = path.iter().map(|p| [p[0], p[1] + 5.0]).collect();
    let mut positions = vec![[10.0, 0.0], [11.0, 2.0], [15.0, -3.0], [-1.0, 1.0]];
    path_deform(&mut positions, &[], &path, &lifted, 10).unwrap();
    assert!(close(positions[0], [10.0, 5.0], 2e-3), "{positions:?}");
    assert!(close(positions[1], [11.0, 7.0], 2e-3), "{positions:?}");
    assert!(close(positions[2], [15.0, 2.0], 2e-3), "{positions:?}");
    // Its nearest sample is the first one: D's uint underflow puts it off the path, so it stays.
    assert_eq!(positions[3], [-1.0, 1.0]);

    // Only selected vertices move; the identity target changes nothing.
    let mut some = vec![[10.0, 0.0], [11.0, 2.0]];
    path_deform(&mut some, &[1], &path, &lifted, 10).unwrap();
    assert_eq!(some[0], [10.0, 0.0]);
    assert!(close(some[1], [11.0, 7.0], 2e-3));
    let mut same = vec![[11.0, 2.0]];
    path_deform(&mut same, &[], &path, &path, 10).unwrap();
    assert!(close(same[0], [11.0, 2.0], 2e-3));

    assert!(path_deform(&mut some, &[], &path[..1], &path[..1], 10).is_err());
    assert!(path_deform(&mut some, &[], &path, &path[..2], 10).is_err());
    assert!(path_deform(&mut some, &[], &path, &path, 0).is_err());
    assert!(path_deform(&mut some, &[5], &path, &path, 10).is_err());
}

#[test]
fn deform_tools_translate_reset_and_rotate() {
    let rest = vec![[0.0, 0.0], [10.0, 0.0], [20.0, 0.0]];
    let options = EditOptions::default();
    let mut positions = rest.clone();
    deform_positions(
        &mut positions,
        &rest,
        &DeformTool::Translate {
            selected: vec![2],
            delta: [1.0, 2.0],
        },
        &options,
    )
    .unwrap();
    assert_eq!(positions[2], [21.0, 2.0]);
    deform_positions(
        &mut positions,
        &rest,
        &DeformTool::Reset { selected: vec![2] },
        &options,
    )
    .unwrap();
    assert_eq!(positions, rest);
    // Locked point 0 pivots the points beyond the dragged point 2's side: a quarter turn.
    let turn = DeformTool::BezierRotate {
        locked: 0,
        index: 2,
        from: [10.0, 0.0],
        to: [0.0, 10.0],
    };
    deform_positions(&mut positions, &rest, &turn, &options).unwrap();
    assert!(
        close(positions[1], [0.0, 10.0], 1e-12) && close(positions[2], [0.0, 20.0], 1e-12),
        "{positions:?}"
    );
    assert_eq!(positions[0], [0.0, 0.0]);
    let bad = DeformTool::BezierTranslate {
        index: 9,
        delta: [1.0, 0.0],
    };
    assert!(deform_positions(&mut positions, &rest, &bad, &options).is_err());
    let nan = DeformTool::Translate {
        selected: vec![0],
        delta: [f64::NAN, 0.0],
    };
    assert!(deform_positions(&mut positions, &rest, &nan, &options).is_err());
}

// -------------------------------------------------------------- bad input

#[test]
fn malformed_edits_are_rejected_without_changing_the_mesh() {
    let before = quad();
    for bad in [
        json!({"op": "add"}),
        json!({"op": "add", "pos": [0, "x"]}),
        json!({"op": "add", "pos": [0, 0], "extra": 1}),
        json!({"op": "teleport"}),
        json!({"op": "remove"}),
        json!({"op": "remove", "indices": [1], "at": [0, 0]}),
        json!({"op": "move", "indices": [-1], "delta": [0, 0]}),
        json!({"op": "flip", "axis": "z"}),
    ] {
        assert!(parse_edit(&bad).is_err(), "{bad}");
    }
    let runs: Vec<Edit> = vec![
        Edit::Add {
            pos: [f64::NAN, 0.0],
            connect: None,
        },
        Edit::Remove { indices: vec![9] },
        Edit::Move {
            indices: vec![9],
            delta: [1.0, 1.0],
        },
        Edit::Move {
            indices: vec![0],
            delta: [f64::INFINITY, 1.0],
        },
        Edit::Connect { a: 0, b: 0 },
        Edit::Connect { a: 0, b: 9 },
        Edit::Cut {
            from: [1.0, 1.0],
            to: [1.0, 1.0],
        },
        Edit::GridCreate {
            bounds: [0.0, 0.0, 1.0, 1.0],
            cuts: 1,
        },
        Edit::GridAxes {
            ys: vec![0.0, 0.0],
            xs: vec![0.0, 1.0],
        },
        Edit::GridAxes {
            ys: (0..300).map(f64::from).collect(),
            xs: (0..300).map(f64::from).collect(),
        },
        Edit::GridMove {
            vertex: 0,
            to: [f64::NAN, 0.0],
        },
    ];
    for edit in runs {
        let mut mesh = before.clone();
        let result = apply_edits(&mut mesh, std::slice::from_ref(&edit), &EditOptions::default());
        assert!(result.is_err(), "{edit:?}");
        assert_eq!(mesh, before, "{edit:?}");
    }
    let zoom = EditOptions {
        zoom: 0.0,
        ..EditOptions::default()
    };
    assert!(apply_edits(&mut quad(), &[Edit::Triangulate], &zoom).is_err());
    assert!(EditMesh::from_triangles(&[[0.0, 0.0]], &[0, 0, 0], [0.0, 0.0]).is_err());
    assert!(EditMesh::from_triangles(&[[0.0, 0.0]], &[0, 1, 2], [0.0, 0.0]).is_err());
}

// ------------------------------------------------------ session operations

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "soul2d-mesh-edit-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    common::scratch(path)
}

fn transform() -> Value {
    json!({"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]})
}

fn part(uuid: u64, texture: u32, verts: Value, indices: Value, extra: Value) -> Value {
    let mut node = json!({
        "uuid": uuid, "name": format!("Part{uuid}"), "type": "Part", "enabled": true, "zsort": 0.0,
        "transform": transform(), "children": [], "lockToRoot": false, "pinToMesh": false,
        "mesh": {"verts": verts, "uvs": [], "indices": indices, "origin": [3.0, 4.0]},
        "textures": [texture], "blend_mode": "Normal", "tint": [1, 1, 1], "screenTint": [0, 0, 0],
        "emissionStrength": 1, "opacity": 1,
    });
    let count = node["mesh"]["verts"].as_array().unwrap().len();
    node["mesh"]["uvs"] = json!(vec![0.0; count]);
    for (key, value) in extra.as_object().unwrap() {
        node[key] = value.clone();
    }
    node
}

fn model(children: Vec<Value>, param: Value) -> Value {
    json!({
        "meta": {"name": "fixture", "version": "1.0-alpha"},
        "nodes": {"uuid": 1, "name": "Root", "type": "Node", "enabled": true, "zsort": 0.0,
            "transform": transform(), "lockToRoot": false, "pinToMesh": false, "children": children},
        "param": param, "animations": {},
    })
}

fn blink() -> Value {
    json!([{
        "uuid": 17, "name": "Blink", "is_vec2": true, "min": [-1.0, -1.0], "max": [1.0, 1.0],
        "defaults": [0.0, 0.0], "axis_points": [[0.0, 1.0], [0.0, 1.0]], "merge_mode": "Additive",
        "bindings": [],
    }])
}

fn session_with(children: Vec<Value>, param: Value) -> Session {
    let mut session = Session::new();
    let reply = session.handle_request(&json!({"op": "new", "model": model(children, param)}));
    assert_eq!(reply["succeeded"], true, "{reply}");
    session
}

fn request(session: &mut Session, value: Value) -> Value {
    session.handle_request(&value)
}

fn ok(reply: &Value) -> &Value {
    assert_eq!(reply["succeeded"], true, "{reply}");
    &reply["result"]
}

fn node(session: &Session, id: u64) -> Value {
    fn find(node: &Value, id: u64) -> Option<Value> {
        if node["uuid"] == id {
            return Some(node.clone());
        }
        node["children"].as_array()?.iter().find_map(|child| find(child, id))
    }
    find(&session.snapshot()["model"]["nodes"], id).unwrap()
}

fn node_json(session: &Session, id: u64) -> Value {
    node(session, id)
}

fn floats(value: &Value) -> Vec<f64> {
    value.as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect()
}

/// Right triangle (0,0), (16,0), (0,16): fewer than four vertices, so it is never a grid.
fn triangle_part(extra: Value) -> Value {
    part(
        5,
        NO_TEXTURE,
        json!([0.0, 0.0, 16.0, 0.0, 0.0, 16.0]),
        json!([0, 1, 2]),
        extra,
    )
}

#[test]
fn mesh_edit_adds_a_vertex_transfers_the_deform_binding_and_undoes_in_one_step() {
    let mut param = blink();
    param[0]["bindings"] = json!([{
        "node": 5, "param_name": "deform", "interpolate_mode": "Linear",
        "values": [[[[0.0, 0.0], [0.0, 0.0], [0.0, 0.0]], null], [[[0.0, 0.0], [0.0, 0.0], [0.0, 8.0]], null]],
        "isSet": [[false, false], [true, false]],
    }]);
    let mut session = session_with(vec![triangle_part(json!({}))], param);
    let before = session.snapshot();

    let reply = request(
        &mut session,
        json!({"op": "mesh.edit", "id": 5, "edits": [
            {"op": "add", "pos": [4, 4]},
            {"op": "connect", "a": 3, "b": 0},
            {"op": "connect", "a": 3, "b": 1},
            {"op": "connect", "a": 3, "b": 2},
        ]}),
    );
    assert_eq!(
        ok(&reply),
        &json!({"id": 5, "changed": true, "vertices": 4, "triangles": 4, "grid": false})
    );

    let mesh = node(&session, 5)["mesh"].clone();
    assert_eq!(floats(&mesh["verts"]), vec![0.0, 0.0, 16.0, 0.0, 0.0, 16.0, 4.0, 4.0]);
    assert_eq!(mesh["origin"], json!([3.0, 4.0]));
    // No texture: the UVs rescale the 16 x 16 bounds to 0..1.
    assert_eq!(floats(&mesh["uvs"]), vec![0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.25, 0.25]);
    let triangles: Vec<Vec<u64>> = mesh["indices"]
        .as_array()
        .unwrap()
        .chunks(3)
        .map(|t| {
            let mut t: Vec<u64> = t.iter().map(|v| v.as_u64().unwrap()).collect();
            t.sort_unstable();
            t
        })
        .collect();
    let mut sorted = triangles.clone();
    sorted.sort();
    // The three new triangles plus the original one, whose edges are still connected.
    assert_eq!(sorted, vec![vec![0, 1, 2], vec![0, 1, 3], vec![0, 2, 3], vec![1, 2, 3]]);

    // The key [1,0] offset is carried onto the new vertices through the old triangle:
    // (4,4) sits at barycentre (0.25, 0.25) and the third corner rose by 8, so it moves (0, 2).
    let binding = &session.snapshot()["model"]["param"][0]["bindings"][0];
    assert_eq!(
        binding["values"][1][0],
        json!([[0.0, 0.0], [0.0, 0.0], [0.0, 8.0], [0.0, 2.0]])
    );
    assert_eq!(binding["isSet"], json!([[false, false], [true, false]]));
    for column in binding["values"].as_array().unwrap() {
        for cell in column.as_array().unwrap() {
            assert_eq!(cell.as_array().unwrap().len(), 4, "every keypoint is resized");
        }
    }

    let undo = request(&mut session, json!({"op": "undo"}));
    ok(&undo);
    assert_eq!(session.snapshot()["model"], before["model"]);
    ok(&request(&mut session, json!({"op": "redo"})));
    assert_eq!(floats(&node(&session, 5)["mesh"]["verts"]).len(), 8);
}

#[test]
fn an_added_point_without_a_connection_is_stored_and_counted() {
    let mut session = session_with(vec![triangle_part(json!({}))], blink());
    let reply = request(
        &mut session,
        json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "add", "pos": [4, 4]}]}),
    );
    assert_eq!(
        ok(&reply),
        &json!({"id": 5, "changed": true, "vertices": 4, "triangles": 1, "grid": false})
    );
    let mesh = node(&session, 5)["mesh"].clone();
    assert_eq!(floats(&mesh["verts"]), vec![0.0, 0.0, 16.0, 0.0, 0.0, 16.0, 4.0, 4.0]);
    assert_eq!(mesh["indices"], json!([0, 1, 2]));

    // The point is in no triangle, and the next edit still finds it.
    let moved = request(
        &mut session,
        json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "move", "indices": [3], "delta": [1, 0]}]}),
    );
    assert_eq!(ok(&moved)["vertices"], 4);
    assert_eq!(
        floats(&node(&session, 5)["mesh"]["verts"]),
        vec![0.0, 0.0, 16.0, 0.0, 0.0, 16.0, 5.0, 4.0]
    );
}

#[test]
fn mesh_edit_errors_leave_the_document_unchanged() {
    let mut session = session_with(
        vec![
            triangle_part(json!({})),
            part(6, NO_TEXTURE, json!([]), json!([]), json!({})),
        ],
        blink(),
    );
    let before = session.snapshot();
    let bad = [
        json!({"op": "mesh.edit", "id": 5, "edits": []}),
        json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "remove", "indices": [7]}]}),
        json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "add", "pos": [1, 1]}, {"op": "connect", "a": 3, "b": 3}]}),
        json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "add", "pos": [1, null]}]}),
        json!({"op": "mesh.edit", "id": 99, "edits": [{"op": "flip", "axis": "h"}]}),
        json!({"op": "mesh.edit", "id": 1, "edits": [{"op": "flip", "axis": "h"}]}),
        json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "flip", "axis": "h"}], "zoom": -1}),
        json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "flip", "axis": "h"}], "unknown": 1}),
        json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "grid_axes", "ys": [0, 1], "xs": [2, 2]}]}),
    ];
    for request_json in bad {
        let reply = request(&mut session, request_json.clone());
        assert_eq!(reply["succeeded"], false, "{request_json}");
        assert_eq!(session.snapshot(), before, "{request_json}");
    }
}

#[test]
fn mesh_edit_rejects_data_it_cannot_remap_but_allows_topology_only_edits() {
    let welded = triangle_part(json!({"weldedLinks": [{"targetUUID": 6, "indices": [0, -1, -1], "weight": 0.5}]}));
    let other = part(
        6,
        NO_TEXTURE,
        json!([0.0, 0.0, 16.0, 0.0, 0.0, 16.0]),
        json!([0, 1, 2]),
        json!({}),
    );
    let mut session = session_with(vec![welded, other], blink());
    let reply = request(
        &mut session,
        json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "move", "indices": [0], "delta": [1, 1]}]}),
    );
    assert_eq!(reply["succeeded"], false);
    assert!(reply["message"].as_str().unwrap().contains("welds"), "{reply}");
    // The other side of the weld is protected as well.
    assert_eq!(
        request(
            &mut session,
            json!({"op": "mesh.edit", "id": 6, "edits": [{"op": "flip", "axis": "h"}]})
        )["succeeded"],
        false
    );
    // Disconnecting an edge keeps every vertex where it is.
    ok(&request(
        &mut session,
        json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "disconnect", "a": 0, "b": 1}]}),
    ));
    assert_eq!(node(&session, 5)["mesh"]["indices"], json!([]));

    let mut depth = session_with(vec![triangle_part(json!({"depths": [0.0, 0.0, 0.0]}))], blink());
    let reply = request(
        &mut depth,
        json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "add", "pos": [4, 4]}]}),
    );
    assert!(reply["message"].as_str().unwrap().contains("depth"), "{reply}");
    // Moving a vertex keeps its index, so per-vertex depths stay valid.
    ok(&request(
        &mut depth,
        json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "move", "indices": [0], "delta": [1, 1]}]}),
    ));
}

fn texture_session(verts: Value, indices: Value) -> Session {
    texture_session_of([64, 64], verts, indices, json!({}))
}

fn texture_session_of([width, height]: [u32; 2], verts: Value, indices: Value, extra: Value) -> Session {
    let image = image::RgbaImage::from_fn(width, height, |_, _| image::Rgba([200, 100, 50, 255]));
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let texture = png.into_inner();
    let raw = serde_json::to_vec(&model(vec![part(5, 0, verts, indices, extra)], json!([]))).unwrap();
    let mut file = b"TRNSRTS\0".to_vec();
    file.extend_from_slice(&(raw.len() as u32).to_be_bytes());
    file.extend_from_slice(&raw);
    file.extend_from_slice(b"TEX_SECT");
    file.extend_from_slice(&1u32.to_be_bytes());
    file.extend_from_slice(&(texture.len() as u32).to_be_bytes());
    file.push(0);
    file.extend_from_slice(&texture);
    let path = temp_dir().join("fixture.inx");
    fs::write(&path, file).unwrap();
    let mut session = Session::new();
    ok(&request(
        &mut session,
        json!({"op": "open", "path": path.to_str().unwrap()}),
    ));
    session
}

#[test]
fn reset_puts_back_the_imported_quad_over_the_texture_and_undoes_in_one_step() {
    // An odd texture: the import halves its size rounded down.
    let mut session = texture_session_of(
        [63, 31],
        json!([0.0, 0.0, 16.0, 0.0, 0.0, 16.0, 4.0, 4.0]),
        json!([0, 1, 2, 0, 1, 3]),
        json!({}),
    );
    let before = session.snapshot();
    let reply = request(&mut session, json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "reset"}]}));
    assert_eq!(ok(&reply), &json!({"id": 5, "changed": true, "vertices": 4, "triangles": 2, "grid": true}));
    let mesh = node(&session, 5)["mesh"].clone();
    assert_eq!(floats(&mesh["verts"]), vec![-31.0, -15.0, 31.0, -15.0, -31.0, 15.0, 31.0, 15.0]);
    assert_eq!(mesh["origin"], json!([0.0, 0.0]));
    let uv = |p: f64, size: f64| p / size + 0.5;
    assert_eq!(floats(&mesh["uvs"]), vec![
        uv(-31.0, 63.0), uv(-15.0, 31.0), uv(31.0, 63.0), uv(-15.0, 31.0),
        uv(-31.0, 63.0), uv(15.0, 31.0), uv(31.0, 63.0), uv(15.0, 31.0),
    ]);
    // Edits after a reset in the same request start from the quad.
    ok(&request(&mut session, json!({"op": "undo"})));
    assert_eq!(session.snapshot()["model"], before["model"]);
    let reply = request(&mut session, json!({"op": "mesh.edit", "id": 5, "edits": [
        {"op": "reset"}, {"op": "move", "indices": [0], "delta": [1, 0]}]}));
    assert_eq!(ok(&reply)["vertices"], 4);
    assert_eq!(floats(&node(&session, 5)["mesh"]["verts"])[..2], [-30.0, -15.0]);

    // A reset renumbers the vertices, even when a move leaves four that are no grid.
    let mut deep = texture_session_of([63, 31], json!([0.0, 0.0, 16.0, 0.0, 0.0, 16.0, 4.0, 4.0]),
        json!([0, 1, 2, 0, 1, 3]), json!({"depths": [0.0, 0.0, 1.0, 1.0]}));
    let reply = request(&mut deep, json!({"op": "mesh.edit", "id": 5, "edits": [
        {"op": "reset"}, {"op": "move", "indices": [0], "delta": [1, 0]}]}));
    assert_eq!(reply["message"], "Part 5 has a depth payload that mesh.edit cannot resize", "{reply}");

    // A Part without a texture has no quad to go back to.
    let mut bare = session_with(vec![triangle_part(json!({}))], blink());
    let before = bare.snapshot();
    let reply = request(&mut bare, json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "reset"}]}));
    assert_eq!(reply["message"], "edit 0: the Part has no texture to reset its mesh to", "{reply}");
    assert_eq!(bare.snapshot(), before);
}

#[test]
fn merge_joins_coincident_vertices_and_keeps_their_triangles() {
    // Vertex 3 sits on vertex 1, and vertex 5 is half a unit from vertex 4.
    let verts = json!([0.0, 0.0, 16.0, 0.0, 0.0, 16.0, 16.0, 0.0, 16.0, 16.0, 16.5, 16.0]);
    let indices = json!([0, 1, 2, 3, 4, 2, 4, 5, 2]);
    let mut session = session_with(vec![part(5, NO_TEXTURE, verts.clone(), indices.clone(), json!({}))], blink());
    let merged = request(&mut session, json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "merge"}]}));
    assert_eq!(ok(&merged)["vertices"], 5);
    let mesh = node(&session, 5)["mesh"].clone();
    assert_eq!(floats(&mesh["verts"]), vec![0.0, 0.0, 16.0, 0.0, 0.0, 16.0, 16.0, 16.0, 16.5, 16.0]);
    let mut triangles: Vec<Vec<u64>> = mesh["indices"].as_array().unwrap().chunks(3)
        .map(|t| { let mut t: Vec<u64> = t.iter().map(|v| v.as_u64().unwrap()).collect(); t.sort_unstable(); t })
        .collect();
    triangles.sort();
    // The triangle of the merged vertex now uses vertex 1.
    assert_eq!(triangles, vec![vec![0, 1, 2], vec![1, 2, 3], vec![2, 3, 4]]);

    // With a distance, the near vertex joins too; its triangle collapses and is gone.
    let mut session = session_with(vec![part(5, NO_TEXTURE, verts.clone(), indices.clone(), json!({}))], blink());
    let merged = request(&mut session, json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "merge", "distance": 0.5}]}));
    assert_eq!(ok(&merged)["vertices"], 4);
    assert_eq!(floats(&node(&session, 5)["mesh"]["verts"]), vec![0.0, 0.0, 16.0, 0.0, 0.0, 16.0, 16.0, 16.0]);
    assert_eq!(ok(&merged)["triangles"], 2);

    // A vertex within reach of two others joins the first; a vertex that joined another reaches no further.
    let chain = |order: [usize; 3], more: Value| {
        let points = [[0.0, 0.0], [1.0, 0.0], [0.5, 0.1]];
        let mut verts: Vec<f64> = order.iter().flat_map(|&i| points[i]).collect();
        verts.extend([0.5, 5.0, 3.0, 5.0]);
        let at = |i: usize| order.iter().position(|&o| o == i).unwrap() as u64;
        let indices = json!([at(0), at(1), 3, at(2), 3, 4]);
        let mut session = session_with(vec![part(5, NO_TEXTURE, json!(verts), indices, json!({}))], blink());
        let mut edits = vec![json!({"op": "merge", "distance": 0.6})];
        edits.extend(more.as_array().unwrap().iter().cloned());
        let merged = request(&mut session, json!({"op": "mesh.edit", "id": 5, "edits": edits}));
        assert_eq!(ok(&merged)["vertices"], 4, "{order:?}");
        node(&session, 5)["mesh"].clone()
    };
    let mesh = chain([0, 1, 2], json!([]));
    assert_eq!(floats(&mesh["verts"]), vec![0.0, 0.0, 1.0, 0.0, 0.5, 5.0, 3.0, 5.0]);
    // The kept vertex 0 takes the edges of the merged one, so its triangles come first.
    assert_eq!(mesh["indices"], json!([0, 1, 2, 0, 3, 2]));
    let mesh = chain([0, 2, 1], json!([]));
    assert_eq!(floats(&mesh["verts"]), vec![0.0, 0.0, 1.0, 0.0, 0.5, 5.0, 3.0, 5.0]);
    // The kept vertex knows the edge it took over (to vertex 3): toggling it from that end removes it and its triangle.
    assert_eq!(chain([0, 1, 2], json!([{"op": "toggle", "a": 3, "b": 0}]))["indices"], json!([0, 1, 2]));

    // A merge that finds nothing keeps per-vertex data; one that merges cannot resize it.
    let mut deep = session_with(vec![triangle_part(json!({"depths": [0.0, 0.0, 1.0]}))], blink());
    ok(&request(&mut deep, json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "merge"}]})));
    let reply = request(&mut deep, json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "merge", "distance": 16}]}));
    assert_eq!(reply["message"], "Part 5 has a depth payload that mesh.edit cannot resize", "{reply}");
    // Even when a point added after it brings the count back.
    let reply = request(&mut deep, json!({"op": "mesh.edit", "id": 5, "edits": [
        {"op": "merge", "distance": 16}, {"op": "add", "pos": [4, 4]}, {"op": "add", "pos": [5, 5]}]}));
    assert_eq!(reply["message"], "Part 5 has a depth payload that mesh.edit cannot resize", "{reply}");

    // A triangulation merges coincident vertices first, and only those.
    let mut session = session_with(vec![part(5, NO_TEXTURE, verts, indices, json!({}))], blink());
    let triangulated = request(&mut session, json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "triangulate"}]}));
    assert_eq!(ok(&triangulated)["vertices"], 5);

    let before = session.snapshot();
    for (edit, message) in [
        (json!({"op": "merge", "distance": -1}), "merge distance must be a finite number of at least 0"),
        (json!({"op": "merge", "distance": "far"}), "distance must be a finite number"),
        (json!({"op": "merge", "radius": 1}), "radius"),
    ] {
        let reply = request(&mut session, json!({"op": "mesh.edit", "id": 5, "edits": [edit]}));
        assert!(reply["message"].as_str().unwrap().contains(message), "{reply}");
        assert_eq!(session.snapshot(), before);
    }
}

#[test]
fn mesh_edit_keeps_the_legacy_top_level_array_shape() {
    let mut node = triangle_part(json!({}));
    node["vertices"] = json!([[0.0, 0.0], [16.0, 0.0], [0.0, 16.0]]);
    node["uvs"] = json!([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]);
    node["indices"] = json!([0, 1, 2]);
    let mut session = session_with(vec![node], blink());
    ok(&request(
        &mut session,
        json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "move", "indices": [1], "delta": [4, 0]}]}),
    ));
    let after = node_json(&session, 5);
    assert_eq!(after["vertices"], json!([[0.0, 0.0], [20.0, 0.0], [0.0, 16.0]]));
    assert_eq!(after["mesh"]["verts"], json!([0.0, 0.0, 20.0, 0.0, 0.0, 16.0]));
    assert_eq!(after["uvs"], json!([[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]));
    assert_eq!(after["indices"], json!([0, 1, 2]));
}

#[test]
fn mesh_edit_uses_the_texture_size_for_uvs_and_tracks_grid_axes() {
    let mut session = texture_session(
        json!([-8.0, -8.0, 8.0, -8.0, 8.0, 8.0, -8.0, 8.0]),
        json!([0, 1, 2, 0, 2, 3]),
    );
    // A no-op edit still applies the grid rule: the quad is sorted and tagged as a grid.
    let reply = request(
        &mut session,
        json!({"op": "mesh.edit", "id": 5, "edits": [
        {"op": "disconnect", "a": 0, "b": 2}, {"op": "connect", "a": 0, "b": 2}]}),
    );
    assert_eq!(ok(&reply)["grid"], true);
    let mesh = node(&session, 5)["mesh"].clone();
    assert_eq!(floats(&mesh["verts"]), vec![-8.0, -8.0, 8.0, -8.0, -8.0, 8.0, 8.0, 8.0]);
    assert_eq!(
        floats(&mesh["uvs"]),
        vec![0.375, 0.375, 0.625, 0.375, 0.375, 0.625, 0.625, 0.625]
    );
    assert_eq!(mesh["gridAxes"], json!([[-8.0, 8.0], [-8.0, 8.0]]));
    assert_eq!(mesh["origin"], json!([3.0, 4.0]));

    // Moving one corner breaks the grid: the axes go away and the texture mapping holds.
    let reply = request(
        &mut session,
        json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "move", "indices": [3], "delta": [4, 0]}]}),
    );
    assert_eq!(ok(&reply)["grid"], false);
    let mesh = node(&session, 5)["mesh"].clone();
    assert!(mesh.get("gridAxes").is_none());
    assert_eq!(floats(&mesh["verts"])[6..], [12.0, 8.0]);
    assert_eq!(floats(&mesh["uvs"])[6..], [12.0 / 64.0 + 0.5, 0.625]);
}

#[test]
fn deform_edit_translates_a_selection_and_reset_restores_it() {
    let verts = json!([0.0, 0.0, 16.0, 0.0, 0.0, 16.0, 4.0, 4.0]);
    let indices = json!([0, 1, 3, 1, 2, 3, 2, 0, 3]);
    let mut session = session_with(vec![part(5, NO_TEXTURE, verts, indices, json!({}))], blink());
    let before = session.snapshot();
    let reply = request(
        &mut session,
        json!({"op": "deform.edit", "id": 5, "parameter": 17, "key": [1, 1],
        "tool": "translate", "rect": [3, 3, 5, 5], "delta": [2, 0]}),
    );
    assert_eq!(
        ok(&reply),
        &json!({"id": 5, "parameter": 17, "key": [1, 1], "vertices": 4})
    );
    let binding = session.snapshot()["model"]["param"][0]["bindings"][0].clone();
    assert_eq!(
        binding["values"][1][1],
        json!([[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [2.0, 0.0]])
    );
    assert_eq!(binding["isSet"], json!([[false, false], [false, true]]));

    // A second gesture starts from the stored offsets.
    ok(&request(
        &mut session,
        json!({"op": "deform.edit", "id": 5, "parameter": 17, "key": [1, 1],
        "tool": "translate", "selected": [3], "delta": [1, 1]}),
    ));
    let values = &session.snapshot()["model"]["param"][0]["bindings"][0]["values"][1][1];
    assert_eq!(values[3], json!([3.0, 1.0]));
    ok(&request(
        &mut session,
        json!({"op": "deform.edit", "id": 5, "parameter": 17, "key": [1, 1],
        "tool": "reset", "selected": [3]}),
    ));
    let values = &session.snapshot()["model"]["param"][0]["bindings"][0]["values"][1][1];
    assert_eq!(values[3], json!([0.0, 0.0]));
    // Without a selection, reset returns the whole key to the rest shape.
    ok(&request(&mut session, json!({"op": "undo"})));
    ok(&request(
        &mut session,
        json!({"op": "deform.edit", "id": 5, "parameter": 17, "key": [1, 1], "tool": "reset"}),
    ));
    let values = &session.snapshot()["model"]["param"][0]["bindings"][0]["values"][1][1];
    assert!(values.as_array().unwrap().iter().all(|offset| offset == &json!([0.0, 0.0])), "{values}");

    ok(&request(&mut session, json!({"op": "undo"})));
    ok(&request(&mut session, json!({"op": "undo"})));
    ok(&request(&mut session, json!({"op": "undo"})));
    assert_eq!(session.snapshot()["model"], before["model"]);
}

#[test]
fn deform_edit_brush_stroke_uses_rest_plus_offset_and_is_one_step() {
    let verts = json!([0.0, 0.0, 150.0, 0.0, 400.0, 0.0]);
    let mut session = session_with(vec![part(5, NO_TEXTURE, verts, json!([0, 1, 2]), json!({}))], blink());
    let reply = request(
        &mut session,
        json!({"op": "deform.edit", "id": 5, "parameter": 17, "key": [0, 0],
        "tool": "brush", "brush": {"kind": "circle", "radius": 300}, "stroke": [[0, 0], [10, 0]]}),
    );
    ok(&reply);
    let values = session.snapshot()["model"]["param"][0]["bindings"][0]["values"][0][0].clone();
    assert_eq!(values, json!([[10.0, 0.0], [5.0, 0.0], [0.0, 0.0]]));
    let depth = request(&mut session, json!({"op": "history.summary"}));
    ok(&depth);
    ok(&request(&mut session, json!({"op": "undo"})));
    assert_eq!(session.snapshot()["model"]["param"][0]["bindings"], json!([]));
}

#[test]
fn temporary_warp_is_bilinear_and_perspective_is_projective() {
    let verts = json!([0.0, 0.0, 16.0, 0.0, 0.0, 16.0, 16.0, 16.0, 8.0, 8.0]);
    let indices = json!([0, 1, 4, 1, 3, 4, 3, 2, 4, 2, 0, 4]);
    let mut session = session_with(vec![part(5, NO_TEXTURE, verts, indices, json!({}))], blink());
    let base = json!({"op": "deform.edit", "id": 5, "parameter": 17, "key": [0, 0]});
    let offsets = |session: &mut Session| {
        let values = session.snapshot()["model"]["param"][0]["bindings"][0]["values"][0][0].clone();
        let pairs: Vec<[f64; 2]> = serde_json::from_value(values).unwrap();
        json!(pairs.iter().map(|p| p.map(|v| (v * 1e6).round() / 1e6 + 0.0)).collect::<Vec<_>>())
    };
    let corners = json!([[0.0, 0.0], [16.0, 0.0], [0.0, 16.0], [32.0, 32.0]]);
    let mut warp = base.clone();
    warp["tool"] = json!("warp");
    warp["grid"] = json!({"columns": 1, "rows": 1, "points": corners});
    ok(&request(&mut session, warp));
    assert_eq!(offsets(&mut session), json!([[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [16.0, 16.0], [4.0, 4.0]]),
        "the centre follows the average of the corners");
    ok(&request(&mut session, json!({"op": "undo"})));
    let mut perspective = base.clone();
    perspective["tool"] = json!("perspective");
    perspective["corners"] = corners;
    ok(&request(&mut session, perspective.clone()));
    assert_eq!(offsets(&mut session), json!([[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [16.0, 16.0], [0.0, 0.0]]),
        "the diagonals still cross at the centre");
    ok(&request(&mut session, json!({"op": "undo"})));
    let before = session.snapshot();
    // The last quad is not convex: TL, TR and BR lie on one line, which would flatten the mesh.
    for (key, bad) in [("corners", json!([[0.0, 0.0], [1.0, 1.0], [2.0, 2.0], [3.0, 3.0]])),
        ("corners", json!([[0.0, 0.0], [16.0, 0.0], [0.0, 16.0]])), ("selected", json!([0, 1])),
        ("corners", json!([[0.0, 0.0], [16.0, 0.0], [0.0, 16.0], [32.0, 0.0]]))] {
        let mut refused = perspective.clone();
        refused[key] = bad;
        assert_eq!(request(&mut session, refused)["succeeded"], false);
        assert_eq!(session.snapshot(), before);
    }
}

#[test]
fn a_canvas_move_is_solved_through_the_transforms_above_the_part() {
    let arm = json!({"uuid": 9, "name": "Arm", "type": "Node", "enabled": true, "zsort": 0.0, "lockToRoot": false,
        "pinToMesh": false, "transform": {"trans": [100.0, 50.0, 0.0], "rot": [0.0, 0.0, std::f64::consts::FRAC_PI_2],
        "scale": [2.0, 2.0]}, "children": [triangle_part(json!({}))]});
    let mut session = session_with(vec![arm], blink());
    ok(&request(&mut session, json!({"op": "value.set", "id": 17, "value": [-1.0, -1.0]})));
    let base = json!({"op": "deform.edit", "id": 5, "parameter": 17, "key": [0, 0], "tool": "translate",
        "space": "canvas", "selected": [1]});
    let mut down = base.clone();
    down["delta"] = json!([0.0, 6.0]);
    down["selected"] = json!([1, 1]);
    let moved = ok(&request(&mut session, down)).clone();
    assert!(moved["residual"].as_f64().unwrap() < 1e-3, "{moved}");
    let offset = |session: &Session| {
        let pair = &session.snapshot()["model"]["param"][0]["bindings"][0]["values"][0][0][1];
        [pair[0].as_f64().unwrap(), pair[1].as_f64().unwrap()]
    };
    // A quarter turn and a scale of 2 above: canvas down is local +x at half the length.
    let [x, y] = offset(&session);
    assert!((x - 3.0).abs() < 1e-3 && y.abs() < 1e-3, "{x}, {y}");
    let mut right = base.clone();
    right["delta"] = json!([4.0, 0.0]);
    ok(&request(&mut session, right));
    let [x, y] = offset(&session);
    assert!((x - 3.0).abs() < 1e-3 && (y + 2.0).abs() < 1e-3, "{x}, {y}");
    let before = session.snapshot();
    for (key, value) in [("tool", json!("brush")), ("selected", json!([])), ("space", json!("world")),
        ("rect", json!([0, 0, 1, 1]))] {
        let mut bad = base.clone();
        bad["delta"] = json!([1.0, 0.0]);
        bad[key] = value;
        assert_eq!(request(&mut session, bad.clone())["succeeded"], false, "{bad}");
        assert_eq!(session.snapshot(), before);
    }
}

#[test]
fn deform_edit_rejects_bad_requests_without_changes() {
    let mut session = session_with(vec![triangle_part(json!({}))], blink());
    let before = session.snapshot();
    let base = json!({"op": "deform.edit", "id": 5, "parameter": 17, "key": [0, 0]});
    let with = |extra: Value| {
        let mut request = base.clone();
        for (key, value) in extra.as_object().unwrap() {
            request[key] = value.clone();
        }
        request
    };
    for bad in [
        with(json!({"tool": "translate", "selected": [1]})),
        with(json!({"tool": "translate", "selected": [9], "delta": [1, 1]})),
        with(json!({"tool": "translate", "selected": [0], "rect": [0, 0, 1, 1], "delta": [1, 1]})),
        with(json!({"tool": "translate", "selected": [], "delta": [1, 1]})),
        with(json!({"tool": "warp"})),
        with(json!({"tool": "warp", "grid": {"columns": 1, "rows": 1, "points": [[0, 0], [1, 0], [0, 1]]}})),
        with(json!({"tool": "warp", "grid": {"columns": 0, "rows": 1, "points": []}})),
        with(json!({"tool": "brush", "brush": {"kind": "circle", "radius": 0}, "stroke": [[0, 0], [1, 1]]})),
        with(json!({"tool": "brush", "brush": {"kind": "star"}, "stroke": [[0, 0], [1, 1]]})),
        with(json!({"tool": "brush", "brush": {"kind": "circle", "radius": 5}, "stroke": [[0, 0]]})),
        with(json!({"tool": "path_deform", "path": [[0, 0]], "target": [[0, 0]]})),
        with(json!({"tool": "reset", "selected": [0], "key": [5, 0]})),
        with(json!({"tool": "reset", "selected": [0], "parameter": 99})),
        with(json!({"tool": "reset", "selected": [0], "id": 99})),
    ] {
        let reply = request(&mut session, bad.clone());
        assert_eq!(reply["succeeded"], false, "{bad}");
        assert_eq!(session.snapshot(), before, "{bad}");
    }
}

#[test]
fn deform_edit_line_brush_validates_and_commits_one_history_step() {
    for (flow, expected) in [(false, [40.0, 40.0, 20.0, 0.0]), (true, [31.2, 31.2, 29.2, 11.2])] {
        let verts = json!([0.0, 0.0, 1000.0, 0.0, 0.0, 50.0, 0.0, 100.0]);
        let mut session = session_with(vec![part(5, NO_TEXTURE, verts, json!([0, 1, 2, 1, 2, 3]), json!({}))], blink());
        let before = session.snapshot();
        for brush in [json!({"kind":"line","radius":0,"angle":0}), json!({"kind":"line","radius":100}),
            json!({"kind":"line","radius":100,"angle":null}), json!({"kind":"line","radius":100,"angle":"0"})] {
            let reply = request(&mut session, json!({"op":"deform.edit","id":5,"parameter":17,"key":[0,0],
                "tool":"brush","brush":brush,"stroke":[[0,0],[0,20]]}));
            assert_eq!(reply["succeeded"], false, "{reply}");
            assert_eq!(session.snapshot(), before);
        }
        ok(&request(&mut session, json!({"op":"deform.edit","id":5,"parameter":17,"key":[0,0],
            "tool":"brush","brush":{"kind":"line","radius":100,"angle":0},
            "stroke":[[0,0],[0,20],[0,40]],"flow":flow})));
        let after = session.snapshot();
        let offsets = after["model"]["param"][0]["bindings"][0]["values"][0][0].as_array().unwrap();
        for (offset, y) in offsets.iter().zip(expected) {
            assert_eq!(offset[0], 0.0);
            assert!((offset[1].as_f64().unwrap() - y).abs() < 1e-5);
        }
        ok(&request(&mut session, json!({"op":"undo"})));
        assert_eq!(session.snapshot()["model"], before["model"]);
        ok(&request(&mut session, json!({"op":"redo"})));
        assert_eq!(session.snapshot()["model"], after["model"]);
    }
}

#[test]
fn deform_edit_inflate_rejects_atomically_and_preserves_noops_and_history() {
    let mut session = session_with(vec![part(5, NO_TEXTURE, json!([0,0,50,0,0,50]), json!([0,1,2]), json!({}))], blink());
    let before = session.snapshot();
    let base = json!({"op":"deform.edit","id":5,"parameter":17,"key":[0,0],"tool":"inflate",
        "brush":{"kind":"circle","radius":100}});
    for samples in [json!([]), json!([{"center":[0,0],"amount":1.1}]), json!([{"center":[0,0],"amount":null}]),
        json!([{"center":[0],"amount":0.1}]), json!([{"center":[0,0],"amount":0.1,"unknown":true}]),
        json!(vec![json!({"center":[0,0],"amount":0.1});MAX_INFLATE_SAMPLES+1])] {
        let mut bad=base.clone();bad["samples"]=samples;
        assert_eq!(request(&mut session,bad)["succeeded"],false);
        assert_eq!(session.snapshot(),before);
    }
    let mut zero=base.clone();zero["samples"]=json!([{"center":[0,0],"amount":0}]);
    ok(&request(&mut session,zero));assert_eq!(session.snapshot(),before);
    let mut grow=base.clone();grow["samples"]=json!([{"center":[0,0],"amount":0.5}]);
    grow["expectedRevision"]=before["revision"].clone();
    let stale=grow.clone();
    ok(&request(&mut session,grow));
    let after=session.snapshot();
    assert_eq!(request(&mut session,stale)["succeeded"],false);assert_eq!(session.snapshot(),after);
    for invalid in [Value::Null,json!(-1),json!("1")] {
        let mut bad=base.clone();bad["samples"]=json!([{"center":[0,0],"amount":0.1}]);bad["expectedRevision"]=invalid;
        assert_eq!(request(&mut session,bad)["succeeded"],false);assert_eq!(session.snapshot(),after);
    }
    assert_eq!(after["model"]["param"][0]["bindings"][0]["values"][0][0],json!([[0.0,0.0],[12.5,0.0],[0.0,12.5]]));
    ok(&request(&mut session,json!({"op":"undo"})));assert_eq!(session.snapshot()["model"],before["model"]);
    ok(&request(&mut session,json!({"op":"redo"})));assert_eq!(session.snapshot()["model"],after["model"]);
}

#[test]
fn inflate_roundtrips_part_path_and_grid_without_changing_their_geometry() {
    let folder=temp_dir();
    for kind in ["Part","PathDeformer","GridDeformer"] {
        let mut item=json!({"uuid":5,"name":kind,"type":kind,"enabled":true,"zsort":0.0,"transform":transform(),
            "children":[],"lockToRoot":false,"pinToMesh":false});
        let fields=match kind {
            "Part" => part(5,NO_TEXTURE,json!([-50,-50,50,-50,0,50]),json!([0,1,2]),json!({})),
            "PathDeformer" => json!({"curve_type":"Spline","dynamic_deformation":false,"vertices":[-50,0,0,0,50,0]}),
            _ => json!({"dynamic":false,"formation":"Bilinear","grid_axis_x":[-50,0,50],"grid_axis_y":[-50,0,50]}),
        };
        item.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        let mut session=session_with(vec![item],blink());
        let before=session.snapshot();
        ok(&request(&mut session,json!({"op":"deform.edit","id":5,"parameter":17,"key":[1,1],"tool":"inflate",
            "brush":{"kind":"circle","radius":100},"samples":[{"center":[0,0],"amount":0.5}]})));
        let after=session.snapshot();
        assert_eq!(after["model"]["nodes"],before["model"]["nodes"]);
        let values=after["model"]["param"][0]["bindings"][0]["values"][1][1].clone();
        assert!(values.as_array().unwrap().iter().any(|v| v != &json!([0.0,0.0])));
        ok(&request(&mut session,json!({"op":"undo"})));assert_eq!(session.snapshot()["model"],before["model"]);
        ok(&request(&mut session,json!({"op":"redo"})));assert_eq!(session.snapshot()["model"],after["model"]);
        let path=folder.join(format!("{kind}.inx"));
        ok(&request(&mut session,json!({"op":"save","path":path})));
        let mut reopened=Session::new();ok(&request(&mut reopened,json!({"op":"open","path":path})));
        assert_eq!(reopened.snapshot()["model"],after["model"]);
    }
}

#[test]
fn mesh_select_is_read_only() {
    let mut session = session_with(vec![triangle_part(json!({}))], blink());
    let before = session.snapshot();
    let reply = request(
        &mut session,
        json!({"op": "mesh.select", "id": 5, "rect": [-1, -1, 1, 20], "mode": "replace"}),
    );
    assert_eq!(ok(&reply), &json!({"selected": [0, 2]}));
    let reply = request(
        &mut session,
        json!({"op": "mesh.select", "id": 5, "rect": [-1, -1, 1, 20], "mode": "toggle", "current": [0, 1]}),
    );
    assert_eq!(ok(&reply), &json!({"selected": [1, 2]}));
    let lasso = json!([[-1, -1], [20, -1], [-1, 20], [-0.9, -0.9]]);
    let reply = request(&mut session, json!({"op": "mesh.select", "id": 5, "lasso": lasso}));
    assert_eq!(ok(&reply), &json!({"selected": [0, 1, 2]}));
    assert_eq!(
        request(&mut session, json!({"op": "mesh.select", "id": 5}))["succeeded"],
        false
    );
    assert_eq!(session.snapshot(), before);
}

#[test]
fn mcp_tools_call_mesh_edit_and_deform_edit() {
    let mut session = session_with(vec![triangle_part(json!({}))], blink());
    let listed = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .unwrap();
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for name in ["mesh_edit", "mesh_select", "deform_edit"] {
        assert!(names.contains(&name), "{name}");
    }
    let call = |session: &mut Session, name: &str, arguments: Value| {
        session
            .handle_mcp(&json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            }))
            .unwrap()
    };
    let edited = call(
        &mut session,
        "mesh_edit",
        json!({"id": 5, "edits": [{"op": "move", "indices": [2], "delta": [0, 4]}]}),
    );
    assert_eq!(edited["result"]["isError"], false, "{edited}");
    assert_eq!(
        floats(&node(&session, 5)["mesh"]["verts"]),
        vec![0.0, 0.0, 16.0, 0.0, 0.0, 20.0]
    );
    let deformed = call(
        &mut session,
        "deform_edit",
        json!({"id": 5, "parameter": 17, "key": [1, 0], "tool": "translate", "selected": [0], "delta": [1, 2]}),
    );
    assert_eq!(deformed["result"]["isError"], false, "{deformed}");
    let binding = &session.snapshot()["model"]["param"][0]["bindings"][0];
    assert_eq!(binding["values"][1][0], json!([[1.0, 2.0], [0.0, 0.0], [0.0, 0.0]]));
    let bad = call(
        &mut session,
        "mesh_edit",
        json!({"id": 5, "edits": [{"op": "remove", "indices": [9]}]}),
    );
    assert_eq!(bad["result"]["isError"], true, "{bad}");
}

#[test]
fn depths_and_skinning_weights_refuse_an_edit_that_renumbers_the_vertices() {
    let add = json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "add", "pos": [4, 4]}]});
    let mut deep = session_with(vec![triangle_part(json!({"depths": [0.0, 0.0, 1.0]}))], blink());
    let before = deep.snapshot();
    let reply = request(&mut deep, add.clone());
    assert_eq!(reply["message"], "Part 5 has a depth payload that mesh.edit cannot resize", "{reply}");
    assert_eq!(deep.snapshot(), before);

    let mut skinned = session_with(vec![triangle_part(json!({}))], blink());
    let id = |reply: Value| ok(&reply)["created"][0]["uuid"].as_u64().unwrap();
    let rig = id(request(&mut skinned, json!({"op": "node.create", "className": "DepthRigRoot", "name": "Rig"})));
    let bone = id(request(&mut skinned, json!({
        "op": "node.create", "className": "DepthBone", "name": "Bone", "parents": [rig],
        "restHead": [0.0, 0.0, 0.0], "restTail": [0.0, 10.0, 0.0]
    })));
    let still = id(request(&mut skinned, json!({
        "op": "node.create", "className": "DepthBone", "name": "Still", "parents": [rig],
        "restHead": [0.0, 0.0, 0.0], "restTail": [0.0, 10.0, 0.0]
    })));
    for source in [bone, still] {
        ok(&request(&mut skinned, json!({"op": "rig.add_source", "root": rig, "target": 5, "bone": source})));
    }
    ok(&request(&mut skinned, json!({
        "op": "rig.set_vertex_weights", "root": rig, "target": 5,
        "weights": [{"bone": bone, "values": [1.0, 1.0, 0.0]}, {"bone": still, "values": [0.0, 0.0, 1.0]}]
    })));
    let before = skinned.snapshot();
    let reply = request(&mut skinned, add);
    assert_eq!(reply["message"], "Part 5 has skinning weights that mesh.edit cannot remap", "{reply}");
    assert_eq!(skinned.snapshot(), before);
    // A move keeps vertex i as vertex i, so the rows still fit, and a driven rig writes the keys of the Part again.
    // Vertex 0 is drawn at x -3, which is 0 less the mesh origin, and a quarter turn of its bone about Y takes it to
    // x 0. Vertex 2 follows the bone that stays, also where the edit puts it: its key is no blend of the others.
    let key = |session: &Session, vertex: usize| {
        let bindings = session.snapshot()["model"]["param"][0]["bindings"].clone();
        let deform = bindings.as_array().unwrap().iter().find(|binding| binding["param_name"] == "deform").cloned();
        let pair = floats(&deform.unwrap()["values"][1][0][vertex]);
        [pair[0], pair[1]]
    };
    ok(&request(&mut skinned, json!({
        "op": "binding.set_trs", "parameter": 17, "nodes": [bone], "key": [1, 0],
        "rotation_y": std::f64::consts::FRAC_PI_2
    })));
    assert!(close(key(&skinned, 0), [3.0, 0.0], 1e-9) && close(key(&skinned, 2), [0.0, 0.0], 1e-9));
    let moved = json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "move", "indices": [2], "delta": [1, 0]}]});
    ok(&request(&mut skinned, moved));
    assert!(close(key(&skinned, 0), [3.0, 0.0], 1e-9), "{:?}", key(&skinned, 0));
    assert!(close(key(&skinned, 2), [0.0, 0.0], 1e-9), "{:?}", key(&skinned, 2));
}

#[test]
fn mesh_copy_appends_rest_vertices_and_a_failed_paste_keeps_the_clipboard() {
    let triangle = json!([0.0, 0.0, 10.0, 0.0, 0.0, 10.0]);
    let indices = json!([0, 1, 2]);
    let source = part(5, NO_TEXTURE, triangle.clone(), indices.clone(), json!({
        "transform": {"trans": [10.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]}
    }));
    let target = part(6, NO_TEXTURE, json!([1.0, 1.0, 2.0, 1.0, 1.0, 2.0]), indices.clone(), json!({}));
    let tilted = part(7, NO_TEXTURE, triangle.clone(), indices, json!({
        "transform": {"trans": [0.0, 0.0, 0.0], "rot": [15.0, 0.0, 0.0], "scale": [1.0, 1.0]}
    }));
    let mut session = session_with(vec![source, target, tilted], json!([]));
    ok(&request(&mut session, json!({"op": "mesh.copy", "id": 5})));
    ok(&request(&mut session, json!({"op": "mesh.edit", "id": 5, "edits": [{"op": "move", "indices": [0], "delta": [4.0, 0.0]}]})));
    let pasted = ok(&request(&mut session, json!({"op": "mesh.paste", "id": 6}))).clone();
    assert_eq!(pasted["appended"], 3);
    let verts = node(&session, 6)["mesh"]["verts"].as_array().unwrap().clone();
    let numbers: Vec<f64> = verts.iter().map(|value| value.as_f64().unwrap()).collect();
    assert!(numbers.windows(2).any(|pair| close([pair[0], pair[1]], [10.0, 0.0], 1e-6)), "{numbers:?}");
    assert_eq!(node(&session, 6)["mesh"]["origin"], json!([3.0, 4.0]));
    assert_eq!(session.handle_request(&json!({"op": "undo"}))["succeeded"], true);
    assert_eq!(node(&session, 6)["mesh"]["verts"].as_array().unwrap().len(), 6);
    let empty = request(&mut session, json!({"op": "mesh.paste", "id": 6}));
    assert_eq!(empty["succeeded"], false, "{empty}");
    ok(&request(&mut session, json!({"op": "mesh.copy", "id": 5})));
    let refused = request(&mut session, json!({"op": "mesh.paste", "id": 7}));
    assert_eq!(refused["succeeded"], false, "{refused}");
    let reflected = ok(&request(&mut session, json!({"op": "mesh.paste", "id": 6, "reflect": "horizontal"}))).clone();
    assert_eq!(reflected["appended"], 3);
    let reflected_verts = node(&session, 6)["mesh"]["verts"].as_array().unwrap().clone();
    let reflected_numbers: Vec<f64> = reflected_verts.iter().map(|value| value.as_f64().unwrap()).collect();
    assert!(
        reflected_numbers.windows(2).any(|pair| close([pair[0], pair[1]], [-8.0, 0.0], 1e-6)),
        "{reflected_numbers:?}"
    );
}
