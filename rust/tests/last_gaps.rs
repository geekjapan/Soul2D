//! Tests for the last behaviours ported from the D editor: JPEG import, command manifest, live-screenshot
//! extras, window shell logic, autosave records and untitled recovery, and the settings-driven MCP server.

mod common;

use serde_json::{json, Value};
use soul2d_core::jpeg;
use soul2d_core::Session;

fn ok(session: &mut Session, request: Value) -> Value {
    let response = session.handle_request(&request);
    assert_eq!(response["succeeded"], true, "{request}: {response}");
    response["result"].clone()
}

fn fail(session: &mut Session, request: Value) -> String {
    let response = session.handle_request(&request);
    assert_eq!(response["succeeded"], false, "{request}: {response}");
    response["message"].as_str().unwrap().to_owned()
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("soul2d-last-gaps-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    common::scratch(dir)
}

// ------------------------------------------------------------------------------------------ JPEG

const SIDE: u32 = 8192;
const PIXELS: u64 = 64 * 1024 * 1024;

struct BitSink {
    bytes: Vec<u8>,
    acc: u32,
    count: u32,
}

impl BitSink {
    fn put(&mut self, value: u32, bits: u32) {
        for shift in (0..bits).rev() {
            self.acc = self.acc << 1 | (value >> shift & 1);
            self.count += 1;
            if self.count == 8 {
                self.flush_byte();
            }
        }
    }
    fn flush_byte(&mut self) {
        self.bytes.push(self.acc as u8);
        if self.acc as u8 == 0xff {
            self.bytes.push(0);
        }
        self.acc = 0;
        self.count = 0;
    }
    fn align(&mut self) {
        while self.count != 0 {
            self.put(1, 1);
        }
    }
}

fn segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) {
    out.extend_from_slice(&[0xff, marker]);
    out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(payload);
}

