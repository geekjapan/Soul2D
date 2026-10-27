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
| `a_capacity_native_texture_limits_refuse_without_state_changes` | Valid PNGs at 8193x1 and 4097x4096 exercise dimension and decoder-allocation refusal. State/history/source remain unchanged; a normal file still opens and renders afterward. |

The last two tests are ignored in the ordinary parallel suite and run explicitly
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
Many simultaneously keyed parameters, dense multi-axis form grids, larger image
sets and interactive GUI workloads need separate representative measurements.
Target hardware and acceptable interactive time/memory budgets remain unfrozen
in `vendor-comparison-contract.md`. No value in this document substitutes a
chosen user workload or establishes Cubism interoperability.
