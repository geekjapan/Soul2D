#!/usr/bin/env python3
import argparse
import copy
import hashlib
import html
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import traceback

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("scene_track_proof", ROOT / "tools/verify-can3-tracks.py")
TRACKS = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = TRACKS
SPEC.loader.exec_module(TRACKS)
CURVES, KEYS = TRACKS.CURVES, TRACKS.KEYS
TOOL = "live2d_animation_scene_delete"
SCHEMA = {
    "type": "object",
    "properties": {"sceneGuid": {"type": "string"}, "expectedRevision": {"type": "integer", "minimum": 0}},
    "required": ["sceneGuid", "expectedRevision"],
    "additionalProperties": False,
}


def proof_helpers():
    return TRACKS.proof_helpers() | {Path(__file__).resolve()}


def descend(node, *names):
    for name in names:
        node = TRACKS.named(node, name)
    return node


def fixture_xml():
    original = CURVES.FIXTURE.read_bytes()
    nodes = TRACKS.xml_nodes(original)
    definitions = TRACKS.definitions(nodes)
    identities = {"scene": "scene-kept", "track": "track-kept", "track2": "track2-kept",
                  "attr": "attr-kept", "attr-t2": "attr-t2-kept"}
    guids = {"scene-guid": "scene-kept-guid", "track-guid": "track-kept-guid", "track2-guid": "track2-kept-guid"}
    additions = []
    for identity in identities:
        node = definitions[identity]
        raw = original[node.start:node.end]
        for old, new in identities.items():
            for attribute in ("xs.id", "xs.ref"):
                raw = raw.replace(f'{attribute}="{old}"'.encode(), f'{attribute}="{new}"'.encode())
        for old, new in guids.items():
            raw = raw.replace(f'uuid="{old}"'.encode(), f'uuid="{new}"'.encode())
        raw = raw.replace(b">Scene</s>", ">残す &amp; &lt;Scene&gt;</s>".encode())
        additions.append(raw)
    scenes = TRACKS.named(definitions["animation"], "_scenes")
    shared = next(n for n in nodes if n.tag == "shared")
    return TRACKS.patch(original, [
        (scenes.start, scenes.open_end, b'<carray_list xs.n="_scenes" discount="999" count=\'0002\'>'),
        (scenes.end - len(b"</carray_list>"), scenes.end - len(b"</carray_list>"),
         b'<CSceneSource xs.ref="scene-kept"/>'),
        (shared.end - len(b"</shared>"), shared.end - len(b"</shared>"), b"".join(additions)),
    ])


def deleted_scene_xml(original, guid):
    nodes = TRACKS.xml_nodes(original)
    storage = TRACKS.definitions(nodes)
    scenes = [n for n in storage.values() if n.tag == "CSceneSource"
              and TRACKS.named(n, "guid").attrs.get("uuid") == guid]
    assert len(scenes) == 1, guid
    scene = scenes[0]
    owners = [n for n in nodes if n.tag == "CSceneSource" and n.attrs == {"xs.ref": scene.attrs["xs.id"]}]
    assert len(owners) == 1 and owners[0].parent.attrs.get("xs.n") == "_scenes"
    owner, removed = owners[0], [scene]
    for member in descend(scene, "trackSourceSet", "_sources").children:
        assert member.tag == TRACKS.TRACK and member.attrs.keys() == {"xs.ref"}
        track = storage[member.attrs["xs.ref"]]
        assert track.tag == TRACKS.TRACK
        assert descend(track, "_sceneSource").attrs["xs.ref"] == scene.attrs["xs.id"]
        removed.append(track)
        for entry in descend(track, "keyParamEffect", "attrList").children:
            attribute = storage[entry.attrs["xs.ref"]]
            if TRACKS.named(attribute, "id").attrs.get("idstr", "").startswith("live2dParam_"):
                assert entry.attrs == {"xs.ref": attribute.attrs["xs.id"]}
                assert TRACKS.named(attribute, "track").attrs["xs.ref"] == track.attrs["xs.id"]
                removed.append(attribute)
    start, end = TRACKS.count_span(original, owner.parent)
    count = int(original[start:end])
    assert count == len(owner.parent.children)
    return TRACKS.patch(original, [(n.start, n.end, b"") for n in [owner, *removed]]
                        + [(start, end, TRACKS.count_value(original[start:end], count - 1))])


def require_delete(original, saved, guid):
    assert saved == deleted_scene_xml(original, guid), "scene deletion changed non-owned spans/count"


