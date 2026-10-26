//! Deformation probe for comparing software or OpenGL offsets with the CPU result.
//!
//! Headless sessions use the software reference. A registered OpenGL callback is
//! synchronous and diagnostic-only; CPU offsets remain authoritative. The packet
//! is transient and no shader binary is written into an INP file.

use std::cell::Cell;
use std::ffi::{c_char, c_void};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct F32Slice {
    pub(crate) ptr: *const f32,
    pub(crate) len: usize,
}

#[repr(C)]
pub struct GpuProbePacket {
    pub(crate) vertices_xy: F32Slice,
    pub(crate) depths: F32Slice,
    pub(crate) bones: F32Slice,
    pub(crate) sources: F32Slice,
    pub(crate) weights: F32Slice,
    pub(crate) target_to_root: [f32; 16],
    pub(crate) root_to_target: [f32; 16],
    pub(crate) vertex_count: u32,
    pub(crate) bone_count: u32,
    pub(crate) source_count: u32,
    pub(crate) max_influences: u32,
    pub(crate) mode: u32,
    pub(crate) radius_floor: f32,
    pub(crate) radius_scale: f32,
}

pub type GpuProbeCallback = unsafe extern "C" fn(
    user: *mut c_void,
    packet: *const GpuProbePacket,
    out_xy: *mut f32,
    out_len: usize,
    error: *mut c_char,
    error_cap: usize,
) -> i32;

#[derive(Clone, Copy)]
pub(crate) struct Probe {
    pub(crate) user: *mut c_void,
    pub(crate) callback: GpuProbeCallback,
}

const MAX_BONES: usize = 64;
const MAX_INFLUENCES: usize = 16;
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const ABSOLUTE_TOLERANCE: f64 = 1e-4;

#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum Fault {
    None,
    Mismatch,
    NonFinite,
}

thread_local! {
    static DEVICE: Cell<Option<bool>> = const { Cell::new(None) };
    static FAULT: Cell<Fault> = const { Cell::new(Fault::None) };
    static ACTIVE_PROBE: Cell<Option<Probe>> = const { Cell::new(None) };
    static CALLBACK_ACTIVE: Cell<bool> = const { Cell::new(false) };
    static HARDWARE: Cell<bool> = const { Cell::new(false) };
}

pub(crate) struct ProbeGuard {
    previous_probe: Option<Probe>,
    previous_hardware: bool,
}

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        ACTIVE_PROBE.with(|cell| cell.set(self.previous_probe));
        HARDWARE.with(|cell| cell.set(self.previous_hardware));
    }
}

struct CallbackGuard {
    previous: bool,
}

impl Drop for CallbackGuard {
    fn drop(&mut self) {
        CALLBACK_ACTIVE.with(|cell| cell.set(self.previous));
    }
}

pub(crate) fn activate_probe(probe: Option<Probe>) -> ProbeGuard {
    ProbeGuard {
        previous_probe: ACTIVE_PROBE.with(|cell| cell.replace(probe)),
        previous_hardware: HARDWARE.with(|cell| cell.replace(false)),
    }
}

pub(crate) fn backend() -> &'static str {
    if device_present() && ACTIVE_PROBE.with(|cell| cell.get().is_some()) {
        "opengl-transform-feedback"
    } else {
        "software"
    }
}

pub(crate) fn hardware() -> bool {
    HARDWARE.with(|cell| cell.get())
}

