use soul2d_core::atlas::{build_atlases, possible_packing_ratio, AtlasPart, AtlasSettings, Pin};
use soul2d_core::cv::WorkBudget;
use soul2d_core::pixels::{PixelFormat, PixelImage};

fn image(width: u32, height: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> PixelImage {
    let mut pixels = Vec::new();
    for y in 0..height {
        for x in 0..width {
            pixels.extend(f(x, y));
        }
    }
    PixelImage::from_pixels(width, height, PixelFormat::Rgba, pixels).unwrap()
}

// Distinct per-texel values: R = 10 * index, G = 50 * x, B = 50 * y, opaque.
fn ramp(width: u32, height: u32) -> PixelImage {
    image(width, height, |x, y| [((y * width + x) * 10) as u8, (x * 50) as u8, (y * 50) as u8, 255])
}

// A linear field, so bilinear averages are exact.
fn field(width: u32, height: u32) -> PixelImage {
    image(width, height, |x, y| [(x * 40 + y * 20) as u8, 0, 0, 255])
}

const FULL: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];

fn part<'a>(albedo: &'a PixelImage, uvs: &'a [[f32; 2]], extent: f32) -> AtlasPart<'a> {
    AtlasPart { textures: [Some(albedo), None, None], uvs, bounds: [0.0, 0.0, extent, extent], pin: None }
}

fn settings(resolution: u32, padding: i32, scale: f32) -> AtlasSettings {
    AtlasSettings { resolution, padding, scale, non_linear_scaling: false }
}

fn budget() -> WorkBudget {
    WorkBudget::new(10_000_000)
}

fn at(image: &PixelImage, x: u32, y: u32) -> [u8; 4] {
    let width = image.size()[0] as usize;
    let i = (y as usize * width + x as usize) * 4;
    image.pixels()[i..i + 4].try_into().unwrap()
}

#[test]
fn identity_copy_with_padding_and_uv_rewrite() {
    let src = ramp(4, 4);
    let parts = [part(&src, &FULL, 4.0)];
    let build = build_atlases(&parts, &settings(8, 1, 1.0), &mut budget()).unwrap();
    assert_eq!(build.atlases.len(), 1);
    assert_eq!(build.output_scale, 1.0);
    let albedo = build.atlases[0].slots[0].as_ref().unwrap();
    assert!(build.atlases[0].slots[1].is_none() && build.atlases[0].slots[2].is_none());
    assert_eq!(albedo.size(), [8, 8]);
    // The 6x6 slot at (0,0) holds a 1 pixel transparent border around the 4x4 copy.
    assert_eq!(at(albedo, 0, 0), [0, 0, 0, 0]);
    assert_eq!(at(albedo, 1, 1), [0, 0, 0, 255]);
    assert_eq!(at(albedo, 4, 1), [30, 150, 0, 255]);
    assert_eq!(at(albedo, 1, 4), [120, 0, 150, 255]);
    assert_eq!(at(albedo, 4, 4), [150, 150, 150, 255]);
    assert_eq!(at(albedo, 5, 4), [0, 0, 0, 0]);
    assert_eq!(at(albedo, 4, 5), [0, 0, 0, 0]);
    assert_eq!(at(albedo, 7, 7), [0, 0, 0, 0]);
    let placement = &build.parts[0];
    assert_eq!(placement.atlas, 0);
    assert_eq!(placement.mapping, [1, 1, 4, 4]);
    assert_eq!(placement.uvs, vec![[0.125, 0.125], [0.625, 0.125], [0.625, 0.625], [0.125, 0.625]]);
}

#[test]
fn half_scale_resamples_bilinearly() {
    let src = field(4, 4);
    let parts = [part(&src, &FULL, 4.0)];
    // size = 4 * 0.5 + 2 padding = 4; the copy is a 2x2 quad at (1,1).
    let build = build_atlases(&parts, &settings(8, 1, 0.5), &mut budget()).unwrap();
    let albedo = build.atlases[0].slots[0].as_ref().unwrap();
    assert_eq!(at(albedo, 1, 1), [30, 0, 0, 255]);
    assert_eq!(at(albedo, 2, 1), [110, 0, 0, 255]);
    assert_eq!(at(albedo, 1, 2), [70, 0, 0, 255]);
    assert_eq!(at(albedo, 2, 2), [150, 0, 0, 255]);
    assert_eq!(at(albedo, 0, 0), [0, 0, 0, 0]);
    assert_eq!(at(albedo, 3, 3), [0, 0, 0, 0]);
    assert_eq!(build.parts[0].mapping, [1, 1, 2, 2]);
    assert_eq!(build.parts[0].uvs, vec![[0.125, 0.125], [0.375, 0.125], [0.375, 0.375], [0.125, 0.375]]);
}

