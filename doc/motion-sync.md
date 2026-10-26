# Motion sync from WAV speech (ANI26)

Soul2D can bake mouth animation from a WAV file by running the external
[Rhubarb Lip Sync](https://github.com/DanielSWolf/rhubarb-lip-sync) command-line
analyzer and writing its mouth-shape cues onto artist-chosen parameter axes.
This is a deliberately narrower alternative to Cubism's MotionSync: it uses a
different analyzer and a different mouth-shape set, so it is **not** CRI
MotionSync parity.

## Research notes and decision

Sources inspected on 2026-10-15 (primary sources only):

- Cubism Editor manual: [Motion Sync](https://docs.live2d.com/en/cubism-editor-manual/motion-sync/)
  and [MotionSync bake](https://docs.live2d.com/en/cubism-editor-manual/motion-sync-bake/) describe
  the CRI middleware integration, its A/I/U/E/O vowel visemes, and the viseme-to-parameter mapping
  workflow this feature imitates in shape only.
- Rhubarb Lip Sync [v1.14.0](https://github.com/DanielSWolf/rhubarb-lip-sync/releases/tag/v1.14.0)
  (released 2025-04-03, latest tag as of the check), its
  [README](https://github.com/DanielSWolf/rhubarb-lip-sync/blob/v1.14.0/README.adoc) and
  [LICENSE.md](https://github.com/DanielSWolf/rhubarb-lip-sync/blob/v1.14.0/LICENSE.md): MIT for the
  program with permissive licenses for the vendored components. CLI, offline, no network.
- Upstream [issue 140](https://github.com/DanielSWolf/rhubarb-lip-sync/issues/140) reports the
  shipped x86_64 macOS build crashing on Apple Silicon, with native arm64 builds working.

Alternatives considered and rejected:

- **CRI MotionSync itself**: the CRI analyzer is not present, obtained or evaluated here; this
  integration uses Rhubarb and does not implement CRI analysis.
- **Hand-rolled DSP (RMS/envelope or custom phoneme detection)**: unvalidated; an envelope cannot
  reproduce phoneme-based shapes and a custom recognizer would ship research risk, not a feature.
- **Shipping the Rhubarb binary or its vendored speech models**: rejected; Soul2D bundles no
  analyzer binaries, models or sources, and nothing is downloaded at runtime.

Chosen architecture: Soul2D treats Rhubarb as an external tool the same way it treats the video
encoder. The session host resolves an executable once at host creation - `SOUL2D_RHUBARB` or else
`rhubarb` on `PATH` - and a worker thread runs it as a child process with a fixed deadline. No
request field can select the program, its arguments or the output destination. Trial evidence
(`out/verification/ani26-analyzer-*/receipt.md`, in the `ani25-26-core` worktree):

- The official x86_64 1.14.0 macOS zip runs under Rosetta for `--version` and silent input, but on
  the tested voiced WAVs it segfaulted during speech recognition (crash stack ends inside
  PocketSphinx model setup; the exact mechanism was not diagnosed). The same files succeed with a
  locally built native arm64 binary, matching the upstream issue-140 report.
- A native arm64 build of the same v1.14.0 tag (vendored dependencies, Boost 1.85.0 headers,
  googletest supplied offline, `--target rhubarb` only) produced valid JSON cues on synthetic TTS
  speech and a single X cue on silence. No speech-accuracy measurement was made or is claimed.

Mouth shapes are Rhubarb's, not CRI's: A is a closed mouth (P/B/M), B slightly open with teeth
(most consonants, EE), C open (EH/AE), D wide open (AA), E rounded (AO/ER), F puckered (UW/OW/W),
G the F/V shape, H the L shape, and X the rest position. They do not correspond one-to-one to the
CRI A/I/U/E/O set; mappings are artist-defined per parameter.

## How it works (reference)

Four session operations, also exposed as MCP tools:

| Operation | MCP tool | Effect |
| --- | --- | --- |
| `animation.motionsync.start {path, recognizer?}` | `animation_motionsync_start` | Starts an asynchronous Rhubarb analysis of a local WAV. No document mutation. |
| `animation.motionsync.status {job}` | `animation_motionsync_status` | Non-blocking poll: `running`, `finished` (with `duration` and `cues`), `failed` (with `error`), or `cancelled`. Read-only. |
| `animation.motionsync.cancel {job}` | `animation_motionsync_cancel` | Flags cancellation, kills and reaps the analyzer child, cleans its temp files. |
| `animation.motionsync.bake {name, job, audio_index, mappings, from?, to?, extend?}` | `animation_motionsync_bake` | Bakes a finished job's cues into clip keys in one undoable commit. |

`start`:

- `path` is a local RIFF/WAVE file (canonicalized through the same checks as `export.video`'s
  audio; RF64 rejected, 64 MiB cap). The bytes are read once and handed to the worker, so a later
  edit of the file cannot change what the running analysis sees.
- `recognizer` is `phonetic` (default; Rhubarb documents it for non-English input as less precise)
  or `pocketSphinx` (English).
- Request fields `program`, `args`, `output` or `timeout` are refused: the executable and its
  options are host settings, never caller input.
- Only one analysis runs per session; settled jobs (up to four) stay queryable and bakeable,
  including after `new`/`open`, but never cross sessions. Dropping the session reaps children.

`bake`:

- Requires the INX document, a `finished` job, and a clip `name` with an audio track at
  `audio_index` whose canonical path still matches the analyzed source and whose bytes still match
  the length+CRC32 fingerprint taken at `start` (stale-input detection, not authenticity). Muted
  tracks and tracks hidden or locked through their group refuse.
- `mappings` is 1-32 unique `{parameter, axis, shapes}` entries; `shapes` must give a finite value
  inside that parameter axis's range for every one of `A B C D E F G H X`. Groups and non-leaf
  axes refuse.
- Sampling uses the half-open cue interval `[start, end)`: at clip `frame`, the audio time is
  `(frame - audio_track.frame) * timestep`, and times outside every cue map to X.
- Defaults: `from` defaults to the audio track's `frame`. `to` defaults to
  `audio_track.frame + ceil(duration / timestep)`, even when `from` is explicit. The final
  recorded frame holds the rest shape. An explicit smaller range writes only that range.
  `to` past the clip end refuses unless `extend: true`, which grows only the clip length
  to `to + 1` in the same commit.
- Limits checked before any write: 2000 frames per bake and 200000 keys.
- Existing selected lanes must be unambiguous Linear/Forced; missing lanes are created the same
  way. Additive clips, `animationWeight != 1`, freeze, frame-stepping on the clip, and blink or
  exposure lanes feeding a mapped parameter refuse. The staged result is re-sampled per frame and
  verified against the expected values before a single `commit_model`; every refusal or late
  failure leaves the document, history and live preview untouched, and repeating the same bake
  adds no history entry.
- The analyzer child runs with a private temp input file, bounded stdout/stderr (8 MiB / 64 KiB)
  and a fixed 120 s deadline; timeout, cancel or overrun kills and reaps it. Its JSON output is
  strictly validated: contiguous, ordered, finite, non-negative cue intervals covering
  `[0, duration]`, `duration` in `(0, 300]`, known shape codes only, at most 100000 cues.

In the GUI, the timeline gains a **Speech lip sync** panel: pick a clip audio track, choose the
recognizer, press **Analyze WAV** (the window keeps working while it runs; Cancel is available),
then edit parameter-axis mappings - the *Openness preset* fills a leaf parameter's whole range
with `r * range + min` per shape and is an artist convenience, not a CRI preset - and press
**Bake speech keys**. Panel state is session-only; only the baked lanes persist in the document.

## Getting a Rhubarb executable

Soul2D runs `rhubarb` from `PATH` or the executable `SOUL2D_RHUBARB` names; the executable needs its
`res/` resource directory (with `res/sphinx`) next to it, exactly as a Rhubarb download or build
lays it out. Rhubarb is not installed by Soul2D and nothing downloads it at startup.

Official builds are at
<https://github.com/DanielSWolf/rhubarb-lip-sync/releases/tag/v1.14.0> (pin that tag; the macOS zip
is x86_64-only). On Apple Silicon the shipped binary crashed under Rosetta on voiced input in the
trial recorded at `out/verification/ani26-analyzer-20261015-213051/receipt.md`, so build the pinned
v1.14.0 source natively instead. The inputs that produced the working build (receipt
`out/verification/ani26-analyzer-native-20261015-221142/receipt.md`, with URLs and SHA-256
fingerprints) are Rhubarb v1.14.0, Boost 1.85.0 headers and GoogleTest v1.16.0, each unpacked from
its official pinned source archive; the vendored dependencies build with the `rhubarb` target:

```sh
# RHUBARB_SRC: unpacked rhubarb-lip-sync-1.14.0 tree; BOOST_ROOT and GOOGLETEST_SRC: unpacked
# boost-1.85.0 and googletest-1.16.0 trees; RHUBARB_BUILD: an empty build directory of your own.
cmake -S "$RHUBARB_SRC" -B "$RHUBARB_BUILD" \
    -DCMAKE_OSX_ARCHITECTURES=arm64 -DCMAKE_BUILD_TYPE=Release \
    -DBOOST_ROOT="$BOOST_ROOT" -DBoost_NO_SYSTEM_PATHS=ON \
    -DFETCHCONTENT_SOURCE_DIR_GOOGLETEST="$GOOGLETEST_SRC" \
    -DFETCHCONTENT_FULLY_DISCONNECTED=ON
cmake --build "$RHUBARB_BUILD" --target rhubarb --parallel 2
```

Keep the upstream LICENSE notices with any copy you redistribute.

Then, from the repository root:

```sh
SOUL2D_RHUBARB=/absolute/path/to/rhubarb ./out/rust/debug/soul2d-editor   # the GUI
SOUL2D_RHUBARB=/absolute/path/to/rhubarb ./out/rust/debug/soul2d-core --mcp-port 18088  # headless MCP
```

## Verification

Run `python3 tools/verify-animation-physics-bake.py` from the repository root to
check ANI25 through the shipped `njc` client. It starts its own loopback host,
compares baked SimplePhysics keys with a public-operation simulation, and checks
undo, redo, save/reopen, and an atomic refusal. No analyzer is needed. Evidence
stays under `build/verification/ani25-26/run-*`.

`cargo test --offline --locked --manifest-path rust/Cargo.toml --target-dir out/rust
--test motion_sync` covers the worker, parser, refusals and atomicity with a fake analyzer (which
proves the plumbing only, never a real analyzer). `python3 tools/verify-motion-sync.py --rhubarb
/absolute/path/to/rhubarb --wav /absolute/path/to/speech.wav` (or the `SOUL2D_VERIFY_RHUBARB` /
`SOUL2D_VERIFY_WAV` environment variables; `--silence` is optional - a short generated silence is
used otherwise) runs the live proof through the shipped `njc` against a real Rhubarb binary and
keeps its evidence under `build/verification/ani25-26/motionsync-*`. No speech-accuracy measurement
is made or claimed by either path.

Use a voiced recording of 7.5 seconds or shorter for the proof's fixed clip.
The live speech proof checks that cancelling a finished job leaves its result and
the document unchanged. Running cancellation and process reaping use the
stub-analyzer tests, not that live proof.

Both live scripts read `resource://vsoul/snapshot` and require its animation
lanes to match the tool results. Neither script drives a native window. Check
**Bake physics** and **Speech lip sync** separately with the GUI appendix in the
verification Skill. A physics bake must disable live physics after success.
A speech bake must create the mapped lanes without changing that preview toggle.
One GUI Undo must remove the generated keys and preserve existing lanes.
GUI evidence does not establish Cubism or CRI compatibility.

Issue #227 remains open for what this slice does not cover: the real CRI MotionSync backend,
PathDeformer physics baking, and native-document support.
