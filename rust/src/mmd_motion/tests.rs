//! Tests build every VMD and VPD stream here with a small writer; no third-party file is read. Shift-JIS byte
//! literals for the standard names were produced once with the host CP932 codec and are pinned as literals.

use super::sjis::decode;
use super::vmd::CHANNEL_ROTATION;
use super::*;
use super::retarget::quat_mul_wxyz;
use crate::bone_deform::{depth_edit_rotation_wxyz, rest_quat_wxyz, world_quat_wxyz};
use std::f64::consts::FRAC_PI_2;

// ---------------------------------------------------------------------------------------------------------------
// Writers

const CENTER: &[u8] = &[0x83, 0x5A, 0x83, 0x93, 0x83, 0x5E, 0x81, 0x5B]; // センター
const LEFT_ARM: &[u8] = &[0x8D, 0xB6, 0x98, 0x72]; // 左腕
const BLINK: &[u8] = &[0x82, 0xDC, 0x82, 0xCE, 0x82, 0xBD, 0x82, 0xAB]; // まばたき

fn padded(bytes: &[u8], width: usize) -> Vec<u8> {
    let mut out = bytes.to_vec();
    out.resize(width, 0);
    out
}

fn header(old: bool, model: &[u8]) -> Vec<u8> {
    let mut out = padded(if old { b"Vocaloid Motion Data file" } else { b"Vocaloid Motion Data 0002" }, 30);
    out.extend(padded(model, if old { 10 } else { 20 }));
    out
}

/// The default MMD curve, control points (20, 20) and (107, 107): exactly linear.
fn linear_interp() -> [u8; 64] {
    let mut block = [0u8; 64];
    for (index, byte) in block.iter_mut().enumerate() {
        *byte = [20, 20, 107, 107][(index / 4) % 4];
    }
    block
}

fn bone(name: &[u8], frame: u32, position: [f32; 3], rotation: [f32; 4], interp: [u8; 64]) -> Vec<u8> {
    let mut out = padded(name, 15);
    out.extend(frame.to_le_bytes());
    for value in position.iter().chain(rotation.iter()) {
        out.extend(value.to_le_bytes());
    }
    out.extend(interp);
    assert_eq!(out.len(), 111);
    out
}

fn morph(name: &[u8], frame: u32, weight: f32) -> Vec<u8> {
    let mut out = padded(name, 15);
    out.extend(frame.to_le_bytes());
    out.extend(weight.to_le_bytes());
    assert_eq!(out.len(), 23);
    out
}

fn section(records: &[Vec<u8>]) -> Vec<u8> {
    let mut out = (records.len() as u32).to_le_bytes().to_vec();
    for record in records {
        out.extend(record);
    }
    out
}

const IDENTITY: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

fn simple_bone(frame: u32) -> Vec<u8> {
    bone(CENTER, frame, [0.0; 3], IDENTITY, linear_interp())
}

// ---------------------------------------------------------------------------------------------------------------
// Shift-JIS

#[test]
fn sjis_rows_decode_known_byte_pairs() {
    for (bytes, expected) in [
        (&[0x82, 0xA0][..], "あ"),
        (&[0x82, 0x9F], "ぁ"),
        (&[0x82, 0xF1], "ん"),
        (&[0x83, 0x41], "ア"),
        (&[0x83, 0x93], "ン"),
        (&[0x83, 0x94], "ヴ"),
        (&[0x81, 0x40], "\u{3000}"),
        (&[0x81, 0x5B], "ー"),
        (&[0x81, 0x75], "「"),
        (&[0x81, 0x93], "％"),
        (&[0x81, 0x60], "\u{FF5E}"),
        (&[0x82, 0x4F], "０"),
        (&[0x82, 0x60], "Ａ"),
        (&[0x82, 0x81], "ａ"),
        (&[0x82, 0x68, 0x82, 0x6A], "ＩＫ"),
        (&[0xB1], "\u{FF71}"),
        (b"Ab 9", "Ab 9"),
    ] {
        assert_eq!(decode(bytes), expected, "{bytes:02X?}");
    }
}

#[test]
fn sjis_decodes_names_outside_the_standard_vocabulary() {
    for (bytes, expected) in [
        (&[0x88, 0x9F][..], "亜"),
        (&[0x93, 0xFA, 0x96, 0x7B], "日本"),
        // NEC row 13: circled digit one.
        (&[0x87, 0x40], "\u{2460}"),
        // IBM extension, the last cell of the code space.
        (&[0xFB, 0xFC], "\u{9AD9}"),
        // User-defined area: the codec maps it to the private use area.
        (&[0xF0, 0x40], "\u{E000}"),
        // 0x81AB (down arrow) is a cell of row 2 that the old hand-written decoder did not carry.
        (&[0x81, 0xAB], "\u{2193}"),
        // ASCII, half-width katakana and kanji in one name.
        (&[b'L', b'_', 0xB1, 0xB2, 0x93, 0xFA, 0x96, 0x7B, b'1'], "L_\u{FF71}\u{FF72}日本1"),
    ] {
        assert_eq!(decode(bytes), expected, "{bytes:02X?}");
    }
}

#[test]
fn sjis_unknown_sequences_become_replacement_characters() {
    // Pairs with a valid lead and trail byte that CP932 does not map: one U+FFFD for the pair, the next byte stands.
    assert_eq!(decode(&[0x85, 0x40]), "\u{FFFD}");
    assert_eq!(decode(&[0x81, 0xAD, b'X']), "\u{FFFD}X");
    assert_eq!(decode(&[0xFC, 0xFC]), "\u{FFFD}");
    // A lead byte at the end, a lead followed by an invalid trail (the trail decodes on its own), stray bytes.
    assert_eq!(decode(&[b'a', 0x82]), "a\u{FFFD}");
    assert_eq!(decode(&[0x82, 0x20]), "\u{FFFD} ");
    assert_eq!(decode(&[0x80, 0xA0, 0xFD, 0xFF]), "\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}");
}