#[test]
fn uv_subrect_crops_the_texture() {
    let src = field(4, 4);
    let uvs = [[0.5, 0.0], [1.0, 0.0], [1.0, 0.5], [0.5, 0.5]];
    let parts = [part(&src, &uvs, 2.0)];
    let build = build_atlases(&parts, &settings(8, 0, 1.0), &mut budget()).unwrap();
    let albedo = build.atlases[0].slots[0].as_ref().unwrap();
    assert_eq!(at(albedo, 0, 0), [80, 0, 0, 255]); // src(2,0)
    assert_eq!(at(albedo, 1, 0), [120, 0, 0, 255]); // src(3,0)
    assert_eq!(at(albedo, 0, 1), [100, 0, 0, 255]); // src(2,1)
    assert_eq!(at(albedo, 1, 1), [140, 0, 0, 255]); // src(3,1)
    assert_eq!(at(albedo, 2, 0), [0, 0, 0, 0]);
    assert_eq!(build.parts[0].mapping, [0, 0, 2, 2]);
    assert_eq!(build.parts[0].uvs, vec![[0.0, 0.0], [0.25, 0.0], [0.25, 0.25], [0.0, 0.25]]);
}

#[test]
fn uvs_outside_unit_range_leave_a_transparent_margin() {
    let src = ramp(4, 4);
    let uvs = [[-0.5, 0.0], [1.0, 0.0], [1.0, 1.0], [0.25, 1.0]];
    let parts = [part(&src, &uvs, 4.0)];
    let build = build_atlases(&parts, &settings(8, 0, 1.0), &mut budget()).unwrap();
    let albedo = build.atlases[0].slots[0].as_ref().unwrap();
    // 1.5 texture widths wide: 2 empty columns, then the texture.
    assert_eq!(build.parts[0].mapping, [0, 0, 6, 4]);
    assert_eq!(at(albedo, 0, 0), [0, 0, 0, 0]);
    assert_eq!(at(albedo, 1, 3), [0, 0, 0, 0]);
    assert_eq!(at(albedo, 2, 0), [0, 0, 0, 255]);
    assert_eq!(at(albedo, 5, 3), [150, 150, 150, 255]);
    // uv.x 0.25 sits one pixel into the texture, i.e. column 3 of 8.
    assert_eq!(build.parts[0].uvs[3], [0.375, 1.0 * 4.0 / 8.0]);
    assert_eq!(build.parts[0].uvs[0], [0.0, 0.0]);
    assert_eq!(build.parts[0].uvs[1], [0.75, 0.0]);
}

#[test]
fn parts_pack_largest_first_and_fill_extra_slots() {
    let small = ramp(4, 2);
    let big = ramp(4, 4);
    let emissive = image(4, 4, |_, _| [9, 8, 7, 255]);
    // Input order is small, big; D sorts by width + height, descending.
    let parts = [
        part(&small, &FULL, 4.0),
        AtlasPart { textures: [Some(&big), Some(&emissive), None], uvs: &FULL, bounds: [0.0, 0.0, 4.0, 4.0], pin: None },
    ];
    let build = build_atlases(&parts, &settings(8, 0, 1.0), &mut budget()).unwrap();
    assert_eq!(build.parts[1].mapping, [0, 0, 4, 4]);
    assert_eq!(build.parts[0].mapping, [4, 0, 4, 2]);
    assert_eq!((build.parts[0].atlas, build.parts[1].atlas), (0, 0));
    let slots = &build.atlases[0].slots;
    assert!(slots[2].is_none());
    let emissive_atlas = slots[1].as_ref().unwrap();
    assert_eq!(at(emissive_atlas, 3, 3), [9, 8, 7, 255]);
    // The small part has no emissive texture: its region stays transparent.
    assert_eq!(at(emissive_atlas, 4, 0), [0, 0, 0, 0]);
    assert_eq!(at(slots[0].as_ref().unwrap(), 7, 1), [70, 150, 50, 255]);
}

