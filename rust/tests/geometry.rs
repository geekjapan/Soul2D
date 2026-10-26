use soul2d_core::geometry::*;

fn mesh(positions: &[Vec2]) -> Vec<MeshVertex> {
    positions.iter().map(|&p| MeshVertex::new(p)).collect()
}

fn twice_area(points: &[Vec2], t: [u32; 3]) -> f64 {
    let [a, b, c] = t.map(|i| points[i as usize]);
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

fn assert_offsets(got: &[Vec2], want: &[Vec2]) {
    assert_eq!(got.len(), want.len(), "{got:?}");
    for (g, w) in got.iter().zip(want) {
        assert!((g[0] - w[0]).abs() < 1e-12 && (g[1] - w[1]).abs() < 1e-12, "{got:?} vs {want:?}");
    }
}

// Triangle index lists printed by the D `triangulate` (LDC build of the original source).
fn d_triangles(text: &str) -> Vec<[u32; 3]> {
    text.split(' ')
        .map(|t| {
            let i: Vec<u32> = t.split(',').map(|n| n.parse().unwrap()).collect();
            [i[0], i[1], i[2]]
        })
        .collect()
}

#[test]
fn connections_are_symmetric_idempotent_and_removable() {
    // regression.d testMeshCommonVertexConnections.
    let mut v = mesh(&[[0.0, 0.0], [1.0, 0.0], [2.0, 0.0]]);
    assert!(!is_connected_to(&v, 0, 1).unwrap());
    connect(&mut v, 0, 1).unwrap();
    assert!(is_connected_to(&v, 0, 1).unwrap() && is_connected_to(&v, 1, 0).unwrap());
    connect(&mut v, 0, 1).unwrap();
    connect(&mut v, 1, 0).unwrap();
    assert_eq!((v[0].connections.clone(), v[1].connections.clone()), (vec![1], vec![0]));
    connect(&mut v, 0, 2).unwrap();
    assert_eq!(v[0].connections, vec![1, 2]);
    disconnect(&mut v, 0, 1).unwrap();
    assert!(!is_connected_to(&v, 0, 1).unwrap() && !is_connected_to(&v, 1, 0).unwrap());
    assert!(is_connected_to(&v, 0, 2).unwrap());
    disconnect(&mut v, 0, 1).unwrap();
    connect(&mut v, 0, 1).unwrap();
    disconnect_all(&mut v, 0).unwrap();
    assert!(v.iter().all(|vertex| vertex.connections.is_empty()));
}

#[test]
fn connections_reject_bad_indices() {
    let mut v = mesh(&[[0.0, 0.0], [1.0, 0.0]]);
    assert!(connect(&mut v, 0, 2).is_err());
    assert!(connect(&mut v, 1, 1).is_err());
    assert!(disconnect(&mut v, 5, 0).is_err());
    assert!(disconnect_all(&mut v, 2).is_err());
    assert!(is_connected_to(&v, 0, 9).is_err());
    assert!(v.iter().all(|vertex| vertex.connections.is_empty()));
    assert_eq!(MeshVertex::new([3.0, 4.0]).group_id, 1);
}

#[test]
fn hit_test_uses_a_strict_zoom_scaled_radius() {
    assert!(is_overlapped([0.0, 0.0], [15.9, 0.0], 1.0).unwrap());
    assert!(!is_overlapped([0.0, 0.0], [16.0, 0.0], 1.0).unwrap());
    assert!(is_overlapped([0.0, 0.0], [7.9, 0.0], 2.0).unwrap());
    assert!(!is_overlapped([0.0, 0.0], [8.0, 0.0], 2.0).unwrap());
    assert!(is_overlapped([0.0, 0.0], [3.0, 4.0], 1.0).unwrap());
    assert!(is_overlapped([0.0, 0.0], [31.0, 0.0], 0.5).unwrap());
}

#[test]
fn vertex_lookup_returns_first_hit_and_find_point_the_nearest() {
    // regression.d node operation: a vertex at the node translation (10, 20).
    let node = [[10.0, 20.0]];
    assert_eq!(get_vertex_from_point(&node, [10.0, 20.0], 1.0).unwrap(), Some(0));
    assert_eq!(get_vertex_from_point(&node, [200.0, 200.0], 1.0).unwrap(), None);
    assert!(is_point_over_vertex(&node, [12.0, 20.0], 1.0).unwrap());
    assert!(!is_point_over_vertex(&node, [200.0, 200.0], 1.0).unwrap());

    let pts = [[0.0, 0.0], [10.0, 0.0], [30.0, 0.0]];
    // Both 0 and 1 are inside the radius; the first in list order wins.
    assert_eq!(get_vertex_from_point(&pts, [8.0, 0.0], 1.0).unwrap(), Some(0));
    assert_eq!(find_point(&pts, [8.0, 0.0], 1.0).unwrap(), Some(1));
    // find_point accepts exactly the radius, get_vertex_from_point does not.
    assert_eq!(find_point(&[[0.0, 0.0]], [16.0, 0.0], 1.0).unwrap(), Some(0));
    assert_eq!(find_point(&[[0.0, 0.0]], [16.5, 0.0], 1.0).unwrap(), None);
    assert_eq!(get_vertex_from_point(&[[0.0, 0.0]], [16.0, 0.0], 1.0).unwrap(), None);
    // Equal distances keep the first vertex.
    assert_eq!(find_point(&[[-2.0, 0.0], [2.0, 0.0]], [0.0, 0.0], 1.0).unwrap(), Some(0));
    assert_eq!(find_point(&[[0.0, 0.0], [10.0, 0.0]], [30.0, 0.0], 1.0).unwrap(), None);
}

#[test]
fn vertex_lookup_degenerate_inputs() {
    assert_eq!(get_vertex_from_point(&[], [0.0, 0.0], 1.0).unwrap(), None);
    assert_eq!(find_point(&[], [0.0, 0.0], 1.0).unwrap(), None);
    assert!(!is_point_over_vertex(&[], [0.0, 0.0], 1.0).unwrap());
    let one = [[0.0, 0.0]];
    for zoom in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(get_vertex_from_point(&one, [0.0, 0.0], zoom).is_err());
        assert!(find_point(&one, [0.0, 0.0], zoom).is_err());
        assert!(is_overlapped([0.0, 0.0], [1.0, 0.0], zoom).is_err());
    }
    assert!(get_vertex_from_point(&one, [f64::NAN, 0.0], 1.0).is_err());
    assert!(get_vertex_from_point(&[[0.0, f64::INFINITY]], [0.0, 0.0], 1.0).is_err());
    assert!(find_point(&[[f64::NAN, 0.0]], [0.0, 0.0], 1.0).is_err());
    assert!(is_overlapped([0.0, 0.0], [f64::INFINITY, 0.0], 1.0).is_err());
}