#[test]
fn sjis_table_agrees_with_the_cp932_codec() {
    // The generator writes the count and a checksum of Python's `cp932` mapping of all 60 x 188 pairs (0 for a pair
    // the codec rejects); this reads the same pairs back through the decoder.
    let (mut mapped, mut checksum) = (0, 0x811C_9DC5u32);
    for lead in (0x81..=0x9Fu8).chain(0xE0..=0xFC) {
        for trail in (0x40..=0x7Eu8).chain(0x80..=0xFC) {
            let text = decode(&[lead, trail]);
            let mut chars = text.chars();
            let (code, rest) = (chars.next().unwrap(), chars.next());
            assert!(rest.is_none(), "{lead:02X}{trail:02X} decoded to {text:?}");
            let value = if code == '\u{FFFD}' { 0 } else { code as u32 };
            mapped += usize::from(value != 0);
            checksum = (checksum ^ value).wrapping_mul(0x0100_0193);
        }
    }
    assert_eq!(mapped, super::sjis_table::MAPPED_COUNT);
    assert_eq!(checksum, super::sjis_table::CHECKSUM);
}

#[test]
fn name_keeps_raw_bytes_and_stops_at_nul() {
    // 0x8540 is a pair that CP932 does not map.
    let name = MmdName::from_field(&[0x85, 0x40, b'X', 0, 0xFD, 0xFD]);
    assert_eq!(name.text, "\u{FFFD}X");
    assert_eq!(name.raw, vec![0x85, 0x40, b'X']);
    assert_eq!(name.hex(), "854058");
    assert!(name.matches_bytes(&[0x85, 0x40, b'X']));
    assert!(!name.is_fully_decoded());
    assert!(MmdName::from_field(LEFT_ARM).is_fully_decoded());
}

/// Every standard bone and morph name, as raw bytes, with the rig target expected for bones.
pub(super) const BONE_BYTES: [(&[u8], &str, Option<&str>); 30] = [
    (&[0x91, 0x53, 0x82, 0xC4, 0x82, 0xCC, 0x90, 0x65], "全ての親", None),
    (&[0x83, 0x5A, 0x83, 0x93, 0x83, 0x5E, 0x81, 0x5B], "センター", Some("Pelvis:T")),
    (&[0x83, 0x4F, 0x83, 0x8B, 0x81, 0x5B, 0x83, 0x75], "グルーブ", None),
    (&[0x8D, 0x98], "腰", None),
    (&[0x89, 0xBA, 0x94, 0xBC, 0x90, 0x67], "下半身", Some("Pelvis")),
    (&[0x8F, 0xE3, 0x94, 0xBC, 0x90, 0x67], "上半身", Some("Spine")),
    (&[0x8F, 0xE3, 0x94, 0xBC, 0x90, 0x67, 0x32], "上半身2", Some("Chest")),
    (&[0x8E, 0xF1], "首", Some("Neck")),
    (&[0x93, 0xAA], "頭", Some("Head")),
    (&[0x8D, 0xB6, 0x8C, 0xA8], "左肩", Some("Clavicle.R")),
    (&[0x89, 0x45, 0x8C, 0xA8], "右肩", Some("Clavicle.L")),
    (&[0x8D, 0xB6, 0x98, 0x72], "左腕", Some("UpperArm.R")),
    (&[0x89, 0x45, 0x98, 0x72], "右腕", Some("UpperArm.L")),
    (&[0x8D, 0xB6, 0x82, 0xD0, 0x82, 0xB6], "左ひじ", Some("Forearm.R")),
    (&[0x89, 0x45, 0x82, 0xD0, 0x82, 0xB6], "右ひじ", Some("Forearm.L")),
    (&[0x8D, 0xB6, 0x8E, 0xE8, 0x8E, 0xF1], "左手首", Some("Hand.R")),
    (&[0x89, 0x45, 0x8E, 0xE8, 0x8E, 0xF1], "右手首", Some("Hand.L")),
    (&[0x8D, 0xB6, 0x91, 0xAB], "左足", Some("Thigh.R")),
    (&[0x89, 0x45, 0x91, 0xAB], "右足", Some("Thigh.L")),
    (&[0x8D, 0xB6, 0x82, 0xD0, 0x82, 0xB4], "左ひざ", Some("Shin.R")),
    (&[0x89, 0x45, 0x82, 0xD0, 0x82, 0xB4], "右ひざ", Some("Shin.L")),
    (&[0x8D, 0xB6, 0x91, 0xAB, 0x8E, 0xF1], "左足首", Some("Foot.R")),
    (&[0x89, 0x45, 0x91, 0xAB, 0x8E, 0xF1], "右足首", Some("Foot.L")),
    (&[0x8D, 0xB6, 0x91, 0xAB, 0x82, 0x68, 0x82, 0x6A], "左足ＩＫ", None),
    (&[0x89, 0x45, 0x91, 0xAB, 0x82, 0x68, 0x82, 0x6A], "右足ＩＫ", None),
    (&[0x8D, 0xB6, 0x82, 0xC2, 0x82, 0xDC, 0x90, 0xE6], "左つま先", None),
    (&[0x89, 0x45, 0x82, 0xC2, 0x82, 0xDC, 0x90, 0xE6], "右つま先", None),
    (&[0x97, 0xBC, 0x96, 0xDA], "両目", None),
    (&[0x8D, 0xB6, 0x96, 0xDA], "左目", None),
    (&[0x89, 0x45, 0x96, 0xDA], "右目", None),
];

const MORPH_BYTES: [(&[u8], &str); 13] = [
    (&[0x82, 0xDC, 0x82, 0xCE, 0x82, 0xBD, 0x82, 0xAB], "まばたき"),
    (&[0x83, 0x45, 0x83, 0x42, 0x83, 0x93, 0x83, 0x4E], "ウィンク"),
    (&[0x83, 0x45, 0x83, 0x42, 0x83, 0x93, 0x83, 0x4E, 0x89, 0x45], "ウィンク右"),
    (&[0x8F, 0xCE, 0x82, 0xA2], "笑い"),
    (&[0x82, 0xA0], "あ"),
    (&[0x82, 0xA2], "い"),
    (&[0x82, 0xA4], "う"),
    (&[0x82, 0xA6], "え"),
    (&[0x82, 0xA8], "お"),
    (&[0x93, 0x7B, 0x82, 0xE8], "怒り"),
    (&[0x8D, 0xA2, 0x82, 0xE9], "困る"),
    (&[0x82, 0xC9, 0x82, 0xB1, 0x82, 0xE8], "にこり"),
    (&[0x90, 0x5E, 0x96, 0xCA, 0x96, 0xDA], "真面目"),
];