pub(crate) fn callback_is_active() -> bool {
    CALLBACK_ACTIVE.with(|cell| cell.get())
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct DeviceGuard {
    previous: Option<bool>,
}

impl Drop for DeviceGuard {
    fn drop(&mut self) {
        DEVICE.with(|cell| cell.set(self.previous));
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct FaultGuard {
    previous: Fault,
}

impl Drop for FaultGuard {
    fn drop(&mut self) {
        FAULT.with(|cell| cell.set(self.previous));
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn force_device(present: bool) -> DeviceGuard {
    DeviceGuard {
        previous: DEVICE.with(|cell| cell.replace(Some(present))),
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn force_fault(fault: Fault) -> FaultGuard {
    FaultGuard {
        previous: FAULT.with(|cell| cell.replace(fault)),
    }
}

pub(crate) fn device_present() -> bool {
    if let Some(forced) = DEVICE.with(|cell| cell.get()) {
        return forced;
    }
    match std::env::var("VSOUL_GPU") {
        Ok(value) => !matches!(value.as_str(), "0" | "off" | "absent" | "false"),
        Err(_) => true,
    }
}

#[derive(Clone)]
pub(crate) struct GpuBone {
    pub rest_head: [f64; 3],
    pub rest_tail: [f64; 3],
    pub rest_length: f64,
    pub parent: Option<usize>,
    pub skin: [[f64; 4]; 4],
    pub no_yaw_skin: [[f64; 4]; 4],
}

#[derive(Clone)]
pub(crate) struct GpuSource {
    pub bone: usize,
    pub terminal: bool,
    pub lock_to_root: bool,
    pub linear: bool,
    pub weight: f64,
    pub depth_scale: f64,
    pub depth_offset: f64,
    pub multiplier: f64,
    pub sin_rotation: f64,
    pub cos_rotation: f64,
    pub pivot_shift: f64,
    pub pose_yaw: f64,
}

#[derive(Clone)]
pub(crate) struct Packet {
    pub vertices: Vec<[f64; 2]>,
    pub depths: Vec<f64>,
    pub bones: Vec<GpuBone>,
    pub sources: Vec<GpuSource>,
    pub weights: Option<Vec<Vec<f64>>>,
    pub target_to_root: [[f64; 4]; 4],
    pub root_to_target: [[f64; 4]; 4],
    pub max_influences: usize,
    pub radius_scale: f64,
    pub radius_floor: f64,
}

pub(crate) fn execute(packet: &Packet) -> Result<Vec<[f64; 2]>, String> {
    if packet.bones.len() > MAX_BONES {
        return Err("Depth bone GPU packet exceeds maximum bone count".into());
    }
    let mut offsets = match (device_present(), ACTIVE_PROBE.with(|cell| cell.get())) {
        (true, Some(probe)) => execute_probe(packet, probe)?,
        _ => execute_software(packet)?,
    };
    match FAULT.with(|cell| cell.get()) {
        Fault::None => {}
        Fault::Mismatch => {
            if let Some(offset) = offsets.first_mut() {
                offset[0] += 1.0;
            }
        }
        Fault::NonFinite => {
            if let Some(offset) = offsets.first_mut() {
                offset[0] = f64::NAN;
            }
        }
    }
    if offsets.iter().flatten().any(|value| !value.is_finite()) {
        return Err("non-finite gpu buffer".into());
    }
    Ok(offsets)
}

fn execute_software(packet: &Packet) -> Result<Vec<[f64; 2]>, String> {
    let mut offsets = Vec::with_capacity(packet.vertices.len());
    for (index, vertex) in packet.vertices.iter().enumerate() {
        let z = packet.depths.get(index).copied().unwrap_or(0.0);
        let offset = if let Some(weights) = &packet.weights {
            skin_weighted(packet, *vertex, z, weights, index)?
        } else {
            skin_gaussian(packet, *vertex, z)?
        };
        offsets.push(offset);
    }
    Ok(offsets)
}

fn execute_probe(packet: &Packet, probe: Probe) -> Result<Vec<[f64; 2]>, String> {
    if CALLBACK_ACTIVE.with(|cell| cell.replace(true)) {
        return Err("reentrant gpu probe callback".into());
    }
    let _callback_guard = CallbackGuard { previous: false };
    HARDWARE.with(|cell| cell.set(false));
    let vertex_count = u32::try_from(packet.vertices.len())
        .map_err(|_| "Depth bone GPU packet exceeds maximum vertex count")?;
    let bone_count = u32::try_from(packet.bones.len())
        .map_err(|_| "Depth bone GPU packet exceeds maximum bone count")?;
    let source_count = u32::try_from(packet.sources.len())
        .map_err(|_| "Depth bone GPU packet exceeds maximum source count")?;
    let max_influences = u32::try_from(packet.max_influences)
        .map_err(|_| "Depth bone GPU packet exceeds maximum influence count")?;

    let mut vertices_xy = Vec::with_capacity(packet.vertices.len() * 2);
    for vertex in &packet.vertices {
        vertices_xy.extend([vertex[0] as f32, vertex[1] as f32]);
    }
    let depths: Vec<f32> = packet.depths.iter().map(|value| *value as f32).collect();
    let mut bones = Vec::with_capacity(packet.bones.len() * 24);
    for bone in &packet.bones {
        bones.extend(bone.rest_head.map(|value| value as f32));
        bones.push(bone.rest_length as f32);
        bones.extend(bone.rest_tail.map(|value| value as f32));
        bones.push(bone.parent.map_or(-1.0, |value| value as f32));
        append_matrix(&mut bones, bone.skin);
    }
    let mut sources = Vec::with_capacity(packet.sources.len() * 28);
    for source in &packet.sources {
        sources.extend([
            source.bone as f32,
            source.terminal as u8 as f32,
            source.lock_to_root as u8 as f32,
            source.linear as u8 as f32,
            source.weight as f32,
            source.depth_scale as f32,
            source.depth_offset as f32,
            source.multiplier as f32,
            source.sin_rotation as f32,
            source.cos_rotation as f32,
            source.pivot_shift as f32,
            source.pose_yaw as f32,
        ]);
        if let Some(bone) = packet.bones.get(source.bone) {
            append_matrix(&mut sources, bone.no_yaw_skin);
        } else {
            sources.extend([0.0; 16]);
        }
    }
    let weights: Vec<f32> = packet.weights.as_ref().map_or_else(Vec::new, |weights| {
        let mut flat = Vec::with_capacity(packet.sources.len() * packet.vertices.len());
        for source in 0..packet.sources.len() {
            for vertex in 0..packet.vertices.len() {
                flat.push(
                    weights
                        .get(source)
                        .and_then(|row| row.get(vertex))
                        .copied()
                        .unwrap_or(0.0) as f32,
                );
            }
        }
        flat
    });
    let ffi_packet = GpuProbePacket {
        vertices_xy: F32Slice {
            ptr: vertices_xy.as_ptr(),
            len: vertices_xy.len(),
        },
        depths: F32Slice {
            ptr: depths.as_ptr(),
            len: depths.len(),
        },
        bones: F32Slice {
            ptr: bones.as_ptr(),
            len: bones.len(),
        },
        sources: F32Slice {
            ptr: sources.as_ptr(),
            len: sources.len(),
        },
        weights: F32Slice {
            ptr: weights.as_ptr(),
            len: weights.len(),
        },
        target_to_root: matrix_array(packet.target_to_root),
        root_to_target: matrix_array(packet.root_to_target),
        vertex_count,
        bone_count,
        source_count,
        max_influences,
        mode: u32::from(packet.weights.is_some()),
        radius_floor: packet.radius_floor as f32,
        radius_scale: packet.radius_scale as f32,
    };
    let mut output = vec![0.0f32; packet.vertices.len() * 2];
    let mut error = [0 as c_char; 512];
    let result = unsafe {
        (probe.callback)(
            probe.user,
            &ffi_packet,
            output.as_mut_ptr(),
            output.len(),
            error.as_mut_ptr(),
            error.len(),
        )
    };
    if result != 0 {
        let message: Vec<u8> = error
            .iter()
            .take_while(|value| **value != 0)
            .map(|value| *value as u8)
            .collect();
        let message = String::from_utf8_lossy(&message);
        return Err(if message.is_empty() {
            format!("gpu probe callback failed ({result})")
        } else {
            message.into_owned()
        });
    }
    if output.iter().any(|value| !value.is_finite()) {
        return Err("non-finite gpu buffer".into());
    }
    let offsets = output
        .chunks_exact(2)
        .map(|offset| [offset[0] as f64, offset[1] as f64])
        .collect();
    HARDWARE.with(|cell| cell.set(true));
    Ok(offsets)
}

fn append_matrix(values: &mut Vec<f32>, matrix: [[f64; 4]; 4]) {
    for row in matrix {
        values.extend(row.map(|value| value as f32));
    }
}

fn matrix_array(matrix: [[f64; 4]; 4]) -> [f32; 16] {
    let mut result = [0.0; 16];
    for (row_index, row) in matrix.into_iter().enumerate() {
        for (column_index, value) in row.into_iter().enumerate() {
            result[row_index * 4 + column_index] = value as f32;
        }
    }
    result
}

fn skin_gaussian(packet: &Packet, vertex: [f64; 2], z: f64) -> Result<[f64; 2], String> {
    let limit = packet.max_influences.min(MAX_INFLUENCES);
    let mut scores = [0.0; MAX_INFLUENCES];
    let mut distances = [0.0; MAX_INFLUENCES];
    let mut source_indices = [0usize; MAX_INFLUENCES];
    let mut rests = [[0.0; 3]; MAX_INFLUENCES];
    let mut count = 0usize;
    let mut locked: Option<(usize, f64, f64, [f64; 3])> = None;
    for (source_index, source) in packet.sources.iter().enumerate() {
        if source.bone >= packet.bones.len() {
            continue;
        }
        let mut score = source.weight * source.multiplier;
        if !(score > 0.0) {
            continue;
        }
        let source_depth = z * source.depth_scale + source.depth_offset;
        let source_rest = transform(packet.target_to_root, [vertex[0], vertex[1], source_depth])?;
        let bone = &packet.bones[source.bone];
        let rest_length = bone.rest_length.max(1e-4);
        let mut distance_sq = 0.0;
        let mut terminal_distance_sq = f64::MAX;
        let mut terminal_projection = 0.0;
        let radius = (rest_length * 0.85 * packet.radius_scale).max(packet.radius_floor);
        let radius_sq = radius * radius;
        if packet.sources.len() > 1 {
            distance_sq = distance_sq_segment(source_rest, bone.rest_head, bone.rest_tail);
            if source.linear {
                score *= (1.0 - distance_sq.sqrt() / radius).max(0.0);
            } else {
                score *= (-distance_sq / radius_sq).exp();
            }
            if source.terminal {
                if let Some(parent) = bone.parent {
                    if parent < packet.bones.len() {
                        terminal_projection = projection(
                            source_rest,
                            packet.bones[parent].rest_head,
                            bone.rest_head,
                        );
                        terminal_distance_sq = distance_sq_segment(
                            source_rest,
                            packet.bones[parent].rest_head,
                            bone.rest_head,
                        );
                    }
                }
            }
        }
        if !(score > 0.0) {
            continue;
        }
        if packet.sources.len() > 1
            && source.terminal
            && source.lock_to_root
            && terminal_projection > 1.0
            && terminal_distance_sq <= radius_sq
        {
            let replace = match locked {
                None => true,
                Some((_, locked_score, locked_distance, _)) => {
                    score > locked_score
                        || (score == locked_score && terminal_distance_sq < locked_distance)
                }
            };
            if replace {
                locked = Some((source_index, score, terminal_distance_sq, source_rest));
            }
            continue;
        }
        insert(
            &mut scores,
            &mut distances,
            &mut source_indices,
            &mut rests,
            &mut count,
            limit,
            score,
            distance_sq,
            source_index,
            source_rest,
        );
    }
    if let Some((source_index, _, _, rest)) = locked {
        count = 1;
        scores[0] = 1.0;
        source_indices[0] = source_index;
        rests[0] = rest;
    }
    if count == 0 {
        return Ok([0.0, 0.0]);
    }
    let mut total = 0.0;
    for score in scores.iter().take(count) {
        total += *score;
    }
    if !(total > 1e-8) {
        total = 1.0;
        count = 1;
        scores[0] = 1.0;
    }
    blend(packet, &scores, &source_indices, &rests, count, total)
}

fn skin_weighted(
    packet: &Packet,
    vertex: [f64; 2],
    z: f64,
    weights: &[Vec<f64>],
    vertex_index: usize,
) -> Result<[f64; 2], String> {
    let mut rest = [0.0; 3];
    let mut deformed = [0.0; 3];
    for (source_index, source) in packet.sources.iter().enumerate() {
        let weight = weights
            .get(source_index)
            .and_then(|row| row.get(vertex_index))
            .copied()
            .unwrap_or(0.0);
        if weight == 0.0 || source.bone >= packet.bones.len() {
            continue;
        }
        let source_depth = z * source.depth_scale + source.depth_offset;
        let source_rest = transform(packet.target_to_root, [vertex[0], vertex[1], source_depth])?;
        let point = posed_point(packet, source, source_rest)?;
        rest = add(rest, scale(source_rest, weight));
        deformed = add(deformed, scale(point, weight));
    }
    offset_of(packet, rest, deformed)
}

fn blend(
    packet: &Packet,
    scores: &[f64],
    source_indices: &[usize],
    rests: &[[f64; 3]],
    count: usize,
    total: f64,
) -> Result<[f64; 2], String> {
    let mut rest = [0.0; 3];
    let mut deformed = [0.0; 3];
    for index in 0..count {
        let weight = scores[index] / total;
        rest = add(rest, scale(rests[index], weight));
        let source = &packet.sources[source_indices[index]];
        let point = posed_point(packet, source, rests[index])?;
        deformed = add(deformed, scale(point, weight));
    }
    offset_of(packet, rest, deformed)
}

fn posed_point(packet: &Packet, source: &GpuSource, rest: [f64; 3]) -> Result<[f64; 3], String> {
    let bone = &packet.bones[source.bone];
    if source.pose_yaw == 0.0
        || source.pivot_shift == 0.0
        || (source.sin_rotation == 0.0 && source.cos_rotation == 1.0)
    {
        return transform(bone.skin, rest);
    }
    let rotation_rest = add(rest, [source.pivot_shift, 0.0, 0.0]);
    let base = transform(bone.no_yaw_skin, rest)?;
    let with_yaw = transform(bone.skin, rotation_rest)?;
    let without_yaw = transform(bone.no_yaw_skin, rotation_rest)?;
    Ok(add(base, sub(with_yaw, without_yaw)))
}

fn offset_of(packet: &Packet, rest: [f64; 3], deformed: [f64; 3]) -> Result<[f64; 2], String> {
    let rest_local = transform(packet.root_to_target, rest)?;
    let local = transform(packet.root_to_target, deformed)?;
    let offset = [local[0] - rest_local[0], local[1] - rest_local[1]];
    if !offset[0].is_finite() || !offset[1].is_finite() {
        return Err("non-finite gpu buffer".into());
    }
    Ok(offset)
}

fn insert(
    scores: &mut [f64],
    distances: &mut [f64],
    source_indices: &mut [usize],
    rests: &mut [[f64; 3]],
    count: &mut usize,
    limit: usize,
    score: f64,
    distance_sq: f64,
    source_index: usize,
    rest: [f64; 3],
) {
    if limit == 0 {
        return;
    }
    let mut pos = *count;
    if *count < limit {
        *count += 1;
    } else {
        pos = limit - 1;
        if score < scores[pos] || (score == scores[pos] && distance_sq >= distances[pos]) {
            return;
        }
    }
    while pos > 0 {
        let prev = pos - 1;
        if score < scores[prev] || (score == scores[prev] && distance_sq >= distances[prev]) {
            break;
        }
        scores[pos] = scores[prev];
        distances[pos] = distances[prev];
        source_indices[pos] = source_indices[prev];
        rests[pos] = rests[prev];
        pos = prev;
    }
    scores[pos] = score;
    distances[pos] = distance_sq;
    source_indices[pos] = source_index;
    rests[pos] = rest;
}

fn transform(matrix: [[f64; 4]; 4], point: [f64; 3]) -> Result<[f64; 3], String> {
    let mut output = [0.0; 3];
    for row in 0..3 {
        output[row] = matrix[row][0] * point[0]
            + matrix[row][1] * point[1]
            + matrix[row][2] * point[2]
            + matrix[row][3];
    }
    if output.iter().any(|value| !value.is_finite()) {
        return Err("non-finite gpu buffer".into());
    }
    Ok(output)
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(value: [f64; 3], factor: f64) -> [f64; 3] {
    [value[0] * factor, value[1] * factor, value[2] * factor]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn projection(point: [f64; 3], a: [f64; 3], b: [f64; 3]) -> f64 {
    let ab = sub(b, a);
    let denom = dot(ab, ab);
    if denom <= 1e-8 {
        0.0
    } else {
        dot(sub(point, a), ab) / denom
    }
}

fn distance_sq_segment(point: [f64; 3], a: [f64; 3], b: [f64; 3]) -> f64 {
    let t = projection(point, a, b).clamp(0.0, 1.0);
    let q = add(a, scale(sub(b, a), t));
    let delta = sub(point, q);
    dot(delta, delta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;
    use serde_json::json;

    thread_local! {
        static REENTRANT_EXECUTE_REJECTED: Cell<bool> = const { Cell::new(false) };
    }

    unsafe extern "C" fn failed_probe(
        _user: *mut c_void,
        _packet: *const GpuProbePacket,
        _out_xy: *mut f32,
        _out_len: usize,
        error: *mut c_char,
        error_cap: usize,
    ) -> i32 {
        let message = b"probe failed";
        if !error.is_null() && error_cap > 0 {
            let count = message.len().min(error_cap - 1);
            unsafe {
                ptr::copy_nonoverlapping(message.as_ptr().cast(), error, count);
                *error.add(count) = 0;
            }
        }
        1
    }

    unsafe extern "C" fn reentrant_probe(
        _user: *mut c_void,
        _packet: *const GpuProbePacket,
        out_xy: *mut f32,
        out_len: usize,
        _error: *mut c_char,
        _error_cap: usize,
    ) -> i32 {
        REENTRANT_EXECUTE_REJECTED.with(|cell| {
            cell.set(execute(&minimal_packet()).is_err());
        });
        if !out_xy.is_null() {
            for index in 0..out_len {
                unsafe { *out_xy.add(index) = 0.0 };
            }
        }
        0
    }

    #[test]
    fn sixty_five_bones_fail_the_kernel_without_a_short_buffer() {
        let bone = GpuBone {
            rest_head: [0.0; 3],
            rest_tail: [0.0, 1.0, 0.0],
            rest_length: 1.0,
            parent: None,
            skin: identity(),
            no_yaw_skin: identity(),
        };
        let packet = Packet {
            vertices: vec![[0.0, 0.0]],
            depths: vec![0.0],
            bones: vec![bone; 65],
            sources: vec![GpuSource {
                bone: 0,
                terminal: true,
                lock_to_root: false,
                linear: false,
                weight: 1.0,
                depth_scale: 1.0,
                depth_offset: 0.0,
                multiplier: 1.0,
                sin_rotation: 0.0,
                cos_rotation: 1.0,
                pivot_shift: 0.0,
                pose_yaw: 0.0,
            }],
            weights: None,
            target_to_root: identity(),
            root_to_target: identity(),
            max_influences: 4,
            radius_scale: 1.0,
            radius_floor: 1.0,
        };
        let error = execute(&packet).unwrap_err();
        assert!(error.contains("maximum bone count"), "{error}");
    }

    fn identity() -> [[f64; 4]; 4] {
        [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    fn minimal_packet() -> Packet {
        Packet {
            vertices: vec![[0.0, 0.0]],
            depths: vec![0.0],
            bones: vec![GpuBone {
                rest_head: [0.0; 3],
                rest_tail: [0.0, 1.0, 0.0],
                rest_length: 1.0,
                parent: None,
                skin: identity(),
                no_yaw_skin: identity(),
            }],
            sources: vec![GpuSource {
                bone: 0,
                terminal: false,
                lock_to_root: false,
                linear: false,
                weight: 1.0,
                depth_scale: 1.0,
                depth_offset: 0.0,
                multiplier: 1.0,
                sin_rotation: 0.0,
                cos_rotation: 1.0,
                pivot_shift: 0.0,
                pose_yaw: 0.0,
            }],
            weights: None,
            target_to_root: identity(),
            root_to_target: identity(),
            max_influences: 1,
            radius_scale: 1.0,
            radius_floor: 1.0,
        }
    }

    #[test]
    fn gpu_probe_packet_uses_c_layout() {
        use std::mem::{offset_of, size_of};

        assert_eq!(size_of::<F32Slice>(), 2 * size_of::<usize>());
        assert_eq!(offset_of!(GpuProbePacket, vertices_xy), 0);
        assert_eq!(offset_of!(GpuProbePacket, depths), size_of::<F32Slice>());
        assert_eq!(offset_of!(GpuProbePacket, bones), 2 * size_of::<F32Slice>());
        assert_eq!(offset_of!(GpuProbePacket, sources), 3 * size_of::<F32Slice>());
        assert_eq!(offset_of!(GpuProbePacket, weights), 4 * size_of::<F32Slice>());
        assert_eq!(
            offset_of!(GpuProbePacket, target_to_root),
            5 * size_of::<F32Slice>()
        );
    }

    #[test]
    fn callback_failure_is_reported_and_nested_callbacks_are_rejected() {
        let _device = force_device(true);
        let packet = minimal_packet();
        {
            let _probe = activate_probe(Some(Probe {
                user: ptr::null_mut(),
                callback: failed_probe,
            }));
            assert_eq!(execute(&packet).unwrap_err(), "probe failed");
            assert!(!hardware());
            assert_eq!(backend(), "opengl-transform-feedback");
        }
        assert_eq!(backend(), "software");

        REENTRANT_EXECUTE_REJECTED.with(|cell| cell.set(false));
        let _probe = activate_probe(Some(Probe {
            user: ptr::null_mut(),
            callback: reentrant_probe,
        }));
        assert_eq!(execute(&packet).unwrap(), vec![[0.0, 0.0]]);
        REENTRANT_EXECUTE_REJECTED.with(|cell| assert!(cell.get()));
        assert!(hardware());
    }

    #[test]
    fn weighted_ik_pose_matches_and_a_bad_gpu_keeps_cpu_offsets() {
        let mut session = crate::Session::new();
        let rig = ok(&session.handle_request(&json!({
            "op":"node.create","className":"DepthRigRoot","name":"Rig"
        })));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let parent = bone(&mut session, rig_id, "Parent", [0.0, 0.0, 0.0], [0.0, 100.0, 0.0]);
        let child = bone(&mut session, parent, "Child", [0.0, 100.0, 0.0], [0.0, 200.0, 0.0]);
        let grid = ok(&session.handle_request(&json!({
            "op":"node.create","className":"GridDeformer","name":"Grid"
        })));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0, 40.0],"axisY":[80.0, 140.0]
        })));
        ok(&session.handle_request(&json!({
            "op":"rig.add_source","root":rig_id,"target":grid_id,"bone":child
        })));
        ok(&session.handle_request(&json!({
            "op":"rig.add_source","root":rig_id,"target":grid_id,"bone":parent
        })));
        ok(&session.handle_request(&json!({
            "op":"rig.set_vertex_weights","root":rig_id,"target":grid_id,
            "weights":[
                {"bone": child, "values": [1.0, 0.25, 0.0, 0.0]},
                {"bone": parent, "values": [0.0, 0.75, 1.0, 1.0]}
            ]
        })));
        let parameter = ok(&session.handle_request(&json!({
            "op":"parameter.create","dimensions":1,"name":"Bend"
        })));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        for bone_id in [parent, child] {
            ok(&session.handle_request(&json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],"key":[1,0],
                "rotation_x":0.0,"rotation_y":0.0,"rotation_degrees":0.0
            })));
            ok(&session.handle_request(&json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],"key":[0,0],
                "rotation_x":0.2,"rotation_y":0.0,"rotation_degrees":0.0
            })));
        }
        ok(&session.handle_request(&json!({
            "op":"rig.solve_ik","root":rig_id,"endEffector":child,"target":[70.0, 80.0, 0.0]
        })));
        let saved = deform_values(&mut session, parameter_id, grid_id);
        let before = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let report = ok(&session.handle_request(&json!({
            "op":"rig.gpu_offsets","root":rig_id,"parameter":parameter_id
        })));
        assert_eq!(report["device"], true, "{report}");
        assert_eq!(report["backend"], "software");
        assert_eq!(report["matched"], true, "{report}");
        assert!(report["maxAbs"].as_f64().unwrap() <= ABSOLUTE_TOLERANCE, "{report}");
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), before);

        ok(&session.handle_request(&json!({"op":"undo"})));
        {
            let _fault = force_fault(Fault::Mismatch);
            ok(&session.handle_request(&json!({
                "op":"rig.solve_ik","root":rig_id,"endEffector":child,"target":[70.0, 80.0, 0.0]
            })));
        }
        assert_eq!(deform_values(&mut session, parameter_id, grid_id), saved);
        let mismatched = ok(&session.handle_request(&json!({
            "op":"rig.gpu_offsets","root":rig_id,"parameter":parameter_id
        })));
        assert_eq!(mismatched["matched"], true, "{mismatched}");

        ok(&session.handle_request(&json!({"op":"undo"})));
        {
            let _device = force_device(false);
            ok(&session.handle_request(&json!({
                "op":"rig.solve_ik","root":rig_id,"endEffector":child,"target":[70.0, 80.0, 0.0]
            })));
            let absent = ok(&session.handle_request(&json!({
                "op":"rig.gpu_offsets","root":rig_id,"parameter":parameter_id
            })));
            assert_eq!(absent["device"], false, "{absent}");
            assert_eq!(absent["backend"], "absent");
            let preview = ok(&session.handle_request(&json!({
                "op":"rig.preview_deform","root":rig_id,"parameter":parameter_id
            })));
            assert!(!preview["targets"].as_array().unwrap().is_empty(), "{preview}");
        }
        assert_eq!(deform_values(&mut session, parameter_id, grid_id), saved);

        ok(&session.handle_request(&json!({"op":"undo"})));
        {
            let _fault = force_fault(Fault::NonFinite);
            ok(&session.handle_request(&json!({
                "op":"rig.solve_ik","root":rig_id,"endEffector":child,"target":[70.0, 80.0, 0.0]
            })));
        }
        assert_eq!(deform_values(&mut session, parameter_id, grid_id), saved);
    }

    fn bone(
        session: &mut crate::Session,
        parent: u64,
        name: &str,
        head: [f64; 3],
        tail: [f64; 3],
    ) -> u64 {
        let created = ok(&session.handle_request(&json!({
            "op":"node.create","className":"DepthBone","name":name,"parents":[parent],
            "restHead":head,"restTail":tail
        })));
        let id = created["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"node.transform","id":id,"translation":head
        })));
        id
    }

    fn deform_values(session: &mut crate::Session, parameter: u64, node: u64) -> serde_json::Value {
        let parameter = ok(&session.handle_request(&json!({"op":"parameter.get","id":parameter})));
        parameter["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["param_name"] == "deform" && binding["node"] == node)
            .unwrap()["values"]
            .clone()
    }

    fn ok(response: &serde_json::Value) -> serde_json::Value {
        assert_eq!(response["succeeded"], true, "{response}");
        response["result"].clone()
    }
}