/// A JPEG whose blocks are flat (DC only), so the expected samples are exact: `sampling` lists `(h, v)` per
/// component and `level(component, block_x, block_y)` the 8-bit sample of one block. Quantisation is all ones;
/// the DC table has 12 four-bit codes and the AC table only the end-of-block symbol.
fn flat_jpeg(width: u16, height: u16, sampling: &[(u8, u8)], restart: u16, level: &dyn Fn(usize, usize, usize) -> u8) -> Vec<u8> {
    let mut out = vec![0xff, 0xd8];
    let mut dqt = vec![0u8];
    dqt.extend([1u8; 64]);
    segment(&mut out, 0xdb, &dqt);
    let mut frame = vec![8];
    frame.extend(height.to_be_bytes());
    frame.extend(width.to_be_bytes());
    frame.push(sampling.len() as u8);
    for (i, (h, v)) in sampling.iter().enumerate() {
        frame.extend([i as u8 + 1, h << 4 | v, 0]);
    }
    segment(&mut out, 0xc0, &frame);
    let mut dc = vec![0x00, 0, 0, 0, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    dc.extend(0..12u8);
    segment(&mut out, 0xc4, &dc);
    let mut ac = vec![0x10, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    ac.push(0);
    segment(&mut out, 0xc4, &ac);
    if restart > 0 {
        segment(&mut out, 0xdd, &restart.to_be_bytes());
    }
    let mut scan = vec![sampling.len() as u8];
    for i in 0..sampling.len() {
        scan.extend([i as u8 + 1, 0x00]);
    }
    scan.extend([0, 63, 0]);
    segment(&mut out, 0xda, &scan);
    let hmax = sampling.iter().map(|s| s.0).max().unwrap() as usize;
    let vmax = sampling.iter().map(|s| s.1).max().unwrap() as usize;
    let mcus_x = (width as usize).div_ceil(8 * hmax);
    let mcus_y = (height as usize).div_ceil(8 * vmax);
    let mut sink = BitSink { bytes: Vec::new(), acc: 0, count: 0 };
    let mut previous = vec![0i32; sampling.len()];
    let (mut done, mut marker) = (0usize, 0u8);
    for my in 0..mcus_y {
        for mx in 0..mcus_x {
            if restart > 0 && done > 0 && done % restart as usize == 0 {
                sink.align();
                sink.bytes.extend_from_slice(&[0xff, 0xd0 + marker % 8]);
                marker += 1;
                previous.iter_mut().for_each(|p| *p = 0);
            }
            done += 1;
            for (c, (h, v)) in sampling.iter().enumerate() {
                for by in 0..*v as usize {
                    for bx in 0..*h as usize {
                        let target = (i32::from(level(c, mx * *h as usize + bx, my * *v as usize + by)) - 128) * 8;
                        let diff = target - previous[c];
                        previous[c] = target;
                        let size = 32 - diff.unsigned_abs().leading_zeros();
                        sink.put(size, 4);
                        if size > 0 {
                            let bits = if diff < 0 { (diff - 1) as u32 & ((1 << size) - 1) } else { diff as u32 };
                            sink.put(bits, size);
                        }
                        sink.put(0, 1); // end of block
                    }
                }
            }
        }
    }
    sink.align();
    out.extend(sink.bytes);
    out.extend_from_slice(&[0xff, 0xd9]);
    out
}

fn rgb_of(luma: u8, cb: u8, cr: u8) -> [u8; 3] {
    let (y, b, r) = (f32::from(luma), f32::from(cb) - 128.0, f32::from(cr) - 128.0);
    let c = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    [c(y + 1.402 * r), c(y - 0.344_136 * b - 0.714_136 * r), c(y + 1.772 * b)]
}

#[test]
fn grayscale_blocks_decode_to_their_exact_levels() {
    let levels = [[10u8, 200], [128, 255]];
    let bytes = flat_jpeg(16, 16, &[(1, 1)], 0, &|_, bx, by| levels[by][bx]);
    let image = jpeg::decode(&bytes, SIDE, PIXELS).unwrap();
    assert_eq!(image.dimensions(), (16, 16));
    for (x, y, expected) in [(0, 0, 10u8), (7, 7, 10), (8, 0, 200), (15, 7, 200), (0, 8, 128), (15, 15, 255)] {
        assert_eq!(image.get_pixel(x, y).0, [expected, expected, expected, 255], "pixel {x},{y}");
    }
}

#[test]
fn subsampled_chroma_covers_the_whole_mcu_and_edges_are_cropped() {
    // 4:2:0, 24x10: two MCUs wide (16 px each), the chroma block of each MCU colours its 16x16 area.
    let chroma = [(90u8, 200u8), (180, 60)];
    let bytes = flat_jpeg(24, 10, &[(2, 2), (1, 1), (1, 1)], 0, &|c, bx, _| match c {
        0 => 100 + 20 * bx as u8,
        1 => chroma[bx].0,
        _ => chroma[bx].1,
    });
    let image = jpeg::decode(&bytes, SIDE, PIXELS).unwrap();
    assert_eq!(image.dimensions(), (24, 10));
    let pixel = |x, y| image.get_pixel(x, y).0;
    assert_eq!(pixel(0, 0)[..3], rgb_of(100, 90, 200));
    assert_eq!(pixel(7, 9)[..3], rgb_of(100, 90, 200));
    assert_eq!(pixel(8, 0)[..3], rgb_of(120, 90, 200), "second luma block, same chroma block");
    assert_eq!(pixel(16, 0)[..3], rgb_of(140, 180, 60));
    assert_eq!(pixel(23, 9)[..3], rgb_of(140, 180, 60), "the cropped edge keeps the MCU's blocks");
}

#[test]
fn restart_intervals_reset_the_dc_predictor() {
    // 4 MCUs per row, a restart marker every 2 MCUs; levels differ so a missed reset shifts the colours.
    let levels = [20u8, 220, 90, 250];
    let bytes = flat_jpeg(32, 8, &[(1, 1)], 2, &|_, bx, _| levels[bx]);
    assert!(bytes.windows(2).any(|w| w == [0xff, 0xd0]));
    let image = jpeg::decode(&bytes, SIDE, PIXELS).unwrap();
    for (block, level) in levels.iter().enumerate() {
        assert_eq!(image.get_pixel(block as u32 * 8 + 3, 4).0, [*level, *level, *level, 255]);
    }
    // Without the marker where the interval says one belongs, the decoder refuses the stream.
    let mut broken = bytes.clone();
    let at = broken.windows(2).position(|w| w == [0xff, 0xd0]).unwrap();
    broken[at + 1] = 0x00;
    assert!(jpeg::decode(&broken, SIDE, PIXELS).is_err());
}

#[test]
fn the_crate_encoder_output_decodes_back_close_to_the_input() {
    let solid = image::RgbaImage::from_pixel(8, 8, image::Rgba([90, 120, 200, 255]));
    let decoded = jpeg::decode(&soul2d_core::export::encode_jpeg(&solid).unwrap(), SIDE, PIXELS).unwrap();
    for pixel in decoded.pixels() {
        for channel in 0..3 {
            assert!((i32::from(pixel[channel]) - i32::from(solid.get_pixel(0, 0)[channel])).abs() <= 2, "{pixel:?}");
        }
        assert_eq!(pixel[3], 255);
    }
    let gradient = image::RgbaImage::from_fn(21, 13, |x, y| image::Rgba([(x * 12) as u8, (y * 18) as u8, 128, 255]));
    let decoded = jpeg::decode(&soul2d_core::export::encode_jpeg(&gradient).unwrap(), SIDE, PIXELS).unwrap();
    assert_eq!(decoded.dimensions(), (21, 13));
    let worst = gradient
        .pixels()
        .zip(decoded.pixels())
        .flat_map(|(a, b)| (0..3).map(move |c| (i32::from(a[c]) - i32::from(b[c])).abs()))
        .max()
        .unwrap();
    assert!(worst <= 12, "worst channel error {worst}");
}

#[test]
fn unsupported_and_oversized_streams_are_refused_with_a_reason() {
    let valid = flat_jpeg(8, 8, &[(1, 1)], 0, &|_, _, _| 128);
    assert!(jpeg::decode(&valid, SIDE, PIXELS).is_ok());
    let patched = |find: &[u8], replace: &[u8]| {
        let mut bytes = valid.clone();
        let at = bytes.windows(find.len()).position(|w| w == find).unwrap();
        bytes[at..at + replace.len()].copy_from_slice(replace);
        bytes
    };
    let err = |bytes: &[u8]| jpeg::decode(bytes, SIDE, PIXELS).unwrap_err();
    assert_eq!(err(b""), "not a JPEG file");
    assert_eq!(err(b"\x89PNG"), "not a JPEG file");
    assert_eq!(
    err(&patched(&[0xff, 0xc0], &[0xff, 0xc2])),
    "progressive JPEG scan mixes DC and AC coefficients"
);
    assert_eq!(err(&patched(&[0xff, 0xc0], &[0xff, 0xc9])), "arithmetic-coded JPEG is not supported");
    // Frame body: length(2) precision(1) height(2) width(2) components(1) ...
    let frame = valid.windows(2).position(|w| w == [0xff, 0xc0]).unwrap() + 4;
    let mut twelve_bit = valid.clone();
    twelve_bit[frame] = 12;
    assert_eq!(err(&twelve_bit), "only 8-bit JPEG is supported");
    let mut huge = valid.clone();
    huge[frame + 1..frame + 5].copy_from_slice(&[0xff, 0xff, 0xff, 0xff]);
    assert_eq!(err(&huge), "65535x65535 is not a supported texture size");
    let mut zero = valid.clone();
    zero[frame + 3..frame + 5].copy_from_slice(&[0, 0]);
    assert_eq!(err(&zero), "JPEG has no size (DNL is not supported)");
    let mut sampling = valid.clone();
    sampling[frame + 7] = 0x51;
    assert_eq!(err(&sampling), "unsupported JPEG sampling factors");
    // The caller's own cap applies before anything is allocated.
    assert_eq!(jpeg::decode(&valid, 4, PIXELS).unwrap_err(), "8x8 is not a supported texture size");
    assert_eq!(jpeg::decode(&valid, SIDE, 63).unwrap_err(), "8x8 is not a supported texture size");
    // A scan whose Huffman table never arrived.
    let no_tables: Vec<u8> = {
        let mut bytes = valid.clone();
        let at = bytes.windows(2).position(|w| w == [0xff, 0xc4]).unwrap();
        bytes.drain(at..at + 2 + 2 + 29);
        bytes
    };
    assert_eq!(err(&no_tables), "JPEG DC Huffman table is missing");
}

#[test]
fn hostile_bytes_never_panic() {
    let seeds = [
        flat_jpeg(24, 10, &[(2, 2), (1, 1), (1, 1)], 2, &|c, bx, by| (c * 40 + bx * 70 + by * 30) as u8),
        flat_jpeg(16, 16, &[(1, 1)], 0, &|_, bx, by| (bx * 100 + by * 50) as u8),
        soul2d_core::export::encode_jpeg(&image::RgbaImage::from_fn(17, 9, |x, y| image::Rgba([x as u8 * 9, y as u8 * 20, 7, 255]))).unwrap(),
    ];
    for seed in &seeds {
        for length in 0..seed.len() {
            let _ = jpeg::decode(&seed[..length], SIDE, PIXELS);
        }
        let mut state = 0x2545_f491u32;
        for _ in 0..600 {
            let mut bytes = seed.clone();
            for _ in 0..3 {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let at = (state >> 8) as usize % bytes.len();
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                bytes[at] = (state >> 16) as u8;
            }
            let _ = jpeg::decode(&bytes, 64, 4096);
        }
    }
}

#[test]
fn jpeg_files_import_as_parts_through_merge_images() {
    let dir = temp_dir("jpeg-merge");
    let photo = image::RgbaImage::from_pixel(8, 8, image::Rgba([90, 120, 200, 255]));
    std::fs::write(dir.join("photo.JPG"), soul2d_core::export::encode_jpeg(&photo).unwrap()).unwrap();
    let mut arithmetic = soul2d_core::export::encode_jpeg(&photo).unwrap();
    let at = arithmetic.windows(2).position(|w| w == [0xff, 0xc0]).unwrap();
    arithmetic[at + 1] = 0xc9;
    std::fs::write(dir.join("broken.jpeg"), arithmetic).unwrap();
    let mut session = Session::new();
    ok(&mut session, json!({"op": "new"}));
    let merged = ok(&mut session, json!({"op": "merge.images", "paths": [dir.join("photo.JPG").to_str().unwrap()]}));
    assert_eq!(merged, json!({"created": [2], "parent": 1}));
    assert_eq!(session.snapshot()["model"]["nodes"]["children"][0]["name"], "photo.JPG");
    let revision = session.snapshot()["revision"].clone();
    let message = fail(&mut session, json!({"op": "merge.images", "paths": [dir.join("broken.jpeg").to_str().unwrap()]}));
    assert_eq!(message, "Could not load broken.jpeg: arithmetic-coded JPEG is not supported");
    assert_eq!(session.snapshot()["revision"], revision, "an Err leaves the document unchanged");
}

// ------------------------------------------------------------------------------------------ group rename

#[test]
fn parameter_rename_renames_groups_as_one_undo_step_and_rejects_taken_names() {
    let mut session = Session::new();
    ok(&mut session, json!({"op": "new"}));
    let leaf = ok(&mut session, json!({"op": "parameter.create", "name": "Head X", "dimensions": 1, "min": [-1.0, 0.0], "max": [1.0, 1.0]}));
    let group = ok(&mut session, json!({"op": "parameter.group.create", "members": [leaf["uuid"]]}));
    let group_id = group["groupUUID"].clone();
    assert!(group_id.is_u64(), "{group}");
    let before = session.snapshot()["history"]["position"].clone();
    let renamed = ok(&mut session, json!({"op": "parameter.rename", "id": group_id, "name": "Face"}));
    assert_eq!(renamed["name"], "Face");
    assert_eq!(renamed["groupUUID"], group_id);
    assert_eq!(session.snapshot()["model"]["param"][0]["name"], "Face");
    assert_eq!(session.snapshot()["history"]["position"].as_u64().unwrap(), before.as_u64().unwrap() + 1);
    assert_eq!(fail(&mut session, json!({"op": "parameter.rename", "id": group_id, "name": "Head X"})), "Name already taken");
    assert_eq!(session.snapshot()["model"]["param"][0]["name"], "Face", "an Err leaves the document unchanged");
    ok(&mut session, json!({"op": "undo"}));
    assert_ne!(session.snapshot()["model"]["param"][0]["name"], "Face");
}

// ------------------------------------------------------------------------------------------ manifest

#[test]
fn the_manifest_is_stable_sorted_and_states_only_what_is_true() {
    let text = soul2d_core::manifest::render().unwrap();
    assert_eq!(text, soul2d_core::manifest::render().unwrap(), "two renders are byte-identical");
    assert!(text.ends_with("}\n"));
    let manifest: Value = serde_json::from_str(&text).unwrap();
    let commands = manifest["commands"].as_array().unwrap();
    assert!(commands.len() > 300, "{} tools", commands.len());
    let ids: Vec<&str> = commands.iter().map(|c| c["id"].as_str().unwrap()).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(ids, sorted, "ids are sorted and unique");
    let entry = |id: &str| commands.iter().find(|c| c["id"] == id).unwrap_or_else(|| panic!("{id} is missing"));
    let save = entry("save");
    assert_eq!(save["effect"], "file-write");
    assert_eq!(save["scope"], json!(["global"]));
    assert_eq!(save["mcpExposed"], true);
    assert_eq!(save["headless_ok"], true);
    assert_eq!(save["inputSchema"]["type"], "object");
    assert!(save["description"].as_str().unwrap().len() > 10);
    assert_eq!(entry("new")["effect"], "project-reset");
    assert_eq!(entry("import_folder")["effect"], "import");
    assert_eq!(entry("snapshot")["effect"], "-");
    assert_eq!(entry("Motion3Command_List")["effect"], "-");
    // An operation that reaches no document-replacing sink but changes what the session holds (here the physics
    // and playback state) is `session-state`.
    assert_eq!(entry("physics_step")["effect"], "session-state");
    // A tool that takes `expectedRevision` edits the document; an editor tool with no file effect sets window state.
    assert!(entry("live2d_animation_key_add")["inputSchema"]["properties"].get("expectedRevision").is_some());
    assert_eq!(entry("live2d_animation_key_add")["effect"], "document-edit");
    assert_eq!(entry("editor_selection_set")["effect"], "editor-state");
    // An operation that reaches a document-replacing sink edits the document; one that only moves the preview
    // or a draft is `session-state`, through a D alias too.
    assert_eq!(entry("node_rename")["effect"], "document-edit");
    assert_eq!(entry("Inspector_Apply_PartOpacity")["effect"], "document-edit");
    assert_eq!(entry("animation_seek")["effect"], "session-state");
    assert_eq!(entry("split_part_set")["effect"], "session-state");
    assert_eq!(entry("DepthBoneCommand_PreviewDepthBoneDeform")["effect"], "session-state");
    // A D alias served by an arm that picks the operation by name takes its sibling tool's operation.
    assert_eq!(entry("DepthBoneCommand_AddDepthBoneSource")["effect"], "document-edit");
    assert_eq!(entry("Motion3Command_SetPoint")["effect"], "document-edit");
    assert_eq!(entry("editor_settings_get")["effect"], "-");
    // Every tool now states an effect.
    for command in commands {
        assert!(command.get("effect").is_some(), "{} has no effect", command["id"]);
    }
    // `guiEntry` comes from the inventory's Soul2D rows; a tool it does not name has no key.
    assert_eq!(entry("new")["guiEntry"], json!(["File > Close Project", "File > New", "Help > Welcome"]));
    assert!(entry("Motion3Command_List").get("guiEntry").is_none());
    assert_eq!(entry("ViewCommand_CaptureLiveScreenshot")["inputSchema"]["type"], "object");
    for id in ["render", "ViewCommand_CaptureLiveScreenshot"] {
        assert_eq!(entry(id)["effect"], "file-write", "{id} can publish a PNG when path is supplied");
        assert!(entry(id)["inputSchema"]["required"].as_array().is_none_or(|fields| !fields.contains(&json!("path"))));
    }
    for id in ["animation_onion", "motion3_export", "Motion3Command_Export"] {
        assert_eq!(entry(id)["effect"], "file-write", "{id}");
    }
    for id in ["import_inp", "import_folder", "FileCommand_ImportINP", "FileCommand_ImportImageFolder"] {
        assert_eq!(entry(id)["effect"], "import");
    }
    // The crate's own tools/list is where the entries come from.
    let mut session = Session::new();
    let listed = session.handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
    assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), commands.len());
    // `headless_ok` is false for exactly the tools a host without a window refuses.
    let editor_only: Vec<&str> = commands.iter().filter(|c| c["headless_ok"] == false).map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(
        editor_only,
        [
            "editor_about_set", "editor_address_dialog", "editor_armed_set", "editor_camera_fit", "editor_camera_set", "editor_camera_sync", "editor_command_browser_set", "editor_grid_set", "editor_guides_set",
            "editor_layout_reset", "editor_native_parameters_set", "editor_onion_set", "editor_panel_set", "editor_parameters_set", "editor_physics_preview_set", "editor_playback_set", "editor_quit", "editor_recovery_keep", "editor_recovery_list", "editor_recovery_restore",
            "editor_selection_set", "editor_settings_get", "editor_settings_set", "editor_shortcut_set", "editor_solo_set",
            "editor_texture_viewer_set", "editor_timeline_zoom_set", "editor_tool_set", "editor_viewport_set", "editor_welcome_set", "editor_workspace_apply", "editor_workspace_delete", "editor_workspace_save",
        ]
    );
    for command in commands {
        let id = command["id"].as_str().unwrap();
        assert!(command["headless_ok"].is_boolean(), "{id}");
        if command["headless_ok"] == false {
            let effect = match id {
                "editor_address_dialog" => Some("clipboard-write"),
                "editor_quit" => Some("application-lifecycle"),
                "editor_recovery_restore" => Some("project-reset"),
                "editor_settings_set" | "editor_shortcut_set" | "editor_panel_set" | "editor_layout_reset" | "editor_workspace_save" | "editor_workspace_apply" | "editor_workspace_delete" => {
                    Some("file-write")
                }
                "editor_recovery_list" | "editor_settings_get" => Some("-"),
                _ => Some("editor-state"),
            };
            assert_eq!(command.get("effect").and_then(Value::as_str), effect, "{id}");
            let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": id, "arguments": {}}});
            let refused = session.handle_mcp(&call).unwrap()["result"].clone();
            assert_eq!(refused["isError"], true, "{id}");
            assert_eq!(refused["content"][0]["text"], format!("{id} needs the editor: this host has no window"));
        }
    }
}