#[test]
fn overflowing_parts_spill_into_a_second_atlas() {
    let big = ramp(4, 4);
    let small = ramp(2, 2);
    let parts = [part(&small, &FULL, 2.0), part(&big, &FULL, 2.0)];
    let build = build_atlases(&parts, &settings(4, 0, 1.0), &mut budget()).unwrap();
    assert_eq!(build.atlases.len(), 2);
    assert_eq!((build.parts[1].atlas, build.parts[0].atlas), (0, 1));
    assert_eq!(at(build.atlases[0].slots[0].as_ref().unwrap(), 3, 3), [150, 150, 150, 255]);
    let second = build.atlases[1].slots[0].as_ref().unwrap();
    assert_eq!(at(second, 1, 1), [30, 50, 50, 255]);
    assert_eq!(at(second, 2, 0), [0, 0, 0, 0]);
}

#[test]
fn texture_larger_than_the_atlas_fails() {
    let huge = ramp(8, 8);
    // The bounds claim a 2x2 part, so no scale-down happens and the 8x8 texture cannot fit.
    let parts = [part(&huge, &FULL, 2.0)];
    let err = build_atlases(&parts, &settings(4, 0, 1.0), &mut budget()).err().unwrap();
    assert_eq!(err, "a texture is too big for the atlas");
    // Padding eats the room too.
    let tiny = ramp(2, 2);
    let parts = [part(&tiny, &FULL, 2.0)];
    assert!(build_atlases(&parts, &settings(4, 2, 1.0), &mut budget()).is_err());
}

#[test]
fn linear_and_non_linear_scale_to_fit_differently() {
    let big = image(16, 16, |x, _| [x as u8, 0, 0, 255]);
    let small = ramp(4, 4);
    let parts = [part(&big, &FULL, 16.0), part(&small, &FULL, 4.0)];
    let linear = settings(8, 0, 1.0);
    let ratio = possible_packing_ratio(&parts, &linear);
    assert_eq!(ratio, (0.5f64 - 0.015) as f32);
    // Everything shrinks by 0.485: 16 -> 7, 4 -> 1.
    let build = build_atlases(&parts, &linear, &mut budget()).unwrap();
    assert_eq!(build.output_scale, ratio);
    assert_eq!(build.parts[0].mapping, [0, 0, 7, 7]);
    assert_eq!(build.parts[1].mapping, [0, 7, 1, 1]);
    assert_eq!(build.atlases.len(), 1);

    // Non-linear only shrinks the oversized part (0.49): 16 -> 7, the 4x4 stays 4 and spills over.
    let non_linear = AtlasSettings { non_linear_scaling: true, ..linear };
    assert_eq!(possible_packing_ratio(&parts, &non_linear), 1.0);
    let build = build_atlases(&parts, &non_linear, &mut budget()).unwrap();
    assert_eq!(build.output_scale, 1.0);
    assert_eq!(build.parts[0].mapping, [0, 0, 7, 7]);
    assert_eq!((build.parts[1].atlas, build.parts[1].mapping), (1, [0, 0, 4, 4]));
    assert_eq!(at(build.atlases[1].slots[0].as_ref().unwrap(), 3, 3), [150, 150, 150, 255]);
}

