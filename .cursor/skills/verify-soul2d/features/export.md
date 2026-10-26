# Export

## Sub-features

- Export one rendered image (PNG, TGA, JPEG) of a camera.
- Export an animation as numbered PNG or TGA stills.
- Export an animation as a video through `ffmpeg`, as a background job with progress and cancel.

## How to get to it (user POV)

Open the Tools panel in the editor and use its image export, frame sequence and video sections. The
buttons run the `export.image`, `export.frames` and `export.video.start` session operations; the
video section shows progress and a Cancel button while the job runs. MCP clients call `export_image`,
`export_frames`, `export_video_start`, `export_video_status` and `export_video_cancel`.

## Driving it with njc and cua-driver

Use scratch copies of a project that has a Camera node (and an animation for frames and video):

```bash
./out/rust/debug/njc --port 18088 tools call export_image --json '{"path":"/tmp/verify-soul2d/shot.png"}'
./out/rust/debug/njc --port 18088 tools call export_frames --json '{"path":"/tmp/verify-soul2d/clip.png"}'
./out/rust/debug/njc --port 18088 tools call export_video_start --json '{"path":"/tmp/verify-soul2d/clip.mp4"}'
./out/rust/debug/njc --port 18088 tools call export_video_status --json '{"job":1}'
```

Decode the written PNG and assert the camera's size and non-background pixels; count the `clip-N.png`
files against `frames`. For video, poll `export_video_status` until `state` is `finished`, then probe
the file (`ffprobe` frame count and size). Confirm the session's `revision` and `modified` did not move.
No GUI driving is needed for any of this.

## Gotchas

- INP/INX documents only. Native Live2D documents (CMO3/CAN3) are refused; use `render` for those.
- Video needs `ffmpeg` on `PATH`. If it is missing the job ends `failed` with "cannot start ffmpeg".
- Existing files are kept unless `overwrite` is true, and the open project file is always refused.
- One video job runs at a time; `export_frames` blocks until every frame is written.
- A cancelled video keeps its partial file.