def opaque_fixture(original, scene_id):
    nodes = TRACKS.xml_nodes(original)
    sources = descend(TRACKS.definitions(nodes)[scene_id], "trackSourceSet", "_sources")
    start, end = TRACKS.count_span(original, sources)
    at = sources.end - len(b"</carray_list>")
    shared_at = next(n for n in nodes if n.tag == "shared").end - len(b"</shared>")
    return TRACKS.patch(original, [
        (start, end, TRACKS.count_value(original[start:end], int(original[start:end]) + 1)),
        (at, at, b'<OpaqueTrack xs.ref="opaque-track"/>'),
        (shared_at, shared_at, b'<OpaqueTrack xs.id="opaque-track"><s xs.n="future">keep</s></OpaqueTrack>'),
    ])


def retained_shapes(original, scene_id):
    nodes = TRACKS.xml_nodes(original)
    storage = TRACKS.definitions(nodes)
    scene = storage[scene_id]
    owners = [n for n in nodes if n.tag == "CSceneSource" and n.attrs == {"xs.ref": scene_id}]
    assert len(owners) == 1
    guid = TRACKS.named(scene, "guid")
    guid_id = scene_id + "-guid-storage"
    assert guid_id not in storage
    shared_at = next(n for n in nodes if n.tag == "shared").end - len(b"</shared>")
    return {
        "inline": TRACKS.patch(original, [
            (owners[0].start, owners[0].end, original[scene.start:scene.end]),
            (scene.start, scene.end, b""),
        ]),
        "aliased-guid": TRACKS.patch(original, [
            (guid.start, guid.end, f'<{guid.tag} xs.n="guid" xs.ref="{guid_id}"/>'.encode()),
            (shared_at, shared_at, f'<{guid.tag} xs.id="{guid_id}" uuid="{guid.attrs["uuid"]}"/>'.encode()),
        ]),
    }


def restore_mixed_scalars(original, saved):
    old_defs = TRACKS.definitions(TRACKS.xml_nodes(original))
    new_defs = TRACKS.definitions(TRACKS.xml_nodes(saved))
    leaves = []
    for path, value in [(("sceneName",), "変更 & <Scene>"), (("movieInfo", "fps"), 30),
                        (("movieInfo", "duration"), 90)]:
        leaves.append((descend(old_defs["scene-kept"], *path), descend(new_defs["scene-kept"], *path), value))
    old_curve, new_curve = old_defs["attr-kept"], new_defs["attr-kept"]
    for field, value in [("curMin", 0.375), ("curMax", 1)]:
        leaves.append((descend(old_curve, "valueData", field), descend(new_curve, "valueData", field), value))
    old_point = descend(old_curve, "valueData", "points").children[0]
    new_point = descend(new_curve, "valueData", "points").children[0]
    leaves.append((descend(old_point, "anchor", "doubleValue"), descend(new_point, "anchor", "doubleValue"), 0.375))
    changes = []
    for old, new, value in leaves:
        old_end = old.end - len(f"</{old.tag}>".encode())
        new_end = new.end - len(f"</{new.tag}>".encode())
        raw = saved[new.open_end:new_end]
        assert (html.unescape(raw.decode()) if isinstance(value, str) else float(raw)) == value
        changes.append((new.open_end, new_end, original[old.open_end:old_end]))
    return TRACKS.patch(saved, changes)


