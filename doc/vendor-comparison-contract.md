# Vendor comparison contract candidate C1

Status: **PROPOSED — NOT FROZEN, NO VENDOR COMPARISON AUTHORIZED**.
This supplies literal candidate criteria before reference results exist. They
are engineering acceptance proposals, not thresholds fitted to observed errors.
Independent review must accept or revise them before the first comparison. A
frozen version must have its own commit hash and review record. Never loosen a
threshold in place after seeing a failure; revise, review, and rerun both sides.

## Mandatory preflight

1. Record licensed Editor 5.3.04 Pro executable/version/hash and supported OS;
   record Native R5 package, numeric Core version, matching headers/library/
   framework hashes, backend, device/driver, and reference exporter. A family
   label or the fact that a binary starts is insufficient.
2. Pin original archive and extracted-file hashes, explicit CAN3 resource GUID
   and CMO3 association, probe ID, all output paths and export settings. No stored
   Windows path searching. Copy originals to read-only evidence inputs; outputs
   use distinct paths. Check original hashes after every workflow.
3. Capture reference commands and actual Editor workflow recording; capture
   Soul2D GUI/MCP/actual ACP-client/Skill transcripts with snapshot reads after
   edits, Undo, Redo, save and reopen. A second process with its own document is
   not evidence of shared history. Record Session/revision identity at every step.
4. Require canonical IDs, topology, units, coordinate transforms and timeline
   mappings before measurement. Never match by display name, nearest geometry,
   or inferred sourceParameter. Keep raw GUID/attribute suffix disagreements.
5. Freeze per-probe settings and the exact measurement implementation. Repeated
   reference-only captures establish whether the proposed limits are measurable;
   they cannot be used to tune limits to Soul2D output. No run starts with an
   unresolved setting or an unlicensed/mismatched reference artifact.

## Sampling, units, reset

- Static parameter probes sample min, default, max, and fractions 0.25, 0.5,
  0.75 of every adjacent key interval; deduplicate identical inputs exactly.
  For coupled parameters use the full Cartesian product of these sets. A large
  product requires a reviewed partition, never silent random subsampling.
- Animation probes include every anchor, every adjacent-anchor midpoint and
  quarter points, scene/track/clip boundaries and boundary ±0.25 source frame,
  negative source frames where stored, explicit offsets, and two loop crossings.
  Record FPS as a rational number; source frame to seconds conversion is explicit.
  UI wall-clock playback is separate from deterministic seek/advance samples.
- Pose/fade/physics order is part of the fixture, not an implicit default.
  Physics-off and physics-on are separate cases. For physics-on reset both
  runtimes, start from the frozen parameter sequence, use exactly 1/120 second
  steps and 240 warm-up steps, then 1,200 observed steps. If the selected vendor
  pipeline cannot provide this schedule, block that probe and review a new one.
- Geometry comparison uses matched drawable/vertex identities transformed into
  the same canvas pixel coordinates at the source's authored canvas scale.
  Report source-space and final-canvas errors separately. Raw forms cannot stand
  in for a rendered, nested-deformer pose.

## Candidate literal pass rules

Every rule applies to each probe/sample, not just an aggregate mean. Non-finite
values, missing fields, mismatched counts/IDs, unknown outcomes, or absent
references fail preflight; they are never coerced to zero.

| Class | Quantity and formula | Candidate limit |
| --- | --- | --- |
| S | IDs, types, ordering, topology, hierarchy, flags, event counts, discrete frame/track membership | Exact equality |
| S | Unedited unknown payloads, texture bytes, container opaque tails in Soul2D same-format save | Byte-identical; edited fields explicitly allowlisted |
| S | Vendor-normalized reserialization | Canonical semantic equality plus retained raw before/after; never claim byte identity where the vendor rewrites data |
| V | Parameter error divided by authored `(max-min)` | Maximum ≤ 0.00001; zero-range values must be exact |
| V | Opacity and normalized color-channel absolute error | Maximum ≤ 0.00001 |
| V | Time error in seconds at deterministic samples | Maximum ≤ 0.000001 |
| V | Explicit seek/play/loop sample frame error | Maximum ≤ 0.0001 source frame; discrete anchor index exact |
| G | Euclidean final-canvas vertex error in pixels | Maximum ≤ 0.01; RMS ≤ 0.002 |
| G | UV coordinate absolute component error | Maximum ≤ 0.000001 |
| G | Rotation error after explicit common-unit conversion | Maximum ≤ 0.0001 degrees; do not normalize away authored turns |
| I | RGBA8 channel absolute difference over every pixel | Maximum ≤ 2 byte levels |
| I | RGBA8 channel RMS over all `4×width×height` channels | ≤ 0.5 byte levels |
| W | Required gesture/command, state transition, Undo/Redo/reopen outcome | Every step succeeds with expected shared state; no numerical waiver |

RGBA8 images use 1024×1024 output, fixed canvas fit with 32-pixel margin, no
per-pose refitting, transparent background, linear texture filtering, no MSAA,
and a pinned identical mask/offscreen resolution on both sides. Record the
actual color-management/framebuffer settings. Compare straight-alpha RGBA8 PNGs
with RGB forced to zero only where **both** alpha values equal zero; also compare
composites over black and white using the same limits. Pin the exact transfer
function and readback alpha conversion before freeze; no auto exposure, image
alignment, edge masks, antialiasing exclusions, or per-image rescaling. If a
vendor cannot expose compatible output settings, the probe is blocked, not PASS.
Editor UI screenshots prove W, not I; I uses a reference render/export, not a
screenshot cropped and resized until it matches.

Lossy video/audio exports require separate codec-specific contracts: decoded
frame timestamps and event timing, frame count, alpha presence, channels/sample
rate, and decoded signal tolerances. They remain **CONTRACT_MISSING**, not covered
by the PNG thresholds. Performance/capacity criteria likewise remain unfrozen.

## Outcomes and aggregation

Each receipt reports contract hash, probe/input hashes, exact command sequence,
all measured maxima/RMS, failure locations, reference/Soul2D artifact hashes,
original-after hashes and outcome. Outcomes: PASS, FAIL, BLOCKED, NOT_RUN,
UNSUPPORTED, CONTRACT_MISSING. Only PASS meets acceptance. No majority vote,
missing-value exclusion or “overall pass” from partial data. All required probes,
four-model cases, conversions and workflow paths must pass to close a phase.

## Freeze checklist

- [ ] Complete per-feature and conversion correspondence reviewed.
- [ ] Legitimate pinned Editor/Core environment and user approval recorded.
- [ ] Transfer function, masks/offscreen settings and coordinate transforms fixed.
- [ ] Lossy export and performance/capacity contracts completed.
- [ ] Independent numeric/methodology review recorded before comparisons.
- [ ] Executable comparator validated on exact-match and injected-error fixtures.
- [ ] Frozen commit/hash recorded; evidence stores that exact version.

This candidate completes none of these gates by itself.