#[test]
fn rect_selection_normalizes_corners_and_filters_groups() {
    // regression.d: rectangle (0,0)-(20,30) selects the node vertex, also with corners swapped.
    let node = [[10.0, 20.0]];
    assert_eq!(get_in_rect(&node, [0.0, 0.0], [20.0, 30.0]).unwrap(), vec![0]);
    assert_eq!(get_in_rect(&node, [20.0, 30.0], [0.0, 0.0]).unwrap(), vec![0]);
    assert_eq!(get_in_rect(&node, [0.0, 25.0], [20.0, 30.0]).unwrap(), Vec::<usize>::new());
    // Edges are inclusive.
    assert_eq!(get_in_rect(&node, [10.0, 20.0], [10.0, 20.0]).unwrap(), vec![0]);
    assert_eq!(get_in_rect(&[], [0.0, 0.0], [1.0, 1.0]).unwrap(), Vec::<usize>::new());

    let mut v = mesh(&[[1.0, 1.0], [2.0, 2.0], [3.0, 3.0], [9.0, 9.0]]);
    v[1].group_id = 2;
    v[2].group_id = 2;
    assert_eq!(get_mesh_vertices_in_rect(&v, [0.0, 0.0], [5.0, 5.0], 0).unwrap(), vec![0, 1, 2]);
    assert_eq!(get_mesh_vertices_in_rect(&v, [5.0, 5.0], [0.0, 0.0], 2).unwrap(), vec![1, 2]);
    assert_eq!(get_mesh_vertices_in_rect(&v, [0.0, 0.0], [5.0, 5.0], 1).unwrap(), vec![0]);
    assert_eq!(get_mesh_vertices_in_rect(&v, [0.0, 0.0], [5.0, 5.0], 7).unwrap(), Vec::<usize>::new());
    assert!(get_in_rect(&node, [f64::NAN, 0.0], [1.0, 1.0]).is_err());
    assert!(get_in_rect(&[[f64::INFINITY, 0.0]], [0.0, 0.0], [1.0, 1.0]).is_err());
}

