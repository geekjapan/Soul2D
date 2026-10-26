//! Known answers for the 3D coordinate conventions and humanoid tables of `doc/3d-conversion-conventions.md`.
//! A converter must give these answers. `gltf_rig` is the first one.

use crate::gltf_rig::{Convention, HUMANOID};
use crate::mmd_motion::{mmd_position_to_rig, BONE_TABLE};
use crate::skeleton::STANDARD_BONE_NAMES;
use serde_json::Value;

fn triple(value: &Value) -> [f64; 3] {
    std::array::from_fn(|axis| value[axis].as_f64().unwrap())
}

fn quaternion(value: &Value) -> [f64; 4] {
    std::array::from_fn(|axis| value[axis].as_f64().unwrap())
}

/// The point `v` turned by the unit quaternion (x, y, z, w), by the standard rotation matrix.
fn rotate(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    let [x, y, z, w] = q;
    [
        (1.0 - 2.0 * (y * y + z * z)) * v[0] + 2.0 * (x * y - z * w) * v[1] + 2.0 * (x * z + y * w) * v[2],
        2.0 * (x * y + z * w) * v[0] + (1.0 - 2.0 * (x * x + z * z)) * v[1] + 2.0 * (y * z - x * w) * v[2],
        2.0 * (x * z - y * w) * v[0] + 2.0 * (y * z + x * w) * v[1] + (1.0 - 2.0 * (x * x + y * y)) * v[2],
    ]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("../tests/fixtures/conversion3d/conventions.json")).unwrap()
}

#[test]
fn every_convention_puts_up_at_minus_y_and_the_front_toward_the_viewer() {
    let fixture = fixture();
    let conventions = fixture["conventions"].as_array().unwrap();
    assert_eq!(conventions.iter().map(|row| row["name"].as_str().unwrap()).collect::<Vec<_>>(),
        ["gltf2", "vrm1", "vrm0", "mmd"]);
    for row in conventions {
        let name = row["name"].as_str().unwrap();
        let signs = triple(&row["toRig"]);
        let to_rig = |v: [f64; 3]| [signs[0] * v[0], signs[1] * v[1], signs[2] * v[2]];
        assert_eq!(to_rig(triple(&row["up"])), [0.0, -1.0, 0.0], "{name}");
        assert_eq!(to_rig(triple(&row["front"])), [0.0, 0.0, 1.0], "{name}");
        // A source of the rig's handedness needs no reflection, any other needs one.
        let determinant = signs[0] * signs[1] * signs[2];
        let same_handedness = row["handedness"] == fixture["rig"]["handedness"];
        assert_eq!(determinant, if same_handedness { 1.0 } else { -1.0 }, "{name}");
        // The viewer's right: up cross front in a right-handed frame, front cross up in a left-handed one.
        let (up, front) = (triple(&row["up"]), triple(&row["front"]));
        let right = if row["handedness"] == "right" { cross(up, front) } else { cross(front, up) };
        assert_eq!(to_rig(right), [1.0, 0.0, 0.0], "{name}");

        for point in row["points"].as_array().unwrap() {
            assert_eq!(to_rig(triple(&point["source"])), triple(&point["rig"]), "{name}");
        }
        for triangle in row["triangles"].as_array().unwrap() {
            let [a, b, c]: [[f64; 3]; 3] = std::array::from_fn(|corner| triple(&triangle["source"][corner]));
            let normal = |a: [f64; 3], b: [f64; 3], c: [f64; 3]| {
                cross(std::array::from_fn(|axis| b[axis] - a[axis]), std::array::from_fn(|axis| c[axis] - a[axis]))
            };
            let converted = normal(to_rig(a), to_rig(b), to_rig(c))[2];
            assert_eq!(converted, triangle["rigCrossZ"].as_f64().unwrap(), "{name}");
        }
        for turn in row["quaternions"].as_array().unwrap() {
            let (source, rig) = (quaternion(&turn["source"]), quaternion(&turn["rig"]));
            // A reflection turns the axis as a pseudovector: the vector part takes the determinant as well.
            let expected = [determinant * signs[0] * source[0], determinant * signs[1] * source[1],
                determinant * signs[2] * source[2], source[3]];
            assert_eq!(rig, expected, "{name}");
            // Turning then converting equals converting then turning.
            for probe in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.3, -0.7, 0.2]] {
                let (turned, converted) = (to_rig(rotate(source, probe)), rotate(rig, to_rig(probe)));
                for axis in 0..3 {
                    assert!((turned[axis] - converted[axis]).abs() < 1e-12, "{name} {turned:?} {converted:?}");
                }
            }
        }
    }
    let mmd = &conventions[3]["points"][0];
    assert_eq!(mmd_position_to_rig(triple(&mmd["source"])), triple(&mmd["rig"]));
    // The glTF rig import uses the signs of its three rows.
    for convention in [Convention::Gltf2, Convention::Vrm1, Convention::Vrm0] {
        let row = conventions.iter().find(|row| row["name"] == convention.name()).unwrap();
        assert_eq!(convention.signs(), triple(&row["toRig"]), "{convention:?}");
    }

    let scale = &fixture["scale"];
    let units = scale["unitsPerSourceUnit"].as_f64().unwrap();
    assert_eq!(units, scale["rigSpan"].as_f64().unwrap() / scale["sourceSpan"].as_f64().unwrap());
    let row = conventions.iter().find(|row| row["name"] == scale["convention"]).unwrap();
    let (signs, source) = (triple(&row["toRig"]), triple(&scale["source"]));
    assert_eq!(triple(&scale["rig"]), std::array::from_fn(|axis| signs[axis] * source[axis] * units));
}

