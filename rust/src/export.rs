//! Image and video export, ported from `io/imageexport.d`, `io/pngheader.d`, `io/videoexport.d`,
//! `windows/imgexport.d`, `windows/videoexport.d` and the `ExportPNG/TGA/JPEG/Video` commands.
//!
//! Everything goes through `Session::handle_request` (`render`, `scene.evaluate`, `animation.seek`,
//! `animation.reset`), so exporting never touches document history, `modified` or `revision`.
//! JPEG stills use the encoder below (the `image` build has PNG and TGA only). Not available here: BMP
//! output, the post-processing pass, and rotated or mirrored cameras (the `render` op takes
//! axis-aligned bounds).

use crate::Session;
use image::{DynamicImage, ImageFormat, RgbaImage};
use serde_json::{json, Value};
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Mirrors the private `MAX_RENDER_PIXELS` of the session's `render` op.
pub const RENDER_PIXEL_CAP: u64 = 64 * 1024 * 1024;
/// Upper bound on exported frames so a huge `loops` cannot exhaust memory or disk.
pub const MAX_VIDEO_FRAMES: usize = 100_000;
/// Upper bound of the D dialog's framerate drag control.
pub const MAX_FRAMERATE: u32 = 240;

// ---------------------------------------------------------------- PNG header

const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// Reads the pixel size from the first 24 bytes of a PNG without decoding it.
pub fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32), String> {
    let header = bytes.get(..24).ok_or("PNG source is too short to contain an IHDR header")?;
    if header[..8] != PNG_SIGNATURE {
        return Err("PNG source has an invalid signature".into());
    }
    let be = |at: usize| u32::from_be_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]]);
    if be(8) != 13 || &header[12..16] != b"IHDR" {
        return Err("PNG source does not begin with an IHDR chunk".into());
    }
    let (width, height) = (be(16), be(20));
    if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
        return Err("PNG source dimensions are invalid".into());
    }
    Ok((width, height))
}

/// File flavour of [`png_dimensions`]; reads at most 24 bytes.
pub fn png_file_dimensions(path: &Path) -> Result<(u32, u32), String> {
    use std::io::Read;
    let file = fs::File::open(path).map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let mut header = Vec::with_capacity(24);
    file.take(24).read_to_end(&mut header).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    png_dimensions(&header)
}

// ---------------------------------------------------------------- options

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StillFormat {
    Png,
    Tga,
    /// Baseline JFIF written by [`encode_jpeg`]; there is no alpha, so the render is always composited.
    Jpeg,
}

impl StillFormat {
    pub fn extension(self) -> &'static str {
        match self {
            StillFormat::Png => "png",
            StillFormat::Tga => "tga",
            StillFormat::Jpeg => "jpeg",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageExportOptions {
    /// Camera node name; `None` (or empty) selects the first camera, as in D.
    pub camera: Option<String>,
    pub format: StillFormat,
    /// Keep the alpha channel. Otherwise the render is composited over `background`.
    pub transparency: bool,
    /// D's post-processing pass; not available, so `true` is rejected.
    pub postprocessing: bool,
    /// Opaque background used when `transparency` is false. D uses the editor clear color
    /// (black in dark mode, white in light mode); black is the default here.
    pub background: [u8; 3],
    pub overwrite: bool,
}

impl Default for ImageExportOptions {
    fn default() -> Self {
        Self {
            camera: None,
            format: StillFormat::Png,
            transparency: false,
            postprocessing: false,
            background: [0, 0, 0],
            overwrite: false,
        }
    }
}

impl ImageExportOptions {
    pub fn validate(&self) -> Result<(), String> {
        no_postprocessing(self.postprocessing)
    }

    /// Reads the `export.image` request keys; absent or null keys keep the defaults.
    pub fn from_request(request: &Value) -> Result<Self, String> {
        let format = match request.get("format") {
            None | Some(Value::Null) => StillFormat::Png,
            Some(Value::String(name)) => match name.to_ascii_lowercase().as_str() {
                "png" => StillFormat::Png,
                "tga" => StillFormat::Tga,
                "jpeg" | "jpg" => StillFormat::Jpeg,
                _ => return Err("format must be png, tga or jpeg".into()),
            },
            Some(_) => return Err("format must be a string".into()),
        };
        Ok(Self {
            camera: text_key(request, "camera")?,
            format,
            transparency: flag_key(request, "transparency")?,
            postprocessing: flag_key(request, "postprocessing")?,
            background: background_key(request)?,
            overwrite: flag_key(request, "overwrite")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VideoExportOptions {
    /// Animation name; `None` (or empty) selects the first animation.
    pub animation: Option<String>,
    pub camera: Option<String>,
    /// Number of loop-point wraps to play; 0 is treated as 1 like D's `clamp(loops, 1, int.max)`.
    pub loops: u32,
    /// Output framerate; `None` uses the animation's own framerate.
    pub framerate: Option<u32>,
    pub transparency: bool,
    pub postprocessing: bool,
    /// D's "Lock to Animation Framerate": floor the sampled frame.
    pub snap_to_framerate: bool,
    pub background: [u8; 3],
    pub overwrite: bool,
}

impl Default for VideoExportOptions {
    fn default() -> Self {
        Self {
            animation: None,
            camera: None,
            loops: 1,
            framerate: None,
            transparency: false,
            postprocessing: false,
            snap_to_framerate: false,
            background: [0, 0, 0],
            overwrite: false,
        }
    }
}

impl VideoExportOptions {
    /// Reads the shared `export.frames` / `export.video.start` request keys; absent or null keys keep the defaults.
    pub fn from_request(request: &Value) -> Result<Self, String> {
        let count = |key: &str| -> Result<Option<u32>, String> {
            match request.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(value) => value
                    .as_u64()
                    .and_then(|count| u32::try_from(count).ok())
                    .map(Some)
                    .ok_or_else(|| format!("{key} must be a non-negative integer")),
            }
        };
        Ok(Self {
            animation: text_key(request, "animation")?,
            camera: text_key(request, "camera")?,
            loops: count("loops")?.unwrap_or(1),
            framerate: count("framerate")?,
            transparency: flag_key(request, "transparency")?,
            postprocessing: flag_key(request, "postprocessing")?,
            snap_to_framerate: flag_key(request, "snap_to_framerate")?,
            background: background_key(request)?,
            overwrite: flag_key(request, "overwrite")?,
        })
    }

    pub fn validate(&self) -> Result<(), String> {
        no_postprocessing(self.postprocessing)?;
        match self.framerate {
            Some(rate) if rate == 0 || rate > MAX_FRAMERATE => {
                Err(format!("framerate must be between 1 and {MAX_FRAMERATE}"))
            }
            _ => Ok(()),
        }
    }
}

fn flag_key(request: &Value, key: &str) -> Result<bool, String> {
    match request.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(value) => value.as_bool().ok_or_else(|| format!("{key} must be a boolean")),
    }
}

fn text_key(request: &Value, key: &str) -> Result<Option<String>, String> {
    match request.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

/// `background`: `[r, g, b]`, each 0 to 255; black when absent.
fn background_key(request: &Value) -> Result<[u8; 3], String> {
    let channels = match request.get("background") {
        None | Some(Value::Null) => return Ok([0, 0, 0]),
        Some(Value::Array(channels)) if channels.len() == 3 => channels,
        Some(_) => return Err("background must be [r, g, b]".into()),
    };
    let mut color = [0u8; 3];
    for (slot, channel) in color.iter_mut().zip(channels) {
        *slot = channel
            .as_u64()
            .and_then(|value| u8::try_from(value).ok())
            .ok_or("background channels must be integers from 0 to 255")?;
    }
    Ok(color)
}

fn no_postprocessing(requested: bool) -> Result<(), String> {
    if requested {
        return Err("post processing is not available in this build".into());
    }
    Ok(())
}

// ---------------------------------------------------------------- session access

fn call(session: &mut Session, request: Value) -> Result<Value, String> {
    let response = session.handle_request(&request);
    if response["succeeded"] == true {
        Ok(response["result"].clone())
    } else {
        Err(response["message"].as_str().unwrap_or("request failed").to_owned())
    }
}

fn collect_cameras<'a>(node: &'a Value, out: &mut Vec<&'a Value>) {
    if node["type"] == "Camera" {
        out.push(node);
    }
    for child in node["children"].as_array().into_iter().flatten() {
        collect_cameras(child, out);
    }
}

/// Port of `ExCamera.normalizeViewport`: absolute value, rounded to an even size of at least 2.
fn even_dimension(value: f64) -> u32 {
    let size = value.abs();
    if !size.is_finite() {
        return 2;
    }
    let rounded = size.round().min(1.0e9) as u32;
    if rounded < 2 {
        2
    } else {
        rounded + rounded % 2
    }
}

/// The area one camera node shows, as `render` arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraFrame {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub bounds: [f64; 4],
}

/// Resolves the camera (first one when `name` is `None` or empty) at the session's current pose.
pub fn camera_frame(session: &mut Session, name: Option<&str>) -> Result<CameraFrame, String> {
    let snapshot = session.snapshot();
    let mut cameras = Vec::new();
    collect_cameras(&snapshot["model"]["nodes"], &mut cameras);
    if cameras.is_empty() {
        return Err("No cameras available in the active puppet".into());
    }
    let wanted = name.filter(|name| !name.is_empty());
    let camera = match wanted {
        None => cameras[0],
        Some(wanted) => *cameras
            .iter()
            .find(|camera| camera["name"] == wanted)
            .ok_or_else(|| format!("Camera not found: {wanted}"))?,
    };
    let id = camera["uuid"].as_u64().ok_or("camera uuid is missing")?;
    let viewport = camera["viewport"].as_array().filter(|v| v.len() == 2);
    let (vx, vy) = match viewport {
        Some(v) => (v[0].as_f64().unwrap_or(f64::NAN), v[1].as_f64().unwrap_or(f64::NAN)),
        None => (1920.0, 1080.0),
    };
    let (width, height) = (even_dimension(vx), even_dimension(vy));
    if u64::from(width) * u64::from(height) > RENDER_PIXEL_CAP {
        return Err("export dimensions exceed the render pixel limit".into());
    }
    let scene = call(session, json!({"op": "scene.evaluate"}))?;
    let m = &scene["worldMatrices"][id.to_string()];
    let at = |row: usize, col: usize| m[row][col].as_f64().filter(|v| v.is_finite());
    let (m00, m01, m10, m11, tx, ty) =
        match (at(0, 0), at(0, 1), at(1, 0), at(1, 1), at(0, 3), at(1, 3)) {
            (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) => (a, b, c, d, e, f),
            _ => return Err(format!("camera {id} has no world transform")),
        };
    let (sx, sy) = (m00.hypot(m10), m01.hypot(m11));
    if sx == 0.0 || sy == 0.0 {
        return Err("camera scale must be nonzero".into());
    }
    if (m10 / sx).abs() > 1e-6 || m00 <= 0.0 || m11 <= 0.0 {
        return Err("rotated or mirrored cameras are not supported".into());
    }
    let (half_x, half_y) = (f64::from(width) * 0.5 * sx, f64::from(height) * 0.5 * sy);
    Ok(CameraFrame {
        name: camera["name"].as_str().unwrap_or_default().to_owned(),
        width,
        height,
        bounds: [tx - half_x, ty - half_y, tx + half_x, ty + half_y],
    })
}

/// Where a track or the model is placed (ANI04): moved by `x`, `y` in world units, scaled about the world origin by
/// `scale`, faded by `opacity`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub x: f64,
    pub y: f64,
    pub scale: f64,
    pub opacity: f64,
}

impl Placement {
    pub const IDENTITY: Placement = Placement { x: 0.0, y: 0.0, scale: 1.0, opacity: 1.0 };

    /// The placement a track names. Edits check the values; one a file brought in is brought into range here.
    fn of(track: &Value) -> Placement {
        let number = |key: &str, default: f64| track.get(key).and_then(Value::as_f64).unwrap_or(default);
        let scale = Some(number("scale", 1.0)).filter(|scale| *scale > 0.0).unwrap_or(1.0);
        Placement { x: number("x", 0.0), y: number("y", 0.0), scale, opacity: number("opacity", 1.0).clamp(0.0, 1.0) }
    }

    /// `inner` inside a group placed at `self`.
    fn then(self, inner: Placement) -> Placement {
        Placement {
            x: self.x + inner.x * self.scale,
            y: self.y + inner.y * self.scale,
            scale: self.scale * inner.scale,
            opacity: self.opacity * inner.opacity,
        }
    }
}

/// A decoded image track: centred at `placement.x`, `placement.y`, one image pixel `placement.scale` world units wide.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub image: RgbaImage,
    pub placement: Placement,
}

/// What a clip draws with the model (ANI04): image tracks behind it and in front of it, each list top track first,
/// and the model's own placement, `None` when its group is hidden.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    pub back: Vec<Layer>,
    pub front: Vec<Layer>,
    pub model: Option<Placement>,
}

