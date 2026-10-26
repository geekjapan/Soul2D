use soul2d_core::automesh::{
    contour, generate, grid, optimum, skeleton, Algorithm, ContourOptions, GridOptions, MeshOutput,
    MeshTarget, OptimumOptions, SkeletonOptions, DEFAULT_WORK_BUDGET,
};
use soul2d_core::cv::WorkBudget;
use soul2d_core::pixels::{PixelFormat, PixelImage};

fn budget() -> WorkBudget {
    WorkBudget::new(DEFAULT_WORK_BUDGET)
}

fn mono(width: u32, height: u32, opaque: impl Fn(u32, u32) -> bool) -> PixelImage {
    let mut pixels = vec![0u8; (width * height) as usize];
    for y in 0..height {
        for x in 0..width {
            if opaque(x, y) {
                pixels[(y * width + x) as usize] = 255;
            }
        }
    }
    PixelImage::from_pixels(width, height, PixelFormat::Mono, pixels).unwrap()
}

/// The 48x48 RGBA part used by the D regression tests: a rectangle crossed by a vertical bar.
fn regression_image() -> PixelImage {
    let (w, h) = (48u32, 48u32);
    let mut pixels = vec![255u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let rect = (10..=37).contains(&x) && (8..=39).contains(&y);
            let bar = (22..=26).contains(&x) && (4..=43).contains(&y);
            pixels[((y * w + x) * 4 + 3) as usize] = if rect || bar { 255 } else { 0 };
        }
    }
    PixelImage::from_pixels(w, h, PixelFormat::Rgba, pixels).unwrap()
}

fn area2(mesh: &MeshOutput, tri: &[u32; 3]) -> f32 {
    let [a, b, c] = tri.map(|i| mesh.vertices[i as usize]);
    (b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1])
}

/// Every index is in range and distinct; all positions are finite.
fn assert_indices(mesh: &MeshOutput) {
    assert!(mesh
        .vertices
        .iter()
        .all(|v| v[0].is_finite() && v[1].is_finite()));
    for tri in &mesh.triangles {
        assert!(
            tri.iter().all(|&i| (i as usize) < mesh.vertices.len()),
            "index out of range: {tri:?}"
        );
        assert!(
            tri[0] != tri[1] && tri[1] != tri[2] && tri[0] != tri[2],
            "repeated index: {tri:?}"
        );
    }
}

/// Valid indices and every triangle has real area.
fn assert_valid(mesh: &MeshOutput) {
    assert_indices(mesh);
    for tri in &mesh.triangles {
        assert!(area2(mesh, tri).abs() > 1e-3, "degenerate triangle {tri:?}");
    }
}

fn inside(mesh: &MeshOutput, tri: &[u32; 3], p: [f32; 2]) -> bool {
    let [a, b, c] = tri.map(|i| mesh.vertices[i as usize]);
    let side = |p1: [f32; 2], p2: [f32; 2]| {
        (p2[0] - p1[0]) * (p[1] - p1[1]) - (p[0] - p1[0]) * (p2[1] - p1[1])
    };
    let d = [side(a, b), side(b, c), side(c, a)];
    let eps = 1e-3;
    !(d.iter().any(|&v| v < -eps) && d.iter().any(|&v| v > eps))
}

/// Number of opaque pixels (mapped to mesh coordinates) that no triangle covers.
fn uncovered_opaque_pixels(image: &PixelImage, mesh: &MeshOutput) -> usize {
    let [w, h] = image.size();
    let channels = image.channels();
    let alpha = if channels == 4 { 3 } else { 0 };
    let mut missing = 0;
    for y in 0..h {
        for x in 0..w {
            if image.pixels()[(y * w + x) as usize * channels + alpha] == 0 {
                continue;
            }
            let p = [x as f32 - (w / 2) as f32, y as f32 - (h / 2) as f32];
            if !mesh.triangles.iter().any(|t| inside(mesh, t, p)) {
                missing += 1;
            }
        }
    }
    missing
}