#[test]
fn export_command_manifest_writes_and_checks_through_the_binary() {
    let dir = temp_dir("manifest");
    let path = dir.join("commands.manifest.json");
    let binary = env!("CARGO_BIN_EXE_soul2d-core");
    let run = |args: &[&str]| std::process::Command::new(binary).args(args).output().unwrap();
    let missing = run(&["--export-command-manifest", path.to_str().unwrap(), "--check"]);
    assert_eq!(missing.status.code(), Some(1), "checking a missing file fails");
    let written = run(&["--export-command-manifest", path.to_str().unwrap()]);
    assert!(written.status.success(), "{}", String::from_utf8_lossy(&written.stderr));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), soul2d_core::manifest::render().unwrap());
    assert!(run(&["--export-command-manifest", path.to_str().unwrap(), "--check"]).status.success());
    std::fs::write(&path, "{}\n").unwrap();
    let stale = run(&["--check", "--export-command-manifest", path.to_str().unwrap()]);
    assert_eq!(stale.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&stale.stderr).contains("out of date"));
}

// ------------------------------------------------------------------------------------------ live screenshot

fn call_tool(session: &mut Session, name: &str, arguments: Value) -> Value {
    let reply = session
        .handle_mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": arguments}}))
        .unwrap();
    reply["result"].clone()
}