#[test]
fn bounds_are_exact_or_rounded_outward() {
    // regression.d testCoreMathTriangleInvariants.
    let square = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0], [5.0, 5.0]];
    assert_eq!(get_bounds(&square).unwrap(), [0.0, 0.0, 10.0, 10.0]);
    let loose = [[-0.5, 1.2], [2.1, 3.9]];
    assert_eq!(get_bounds(&loose).unwrap(), [-1.0, 1.0, 3.0, 4.0]);
    assert_eq!(get_min_max(&loose).unwrap(), ([-0.5, 1.2], [2.1, 3.9]));
    assert_eq!(get_min_max(&[[7.0, -3.0]]).unwrap(), ([7.0, -3.0], [7.0, -3.0]));
    assert!(get_bounds(&[]).is_err() && get_min_max(&[]).is_err());
    assert!(get_bounds(&[[0.0, f64::NAN]]).is_err());
    assert!(get_min_max(&[[0.0, 0.0], [f64::NEG_INFINITY, 1.0]]).is_err());
}

#[test]
fn point_in_triangle_is_inclusive_and_winding_independent() {
    let tri = [[0.0, 0.0], [4.0, 0.0], [0.0, 4.0]];
    let reversed = [tri[0], tri[2], tri[1]];
    for (point, inside) in [
        ([1.0, 1.0], true),
        ([2.0, 2.0], true),
        ([4.0, 0.0], true),
        ([0.0, 0.0], true),
        ([2.0, 0.0], true),
        ([2.1, 2.1], false),
        ([-0.1, 1.0], false),
        ([1.0, -0.1], false),
    ] {
        assert_eq!(point_in_triangle(point, tri).unwrap(), inside, "{point:?}");
        assert_eq!(point_in_triangle(point, reversed).unwrap(), inside, "{point:?}");
    }
    // D quirk: a collinear triangle accepts points on its line and rejects some off it.
    let line = [[0.0, 0.0], [1.0, 0.0], [2.0, 0.0]];
    assert!(point_in_triangle([5.0, 0.0], line).unwrap());
    assert!(!point_in_triangle([5.0, 1.0], line).unwrap());
    assert!(point_in_triangle([f64::NAN, 0.0], tri).is_err());
    assert!(point_in_triangle([0.0, 0.0], [tri[0], tri[1], [0.0, f64::INFINITY]]).is_err());
}

#[test]
fn point_in_polygon_counts_signed_crossings_of_edge_pairs() {
    let square = [
        [0.0, 0.0], [10.0, 0.0], [10.0, 0.0], [10.0, 10.0],
        [10.0, 10.0], [0.0, 10.0], [0.0, 10.0], [0.0, 0.0],
    ];
    assert!(point_in_polygon([5.0, 5.0], &square).unwrap());
    assert!(!point_in_polygon([15.0, 5.0], &square).unwrap());
    assert!(!point_in_polygon([-5.0, 5.0], &square).unwrap());
    assert!(!point_in_polygon([5.0, 15.0], &square).unwrap());
    // Reverse winding still reads as inside (the signed count is nonzero).
    let mut reversed = square;
    reversed.reverse();
    assert!(point_in_polygon([5.0, 5.0], &reversed).unwrap());
    assert!(!point_in_polygon([15.0, 5.0], &reversed).unwrap());
    // Degenerate input.
    assert!(!point_in_polygon([5.0, 5.0], &[]).unwrap());
    assert!(!point_in_polygon([5.0, 5.0], &[[0.0, 5.0], [10.0, 5.0]]).unwrap());
    assert!(point_in_polygon([5.0, 5.0], &square[..7]).is_err());
    assert!(point_in_polygon([f64::NAN, 5.0], &square).is_err());
    let mut bad = square;
    bad[3] = [10.0, f64::INFINITY];
    assert!(point_in_polygon([5.0, 5.0], &bad).is_err());
}