#[test]
fn every_standard_name_decodes_and_maps_as_listed() {
    assert_eq!(BONE_BYTES.len(), BONE_TABLE.len());
    for (bytes, text, rig) in BONE_BYTES {
        let name = MmdName::from_field(bytes);
        assert_eq!(name.text, text);
        assert!(name.is_fully_decoded(), "{text}");
        let entry = lookup_bone(&name).unwrap_or_else(|| panic!("{text} is not in the table"));
        let got = entry.target.map(|target| match target.channel {
            Channel::Rotation => target.bone.to_string(),
            Channel::Translation => format!("{}:T", target.bone),
        });
        assert_eq!(got.as_deref(), rig, "{text}");
    }
    assert_eq!(MORPH_BYTES.len(), MORPH_VOCABULARY.len());
    for (bytes, text) in MORPH_BYTES {
        let name = MmdName::from_field(bytes);
        assert_eq!(name.text, text);
        assert!(MORPH_VOCABULARY.contains(&name.text.as_str()), "{text}");
    }
}

#[test]
fn mapping_pins_sides_and_covers_the_skeleton() {
    // skeleton.rs puts every .L bone at negative x and every .R bone at positive x; MMD 左 sits at positive x.
    for entry in BONE_TABLE {
        if let Some(target) = entry.target {
            if entry.mmd.starts_with('左') {
                assert!(target.bone.ends_with(".R"), "{}", entry.mmd);
            }
            if entry.mmd.starts_with('右') {
                assert!(target.bone.ends_with(".L"), "{}", entry.mmd);
            }
            assert!(crate::skeleton::STANDARD_BONE_NAMES.contains(&target.bone), "{}", target.bone);
        }
    }
    for rig in crate::skeleton::STANDARD_BONE_NAMES {
        assert!(BONE_TABLE.iter().any(|entry| entry.target.is_some_and(|target| target.bone == *rig)), "{rig}");
    }
    // One source per rig bone channel.
    for (index, entry) in BONE_TABLE.iter().enumerate() {
        if let Some(target) = entry.target {
            let clash = BONE_TABLE.iter().skip(index + 1).any(|other| other.target == Some(target));
            assert!(!clash, "{} shares a rig channel", entry.mmd);
        }
    }
}

#[test]
fn only_the_lower_body_is_marked_as_a_sibling_of_the_upper_body() {
    let marked: Vec<&str> =
        BONE_TABLE.iter().filter(|entry| entry.sibling_of_upper_body_in_mmd).map(|entry| entry.mmd).collect();
    assert_eq!(marked, vec!["下半身"]);
}

// ---------------------------------------------------------------------------------------------------------------
// VMD

