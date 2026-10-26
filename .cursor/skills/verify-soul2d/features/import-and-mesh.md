# Image import and automatic mesh

## Sub-features

- Add image files to the open project as one Part each.
- Start a new project from a folder of images.
- Generate a Part's mesh from its texture alpha.

## How to get to it (user POV)

`File > Merge > Merge Image Files...` adds images to the open project;
`File > Import > Import Image Folder...` starts a new one. Both ask for a path.
With a Part selected, the `Auto mesh` section offers `Algorithm`, `Preset` and
`Generate mesh`.

## Driving it with njc

```bash
./out/rust/debug/njc --port 18090 tools call merge_images --json '{"paths":["/tmp/verify-soul2d-run/disc.png"]}'
./out/rust/debug/njc --port 18090 tools call automesh_generate --json '{"id":PART,"algorithm":"grid"}'
./out/rust/debug/njc --port 18090 tools call scene_geometry --json '{}'
```

`merge_images` answers `{"created":[PART],"parent":1}`; the Part is named after the
file. A fresh Part is a quad: `scene_geometry` lists it with 4 vertices and 6
indices. `automesh_generate` answers
`{"changed":true,"id":PART,"triangles":32,"vertices":25}` for a 64-pixel disc with
`grid`, and `scene_geometry` then reports the same vertex count.

Proof: the vertex count from `automesh_generate` read back through
`scene_geometry` after save and reopen, plus a capture with painted pixels
(`prove.py` does this with a PNG it generates, so no fixture is needed).

Algorithms are `grid`, `contour`, `optimum` and `skeleton`; only `grid` has been
driven. `import_folder` (`path`) replaces the Session with a new project.
`import_psd`, `import_kra` and the `merge_psd_*`/`merge_kra_*` pairs need real
layered files, which the project has not used yet.

## Gotchas

- Images must be PNG or TGA.
- Both import tools read paths on the host's file system; use absolute scratch paths.
- `automesh_generate` rejects a Part that has deform bindings, welds or depth data.
- `merge_images` is one undo step; `EditCommand_Undo` removes every Part it added.
- `*.psd` is ignored by Git. Do not commit third-party artwork as a fixture.
