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

[Run 40444207528](https://github.com/geekjapan/Soul2D/actions/runs/40444207528)
at `b0408afd39e547f43d2bf334b9381d4bfdea47f2` completed all six workloads on
all three targets, with five fresh processes per case and target. All 90 processes
passed their assertions. The earlier [75-process run](https://github.com/geekjapan/Soul2D/actions/runs/40439454235)
at `9b689c8` covered the original five workloads; its measurements are separate.

| Runner | CPU / logical CPUs | Physical RAM | Image version |
| --- | --- | --- | --- |
| Linux x64, Ubuntu 24.04 | AMD EPYC 7763 64-Core Processor / 4 | 15.619 GiB | `ubuntu24 / 20261023.334.3` |
| Windows x64, Server 2025 | AMD EPYC 7763 64-Core Processor / 4 | 15.995 GiB | `win25-vs2026 / 20261021.278.1` |
| macOS ARM64, 15.7.9 | Apple M1 (Virtual) / 3 | 7.000 GiB | `macos15 / 20260907.0337.1` |

Each cell below gives the median and `[min, max]` for five runs. MiB means
1,048,576 bytes. Memory is the OS counter described above, including fixture
construction and assertions. These are independent host observations, not a
platform ranking. Startup load averages were 1.000, 1.004 and 0.957 on Linux,
and 3.551, 2.539 and 5.395 on macOS. Windows does not supply this counter.

| Workload | Linux seconds / MiB | Windows seconds / MiB | macOS seconds / MiB |
| --- | --- | --- | --- |
| 2,000 added meshes, 1,000 added parameters, 100x100 warp | 2.031 [2.011, 2.051] / 385.320 [385.133, 385.457] | 3.014 [2.997, 3.569] / 355.383 [355.027, 355.793] | 1.261 [1.162, 1.843] / 408.547 [385.156, 424.172] |
| 1,000 added textures/meshes, 500 warps, 500 rotations | 3.177 [3.157, 3.221] / 502.113 [501.734, 503.102] | 4.587 [4.531, 4.638] / 449.605 [449.254, 449.934] | 2.040 [1.718, 2.746] / 514.203 [507.656, 525.719] |
| CAN3 20,000-key curve | 5.670 [5.594, 5.750] / 910.594 [910.266, 913.012] | 8.230 [8.201, 8.354] / 834.832 [834.762, 835.250] | 3.403 [3.083, 3.774] / 917.141 [896.344, 957.328] |
| Four 2048x2048 RGBA textures | 0.382 [0.382, 0.403] / 91.582 [91.328, 91.598] | 0.517 [0.504, 0.523] / 74.918 [74.805, 75.008] | 0.362 [0.300, 0.378] / 78.297 [77.359, 79.312] |
| 1,027 keyed parameters, 5,337 forms | 7.790 [7.724, 7.855] / 603.473 [601.750, 603.801] | 12.875 [12.808, 12.996] / 559.078 [558.645, 559.691] | 4.993 [4.828, 5.469] / 703.500 [694.297, 723.922] |
| Texture refusal and recovery | 0.041 [0.041, 0.041] / 75.578 [75.250, 75.738] | 0.049 [0.046, 0.062] / 72.645 [72.613, 72.656] | 0.067 [0.050, 0.164] / 74.500 [73.609, 74.859] |

All targets used Rust/Cargo 1.99.1. Independent artifact readback checked all
logs, exact test commands, summary arithmetic, source/lock hashes against the
pinned commit and optimized Cargo receipts. Each job log also confirms the
child-memory/failure/timeout self-check ran. The executable SHA-256 is recorded,
but executable bytes are not retained for independent hash recomputation.
Raw workflow artifacts expire after 14 days. No absolute wall-time or RSS
acceptance threshold was chosen from these observations.

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
The keyed-parameter and dense-grid workload is covered by the 90-process receipt.
Representative interactive GUI workloads still need separate measurements.
Target hardware and acceptable interactive time/memory budgets remain unfrozen
in `vendor-comparison-contract.md`. No value in this document substitutes a
chosen user workload or establishes Cubism interoperability.