def self_test():
    original = fixture_xml()
    deleted = deleted_scene_xml(original, "scene-guid")
    require_delete(original, deleted, "scene-guid")
    old_defs = TRACKS.definitions(TRACKS.xml_nodes(original))
    new_defs = TRACKS.definitions(TRACKS.xml_nodes(deleted))
    assert old_defs.keys() - new_defs.keys() == {"scene", "track", "track2", "attr", "attr-t2"}
    assert b'xs.n="_scenes" discount="999" count=\'0001\'' in deleted
    assert ">残す &amp; &lt;Scene&gt;".encode() in deleted
    for bad in [
        deleted.replace(b"count='0001'", b"count='1'", 1),
        deleted.replace(b"count='0001'", b"count='0002'", 1),
        deleted.replace(b">Model</s>", b">Wrong</s>", 1),
        deleted.replace(b'xs.id="attr-opaque"', b'xs.id="wrong-opaque"', 1),
        deleted.replace(b"</shared>", original[old_defs["scene"].start:old_defs["scene"].end] + b"</shared>"),
    ]:
        assert bad != deleted
        TRACKS.must_reject(require_delete, original, bad, "scene-guid")
    empty = deleted_scene_xml(deleted, "scene-kept-guid")
    assert b'xs.n="_scenes" discount="999" count=\'0000\'' in empty
    assert "resource" in TRACKS.definitions(TRACKS.xml_nodes(empty))
    mixed = deleted
    for old, new in [
        (">残す &amp; &lt;Scene&gt;</s>".encode(), ">変更 &amp; &lt;Scene&gt;</s>".encode()),
        (b'xs.n="fps">60</f>', b'xs.n="fps">30</f>'),
        (b'xs.n="duration">120</i>', b'xs.n="duration">90</i>'),
        (b'xs.n="curMin">0.0000</d>', b'xs.n="curMin">0.375</d>'),
        (b'xs.n="curMax">1.0000</d>', b'xs.n="curMax">1</d>'),
        (b'xs.n="anchor"><i xs.n="pos">0</i><d xs.n="doubleValue">0</d>',
         b'xs.n="anchor"><i xs.n="pos">0</i><d xs.n="doubleValue">0.375</d>'),
    ]:
        assert old in mixed
        mixed = mixed.replace(old, new, 1)
    assert restore_mixed_scalars(deleted, mixed) == deleted
    TRACKS.must_reject(restore_mixed_scalars, deleted,
                      mixed.replace(b'xs.n="fps">30</f>', b'xs.n="fps">31</f>', 1))
    normalized_bad = restore_mixed_scalars(deleted, mixed.replace(b">Model</s>", b">Wrong</s>", 1))
    TRACKS.must_reject(TRACKS.require_add, deleted, normalized_bad, {}, [])
    opaque = opaque_fixture(original, "scene-kept")
    opaque_deleted = deleted_scene_xml(opaque, "scene-guid")
    assert b'<OpaqueTrack xs.ref="opaque-track"/>' in opaque_deleted
    assert b'<OpaqueTrack xs.id="opaque-track">' in opaque_deleted
    for label, data in retained_shapes(original, "scene-kept").items():
        removed = deleted_scene_xml(data, "scene-guid")
        kept = TRACKS.definitions(TRACKS.xml_nodes(removed))["scene-kept"]
        if label == "inline":
            assert kept.parent.attrs.get("xs.n") == "_scenes"
        else:
            assert TRACKS.named(kept, "guid").attrs["xs.ref"] == "scene-kept-guid-storage"
    assert ROOT / "tools/verify-rust-core.py" in proof_helpers()


def scene_list(host, label, model):
    actual = host.call(label, "live2d_animation_list", {})["scenes"]
    assert actual == [{k: s[k] for k in ("guid", "name", "fps", "durationFrames")} for s in model["scenes"]]


def delete(host, label, guid):
    return KEYS.edit(host, label, TOOL, sceneGuid=guid)


def add_scene(host, label, name="New & <Scene>", duration=120):
    reply, model = KEYS.edit(host, label, "live2d_animation_scene_add",
                             name=name, fps=30, durationFrames=duration)
    guid = reply["sceneGuid"]
    assert model["scenes"][-1] == {"guid": guid, "name": name, "fps": 30, "durationFrames": duration, "tracks": []}
    return guid, model


def undo_redo(host, label, before, after):
    snap, _ = KEYS.state(host, label)
    position = snap["history"]["position"]
    host.call(label + "-undo", "history_goto", {"index": position - 1, "expectedRevision": snap["revision"]})
    undone, model = KEYS.state(host, label + "-undone")
    assert model == before and undone["revision"] == snap["revision"] + 1
    host.call(label + "-redo", "history_goto", {"index": position, "expectedRevision": undone["revision"]})
    redone, model = KEYS.state(host, label + "-redone")
    assert model == after and redone["revision"] == undone["revision"] + 1