#[test]
fn fill_poly_rasterizes_inclusive_pixels() {
    let corner = [[0.0, 0.0], [4.0, 0.0], [0.0, 4.0]];
    let mut texture = vec![0u8; 25];
    fill_poly(&mut texture, 5, 5, [0, 0], &corner, [0, 1, 2], 7).unwrap();
    // Rows hold 5, 4, 3, 2, 1 pixels.
    let expected: Vec<u8> = [5, 4, 3, 2, 1]
        .iter()
        .flat_map(|&n| (0..5).map(move |x| if x < n { 7 } else { 0 }))
        .collect();
    assert_eq!(texture, expected);
    // The index triple selects vertices, in any order.
    let mut again = vec![0u8; 25];
    fill_poly(&mut again, 5, 5, [0, 0], &corner, [2, 1, 0], 7).unwrap();
    assert_eq!(again, expected);
}

#[test]
fn fill_poly_matches_the_d_triangulation_case() {
    // regression.d: 11x11 texture, bounds (0,0,10,10), first triangle of the square with center.
    let square = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0], [5.0, 5.0]];
    let tris = triangulate(&square, get_bounds(&square).unwrap()).unwrap();
    let mut texture = vec![0i32; 121];
    fill_poly(&mut texture, 11, 11, [0, 0], &square, tris[0], 7).unwrap();
    // Triangle (10,10) (5,5) (0,10): rows 5..=10 hold 1, 3, 5, 7, 9, 11 pixels.
    assert_eq!(texture.iter().filter(|&&p| p == 7).count(), 36);
    assert_eq!(texture[5 * 11 + 5], 7);
    assert_eq!(texture[4 * 11 + 5], 0);
    assert_eq!(texture[10 * 11], 7);
    assert_eq!(texture[9 * 11], 0);
}

#[test]
fn fill_poly_clips_to_the_texture_without_wrapping() {
    // Row 0 of the triangle runs past a 3-wide texture; D would spill into the next row.
    let tri = [[1.0, 0.0], [6.0, 0.0], [1.0, 1.0]];
    let mut texture = vec![0u8; 9];
    fill_poly(&mut texture, 3, 3, [0, 0], &tri, [0, 1, 2], 1).unwrap();
    assert_eq!(texture, vec![0, 1, 1, 0, 1, 0, 0, 0, 0]);
    // A shifted origin maps world pixel (2,2) to texture (0,0).
    let big = [[0.0, 0.0], [8.0, 0.0], [0.0, 8.0]];
    let mut shifted = vec![0u8; 9];
    fill_poly(&mut shifted, 3, 3, [2, 2], &big, [0, 1, 2], 1).unwrap();
    assert_eq!(shifted, vec![1; 9]);
    // Entirely outside the texture, in either direction, leaves it alone.
    let small = [[0.0, 0.0], [2.0, 0.0], [0.0, 2.0]];
    let mut untouched = vec![0u8; 9];
    fill_poly(&mut untouched, 3, 3, [0, 0], &[[10.0, 10.0], [12.0, 10.0], [10.0, 12.0]], [0, 1, 2], 1).unwrap();
    fill_poly(&mut untouched, 3, 3, [-9, -9], &small, [0, 1, 2], 1).unwrap();
    assert_eq!(untouched, vec![0; 9]);
    // A huge triangle costs only the texture area.
    let huge = [[-1e12, -1e12], [1e12, -1e12], [0.0, 1e12]];
    let mut covered = vec![0u8; 4];
    fill_poly(&mut covered, 2, 2, [0, 0], &huge, [0, 1, 2], 9).unwrap();
    assert_eq!(covered, vec![9; 4]);
}