impl Default for Scene {
    fn default() -> Self {
        Scene { back: Vec::new(), front: Vec::new(), model: Some(Placement::IDENTITY) }
    }
}

/// The resolved name of an image file `read_rgba` decodes (PNG, TGA, baseline JPEG within the texture caps), and the
/// image.
pub fn image_file(path: &str) -> Result<(String, RgbaImage), String> {
    let file = fs::canonicalize(path)
        .ok()
        .filter(|file| file.is_file())
        .ok_or_else(|| format!("image file {path} was not found"))?;
    let (_, image) = crate::model_import::read_rgba(&file)?;
    Ok((file.to_string_lossy().into_owned(), image))
}

/// Import a bounded set of numbered PNG/JPEG links. Reload keeps the original source-frame origin.
pub fn image_sequence(directory: &str, prefix: &str, first_number: Option<u64>) -> Result<Value, String> {
    local_path(directory).map_err(str::to_owned)?;
    if prefix.contains(['/', '\\']) {
        return Err("sequence prefix must be a filename prefix".into());
    }
    let directory = fs::canonicalize(directory).map_err(|error| format!("sequence directory: {error}"))?;
    if !directory.is_dir() { return Err("sequence directory is not a directory".into()); }
    let mut files = std::collections::BTreeMap::new();
    let mut pixels = 0u64;
    for (scanned, entry) in fs::read_dir(&directory).map_err(|error| error.to_string())?.enumerate() {
        if scanned >= 100_000 { return Err("sequence directory exceeds 100000 entries".into()); }
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let extension = path.extension().and_then(|name| name.to_str()).unwrap_or_default().to_ascii_lowercase();
        if !matches!(extension.as_str(), "png" | "jpg" | "jpeg") { continue; }
        let Some(suffix) = path.file_stem().and_then(|name| name.to_str()).and_then(|name| name.strip_prefix(prefix)) else { continue; };
        if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) { continue; }
        let number: u64 = suffix.parse().map_err(|_| "sequence suffix is too large")?;
        if entry.file_type().map_err(|error| error.to_string())?.is_symlink() {
            return Err("sequence symbolic links are not supported".into());
        }
        if number > (1u64 << 53) { return Err("sequence suffix exceeds exact frame precision".into()); }
        if files.len() == crate::animation::MAX_SEQUENCE_FILES { return Err("sequence exceeds 4096 files".into()); }
        if files.contains_key(&number) { return Err(format!("sequence has duplicate frame number {number}")); }
        let canonical = fs::canonicalize(&path).map_err(|error| error.to_string())?;
        if canonical.parent() != Some(directory.as_path()) { return Err("sequence file points outside its directory".into()); }
        let (path, image) = image_file(canonical.to_str().ok_or("sequence path is not UTF-8")?)?;
        pixels += u64::from(image.width()) * u64::from(image.height());
        if pixels > 1_000_000_000 { return Err("sequence exceeds one billion decoded pixels".into()); }
        files.insert(number, path);
    }
    let first = first_number.or_else(|| files.keys().next().copied()).ok_or("no numbered PNG/JPEG images match the prefix")?;
    if files.is_empty() { return Err("no numbered PNG/JPEG images match the prefix".into()); }
    Ok(json!({"directory":directory, "prefix":prefix, "firstNumber":first,
        "files":files.into_iter().map(|(number,path)| json!({"number":number,"path":path})).collect::<Vec<_>>()}))
}

/// The placement of a track or of the model in `clip`: its own (`own`) inside its group's, or `None` when it or its
/// group is hidden. A group a file names but does not hold places nothing.
fn placed(clip: &Value, own: Placement, hidden: bool, group: Option<&str>) -> Option<Placement> {
    if hidden {
        return None;
    }
    let groups = clip.get("groups").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let Some(group) = group.and_then(|group| groups.iter().find(|item| item["name"].as_str() == Some(group))) else {
        return Some(own);
    };
    (group.get("hidden") != Some(&json!(true))).then(|| Placement::of(group).then(own))
}

/// Whether a track of `clip` is in a hidden group, so it is not exported.
fn in_hidden_group(clip: &Value, track: &Value) -> bool {
    let group = track.get("group").and_then(Value::as_str);
    group.is_some() && placed(clip, Placement::IDENTITY, false, group).is_none()
}

/// The image tracks and the model placement of clip `name`, the images decoded. A track whose file is gone, not
/// absolute or not an image, or past the first [`crate::animation::MAX_IMAGE_TRACKS`], is left out rather than
/// stopping the export; the second list names those, with the error. Hidden tracks are not exported. Without `keep`
/// each image is only checked: decoded, then dropped, so a plan does not hold the pictures.
pub fn clip_scene(model: &Value, name: &str, keep: bool) -> (Scene, Vec<String>) {
    clip_scene_at(model, name, 0.0, keep, false)
}

/// Resolve image tracks for one clip frame. Guides are included only for editor preview.
pub fn clip_scene_at(model: &Value, name: &str, frame: f64, keep: bool, include_guides: bool) -> (Scene, Vec<String>) {
    let clip = &model["animations"][name];
    let flag = |track: &Value, key: &str| track.get(key) == Some(&json!(true));
    let group = |track: &Value| track.get("group").and_then(Value::as_str).map(str::to_owned);
    let model_group = clip.get("modelGroup").and_then(Value::as_str);
    let mut scene = Scene { model: placed(clip, Placement::IDENTITY, false, model_group), ..Scene::default() };
    let mut left_out = Vec::new();
    // Only the first tracks shown are read, so a document naming many files cannot make the export read them all.
    let (mut tried, mut past) = (0, 0);
    for track in clip.get("images").and_then(Value::as_array).into_iter().flatten() {
        if !include_guides && flag(track, "guide") {
            continue;
        }
        let Some(placement) = placed(clip, Placement::of(track), flag(track, "hidden"), group(track).as_deref()) else {
            continue;
        };
        if tried == crate::animation::MAX_IMAGE_TRACKS {
            past += 1;
            continue;
        }
        tried += 1;
        let selected;
        let path = if track.get("sequence").is_some() {
            match crate::animation::image_path_at(track, clip, frame) {
                Ok((Some(path), _)) => { selected = path; selected.as_str() }
                Ok((None, _)) => continue,
                Err(error) => { left_out.push(error); continue; }
            }
        } else {
            track.get("path").and_then(Value::as_str).unwrap_or_default()
        };
        if let Err(error) = local_path(path) {
            left_out.push(format!("image file {path} {error}"));
            continue;
        }
        match image_file(path) {
            Ok((_, image)) => {
                if keep {
                    let layer = Layer { image, placement };
                    if flag(track, "front") { scene.front.push(layer) } else { scene.back.push(layer) }
                }
            }
            Err(error) => left_out.push(error),
        }
    }
    if past > 0 {
        left_out.push(format!("{past} more image tracks are past the first {}", crate::animation::MAX_IMAGE_TRACKS));
    }
    (scene, left_out)
}

/// Draw sequence layers around the current pose without seeking or changing interactive model coordinates.
pub fn render_clip_frame(
    session: &mut Session, camera: &CameraFrame, name: &str, frame: f64, include_guides: bool,
) -> Result<(RgbaImage, Vec<String>), String> {
    let (mut scene, warnings) = clip_scene_at(&session.document.model, name, frame, true, include_guides);
    scene.model = Some(Placement::IDENTITY);
    let image = render_camera(session, camera, true, [0, 0, 0], &scene)?;
    Ok((image, warnings))
}

/// Includes unavailable linked sequence sources, even if the current remap does not select them.
pub(crate) fn image_source_warnings(model: &Value, name: &str) -> Vec<String> {
    let (_, mut warnings) = clip_scene(model, name, false);
    let clip = &model["animations"][name];
    let mut seen = std::collections::BTreeSet::new();
    let mut pixels = 0u64;
    let tracks = clip["images"].as_array().into_iter().flatten()
        .filter(|track| track["hidden"] != true && track["guide"] != true && !in_hidden_group(clip, track))
        .take(crate::animation::MAX_IMAGE_TRACKS);
    'tracks: for track in tracks {
        let Some(files) = track["sequence"]["files"].as_array() else { continue; };
        if files.len() > crate::animation::MAX_SEQUENCE_FILES {
            warnings.push("sequence source check exceeds 4096 files".into());
            continue;
        }
        for file in files {
            let path = file["path"].as_str().unwrap_or_default();
            if !seen.insert(path) { continue; }
            let decoded = local_path(path).map_err(|error| format!("image file {path} {error}"))
                .and_then(|()| image_file(path));
            match decoded {
                Ok((_, image)) => {
                    pixels += u64::from(image.width()) * u64::from(image.height());
                    if pixels > 1_000_000_000 {
                        warnings.push("remaining sequence sources were not checked: pixel budget exceeded".into());
                        break 'tracks;
                    }
                }
                Err(error) if !warnings.iter().any(|warning| warning.contains(&error)) => {
                    warnings.push(format!("linked sequence source (possibly unselected): {error}"));
                }
                Err(_) => {}
            }
        }
    }
    warnings
}