fn apply(matrix: &Value, point: [f64; 2]) -> [f64; 2] {
    let row = |r: usize| {
        let cells: Vec<f64> = matrix[r].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
        cells[0] * point[0] + cells[1] * point[1] + cells[3]
    };
    [row(0), row(1)]
}

#[test]
fn live_screenshot_defaults_bounds_applies_a_parameter_value_and_reports_the_world_mapping() {
    let dir = temp_dir("capture");
    let png = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255]));
    png.save(dir.join("red.png")).unwrap();
    let mut session = Session::new();
    ok(&mut session, json!({"op": "new"}));
    ok(&mut session, json!({"op": "merge.images", "paths": [dir.join("red.png").to_str().unwrap()]}));
    let parameter = ok(&mut session, json!({"op": "parameter.create", "name": "Slide", "dimensions": 1, "min": [0.0, 0.0], "max": [1.0, 1.0]}));
    let id = parameter["uuid"].as_u64().unwrap();
    ok(&mut session, json!({"op": "binding.set_trs", "parameter": id, "nodes": [2], "key": [0, 0], "translation": [0.0, 0.0]}));
    ok(&mut session, json!({"op": "binding.set_trs", "parameter": id, "nodes": [2], "key": [1, 0], "translation": [10.0, 0.0]}));
    let before = session.snapshot();

    let plain = call_tool(&mut session, "ViewCommand_CaptureLiveScreenshot", json!({}));
    assert_eq!(plain["isError"], false, "{plain}");
    assert_eq!(plain["content"][0]["type"], "image");
    assert_eq!((plain["_meta"]["width"].clone(), plain["_meta"]["height"].clone()), (json!(640), json!(480)));
    let bounds: Vec<f64> = plain["_meta"]["bounds"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    assert!(bounds[0] < -2.0 && bounds[2] > 2.0 && bounds[0] > -3.0, "scene bounds with a margin: {bounds:?}");
    let capture = &plain["_meta"]["worldCapture"];
    assert_eq!(capture["coordinateSystem"], "soul2d-world");
    assert_eq!(capture["pixelOrigin"], "top-left");
    assert_eq!(capture["aabb"], json!({"min": [bounds[0], bounds[1]], "max": [bounds[2], bounds[3]]}));
    let to_image = apply(&capture["worldToImageMatrix"], [bounds[0], bounds[1]]);
    assert!(to_image[0].abs() < 1e-9 && to_image[1].abs() < 1e-9, "min corner is pixel (0,0): {to_image:?}");
    let far = apply(&capture["worldToImageMatrix"], [bounds[2], bounds[3]]);
    assert!((far[0] - 640.0).abs() < 1e-9 && (far[1] - 480.0).abs() < 1e-9, "{far:?}");
    let clip = apply(&capture["worldToClipMatrix"], [bounds[0], bounds[1]]);
    assert!((clip[0] + 1.0).abs() < 1e-9 && (clip[1] - 1.0).abs() < 1e-9, "top-left is clip (-1, 1): {clip:?}");
    let back = apply(&capture["clipToWorldMatrix"], [1.0, -1.0]);
    assert!((back[0] - bounds[2]).abs() < 1e-9 && (back[1] - bounds[3]).abs() < 1e-9);
    let bytes = base64_decode(plain["content"][0]["data"].as_str().unwrap());
    let decoded = image::load_from_memory(&bytes).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (640, 480));

    // The parameter value moves the framing by the binding's translation and sets nothing in the session.
    let moved = call_tool(
        &mut session,
        "ViewCommand_CaptureLiveScreenshot",
        json!({"width": 100, "height": 50, "context": {"parameters": [id], "parameterValue": [1.0, 0.0]}}),
    );
    assert_eq!(moved["isError"], false, "{moved}");
    assert_eq!((moved["_meta"]["width"].clone(), moved["_meta"]["height"].clone()), (json!(100), json!(50)));
    let shifted = moved["_meta"]["bounds"][0].as_f64().unwrap() - bounds[0];
    assert!((shifted - 10.0).abs() < 1e-6, "framing moved by {shifted}");
    assert_eq!(session.snapshot(), before, "capture changes neither values nor revision");

    // Explicit bounds are used as given.
    let explicit = call_tool(&mut session, "ViewCommand_CaptureLiveScreenshot", json!({"width": 8, "height": 8, "bounds": [-4.0, -4.0, 4.0, 4.0]}));
    assert_eq!(explicit["_meta"]["bounds"], json!([-4.0, -4.0, 4.0, 4.0]));
    let center = apply(&explicit["_meta"]["worldCapture"]["worldToImageMatrix"], [0.0, 0.0]);
    assert_eq!(center, [4.0, 4.0]);

    for (arguments, expected) in [
        (json!({"overlayObjects": [{"uuid": 2, "overlay": "bounds"}]}), "overlayObjects are not supported in Rust Core mode"),
        (json!({"context": {"parameters": [id, id], "parameterValue": [1.0, 0.0]}}), "screenshot requires one parameter UUID"),
        (json!({"context": {"parameters": [id], "parameterValue": [1.0]}}), "context.parameterValue must be two finite numbers"),
        (json!({"width": 0}), "width must be a positive integer"),
    ] {
        let refused = call_tool(&mut session, "ViewCommand_CaptureLiveScreenshot", arguments);
        assert_eq!(refused["isError"], true);
        assert_eq!(refused["content"][0]["text"], expected);
    }
}

fn base64_decode(text: &str) -> Vec<u8> {
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, text).unwrap()
}

// ------------------------------------------------------------------------------------------ window shell

use soul2d_core::settings::Settings;
use soul2d_core::window_shell::{self as shell, CloseSetting, DropAction, Gate, WindowGeometry};
use std::path::{Path, PathBuf};

#[test]
fn unsaved_changes_are_asked_about_only_when_modified_and_the_setting_says_ask() {
    let mut store = Settings::empty();
    assert_eq!(shell::close_setting(&mut store), CloseSetting::Ask, "D default is Always ask");
    assert_eq!(shell::gate(true, CloseSetting::Ask), Gate::Ask);
    assert_eq!(shell::gate(false, CloseSetting::Ask), Gate::Proceed);
    assert_eq!(shell::gate(true, CloseSetting::DontSave), Gate::Proceed);
    shell::set_close_setting(&mut store, CloseSetting::DontSave).unwrap();
    assert_eq!(store.get::<String>("SaveProjectOnClose").unwrap(), "dontSave");
    assert_eq!(shell::close_setting(&mut store), CloseSetting::DontSave);
    // D rejects values outside its option list (including "Save") and writes "Ask" back.
    store.set("SaveProjectOnClose", "Save".to_owned()).unwrap();
    assert_eq!(shell::close_setting(&mut store), CloseSetting::Ask);
    assert_eq!(store.get::<String>("SaveProjectOnClose").unwrap(), "Ask");
}

#[test]
fn save_asks_for_a_path_only_when_the_document_has_none() {
    assert_eq!(shell::save_target(Some(Path::new("/p/a.inx")), ""), Some("/p/a.inx".to_owned()));
    assert_eq!(shell::save_target(Some(Path::new("/p/a.inx")), " /p/b.inx "), Some("/p/b.inx".to_owned()));
    assert_eq!(shell::save_target(None, "  "), None);
    assert_eq!(shell::save_target(None, "/p/new.inx"), Some("/p/new.inx".to_owned()));
}

#[test]
fn the_title_follows_d_format() {
    assert_eq!(shell::window_title(None, false), "Soul2D");
    assert_eq!(shell::window_title(None, true), "Soul2D");
    assert_eq!(shell::window_title(Some("hero.inx"), false), "Soul2D - hero.inx");
    assert_eq!(shell::window_title(Some("hero.inx"), true), "Soul2D - hero.inx *");
    assert_eq!(shell::document_subtitle(Some(Path::new("/work/hero.inx")), true, "New Project"), Some("hero.inx".to_owned()));
    assert_eq!(shell::document_subtitle(None, true, "New Project"), Some("New Project".to_owned()));
    assert_eq!(shell::document_subtitle(None, false, "New Project"), None);
}