fn approx(a: &[f32], b: &[f32]) {
    assert_eq!(a.len(), b.len(), "{a:?} vs {b:?}");
    for (x, y) in a.iter().zip(b) {
        assert!((x - y).abs() < 1e-3, "{a:?} vs {b:?}");
    }
}

fn flat(vertices: &[[f32; 2]]) -> Vec<f32> {
    vertices.iter().flatten().copied().collect()
}

// --- grid ---------------------------------------------------------------------------------------

#[test]
fn grid_on_regression_image_follows_the_d_test_and_maps_around_the_image_centre() {
    let options = GridOptions {
        x_segments: 2,
        y_segments: 2,
        margin: 0.0,
        ..GridOptions::default()
    };
    let mesh = grid(&regression_image(), &options, &mut budget())
        .unwrap()
        .unwrap();
    // Opaque bounds are x 10..37 and y 4..43; the image centre is (24, 24).
    let axes = mesh.grid.clone().unwrap();
    approx(&axes.rows, &[-20.0, -0.5, 19.0]);
    approx(&axes.columns, &[-14.0, -0.5, 13.0]);
    assert_eq!(mesh.vertices.len(), 9);
    approx(&mesh.vertices[0], &[-14.0, -20.0]);
    approx(&mesh.vertices[4], &[-0.5, -0.5]);
    approx(&mesh.vertices[8], &[13.0, 19.0]);
    // The D grid centre swaps width and height, which picks these diagonals.
    assert_eq!(
        mesh.triangles,
        vec![
            [0, 1, 4],
            [0, 4, 3],
            [1, 2, 5],
            [1, 5, 4],
            [3, 6, 4],
            [6, 4, 7],
            [4, 7, 5],
            [7, 5, 8]
        ]
    );
    assert_valid(&mesh);
}

#[test]
fn grid_default_margin_extends_one_tenth_past_the_opaque_bounds() {
    let mesh = grid(&regression_image(), &GridOptions::default(), &mut budget())
        .unwrap()
        .unwrap();
    let axes = mesh.grid.clone().unwrap();
    approx(&axes.rows, &[-23.9, -20.0, -0.5, 19.0, 22.9]);
    approx(&axes.columns, &[-16.7, -14.0, -0.5, 13.0, 15.7]);
    assert_eq!(mesh.vertices.len(), 25);
    assert_eq!(mesh.triangles.len(), 32);
    assert_valid(&mesh);
}

#[test]
fn grid_explicit_scales_are_sorted_and_override_segments() {
    let image = mono(20, 20, |x, y| {
        (4..=15).contains(&x) && (4..=15).contains(&y)
    });
    let options = GridOptions {
        scale_x: Some(vec![1.0, 0.0]),
        scale_y: Some(vec![1.0, 0.25, 0.0]),
        ..GridOptions::default()
    };
    let mesh = grid(&image, &options, &mut budget()).unwrap().unwrap();
    let axes = mesh.grid.unwrap();
    approx(&axes.columns, &[-6.0, 5.0]);
    approx(&axes.rows, &[-6.0, -3.25, 5.0]);
    assert_eq!(mesh.vertices.len(), 6);
    assert_eq!(mesh.triangles.len(), 4);
}

#[test]
fn grid_reports_nothing_for_empty_single_pixel_and_single_line_images() {
    let options = GridOptions::default();
    assert_eq!(
        grid(&mono(9, 9, |_, _| false), &options, &mut budget()).unwrap(),
        None
    );
    let empty = PixelImage::from_pixels(0, 0, PixelFormat::Mono, vec![]).unwrap();
    assert_eq!(grid(&empty, &options, &mut budget()).unwrap(), None);
    assert_eq!(
        grid(
            &mono(9, 9, |x, y| x == 4 && y == 4),
            &options,
            &mut budget()
        )
        .unwrap(),
        None
    );
    assert_eq!(
        grid(&mono(9, 9, |x, y| y == 4 && x > 1), &options, &mut budget()).unwrap(),
        None
    );
}

// --- contour ------------------------------------------------------------------------------------