/// Refuses a name a document gives that is not a local absolute path: a relative one would resolve against this
/// process's folder, not the document's, and a network or device name (`\\host\share`) would open a connection.
pub(crate) fn local_path(path: &str) -> Result<(), &'static str> {
    let verbatim_disk = matches!(Path::new(path).components().next(),
        Some(std::path::Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::VerbatimDisk(_)));
    if (path.starts_with("\\\\") || path.starts_with("//")) && !verbatim_disk {
        return Err("is a network or device path");
    }
    if !Path::new(path).is_absolute() {
        return Err("is not an absolute path");
    }
    Ok(())
}

/// Draws `layer` over the premultiplied `canvas` of the camera `frame`.
// ponytail: nearest-neighbour sampling; bilinear when a scaled background looks blocky.
fn draw_layer(canvas: &mut [[f32; 4]], frame: &CameraFrame, layer: &Layer) {
    let (width, height) = (frame.width as usize, frame.height as usize);
    let [x0, y0, x1, y1] = frame.bounds;
    let Placement { x, y, scale, opacity } = layer.placement;
    let (columns, rows) = layer.image.dimensions();
    let (left, top) = (x - f64::from(columns) * scale / 2.0, y - f64::from(rows) * scale / 2.0);
    for py in 0..height {
        let row = ((y0 + (py as f64 + 0.5) * (y1 - y0) / height as f64 - top) / scale).floor();
        if !(0.0..f64::from(rows)).contains(&row) {
            continue;
        }
        for px in 0..width {
            let column = ((x0 + (px as f64 + 0.5) * (x1 - x0) / width as f64 - left) / scale).floor();
            if !(0.0..f64::from(columns)).contains(&column) {
                continue;
            }
            let pixel = layer.image.get_pixel(column as u32, row as u32);
            over(&mut canvas[py * width + px], pixel.0, opacity as f32);
        }
    }
}

/// Straight-alpha `source` faded by `opacity`, over the premultiplied `target`.
fn over(target: &mut [f32; 4], source: [u8; 4], opacity: f32) {
    let alpha = f32::from(source[3]) / 255.0 * opacity;
    for channel in 0..3 {
        target[channel] = f32::from(source[channel]) / 255.0 * alpha + target[channel] * (1.0 - alpha);
    }
    target[3] = alpha + target[3] * (1.0 - alpha);
}

/// Renders the camera and applies the transparency option.
fn render_camera(
    session: &mut Session,
    frame: &CameraFrame,
    transparency: bool,
    background: [u8; 3],
    scene: &Scene,
) -> Result<RgbaImage, String> {
    if *scene != Scene::default() {
        return render_scene(session, frame, transparency, background, scene);
    }
    let rendered = call(session, json!({
        "op": "render", "width": frame.width, "height": frame.height, "bounds": frame.bounds,
    }))?;
    let encoded = rendered["png"].as_str().ok_or("render returned no png")?;
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
        .map_err(|error| format!("render returned invalid png data: {error}"))?;
    let mut image = image::load_from_memory(&bytes)
        .map_err(|error| format!("render returned an unreadable png: {error}"))?
        .into_rgba8();
    if image.dimensions() != (frame.width, frame.height) {
        return Err("render returned an unexpected image size".into());
    }
    if !transparency {
        for pixel in image.pixels_mut() {
            let alpha = u32::from(pixel[3]);
            for channel in 0..3 {
                let mixed = u32::from(pixel[channel]) * alpha + u32::from(background[channel]) * (255 - alpha);
                pixel[channel] = ((mixed + 127) / 255) as u8;
            }
            pixel[3] = 255;
        }
    }
    Ok(image)
}

/// [`render_camera`] with image tracks behind and in front of the model and the model placed by its group: the
/// background (or nothing, under transparency), the back tracks from the bottom one up, the model, then the front
/// tracks.
fn render_scene(
    session: &mut Session,
    frame: &CameraFrame,
    transparency: bool,
    background: [u8; 3],
    scene: &Scene,
) -> Result<RgbaImage, String> {
    let fill = if transparency {
        [0.0; 4]
    } else {
        [f32::from(background[0]) / 255.0, f32::from(background[1]) / 255.0, f32::from(background[2]) / 255.0, 1.0]
    };
    let mut canvas = vec![fill; frame.width as usize * frame.height as usize];
    for layer in scene.back.iter().rev() {
        draw_layer(&mut canvas, frame, layer);
    }
    if let Some(placement) = scene.model {
        // A model moved and scaled by its group is the model rendered through the inverse placement.
        let [x0, y0, x1, y1] = frame.bounds;
        let Placement { x, y, scale, opacity } = placement;
        let bounds = [(x0 - x) / scale, (y0 - y) / scale, (x1 - x) / scale, (y1 - y) / scale];
        let model = render_camera(session, &CameraFrame { bounds, ..frame.clone() }, true, background, &Scene::default())?;
        for (target, pixel) in canvas.iter_mut().zip(model.pixels()) {
            over(target, pixel.0, opacity as f32);
        }
    }
    for layer in scene.front.iter().rev() {
        draw_layer(&mut canvas, frame, layer);
    }
    let byte = |value: f32| (value * 255.0).round().clamp(0.0, 255.0) as u8;
    let pixels = canvas.iter().flat_map(|[r, g, b, a]| {
        let straight = |channel: f32| if *a > 0.0 { byte(channel / a) } else { 0 };
        [straight(*r), straight(*g), straight(*b), if transparency { byte(*a) } else { 255 }]
    });
    Ok(RgbaImage::from_raw(frame.width, frame.height, pixels.collect()).expect("the canvas fills the frame"))
}

fn encode_still(image: &RgbaImage, format: StillFormat) -> Result<Vec<u8>, String> {
    let mut out = Cursor::new(Vec::new());
    match format {
        StillFormat::Png => DynamicImage::ImageRgba8(image.clone()).write_to(&mut out, ImageFormat::Png),
        StillFormat::Tga => image::codecs::tga::TgaEncoder::new(&mut out).disable_rle()
            .encode(image.as_raw(), image.width(), image.height(), image::ExtendedColorType::Rgba8),
        StillFormat::Jpeg => return encode_jpeg(image),
    }.map_err(|error| format!("failed to encode image: {error}"))?;
    Ok(out.into_inner())
}

/// Publishes `bytes` atomically: no reader sees a partial file, and without `overwrite` an existing
/// file is never replaced.
fn write_file(path: &Path, bytes: &[u8], overwrite: bool) -> Result<(), String> {
    crate::write_atomic(path, bytes, overwrite)
}

// ---------------------------------------------------------------- still export

#[derive(Clone, Debug, PartialEq)]
pub struct ImageExportResult {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
}

/// Renders one camera to `destination`; the extension is replaced by the format's, as D's commands do,
/// except that a typed `.jpg` stays for JPEG, since it names the same format.
pub fn export_image(
    session: &mut Session,
    options: &ImageExportOptions,
    destination: &Path,
) -> Result<ImageExportResult, String> {
    options.validate()?;
    if destination.as_os_str().is_empty() || destination.file_name().is_none() {
        return Err("File path is required".into());
    }
    let keep = options.format == StillFormat::Jpeg
        && destination.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("jpg"));
    let path = if keep { destination.to_path_buf() } else { destination.with_extension(options.format.extension()) };
    export_image_to_target(session, options, &path)
}

pub(crate) fn export_image_to_target(
    session: &mut Session,
    options: &ImageExportOptions,
    path: &Path,
) -> Result<ImageExportResult, String> {
    if !options.overwrite && path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    let frame = camera_frame(session, options.camera.as_deref())?;
    let transparency = options.transparency && options.format != StillFormat::Jpeg;
    let image = render_camera(session, &frame, transparency, options.background, &Scene::default())?;
    let bytes = encode_still(&image, options.format)?;
    write_file(path, &bytes, options.overwrite)?;
    Ok(ImageExportResult { path: path.to_owned(), width: frame.width, height: frame.height })
}

// ---------------------------------------------------------------- video frame plan

/// The animation fields the player uses.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct AnimationTiming {
    pub length: i32,
    pub timestep: f32,
    pub lead_in: i32,
    pub lead_out: i32,
}

/// Which animation frames to render, one entry per exported image.
#[derive(Clone, Debug, PartialEq)]
pub struct FramePlan {
    /// Framerate written to the encoder (`framerate` option, else the animation's own).
    pub fps: i32,
    /// Fractional animation frame to seek to per image; `None` means the player was idle (rest pose).
    pub poses: Vec<Option<f32>>,
}

/// State of D's `AnimationPlayback`, restricted to what the headless export drives.
struct Playback {
    t: AnimationTiming,
    time: f32,
    looped: i32,
    looping: bool,
    playing: bool,
    stopping: bool,
    play_lead_out: bool,
}

impl Playback {
    fn frame(&self) -> i32 {
        (self.time / self.t.timestep).round() as i32
    }
    fn has_lead_in(&self) -> bool {
        self.t.lead_in > 0 && self.t.lead_in + 1 < self.t.length
    }
    fn has_lead_out(&self) -> bool {
        self.t.lead_out > 0 && self.t.lead_out + 1 < self.t.length
    }
    fn loop_end(&self) -> i32 {
        if self.has_lead_out() { self.t.lead_out } else { self.t.length }
    }
    fn loop_begin(&self) -> i32 {
        if self.has_lead_in() { self.t.lead_in } else { 0 }
    }
    fn playing_lead_out(&self) -> bool {
        ((self.playing && !self.looping) || self.stopping) && self.play_lead_out && self.frame() < self.t.length
    }

    /// `AnimationPlayback.update`; returns the half-frame it rendered, or `None` when idle.
    fn update(&mut self, delta: f32) -> Option<f32> {
        if !(self.playing || self.playing_lead_out()) {
            return None;
        }
        self.time += delta;
        if !self.playing_lead_out() && self.looping && self.frame() >= self.loop_end() {
            self.time = self.loop_begin() as f32 * self.t.timestep;
            self.looped += 1;
        }
        if self.frame() + 1 >= self.t.length {
            self.time = (self.t.length - 1) as f32 * self.t.timestep;
        }
        let rendered = self.time / self.t.timestep;
        if !self.looping && self.playing_lead_out() && self.frame() + 1 >= self.t.length {
            self.playing = false;
            self.play_lead_out = false;
            self.stopping = false;
            self.time = 0.0;
            self.looped = 0;
        }
        Some(rendered)
    }

    /// `AnimationPlayback.stop(false)`.
    fn stop(&mut self) {
        if self.stopping {
            return;
        }
        let immediate = self.frame() == 0 || !self.has_lead_out();
        self.stopping = !immediate;
        self.looping = false;
        self.playing = false;
        self.play_lead_out = !immediate;
        if immediate {
            self.time = 0.0;
            self.looped = 0;
        }
    }
}