#[test]
fn window_geometry_defaults_clamps_and_keeps_the_normal_size_under_maximise() {
    let mut store = Settings::empty();
    assert_eq!(WindowGeometry::load(&store), WindowGeometry { width: 1280.0, height: 800.0, maximized: false });
    store.set("WinW", 5i64).unwrap();
    store.set("WinH", 99_999i64).unwrap();
    store.set("WinMax", true).unwrap();
    assert_eq!(WindowGeometry::load(&store), WindowGeometry { width: 320.0, height: 16384.0, maximized: true });
    let mut geometry = WindowGeometry { width: 1280.0, height: 800.0, maximized: false };
    geometry.observe(Some((900.0, 700.0)), Some(false));
    assert_eq!((geometry.width, geometry.height), (900.0, 700.0));
    geometry.observe(Some((1920.0, 1080.0)), Some(true));
    assert_eq!((geometry.width, geometry.height, geometry.maximized), (900.0, 700.0, true));
    geometry.observe(Some((f32::NAN, 10.0)), None);
    assert_eq!(geometry.width, 900.0);
    geometry.store(&mut store).unwrap();
    assert_eq!(WindowGeometry::load(&store), geometry);
}

fn paths(names: &[&str]) -> Vec<PathBuf> {
    names.iter().map(PathBuf::from).collect()
}

#[test]
fn dropped_files_follow_d_order_and_a_project_ends_the_walk() {
    assert_eq!(
        shell::plan_drops(&paths(&["/a/x.PNG", "/a/y.jpg", "/a/z.TGA"])),
        [DropAction::MergeImages(paths(&["/a/x.PNG", "/a/y.jpg", "/a/z.TGA"]))]
    );
    assert_eq!(
        shell::plan_drops(&paths(&["/a/x.png", "/a/notes.txt", "/a/y.jpeg", "/a/hero.INX", "/a/late.png"])),
        [
            DropAction::MergeImages(paths(&["/a/x.png"])),
            DropAction::Unsupported("notes.txt".into()),
            DropAction::MergeImages(paths(&["/a/y.jpeg"])),
            DropAction::Open(PathBuf::from("/a/hero.INX")),
        ]
    );
    assert_eq!(shell::plan_drops(&paths(&["/a/old.inp"])), [DropAction::Open(PathBuf::from("/a/old.inp"))]);
    assert_eq!(shell::plan_drops(&paths(&["/a/art.psd", "/a/x.png"])), [DropAction::ImportLayers(PathBuf::from("/a/art.psd"))]);
    assert_eq!(shell::plan_drops(&paths(&["/a/noext"])), [DropAction::Unsupported("noext".into())]);
    assert!(shell::plan_drops(&[]).is_empty());
    let request = shell::merge_images_request(&paths(&["/a/p|q.png"]), Some(7)).unwrap();
    assert_eq!(request, json!({"op": "merge.images", "paths": ["/a/p|q.png"], "parent": 7}));
}

#[test]
fn dropped_images_merge_as_one_undo_step_and_a_bad_one_changes_nothing() {
    let dir = temp_dir("drop");
    image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255])).save(dir.join("a.png")).unwrap();
    std::fs::write(dir.join("b.jpg"), soul2d_core::export::encode_jpeg(&image::RgbaImage::from_pixel(8, 8, image::Rgba([9, 9, 9, 255]))).unwrap()).unwrap();
    let mut session = Session::new();
    ok(&mut session, json!({"op": "new"}));
    let plan = shell::plan_drops(&[dir.join("a.png"), dir.join("b.jpg")]);
    let DropAction::MergeImages(images) = &plan[0] else { panic!("{plan:?}") };
    let created = ok(&mut session, shell::merge_images_request(images, None).unwrap());
    assert_eq!(created["created"], json!([2, 3]));
    ok(&mut session, json!({"op": "undo"}));
    assert_eq!(session.snapshot()["model"]["nodes"]["children"], Value::Null, "one undo removes both Parts");
    let revision = session.snapshot()["revision"].clone();
    let missing = shell::merge_images_request(&[dir.join("a.png"), dir.join("gone.png")], None).unwrap();
    assert!(fail(&mut session, missing).contains("gone.png"));
    assert_eq!(session.snapshot()["revision"], revision);
}

// ------------------------------------------------------------------------------------------ autosave records

use soul2d_core::autosave::{self, Now};

#[test]
fn previous_autosaves_are_a_deduplicated_front_first_list_of_ten_pruned_by_existence() {
    let dir = temp_dir("records");
    let mut store = Settings::empty();
    assert!(autosave::prev_autosaves(&store).is_empty());
    let file = |n: usize| {
        let path = dir.join(format!("backup_{n}.inx"));
        std::fs::write(&path, b"x").unwrap();
        path.to_str().unwrap().to_owned()
    };
    for n in 0..12 {
        autosave::add_prev_autosave(&mut store, &file(n), "/p/main.inx").unwrap();
    }
    let records = autosave::prev_autosaves(&store);
    assert_eq!(records.len(), 10);
    assert_eq!(records[0].autosave, file(11));
    assert_eq!(records[9].autosave, file(2));
    // Re-adding moves the record to the front and replaces its main path.
    autosave::add_prev_autosave(&mut store, &file(5), "/p/other.inx").unwrap();
    let records = autosave::prev_autosaves(&store);
    assert_eq!((records[0].autosave.clone(), records[0].mainsave.clone()), (file(5), "/p/other.inx".to_owned()));
    assert_eq!(records.len(), 10);
    assert_eq!(records.iter().filter(|r| r.autosave == file(5)).count(), 1);
    assert_eq!(store.get::<Vec<String>>("prev_autosaves").unwrap().len(), 10);
    assert_eq!(store.get::<Vec<String>>("prev_autosave_mainpaths").unwrap()[0], "/p/other.inx");
    // Pruning drops the records whose file is gone.
    std::fs::remove_file(file(11)).unwrap();
    std::fs::remove_file(file(2)).unwrap();
    assert_eq!(autosave::prune_prev_autosaves(&mut store).unwrap(), 2);
    assert_eq!(autosave::prev_autosaves(&store).len(), 8);
    // A shorter main-path list reads as empty paths (D tolerates old settings files).
    store.set("prev_autosave_mainpaths", Vec::<String>::new()).unwrap();
    assert!(autosave::prev_autosaves(&store).iter().all(|r| r.mainsave.is_empty()));
}

#[test]
fn a_recent_autosave_opens_under_the_project_path_and_modified() {
    let dir = temp_dir("record-open");
    let main = dir.join("main.inx");
    let backup = dir.join("main_100.inx");
    let mut session = Session::new();
    ok(&mut session, json!({"op": "new"}));
    ok(&mut session, json!({"op": "save", "path": main.to_str().unwrap()}));
    ok(&mut session, json!({"op": "parameter.create", "name": "Edited", "dimensions": 1, "min": [0.0, 0.0], "max": [1.0, 1.0]}));
    ok(&mut session, json!({"op": "save", "path": backup.to_str().unwrap(), "mark_saved": false}));
    let record = autosave::AutosaveRecord { autosave: backup.to_str().unwrap().to_owned(), mainsave: main.to_str().unwrap().to_owned() };
    let mut fresh = Session::new();
    ok(&mut fresh, autosave::open_record_request(&record));
    let snapshot = fresh.snapshot();
    assert_eq!(snapshot["model"]["param"][0]["name"], "Edited");
    assert_eq!(snapshot["modified"], true, "the main file does not have the edit");
    // Saving writes the project, never the backup.
    let backup_bytes = std::fs::read(&backup).unwrap();
    let main_before = std::fs::read(&main).unwrap();
    ok(&mut fresh, json!({"op": "save", "path": main.to_str().unwrap()}));
    assert_ne!(std::fs::read(&main).unwrap(), main_before, "the project file received the edit");
    assert_eq!(std::fs::read(&backup).unwrap(), backup_bytes, "the backup is untouched");
    assert_eq!(fresh.snapshot()["modified"], false);
    assert_eq!(autosave::open_record_request(&autosave::AutosaveRecord { autosave: "b.inx".into(), mainsave: String::new() }),
        json!({"op": "open", "path": "b.inx", "untitled": true}));
}