#[test]
fn humanoid_tables_name_each_standard_bone_once_and_agree_with_the_mmd_table_on_sides() {
    let fixture = fixture();
    for (table, rows) in fixture["humanoid"].as_object().unwrap() {
        let mut targets: Vec<&str> = rows.as_object().unwrap().values().map(|bone| bone.as_str().unwrap()).collect();
        targets.sort_unstable();
        let mut standard = STANDARD_BONE_NAMES.to_vec();
        standard.sort_unstable();
        assert_eq!(targets, standard, "{table}");
        let rows = rows.as_object().unwrap();
        for (source, bone) in rows {
            let bone = bone.as_str().unwrap();
            let lower = source.to_lowercase();
            // The character's left is the right of the picture, as in the MMD table.
            match (lower.strip_prefix("left"), lower.starts_with("right")) {
                (Some(part), _) => {
                    assert!(bone.ends_with(".R"), "{table} {source} {bone}");
                    // The right bone of the same part maps to the same standard bone on the other side.
                    let twin = rows.iter().find(|(name, _)| name.to_lowercase() == format!("right{part}")).unwrap().1;
                    assert_eq!(twin.as_str().unwrap(), bone.replace(".R", ".L"), "{table} {source}");
                }
                (None, true) => assert!(bone.ends_with(".L"), "{table} {source} {bone}"),
                (None, false) => assert!(!bone.contains('.'), "{table} {source} {bone}"),
            }
        }
    }
    for row in BONE_TABLE.iter().filter_map(|row| Some((row.mmd, row.target?.bone))) {
        match row.0.chars().next() {
            Some('左') => assert!(row.1.ends_with(".R"), "{row:?}"),
            Some('右') => assert!(row.1.ends_with(".L"), "{row:?}"),
            _ => assert!(!row.1.contains('.'), "{row:?}"),
        }
    }
    // The glTF rig import reads both tables from one array.
    for (column, table) in ["vrm", "mixamo"].into_iter().enumerate() {
        let rows = fixture["humanoid"][table].as_object().unwrap();
        assert_eq!(rows.len(), HUMANOID.len(), "{table}");
        for (bone, vrm, mixamo) in HUMANOID {
            assert_eq!(rows[[vrm, mixamo][column]], bone, "{table}");
        }
    }
    let left_arm = BONE_TABLE.iter().find(|row| row.mmd == "左腕").unwrap();
    assert_eq!(left_arm.target.unwrap().bone, fixture["humanoid"]["vrm"]["leftUpperArm"]);
}