/// Frame count and per-image pose exactly as D's `ExportVideoCommand` produces them.
///
/// D's puppet applies the offsets the player pushed on the previous step, so image 0 shows the
/// pre-rendered frame 0 and image k shows the frame rendered by player update k.
pub fn plan_frames(timing: AnimationTiming, loops: u32, framerate: Option<u32>) -> Result<FramePlan, String> {
    if timing.length < 1 {
        return Err("animation length must be at least 1 frame".into());
    }
    if !(timing.timestep.is_finite() && timing.timestep > 0.0) {
        return Err("animation timestep must be positive".into());
    }
    let loops = loops.clamp(1, i32::MAX as u32) as i32;
    // Rounded: an f32 timestep of 1/60 has a reciprocal just below 60 and must not become 59.
    let anim_fps = (1.0 / f64::from(timing.timestep)).round() as i32;
    if anim_fps < 1 {
        return Err("animation frame rate is below 1 fps".into());
    }
    let mut frametime = timing.timestep;
    let mut factor = 1.0f32;
    let mut fps = anim_fps;
    if let Some(rate) = framerate.filter(|rate| *rate >= 1) {
        fps = rate as i32;
        if fps != anim_fps {
            factor = rate as f32 / anim_fps as f32;
            frametime = 1.0 / rate as f32;
        }
    }
    let probe = Playback {
        t: timing, time: 0.0, looped: 0, looping: true, playing: true, stopping: false, play_lead_out: true,
    };
    let scaled = |frames: f32| (frames * factor).ceil() as i64;
    let begin = scaled(probe.loop_begin() as f32);
    let end = scaled(timing.length as f32 - probe.loop_end() as f32);
    let in_loop = scaled((probe.loop_end() - probe.loop_begin()) as f32);
    let total = begin + in_loop * i64::from(loops) + end;
    if total < 1 || total > MAX_VIDEO_FRAMES as i64 {
        return Err(format!("video would need {total} frames; the limit is {MAX_VIDEO_FRAMES}"));
    }
    let mut playback = probe;
    let mut poses = Vec::with_capacity(total as usize);
    let mut pose = Some(0.0f32);
    for _ in 0..total {
        poses.push(pose);
        pose = playback.update(frametime);
        if playback.looped >= loops {
            playback.stop();
        }
    }
    Ok(FramePlan { fps, poses })
}

// ---------------------------------------------------------------- video export

#[derive(Clone, Debug, PartialEq)]
pub struct VideoPlan {
    pub animation: String,
    pub camera: CameraFrame,
    pub plan: FramePlan,
}

impl VideoPlan {
    pub fn frames(&self) -> usize {
        self.plan.poses.len()
    }
}

fn animation_timing(model: &Value, name: &str) -> Result<AnimationTiming, String> {
    let animation = &model["animations"][name];
    let int = |key: &str, default: i64| -> Result<i32, String> {
        match animation.get(key) {
            None | Some(Value::Null) => Ok(default as i32),
            Some(value) => value
                .as_i64()
                .filter(|v| i32::try_from(*v).is_ok())
                .map(|v| v as i32)
                .ok_or_else(|| format!("animation {key} must be an integer")),
        }
    };
    Ok(AnimationTiming {
        length: int("length", 0)?,
        timestep: animation["timestep"].as_f64().ok_or("animation timestep is missing")? as f32,
        lead_in: int("leadIn", -1)?,
        lead_out: int("leadOut", -1)?,
    })
}

fn animation_plan(snapshot: &Value, options: &VideoExportOptions) -> Result<(String, FramePlan), String> {
    options.validate()?;
    if !snapshot["motion3"].is_null() {
        return Err("stop the Motion3 preview before exporting a video".into());
    }
    let animations = snapshot["model"]["animations"].as_object().filter(|a| !a.is_empty())
        .ok_or("No animations are defined for this model")?;
    let animation = match options.animation.as_deref().filter(|name| !name.is_empty()) {
        Some(name) if animations.contains_key(name) => name.to_owned(),
        Some(name) => return Err(format!("Animation not found: {name}")),
        None => animations.keys().next().cloned().unwrap_or_default(),
    };
    let timing = animation_timing(&snapshot["model"], &animation)?;
    let plan = plan_frames(timing, options.loops, options.framerate)?;
    Ok((animation, plan))
}

/// Resolves animation, camera and frame plan without rendering anything.
pub fn plan_video(session: &mut Session, options: &VideoExportOptions) -> Result<VideoPlan, String> {
    let (animation, plan) = animation_plan(&session.snapshot(), options)?;
    let camera = camera_frame(session, options.camera.as_deref())?;
    Ok(VideoPlan { animation, camera, plan })
}

/// Renders every planned frame and hands it to `sink(index, image)`; `image.as_raw()` is the RGBA
/// byte stream D pipes into ffmpeg. The session's animation preview is restored afterwards, also on
/// failure; physics state is reset by seeking and cannot be restored.
pub fn render_video_frames(
    session: &mut Session,
    options: &VideoExportOptions,
    sink: &mut dyn FnMut(usize, &RgbaImage) -> Result<(), String>,
) -> Result<VideoPlan, String> {
    let plan = plan_video(session, options)?;
    render_video_frames_planned(session, options, &plan, sink)?;
    Ok(plan)
}

fn render_video_frames_planned(
    session: &mut Session, options: &VideoExportOptions, plan: &VideoPlan,
    sink: &mut dyn FnMut(usize, &RgbaImage) -> Result<(), String>,
) -> Result<(), String> {
    let before = session.snapshot()["animation"].clone();
    let animated_images = session.document.model["animations"][&plan.animation]["images"]
        .as_array().into_iter().flatten().any(|image| image.get("sequence").is_some());
    let static_scene = (!animated_images).then(|| clip_scene(&session.document.model, &plan.animation, true).0);
    let run: Result<(), String> = (|| {
        for (index, pose) in plan.plan.poses.iter().enumerate() {
            match pose {
                Some(frame) => {
                    let frame = if options.snap_to_framerate { frame.floor() } else { *frame };
                    call(session, json!({
                        "op": "animation.seek", "name": plan.animation, "frame": f64::from(frame),
                    }))?;
                }
                None => {
                    call(session, json!({"op": "animation.reset"}))?;
                }
            }
            let camera = camera_frame(session, options.camera.as_deref())?;
            let resolved;
            let scene = if let Some(scene) = &static_scene { scene } else {
                let frame = session.snapshot()["animation"]["frame"].as_f64().unwrap_or(0.0);
                resolved = clip_scene_at(&session.document.model, &plan.animation, frame, true, false).0;
                &resolved
            };
            let image = render_camera(session, &camera, options.transparency, options.background, scene)?;
            sink(index, &image)?;
        }
        Ok(())
    })();
    let restored = match (before["name"].as_str(), before["frame"].as_f64()) {
        (Some(name), Some(frame)) => call(session, json!({"op": "animation.seek", "name": name, "frame": frame})),
        _ => call(session, json!({"op": "animation.reset"})),
    };
    run?;
    restored?;
    Ok(())
}

/// `stem-N.ext` for N starting at 1, the names ffmpeg's `%d` image2 pattern produces.
pub fn frame_path(destination: &Path, number: usize) -> PathBuf {
    let stem = destination.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let name = match destination.extension() {
        Some(ext) => format!("{stem}-{number}.{}", ext.to_string_lossy()),
        None => format!("{stem}-{number}"),
    };
    destination.with_file_name(name)
}

/// What `export.gif` adds to the video options.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct GifOptions {
    /// Colours in each frame's palette, 2 to 256; one slot goes to transparency when it is kept.
    pub colors: usize,
    /// `None` loops forever, `Some(n)` plays the clip `n + 1` times.
    pub repeat: Option<u16>,
}

impl Default for GifOptions {
    fn default() -> Self {
        Self { colors: 256, repeat: None }
    }
}

impl GifOptions {
    pub fn from_request(request: &Value) -> Result<Self, String> {
        let colors = match request.get("colors") {
            None | Some(Value::Null) => 256,
            Some(value) => value
                .as_u64()
                .filter(|colors| (2..=256).contains(colors))
                .ok_or("colors must be an integer from 2 to 256")? as usize,
        };
        let repeat = match request.get("repeat") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .and_then(|repeat| u16::try_from(repeat).ok())
                    .ok_or("repeat must be an integer from 0 to 65535")?,
            ),
        };
        Ok(Self { colors, repeat })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GifExportResult {
    pub path: PathBuf,
    pub frames: usize,
    pub width: u32,
    pub height: u32,
    /// Hundredths of a second each frame is shown: whole hundredths, at least 2 ([`GIF_MAX_FPS`]).
    pub delay: u16,
}

/// Browsers show a GIF frame of 0 or 1 hundredth of a second for a tenth, so a frame lasts at least 2.
pub const GIF_MAX_FPS: u32 = 50;

/// `plan_video` for a GIF: a clip faster than [`GIF_MAX_FPS`] is rendered at that rate, so it keeps its length.
// ponytail: an animation over 50 fps with no `framerate` is planned once at its own rate first, so the frame limit
// is checked there; pass `framerate` to plan a very long fast clip at 50 fps directly.
pub fn plan_gif(
    session: &mut Session,
    options: &VideoExportOptions,
) -> Result<(VideoExportOptions, VideoPlan), String> {
    let mut options = options.clone();
    options.framerate = options.framerate.map(|rate| rate.min(GIF_MAX_FPS));
    let plan = plan_video(session, &options)?;
    if plan.plan.fps <= GIF_MAX_FPS as i32 {
        return Ok((options, plan));
    }
    let options = VideoExportOptions { framerate: Some(GIF_MAX_FPS), ..options };
    let plan = plan_video(session, &options)?;
    Ok((options, plan))
}

/// Writes the animation as one animated GIF at `destination` (its extension is already `.gif`).
pub fn export_gif(
    session: &mut Session,
    options: &VideoExportOptions,
    gif: &GifOptions,
    destination: &Path,
) -> Result<GifExportResult, String> {
    options.validate()?;
    if !options.overwrite && destination.exists() {
        return Err(format!("{} already exists", destination.display()));
    }
    let (options, plan) = plan_gif(session, options)?;
    let options = &options;
    let delay = (100.0 / f64::from(plan.plan.fps)).round() as u16;
    let (width, height) = (plan.camera.width, plan.camera.height);
    let mut writer =
        crate::gif::GifWriter::new(width, height, delay, gif.colors, options.transparency, gif.repeat)?;
    render_video_frames(session, options, &mut |_, image| writer.add_frame(image))?;
    write_file(destination, &writer.finish(), options.overwrite)?;
    Ok(GifExportResult {
        path: destination.to_path_buf(),
        frames: plan.frames(),
        width: plan.camera.width,
        height: plan.camera.height,
        delay,
    })
}

/// Lists the exact numbered entries a frame export replaces, without rendering or writing.
pub fn frame_sequence_paths(snapshot: &Value, options: &VideoExportOptions, destination: &Path) -> Result<Vec<PathBuf>, String> {
    match destination.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("png" | "tga") => {},
        _ => return Err("frame sequence destination must end in .png or .tga".into()),
    }
    let (_, plan) = animation_plan(snapshot, options)?;
    Ok((1..=plan.poses.len()).map(|n| frame_path(destination, n)).collect())
}