#[test]
fn contour_of_a_filled_square_is_its_four_corners() {
    let image = mono(32, 32, |x, y| {
        (8..=23).contains(&x) && (8..=23).contains(&y)
    });
    let options = ContourOptions {
        sampling_step: 1.0,
        min_distance: 1.0,
        scales: vec![1.0],
        ..ContourOptions::default()
    };
    let mesh = contour(&image, &options, MeshTarget::Drawable, &mut budget())
        .unwrap()
        .unwrap();
    assert_eq!(
        flat(&mesh.vertices),
        vec![-8.0, -8.0, 7.0, -8.0, 7.0, 7.0, -8.0, 7.0]
    );
    assert_eq!(mesh.triangles, vec![[0, 2, 1], [0, 3, 2]]);
    let covered: f32 = mesh
        .triangles
        .iter()
        .map(|t| area2(&mesh, t).abs() / 2.0)
        .sum();
    assert!((covered - 225.0).abs() < 1e-3, "hull area {covered}");
    assert_valid(&mesh);
}

#[test]
fn contour_on_regression_image_matches_the_d_test_parameters() {
    let options = ContourOptions {
        sampling_step: 12.0,
        mask_threshold: 1.0,
        min_distance: 4.0,
        max_distance: 24.0,
        scales: vec![1.0, 0.5, 0.0],
        ..ContourOptions::default()
    };
    let image = regression_image();
    let mesh = contour(&image, &options, MeshTarget::Drawable, &mut budget())
        .unwrap()
        .unwrap();
    assert!(mesh.vertices.len() >= 3);
    assert_eq!(
        flat(&mesh.vertices),
        vec![
            -2.0,
            -20.0,
            13.0,
            -16.0,
            13.0,
            15.0,
            -2.0,
            19.0,
            -14.0,
            15.0,
            -14.0,
            -16.0,
            -1.1176472,
            -10.823529,
            -0.23529434,
            -1.6470585,
        ]
    );
    assert_eq!(mesh.triangles.len(), 8);
    assert_valid(&mesh);
    // Sampling every 12 pixels skips the bar's top-right corner, so contour meshes do not promise
    // full coverage of the opaque area.
    assert_eq!(uncovered_opaque_pixels(&image, &mesh), 10);

    let points = contour(&image, &options, MeshTarget::Deformable, &mut budget())
        .unwrap()
        .unwrap();
    assert_eq!(points.vertices, mesh.vertices);
    assert!(points.triangles.is_empty());
}

#[test]
fn contour_of_a_ring_uses_only_the_outer_boundary() {
    let ring = mono(40, 40, |x, y| {
        let d = ((x as f32 - 20.0).powi(2) + (y as f32 - 20.0).powi(2)).sqrt();
        (8.0..=16.0).contains(&d)
    });
    let options = ContourOptions {
        sampling_step: 6.0,
        min_distance: 4.0,
        scales: vec![1.0],
        ..ContourOptions::default()
    };
    let mesh = contour(&ring, &options, MeshTarget::Drawable, &mut budget())
        .unwrap()
        .unwrap();
    assert_eq!(mesh.vertices.len(), 12);
    assert_eq!(mesh.triangles.len(), 10);
    for v in &mesh.vertices {
        let radius = (v[0] * v[0] + v[1] * v[1]).sqrt();
        assert!(
            (14.5..=16.5).contains(&radius),
            "vertex {v:?} is not on the outer boundary"
        );
    }
    assert_valid(&mesh);
}

#[test]
fn contour_of_an_l_shape_triangulates_its_six_corners() {
    let l = mono(30, 30, |x, y| {
        ((5..=10).contains(&x) && (4..=25).contains(&y))
            || ((5..=24).contains(&x) && (20..=25).contains(&y))
    });
    let options = ContourOptions {
        sampling_step: 4.0,
        min_distance: 2.0,
        scales: vec![1.0],
        ..ContourOptions::default()
    };
    let mesh = contour(&l, &options, MeshTarget::Drawable, &mut budget())
        .unwrap()
        .unwrap();
    assert_eq!(mesh.vertices.len(), 6);
    assert_eq!(mesh.triangles.len(), 5);
    assert_valid(&mesh);
    assert_eq!(uncovered_opaque_pixels(&l, &mesh), 0);
}

