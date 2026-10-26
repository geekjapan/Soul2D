# Import and merge

Status: run live against the headless host (`import_folder`, `merge_images`, `merge_inp`, `FileCommand_MergeImageFiles`, `FileCommand_MergeINP`, `FileCommand_ImportImageFolder`, missing-file error). PSD and Krita tools are written from the manifest only: no layered file was available that is not third-party.

Two kinds of tool. `import_*` replaces the whole session with a new unsaved project (history empty, no project path). `merge_*` adds to the open
INX document as one undo step. Save with `save` afterwards (session.md).

## Sequence

1. Explore: look at the source before importing. `merge_psd_plan {path}` / `merge_kra_plan {path}` match layers to existing Parts and change nothing;
   for images, list the folder yourself.
2. Act, importing: `import_folder {path}` (every PNG or TGA in the folder becomes a Part named after the file without extension),
   `import_inp {path}`, `import_psd {path}`, `import_kra {path}` (options `keepStructure`, `layerGroupNodeType: Node|Composite|DynamicComposite`).
   These need no open document and discard the current one.
3. Act, merging into the open INX: `merge_images {paths:[...], parent?}`, `merge_inp {path, mergeParameters?, mergeAnimations?}`,
   `merge_psd_apply {path, choices?}` / `merge_kra_apply {path, choices?}` after reading the plan.
4. Confirm: `nodes_list {}` shows the new nodes; `merge_*` and `import_*` answer `{created:[uuid...]}` or a snapshot. Then `history_summary {}` shows one entry
   (`merge.images`, `merge.inp`) that `undo` removes; an import shows an empty history.

## Tools

| Tool | Purpose and minimum arguments |
| --- | --- |
| `import_folder` / `FileCommand_ImportImageFolder` | `{path}` folder of PNG/TGA files to a new project. |
| `import_inp` / `FileCommand_ImportINP` | `{path}` open an INP/INX as a new unsaved project. |
| `import_psd` | `{path, keepStructure?, layerGroupNodeType?}` layered PSD to a new project; a clipped layer gets a Mask binding. `imported.blendModesNotApplied` as for `import_kra`. |
| `import_kra` | `{path, keepStructure?, layerGroupNodeType?}` Krita file to a new project. `imported.skippedLayers {visible, types:[{type,count}], other}` lists the layers not imported (vector, file, adjustment and other node types that are not read); `imported.groupBlendNotApplied {count, groups:[{name,blend}]}` lists groups whose blend mode no node carries; `imported.blendModesNotApplied {count, modes:[{mode,count}], other}` counts layers and groups whose blend mode the engine lacks (imported as Normal) and names up to 16 modes in the source's own words (`other` counts the rest). All of these are absent when empty. |
| `merge_images` / `FileCommand_MergeImageFiles` | `{paths:[...], parent?}` one Part per file under `parent` (default root). The D form takes `paths` plus optional `context:{nodes:[parent]}`: the first node becomes the parent (run: `{paths, context:{nodes:[3]}}` created the Part under node 3). |
| `merge_inp` / `FileCommand_MergeINP` | `{path, mergeParameters?, mergeAnimations?}` the other file's top-level nodes are copied under the root; the reply lists them in `created`. |
| `merge_psd_plan` / `merge_kra_plan` | `{path, renameMapped?, retranslateMapped?}` read only layer-to-Part matching. A Krita plan also has `skippedLayers` when layers were skipped. Both plans have `blendModesNotApplied` when a layer's blend mode is not one the engine has. A PSD plan also has `source` and `missingLayers [{part, path}]` (Parts whose linked layer is gone); `path` may be omitted to read the linked PSD (`meta.psdSource`). |
| `merge_psd_apply` / `merge_kra_apply` | `{path, choices?, renameMapped?, retranslateMapped?}` `choices` override the plan per layer index. PSD only: `path` may be omitted to relink to `meta.psdSource` (set by `import_psd` and by a merge into an unlinked document; `link:true` relinks to `path`); `removed: keep\|hide\|remove` handles Parts whose linked layer is gone by path and by name (only when merging the linked PSD; a Part with children is hidden, not removed); the stored link is absolute; the result adds `missingLayers` and `removed`. |
| `model_template_export` | `{path, name?, description?, overwrite?}` writes the node tree without textures and the parameters to a `.json` model template (`soul2d.model-template.v1`). |
| `model_template_apply` | `{path, parts?, match?: name\|position, parameters?: join\|add, scale?, offset?}` fits a template onto the Parts with one uniform scale and a move, copies its Nodes and deformers with new UUIDs, moves each mapped Part under its template parent, and joins or adds the parameters (Part deform keys re-sampled on the Part's own mesh), in one undo step. Answers `{scale, offset, created, mapped, unmapped, parameters, skippedBindings}`. The template's groups and physics are not copied; a grouped document parameter can be joined. |
| `tool_import_session_data` / `ToolCommand_ImportSessionData` | `{path}` copy Inochi Session tracking bindings from another INP/INX. |

## Gotchas

- Relative paths worked when the host and the caller shared a working directory; the host resolves them itself, so give absolute paths.
- Verified: a missing file fails with "Could not load <name>: No such file or directory" and leaves the document unchanged.
- Verified: `import_folder` clears history and DepthDraw state; `revision` still goes up (it never goes back inside one host process).
- Verified: `merge_images` names a Part after the file including the extension (`disc.png`), `import_folder` without it.
- Verified: `merge_inp` of a file with one Part under the root created one node (`created:[<uuid>]`) directly under the root, with a fresh UUID.
- For one flattened illustration cut into Parts use split.md, not these tools.