/// Writes the animation as numbered stills (`.png` or `.tga` destination).
pub fn export_frame_sequence(
    session: &mut Session,
    options: &VideoExportOptions,
    destination: &Path,
) -> Result<Vec<PathBuf>, String> {
    let format = match destination.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("png") => StillFormat::Png,
        Some("tga") => StillFormat::Tga,
        _ => return Err("frame sequence destination must end in .png or .tga".into()),
    };
    options.validate()?;
    plan_video(session, options)?;
    let paths = frame_sequence_paths(&session.snapshot(), options, destination)?;
    if !options.overwrite {
        if let Some(existing) = paths.iter().find(|path| path.exists()) {
            return Err(format!("{} already exists", existing.display()));
        }
    }
    let mut written: Vec<PathBuf> = Vec::new();
    let outcome = render_video_frames(session, options, &mut |index, image| {
        // A replaced file cannot be restored, so only frames this run created are removed on failure.
        let created = !paths[index].exists();
        write_file(&paths[index], &encode_still(image, format)?, options.overwrite)?;
        if created {
            written.push(paths[index].clone());
        }
        Ok(())
    });
    match outcome {
        Ok(_) => Ok(paths),
        Err(error) => {
            for path in &written {
                let _ = fs::remove_file(path);
            }
            Err(error)
        }
    }
}

// ---------------------------------------------------------------- ffmpeg command line

#[derive(Clone, Debug, PartialEq)]
pub struct EncoderSettings {
    /// ffmpeg encoder tag, or `"auto"`.
    pub codec: String,
    pub framerate: u32,
    pub file: String,
    pub frames: u32,
    pub width: u32,
    pub height: u32,
    pub transparency: bool,
    /// Extra user options, split on whitespace and inserted before the output file.
    pub ffmpeg_options: String,
    /// Sounds muxed in, mixed at full volume, padded with silence or cut to the video's length.
    pub audio: Vec<AudioInput>,
}

/// A local sound file for a video, starting `delay_ms` after the first frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioInput {
    pub path: String,
    pub delay_ms: u64,
}

/// Image sequences have no audio track.
pub fn is_image_codec(codec: &str) -> bool {
    matches!(codec, "png" | "jpeg" | "jpg" | "tga")
}

/// The resolved name of a WAV file (RIFF/WAVE; an RF64 file is refused, as Cubism does). The header is read again
/// at every export, since a document can name any file.
pub fn wav_file(path: &str) -> Result<String, String> {
    let file = fs::canonicalize(path)
        .ok()
        .filter(|file| file.is_file())
        .ok_or_else(|| format!("audio file {path} was not found"))?;
    // ffmpeg reads a name with `%` as a numbered sequence of files.
    if file.to_string_lossy().contains('%') {
        return Err(format!("audio file {path} must not contain '%'"));
    }
    let mut header = [0u8; 12];
    fs::File::open(&file)
        .and_then(|mut opened| opened.read_exact(&mut header))
        .map_err(|_| format!("audio file {path} is not a WAV file"))?;
    if &header[..4] == b"RF64" {
        return Err(format!("audio file {path} is an RF64 WAV, which is not supported"));
    }
    if &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
        return Err(format!("audio file {path} is not a WAV file"));
    }
    Ok(file.to_string_lossy().into_owned())
}

/// Most audio tracks a video mixes; a clip may hold more, and the rest are left out.
pub const MAX_AUDIO_TRACKS: usize = 32;

/// The audio of clip `name` for a video planned as `plan` and written by `codec` to `file`: the tracks that are not
/// muted (`audio: [{path, frame, mute?}]`), each starting where its frame first shows in the video, so a lead-in,
/// loops, a lead-out and a new framerate keep it in step and a track plays once. A format without sound (an image
/// sequence, GIF, APNG, WebP, Y4M) takes none. A track whose file is gone, not absolute or not a WAV is left out
/// rather than stopping the export; the second list names those, with the error. Only the header is read, so a file
/// ffmpeg cannot decode still fails the encode.
pub fn clip_audio(
    model: &Value,
    name: &str,
    plan: &FramePlan,
    codec: &str,
    file: &str,
) -> (Vec<AudioInput>, Vec<String>) {
    const SILENT: [&str; 11] =
        [".gif", ".apng", ".png", ".webp", ".jpg", ".jpeg", ".tga", ".bmp", ".tif", ".tiff", ".y4m"];
    let silent = matches!(codec, "gif" | "apng")
        || codec.starts_with("libwebp")
        || SILENT.iter().any(|extension| file.to_ascii_lowercase().ends_with(extension));
    if is_image_codec(codec) || silent {
        return (Vec::new(), Vec::new());
    }
    let (mut inputs, mut left_out) = (Vec::new(), Vec::new());
    let clip = &model["animations"][name];
    for track in clip.get("audio").and_then(Value::as_array).into_iter().flatten() {
        // A muted track and one in a hidden group (ANI04) are not exported.
        if track.get("mute") == Some(&Value::Bool(true)) || in_hidden_group(clip, track) {
            continue;
        }
        let path = track["path"].as_str().unwrap_or_default();
        if inputs.len() == MAX_AUDIO_TRACKS {
            left_out.push(format!("audio file {path} is past the first {MAX_AUDIO_TRACKS} tracks"));
            continue;
        }
        if let Err(error) = local_path(path) {
            left_out.push(format!("audio file {path} {error}"));
            continue;
        }
        let shown = track["frame"].as_f64().and_then(|frame| first_shown(plan, frame));
        let input = match (wav_file(path), shown) {
            (Err(error), _) => Err(error),
            (Ok(_), None) => Err(format!("audio file {path} never shows in the video")),
            (Ok(path), Some(seconds)) => Ok(AudioInput { path, delay_ms: (seconds * 1000.0).round() as u64 }),
        };
        match input {
            Ok(input) => inputs.push(input),
            Err(error) => left_out.push(error),
        }
    }
    (inputs, left_out)
}

/// [`clip_audio`] for the document open in `session`, as `export.video.start` would use it.
pub fn session_audio(session: &Session, plan: &VideoPlan, codec: &str, file: &str) -> (Vec<AudioInput>, Vec<String>) {
    clip_audio(&session.document.model, &plan.animation, &plan.plan, codec, file)
}

/// Seconds into the video where clip frame `frame` first shows, between two images when it falls between them.
fn first_shown(plan: &FramePlan, frame: f64) -> Option<f64> {
    let mut previous: Option<(usize, f64)> = None;
    for (index, pose) in plan.poses.iter().enumerate() {
        // A plan has no pose only after playback stops.
        let Some(pose) = pose.map(f64::from) else {
            break;
        };
        if pose >= frame {
            let at = match previous {
                Some((before, shown)) if shown < frame => before as f64 + (frame - shown) / (pose - shown),
                _ => index as f64,
            };
            return Some(at / f64::from(plan.fps));
        }
        previous = Some((index, pose));
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncoderCommand {
    pub program: String,
    pub args: Vec<String>,
}

/// The external command and captured rendering source reviewed before a video job starts.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoConsent {
    pub command: EncoderCommand,
    pub directory: PathBuf,
    pub plan: VideoPlan,
    pub audio_left_out: Vec<String>,
    pub overwrite: bool,
    pub(crate) source: crate::ExportCopy,
    pub(crate) source_path: Option<PathBuf>,
    pub(crate) host_program: String,
    pub(crate) host_options: String,
}

/// One external file copied into a collected project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionFile {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub(crate) source_alias: PathBuf,
    pub(crate) destination_alias: PathBuf,
}

/// The final files and document bytes reviewed before collecting a project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionPlan {
    pub directory: PathBuf,
    pub copies: Vec<CollectionFile>,
    pub document: PathBuf,
    pub overwrite: bool,
    pub(crate) directory_alias: PathBuf,
    pub(crate) document_alias: PathBuf,
    pub(crate) bytes: Arc<[u8]>,
}

/// A glTF file and conversion report captured before host approval.
#[derive(Clone, Debug, PartialEq)]
pub struct GltfPlan {
    pub target: PathBuf,
    pub overwrite: bool,
    pub report: Value,
    pub(crate) target_alias: PathBuf,
    pub(crate) bytes: Arc<[u8]>,
    pub(crate) units: f64,
}

impl GltfPlan {
    pub fn byte_len(&self) -> usize { self.bytes.len() }
}

/// Captured DepthDraw PNGs and their manifest, in publication order.
#[derive(Clone, Debug, PartialEq)]
pub struct DepthPngPlan {
    pub directory: PathBuf,
    pub manifest: PathBuf,
    pub layers: Vec<PathBuf>,
    pub overwrite: bool,
    pub(crate) directory_alias: PathBuf,
    pub(crate) manifest_alias: PathBuf,
    pub(crate) depth_revision: u64,
    pub(crate) sources: Vec<(String, PathBuf, bool)>,
    pub(crate) outputs: Vec<(PathBuf, Arc<[u8]>)>,
    pub(crate) response: Value,
}