#[test]
fn contour_single_pixel_and_empty_images() {
    let options = ContourOptions::default();
    let one = mono(5, 5, |x, y| x == 2 && y == 2);
    let points = contour(&one, &options, MeshTarget::Deformable, &mut budget())
        .unwrap()
        .unwrap();
    assert_eq!(points.vertices, vec![[0.0, 0.0]]);
    // A Drawable needs three vertices, so D keeps the incoming mesh.
    assert_eq!(
        contour(&one, &options, MeshTarget::Drawable, &mut budget()).unwrap(),
        None
    );
    let empty = mono(5, 5, |_, _| false);
    assert_eq!(
        contour(&empty, &options, MeshTarget::Deformable, &mut budget()).unwrap(),
        None
    );
}

#[test]
fn contour_mirror_keeps_one_side_of_the_axis_after_the_start_vertex() {
    let shape = mono(60, 40, |x, y| {
        (5..=54).contains(&x)
            && (5..=34).contains(&y)
            && !((20..=40).contains(&x) && (15..=25).contains(&y))
    });
    let options = ContourOptions {
        sampling_step: 10.0,
        min_distance: 3.0,
        scales: vec![1.0],
        mirror_horizontal: true,
        ..ContourOptions::default()
    };
    let mesh = contour(&shape, &options, MeshTarget::Deformable, &mut budget())
        .unwrap()
        .unwrap();
    assert_eq!(
        flat(&mesh.vertices),
        vec![-25.0, -15.0, 24.0, -15.0, 24.0, 14.0]
    );
}

// --- skeleton -----------------------------------------------------------------------------------

#[test]
fn skeleton_on_regression_image_returns_the_target_point_count() {
    let options = SkeletonOptions {
        mask_threshold: 1.0,
        target_point_count: 6,
    };
    let image = regression_image();
    let points = skeleton(&image, &options, MeshTarget::Deformable, &mut budget())
        .unwrap()
        .unwrap();
    assert_eq!(
        flat(&points.vertices),
        vec![0.0, 16.0, -1.0, 7.0, -1.0, -3.0, -5.0, -7.0, -5.0, -12.0, 0.0, -17.0]
    );
    for v in &points.vertices {
        // Every control point lies on an opaque pixel.
        let (x, y) = ((v[0] + 24.0) as u32, (v[1] + 24.0) as u32);
        assert_eq!(image.pixels()[((y * 48 + x) * 4 + 3) as usize], 255);
    }
    let mesh = skeleton(&image, &options, MeshTarget::Drawable, &mut budget())
        .unwrap()
        .unwrap();
    assert_eq!(mesh.vertices, points.vertices);
    assert_eq!(mesh.triangles.len(), 3);
    assert_valid(&mesh);
}

#[test]
fn skeleton_degenerate_inputs() {
    let options = SkeletonOptions::default();
    let target = MeshTarget::Deformable;
    // A filled square thins to its centre pixel.
    let square = mono(32, 32, |x, y| {
        (8..=23).contains(&x) && (8..=23).contains(&y)
    });
    assert_eq!(
        skeleton(&square, &options, target, &mut budget())
            .unwrap()
            .unwrap()
            .vertices,
        vec![[-1.0, -1.0]]
    );
    assert_eq!(
        skeleton(&mono(5, 5, |_, _| false), &options, target, &mut budget()).unwrap(),
        None
    );
    let one = mono(5, 5, |x, y| x == 2 && y == 2);
    assert_eq!(
        skeleton(&one, &options, target, &mut budget())
            .unwrap()
            .unwrap()
            .vertices,
        vec![[0.0, 0.0]]
    );
    assert_eq!(
        skeleton(&one, &options, MeshTarget::Drawable, &mut budget()).unwrap(),
        None
    );
}