def unsafe_variants(original):
    nodes = TRACKS.xml_nodes(original)
    storage = TRACKS.definitions(nodes)
    scene = storage["scene"]
    shared_at = next(n for n in nodes if n.tag == "shared").end - len(b"</shared>")
    variants = []
    for identity, tag in [("scene", "CSceneSource"), ("track", TRACKS.TRACK), ("attr", TRACKS.ATTR)]:
        variants.append(("outside-" + identity, TRACKS.patch(original, [
            (shared_at, shared_at, f'<Opaque xs.id="outside"><{tag} xs.ref="{identity}"/></Opaque>'.encode())
        ]), ("outside", "referenced", "owned", "alias")))
    owner = next(n for n in nodes if n.tag == "CSceneSource" and n.attrs == {"xs.ref": "scene"})
    variants.append(("nonbare-owner", TRACKS.patch(original, [
        (owner.start, owner.end, b'<CSceneSource xs.ref="scene" future="keep"/>')
    ]), ("bare", "owner", "reference", "unsupported")))
    variants.append(("nested-id", TRACKS.patch(original, [
        (scene.open_end, scene.open_end, b'<Opaque xs.id="nested-scene"/>')
    ]), ("nested", "xs.id", "owned", "unsupported")))
    for field, owner_node in [("_scenes", storage["animation"]), ("trackSourceSet", scene), ("sceneName", scene)]:
        node = TRACKS.named(owner_node, field)
        raw = original[node.start:node.end].replace(f'xs.n="{field}"'.encode(), b'xs.id="shared-field"', 1)
        reference = f'<{node.tag} xs.n="{field}" xs.ref="shared-field"/>'.encode()
        variants.append(("shared-" + field, TRACKS.patch(original, [
            (node.start, node.end, reference), (shared_at, shared_at, raw)
        ]), ("shared", "inline", "owned", "alias", field)))
    source_set = TRACKS.named(scene, "trackSourceSet")
    raw = original[source_set.start:source_set.end]
    variants.append(("inherited-set", TRACKS.patch(original, [
        (source_set.start, source_set.end, b'<CSceneSource xs.n="super" xs.ref="scene-super"/>'),
        (shared_at, shared_at, b'<CSceneSource xs.id="scene-super">' + raw + b"</CSceneSource>"),
    ]), ("super", "inherited", "inline", "trackSourceSet", "unsupported")))
    variants.append(("opaque-track", opaque_fixture(original, "scene"),
                     ("unsupported", "model", "opaque", "track")))
    backlinked = opaque_fixture(original, "scene").replace(
        b">keep</s></OpaqueTrack>",
        b'>keep</s><CSceneSource xs.n="_sceneSource" xs.ref="scene"/></OpaqueTrack>')
    variants.append(("opaque-track-backlink", backlinked, ("unsupported", "opaque", "track", "outside", "referenced")))
    return variants