pub enum ExportConsent<'a> {
    Video(&'a VideoConsent),
    Collection(&'a CollectionPlan),
    GltfReport,
    Gltf(&'a GltfPlan),
    DepthPng(&'a DepthPngPlan),
    Store(&'a crate::StoreWritePlan),
}

/// `export.video.start` presets: an encoder and its options for a common container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoPreset {
    /// H.264, no alpha.
    Mp4,
    /// VP9, with alpha when the export keeps transparency.
    Webm,
    /// ProRes 4444 with alpha, ProRes 422 HQ without.
    Mov,
}

impl VideoPreset {
    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "mp4" => Ok(Self::Mp4),
            "webm" => Ok(Self::Webm),
            "mov" => Ok(Self::Mov),
            _ => Err("preset must be mp4, webm or mov".into()),
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Webm => "webm",
            Self::Mov => "mov",
        }
    }

    /// The encoder tag and the options that follow `ffmpeg_command`'s own; a later `-pix_fmt` wins. H.264 and
    /// ProRes need an even width and height, so an odd one gets a row or column of transparent black.
    pub fn encoder(self, transparency: bool) -> (&'static str, &'static str) {
        match (self, transparency) {
            (Self::Mp4, _) => ("libx264", "-crf 18 -movflags +faststart -vf pad=ceil(iw/2)*2:ceil(ih/2)*2"),
            (Self::Webm, _) => ("libvpx-vp9", "-b:v 0 -crf 30"),
            (Self::Mov, true) => {
                ("prores_ks", "-profile:v 4444 -pix_fmt yuva444p10le -vf pad=ceil(iw/2)*2:ceil(ih/2)*2:color=black@0")
            }
            (Self::Mov, false) => ("prores_ks", "-profile:v 3 -pix_fmt yuv422p10le -vf pad=ceil(iw/2)*2:ceil(ih/2)*2"),
        }
    }
}

/// Port of `incBuildFFmpegCommand`: raw RGBA frames arrive on stdin. Never executed here.
pub fn ffmpeg_command(settings: &EncoderSettings) -> Result<EncoderCommand, String> {
    let codec = settings.codec.as_str();
    if !codec.starts_with(|c: char| c.is_ascii_alphanumeric())
        || !codec.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("codec must be \"auto\" or an ffmpeg encoder tag".into());
    }
    if settings.file.is_empty() || settings.file.starts_with('-') {
        return Err("output file must be nonempty and must not start with '-'".into());
    }
    if settings.framerate == 0 || settings.frames == 0 || settings.width == 0 || settings.height == 0 {
        return Err("framerate, frames, width and height must be positive".into());
    }
    for audio in &settings.audio {
        if audio.path.is_empty() || audio.path.starts_with('-') {
            return Err("audio file must be nonempty and must not start with '-'".into());
        }
        if is_image_codec(codec) {
            return Err("an image sequence has no audio track".into());
        }
    }
    let file = if is_image_codec(codec) {
        let name_start = settings.file.rfind(['/', '\\']).map_or(0, |at| at + 1);
        match settings.file[name_start..].rfind('.').filter(|dot| *dot > 0) {
            Some(dot) => {
                let split = name_start + dot;
                format!("{}-%d{}", &settings.file[..split], &settings.file[split..])
            }
            None => format!("{}-%d", settings.file),
        }
    } else {
        settings.file.clone()
    };
    let text = |s: &str| s.to_owned();
    let mut args = vec![
        text("-y"),
        // Without the banner and info lines the stderr tail returned to callers is the real error only.
        text("-hide_banner"), text("-loglevel"), text("error"),
        text("-f"), text("rawvideo"),
        text("-vcodec"), text("rawvideo"),
        text("-pix_fmt"), text("rgba"),
        text("-s"), format!("{}x{}", settings.width, settings.height),
        text("-r"), settings.framerate.to_string(),
        text("-i"), text("-"),
    ];
    if !settings.audio.is_empty() {
        // The first audio stream of each local file, in the container's own audio codec. `apad` fills it with
        // silence and `-t` ends it with the video; `-t` and not `-shortest`, so an `-af` among the host's options
        // (which replaces the one here, delay included) shortens only the sound, never the video. Two or more
        // files are mixed in a filter graph instead, which a host `-af` cannot be combined with. Only computed
        // numbers go in a filter.
        let seconds = f64::from(settings.frames) / f64::from(settings.framerate);
        for audio in &settings.audio {
            args.extend(["-protocol_whitelist", "file", "-i", &audio.path].map(str::to_owned));
        }
        args.extend([text("-map"), text("0:v")]);
        let delay = |audio: &AudioInput| match audio.delay_ms {
            0 => String::new(),
            ms => format!("adelay={ms}:all=1,"),
        };
        match settings.audio.as_slice() {
            [one] => args.extend([text("-map"), text("1:a:0"), text("-af"), format!("{}apad", delay(one))]),
            many => {
                let mut graph = String::new();
                for (index, audio) in many.iter().enumerate() {
                    graph += &format!("[{}:a:0]{}anull[a{index}];", index + 1, delay(audio));
                }
                for index in 0..many.len() {
                    graph += &format!("[a{index}]");
                }
                // `-t` does not end a padded graph output, so it is padded to and cut at the video's length here.
                // amix can hand on frames without timestamps when the video ends first, which left atrim waiting
                // forever in 6 of 10 runs; asetpts numbers them from the sample count again.
                let mix = format!("amix=inputs={}:normalize=0,asetpts=N/SR/TB", many.len());
                graph += &format!("{mix},apad=whole_dur={seconds},atrim=end={seconds}[audio]");
                args.extend([text("-filter_complex"), graph, text("-map"), text("[audio]")]);
            }
        }
        args.extend([text("-t"), seconds.to_string()]);
    }
    args.extend([text("-vframes"), settings.frames.to_string()]);
    if settings.audio.is_empty() {
        args.push(text("-an"));
    }
    args.extend([text("-pix_fmt"), text(if settings.transparency { "yuva420p" } else { "yuv420p" })]);
    if codec != "auto" {
        args.push(text("-vcodec"));
        args.push(text(match codec {
            "jpeg" | "jpg" => "mjpeg",
            "tga" => "targa",
            other => other,
        }));
    }
    args.extend(settings.ffmpeg_options.split_whitespace().map(str::to_owned));
    args.push(file);
    Ok(EncoderCommand { program: "ffmpeg".into(), args })
}

#[cfg(any(windows, test))]
fn windows_encoder_candidates(program: &str, directory: &Path, search: Vec<PathBuf>) -> Vec<PathBuf> {
    let path = Path::new(program);
    if program.contains(['/', '\\']) || path.is_absolute() {
        let path = if path.is_absolute() { path.to_owned() } else { directory.join(path) };
        if program.to_ascii_lowercase().ends_with(".exe") { return vec![path] }
        let mut suffixed = path.as_os_str().to_owned();suffixed.push(".exe");
        vec![PathBuf::from(suffixed), path]
    } else {
        let name = if program.contains('.') { program.to_owned() } else { format!("{program}.exe") };
        search.into_iter().filter(|parent| !parent.as_os_str().is_empty()).map(|parent| {
            let parent = if parent.is_absolute() { parent } else { directory.join(parent) };
            parent.join(&name)
        }).collect()
    }
}

#[cfg(windows)]
fn windows_encoder_search_paths() -> Vec<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
        fn GetWindowsDirectoryW(buffer: *mut u16, size: u32) -> u32;
    }
    let mut paths = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        if let Some(parent) = executable.parent() { paths.push(parent.to_owned()); }
    }
    for read in [GetSystemDirectoryW, GetWindowsDirectoryW] {
        let mut buffer = vec![0u16; 32768];
        let length = unsafe { read(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        if length > 0 && length < buffer.len() {
            paths.push(PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length])));
        }
    }
    paths.extend(std::env::var_os("PATH").map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>()).unwrap_or_default());
    paths
}

pub(crate) fn resolve_encoder_program(program: &str, directory: &Path) -> Result<String, String> {
    #[cfg(windows)]
    let candidates = windows_encoder_candidates(program, directory, windows_encoder_search_paths());
    #[cfg(not(windows))]
    let candidates = {
        let path = Path::new(program);
        if path.is_absolute() || path.components().count() > 1 {
            vec![if path.is_absolute() { path.to_owned() } else { directory.join(path) }]
        } else {
            std::env::var_os("PATH").map(|paths| std::env::split_paths(&paths).map(|parent| {
                let parent = if parent.is_absolute() { parent } else { directory.join(parent) };
                parent.join(path)
            }).collect()).unwrap_or_default()
        }
    };
    for name in candidates {
        if !name.is_file() { continue }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if fs::metadata(&name).map_or(true, |metadata| metadata.permissions().mode() & 0o111 == 0) { continue }
        }
        if let Ok(path) = name.canonicalize() { return Ok(path.to_string_lossy().into_owned()) }
    }
    Err(format!("cannot resolve encoder executable: {program}"))
}

pub(crate) fn check_video_primary_output(command: &EncoderCommand, frames: usize, source: Option<&Path>) -> Result<(), String> {
    let output = command.args.last().ok_or("video output is missing")?;
    let count = if output.contains("%d") { frames } else { 1 };
    for number in 1..=count {
        let path = PathBuf::from(output.replacen("%d", &number.to_string(), 1));
        if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(format!("video output must not be a symbolic link: {}", path.display()));
        }
        if let Some(source) = source {
            if crate::normalize_target_path(&path)? == crate::normalize_target_path(source)? {
                return Err("export.video.start would overwrite the open document".into());
            }
        }
    }
    Ok(())
}

pub(crate) fn encoder_launch_command(program: &str, settings: &EncoderSettings, overwrite: bool) -> Result<EncoderCommand, String> {
    let mut command = ffmpeg_command(settings)?;
    command.program = program.to_owned();
    if !overwrite { command.args[0] = "-n".into(); }
    Ok(command)
}

/// The probe D runs once at startup (`ffmpeg -codecs`) to learn the available encoders.
pub fn ffmpeg_codecs_command() -> EncoderCommand {
    EncoderCommand { program: "ffmpeg".into(), args: vec!["-codecs".into()] }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VideoCodec {
    pub tag: String,
    pub name: String,
}

/// Port of `parseEncoders`: video encoders from `ffmpeg -codecs` output, sorted by name, with the
/// `auto` entry first. `Err` when the list marker is missing (D then assumes ffmpeg is absent).
pub fn parse_encoders(output: &str) -> Result<Vec<VideoCodec>, String> {
    const LIST_START: &str = " -------";
    let start = output.find(LIST_START).ok_or("ffmpeg codec list was not recognised")?;
    let mut codecs = Vec::new();
    for line in output[start + LIST_START.len()..].trim().lines() {
        let line = line.trim();
        let flags = line.as_bytes();
        if flags.len() < 3 || flags[1] != b'E' || flags[2] != b'V' {
            continue;
        }
        let Some(rest) = line.get(7..) else { continue };
        let Some(split) = rest.find(char::is_whitespace) else { continue };
        codecs.push(VideoCodec { tag: rest[..split].to_owned(), name: rest[split..].trim().to_owned() });
    }
    codecs.sort_by(|a, b| a.name.cmp(&b.name));
    codecs.insert(0, VideoCodec { tag: "auto".into(), name: "Automatic".into() });
    Ok(codecs)
}

// ---------------------------------------------------------------- JPEG

/// Quality of [`encode_jpeg`]. D's `imagefmt` has its JPEG writer disabled, so there is nothing to match.
pub const JPEG_QUALITY: u32 = 90;

type HuffmanSpec = ([u8; 16], &'static [u8]);

const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14,
    21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53,
    60, 61, 54, 47, 55, 62, 63,
];

// ITU-T T.81 Annex K.1, natural (row-major) order.
const LUMA_QUANT: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56, 14, 17, 22, 29,
    51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113, 92, 49, 64, 78, 87, 103, 121,
    120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];
const CHROMA_QUANT: [u16; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99, 47, 66, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
];

// ITU-T T.81 Annex K.3 (typical Huffman tables): code length counts, then symbols in code order.
const DC_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const DC_LUMA: HuffmanSpec = ([0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0], &DC_VALUES);
const DC_CHROMA: HuffmanSpec = ([0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0], &DC_VALUES);
const AC_LUMA: HuffmanSpec = ([0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d], &[
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71,
    0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72,
    0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37,
    0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59,
    0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83,
    0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3,
    0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
    0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
    0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
]);
const AC_CHROMA: HuffmanSpec = ([0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77], &[
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71, 0x13, 0x22,
    0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1,
    0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36,
    0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58,
    0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a,
    0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a,
    0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba,
    0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda,
    0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
]);