#[test]
fn skeleton_of_an_l_shape_is_simplified_to_four_points_keeping_both_ends() {
    let l = mono(30, 30, |x, y| {
        ((5..=10).contains(&x) && (4..=25).contains(&y))
            || ((5..=24).contains(&x) && (20..=25).contains(&y))
    });
    let options = SkeletonOptions {
        mask_threshold: 1.0,
        target_point_count: 4,
    };
    let points = skeleton(&l, &options, MeshTarget::Deformable, &mut budget())
        .unwrap()
        .unwrap();
    assert_eq!(
        flat(&points.vertices),
        vec![2.0, 7.0, 0.0, 9.0, -6.0, 8.0, -10.0, -1.0]
    );
}

// --- optimum ------------------------------------------------------------------------------------

#[test]
fn optimum_of_a_filled_square_is_a_sharp_part_with_slightly_expanded_corners() {
    let image = mono(32, 32, |x, y| {
        (8..=23).contains(&x) && (8..=23).contains(&y)
    });
    let mesh = optimum(
        &image,
        &OptimumOptions::default(),
        MeshTarget::Drawable,
        &mut budget(),
    )
    .unwrap()
    .unwrap();
    // The traced contour closes on its start point, so the start corner appears twice and is pushed
    // out along two different chord normals by 0.01 * 32 pixels.
    approx(
        &flat(&mesh.vertices),
        &[
            -8.0, -8.32, 7.2262745, -8.2262745, 7.2262745, 7.2262745, -8.2262745, 7.2262745, -8.32,
            -8.0,
        ],
    );
    assert_eq!(mesh.triangles, vec![[0, 2, 1], [3, 2, 4], [0, 4, 2]]);
    assert_valid(&mesh);
    assert_eq!(uncovered_opaque_pixels(&image, &mesh), 0);
}

#[test]
fn optimum_on_regression_image_matches_the_d_test_parameters() {
    let options = OptimumOptions {
        scales: vec![0.5, 0.0],
        min_distance: 4.0,
        mask_threshold: 1.0,
        div_per_part: 8.0,
        large_threshold: 50.0,
        length_threshold: 20.0,
        ratio_threshold: 0.05,
        ..OptimumOptions::default()
    };
    let image = regression_image();
    let mesh = optimum(&image, &options, MeshTarget::Drawable, &mut budget())
        .unwrap()
        .unwrap();
    assert!(mesh.vertices.len() >= 3);
    assert_eq!(mesh.vertices.len(), 33);
    assert_eq!(mesh.triangles.len(), 57);
    approx(&mesh.vertices[0], &[-2.0, -20.0]);
    approx(&mesh.vertices[1], &[3.0, -16.0]);
    assert_valid(&mesh);
    assert_eq!(uncovered_opaque_pixels(&image, &mesh), 0);
    let points = optimum(&image, &options, MeshTarget::Deformable, &mut budget())
        .unwrap()
        .unwrap();
    assert_eq!(points.vertices, mesh.vertices);
    assert!(points.triangles.is_empty());
}

#[test]
fn optimum_wide_part_adds_expanded_contracted_and_scaled_rings() {
    let disk = mono(80, 80, |x, y| {
        ((x as f32 - 40.0).powi(2) + (y as f32 - 40.0).powi(2)).sqrt() <= 30.0
    });
    let sharp = optimum(
        &disk,
        &OptimumOptions::default(),
        MeshTarget::Drawable,
        &mut budget(),
    )
    .unwrap()
    .unwrap();
    assert_eq!((sharp.vertices.len(), sharp.triangles.len()), (20, 19));
    let thick = OptimumOptions {
        length_threshold: 20.0,
        ratio_threshold: 0.01,
        ..OptimumOptions::default()
    };
    let mesh = optimum(&disk, &thick, MeshTarget::Drawable, &mut budget())
        .unwrap()
        .unwrap();
    assert_eq!((mesh.vertices.len(), mesh.triangles.len()), (54, 92));
    approx(&mesh.vertices[0], &[0.0, -30.0]);
    assert_indices(&mesh);
    assert_eq!(uncovered_opaque_pixels(&disk, &mesh), 0);
    let mirrored = OptimumOptions {
        mirror_horizontal: true,
        ..thick
    };
    let mirrored = optimum(&disk, &mirrored, MeshTarget::Drawable, &mut budget())
        .unwrap()
        .unwrap();
    // Mirroring reflects points onto each other and may leave collinear triangles; indices stay valid.
    assert_indices(&mirrored);
    assert!(!mirrored.triangles.is_empty());
}