// ------------------------------------------------------------------------------------------ untitled recovery

fn now(mono: u64, unix: u64) -> Now {
    Now { mono_secs: mono, unix_secs: unix }
}

#[test]
fn recovery_paths_follow_d_naming() {
    assert!(autosave::is_recovery_path(Path::new("/c/RustRecovery/rust-recovery-1-2.inx")));
    assert!(autosave::is_recovery_path(Path::new("rust-recovery-1-2-3.cmo3")));
    assert!(autosave::is_recovery_path(Path::new("rust-recovery-x.can3")));
    assert!(!autosave::is_recovery_path(Path::new("rust-recovery-.inx")), "needs something after the prefix");
    assert!(!autosave::is_recovery_path(Path::new("rust-recovery-1.png")));
    assert!(!autosave::is_recovery_path(Path::new("my-recovery-1.inx")));
    assert!(!autosave::is_recovery_path(Path::new("rust-recovery-1")));
}

#[test]
fn an_untitled_document_gets_one_rolling_snapshot_every_thirty_seconds_while_modified() {
    let config = temp_dir("recovery");
    let mut recovery = autosave::Recovery::new(&config, now(100, 1_700_000_000));
    let write = |path: &Path| std::fs::write(path, b"snapshot").map_err(|e| e.to_string());
    assert_eq!(recovery.tick(true, "inx", 77, now(129, 1_700_000_029), write).unwrap(), None, "before 30 s");
    assert_eq!(recovery.tick(false, "inx", 77, now(140, 1_700_000_040), write).unwrap(), None, "unmodified");
    let first = recovery.tick(true, "inx", 77, now(131, 1_700_000_031), write).unwrap().unwrap();
    assert_eq!(first, config.join("RustRecovery").join("rust-recovery-17000000310000000-77.inx"));
    assert_eq!(recovery.current(), Some(first.as_path()));
    assert_eq!(recovery.tick(true, "inx", 77, now(150, 1_700_000_050), write).unwrap(), None, "timer restarted");
    let second = recovery.tick(true, "cmo3", 77, now(165, 1_700_000_065), write).unwrap().unwrap();
    assert!(second.to_str().unwrap().ends_with("-77.cmo3"));
    assert!(!first.exists(), "the previous snapshot is replaced");
    assert_eq!(autosave::list_recovery(&config).unwrap(), [second.clone()]);
    // The same tick in the same second never overwrites a file.
    let mut other = autosave::Recovery::new(&config, now(0, 1_700_000_065));
    let clash = other.tick(true, "cmo3", 77, now(30, 1_700_000_065), write).unwrap().unwrap();
    assert!(clash.to_str().unwrap().ends_with("-77-1.cmo3"), "{clash:?}");
    // Saving the document releases this session's snapshot and only that one.
    recovery.release(now(170, 1_700_000_070));
    assert!(!second.exists() && clash.exists());
    assert!(recovery.current().is_none());
    // A failed write reports, keeps the old snapshot and still restarts the timer.
    let kept = other.current().map(Path::to_path_buf).unwrap();
    let error = other.tick(true, "inx", 77, now(70, 1_700_000_105), |_| Err("disk full".into())).unwrap_err();
    assert_eq!(error, "Recovery save failed: disk full");
    assert!(kept.exists());
    assert_eq!(other.tick(true, "inx", 77, now(71, 1_700_000_106), write).unwrap(), None);
    assert!(recovery.tick(true, "png", 1, now(500, 1_700_000_500), write).is_err(), "unknown extension");
}

#[test]
fn recovery_files_list_newest_first_delete_only_themselves_and_reopen_as_untitled() {
    let config = temp_dir("recovery-list");
    let dir = config.join("RustRecovery");
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["rust-recovery-1-9.inx", "rust-recovery-3-9.inx", "rust-recovery-2-9.can3", "notes.txt", "rust-recovery-4.png"] {
        std::fs::write(dir.join(name), b"x").unwrap();
    }
    let listed: Vec<String> = autosave::list_recovery(&config).unwrap().iter().map(|p| p.file_name().unwrap().to_str().unwrap().to_owned()).collect();
    assert_eq!(listed, ["rust-recovery-3-9.inx", "rust-recovery-2-9.can3", "rust-recovery-1-9.inx"]);
    assert!(autosave::delete_recovery(&config, &dir.join("notes.txt")).is_err());
    assert!(autosave::delete_recovery(&config, &config.join("rust-recovery-1-9.inx")).is_err(), "outside the directory");
    autosave::delete_recovery(&config, &dir.join("rust-recovery-1-9.inx")).unwrap();
    assert_eq!(autosave::list_recovery(&config).unwrap().len(), 2);
    assert!(autosave::list_recovery(&temp_dir("recovery-empty")).unwrap().is_empty());

    // A snapshot of an unsaved document restores as an untitled, modified document.
    let mut session = Session::new();
    ok(&mut session, json!({"op": "new"}));
    ok(&mut session, json!({"op": "parameter.create", "name": "Unsaved work", "dimensions": 1, "min": [0.0, 0.0], "max": [1.0, 1.0]}));
    let snapshot_path = dir.join("rust-recovery-7-9.inx");
    ok(&mut session, json!({"op": "save", "path": snapshot_path.to_str().unwrap(), "mark_saved": false}));
    assert_eq!(session.snapshot()["modified"], true, "writing a snapshot leaves the document modified");
    let mut restored = Session::new();
    ok(&mut restored, autosave::recovery_open_request(&snapshot_path).unwrap());
    let state = restored.snapshot();
    assert_eq!(state["model"]["param"][0]["name"], "Unsaved work");
    assert_eq!(state["modified"], true);
    assert!(fail(&mut restored, json!({"op": "save"})).contains("path"), "untitled: Save has to ask for a path");
    assert_eq!(fail(&mut restored, json!({"op": "open", "path": snapshot_path.to_str().unwrap(), "untitled": true, "as": "/x.inx"})), "open takes either as or untitled, not both");
}

// ------------------------------------------------------------------------------------------ MCP from settings

use soul2d_core::mcp_listener::{self as listener, Auth, McpServer, McpSettings};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::Duration;

#[test]
fn mcp_settings_are_off_by_default_and_round_trip_through_the_store() {
    let mut store = Settings::empty();
    assert_eq!(McpSettings::load(&store), McpSettings { enabled: false, host: "127.0.0.1".into(), port: 8088, auth: false });
    let wanted = McpSettings { enabled: true, host: "localhost".into(), port: 9001, auth: true };
    wanted.store(&mut store).unwrap();
    assert_eq!(store.get::<bool>("MCP.Enabled").unwrap(), true);
    assert_eq!(store.get::<String>("MCP.Host").unwrap(), "localhost");
    assert_eq!(store.get::<i64>("MCP.Port").unwrap(), 9001);
    assert_eq!(store.get::<bool>("MCP.authEnabled").unwrap(), true);
    assert_eq!(McpSettings::load(&store), wanted);
    store.set("MCP.Port", 70_000i64).unwrap();
    store.set("MCP.Host", "  ".to_owned()).unwrap();
    let loaded = McpSettings::load(&store);
    assert_eq!((loaded.port, loaded.host.as_str()), (8088, "127.0.0.1"), "a bad port or host falls back");
}