/// Canonical Huffman encoder table: `(code, length)` per symbol.
fn huffman_codes(spec: &HuffmanSpec) -> [(u16, u8); 256] {
    let mut codes = [(0u16, 0u8); 256];
    let (mut code, mut next) = (0u16, 0usize);
    for length in 1..=16u8 {
        for _ in 0..spec.0[usize::from(length) - 1] {
            codes[usize::from(spec.1[next])] = (code, length);
            code += 1;
            next += 1;
        }
        code <<= 1;
    }
    codes
}

/// libjpeg's quality scaling of an Annex K table.
fn scale_quant(base: &[u16; 64], quality: u32) -> [u16; 64] {
    let scale = if quality < 50 { 5000 / quality } else { 200 - 2 * quality };
    base.map(|value| ((u32::from(value) * scale + 50) / 100).clamp(1, 255) as u16)
}

struct BitWriter {
    out: Vec<u8>,
    accumulator: u32,
    pending: u32,
}

impl BitWriter {
    fn put(&mut self, value: u32, length: u32) {
        self.accumulator = (self.accumulator << length) | (value & ((1 << length) - 1));
        self.pending += length;
        while self.pending >= 8 {
            let byte = (self.accumulator >> (self.pending - 8)) as u8;
            self.out.push(byte);
            if byte == 0xff {
                self.out.push(0);
            }
            self.pending -= 8;
        }
        self.accumulator &= (1 << self.pending) - 1;
    }

    fn put_symbol(&mut self, codes: &[(u16, u8); 256], symbol: usize) {
        let (code, length) = codes[symbol];
        self.put(u32::from(code), u32::from(length));
    }

    fn finish(mut self) -> Vec<u8> {
        if self.pending > 0 {
            let padding = 8 - self.pending;
            self.put((1 << padding) - 1, padding);
        }
        self.out
    }
}

fn magnitude_bits(value: i32) -> (usize, u32) {
    let category = (32 - value.unsigned_abs().leading_zeros()) as usize;
    let bits = if value < 0 { (value - 1) as u32 } else { value as u32 };
    (category, bits)
}

fn jpeg_segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) {
    out.extend_from_slice(&[0xff, marker]);
    out.extend_from_slice(&(payload.len() as u16 + 2).to_be_bytes());
    out.extend_from_slice(payload);
}

/// Orthonormal 8x8 forward DCT.
fn forward_dct(block: &[f32; 64], basis: &[[f32; 8]; 8]) -> [f32; 64] {
    let mut rows = [0f32; 64];
    for y in 0..8 {
        for u in 0..8 {
            rows[y * 8 + u] = (0..8).map(|x| block[y * 8 + x] * basis[u][x]).sum();
        }
    }
    let mut out = [0f32; 64];
    for v in 0..8 {
        for u in 0..8 {
            out[v * 8 + u] = (0..8).map(|y| rows[y * 8 + u] * basis[v][y]).sum();
        }
    }
    out
}

/// Encodes the image as a baseline JFIF JPEG: 8-bit YCbCr 4:4:4, Annex K tables at [`JPEG_QUALITY`].
/// Alpha is ignored, so composite over a background first. Sizes up to 65535x65535 are supported.
pub fn encode_jpeg(image: &RgbaImage) -> Result<Vec<u8>, String> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 || width > 0xffff || height > 0xffff {
        return Err("JPEG dimensions must be between 1 and 65535".into());
    }
    let quant = [scale_quant(&LUMA_QUANT, JPEG_QUALITY), scale_quant(&CHROMA_QUANT, JPEG_QUALITY)];

    let mut out = vec![0xff, 0xd8];
    jpeg_segment(&mut out, 0xe0, &[b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0]);
    for (id, table) in quant.iter().enumerate() {
        let mut payload = vec![id as u8];
        payload.extend(ZIGZAG.iter().map(|&at| table[at] as u8));
        jpeg_segment(&mut out, 0xdb, &payload);
    }
    let mut frame = vec![8];
    frame.extend_from_slice(&(height as u16).to_be_bytes());
    frame.extend_from_slice(&(width as u16).to_be_bytes());
    frame.extend_from_slice(&[3, 1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1]);
    jpeg_segment(&mut out, 0xc0, &frame);
    for (class_and_id, spec) in [(0x00u8, &DC_LUMA), (0x10, &AC_LUMA), (0x01, &DC_CHROMA), (0x11, &AC_CHROMA)] {
        let mut payload = vec![class_and_id];
        payload.extend_from_slice(&spec.0);
        payload.extend_from_slice(spec.1);
        jpeg_segment(&mut out, 0xc4, &payload);
    }
    jpeg_segment(&mut out, 0xda, &[3, 1, 0x00, 2, 0x11, 3, 0x11, 0, 63, 0]);

    let dc_codes = [huffman_codes(&DC_LUMA), huffman_codes(&DC_CHROMA)];
    let ac_codes = [huffman_codes(&AC_LUMA), huffman_codes(&AC_CHROMA)];
    let mut basis = [[0f32; 8]; 8];
    for (u, row) in basis.iter_mut().enumerate() {
        let scale = if u == 0 { (1.0f32 / 8.0).sqrt() } else { 0.5 };
        for (x, cell) in row.iter_mut().enumerate() {
            *cell = scale * ((2 * x + 1) as f32 * u as f32 * std::f32::consts::PI / 16.0).cos();
        }
    }

    let mut bits = BitWriter { out, accumulator: 0, pending: 0 };
    let mut previous_dc = [0i32; 3];
    for block_y in 0..height.div_ceil(8) {
        for block_x in 0..width.div_ceil(8) {
            let mut planes = [[0f32; 64]; 3];
            for i in 0..64u32 {
                // Edge pixels are replicated into the padding of partial blocks.
                let px = image.get_pixel((block_x * 8 + i % 8).min(width - 1), (block_y * 8 + i / 8).min(height - 1));
                let (r, g, b) = (f32::from(px[0]), f32::from(px[1]), f32::from(px[2]));
                planes[0][i as usize] = 0.299 * r + 0.587 * g + 0.114 * b - 128.0;
                planes[1][i as usize] = -0.168_736 * r - 0.331_264 * g + 0.5 * b;
                planes[2][i as usize] = 0.5 * r - 0.418_688 * g - 0.081_312 * b;
            }
            for (component, plane) in planes.iter().enumerate() {
                let table = usize::from(component > 0);
                let spatial = forward_dct(plane, &basis);
                let coefficients: Vec<i32> = ZIGZAG
                    .iter()
                    .map(|&at| (spatial[at] / f32::from(quant[table][at])).round() as i32)
                    .collect();
                let difference = coefficients[0] - previous_dc[component];
                previous_dc[component] = coefficients[0];
                let (category, extra) = magnitude_bits(difference);
                bits.put_symbol(&dc_codes[table], category);
                bits.put(extra, category as u32);
                let mut run = 0;
                for &value in &coefficients[1..] {
                    if value == 0 {
                        run += 1;
                        continue;
                    }
                    while run >= 16 {
                        bits.put_symbol(&ac_codes[table], 0xf0);
                        run -= 16;
                    }
                    let (category, extra) = magnitude_bits(value);
                    bits.put_symbol(&ac_codes[table], (run << 4) | category);
                    bits.put(extra, category as u32);
                    run = 0;
                }
                if run > 0 {
                    bits.put_symbol(&ac_codes[table], 0x00);
                }
            }
        }
    }
    let mut out = bits.finish();
    out.extend_from_slice(&[0xff, 0xd9]);
    Ok(out)
}

// ---------------------------------------------------------------- running the encoder

/// The `Err` text of [`export_video`] and [`Encoder::write_frame`] when the cancel flag was raised.
pub const EXPORT_CANCELLED: &str = "video export cancelled";
const STDERR_TAIL_BYTES: usize = 4096;
const PROBE_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
/// D's cancel closes ffmpeg's stdin so it finalises the file; only a stuck child is killed after this.
const CANCEL_GRACE: Duration = Duration::from_millis(500);
/// How long to wait for a pipe reader after its child exited (a grandchild may keep the pipe open).
const DRAIN_WAIT: Duration = Duration::from_millis(200);
const POLL: Duration = Duration::from_millis(2);
const FRAME_QUEUE: usize = 2;
/// How long [`Encoder::finish`] waits for the encoder to flush and exit once its input is closed.
pub const FINISH_DEADLINE: Duration = Duration::from_secs(120);

/// Background reader that keeps a bounded part of a pipe so the child never blocks on a full pipe.
struct Drain {
    data: Arc<Mutex<Vec<u8>>>,
    handle: JoinHandle<()>,
}

impl Drain {
    /// Keeps the last `limit` bytes when `keep_tail`, else the first `limit` bytes.
    fn spawn(mut reader: impl Read + Send + 'static, limit: usize, keep_tail: bool) -> Drain {
        let data = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::clone(&data);
        let handle = thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(read @ 1..) = reader.read(&mut chunk) {
                let mut kept = shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                if keep_tail {
                    kept.extend_from_slice(&chunk[..read]);
                    let excess = kept.len().saturating_sub(limit);
                    kept.drain(..excess);
                } else {
                    let room = limit.saturating_sub(kept.len()).min(read);
                    kept.extend_from_slice(&chunk[..room]);
                }
            }
        });
        Drain { data, handle }
    }

    fn collect(self) -> String {
        let deadline = Instant::now() + DRAIN_WAIT;
        while !self.handle.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        let kept = self.data.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        String::from_utf8_lossy(&kept).into_owned()
    }
}