#[test]
fn optimum_ring_and_l_shape_cover_their_opaque_pixels() {
    let ring = mono(120, 120, |x, y| {
        let d = ((x as f32 - 60.0).powi(2) + (y as f32 - 60.0).powi(2)).sqrt();
        (25.0..=50.0).contains(&d)
    });
    let l = mono(90, 90, |x, y| {
        ((15..=34).contains(&x) && (10..=79).contains(&y))
            || ((15..=74).contains(&x) && (60..=79).contains(&y))
    });
    let thick = OptimumOptions {
        length_threshold: 20.0,
        ratio_threshold: 0.01,
        ..OptimumOptions::default()
    };
    // (image, options, vertices, triangles, collapsed triangles). Scaled rings repeat vertex positions
    // because, like D's consecutive-only `uniq`, the dedup here only drops adjacent duplicates; the
    // duplicates then collapse some triangles.
    let cases = [
        (&ring, OptimumOptions::default(), 29, 27, 0),
        (&ring, thick.clone(), 103, 172, 2),
        (&l, OptimumOptions::default(), 26, 40, 2),
        (&l, thick, 26, 40, 2),
    ];
    for (image, options, vertices, triangles, collapsed) in cases {
        let mesh = optimum(image, &options, MeshTarget::Drawable, &mut budget())
            .unwrap()
            .unwrap();
        assert_eq!(
            (mesh.vertices.len(), mesh.triangles.len()),
            (vertices, triangles)
        );
        assert_indices(&mesh);
        assert_eq!(
            mesh.triangles
                .iter()
                .filter(|t| area2(&mesh, t).abs() <= 1e-3)
                .count(),
            collapsed
        );
        assert_eq!(uncovered_opaque_pixels(image, &mesh), 0);
    }
}

#[test]
fn optimum_nothing_to_mesh_for_empty_and_single_pixel_images() {
    let options = OptimumOptions::default();
    assert_eq!(
        optimum(
            &mono(9, 9, |_, _| false),
            &options,
            MeshTarget::Drawable,
            &mut budget()
        )
        .unwrap(),
        None
    );
    assert_eq!(
        optimum(
            &mono(9, 9, |x, y| x == 4 && y == 4),
            &options,
            MeshTarget::Deformable,
            &mut budget()
        )
        .unwrap(),
        None
    );
}

// --- shared behaviour ---------------------------------------------------------------------------

#[test]
fn rgba_alpha_channel_and_mono_inputs_agree_and_rgb_is_rejected() {
    let rgba = regression_image();
    let alpha: Vec<u8> = rgba.pixels().chunks_exact(4).map(|c| c[3]).collect();
    let as_mono = PixelImage::from_pixels(48, 48, PixelFormat::Mono, alpha).unwrap();
    let options = GridOptions::default();
    assert_eq!(
        grid(&rgba, &options, &mut budget()),
        grid(&as_mono, &options, &mut budget())
    );
    let rgb = PixelImage::from_pixels(2, 2, PixelFormat::Rgb, vec![255; 12]).unwrap();
    assert!(grid(&rgb, &options, &mut budget())
        .unwrap_err()
        .contains("alpha"));
}

