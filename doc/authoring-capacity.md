# Authoring capacity measurements

The capacity workflow measures synthetic authoring regression processes. Its
results describe the tested workload on the recorded runner, not an unlimited
document size, interactive latency guarantee or vendor-editor comparison.
No defensive product limit is increased for these workloads.

## Workloads

Each workload constructs an owned synthetic CAFF file and checks successful
operations and values. The fixtures use generated XML and PNGs; no proprietary
model or texture is required or uploaded.

| Test | Work performed |
| --- | --- |
| `a_pro_size_cmo3_opens_edits_saves_and_reopens` | 2,000 added ArtMeshes, 1,000 added parameters and a 100x100 warp; vertex edit, save and reopened model equality |
| `a_pro_size_cmo3_with_many_textures_and_deformers_opens_edits_saves_and_reopens` | 1,000 added textures/ArtMeshes, 500 warps and 500 rotations; both deformer edits and save/reopen equality. Images are 1x1 and establish object-count capacity only. |
| `a_pro_size_can3_curve_opens_edits_saves_and_reopens` | One 20,000-key curve; key edit, save and reopened model equality |
| `a_pro_size_cmo3_large_textures_render_and_roundtrip` | Four 2048x2048 RGBA textures, 64 MiB decoded pixels; actual rendering with a distinct checked colour in each region, opacity edit, undo/redo, save/reopen, unchanged PNG entries and source bytes |
| `a_pro_size_cmo3_keyed_parameters_and_dense_forms_roundtrip` | 512 meshes with two independent three-key axes each, plus one mesh with three nine-key axes. All 1,027 parameters are sampled together across 5,337 forms; checked interpolated positions/weights, two vertex edits, undo/redo, stale-edit refusal, new-session save/reopen and unchanged image/opaque data. |
| `a_capacity_native_texture_limits_refuse_without_state_changes` | Valid PNGs at 8193x1 and 4097x4096 exercise dimension and decoder-allocation refusal. State/history/source remain unchanged; a normal file still opens and renders afterward. |

The last three tests are ignored in the ordinary parallel suite and run explicitly
and sequentially in capacity CI. Large images are decoded during rendering;
opening a file or counting its texture records alone would not prove that work.

## Reproduction and evidence

Run the **Authoring capacity** GitHub Actions workflow with `workflow_dispatch`,
or `gh workflow run capacity.yml --repo geekjapan/Soul2D --ref main`.
The matrix is Linux x64, Windows x64 and macOS ARM64. Intel macOS and Win32 are
outside the target set. Heavy measurement is deliberately separate from normal
push CI.

The workflow builds `rust/tests/core.rs` with locked dependencies and the release
profile before measurement. The measurement tool checks the executable's matching
Cargo JSON artifact and requires optimization level 3 without debug assertions.
It launches one exact test in a fresh process, with one test thread, five times.
Cases alternate between rounds. An exit failure, timeout, missing peak counter
or zero selected tests fails the workflow. The measurement self-check verifies
independent child high-water values, a failed exit and a killed timeout.

Artifacts contain raw logs, commands, exit codes, repeated wall times and memory
high-water values, median/min/max, commit and executable/source/lock hashes,
Rust/Cargo versions, runner image, CPU model, logical CPUs, physical RAM and load
average where the OS provides one. The Cargo build receipt is retained separately.
Linux/macOS use `wait4` child resource usage. Windows uses the owned process
handle's `GetProcessMemoryInfo` peak working set. These OS counters differ; do
not rank platforms by treating them as identical memory measurements.

Wall time includes child startup, fixture construction, operations, assertions
and cleanup. Peak memory likewise includes the test host, source construction
and assertion snapshots. Printed phase timings start after construction and
cover only the phases named in each log. Neither is GUI-only or product-only
memory. Compilation, the Python measurement driver and other matrix jobs are
outside the measured child. No speedup, bottleneck or recommended latency claim
is made from these results.

## Completed measurements on 2026-10-27