#[test]
fn auth_follows_the_soul2d_core_rules() {
    let local = McpSettings { enabled: true, ..McpSettings::default() };
    assert_eq!(listener::decide_auth(&local, None).unwrap(), Auth::Open);
    assert_eq!(listener::decide_auth(&local, Some(String::new())).unwrap(), Auth::Open, "an empty variable is no token");
    let secured = McpSettings { auth: true, ..local.clone() };
    let Auth::Generated(token) = listener::decide_auth(&secured, None).unwrap() else { panic!("expected a generated token") };
    assert_eq!(token.len(), 64);
    assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
    let Auth::Generated(other) = listener::decide_auth(&secured, None).unwrap() else { panic!() };
    assert_ne!(token, other, "tokens are random");
    assert_eq!(listener::decide_auth(&secured, Some("env-token".into())).unwrap(), Auth::Configured("env-token".into()));
    let exposed = McpSettings { host: "0.0.0.0".into(), ..secured.clone() };
    let refused = listener::decide_auth(&exposed, None).unwrap_err();
    assert!(refused.contains("SOUL2D_MCP_BEARER") && refused.contains("0.0.0.0"), "{refused}");
    assert_eq!(listener::decide_auth(&exposed, Some("t".into())).unwrap(), Auth::Configured("t".into()));
    assert!(McpServer::start(&exposed, None).is_err(), "nothing is bound without a bearer");
}

fn post(port: u16, token: Option<&str>, body: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    write!(stream, "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).unwrap();
    out
}

#[test]
fn the_settings_server_answers_waiting_connections_per_frame_through_the_handler() {
    let settings = McpSettings { enabled: true, port: 0, auth: true, ..McpSettings::default() };
    let server = McpServer::start(&settings, None).unwrap();
    let token = server.auth().token().unwrap().to_owned();
    let port: u16 = server.url().rsplit_once(':').unwrap().1.trim_end_matches("/mcp").parse().unwrap();
    assert!(server.url().starts_with("http://127.0.0.1:"));
    let mut session = Session::new();
    let mut seen = Vec::new();
    // Idle: a frame with no connection returns at once.
    assert_eq!(server.serve_pending(4, listener::CONNECTION_BUDGET, &mut |m| soul2d_core::mcp_http::RpcReply::plain(session.handle_mcp(m))), 0);
    let clients: Vec<_> = [Some(token.clone()), Some("wrong".to_owned()), None]
        .into_iter()
        .map(|t| std::thread::spawn(move || post(port, t.as_deref(), r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)))
        .collect();
    let mut served = 0;
    for _ in 0..200 {
        served += server.serve_pending(4, Duration::from_secs(2), &mut |message| {
            seen.push(message["method"].as_str().unwrap().to_owned());
            soul2d_core::mcp_http::RpcReply::plain(session.handle_mcp(message))
        });
        if served >= 3 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(served, 3);
    let replies: Vec<String> = clients.into_iter().map(|c| c.join().unwrap()).collect();
    assert!(replies[0].starts_with("HTTP/1.1 200") && replies[0].contains("\"tools\""), "{}", replies[0]);
    assert!(replies[1].starts_with("HTTP/1.1 401") && replies[2].starts_with("HTTP/1.1 401"), "{replies:?}");
    assert_eq!(seen, ["tools/list"], "unauthorised requests never reach the handler");
}

// ------------------------------------------------------------------------------------------ binding values

use soul2d_core::binding_values::{self as values, TrsChannel};

#[test]
fn trs_values_read_per_keypoint_and_each_write_is_one_undo_step() {
    let dir = temp_dir("trs");
    image::RgbaImage::from_pixel(4, 2, image::Rgba([255, 0, 0, 255])).save(dir.join("p.png")).unwrap();
    let mut session = Session::new();
    ok(&mut session, json!({"op": "new"}));
    ok(&mut session, json!({"op": "merge.images", "paths": [dir.join("p.png").to_str().unwrap()]}));
    let parameter = ok(&mut session, json!({"op": "parameter.create", "name": "Slide", "dimensions": 1, "min": [0.0, 0.0], "max": [1.0, 1.0]}));
    let id = parameter["uuid"].as_u64().unwrap() as u32;
    let model = |session: &Session| session.snapshot()["model"].clone();

    let neutral = values::read_trs(&model(&session), id, 2, [1, 0]).unwrap();
    assert_eq!(neutral.translation, [0.0, 0.0]);
    assert_eq!((neutral.scale, neutral.rotation_degrees, neutral.translation_z), ([1.0, 1.0], 0.0, 0.0));
    assert_eq!(neutral.is_set, [false; 8]);

    let mut edited = neutral.clone();
    edited.translation = [4.0, -2.0];
    ok(&mut session, values::trs_request(id, 2, [1, 0], TrsChannel::Translation, &edited).unwrap());
    edited.rotation_degrees = 90.0;
    ok(&mut session, values::trs_request(id, 2, [1, 0], TrsChannel::Rotation, &edited).unwrap());
    edited.rotation_x_degrees = 30.0;
    ok(&mut session, values::trs_request(id, 2, [1, 0], TrsChannel::RotationX, &edited).unwrap());
    edited.scale = [2.0, 0.5];
    ok(&mut session, values::trs_request(id, 2, [1, 0], TrsChannel::Scale, &edited).unwrap());
    edited.translation_z = 3.0;
    ok(&mut session, values::trs_request(id, 2, [1, 0], TrsChannel::TranslationZ, &edited).unwrap());
    edited.rotation_y_degrees = -45.0;
    ok(&mut session, values::trs_request(id, 2, [1, 0], TrsChannel::RotationY, &edited).unwrap());

    let read = values::read_trs(&model(&session), id, 2, [1, 0]).unwrap();
    assert_eq!(read.translation, [4.0, -2.0]);
    assert_eq!(read.scale, [2.0, 0.5]);
    assert_eq!(read.translation_z, 3.0);
    assert!((read.rotation_degrees - 90.0).abs() < 1e-9);
    assert!((read.rotation_x_degrees - 30.0).abs() < 1e-9);
    assert!((read.rotation_y_degrees + 45.0).abs() < 1e-9);
    assert_eq!(read.is_set, [true; 8]);
    // The key at index 0 keeps its neutral values.
    let first = values::read_trs(&model(&session), id, 2, [0, 0]).unwrap();
    assert_eq!((first.translation, first.scale, first.rotation_degrees), ([0.0, 0.0], [1.0, 1.0], 0.0));
    // Radians in the file, as D wrote them.
    let stored = &session.snapshot()["model"]["param"][0]["bindings"];
    let rz = stored.as_array().unwrap().iter().find(|b| b["param_name"] == "transform.r.z").unwrap();
    assert!((rz["values"][1][0].as_f64().unwrap() - std::f64::consts::FRAC_PI_2).abs() < 1e-9);

    // Each channel write was exactly one undo step.
    ok(&mut session, json!({"op": "undo"}));
    assert_eq!(values::read_trs(&model(&session), id, 2, [1, 0]).unwrap().rotation_y_degrees, 0.0);
    assert_eq!(values::read_trs(&model(&session), id, 2, [1, 0]).unwrap().translation_z, 3.0);

    assert_eq!(values::read_trs(&model(&session), id, 2, [5, 0]).unwrap_err(), "Select an existing parameter keypoint");
    assert_eq!(values::read_trs(&model(&session), 999, 2, [0, 0]).unwrap_err(), "Parameter not found: 999");
    let mut bad = read.clone();
    bad.scale = [f64::NAN, 1.0];
    assert_eq!(values::trs_request(id, 2, [1, 0], TrsChannel::Scale, &bad).unwrap_err(), "Binding values must be finite");
    let revision = session.snapshot()["revision"].clone();
    assert!(fail(&mut session, values::trs_request(id, 77, [1, 0], TrsChannel::Translation, &read).unwrap()).contains("77"));
    assert_eq!(session.snapshot()["revision"], revision, "an Err leaves the document unchanged");
}

#[test]
fn deform_offsets_read_per_vertex_and_write_as_one_undo_step() {
    let dir = temp_dir("deform");
    image::RgbaImage::from_pixel(4, 2, image::Rgba([255, 0, 0, 255])).save(dir.join("p.png")).unwrap();
    let mut session = Session::new();
    ok(&mut session, json!({"op": "new"}));
    ok(&mut session, json!({"op": "merge.images", "paths": [dir.join("p.png").to_str().unwrap()]}));
    let parameter = ok(&mut session, json!({"op": "parameter.create", "name": "Slide", "dimensions": 1, "min": [0.0, 0.0], "max": [1.0, 1.0]}));
    let id = parameter["uuid"].as_u64().unwrap() as u32;
    let model = |session: &Session| session.snapshot()["model"].clone();
    let fresh = values::read_deform(&model(&session), id, 2, [1, 0]).unwrap();
    assert_eq!(fresh.offsets, vec![[0.0, 0.0]; 4], "a quad Part has four vertices");
    assert!(!fresh.is_set);
    let offsets = [[1.0, 2.0], [3.0, 4.0], [-1.5, 0.25], [0.0, 8.0]];
    ok(&mut session, values::deform_request(id, 2, [1, 0], &offsets).unwrap());
    let read = values::read_deform(&model(&session), id, 2, [1, 0]).unwrap();
    assert_eq!(read, values::DeformValues { offsets: offsets.to_vec(), is_set: true });
    assert!(!values::read_deform(&model(&session), id, 2, [0, 0]).unwrap().is_set);
    ok(&mut session, json!({"op": "undo"}));
    assert!(!values::read_deform(&model(&session), id, 2, [1, 0]).unwrap().is_set, "one undo step removes the key");
    assert_eq!(values::read_deform(&model(&session), id, 1, [1, 0]).unwrap_err(), "Node 1 has no deform geometry");
    assert_eq!(values::deform_request(id, 2, [1, 0], &[]).unwrap_err(), "Manual deform offsets must contain one XY pair per vertex");
    assert_eq!(values::deform_request(id, 2, [1, 0], &[[f64::INFINITY, 0.0]]).unwrap_err(), "Manual deform offsets must be finite");
    let revision = session.snapshot()["revision"].clone();
    assert!(fail(&mut session, values::deform_request(id, 2, [1, 0], &offsets[..3]).unwrap()).contains("exactly 4"));
    assert_eq!(session.snapshot()["revision"], revision);
}

// ------------------------------------------------------------------------------------------ controller

use soul2d_core::editor_controller::{replaces_document, EditorController, UNSAVED_REPLACEMENT_MESSAGE};

#[test]
fn only_replacing_requests_are_held_back_and_a_discard_permission_is_used_once() {
    for op in ["new", "open", "import.psd", "import.kra", "import.inp", "import.folder"] {
        assert!(replaces_document(&json!({"op": op})), "{op}");
    }
    for op in ["save", "merge.images", "merge.inp", "undo", "parameter.create", "export.inp"] {
        assert!(!replaces_document(&json!({"op": op})), "{op}");
    }
    let mut controller = EditorController::new();
    let made = controller.core_request(&json!({"op": "parameter.create", "name": "Edited", "dimensions": 1, "min": [0, 0], "max": [1, 1]}));
    assert_eq!(made["succeeded"], true);
    let refused = controller.core_request(&json!({"op": "new"}));
    assert_eq!(refused["message"], UNSAVED_REPLACEMENT_MESSAGE, "without the user's answer the refusal stands");
    // The permission is not spent by requests that do not replace the document.
    controller.allow_replace_unsaved(true);
    controller.core_request(&json!({"op": "bounds"}));
    let replaced = controller.core_request(&json!({"op": "new"}));
    assert_eq!(replaced["succeeded"], true, "{replaced}");
    assert_eq!(controller.snapshot()["model"]["param"], json!([]));
    // ... and a spent permission does not carry over to the next replacement.
    controller.core_request(&json!({"op": "parameter.create", "name": "Again", "dimensions": 1, "min": [0, 0], "max": [1, 1]}));
    assert_eq!(controller.core_request(&json!({"op": "new"}))["message"], UNSAVED_REPLACEMENT_MESSAGE);
    controller.allow_replace_unsaved(true);
    controller.allow_replace_unsaved(false);
    assert_eq!(controller.core_request(&json!({"op": "new"}))["succeeded"], false);
    // Remote callers never get the permission.
    let call = controller
        .mcp_request(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "new", "arguments": {}}}))
        .unwrap();
    assert_eq!(call["result"]["isError"], true);
}