/// Polls `child` until it exits; `None` when `keep_waiting` turns false first.
fn wait_while(child: &mut Child, mut keep_waiting: impl FnMut() -> bool) -> Result<Option<ExitStatus>, String> {
    loop {
        if let Some(status) = child.try_wait().map_err(|error| format!("cannot wait for the encoder: {error}"))? {
            return Ok(Some(status));
        }
        if !keep_waiting() {
            return Ok(None);
        }
        thread::sleep(POLL);
    }
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// A running encoder process fed raw RGBA frames on stdin. Dropping it kills and reaps the child.
pub struct Encoder {
    child: Child,
    frames: Option<SyncSender<Vec<u8>>>,
    writer: Option<JoinHandle<Result<(), String>>>,
    stderr: Option<Drain>,
    frame_bytes: usize,
    cancel: Arc<AtomicBool>,
}

impl Drop for Encoder {
    fn drop(&mut self) {
        kill_and_reap(&mut self.child);
    }
}

/// Output files `settings` would create: the numbered pattern expanded for image codecs.
fn existing_output(command: &EncoderCommand, frames: u32) -> Option<String> {
    let file = command.args.last()?;
    if file.contains("%d") {
        (1..=frames).map(|number| file.replacen("%d", &number.to_string(), 1)).find(|path| Path::new(path).exists())
    } else {
        Path::new(file).exists().then(|| file.clone())
    }
}

impl Encoder {
    /// Starts `program` with the argument vector of [`ffmpeg_command`] (never through a shell).
    /// Without `overwrite` an existing output is an error and ffmpeg gets `-n` instead of `-y`.
    pub fn spawn(
        program: &str,
        settings: &EncoderSettings,
        overwrite: bool,
        cancel: Arc<AtomicBool>,
    ) -> Result<Encoder, String> {
        let command = encoder_launch_command(program, settings, overwrite)?;
        Self::spawn_prepared(&command, settings.width, settings.height, settings.frames, overwrite, cancel, None)
    }

    pub(crate) fn spawn_prepared(
        command: &EncoderCommand, width: u32, height: u32, frames: u32, overwrite: bool,
        cancel: Arc<AtomicBool>, directory: Option<&Path>,
    ) -> Result<Encoder, String> {
        let frame_bytes = [width, height, 4].iter()
            .try_fold(1usize, |bytes, part| bytes.checked_mul(*part as usize)).ok_or("frame size overflows")?;
        if !overwrite {
            if let Some(existing) = existing_output(command, frames) { return Err(format!("{existing} already exists")); }
        }
        let mut process = Command::new(&command.program);
        process.args(&command.args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
        if let Some(directory) = directory { process.current_dir(directory); }
        let mut child = process.spawn().map_err(|error| format!("cannot start {}: {error}", command.program))?;
        let (Some(stdin), Some(stderr)) = (child.stdin.take(), child.stderr.take()) else {
            kill_and_reap(&mut child);
            return Err(format!("cannot open pipes to {}", command.program));
        };
        let (sender, receiver) = sync_channel::<Vec<u8>>(FRAME_QUEUE);
        // A thread owns stdin so a stuck child blocks it and not the caller's cancel check.
        let writer = thread::spawn(move || -> Result<(), String> {
            let mut stdin: ChildStdin = stdin;
            for frame in receiver {
                stdin.write_all(&frame).map_err(|error| format!("cannot write a frame to the encoder: {error}"))?;
            }
            Ok(())
        });
        Ok(Encoder {
            child,
            frames: Some(sender),
            writer: Some(writer),
            stderr: Some(Drain::spawn(stderr, STDERR_TAIL_BYTES, true)),
            frame_bytes,
            cancel,
        })
    }

    /// Queues one frame (`width*height*4` RGBA bytes); blocks while the encoder is behind, checking
    /// the cancel flag meanwhile.
    pub fn write_frame(&mut self, rgba: &[u8]) -> Result<(), String> {
        if rgba.len() != self.frame_bytes {
            return Err(format!("frame has {} bytes, expected {}", rgba.len(), self.frame_bytes));
        }
        let sender = self.frames.as_ref().ok_or("encoder input is closed")?;
        let mut frame = rgba.to_vec();
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(EXPORT_CANCELLED.into());
            }
            match sender.try_send(frame) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Full(returned)) => {
                    frame = returned;
                    thread::sleep(POLL);
                }
                Err(TrySendError::Disconnected(_)) => return Err("the encoder stopped accepting frames".into()),
            }
        }
    }

    /// Closes stdin, waits for the encoder and turns a non-zero exit into `Err` with its stderr tail.
    /// Raising the cancel flag meanwhile cancels instead; an encoder still running after
    /// [`FINISH_DEADLINE`] is killed and reported as a failure.
    pub fn finish(self) -> Result<(), String> {
        self.finish_within(FINISH_DEADLINE)
    }

    /// [`Encoder::finish`] with an explicit deadline.
    pub fn finish_within(mut self, limit: Duration) -> Result<(), String> {
        self.frames = None;
        let cancel = Arc::clone(&self.cancel);
        let deadline = Instant::now() + limit;
        let waited = wait_while(&mut self.child, || !cancel.load(Ordering::Relaxed) && Instant::now() < deadline)?;
        let Some(status) = waited else {
            if cancel.load(Ordering::Relaxed) {
                self.cancel_now();
                return Err(EXPORT_CANCELLED.into());
            }
            kill_and_reap(&mut self.child);
            let tail = self.stderr.take().map(Drain::collect).unwrap_or_default();
            let message = format!("ffmpeg did not exit within {limit:?} after the last frame");
            return Err(with_tail(&message, &tail));
        };
        let tail = self.stderr.take().map(Drain::collect).unwrap_or_default();
        let written = self.join_writer();
        if !status.success() {
            return Err(with_tail(&format!("ffmpeg exited with {status}"), &tail));
        }
        written
    }

    /// Stops after a failed frame (or a cancel): the cancel text is returned untouched, any other
    /// reason is extended with the encoder's stderr tail. The child is killed and reaped.
    pub fn abort(mut self, reason: String) -> String {
        if reason == EXPORT_CANCELLED {
            self.cancel_now();
            return reason;
        }
        kill_and_reap(&mut self.child);
        let tail = self.stderr.take().map(Drain::collect).unwrap_or_default();
        with_tail(&reason, &tail)
    }

    /// D's cancel: close stdin, let ffmpeg finalise the partial file briefly, then kill; always reaped.
    fn cancel_now(&mut self) {
        self.frames = None;
        let deadline = Instant::now() + CANCEL_GRACE;
        if !matches!(wait_while(&mut self.child, || Instant::now() < deadline), Ok(Some(_))) {
            kill_and_reap(&mut self.child);
        }
        let _ = self.join_writer();
    }

    fn join_writer(&mut self) -> Result<(), String> {
        let Some(handle) = self.writer.take() else { return Ok(()) };
        let deadline = Instant::now() + DRAIN_WAIT;
        while !handle.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        if handle.is_finished() {
            handle.join().unwrap_or_else(|_| Err("encoder writer thread panicked".into()))
        } else {
            Ok(())
        }
    }
}

fn with_tail(message: &str, tail: &str) -> String {
    let tail = tail.trim();
    if tail.is_empty() { message.to_owned() } else { format!("{message}: {tail}") }
}

/// Where and how [`export_video`] encodes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoOutput {
    /// Encoder executable; a stub in tests.
    pub program: String,
    /// ffmpeg encoder tag or `"auto"`.
    pub codec: String,
    pub file: String,
    /// Extra ffmpeg options, split on whitespace.
    pub ffmpeg_options: String,
    /// See [`EncoderSettings::audio`].
    pub audio: Vec<AudioInput>,
}

impl Default for VideoOutput {
    fn default() -> Self {
        Self {
            program: "ffmpeg".into(),
            codec: "auto".into(),
            file: String::new(),
            ffmpeg_options: String::new(),
            audio: Vec::new(),
        }
    }
}

/// Renders the animation and pipes every frame to the encoder, as D's video window does.
/// `progress(done, total)` runs after each frame is handed over. When `cancel` is raised the encoder
/// is closed (the partial file is kept, as in D) and `Err(EXPORT_CANCELLED)` is returned. Any encoder
/// failure returns `Err` carrying the tail of its stderr.
pub fn export_video(
    session: &mut Session,
    options: &VideoExportOptions,
    output: &VideoOutput,
    cancel: Arc<AtomicBool>,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<VideoPlan, String> {
    let plan = plan_video(session, options)?;
    let total = plan.frames();
    let settings = EncoderSettings {
        codec: output.codec.clone(),
        framerate: u32::try_from(plan.plan.fps).unwrap_or(0),
        file: output.file.clone(),
        frames: total as u32,
        width: plan.camera.width,
        height: plan.camera.height,
        transparency: options.transparency,
        ffmpeg_options: output.ffmpeg_options.clone(),
        audio: output.audio.clone(),
    };
    let command = encoder_launch_command(&output.program, &settings, options.overwrite)?;
    export_video_planned(session, options, &plan, &command, None, cancel, progress)?;
    Ok(plan)
}

pub(crate) fn export_video_planned(
    session: &mut Session, options: &VideoExportOptions, plan: &VideoPlan, command: &EncoderCommand,
    directory: Option<&Path>, cancel: Arc<AtomicBool>, progress: &mut dyn FnMut(usize, usize),
) -> Result<(), String> {
    let total = plan.frames();
    let mut encoder = Encoder::spawn_prepared(command, plan.camera.width, plan.camera.height, total as u32,
        options.overwrite, cancel, directory)?;
    let rendered = render_video_frames_planned(session, options, plan, &mut |index, image| {
        encoder.write_frame(image.as_raw())?;
        progress(index + 1, total);
        Ok(())
    });
    match rendered {
        Ok(()) => encoder.finish(),
        Err(error) => Err(encoder.abort(error)),
    }
}

/// Runs `program -codecs` and parses the encoder list; `Err` on a spawn failure, a non-zero exit,
/// unrecognised output, or when it takes longer than `timeout` (the process is then killed).
pub fn probe_encoders(program: &str, timeout: Duration) -> Result<Vec<VideoCodec>, String> {
    let mut child = Command::new(program)
        .args(&ffmpeg_codecs_command().args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot start {program}: {error}"))?;
    let Some(stdout) = child.stdout.take() else {
        kill_and_reap(&mut child);
        return Err(format!("cannot open pipes to {program}"));
    };
    let output = Drain::spawn(stdout, PROBE_OUTPUT_LIMIT, false);
    let deadline = Instant::now() + timeout;
    let status = match wait_while(&mut child, || Instant::now() < deadline) {
        Ok(Some(status)) => status,
        Ok(None) => {
            kill_and_reap(&mut child);
            return Err(format!("{program} -codecs did not finish within {} ms", timeout.as_millis()));
        }
        Err(error) => {
            kill_and_reap(&mut child);
            return Err(error);
        }
    };
    if !status.success() {
        return Err(format!("{program} -codecs exited with {status}"));
    }
    parse_encoders(&output.collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_paths_refuse_network_and_device_names() {
        for path in [r"\\host\share\image.png", "//host/share/image.png", r"\\.\pipe\image", r"\\?\UNC\host\share\image.png"] {
            assert_eq!(local_path(path), Err("is a network or device path"));
        }
        assert_eq!(local_path("relative/image.png"), Err("is not an absolute path"));
        #[cfg(windows)]
        for path in [r"C:\images\a.png", r"\\?\C:\images\a.png"] {
            assert_eq!(local_path(path), Ok(()));
        }
    }

    #[test]
    fn windows_encoder_lookup_preserves_application_system_and_path_order() {
        let cwd = Path::new("cwd");
        let search: Vec<PathBuf> = ["application", "system", "windows", "path"].into_iter().map(PathBuf::from).collect();
        assert_eq!(windows_encoder_candidates("ffmpeg", cwd, search.clone()),
            ["application", "system", "windows", "path"].map(|dir| cwd.join(dir).join("ffmpeg.exe")));
        assert_eq!(windows_encoder_candidates("ffmpeg.cmd", cwd, search.clone())[0], cwd.join("application/ffmpeg.cmd"));
        assert_eq!(windows_encoder_candidates("tools/ffmpeg.cmd", cwd, search),
            [cwd.join("tools/ffmpeg.cmd.exe"), cwd.join("tools/ffmpeg.cmd")]);
    }

    #[test]
    fn even_dimension_matches_excamera() {
        assert_eq!(even_dimension(1919.4), 1920);
        assert_eq!(even_dimension(-7.0), 8);
        assert_eq!(even_dimension(0.2), 2);
        assert_eq!(even_dimension(f64::NAN), 2);
    }
}
