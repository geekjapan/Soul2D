# Viewport capture

## Sub-features

- Capture the rendered scene as PNG bytes in an MCP response.
- Save a PNG of the whole puppet to a file.

## How to get to it (user POV)

`View > Save Screenshot...` writes the puppet to a PNG. The viewport itself shows
the same render live.

## Driving it with njc

```bash
./out/rust/debug/njc --port 18090 tools call ViewCommand_CaptureLiveScreenshot --json '{}'
```

`result.content` holds an item with `type:"image"` and `mimeType:"image/png"`
(base64 in `data`) followed by a text item with the `bounds`, `width` and `height`
used. Check the PNG signature and the size, save the bytes as evidence, then open
the image.

Proof: `prove.py` requires an untextured scene to capture with no painted pixel and
the scene with a textured Part to capture with painted pixels, and keeps the latter
as `viewport.png`.

`ViewCommand_SaveScreenshot` (`filename`, optional `size`) and `render` (`width`,
`height`, `bounds`, optional `path`) write files; this skill has not driven them.

## Gotchas

- The default capture is 640x480 over the scene bounds and does not keep the aspect
  ratio: a round texture comes out as an ellipse. Pass `width`, `height` and
  `bounds` when shape matters.
- `overlayObjects` is refused by the Rust host, so mesh and bounds overlays cannot
  be verified over MCP.
- Rendering is the CPU path. `python3 tools/verify-composite-pixels.py` checks
  Composite fixtures pixel by pixel through the built library.
- A capture shows the scene, never the editor's panels, menus or selection.
- On a native CMO3 document the text item also lists approximation `warnings`.