def verify(host, run, result):
    manifest = {t["id"]: t for t in json.loads((ROOT / "doc/commands.manifest.json").read_text())["commands"]}
    tools = {t["name"]: t for t in host.tools()}
    assert tools[TOOL]["inputSchema"] == manifest[TOOL]["inputSchema"] == SCHEMA
    assert tools[TOOL]["description"] == manifest[TOOL]["description"]
    result["checks"].append("literal-live-and-manifest-delete-schema")
    xml = fixture_xml()
    original = CURVES.caff_pack(xml)
    seed, cmo3 = run / "scenes.can3", run / "source.cmo3"
    seed.write_bytes(original)
    cmo3.write_bytes(CURVES.cmo3_bytes())
    _, initial = TRACKS.reopen(host, "seed", seed)
    assert [s["guid"] for s in initial["scenes"]] == ["scene-guid", "scene-kept-guid"]
    assert [t["guid"] for t in initial["scenes"][1]["tracks"]] == ["track-kept-guid", "track2-kept-guid"]
    expected = copy.deepcopy(initial)
    expected["scenes"].pop(0)
    _, removed = delete(host, "delete-first", "scene-guid")
    assert removed == expected
    scene_list(host, "list-after-delete", expected)
    TRACKS.refuse(host, "deleted-selection", "live2d_animation_scene_set",
                  {"sceneGuid": "scene-guid", "name": "Must not apply"}, ("sceneGuid", "not found"))
    TRACKS.refuse(host, "deleted-again", TOOL, {"sceneGuid": "scene-guid"}, ("sceneGuid", "not found"))
    undo_redo(host, "delete-history", initial, expected)
    saved = CURVES.save_and_reopen(host, "deleted-first", run / "deleted-first.can3", expected, original)
    require_delete(xml, CURVES.decode_entries(saved)[1], "scene-guid")
    _, empty = delete(host, "delete-last-after-reopen", "scene-kept-guid")
    assert empty == {**expected, "scenes": []}
    empty_bytes = CURVES.save_and_reopen(host, "empty", run / "empty.can3", empty, saved)
    empty_xml = CURVES.decode_entries(empty_bytes)[1]
    require_delete(CURVES.decode_entries(saved)[1], empty_xml, "scene-kept-guid")
    scene_list(host, "list-empty", empty)
    new_scene, _ = add_scene(host, "after-empty-scene")
    track, _ = TRACKS.add_track(host, "after-empty-track", scene_guid=new_scene)
    populated = TRACKS.populate(host, "after-empty-fill", new_scene, track, cmo3)
    grown = CURVES.save_and_reopen(host, "after-empty", run / "after-empty.can3", populated, empty_bytes)
    TRACKS.require_add(empty_xml, CURVES.decode_entries(grown)[1],
                      {"CSceneSource": 1, TRACKS.TRACK: 1, TRACKS.ATTR: 1}, [("animation", "_scenes", 1)])
    _, empty_again = delete(host, "delete-written-factory-scene", new_scene)
    assert empty_again == empty
    shrunk = CURVES.save_and_reopen(host, "factory-removed", run / "factory-removed.can3", empty_again, grown)
    require_delete(CURVES.decode_entries(grown)[1], CURVES.decode_entries(shrunk)[1], new_scene)
    result["checks"].append("owned-original-and-written-factory-scene-delete-empty-list-history-reopen")

    TRACKS.reopen(host, "mixed-seed", seed)
    delete(host, "mixed-delete-first", "scene-guid")
    _, model = KEYS.edit(host, "mixed-settings", "live2d_animation_scene_set",
                         sceneGuid="scene-kept-guid", name="変更 & <Scene>", fps=30, durationFrames=90)
    _, model = KEYS.edit(host, "mixed-key", "live2d_animation_key_set", sceneGuid="scene-kept-guid",
                         trackGuid="track-kept-guid", curveId="live2dParam_ParamTest", anchorFrame=0, value=0.375)
    mixed = copy.deepcopy(expected)
    mixed["scenes"][0].update(name="変更 & <Scene>", fps=30, durationFrames=90)
    mixed["scenes"][0]["tracks"][0]["parameterCurves"][0]["points"][0]["anchor"]["value"] = 0.375
    assert model == mixed
    track, _ = TRACKS.add_track(host, "mixed-track", scene_guid="scene-kept-guid", duration=90)
    model = TRACKS.populate(host, "mixed-fill", "scene-kept-guid", track, cmo3)
    mixed["scenes"][0]["tracks"].append(copy.deepcopy(TRACKS.find_track(model, "scene-kept-guid", track)))
    assert model == mixed
    persisted = CURVES.save_and_reopen(host, "mixed", run / "mixed.can3", mixed, original)
    deleted_xml = deleted_scene_xml(xml, "scene-guid")
    normalized = restore_mixed_scalars(deleted_xml, CURVES.decode_entries(persisted)[1])
    TRACKS.require_add(deleted_xml, normalized, {TRACKS.TRACK: 1, TRACKS.ATTR: 1},
                      [("scene-kept", "_sources", 1)])
    scene_list(host, "list-mixed", mixed)
    result["checks"].append("delete-first-retained-guid-settings-keys-new-track-curves-before-save-exact-xml")

    TRACKS.reopen(host, "pending-seed", seed)
    KEYS.edit(host, "pending-key", "live2d_animation_key_set", sceneGuid="scene-guid",
              trackGuid="track-guid", curveId="live2dParam_ParamTest", anchorFrame=0, value=0.25)
    KEYS.edit(host, "pending-curve-delete", "live2d_animation_curve_delete", sceneGuid="scene-guid",
              trackGuid="track2-guid", curveId="live2dParam_ParamTest")
    track, _ = TRACKS.add_track(host, "pending-track")
    TRACKS.populate(host, "pending-fill", "scene-guid", track, cmo3)
    KEYS.edit(host, "pending-old-track-delete", "live2d_animation_track_delete",
              sceneGuid="scene-guid", trackGuid="track-guid")
    _, model = delete(host, "pending-scene-delete", "scene-guid")
    assert model == expected
    pending = CURVES.save_and_reopen(host, "pending", run / "pending.can3", expected, original)
    require_delete(xml, CURVES.decode_entries(pending)[1], "scene-guid")
    result["checks"].append("deleted-original-scene-discards-pending-child-edits-without-orphans")

    TRACKS.reopen(host, "cancel-seed", seed)
    scene, _ = add_scene(host, "cancel-scene")
    track, _ = TRACKS.add_track(host, "cancel-track", scene_guid=scene)
    populated = TRACKS.populate(host, "cancel-fill", scene, track, cmo3)
    _, model = delete(host, "cancel-delete", scene)
    assert model == initial
    undo_redo(host, "cancel-history", populated, initial)
    cancelled = CURVES.save_and_reopen(host, "cancel", run / "cancel.can3", initial, original)
    assert cancelled == original
    result["checks"].append("add-populate-delete-scene-before-save-byte-identity-and-history")

    TRACKS.reopen(host, "all-seed", seed)
    delete(host, "all-delete-first", "scene-guid")
    delete(host, "all-delete-kept", "scene-kept-guid")
    scene, _ = add_scene(host, "all-new-scene")
    track, _ = TRACKS.add_track(host, "all-new-track", scene_guid=scene)
    model = TRACKS.populate(host, "all-fill", scene, track, cmo3)
    assert len(model["scenes"]) == 1 and model["resources"] == initial["resources"]
    all_saved = CURVES.save_and_reopen(host, "all-replaced", run / "all-replaced.can3", model, original)
    empty_original = deleted_scene_xml(deleted_scene_xml(xml, "scene-guid"), "scene-kept-guid")
    TRACKS.require_add(empty_original, CURVES.decode_entries(all_saved)[1],
                      {"CSceneSource": 1, TRACKS.TRACK: 1, TRACKS.ATTR: 1}, [("animation", "_scenes", 1)])
    result["checks"].append("delete-all-and-compose-new-scene-track-curves-before-first-save")

    special = xml.replace(b'xs.id="scene"', b'xs.id="scene&amp;drop"').replace(
        b'xs.ref="scene"', b'xs.ref="scene&amp;drop"').replace(b'uuid="scene-guid"', b'uuid="scene&amp;guid"')
    special_path = run / "special.can3"
    special_bytes = CURVES.caff_pack(special)
    special_path.write_bytes(special_bytes)
    _, special_model = TRACKS.reopen(host, "special", special_path)
    _, model = delete(host, "special-delete", "scene&guid")
    assert model == {**special_model, "scenes": special_model["scenes"][1:]}
    special_saved = CURVES.save_and_reopen(host, "special", run / "special-saved.can3", model, special_bytes)
    require_delete(special, CURVES.decode_entries(special_saved)[1], "scene&guid")
    result["checks"].append("special-scene-guid-storage-identities-and-utf8-span-preservation")

    opaque_xml = opaque_fixture(xml, "scene-kept")
    opaque_bytes = CURVES.caff_pack(opaque_xml)
    opaque_path = run / "kept-opaque.can3"
    opaque_path.write_bytes(opaque_bytes)
    _, model = TRACKS.reopen(host, "kept-opaque", opaque_path)
    assert model == initial
    _, model = delete(host, "delete-before-kept-opaque", "scene-guid")
    assert model == expected
    opaque_saved = CURVES.save_and_reopen(host, "kept-opaque", run / "kept-opaque-saved.can3",
                                          expected, opaque_bytes)
    require_delete(opaque_xml, CURVES.decode_entries(opaque_saved)[1], "scene-guid")
    result["checks"].append("retained-scene-opaque-raw-track-member-and-definition-exact-preservation")

    for label, data in retained_shapes(xml, "scene-kept").items():
        path = run / f"retained-{label}.can3"
        template = CURVES.caff_pack(data)
        path.write_bytes(template)
        _, model = TRACKS.reopen(host, "retained-" + label, path)
        assert model == initial
        guid, model = add_scene(host, "retained-" + label + "-add")
        grown_model = copy.deepcopy(initial)
        grown_model["scenes"].append({"guid": guid, "name": "New & <Scene>", "fps": 30,
                                     "durationFrames": 120, "tracks": []})
        assert model == grown_model
        grown = CURVES.save_and_reopen(host, "retained-" + label + "-grown",
                                       run / f"retained-{label}-grown.can3", model, template)
        grown_xml = CURVES.decode_entries(grown)[1]
        TRACKS.require_add(data, grown_xml, {"CSceneSource": 1}, [("animation", "_scenes", 1)])
        _, model = delete(host, "retained-" + label + "-delete-other", "scene-guid")
        del grown_model["scenes"][0]
        assert model == grown_model
        kept = CURVES.save_and_reopen(host, "retained-" + label + "-kept",
                                      run / f"retained-{label}-kept.can3", model, grown)
        require_delete(grown_xml, CURVES.decode_entries(kept)[1], "scene-guid")
    result["checks"].append("legacy-scene-add-and-safe-delete-preserve-inline-or-aliased-guid-survivors")

    for label, data, needles in unsafe_variants(xml):
        path = run / f"{label}.can3"
        path.write_bytes(CURVES.caff_pack(data))
        _, model = TRACKS.reopen(host, label + "-open", path)
        assert [s["guid"] for s in model["scenes"]] == ["scene-guid", "scene-kept-guid"]
        TRACKS.refuse(host, label, TOOL, {"sceneGuid": "scene-guid"}, needles)
    TRACKS.reopen(host, "invalid-seed", seed)
    for label, arguments in [("missing-guid", {}), ("null-guid", {"sceneGuid": None}),
                             ("numeric-guid", {"sceneGuid": 1}), ("empty-guid", {"sceneGuid": ""}),
                             ("unknown-guid", {"sceneGuid": "missing"}),
                             ("extra", {"sceneGuid": "scene-guid", "unexpected": True})]:
        TRACKS.refuse(host, label, TOOL, arguments, ("sceneGuid", "argument", "unknown", "required"))
    revision = KEYS.snapshot(host, "invalid-revision")["revision"]
    for label, arguments in [("missing-revision", {}), ("null-revision", {"expectedRevision": None}),
                             ("negative-revision", {"expectedRevision": -1}),
                             ("stale-revision", {"expectedRevision": revision - 1})]:
        TRACKS.refuse(host, label, TOOL, {"sceneGuid": "scene-guid", **arguments},
                      ("expectedRevision", "revision conflict"), with_revision=False)
    host.call("noncan-new", "new", {})
    snapshot = KEYS.snapshot(host, "noncan")
    history, files = host.call("noncan-history", "history_summary", {}), TRACKS.files(run)
    reply = host.call("noncan-delete", TOOL, {"sceneGuid": "scene-guid", "expectedRevision": snapshot["revision"]},
                      expected_error=True)
    assert "active CAN3" in json.dumps(reply)
    assert KEYS.snapshot(host, "noncan-after") == snapshot
    assert host.call("noncan-history-after", "history_summary", {}) == history
    assert TRACKS.files(run) == files
    result["checks"].append("valid-open-ownership-variants-and-selector-schema-revision-atomic-refusals")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--core", type=Path)
    parser.add_argument("--njc", type=Path)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    self_test()
    if args.self_test:
        print(json.dumps({"oracleSelfTest": True}))
        return 0
    if args.core is None or args.njc is None:
        parser.error("--core and --njc are required unless --self-test is selected")
    evidence = ROOT / "build/verification/ani145-scenes"
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="mcp-", dir=evidence))
    result = {"passed": False, "run": str(run), "checks": ["independent-scene-span-oracle-negative-self-tests"],
              "scope": "Synthetic native CAN3 scene-delete HTTP MCP; native GUI SKIP; vendor NOT_RUN; CI skipped"}
    host = None
    paths = [args.core.resolve(), args.njc.resolve(), CURVES.FIXTURE, *sorted(proof_helpers()),
             *[ROOT / "rust/src" / name for name in
               ("lib.rs", "mcp.rs", "animation_source.rs", "document.rs", "authoring_graph.rs", "native_playback.rs")],
             ROOT / "doc/commands.manifest.json"]
    try:
        result["inputHashes"] = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in paths}
        host = CURVES.DRIVER.Host(run, args.core.resolve(), args.njc.resolve(), CURVES.DRIVER.free_port())
        host.wait_ready()
        result["hostPid"] = host.child.pid
        verify(host, run, result)
        assert all(hashlib.sha256(Path(p).read_bytes()).hexdigest() == digest
                   for p, digest in result["inputHashes"].items()), "proof inputs changed during execution"
        result["passed"] = True
    except Exception:
        (run / "failure.txt").write_text(traceback.format_exc())
        traceback.print_exc()
    finally:
        if host is not None:
            try:
                host.close()
            except Exception:
                result["passed"] = False
                (run / "cleanup-failure.txt").write_text(traceback.format_exc())
            result["hostExited"] = host.child.poll() is not None
            result["passed"] = result["passed"] and result["hostExited"]
        (run / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({"passed": result["passed"], "run": str(run)}))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