#[test]
fn fill_poly_rejects_bad_input() {
    let tri = [[0.0, 0.0], [2.0, 0.0], [0.0, 2.0]];
    let mut texture = vec![0u8; 4];
    assert!(fill_poly(&mut texture, 3, 3, [0, 0], &tri, [0, 1, 2], 1).is_err());
    assert!(fill_poly(&mut texture, 2, 2, [0, 0], &tri, [0, 1, 3], 1).is_err());
    assert!(fill_poly(&mut texture, usize::MAX, 2, [0, 0], &tri, [0, 1, 2], 1).is_err());
    let nan = [[0.0, 0.0], [f64::NAN, 0.0], [0.0, 2.0]];
    assert!(fill_poly(&mut texture, 2, 2, [0, 0], &nan, [0, 1, 2], 1).is_err());
    // Fewer than three vertices is a silent no-op, as in D.
    fill_poly(&mut texture, 2, 2, [0, 0], &tri[..2], [0, 1, 1], 1).unwrap();
    fill_poly(&mut Vec::<u8>::new(), 0, 0, [0, 0], &tri, [0, 1, 2], 1).unwrap();
    assert_eq!(texture, vec![0; 4]);
}

#[test]
fn triangulate_matches_d_output() {
    // Expected strings come from running the D `triangulate` source (LDC 1.43) on the same input.
    let square = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0], [5.0, 5.0]];
    let tris = triangulate(&square, [0.0, 0.0, 10.0, 10.0]).unwrap();
    assert_eq!(tris, d_triangles("2,4,3 4,0,3 4,2,1 0,4,1"));
    // Each triangle is clockwise (negative twice-area) in the D triangulation.
    assert!(tris.iter().all(|&t| twice_area(&square, t) < 0.0));

    // A vertex on a hull edge must not leave a zero-area sliver.
    let on_edge = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0], [5.0, 0.0]];
    let tris = triangulate(&on_edge, [0.0, 0.0, 10.0, 10.0]).unwrap();
    assert_eq!(tris, d_triangles("2,4,3 4,0,3 1,4,2"));
    assert!(tris.iter().all(|&t| twice_area(&on_edge, t) != 0.0));

    let right = [[0.0, 0.0], [4.0, 0.0], [0.0, 3.0]];
    assert_eq!(triangulate(&right, [0.0, 0.0, 4.0, 3.0]).unwrap(), vec![[0, 2, 1]]);

    let scattered = [
        [21.0, 38.0], [0.0, 36.0], [4.0, 13.0], [39.0, 24.0],
        [9.0, 34.0], [3.0, 6.0], [6.0, 23.0], [39.0, 1.0],
    ];
    assert_eq!(
        triangulate(&scattered, [0.0, 0.0, 50.0, 50.0]).unwrap(),
        d_triangles("2,5,1 4,0,6 7,5,2 0,3,6 6,2,1 0,4,1 4,6,1 3,2,6 3,7,2")
    );
}

#[test]
fn triangulate_cocircular_grid_matches_d_tie_breaking() {
    let grid: Vec<Vec2> = (0..5)
        .flat_map(|y| (0..5).map(move |x| [x as f64 * 3.0, y as f64 * 3.0]))
        .collect();
    let tris = triangulate(&grid, [0.0, 0.0, 12.0, 12.0]).unwrap();
    assert_eq!(
        tris,
        d_triangles(
            "12,8,7 7,8,3 17,18,13 12,13,8 5,1,0 7,3,2 13,9,8 11,7,6 6,2,1 8,4,3 10,6,5 5,6,1 \
             16,12,11 6,7,2 21,17,16 18,14,13 8,9,4 15,11,10 10,11,6 17,13,12 11,12,7 23,19,18 \
             13,14,9 20,16,15 15,16,11 22,18,17 16,17,12 18,19,14 20,21,16 22,23,18 21,22,17 23,24,19"
        )
    );
    // The triangles tile the 12x12 hull exactly.
    let area: f64 = tris.iter().map(|&t| -twice_area(&grid, t) / 2.0).sum();
    assert_eq!(area, 144.0);
}