#[test]
fn mask_threshold_decides_which_alpha_values_count_as_opaque() {
    let faint = PixelImage::from_pixels(
        16,
        16,
        PixelFormat::Mono,
        (0..256)
            .map(|i| {
                if (4..12).contains(&(i % 16)) && (4..12).contains(&(i / 16)) {
                    10
                } else {
                    0
                }
            })
            .collect(),
    )
    .unwrap();
    let low = GridOptions {
        mask_threshold: 10.0,
        ..GridOptions::default()
    };
    let high = GridOptions {
        mask_threshold: 11.0,
        ..GridOptions::default()
    };
    assert!(grid(&faint, &low, &mut budget()).unwrap().is_some());
    assert_eq!(grid(&faint, &high, &mut budget()).unwrap(), None);
}

#[test]
fn invalid_options_are_rejected_with_errors() {
    let image = regression_image();
    let nan = f32::NAN;
    let grid_cases = [
        GridOptions {
            mask_threshold: 0.0,
            ..GridOptions::default()
        },
        GridOptions {
            mask_threshold: 201.0,
            ..GridOptions::default()
        },
        GridOptions {
            x_segments: 1,
            ..GridOptions::default()
        },
        GridOptions {
            y_segments: 21,
            ..GridOptions::default()
        },
        GridOptions {
            margin: 1.5,
            ..GridOptions::default()
        },
        GridOptions {
            margin: nan,
            ..GridOptions::default()
        },
        GridOptions {
            scale_x: Some(vec![0.5]),
            ..GridOptions::default()
        },
        GridOptions {
            scale_y: Some(vec![0.0, nan]),
            ..GridOptions::default()
        },
        GridOptions {
            scale_y: Some(vec![0.0; 256]),
            ..GridOptions::default()
        },
    ];
    for options in &grid_cases {
        assert!(grid(&image, options, &mut budget()).is_err(), "{options:?}");
    }
    let contour_cases = [
        ContourOptions {
            sampling_step: 0.5,
            ..ContourOptions::default()
        },
        ContourOptions {
            min_distance: 201.0,
            ..ContourOptions::default()
        },
        ContourOptions {
            max_distance: -2.0,
            ..ContourOptions::default()
        },
        ContourOptions {
            scales: vec![2.5],
            ..ContourOptions::default()
        },
        ContourOptions {
            scales: vec![-0.5],
            ..ContourOptions::default()
        },
        ContourOptions {
            scales: vec![1.0; 65],
            ..ContourOptions::default()
        },
        ContourOptions {
            mirror_axis: f32::INFINITY,
            ..ContourOptions::default()
        },
    ];
    for options in &contour_cases {
        assert!(
            contour(&image, options, MeshTarget::Drawable, &mut budget()).is_err(),
            "{options:?}"
        );
    }
    let optimum_cases = [
        OptimumOptions {
            large_threshold: 10.0,
            ..OptimumOptions::default()
        },
        OptimumOptions {
            ratio_threshold: 0.0,
            ..OptimumOptions::default()
        },
        OptimumOptions {
            sharp_expand: 0.3,
            ..OptimumOptions::default()
        },
        OptimumOptions {
            unsharp_contract: -0.1,
            ..OptimumOptions::default()
        },
        OptimumOptions {
            div_per_part: 3.0,
            ..OptimumOptions::default()
        },
        OptimumOptions {
            mask_threshold: nan,
            ..OptimumOptions::default()
        },
    ];
    for options in &optimum_cases {
        assert!(
            optimum(&image, options, MeshTarget::Drawable, &mut budget()).is_err(),
            "{options:?}"
        );
    }
    for options in [
        SkeletonOptions {
            mask_threshold: 15.0,
            target_point_count: 1,
        },
        SkeletonOptions {
            mask_threshold: 15.0,
            target_point_count: 129,
        },
    ] {
        assert!(
            skeleton(&image, &options, MeshTarget::Deformable, &mut budget()).is_err(),
            "{options:?}"
        );
    }
}