#[test]
fn malformed_input_is_rejected() {
    let src = ramp(4, 4);
    let ok = settings(8, 0, 1.0);
    let run = |parts: &[AtlasPart], s: &AtlasSettings| build_atlases(parts, s, &mut budget()).err();

    assert!(run(&[], &ok).is_some());
    let zero = PixelImage::from_pixels(0, 4, PixelFormat::Rgba, vec![]).unwrap();
    assert!(run(&[part(&zero, &FULL, 4.0)], &ok).unwrap().contains("non-empty RGBA"));
    let mono = PixelImage::zeroed(4, 4, PixelFormat::Mono).unwrap();
    assert!(run(&[part(&mono, &FULL, 4.0)], &ok).unwrap().contains("non-empty RGBA"));
    let no_albedo = AtlasPart { textures: [None, Some(&src), None], uvs: &FULL, bounds: [0.0; 4], pin: None };
    assert!(run(&[no_albedo], &ok).unwrap().contains("no albedo"));
    assert!(run(&[part(&src, &[], 4.0)], &ok).unwrap().contains("uvs"));
    let nan = [[f32::NAN, 0.0]];
    assert!(run(&[part(&src, &nan, 4.0)], &ok).unwrap().contains("uvs"));
    let line = [[0.25, 0.0], [0.25, 1.0]];
    assert!(run(&[part(&src, &line, 4.0)], &ok).unwrap().contains("no area"));
    let bad_bounds = AtlasPart { bounds: [0.0, 0.0, f32::INFINITY, 1.0], ..part(&src, &FULL, 4.0) };
    assert!(run(&[bad_bounds], &ok).unwrap().contains("bounds"));

    let one = [part(&src, &FULL, 4.0)];
    assert!(run(&one, &settings(8193, 0, 1.0)).unwrap().contains("resolution"));
    assert!(run(&one, &settings(0, 0, 1.0)).unwrap().contains("resolution"));
    assert!(run(&one, &settings(8, -1, 1.0)).unwrap().contains("padding"));
    assert!(run(&one, &settings(8, 9, 1.0)).unwrap().contains("padding"));
    assert!(run(&one, &settings(8, 0, f32::NAN)).unwrap().contains("scale"));
    assert!(run(&one, &settings(8, 0, 0.0)).unwrap().contains("scale"));
    assert!(run(&one, &settings(8, 0, 1000.0)).unwrap().contains("scale"));
    // A scale that squeezes the part to nothing is an error, not a panic.
    assert!(run(&one, &settings(8, 0, 0.1)).unwrap().contains("empty rectangle"));
}

#[test]
fn work_budget_caps_the_build() {
    let src = ramp(4, 4);
    let parts = [part(&src, &FULL, 4.0)];
    let err = build_atlases(&parts, &settings(8, 0, 1.0), &mut WorkBudget::new(10)).err().unwrap();
    assert_eq!(err, "work budget exhausted");
    // 64 atlas pixels + 16 drawn pixels + 4 uvs is exactly enough.
    assert!(build_atlases(&parts, &settings(8, 0, 1.0), &mut WorkBudget::new(84)).is_ok());
    assert!(build_atlases(&parts, &settings(8, 0, 1.0), &mut WorkBudget::new(83)).is_err());
}

#[test]
fn pinned_parts_keep_their_place_and_the_rest_packs_around_them() {
    let src = ramp(4, 4);
    let pinned = |atlas: usize, x: u32, y: u32, scale: Option<f32>| AtlasPart {
        pin: Some(Pin { atlas, x, y, scale }),
        ..part(&src, &FULL, 4.0)
    };
    let build = build_atlases(&[pinned(0, 8, 8, None), part(&src, &FULL, 4.0)], &settings(16, 0, 1.0), &mut budget())
        .unwrap();
    assert_eq!(build.parts[0].mapping, [8, 8, 4, 4]);
    let [x, y, ..] = build.parts[1].mapping;
    assert!(x + 4 <= 8 || x >= 12 || y + 4 <= 8 || y >= 12, "the free part packs around the pinned one: {x}, {y}");
    assert_eq!(at(build.atlases[0].slots[0].as_ref().unwrap(), 8, 8), at(&src, 0, 0));
    let doubled = build_atlases(&[pinned(1, 2, 2, Some(2.0))], &settings(16, 0, 1.0), &mut budget()).unwrap();
    assert_eq!((doubled.atlases.len(), doubled.parts[0].atlas, doubled.parts[0].mapping), (2, 1, [2, 2, 8, 8]));
    let crowded = build_atlases(&[pinned(0, 0, 0, None), pinned(0, 2, 2, None)], &settings(16, 0, 1.0), &mut budget());
    assert!(crowded.err().unwrap().contains("overlaps"));
    let outside = build_atlases(&[pinned(0, 14, 0, None)], &settings(16, 0, 1.0), &mut budget());
    assert!(outside.err().unwrap().contains("does not fit"));
    let padded = build_atlases(&[pinned(0, 0, 0, None)], &settings(16, 1, 1.0), &mut budget());
    assert!(padded.err().unwrap().contains("padding"));
}