[Run 40439454235](https://github.com/geekjapan/Soul2D/actions/runs/40439454235)
at `9b689c8a0d853505c5653885aeaf3f80034793f9` completed the original five
workloads on all three targets, with five fresh processes per case and target.
All 75 processes passed their assertions. The keyed-parameter workload was added
later and is not covered by this receipt.

| Runner | CPU / logical CPUs | Physical RAM | Image version |
| --- | --- | --- | --- |
| Linux x64, Ubuntu 24.04 | AMD EPYC 7763 / 4 | 15.615 GiB | `ubuntu24 / 20261023.334.3` |
| Windows x64, Server 2025 | AMD EPYC 9V45 / 4 | 15.995 GiB | `win25-vs2026 / 20261021.278.1` |
| macOS ARM64, 15.7.9 | Apple M1 Virtual / 3 | 7 GiB | `macos15 / 20260907.0337.1` |

Each cell below gives the median and `[min, max]` for five runs. MiB means
1,048,576 bytes. Memory is the OS counter described above, including fixture
construction and assertions. These are independent host observations, not a
platform ranking. macOS load averages at startup were 1.562, 2.148 and 4.305;
Linux values were 1.000, 1.008 and 0.958. Windows does not supply this counter.

| Workload | Linux seconds / MiB | Windows seconds / MiB | macOS seconds / MiB |
| --- | --- | --- | --- |
| 2,000 added meshes, 1,000 added parameters, 100x100 warp | 2.113 [2.094, 2.137] / 385.145 [385.137, 385.539] | 2.253 [2.184, 2.414] / 355.051 [354.816, 355.820] | 1.342 [1.214, 1.786] / 415.953 [401.750, 421.188] |
| 1,000 added textures/meshes, 500 warps, 500 rotations | 3.264 [3.259, 3.285] / 502.051 [501.895, 502.199] | 3.552 [3.329, 3.649] / 449.523 [449.395, 449.992] | 2.124 [2.014, 2.542] / 513.922 [509.359, 531.078] |
| CAN3 20,000-key curve | 5.764 [5.700, 5.823] / 910.527 [906.785, 911.742] | 6.764 [6.581, 6.779] / 835.410 [835.090, 835.574] | 4.248 [3.344, 4.871] / 961.266 [922.797, 969.734] |
| Four 2048x2048 RGBA textures | 0.383 [0.382, 0.384] / 91.363 [91.305, 91.590] | 0.351 [0.348, 0.358] / 74.941 [74.859, 75.055] | 0.353 [0.304, 0.398] / 78.703 [76.172, 79.688] |
| Texture refusal and recovery | 0.041 [0.041, 0.041] / 75.555 [75.320, 75.691] | 0.039 [0.037, 0.040] / 72.605 [72.586, 72.641] | 0.108 [0.065, 0.199] / 73.812 [72.734, 74.703] |

All targets used Rust/Cargo 1.99.1. Independent artifact readback checked all
logs, exact test commands, summary arithmetic, source/lock hashes and optimized
Cargo receipts. The executable SHA-256 is recorded, but executable bytes are not
retained for independent hash recomputation. Raw workflow artifacts expire after
14 days. No absolute wall-time or RSS acceptance threshold was chosen from these
observations.

## Defensive boundaries

| Boundary | Current behavior and coverage |
| --- | --- |
| CAFF input | 256 MiB maximum input; `CaffDocument::parse` refuses larger input before decoding |
| CAFF entries | At most 100,000; count/range/duplicate/truncation tests in `caff::tests` |
| Expanded XML | At most 64 MiB; ZIP size/inflate/CRC checks in `caff::tests` |
| XML nodes | CMO3 authoring 1,000,000; CAN3 2,000,000. This is a parser-node budget, not object count. |
| Projected scalar/text data | 5,000,000 scalar budget and 64 MiB copied-text budget; injected small-limit and shared-alias charging tests |
| Native PNG decoder | At most 8192 per edge and 64 MiB decoder allocation; explicit valid-PNG refusals above |
| Renderer | 16x1024x1024 pixels per texture/output, 100x1024x1024 raster work and 640 MiB accounted active memory |

Renderer accounting is not a whole-process RSS cap. Native preprocessing, CAFF
payloads, XML, document/history JSON and other allocations are outside it.
An OS OOM termination is a failure, not graceful refusal or a supported boundary.
The capacity workflow retains failure logs instead of labelling such cases PASS.

## Remaining acceptance

Results must be read from a completed run at the reviewed commit. The successful
tested sizes establish a synthetic envelope, not the maximum possible document.
The keyed-parameter and dense-grid workload, larger image sets and interactive
GUI workloads need separate representative measurements.
Target hardware and acceptable interactive time/memory budgets remain unfrozen
in `vendor-comparison-contract.md`. No value in this document substitutes a
chosen user workload or establishes Cubism interoperability.
