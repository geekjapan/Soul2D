# Textures

Status: run live against the headless host (`texture_get`, `tool_rebleed_textures`, `image_process inspect`, slot out of range). `tool_premultiply_textures` and `texture_set` are written from the manifest and doc/mcp_api.md only.

Textures are stored inside the INX document, referenced by slot number from Parts (`textures:[slot]` on the Part resource). Bring pixels in with import-merge.md or split.md; this map reads
them back and cleans them.

## Sequence

1. Explore: read a Part resource for `textures:[slot, ...]`.
2. Read: `texture_get {slot}` answers `{slot, format: png|tga, width, height, base64}`; decode `base64` to look at the stored image.
3. Act: `texture_export {directory}` writes every slot as `slot-N.png` of the stored pixels and does not change the document. Edit the PNGs, then `texture_replace {directory}` writes the changed slots back as one undo step. `texture_set {slot, path}` replaces one slot from straight alpha. `tool_rebleed_textures {}` or `tool_premultiply_textures {}` process every texture in one undo step. `image_process` works on pixels you pass in and never touches the session.
4. Confirm: `resource://soul2d/textures/{slot}` or `texture_get` again, and `render` (render-scene.md) to see edge halos disappear.

## Tools

| Tool | Purpose and minimum arguments |
| --- | --- |
| `texture_get` | `{slot}` read only. |
| `texture_set` | `{slot, path \| png_base64, premultiplied?, allowResize?}` replace the pixels of an existing slot for every Part that uses it; exactly one image source. INP/INX only, one undo step. |
| `texture_export` | `{directory, overwrite?}` write every slot as slot-N.png of the stored premultiplied pixels into an existing directory. The document does not change. An existing file is refused before any write unless overwrite is true. |
| `texture_replace` | `{directory, allowResize?}` read slot-N.png back into that slot in the slot's own format, without a second premultiply. A missing file is listed in missing and leaves the slot. Changed slots are one undo step. |
| `tool_rebleed_textures` / `ToolCommand_RebleedTexture` | `{}` bleed colour into fully transparent pixels; answers `{changed, textures}`; history label `tool.rebleed_textures`. |
| `tool_premultiply_textures` / `ToolCommand_PremultTexture` | `{}` premultiply every texture by its alpha. Apply once; repeating it darkens edges again. |
| `image_process` | `{method: inspect\|bleed, format: Mono\|Rgb\|Rgba, size:[w,h], pixels:[...], iterations?}` flat 8-bit sample array, row major. `inspect` answers shape and channel info. |

## Gotchas

- Verified: a slot beyond the count fails with "texture slot 9 is out of range (1 textures)", which also tells you how many slots exist.
- Verified: `image_process inspect` of `[255,0,0,255, 0,0,0,0]` with `size:[2,1]` answered `shape:[1,2,4]` (rows, columns, channels).
- Premultiplying is not reversible by a tool; `undo` is the way back.
- Not run: `texture_set` takes straight alpha and premultiplies it unless `premultiplied` is true; an image of another size is refused unless `allowResize` is true; `png_base64` is
  limited to 7 MiB of base64 text, so use `path` for bigger images. The slot must exist; the tool adds none.
- `texture_export` writes the stored premultiplied pixels. `texture_replace` stores the decoded PNG as those pixels and does not premultiply again. A straight-alpha file belongs in `texture_set`. `resource://soul2d/textures/{slot}` returns the same JSON as `texture_get`.
- Atlas packing and size limits for export are in atlas-export.md.