#[test]
fn defaults_and_presets_match_the_d_values_and_validate() {
    let contour_default = ContourOptions::default();
    assert_eq!(
        (
            contour_default.sampling_step,
            contour_default.min_distance,
            contour_default.max_distance
        ),
        (32.0, 16.0, -1.0)
    );
    assert_eq!(
        contour_default.scales,
        vec![1.0, 1.1, 0.9, 0.7, 0.4, 0.2, 0.1]
    );
    let optimum_default = OptimumOptions::default();
    assert_eq!(
        (
            optimum_default.large_threshold,
            optimum_default.mask_threshold,
            optimum_default.div_per_part
        ),
        (400.0, 1.0, 12.0)
    );
    assert_eq!(optimum_default.scales, vec![0.5, 0.0]);
    let skeleton_default = SkeletonOptions::default();
    assert_eq!(
        (
            skeleton_default.mask_threshold,
            skeleton_default.target_point_count
        ),
        (15.0, 10)
    );
    assert_eq!(
        (
            GridOptions::default().margin,
            GridOptions::default().x_segments
        ),
        (0.1, 2)
    );

    let names = [
        "Normal parts",
        "Detailed mesh",
        "Large parts",
        "Small parts",
        "Thin and minimum parts",
        "Preserve edges",
    ];
    for name in names {
        ContourOptions::preset(name).unwrap().validate().unwrap();
        OptimumOptions::preset(name).unwrap().validate().unwrap();
    }
    assert!(ContourOptions::preset("nope").is_none() && OptimumOptions::preset("nope").is_none());
    let thin = ContourOptions::preset("Thin and minimum parts").unwrap();
    assert_eq!(
        (
            thin.sampling_step,
            thin.mask_threshold,
            thin.min_distance,
            thin.max_distance
        ),
        (12.0, 1.0, 4.0, 24.0)
    );
    assert_eq!(thin.scales, vec![1.0]);
    let large = OptimumOptions::preset("Large parts").unwrap();
    assert_eq!(
        (
            large.div_per_part,
            large.min_distance,
            large.large_threshold,
            large.length_threshold
        ),
        (8.0, 24.0, 600.0, 200.0)
    );
    assert_eq!(
        (
            large.ratio_threshold,
            large.sharp_expand,
            large.unsharp_expand,
            large.unsharp_contract
        ),
        (0.30, 0.015, 0.080, 0.080)
    );
}

#[test]
fn work_budget_bounds_every_generator_and_generate_dispatches() {
    let image = regression_image();
    let algorithms = [
        Algorithm::Grid(GridOptions::default()),
        Algorithm::Contour(ContourOptions::default()),
        Algorithm::Optimum(OptimumOptions::default()),
        Algorithm::Skeleton(SkeletonOptions::default()),
    ];
    for algorithm in &algorithms {
        let result = generate(
            &image,
            algorithm,
            MeshTarget::Drawable,
            &mut WorkBudget::new(10),
        );
        assert_eq!(result.unwrap_err(), "work budget exhausted");
        let full = generate(&image, algorithm, MeshTarget::Drawable, &mut budget())
            .unwrap()
            .unwrap();
        assert_valid(&full);
    }
    let direct = grid(&image, &GridOptions::default(), &mut budget()).unwrap();
    assert_eq!(
        generate(
            &image,
            &algorithms[0],
            MeshTarget::Deformable,
            &mut budget()
        )
        .unwrap(),
        direct
    );
}

#[test]
fn generate_handles_degenerate_images_without_panicking() {
    let widths = [(1, 1), (1, 7), (7, 1), (2, 2), (3, 3)];
    for (w, h) in widths {
        for fill in [false, true] {
            let image = mono(w, h, |_, _| fill);
            for algorithm in [
                Algorithm::Grid(GridOptions::default()),
                Algorithm::Contour(ContourOptions::default()),
                Algorithm::Optimum(OptimumOptions::default()),
                Algorithm::Skeleton(SkeletonOptions::default()),
            ] {
                for target in [MeshTarget::Drawable, MeshTarget::Deformable] {
                    if let Some(mesh) = generate(&image, &algorithm, target, &mut budget()).unwrap()
                    {
                        assert_valid(&mesh);
                    }
                }
            }
        }
    }
}