#[test]
fn triangulate_degenerate_input() {
    let bounds = [0.0, 0.0, 10.0, 10.0];
    assert_eq!(triangulate(&[], bounds).unwrap(), Vec::<[u32; 3]>::new());
    assert!(triangulate(&[[1.0, 1.0]], bounds).unwrap().is_empty());
    assert!(triangulate(&[[1.0, 1.0], [5.0, 5.0]], bounds).unwrap().is_empty());
    // Collinear points have no triangle with all real corners.
    let line = [[0.0, 0.0], [1.0, 0.0], [2.0, 0.0], [7.0, 0.0]];
    assert!(triangulate(&line, bounds).unwrap().is_empty());
    let diagonal = [[0.0, 0.0], [5.0, 5.0], [10.0, 10.0]];
    assert!(triangulate(&diagonal, bounds).unwrap().is_empty());
    // Zero-size bounds around one point still work.
    assert!(triangulate(&[[3.0, 3.0]], [3.0, 3.0, 3.0, 3.0]).unwrap().is_empty());
}

#[test]
fn triangulate_rejects_bad_input() {
    let bounds = [0.0, 0.0, 10.0, 10.0];
    let dup = [[1.0, 1.0], [4.0, 1.0], [1.0, 4.0], [4.0, 1.0]];
    assert_eq!(triangulate(&dup, bounds).unwrap_err(), "points 1 and 3 coincide");
    assert!(triangulate(&[[0.0, 0.0], [-0.0, 0.0]], bounds).is_err());
    assert!(triangulate(&[[1.0, f64::NAN], [4.0, 1.0], [1.0, 4.0]], bounds).is_err());
    assert!(triangulate(&[[1.0, 1.0], [f64::INFINITY, 1.0], [1.0, 4.0]], bounds).is_err());
    // A point far outside the padded bounds is reported, not silently dropped.
    let outside = [[1.0, 1.0], [1e9, 1.0], [1.0, 4.0]];
    assert_eq!(
        triangulate(&outside, bounds).unwrap_err(),
        "point 1 lies outside the triangulation bounds"
    );
    for bad in [
        [0.0, 0.0, f64::NAN, 10.0],
        [0.0, 0.0, f64::INFINITY, 10.0],
        [5.0, 0.0, 0.0, 10.0],
        [0.0, 5.0, 10.0, 0.0],
    ] {
        assert!(triangulate(&[[1.0, 1.0], [4.0, 1.0], [1.0, 4.0]], bad).is_err(), "{bad:?}");
    }
}

#[test]
fn triangulate_covers_the_hull_for_shuffled_input() {
    let pts = [[5.0, 5.0], [10.0, 10.0], [0.0, 10.0], [10.0, 0.0], [0.0, 0.0], [2.0, 7.0]];
    let tris = triangulate(&pts, [0.0, 0.0, 10.0, 10.0]).unwrap();
    let area: f64 = tris.iter().map(|&t| -twice_area(&pts, t) / 2.0).sum();
    assert_eq!(area, 100.0);
    let mut used: Vec<u32> = tris.iter().flatten().copied().collect();
    used.sort_unstable();
    used.dedup();
    assert_eq!(used, vec![0, 1, 2, 3, 4, 5]);
}

#[test]
fn transfer_offsets_follows_the_deformed_mesh() {
    let mesh_v = [[0.0, 0.0], [10.0, 0.0], [0.0, 10.0]];
    let idx = [0, 1, 2];
    // Offsets equal the vertices, so the mesh doubles in size about the origin.
    let grow = mesh_v;
    let points = [[2.0, 3.0], [5.0, 5.0], [0.0, 0.0]];
    let got = transfer_mesh_offsets(&points, &mesh_v, &idx, &grow, false).unwrap();
    assert_offsets(&got, &[[2.0, 3.0], [5.0, 5.0], [0.0, 0.0]]);
    // A constant shift moves every inside point by the shift.
    let got = transfer_mesh_offsets(&points, &mesh_v, &idx, &[[1.0, 2.0]; 3], false).unwrap();
    assert_offsets(&got, &[[1.0, 2.0]; 3]);
    // Outside points use the triangle owning the nearest vertex and extrapolate.
    let outside = [[20.0, 0.0], [-3.0, -3.0], [0.0, 1.0]];
    let got = transfer_mesh_offsets(&outside, &mesh_v, &idx, &grow, false).unwrap();
    assert_offsets(&got, &[[20.0, 0.0], [-3.0, -3.0], [0.0, 1.0]]);
}