#[test]
fn parses_bones_morphs_and_groups_tracks() {
    let mut file = header(false, b"Model");
    file.extend(section(&[
        bone(CENTER, 30, [0.5, -1.25, 2.0], [0.0, 0.0, 0.0, 2.0], linear_interp()),
        bone(LEFT_ARM, 7, [0.0; 3], IDENTITY, linear_interp()),
        bone(CENTER, 0, [1.0, 2.0, 3.0], IDENTITY, linear_interp()),
        bone(CENTER, 30, [9.0, 9.0, 9.0], IDENTITY, linear_interp()),
    ]));
    file.extend(section(&[morph(BLINK, 12, 0.75)]));
    let vmd = parse_vmd(&file).unwrap();
    assert_eq!(vmd.model_name.text, "Model");
    assert!(!vmd.old_header);
    assert_eq!(vmd.bone_keys.len(), 4);
    let first = &vmd.bone_keys[0];
    assert_eq!((first.name.text.as_str(), first.frame), ("センター", 30));
    assert_eq!(first.position, [0.5, -1.25, 2.0]);
    // (0, 0, 0, 2) is normalised.
    assert_eq!(first.rotation, [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(vmd.morph_keys[0].name.text, "まばたき");
    assert_eq!((vmd.morph_keys[0].frame, vmd.morph_keys[0].weight), (12, 0.75));
    assert_eq!(vmd.frame_range(), Some((0, 30)));
    assert_eq!(vmd.skipped, SkippedSections::default());

    let tracks = vmd.bone_tracks();
    assert_eq!(tracks.len(), 2);
    let center = tracks.iter().find(|track| track.name.raw == CENTER).unwrap();
    // Sorted by frame; for the duplicated frame 30 the later key in the file wins.
    assert_eq!(center.keys.iter().map(|key| key.frame).collect::<Vec<_>>(), vec![0, 30]);
    assert_eq!(center.keys[1].position, [9.0, 9.0, 9.0]);
}

#[test]
fn parses_the_old_header_with_a_short_model_name() {
    let mut file = header(true, b"Old");
    file.extend(section(&[simple_bone(3)]));
    let vmd = parse_vmd(&file).unwrap();
    assert!(vmd.old_header);
    assert_eq!(vmd.model_name.text, "Old");
    assert_eq!(vmd.bone_keys[0].frame, 3);
    // Bones only: no morph section follows.
    assert!(vmd.morph_keys.is_empty());
}

#[test]
fn skips_camera_light_shadow_and_ik_sections() {
    let mut file = header(false, b"");
    file.extend(section(&[simple_bone(0)]));
    file.extend(section(&[]));
    file.extend(section(&[vec![0u8; 61], vec![1u8; 61]]));
    file.extend(section(&[vec![0u8; 28]]));
    file.extend(section(&[vec![0u8; 9], vec![0u8; 9], vec![0u8; 9]]));
    // One IK key with two entries: frame, show, count, then 2 x 21 bytes.
    let mut ik = vec![0u8; 5];
    ik.extend(2u32.to_le_bytes());
    ik.extend(vec![0u8; 42]);
    file.extend(section(&[ik]));
    file.extend([7u8, 7]);
    let vmd = parse_vmd(&file).unwrap();
    assert_eq!(
        vmd.skipped,
        SkippedSections { camera: 2, light: 1, shadow: 3, ik: 1, trailing_bytes: 2 }
    );
}

#[test]
fn refuses_truncated_and_oversized_files() {
    let mut file = header(false, b"M");
    file.extend(section(&[simple_bone(0), simple_bone(1)]));
    file.extend(section(&[morph(BLINK, 0, 1.0)]));
    parse_vmd(&file).unwrap();

    // Cut inside the model name, the bone count, a bone key and the morph count.
    for (cut, message) in [
        (10, "VMD is truncated in the header"),
        (40, "VMD is truncated in the model name"),
        (52, "VMD is truncated in bone keys"),
    ] {
        assert_eq!(parse_vmd(&file[..cut]).unwrap_err(), message, "cut {cut}");
    }
    // 50 + 4 + 111 + 50 bytes: the second key is cut short, which the count check catches before any key is read.
    assert_eq!(
        parse_vmd(&file[..50 + 4 + 111 + 50]).unwrap_err(),
        "VMD declares 2 bone keys of 111 bytes but only 161 bytes remain"
    );
    // The morph count itself is cut.
    assert_eq!(parse_vmd(&file[..50 + 4 + 222 + 2]).unwrap_err(), "VMD is truncated in morph keys");

    let mut huge = header(false, b"M");
    huge.extend(u32::MAX.to_le_bytes());
    assert_eq!(
        parse_vmd(&huge).unwrap_err(),
        "VMD declares 4294967295 bone keys of 111 bytes but only 0 bytes remain"
    );
    let mut morph_huge = header(false, b"M");
    morph_huge.extend(section(&[]));
    morph_huge.extend(1_000_000u32.to_le_bytes());
    assert_eq!(
        parse_vmd(&morph_huge).unwrap_err(),
        "VMD declares 1000000 morph keys of 23 bytes but only 0 bytes remain"
    );
    let mut camera_huge = header(false, b"M");
    camera_huge.extend(section(&[]));
    camera_huge.extend(section(&[]));
    camera_huge.extend(u32::MAX.to_le_bytes());
    assert!(parse_vmd(&camera_huge).unwrap_err().starts_with("VMD declares 4294967295 camera keys of 61 bytes"));
    // IK entry count that does not fit.
    let mut ik_huge = header(false, b"M");
    for _ in 0..5 {
        ik_huge.extend(section(&[]));
    }
    let mut ik = vec![0u8; 5];
    ik.extend(u32::MAX.to_le_bytes());
    ik_huge.extend(section(&[ik]));
    assert_eq!(parse_vmd(&ik_huge).unwrap_err(), "VMD IK key entries do not fit the remaining bytes");

    assert_eq!(parse_vmd(&[]).unwrap_err(), "VMD is truncated in the header");
    assert_eq!(parse_vmd(&vec![0u8; MAX_VMD_BYTES + 1]).unwrap_err(), "VMD is larger than the 67108864 byte limit");
    let mut wrong = header(false, b"M");
    wrong[0] = b'X';
    assert_eq!(parse_vmd(&wrong).unwrap_err(), "not a VMD file: unknown header");
}

#[test]
fn refuses_bad_numbers_and_frames() {
    let build = |key: Vec<u8>| {
        let mut file = header(false, b"M");
        file.extend(section(&[key]));
        file
    };
    // Exactly zero reads as the identity and is counted; a tiny non-zero quaternion is refused.
    let zero = parse_vmd(&build(bone(CENTER, 0, [0.0; 3], [0.0; 4], linear_interp()))).unwrap();
    assert_eq!(zero.bone_keys[0].rotation, [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(zero.zero_quaternions, 1);
    assert_eq!(zero.retarget_report(&BoneTable::default()).zero_quaternions, 1);
    let tiny = build(bone(CENTER, 0, [0.0; 3], [1e-9, 0.0, 0.0, 0.0], linear_interp()));
    assert_eq!(parse_vmd(&tiny).unwrap_err(), "bone key 0 (センター) has a near-zero quaternion");
    let nan_position = build(bone(LEFT_ARM, 0, [0.0, f32::NAN, 0.0], IDENTITY, linear_interp()));
    assert_eq!(parse_vmd(&nan_position).unwrap_err(), "bone key 0 (左腕) has a non-finite value");
    let inf_rotation = build(bone(CENTER, 0, [0.0; 3], [f32::INFINITY, 0.0, 0.0, 1.0], linear_interp()));
    assert_eq!(parse_vmd(&inf_rotation).unwrap_err(), "bone key 0 (センター) has a non-finite value");
    let late = build(bone(CENTER, MAX_FRAME + 1, [0.0; 3], IDENTITY, linear_interp()));
    assert_eq!(parse_vmd(&late).unwrap_err(), "bone key 0 (センター) has frame 1000001 above the limit 1000000");
    assert!(parse_vmd(&build(bone(CENTER, MAX_FRAME, [0.0; 3], IDENTITY, linear_interp()))).is_ok());
    let mut bad_morph = header(false, b"M");
    bad_morph.extend(section(&[]));
    bad_morph.extend(section(&[morph(BLINK, 0, f32::NAN)]));
    assert_eq!(parse_vmd(&bad_morph).unwrap_err(), "morph key 0 (まばたき) has a non-finite weight");
}

#[test]
fn control_characters_in_names_do_not_reach_error_strings() {
    // A name with a newline and an escape byte; both are ASCII, so the decoder passes them through.
    let name = b"a\nb\x1b[31mc\x7fd";
    let mut file = header(false, b"M");
    file.extend(section(&[bone(name, MAX_FRAME + 1, [0.0; 3], IDENTITY, linear_interp())]));
    let message = parse_vmd(&file).unwrap_err();
    assert_eq!(message, "bone key 0 (a?b?[31mc?d) has frame 1000001 above the limit 1000000");
    assert!(message.chars().all(|c| !c.is_control()), "{message:?}");
    let mut bad_morph = header(false, b"M");
    bad_morph.extend(section(&[]));
    bad_morph.extend(section(&[morph(name, 0, f32::NAN)]));
    let message = parse_vmd(&bad_morph).unwrap_err();
    assert!(message.chars().all(|c| !c.is_control()), "{message:?}");
    // The decoded text itself keeps the characters; only the label is sanitised.
    assert_eq!(MmdName::from_field(name).text, "a\nb\x1b[31mc\x7fd");
}

/// A clean 64-byte interpolation block as MMD lays it out: the 16 control bytes `X_x1 Y_x1 Z_x1 R_x1 X_y1 ..`
/// (element `4 * k + c`), written four times, row `r` shifted left by `r` and padded with 01, 00, 00.
fn mmd_interp_block(elements: [u8; 16]) -> [u8; 64] {
    let mut block = [0u8; 64];
    for row in 0..4 {
        for slot in 0..16 {
            block[16 * row + slot] = match slot + row {
                element if element < 16 => elements[element],
                16 => 1,
                _ => 0,
            };
        }
    }
    block
}

#[test]
fn interpolation_read_ignores_the_physics_flag_bytes() {
    // Distinct values per element: element 4k + c = 10 * (k + 1) + c, so every control byte is different.
    let elements: [u8; 16] = std::array::from_fn(|element| (10 * (element / 4 + 1) + element % 4) as u8);
    let clean = mmd_interp_block(elements);
    // Row 3 begins with R_x1 (element 3) and ends with the 01 00 00 padding: the layout is as documented.
    assert_eq!(&clean[48..52], &[13, 20, 21, 22]);
    assert_eq!(&clean[61..64], &[1, 0, 0]);
    let parse = |block: [u8; 64]| {
        let mut file = header(false, b"M");
        file.extend(section(&[bone(CENTER, 0, [0.0; 3], IDENTITY, block)]));
        parse_vmd(&file).unwrap().bone_keys.remove(0)
    };
    let expected: [[f64; 4]; 4] =
        std::array::from_fn(|c| std::array::from_fn(|k| f64::from(elements[4 * k + c]) / 127.0));
    assert_eq!(parse(clean).curves, expected);
    // Newer MMD stores the physics-off flag in bytes 2 and 3; the control points must not change.
    let mut flagged = clean;
    flagged[2] = 0x63;
    flagged[3] = 0x0F;
    let key = parse(flagged);
    assert_eq!(key.curves, expected);
    assert_eq!(key.interpolation, flagged);
}

#[test]
fn interpolation_bytes_follow_the_documented_offsets() {
    let mut interp = linear_interp();
    // Rotation channel (3) is read from its own row: x1 at 48, y1 at 52, x2 at 56, y2 at 60. X channel (0): y2 at 12.
    interp[48] = 127;
    interp[52] = 0;
    interp[56] = 64;
    interp[60] = 127;
    interp[12] = 0;
    let mut file = header(false, b"M");
    file.extend(section(&[bone(CENTER, 0, [0.0; 3], IDENTITY, interp)]));
    let key = &parse_vmd(&file).unwrap().bone_keys[0];
    assert_eq!(key.interpolation, interp);
    assert_eq!(key.curves[CHANNEL_ROTATION], [1.0, 0.0, 64.0 / 127.0, 1.0]);
    assert_eq!(key.curves[0], [20.0 / 127.0, 20.0 / 127.0, 107.0 / 127.0, 0.0]);
    assert_eq!(key.curves[1], [20.0 / 127.0, 20.0 / 127.0, 107.0 / 127.0, 107.0 / 127.0]);
}

#[test]
fn bezier_is_exactly_linear_for_diagonal_control_points() {
    let curve = [20.0 / 127.0, 20.0 / 127.0, 107.0 / 127.0, 107.0 / 127.0];
    for step in 0..=10 {
        let s = f64::from(step) / 10.0;
        assert_eq!(bezier_ease(curve, s), s);
    }
    assert_eq!(bezier_ease([0.5, 0.5, 0.5, 0.5], 0.3), 0.3);
}

#[test]
fn bezier_matches_a_hand_computed_point() {
    // Control points (0, 1) and (1, 1): x(t) = 3t^2 - 2t^3 and y(t) = 3t - 3t^2 + t^3. At x = 0.5 the parameter is
    // t = 0.5 (x(0.5) = 0.75 - 0.25), and y(0.5) = 1.5 - 0.75 + 0.125 = 0.875.
    let curve = [0.0, 1.0, 1.0, 1.0];
    assert!((bezier_ease(curve, 0.5) - 0.875).abs() < 1e-12);
    assert_eq!(bezier_ease(curve, 0.0), 0.0);
    assert!((bezier_ease(curve, 1.0) - 1.0).abs() < 1e-12);
    // Out-of-range input is clamped.
    assert_eq!(bezier_ease(curve, -3.0), 0.0);
}

fn two_key_track(second_interp: [u8; 64], second_rotation: [f32; 4]) -> BoneTrack {
    let mut file = header(false, b"M");
    file.extend(section(&[
        bone(CENTER, 0, [0.0, 10.0, -4.0], IDENTITY, linear_interp()),
        bone(CENTER, 30, [8.0, 20.0, 4.0], second_rotation, second_interp),
    ]));
    parse_vmd(&file).unwrap().bone_tracks().remove(0)
}

#[test]
fn sampling_interpolates_holds_and_uses_30_fps_frames() {
    let track = two_key_track(linear_interp(), IDENTITY);
    let (position, rotation) = sample_bone(&track, 15.0);
    assert_eq!(position, [4.0, 15.0, 0.0]);
    assert_eq!(rotation, [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(sample_bone(&track, 7.5).0, [2.0, 12.5, -2.0]);
    // First and last key are held outside the range, and a non-finite frame holds the first.
    assert_eq!(sample_bone(&track, -5.0).0, [0.0, 10.0, -4.0]);
    assert_eq!(sample_bone(&track, 1000.0).0, [8.0, 20.0, 4.0]);
    assert_eq!(sample_bone(&track, f64::NAN).0, [0.0, 10.0, -4.0]);
    assert_eq!(frame_at_seconds(1.5), 45.0);
    assert_eq!(FRAMES_PER_SECOND, 30.0);
}

#[test]
fn sampling_applies_the_curve_of_the_later_key_per_channel() {
    // X channel curve (0, 127, 127, 127): at half way the eased fraction is 0.875 (see the Bezier test). Y stays linear.
    let mut interp = linear_interp();
    interp[0] = 0;
    interp[4] = 127;
    interp[8] = 127;
    interp[12] = 127;
    let track = two_key_track(interp, IDENTITY);
    let (position, _) = sample_bone(&track, 15.0);
    assert!((position[0] - 7.0).abs() < 1e-9, "{position:?}");
    assert_eq!(position[1], 15.0);
}

#[test]
fn rotation_midpoint_of_a_quarter_turn_is_an_eighth_turn() {
    let half = std::f64::consts::FRAC_1_SQRT_2;
    let quarter_turn_about_y = [0.0, half, 0.0, half];
    let mid = slerp([0.0, 0.0, 0.0, 1.0], quarter_turn_about_y, 0.5);
    // 45 degrees about y: (0, sin 22.5, 0, cos 22.5).
    let expected = [0.0, 0.3826834323650898, 0.0, 0.9238795325112867];
    for axis in 0..4 {
        assert!((mid[axis] - expected[axis]).abs() < 1e-12, "{mid:?}");
    }
    // The short arc is taken when the second quaternion has the opposite sign.
    let flipped = slerp([0.0, 0.0, 0.0, 1.0], quarter_turn_about_y.map(|value| -value), 0.5);
    for axis in 0..4 {
        assert!((flipped[axis] - expected[axis]).abs() < 1e-12, "{flipped:?}");
    }
    // Through sampling (key quaternions are stored as f32).
    let track = two_key_track(linear_interp(), [0.0, 0.70710677, 0.0, 0.70710677]);
    let (_, rotation) = sample_bone(&track, 15.0);
    for axis in 0..4 {
        assert!((rotation[axis] - expected[axis]).abs() < 1e-6, "{rotation:?}");
    }
}

// ---------------------------------------------------------------------------------------------------------------
// VPD

fn vpd_text(body: &str) -> Vec<u8> {
    let mut out = b"Vocaloid Pose Data file\r\n\r\n".to_vec();
    out.extend(body.as_bytes());
    out
}

#[test]
fn parses_a_pose_with_comments_and_crlf() {
    let mut file = vpd_text("model.osm;\t\t// parent\r\n2;\t// count\r\n\r\nBone0{");
    file.extend(LEFT_ARM);
    file.extend(b"\r\n  1.5,-2,0.25;  // trans\r\n  0,0,0,2;\r\n}\r\n\r\nBone1{Head\r\n0,0,0;\r\n0,1,0,0;\r\n}\r\n");
    let vpd = parse_vpd(&file).unwrap();
    assert_eq!(vpd.model_file.text, "model.osm");
    assert_eq!(vpd.bones.len(), 2);
    assert_eq!(vpd.bones[0].name.text, "左腕");
    assert_eq!(vpd.bones[0].position, [1.5, -2.0, 0.25]);
    assert_eq!(vpd.bones[0].rotation, [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(vpd.bones[1].name.text, "Head");
    assert_eq!(vpd.bones[1].rotation, [0.0, 1.0, 0.0, 0.0]);
    let report = vpd.retarget_report(&BoneTable::default());
    assert_eq!(report.mapped_bones.len(), 1);
    assert_eq!(report.mapped_bones[0].rig, "UpperArm.R");
    assert_eq!(report.frame_range, None);
    assert_eq!(report.unmapped_bones[0].text, "Head");
    assert!(!report.unmapped_bones[0].standard);
}

#[test]
fn vpd_refusals_have_fixed_messages() {
    let block = "Bone0{A\n0,0,0;\n0,0,0,1;\n}\n";
    assert!(parse_vpd(&vpd_text(&format!("m;\n1;\n{block}"))).is_ok());
    for (body, message) in [
        (format!("m;\n2;\n{block}"), "VPD declares 2 bones but only 4 lines remain"),
        ("m;\n4294967295;\n".to_string(), "VPD declares 4294967295 bones, above the limit 4096"),
        ("m;\n4097;\n".to_string(), "VPD declares 4097 bones, above the limit 4096"),
        (format!("m\n1;\n{block}"), "VPD model file line must end with ';'"),
        (format!("m;\nx;\n{block}"), "VPD bone count is not a number"),
        (format!("m;\n1;\nBone0{{A\n0,0;\n0,0,0,1;\n}}\n"), "VPD bone 0: position needs 3 numbers"),
        (format!("m;\n1;\nBone0{{A\n0,0,0;\n1e-9,0,0,0;\n}}\n"), "VPD bone 0: near-zero quaternion"),
        (format!("m;\n1;\nBone0{{A\n0,NaN,0;\n0,0,0,1;\n}}\n"), "VPD bone 0: position is not finite"),
        (format!("m;\n1;\nBone0{{A\n0,0,0;\n0,0,0,1,1;\n}}\n"), "VPD bone 0: rotation has more than 4 numbers"),
        (format!("m;\n1;\nBone0{{A\n0,0,0;\n0,0,0,1;\nx\n"), "VPD bone 0: expected '}'"),
        (format!("m;\n1;\nBone{{A\n0,0,0;\n0,0,0,1;\n}}\n"), "VPD bone 0: expected 'Bone<n>{<name>'"),
        ("m;\n".to_string(), "VPD is truncated before the bone count"),
    ] {
        assert_eq!(parse_vpd(&vpd_text(&body)).unwrap_err(), message, "{body:?}");
    }
    assert_eq!(parse_vpd(b"Vocaloid Pose Data\n").unwrap_err(), "not a VPD file: unknown header");
    assert_eq!(
        parse_vpd(&vec![b' '; MAX_VPD_BYTES + 1]).unwrap_err(),
        "VPD is larger than the 1048576 byte limit"
    );
}

#[test]
fn vpd_caps_names_and_does_not_collect_trailing_lines() {
    let long = "N".repeat(MAX_VPD_NAME_BYTES + 1);
    let ok = "N".repeat(MAX_VPD_NAME_BYTES);
    let block = |name: &str| format!("Bone0{{{name}\n0,0,0;\n0,0,0,1;\n}}\n");
    assert!(parse_vpd(&vpd_text(&format!("m;\n1;\n{}", block(&ok)))).is_ok());
    assert_eq!(
        parse_vpd(&vpd_text(&format!("m;\n1;\n{}", block(&long)))).unwrap_err(),
        "VPD bone 0: name is longer than 128 bytes"
    );
    assert_eq!(
        parse_vpd(&vpd_text(&format!("{long};\n0;\n"))).unwrap_err(),
        "VPD model file name is longer than 128 bytes"
    );
    // The declared maximum is accepted; a megabyte of trailing short lines is ignored without being collected.
    let mut body = format!("m;\n{MAX_VPD_BONES};\n");
    for _ in 0..MAX_VPD_BONES {
        body.push_str(&block("A"));
    }
    body.push_str(&"x\n".repeat(400_000));
    let file = vpd_text(&body);
    assert!(file.len() <= MAX_VPD_BYTES);
    assert_eq!(parse_vpd(&file).unwrap().bones.len(), MAX_VPD_BONES);
}

#[test]
fn vpd_zero_quaternion_reads_as_identity_and_names_stay_out_of_errors() {
    let vpd = parse_vpd(&vpd_text("m;\n1;\nBone0{A\n0,0,0;\n0,0,0,0;\n}\n")).unwrap();
    assert_eq!(vpd.bones[0].rotation, [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(vpd.zero_quaternions, 1);
    assert_eq!(vpd.retarget_report(&BoneTable::default()).zero_quaternions, 1);
    // Errors carry the bone index only, never the (possibly control-character) name.
    let message = parse_vpd(&vpd_text("m;\n1;\nBone0{a\x1b[31m\x7f\n0,NaN,0;\n0,0,0,1;\n}\n")).unwrap_err();
    assert_eq!(message, "VPD bone 0: position is not finite");
}

// ---------------------------------------------------------------------------------------------------------------
// Coordinates

fn assert_close(actual: [f64; 3], expected: [f64; 3], tolerance: f64) {
    for axis in 0..3 {
        assert!((actual[axis] - expected[axis]).abs() < tolerance, "{actual:?} vs {expected:?}");
    }
}

#[test]
fn thirty_degrees_about_each_mmd_axis_have_hand_computed_angles() {
    // Frame change MMD -> rig is P = diag(1, -1, -1): (x, y, z, w) -> rig (x, -y, -z, w). The rig matrix is
    // Rz(roll) Ry(-yaw) Rx(-pitch), so rig pitch = -(angle about rig x), yaw = -(angle about rig y), roll = angle
    // about rig z. sin 15 degrees = 0.25881904510252074, cos 15 degrees = 0.9659258262890683.
    let (s, c) = (0.25881904510252074, 0.9659258262890683);
    let thirty = 0.5235987755982988; // pi / 6
    // About MMD +x: rig +30 degrees about x, so pitch = -30 degrees.
    assert_close(mmd_quaternion_to_rig_world_pitch_yaw_roll([s, 0.0, 0.0, c]), [-thirty, 0.0, 0.0], 1e-12);
    // About MMD +y: rig -30 degrees about y, so yaw = +30 degrees.
    assert_close(mmd_quaternion_to_rig_world_pitch_yaw_roll([0.0, s, 0.0, c]), [0.0, thirty, 0.0], 1e-12);
    // About MMD +z: rig -30 degrees about z, so roll = -30 degrees.
    assert_close(mmd_quaternion_to_rig_world_pitch_yaw_roll([0.0, 0.0, s, c]), [0.0, 0.0, -thirty], 1e-12);
    // The input is normalised, and the identity and the zero quaternion are handled.
    assert_close(mmd_quaternion_to_rig_world_pitch_yaw_roll([2.0 * s, 0.0, 0.0, 2.0 * c]), [-thirty, 0.0, 0.0], 1e-12);
    assert_eq!(mmd_quaternion_to_rig_world_pitch_yaw_roll([0.0, 0.0, 0.0, 1.0]), [0.0; 3]);
    assert_eq!(mmd_quaternion_to_rig_world_pitch_yaw_roll([0.0; 4]), [0.0; 3]);
    assert_eq!(mmd_quaternion_to_rig_world_pitch_yaw_roll([f64::NAN, 0.0, 0.0, 1.0]), [0.0; 3]);
    assert_eq!(mmd_position_to_rig([1.0, 2.0, 3.0]), [1.0, -2.0, -3.0]);
}

const THIRTY: f64 = 0.5235987755982988; // pi / 6

fn about(axis: usize) -> [f64; 4] {
    // 30 degrees about one MMD axis as (x, y, z, w).
    let mut q = [0.0, 0.0, 0.0, 0.9659258262890683];
    q[axis] = 0.25881904510252074;
    q
}

#[test]
fn bone_pose_for_an_upright_bone_flips_pitch_and_yaw() {
    // Pelvis, Spine, Chest, Neck and Head point up the screen (head at the smaller y): tail - head = -y, which
    // quat_from_unit turns into a 180 degree turn about z, (w, x, y, z) = (0, 0, 0, 1).
    let rest = rest_quat_wxyz([0.0; 3], [0.0, -1.0, 0.0], 0.0);
    assert_eq!(rest, [0.0, 0.0, 0.0, 1.0]);
    // Rz(180)^-1 * D * Rz(180) turns an x rotation into the opposite x rotation and a y rotation into the opposite
    // y rotation; z is unchanged. So pitch = +30 (the world-axes answer is -30), yaw = -30 (world axes +30).
    assert_close(mmd_quaternion_to_bone_pose(rest, about(0)), [THIRTY, 0.0, 0.0], 1e-12);
    assert_close(mmd_quaternion_to_bone_pose(rest, about(1)), [0.0, -THIRTY, 0.0], 1e-12);
    assert_close(mmd_quaternion_to_bone_pose(rest, about(2)), [0.0, 0.0, -THIRTY], 1e-12);
}

#[test]
fn bone_pose_for_a_sideways_bone_swaps_pitch_and_yaw() {
    // A clavicle pointing to +x: the rest is -90 degrees about z; pointing to -x it is +90 degrees about z.
    let toward_plus_x = rest_quat_wxyz([0.0; 3], [1.0, 0.0, 0.0], 0.0);
    let h = std::f64::consts::FRAC_1_SQRT_2;
    assert_close(toward_plus_x[..3].try_into().unwrap(), [h, 0.0, 0.0], 1e-12);
    assert!((toward_plus_x[3] + h).abs() < 1e-12);
    assert_close(mmd_quaternion_to_bone_pose(toward_plus_x, about(0)), [0.0, -THIRTY, 0.0], 1e-12);
    assert_close(mmd_quaternion_to_bone_pose(toward_plus_x, about(1)), [-THIRTY, 0.0, 0.0], 1e-12);
    assert_close(mmd_quaternion_to_bone_pose(toward_plus_x, about(2)), [0.0, 0.0, -THIRTY], 1e-12);
    let toward_minus_x = rest_quat_wxyz([0.0; 3], [-1.0, 0.0, 0.0], 0.0);
    assert_close(mmd_quaternion_to_bone_pose(toward_minus_x, about(0)), [0.0, THIRTY, 0.0], 1e-12);
    assert_close(mmd_quaternion_to_bone_pose(toward_minus_x, about(1)), [THIRTY, 0.0, 0.0], 1e-12);
    assert_close(mmd_quaternion_to_bone_pose(toward_minus_x, about(2)), [0.0, 0.0, -THIRTY], 1e-12);
}

#[test]
fn bone_pose_with_the_identity_rest_is_the_world_axes_answer() {
    let identity_rest = rest_quat_wxyz([0.0; 3], [0.0, 1.0, 0.0], 0.0);
    assert_eq!(identity_rest, [1.0, 0.0, 0.0, 0.0]);
    for axis in 0..3 {
        assert_close(
            mmd_quaternion_to_bone_pose(identity_rest, about(axis)),
            mmd_quaternion_to_rig_world_pitch_yaw_roll(about(axis)),
            1e-12,
        );
    }
    // Degenerate input gives zeros.
    assert_eq!(mmd_quaternion_to_bone_pose([0.0; 4], about(0)), [0.0; 3]);
    assert_eq!(mmd_quaternion_to_bone_pose(identity_rest, [0.0; 4]), [0.0; 3]);
}

#[test]
fn bone_pose_rebuilds_the_mmd_world_rotation_through_the_rigs_forward_path() {
    // The rig poses a bone with unposed ancestors as world = rest * pose_quat. Its world rotation relative to the
    // rest must be P q P = (w, x, -y, -z) for the MMD rotation q, whatever the rest orientation.
    let rests = [
        rest_quat_wxyz([0.0; 3], [0.0, -1.0, 0.0], 0.0),
        rest_quat_wxyz([0.0; 3], [0.0, 1.0, 0.0], 0.0),
        rest_quat_wxyz([0.0; 3], [1.0, 0.0, 0.0], 0.0),
        rest_quat_wxyz([0.0; 3], [-1.0, 0.0, 0.0], 0.0),
        rest_quat_wxyz([0.1, 0.2, 0.0], [0.4, -0.5, 0.3], 0.7),
        rest_quat_wxyz([0.0; 3], [0.0, 0.0, 1.0], -1.1),
    ];
    let mut state = 777;
    for rest in rests {
        for _ in 0..500 {
            let raw: [f64; 4] = std::array::from_fn(|_| lcg(&mut state) * 2.0 - 1.0);
            let length = raw.iter().map(|value| value * value).sum::<f64>().sqrt();
            let q = raw.map(|value| value / length);
            let [pitch, yaw, roll] = mmd_quaternion_to_bone_pose(rest, q);
            let world = world_quat_wxyz(rest, pitch, yaw, roll);
            let rest_inverse = [rest[0], -rest[1], -rest[2], -rest[3]];
            let delta = quat_mul_wxyz(world, rest_inverse);
            let expected = [q[3], q[0], -q[1], -q[2]];
            let dot: f64 = delta.iter().zip(&expected).map(|(a, b)| a * b).sum();
            // Same rotation up to quaternion sign: the two unit quaternions agree to 1e-9 per component.
            for axis in 0..4 {
                assert!((delta[axis] - dot.signum() * expected[axis]).abs() < 1e-9, "{delta:?} vs {expected:?}");
            }
        }
    }
}

/// Deterministic pseudo-random numbers in 0..1 (no `rand` dependency).
fn lcg(state: &mut u64) -> f64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    ((*state >> 11) as f64) / ((1u64 << 53) as f64)
}

#[test]
fn angles_round_trip_through_the_rig_rotation() {
    let mut state = 12345;
    for _ in 0..2000 {
        // Yaw is the middle angle of the rig's Euler order, so it is the one that must stay inside (-90, 90) degrees.
        let pitch = (lcg(&mut state) * 2.0 - 1.0) * 3.1;
        let yaw = (lcg(&mut state) * 2.0 - 1.0) * 1.5707;
        let roll = (lcg(&mut state) * 2.0 - 1.0) * 3.1;
        let mmd = rig_world_pitch_yaw_roll_to_mmd_quaternion(pitch, yaw, roll);
        assert_close(mmd_quaternion_to_rig_world_pitch_yaw_roll(mmd), [pitch, yaw, roll], 1e-9);
        // The negated quaternion is the same rotation.
        assert_close(mmd_quaternion_to_rig_world_pitch_yaw_roll(mmd.map(|value| -value)), [pitch, yaw, roll], 1e-9);
    }
}

#[test]
fn gimbal_lock_at_plus_or_minus_90_degrees_yaw_is_deterministic() {
    for yaw in [FRAC_PI_2, -FRAC_PI_2] {
        let mmd = rig_world_pitch_yaw_roll_to_mmd_quaternion(0.3, yaw, -0.5);
        let first = mmd_quaternion_to_rig_world_pitch_yaw_roll(mmd);
        assert_eq!(first, mmd_quaternion_to_rig_world_pitch_yaw_roll(mmd));
        // The rig's rule: roll is 0 and the coupled angle is carried by pitch.
        assert_eq!(first[2], 0.0);
        assert!((first[1] - yaw).abs() < 1e-6, "{first:?}");
        // The angles rebuild the same rotation (compare quaternions up to sign).
        let again = depth_edit_rotation_wxyz(first[0], first[1], first[2]);
        let original = depth_edit_rotation_wxyz(0.3, yaw, -0.5);
        let dot: f64 = again.iter().zip(&original).map(|(a, b)| a * b).sum();
        assert!((dot.abs() - 1.0).abs() < 1e-9, "dot {dot}");
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Report

#[test]
fn report_lists_mapped_unmapped_and_frame_range() {
    let unknown: &[u8] = &[0x85, 0x40, b'X']; // a pair that CP932 does not map, then X
    let ik: &[u8] = &[0x8D, 0xB6, 0x91, 0xAB, 0x82, 0x68, 0x82, 0x6A]; // 左足ＩＫ
    let mut file = header(false, b"M");
    file.extend(section(&[
        bone(CENTER, 10, [0.0; 3], IDENTITY, linear_interp()),
        bone(CENTER, 40, [0.0; 3], IDENTITY, linear_interp()),
        bone(LEFT_ARM, 20, [0.0; 3], IDENTITY, linear_interp()),
        bone(ik, 5, [0.0; 3], IDENTITY, linear_interp()),
        bone(unknown, 15, [0.0; 3], IDENTITY, linear_interp()),
    ]));
    file.extend(section(&[morph(BLINK, 2, 1.0), morph(b"foo", 90, 0.5)]));
    let report = parse_vmd(&file).unwrap().retarget_report(&BoneTable::default());
    assert_eq!(
        report.mapped_bones,
        vec![
            MappedBone { mmd: "センター".into(), rig: "Pelvis", channel: Channel::Translation },
            MappedBone { mmd: "左腕".into(), rig: "UpperArm.R", channel: Channel::Rotation },
        ]
    );
    assert_eq!(
        report.unmapped_bones,
        vec![
            UnmappedName { text: "左足ＩＫ".into(), hex: "8db691ab8268826a".into(), standard: true },
            UnmappedName { text: "\u{FFFD}X".into(), hex: "854058".into(), standard: false },
        ]
    );
    assert_eq!(
        report.unmapped_morphs,
        vec![
            UnmappedName { text: "まばたき".into(), hex: "82dc82ce82bd82ab".into(), standard: true },
            UnmappedName { text: "foo".into(), hex: "666f6f".into(), standard: false },
        ]
    );
    assert_eq!(report.frame_range, Some((2, 90)));
}
