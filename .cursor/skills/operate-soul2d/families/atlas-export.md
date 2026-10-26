# Atlas and export

Status: run live against the headless host (`atlas_build`, `atlas_pack`, `export_inp`, `export_image`, `export_frames`, `export_gif` (on `soul2d-core --mcp`, decoded with Pillow), `view_screenshot`, `ViewCommand_SaveScreenshot`, overwrite refusal). The video tools and `export` are written from the manifest only (no ffmpeg run).

Write runtime files from the open INP/INX document. Exports change nothing in the session: no history entry, no `modified`, no `revision`
(doc/mcp_api.md "Export Tools" has the path, overwrite and atomic-write rules). Saving the project itself is `save` (session.md); previewing is `render` (render-scene.md).
Native Live2D documents also allow video status/cancel and codec probe/status; rendering exports and command previews require INP/INX.

## Sequence

1. Explore: `atlas_build {}` plans the atlases the INP export would build (`atlasCount`, per-Part `mapping`, `uvs`) without writing; `atlas_pack {size, rectangles}` is the bare packer.
2. Read: `nodes_list {}` for a Camera node (image and frame exports need one); `snapshot.model.animations` for clip names.
3. Act: pick the output. The parent directory must exist; an existing file is kept unless `overwrite:true`. ACP single-file exports wait for [host approval](editor-state.md); the prompt shows the effective filename after extension forcing.
4. Confirm: the reply gives the path and sizes; read the file back (decode the PNG, or open the INP with `open` in a throwaway host) before trusting it.

## Tools

| Tool | Purpose and minimum arguments |
| --- | --- |
| `atlas_build` | `{resolution?, padding?, scale?, nonLinearScaling?, parts?, preview?}` plan only. Pages up to 8192 (Soul2D itself does not draw textures over 16M pixels); `preview: n` adds page n's albedo as a base64 PNG (at most 512 px). Manual places are kept. |
| `atlas_layout_set` / `atlas_layout_clear` | `{parts:[{id, atlas, x, y, scale?}]}` / `{parts?}`. Manual atlas places (model `atlasLayout.parts`), one undo; `atlas_build` and `export_inp` keep them and pack the rest around; overlapping or off-page places are refused at build time. |
| `atlas_pack` | `{size:[w,h], rectangles:[[w,h],...]}` plan only; answers `{placements:[[x,y,w,h]...], occupancy}`. |
| `export_inp` | `{path, atlasResolution?, padding?, scale?, nonLinearScaling?, optimizePruneUnused?, overwrite?}` the `.inp` extension is forced. Answers `{atlases, bytes, prunedNodes, skippedParts, unbakedDeformers}`. |
| `export_image` | `{path, format?: png\|tga\|jpeg, camera?, transparency?, background?:[r,g,b], overwrite?}` answers `{path, width, height}` (the camera viewport). |
| `export_frames` | `{path: *.png\|*.tga, animation?, camera?, loops?, framerate?, overwrite?}` writes `<stem>-N.<ext>`, N from 1, with the clip's image tracks drawn; answers `{frames, first, last, imagesLeftOut?}`. |
| `export_gif` | `{path, animation?, camera?, loops?, framerate?, snap_to_framerate?, transparency?, background?, colors?, repeat?, overwrite?}` writes one animated GIF (`.gif` forced); `colors` 2-256 per frame (one is the transparent colour with `transparency`), `repeat` absent loops forever and 0 plays once; answers `{path, frames, width, height, delay, imagesLeftOut?}` (`delay` in hundredths of a second, rounded, at least 2: a clip over 50 fps is rendered at 50 fps and keeps its length). |
| `export_collect` | `{dir, overwrite?}` copies the open INX and every DepthDraw or animation-sequence source image into `dir` (images under `dir/assets/`, named by relative paths in the copy); answers `{path, files}`. A missing source refuses everything; CMO3/CAN3 are refused. |
| `export_video_start` | `{path, codec? or preset? (mp4, webm, mov), audio? (a sound file, AAC in mp4/mov; without it the clip's unmuted audio tracks), animation?, camera?, framerate?, overwrite?}` answers `{job, path, frames, width, height, fps, audioLeftOut?, imagesLeftOut?}` at once (a preset sets the extension); needs `ffmpeg` on `PATH`. |
| `export_video_status` | `{job}` states `running`, `finished`, `failed`, `cancelled`. |
| `export_video_probe` | `{}` starts the sole background codec probe using the host encoder and fixed `-codecs` arguments. Another running probe is refused; existing five-second timeout and bounded output. No request executable/options or cancellation. |
| `export_video_probe_status` | `{}` returns `idle`, `running`, `finished` with `codecs:[{tag,name}]`, or `failed` with `error`; includes the program captured at start. Same result: `resource://soul2d/export/video-codecs`. |
| `export_video_command` | `{path, codec? or preset?, audio?, animation?, camera?, loops?, framerate?, snap_to_framerate?, transparency?, overwrite?}` as `export_video_start`; returns the host program, argument vector, working directory, output/overwrite, timing/dimensions and omission warnings without execution or writing. Same last successful result: `resource://soul2d/export/video-command`; new/open clears it, failure preserves it. |
| `export_video_cancel` | `{job}` |
| `export_gltf` | `{path, unitsPerMetre?, lossy?, acknowledgeRights?, overwrite?, dryRun?}` write the document as layered planes in glTF 2.0 (`.glb` or `.gltf`): one mesh for each drawn Part in draw order along Z, textures inside, bones as nodes, one-axis parameter keys as morph targets. Answers a loss report `{direction, target, counts, items, truncated, rights, lossy, needs, applied}`. Nothing is written while `needs` is not empty: it names `unitsPerMetre` (canvas units for one metre), `lossy` (weights, masks, blend modes, physics and animations are not written) or `acknowledgeRights` (the document keeps terms under `rights`; show them to the user, do not set the flag on your own). Last applied report: `resource://soul2d/export/gltf-report`. Not run against a glTF validator. |
| `export` | `{}` returns the INP container as a JSON array of byte values: large, prefer `export_inp`. |
| `view_screenshot` / `ViewCommand_SaveScreenshot` | `{path, size?}` / `{filename}` save a PNG of the whole puppet; longest side `size` (default 1024). |

## Gotchas

- Probe and command preview leave document revision/history/modified unchanged. They change host session state and need no file-write consent. Actual video start retains its existing ACP approval.
- Preview requires an existing output parent, protects the open document and refuses Motion3 preview. It can run during an encode; `-n`/`-y`, host program/options, preset and audio match launch planning. The last preview is not a guarantee of later execution or current executable resolution.
- GUI Show applies the local Extra ffmpeg options field through the host setter, just like Run; subsequent agent starts use those options. Passive result polling never writes drafts back. GUI reflects agent-started probes and command previews.

- Verified: `export_image` without a Camera node fails with "No cameras available in the active puppet"; after `node_create {className:"Camera"}` it wrote a 1920x1080 PNG.
- Verified: a second `export_inp` to the same path fails with "export destination already exists"; with `overwrite:true` it replaces the file.
- Verified: `export_frames` with a 3-frame animation wrote `fr-1.png`, `fr-2.png`, `fr-3.png` and answered absolute paths.
- Verified: `atlas_pack` rectangles are `[w,h]` pairs; `[x,y,w,h]` fails with "must contain [width, height]". `size` is `[w,h]` too.
- Verified: `view_screenshot {path, size:128}` wrote a 128x128 PNG; `ViewCommand_SaveScreenshot {filename}` wrote 1024x1024 (the default).
- `export_inp` reports `unbakedDeformers` and `skippedParts`; read both after every export.