#[test]
fn transfer_offsets_flip_mirrors_lookup_and_result() {
    let mesh_v = [[0.0, 0.0], [10.0, 0.0], [0.0, 10.0]];
    // (-2,3) looks up (2,3), which moves to (4,6) and flips back to (-4,6): offset (-2,3).
    let points = [[-2.0, 3.0], [-1.0, 1.0], [-3.0, 3.0]];
    let got = transfer_mesh_offsets(&points, &mesh_v, &[0, 1, 2], &mesh_v, true).unwrap();
    assert_offsets(&got, &[[-2.0, 3.0], [-1.0, 1.0], [-3.0, 3.0]]);
    // (2,3) mirrors to (-2,3), outside the mesh: nearest triangle extrapolates to (-1,3), flipped to (1,3).
    let got = transfer_mesh_offsets(&[[2.0, 3.0]; 3], &mesh_v, &[0, 1, 2], &[[1.0, 0.0]; 3], true).unwrap();
    assert_offsets(&got, &[[-1.0, 0.0]; 3]);
}

#[test]
fn transfer_offsets_uses_the_triangle_containing_the_point() {
    // Two triangles sharing a diagonal; only vertex 3 moves.
    let mesh_v = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
    let idx = [0, 1, 2, 0, 2, 3];
    let offsets = [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 4.0]];
    // (8,2) lies in the first triangle. (2,8) has weight 0.6 on vertex 3 in the second.
    // (5,5) is on the shared diagonal and takes the first triangle in index order.
    let points = [[8.0, 2.0], [2.0, 8.0], [5.0, 5.0]];
    let got = transfer_mesh_offsets(&points, &mesh_v, &idx, &offsets, false).unwrap();
    assert_offsets(&got, &[[0.0, 0.0], [0.0, 2.4], [0.0, 0.0]]);
}

#[test]
fn transfer_offsets_degenerate_input() {
    let mesh_v = [[0.0, 0.0], [10.0, 0.0], [0.0, 10.0]];
    let off = [[1.0, 1.0]; 3];
    // D returns zero offsets when points, offsets or mesh vertices number fewer than three.
    let two = [[1.0, 1.0], [2.0, 2.0]];
    assert_eq!(transfer_mesh_offsets(&two, &mesh_v, &[0, 1, 2], &off, false).unwrap(), vec![[0.0, 0.0]; 2]);
    assert_eq!(transfer_mesh_offsets(&[], &mesh_v, &[0, 1, 2], &off, false).unwrap(), Vec::<Vec2>::new());
    let pts = [[1.0, 1.0]; 3];
    assert_eq!(transfer_mesh_offsets(&pts, &mesh_v[..2], &[0, 1, 2], &off[..2], false).unwrap(), vec![[0.0, 0.0]; 3]);
    assert_eq!(transfer_mesh_offsets(&pts, &mesh_v, &[0, 1, 2], &off[..2], false).unwrap(), vec![[0.0, 0.0]; 3]);
    // Malformed meshes are errors instead of D's out-of-range reads or NaN offsets.
    assert!(transfer_mesh_offsets(&pts, &mesh_v, &[], &off, false).is_err());
    assert!(transfer_mesh_offsets(&pts, &mesh_v, &[0, 1], &off, false).is_err());
    assert!(transfer_mesh_offsets(&pts, &mesh_v, &[0, 1, 3], &off, false).is_err());
    assert!(transfer_mesh_offsets(&pts, &mesh_v, &[0, 1, 2], &[[0.0, 0.0]; 4], false).is_err());
    let flat_mesh = [[0.0, 0.0], [5.0, 0.0], [10.0, 0.0]];
    assert!(transfer_mesh_offsets(&[[3.0, 0.0]; 3], &flat_mesh, &[0, 1, 2], &off, false).is_err());
    assert!(transfer_mesh_offsets(&[[f64::NAN, 0.0]; 3], &mesh_v, &[0, 1, 2], &off, false).is_err());
    let inf_mesh = [[0.0, 0.0], [10.0, f64::INFINITY], [0.0, 10.0]];
    assert!(transfer_mesh_offsets(&pts, &inf_mesh, &[0, 1, 2], &off, false).is_err());
    assert!(transfer_mesh_offsets(&pts, &mesh_v, &[0, 1, 2], &[[f64::NAN, 0.0]; 3], false).is_err());
}