#[test]
fn a_motion3_preview_of_an_inx_document_plays_with_the_controller_clock() {
    let dir = temp_dir("inx-motion-play");
    let motion = json!({"Version":3,"Meta":{"Duration":2,"Fps":30,"Loop":false,"AreBeziersRestricted":false,
        "CurveCount":1,"TotalSegmentCount":1,"TotalPointCount":2,"FadeInTime":-1,"UserDataCount":0,"TotalUserDataSize":0},
        "Curves":[{"Target":"Parameter","Id":"Linear","Segments":[0,0,0,2,1]}]});
    std::fs::write(dir.join("m.json"), motion.to_string()).unwrap();
    let mut controller = EditorController::new();
    let parameter = controller.core_request(&json!({"op": "parameter.create", "name": "P", "dimensions": 1, "min": [0, 0], "max": [1, 1]}));
    let id = parameter["result"]["uuid"].clone();
    controller.core_request(&json!({"op": "motion3.import", "name": "M", "path": dir.join("m.json").to_str().unwrap()}));
    controller.core_request(&json!({"op": "motion3.bind", "name": "M", "bindings": [{"id": "Linear", "parameter": id, "axis": 0}]}));
    assert_eq!(controller.snapshot()["documentFormat"], "inx");
    assert_eq!(controller.play_motion3("M")["succeeded"], true);
    assert!(controller.playing());
    controller.tick(0.5, false).expect("an INX Motion3 preview ticks");
    assert_eq!(controller.snapshot()["motion3"]["time"], 0.1, "a tick advances a clip by at most 0.1 s");
    assert!(controller.playing());
    let mut ticks = 1;
    while controller.playing() && ticks < 40 {
        controller.tick(5.0, false).unwrap();
        ticks += 1;
    }
    assert!(!controller.playing(), "playback stops at the clip end");
    assert!((20..=21).contains(&ticks), "{ticks} capped ticks cover a 2 s clip");
    assert_eq!(controller.snapshot()["motion3"]["time"], 2.0);
}

#[test]
fn a_failed_autosave_keeps_the_previous_backup_and_foreign_files_are_never_adopted() {
    use soul2d_core::autosave::{Autosave, Now, Recovery};
    let dir = std::env::temp_dir().join(format!("soul2d-review-fix-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let settings = soul2d_core::autosave::Settings::new(true, 5, 1);
    let mut autosave = Autosave::new(dir.join("config"), Now { mono_secs: 0, unix_secs: 1000 });
    autosave.set_project(Some(dir.join("proj.inx")), Now { mono_secs: 0, unix_secs: 1000 });
    let first = autosave
        .autosave_now(&settings, Now { mono_secs: 1, unix_secs: 1000 }, |path| std::fs::write(path, b"one").map_err(|e| e.to_string()))
        .unwrap();
    assert!(autosave.autosave_now(&settings, Now { mono_secs: 2, unix_secs: 2000 }, |_| Err("disk full".into())).is_err());
    assert_eq!(std::fs::read(&first).unwrap(), b"one");
    let second = autosave
        .autosave_now(&settings, Now { mono_secs: 3, unix_secs: 3000 }, |path| std::fs::write(path, b"two").map_err(|e| e.to_string()))
        .unwrap();
    assert!(!first.exists() && second.exists());

    let outside = dir.join("rust-recovery-notes.inx");
    std::fs::write(&outside, b"mine").unwrap();
    let mut recovery = Recovery::new(&dir.join("config"), Now { mono_secs: 0, unix_secs: 0 });
    recovery.adopt(outside.clone());
    assert!(recovery.current().is_none());
    recovery.release(Now { mono_secs: 1, unix_secs: 1 });
    assert!(outside.exists());
    let _ = std::fs::remove_dir_all(&dir);
}
