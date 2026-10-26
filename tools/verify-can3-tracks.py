#!/usr/bin/env python3
from __future__ import annotations

import argparse
from collections import Counter
import copy
from dataclasses import dataclass, field
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import sys
import tempfile
import traceback
from types import ModuleType
import xml.parsers.expat as expat

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]

SPEC = importlib.util.spec_from_file_location("curve_proof", ROOT / "tools" / "verify-can3-curves.py")
CURVES = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CURVES)
KEYS = CURVES.KEYPROOF
TRACK = "CMvTrack_Live2DModel_Source"
ATTR = "CMvAttrF"
TAG_END = re.compile(rb"""(?:[^"'<>]|"[^"]*"|'[^']*')*>""")
COUNT = re.compile(rb"""(?<![\w.:-])count\s*=\s*(["'])([0-9]+)\1""")
SCHEMAS = {
    "live2d_animation_track_add": {
        "type": "object",
        "properties": {"sceneGuid": {"type": "string"}, "resourceGuid": {"type": "string"},
                       "name": {"type": "string"}, "expectedRevision": {"type": "integer", "minimum": 0}},
        "required": ["sceneGuid", "resourceGuid", "name", "expectedRevision"],
        "additionalProperties": False,
    },
    "live2d_animation_track_delete": {
        "type": "object",
        "properties": {"sceneGuid": {"type": "string"}, "trackGuid": {"type": "string"},
                       "expectedRevision": {"type": "integer", "minimum": 0}},
        "required": ["sceneGuid", "trackGuid", "expectedRevision"],
        "additionalProperties": False,
    },
}


def proof_helpers():
    paths = {Path(__file__).resolve()}
    namespaces, visited = [globals()], set()
    while namespaces:
        for value in namespaces.pop().values():
            if not isinstance(value, ModuleType) or id(value) in visited:
                continue
            visited.add(id(value))
            filename = getattr(value, "__file__", None)
            if filename is not None and Path(filename).resolve().parent == ROOT / "tools":
                paths.add(Path(filename).resolve())
                namespaces.append(vars(value))
    return paths


@dataclass
class Span:
    tag: str
    attrs: dict
    start: int
    open_end: int
    parent: Span | None
    children: list = field(default_factory=list)
    end: int = 0


def xml_nodes(data):
    nodes, stack = [], []
    parser = expat.ParserCreate()

    def start(tag, attrs):
        offset = parser.CurrentByteIndex
        end = TAG_END.match(data, offset + 1).end()
        node = Span(tag, attrs, offset, end, stack[-1] if stack else None)
        if data[end - 2:end] == b"/>":
            node.end = end
        if stack:
            stack[-1].children.append(node)
        nodes.append(node)
        stack.append(node)

    def end(tag):
        node = stack.pop()
        assert node.tag == tag
        if not node.end:
            offset = parser.CurrentByteIndex
            assert data[offset:].startswith(f"</{tag}>".encode())
            node.end = offset + len(f"</{tag}>".encode())

    parser.StartElementHandler, parser.EndElementHandler = start, end
    parser.Parse(data, True)
    return nodes


def definitions(nodes):
    pairs = [(n.attrs["xs.id"], n) for n in nodes if "xs.id" in n.attrs]
    result = dict(pairs)
    assert len(result) == len(pairs), "duplicate xs.id"
    return result


def named(node, name):
    matches = [n for n in node.children if n.attrs.get("xs.n") == name]
    assert len(matches) == 1, (node.tag, name)
    return matches[0]


def count_span(data, node):
    matches = list(COUNT.finditer(data[node.start:node.open_end]))
    assert len(matches) == 1, "expected one exact count attribute"
    match = matches[0]
    return node.start + match.start(2), node.start + match.end(2)


def count_value(original, value):
    text = str(value).encode()
    return text.zfill(len(original)) if original.startswith(b"0") else text


def patch(data, changes):
    ordered = sorted(changes, key=lambda p: (p[0], p[1]))
    assert all(a[1] <= b[0] for a, b in zip(ordered, ordered[1:])), "overlapping oracle spans"
    for start, end, replacement in reversed(ordered):
        data = data[:start] + replacement + data[end:]
    return data


def track_definition(nodes, guid):
    matches = [n for n in nodes if n.tag == TRACK and "xs.id" in n.attrs
               and named(n, "guid").attrs.get("uuid") == guid]
    assert len(matches) == 1, guid
    return matches[0]


def deleted_xml(original, guid):
    nodes = xml_nodes(original)
    track = track_definition(nodes, guid)
    track_id = track.attrs["xs.id"]
    owners = [n for n in nodes if n.tag == TRACK and n.attrs == {"xs.ref": track_id}]
    assert len(owners) == 1, "expected one bare source-list owner"
    owner = owners[0]
    assert owner.parent.attrs.get("xs.n") == "_sources"
    storage = definitions(nodes)
    owned = []
    for entry in named(named(track, "keyParamEffect"), "attrList").children:
        definition = storage[entry.attrs["xs.ref"]]
        if named(definition, "id").attrs.get("idstr", "").startswith("live2dParam_"):
            assert definition.tag == ATTR and entry.attrs == {"xs.ref": definition.attrs["xs.id"]}
            assert named(definition, "track").attrs.get("xs.ref") == track_id
            owned.append(definition)
    start, end = count_span(original, owner.parent)
    count = int(original[start:end])
    changes = [(n.start, n.end, b"") for n in [owner, track, *owned]]
    changes.append((start, end, count_value(original[start:end], count - 1)))
    return patch(original, changes)


def require_delete(original, saved, guid):
    assert saved == deleted_xml(original, guid), "delete changed bytes outside owned spans/count"


def require_add(original, saved, expected_types, count_deltas):
    old_nodes, nodes = xml_nodes(original), xml_nodes(saved)
    old_defs, new_defs = definitions(old_nodes), definitions(nodes)
    assert old_defs.keys() <= new_defs.keys(), "addition removed an existing definition"
    added = [node for identity, node in new_defs.items() if identity not in old_defs]
    assert Counter(n.tag for n in added) == Counter(expected_types), "unexpected added definition types"
    added_ids = {n.attrs["xs.id"] for n in added}
    changes = [(n.start, n.end, b"") for n in added]
    for node in nodes:
        if node.attrs.get("xs.ref") not in added_ids:
            continue
        if any(parent.start <= node.start < parent.end for parent in added):
            continue
        assert node.attrs == {"xs.ref": node.attrs["xs.ref"]}, "new owner is not bare"
        expected_field = "_scenes" if node.tag == "CSceneSource" else "_sources"
        assert node.tag in (TRACK, "CSceneSource") and node.parent.attrs.get("xs.n") == expected_field
        changes.append((node.start, node.end, b""))
    for identity, field_name, delta in count_deltas:
        old_owner, owner = old_defs[identity], new_defs[identity]
        old_lists = [n for n in old_nodes if old_owner.start <= n.start < old_owner.end
                     and n.attrs.get("xs.n") == field_name]
        lists = [n for n in nodes if owner.start <= n.start < owner.end
                 and n.attrs.get("xs.n") == field_name]
        assert len(old_lists) == len(lists) == 1
        a, b = count_span(original, old_lists[0])
        x, y = count_span(saved, lists[0])
        assert saved[x:y] == count_value(original[a:b], int(original[a:b]) + delta), "wrong add count"
        changes.append((x, y, original[a:b]))
    assert patch(saved, changes) == original, "addition changed unrelated XML"


def must_reject(function, *arguments):
    try:
        function(*arguments)
    except AssertionError:
        return
    raise AssertionError("oracle accepted a corrupted document")


def fixture_xml():
    text = CURVES.FIXTURE.read_text()
    text = text.replace('<carray_list xs.n="_sources" count="2">',
                        '<carray_list xs.n="_sources" discount="999" count=\'0003\'>', 1)
    ref = '<CMvTrack_Live2DModel_Source xs.ref="track2"/>'
    text = text.replace(ref, '<OpaqueTrack xs.ref="opaque-track"/>' + ref, 1)
    return text.replace("</shared>", '<OpaqueTrack xs.id="opaque-track">'
                        '<s xs.n="futureData">opaque &amp; 0.1234500</s></OpaqueTrack></shared>').encode()


def self_test():
    assert ROOT / "tools" / "verify-rust-core.py" in proof_helpers(), "transitive helper was not captured"
    original = fixture_xml()
    nodes = xml_nodes(original)
    scene = definitions(nodes)["scene"]
    sources = named(named(scene, "trackSourceSet"), "_sources")
    start, end = count_span(original, sources)
    at = sources.end - len(b"</carray_list>")
    shared = next(n for n in nodes if n.tag == "shared")
    definition = ('<CMvTrack_Live2DModel_Source xs.id="probe-track">'
                  '<CGuid xs.n="guid" uuid="probe-guid"/>'
                  '<s xs.n="name">新規 &amp; &lt;probe&gt;</s>'
                  '<CMvKeyParamEffect xs.n="keyParamEffect">'
                  '<CMvTrack_Live2DModel_Source xs.n="track" xs.ref="probe-track"/>'
                  '<carray_list xs.n="attrList" count="0"/></CMvKeyParamEffect>'
                  '</CMvTrack_Live2DModel_Source>').encode()
    saved = patch(original, [(start, end, b"0004"), (at, at, b'<CMvTrack_Live2DModel_Source xs.ref="probe-track"/>'),
                             (shared.end - 9, shared.end - 9, definition)])
    require_add(original, saved, {TRACK: 1}, [("scene", "_sources", 1)])
    removed = patch(saved, [(n.start, n.end, b"") for n in xml_nodes(saved)
                           if n.attrs.get("xs.id") == "probe-track" or n.attrs == {"xs.ref": "probe-track"}]
                    + [(*count_span(saved, named(named(definitions(xml_nodes(saved))["scene"],
                                                      "trackSourceSet"), "_sources")), b"0003")])
    require_delete(saved, removed, "probe-guid")
    for bad in [saved.replace(b"count='0004'", b"count='0005'", 1),
                saved.replace(b"count='0004'", b"count='4'", 1),
                saved.replace(b">Model</s>", b">Bad</s>", 1),
                saved.replace(b'xs.ref="track2"/>', b'xs.ref="track"/>', 1),
                saved.replace(b'xs.id="attr-opaque"', b'xs.id="bad-opaque"', 1)]:
        assert bad != saved
        must_reject(require_add, original, bad, {TRACK: 1}, [("scene", "_sources", 1)])
    must_reject(require_delete, saved, removed.replace(b">Model</s>", b">Bad</s>", 1), "probe-guid")


def find_track(model, scene_guid, track_guid):
    scene = next(s for s in model["scenes"] if s["guid"] == scene_guid)
    return next(t for t in scene["tracks"] if t["guid"] == track_guid)


def add_track(host, label, scene_guid="scene-guid", name="新規 & <Track>", duration=120):
    reply, model = KEYS.edit(host, label, "live2d_animation_track_add",
                            sceneGuid=scene_guid, resourceGuid="res-guid", name=name)
    guid = reply["trackGuid"]
    assert guid not in ("track-guid", "track2-guid") and guid
    assert find_track(model, scene_guid, guid) == {
        "guid": guid, "name": name, "resourceGuid": "res-guid", "start": 0, "internalOffset": 0,
        "duration": duration, "parameterCurves": [], "partOpacityCurves": [],
        "legacyPartOpacityCurves": [], "underscorePartOpacityCurves": [], "skippedLegacyPartOpacityCount": 0,
    }
    return guid, model


def populate(host, label, scene_guid, track_guid, cmo3):
    target = {"sceneGuid": scene_guid, "trackGuid": track_guid, "curveId": "live2dParam_ParamNew"}
    KEYS.edit(host, label + "-curve", "live2d_animation_curve_add", sceneGuid=scene_guid,
              trackGuid=track_guid, modelPath=str(cmo3), parameterId="ParamNew")
    for index, (frame, value) in enumerate([(0, -1.5), (5, 0.5), (12, 1.25)]):
        KEYS.edit(host, f"{label}-key{index}", "live2d_animation_key_add", **target,
                  anchorFrame=frame, value=value, type="SMOOTH" if index == 2 else "LINEAR")
    KEYS.edit(host, label + "-set", "live2d_animation_key_set", **target, anchorFrame=5, value=0.75)
    KEYS.edit(host, label + "-handle", "live2d_animation_handle_set", **target,
              anchorFrame=5, side="next", frame=6.5, value=1.0)
    _, model = KEYS.edit(host, label + "-type", "live2d_animation_curve_type_set",
                         **target, anchorFrame=5, type="BEZIER")
    curve = next(c for c in find_track(model, scene_guid, track_guid)["parameterCurves"]
                 if c["id"] == target["curveId"])
    assert curve["parameterGuid"] == "new-parameter-guid" and curve["range"] == [-2.0, 2.0]
    assert curve["sourceParameter"] == {"type": "CParameterId", "value": "ParamNew"}
    assert [p["anchor"] for p in curve["points"]] == [
        {"frame": 0, "value": -1.5}, {"frame": 5, "value": 0.75}, {"frame": 12, "value": 1.25}]
    assert curve["points"][1]["next"] == {
        "frame": 6.5, "integerFrame": 5, "value": 1.0, "positionOptimized": False}
    assert curve["points"][1]["corner"] is True
    assert curve["curveTypes"] == ["LINEAR", "BEZIER", "SMOOTH"]
    return model


def files(run):
    return {p.name: p.read_bytes() for pattern in ("*.can3", "*.cmo3") for p in run.glob(pattern)}


def refuse(host, label, tool, arguments, needles, with_revision=True):
    assert needles and all(needles), "refusal oracle requires a specific error"
    snap, _ = KEYS.state(host, label)
    history = host.call(label + "-history", "history_summary", {})
    before = files(host.run)
    arguments = dict(arguments)
    if with_revision:
        arguments.setdefault("expectedRevision", snap["revision"])
    reply = host.call(label, tool, arguments, expected_error=True)
    assert any(needle.lower() in json.dumps(reply).lower() for needle in needles), (label, reply)
    assert KEYS.state(host, label + "-after")[0] == snap, label
    assert host.call(label + "-history-after", "history_summary", {}) == history, label
    assert files(host.run) == before, label


def reopen(host, label, path):
    host.call(label, "open", {"path": str(path)})
    return KEYS.state(host, label + "-state")


def verify(host, run, result):
    manifest = {t["id"]: t for t in json.loads((ROOT / "doc" / "commands.manifest.json").read_text())["commands"]}
    tools = {t["name"]: t for t in host.tools()}
    for name, schema in SCHEMAS.items():
        assert tools[name]["inputSchema"] == schema == manifest[name]["inputSchema"], name
        assert tools[name]["description"] == manifest[name]["description"], name
    result["checks"].append("literal-live-and-manifest-schemas")
    seed = run / "tracks.can3"
    original_xml = fixture_xml()
    original = CURVES.caff_pack(original_xml)
    seed.write_bytes(original)
    cmo3 = run / "source.cmo3"
    cmo3.write_bytes(CURVES.cmo3_bytes())
    _, initial = reopen(host, "open-seed", seed)
    assert [t["guid"] for t in initial["scenes"][0]["tracks"]] == ["track-guid", "track2-guid"]
    guid, added = add_track(host, "add")
    assert added["scenes"][0]["tracks"][:2] == initial["scenes"][0]["tracks"]
    assert added["resources"] == initial["resources"]
    snap, _ = KEYS.state(host, "before-undo")
    position = snap["history"]["position"]
    host.call("undo-add", "history_goto", {"index": position - 1, "expectedRevision": snap["revision"]})
    undone, model = KEYS.state(host, "undone-add")
    assert model == initial and undone["revision"] == snap["revision"] + 1
    host.call("redo-add", "history_goto", {"index": position, "expectedRevision": undone["revision"]})
    redone, model = KEYS.state(host, "redone-add")
    assert model == added and redone["revision"] == undone["revision"] + 1
    model = populate(host, "factory", "scene-guid", guid, cmo3)
    saved = CURVES.save_and_reopen(host, "factory", run / "factory.can3", model, original)
    saved_xml = CURVES.decode_entries(saved)[1]
    require_add(original_xml, saved_xml, {TRACK: 1, ATTR: 1}, [("scene", "_sources", 1)])
    for corrupted in [saved_xml.replace(b"count='0004'", b"count='0005'", 1),
                      saved_xml.replace(b"count='0004'", b"count='4'", 1),
                      saved_xml.replace(b">Model</s>", b">Bad</s>", 1),
                      saved_xml.replace(b'xs.id="attr-opaque"', b'xs.id="bad-opaque"', 1)]:
        assert corrupted != saved_xml
        must_reject(require_add, original_xml, corrupted, {TRACK: 1, ATTR: 1}, [("scene", "_sources", 1)])
    result["checks"].append("added-track-final-curves-keys-handles-types-undo-redo-and-xml")
    _, model = KEYS.edit(host, "delete-written", "live2d_animation_track_delete",
                         sceneGuid="scene-guid", trackGuid=guid)
    assert model == initial
    deleted = CURVES.save_and_reopen(host, "written-deleted", run / "written-deleted.can3", model, saved)
    require_delete(saved_xml, CURVES.decode_entries(deleted)[1], guid)
    result["checks"].append("physical-owned-factory-deletion-after-native-reopen")
    reopen(host, "open-special-parameter", seed)
    special_source = run / "escaped-source.cmo3"
    special_source.write_bytes(CURVES.caff_pack(
        CURVES.cmo3_xml().replace("ParamNew", "Param&amp;A").encode()))
    special_guid, _ = add_track(host, "escaped-track", name="")
    _, model = KEYS.edit(host, "escaped-curve", "live2d_animation_curve_add",
                         sceneGuid="scene-guid", trackGuid=special_guid,
                         modelPath=str(special_source), parameterId="Param&A")
    special_curve = find_track(model, "scene-guid", special_guid)["parameterCurves"][0]
    assert special_curve["id"] == "live2dParam_Param&A"
    assert special_curve["parameterId"] == "Param&A"
    assert special_curve["sourceParameter"] == {"type": "CParameterId", "value": "Param&A"}
    _, model = KEYS.edit(host, "escaped-key", "live2d_animation_key_add",
                         sceneGuid="scene-guid", trackGuid=special_guid,
                         curveId="live2dParam_Param&A", anchorFrame=3, value=0.25)
    saved = CURVES.save_and_reopen(host, "escaped", run / "escaped.can3", model, original)
    require_add(original_xml, CURVES.decode_entries(saved)[1],
                {TRACK: 1, ATTR: 1}, [("scene", "_sources", 1)])
    result["checks"].append("public-cmo-parameter-ampersand-and-empty-track-name")
    reopen(host, "open-original-delete", seed)
    _, model = KEYS.edit(host, "delete-first", "live2d_animation_track_delete",
                         sceneGuid="scene-guid", trackGuid="track-guid")
    expected = copy.deepcopy(initial)
    expected["scenes"][0]["tracks"].pop(0)
    assert model == expected
    deleted = CURVES.save_and_reopen(host, "original-deleted", run / "original-deleted.can3", expected, original)
    require_delete(original_xml, CURVES.decode_entries(deleted)[1], "track-guid")
    result["checks"].append("original-track-and-owned-curve-exact-delete-with-opaque-resource-preservation")
    reopen(host, "open-original-mixed", seed)
    KEYS.edit(host, "mixed-delete", "live2d_animation_track_delete",
              sceneGuid="scene-guid", trackGuid="track-guid")
    _, model = KEYS.edit(host, "mixed-edit-second", "live2d_animation_key_set",
                         sceneGuid="scene-guid", trackGuid="track2-guid",
                         curveId="live2dParam_ParamTest", anchorFrame=0, value=0.25)
    expected["scenes"][0]["tracks"][0]["parameterCurves"][0]["points"][0]["anchor"]["value"] = 0.25
    assert model == expected
    CURVES.save_and_reopen(host, "mixed", run / "mixed.can3", expected, original)
    result["checks"].append("delete-first-edit-retained-guid-before-save")
    reopen(host, "open-original-last", seed)
    populate(host, "pending-original", "scene-guid", "track-guid", cmo3)
    for track_guid in ("track-guid", "track2-guid"):
        KEYS.edit(host, "last-delete-" + track_guid, "live2d_animation_track_delete",
                  sceneGuid="scene-guid", trackGuid=track_guid)
    _, empty = KEYS.state(host, "all-model-tracks-deleted")
    assert empty["scenes"][0]["tracks"] == [] and empty["resources"] == initial["resources"]
    replacement, _ = add_track(host, "last-replacement")
    model = populate(host, "last-replacement-populated", "scene-guid", replacement, cmo3)
    assert len(model["scenes"][0]["tracks"]) == 1
    saved = CURVES.save_and_reopen(host, "last-replacement", run / "last-replacement.can3", model, original)
    removed_xml = deleted_xml(deleted_xml(original_xml, "track-guid"), "track2-guid")
    require_add(removed_xml, CURVES.decode_entries(saved)[1],
                {TRACK: 1, ATTR: 1}, [("scene", "_sources", 1)])
    result["checks"].append("pending-original-curve-edits-delete-all-then-add-with-opaque-raw-member")
    reopen(host, "open-original-scene", seed)
    reply, _ = KEYS.edit(host, "add-scene", "live2d_animation_scene_add",
                         name="New scene", fps=30, durationFrames=90)
    scene_guid = reply["sceneGuid"]
    new_guid, _ = add_track(host, "new-scene-track", scene_guid, duration=90)
    KEYS.edit(host, "scene-duration", "live2d_animation_scene_set",
              sceneGuid=scene_guid, durationFrames=120)
    populate(host, "new-scene-populated", scene_guid, new_guid, cmo3)
    second_guid, model = add_track(host, "existing-scene-track")
    assert second_guid != new_guid
    assert find_track(model, scene_guid, new_guid)["duration"] == 90
    assert {k: v for k, v in model["scenes"][1].items() if k != "tracks"} == {
        "guid": scene_guid, "name": "New scene", "fps": 30, "durationFrames": 120}
    saved = CURVES.save_and_reopen(host, "new-scene", run / "new-scene.can3", model, original)
    require_add(original_xml, CURVES.decode_entries(saved)[1],
                {"CSceneSource": 1, TRACK: 2, ATTR: 1},
                [("animation", "_scenes", 1), ("scene", "_sources", 1)])
    result["checks"].append("new-scene-track-curves-and-multiple-tracks-before-first-save")
    reopen(host, "open-original-cancel", seed)
    guid, _ = add_track(host, "cancel-add")
    before_delete = populate(host, "cancel-populate", "scene-guid", guid, cmo3)
    _, model = KEYS.edit(host, "cancel-delete", "live2d_animation_track_delete",
                         sceneGuid="scene-guid", trackGuid=guid)
    assert model == initial
    snap, _ = KEYS.state(host, "cancel-before-undo")
    position = snap["history"]["position"]
    host.call("undo-delete", "history_goto", {"index": position - 1, "expectedRevision": snap["revision"]})
    undone, model = KEYS.state(host, "delete-undone")
    assert model == before_delete and undone["revision"] == snap["revision"] + 1
    host.call("redo-delete", "history_goto", {"index": position, "expectedRevision": undone["revision"]})
    redone, model = KEYS.state(host, "delete-redone")
    assert model == initial and redone["revision"] == undone["revision"] + 1
    cancelled = CURVES.save_and_reopen(host, "cancel", run / "cancel.can3", model, original)
    assert cancelled == original
    result["checks"].append("add-populate-delete-cancellation-byte-identity-and-deletion-undo-redo")
    verify_refusals(host, run, seed, original_xml)
    result["checks"].append("selector-schema-revision-and-owned-storage-refusals-atomic")


def verify_refusals(host, run, seed, original_xml):
    reopen(host, "open-invalid", seed)
    add = {"sceneGuid": "scene-guid", "resourceGuid": "res-guid", "name": "Track"}
    delete = {"sceneGuid": "scene-guid", "trackGuid": "track-guid"}
    for label, tool, arguments, needles in [
        ("wrong-scene", "add", dict(add, sceneGuid="none"), ("scene", "not found")),
        ("wrong-resource", "add", dict(add, resourceGuid="none"), ("resource",)),
        ("empty-resource", "add", dict(add, resourceGuid=""), ("resource",)),
        ("null-name", "add", dict(add, name=None), ("name",)),
        ("control-name", "add", dict(add, name="\x01"), ("name", "control")),
        ("extra", "add", dict(add, extra=True), ("unknown argument",)),
        ("stale", "add", dict(add, expectedRevision=0), ("revision",)),
        ("null-revision", "add", dict(add, expectedRevision=None), ("expectedRevision",)),
        ("negative-revision", "add", dict(add, expectedRevision=-1), ("expectedRevision",)),
        ("missing-name", "add", {k: v for k, v in add.items() if k != "name"}, ("name",)),
        ("missing-track", "delete", {"sceneGuid": "scene-guid"}, ("trackGuid",)),
        ("wrong-track", "delete", dict(delete, trackGuid="none"), ("track", "not found")),
        ("extra-delete", "delete", dict(delete, extra=True), ("unknown argument",)),
    ]:
        refuse(host, label, "live2d_animation_track_" + tool, arguments, needles)
    for tool, arguments in (("add", add), ("delete", delete)):
        refuse(host, "missing-revision-" + tool, "live2d_animation_track_" + tool,
               arguments, ("expectedRevision",), with_revision=False)
    variants = [
        ("outside-track", original_xml.replace(b"</shared>", b'<Unknown xs.ref="track"/></shared>'),
         ("outside", "referenced")),
        ("outside-curve", original_xml.replace(b"</shared>", b'<Unknown xs.ref="attr"/></shared>'),
         ("outside", "referenced")),
        ("nonbare-owner", original_xml.replace(b'<CMvTrack_Live2DModel_Source xs.ref="track"/>',
                                              b'<CMvTrack_Live2DModel_Source xs.ref="track" extra="keep"/>', 1),
         ("bare", "reference")),
        ("nested-shared", original_xml.replace(b'<CGuid xs.n="guid" uuid="track-guid"/>',
                                               b'<CGuid xs.n="guid" uuid="track-guid"/>'
                                               b'<s xs.n="future" xs.id="nested-track-data">keep</s>', 1),
         ("nested", "shared")),
        ("opaque-backlink", original_xml.replace(b'<CMvAttrF xs.id="attr-opaque">',
                                                 b'<CMvAttrF xs.id="attr-opaque">'
                                                 b'<CMvTrack_Live2DModel_Source xs.n="track" xs.ref="track"/>', 1),
         ("outside", "referenced")),
    ]
    nodes = xml_nodes(original_xml)
    outside_curve = definitions(nodes)["attr"]
    outside_body = original_xml[outside_curve.start:outside_curve.end].replace(
        b'xs.id="attr"', b'xs.id="outside-attr"', 1).replace(b'xs.ref="attr"', b'xs.ref="outside-attr"')
    variants.append(("unowned-parameter-backlink", original_xml.replace(
        b"</shared>", outside_body + b"</shared>"), ("outside", "referenced")))
    source_set = named(definitions(nodes)["scene"], "trackSourceSet")
    body = original_xml[source_set.start:source_set.end]
    shared_body = body.replace(b'xs.n="trackSourceSet"', b'xs.id="shared-set"', 1)
    shared_set = patch(original_xml, [(source_set.start, source_set.end,
                                      b'<CTrackSourceSet xs.n="trackSourceSet" xs.ref="shared-set"/>')])
    shared_set = shared_set.replace(b"</shared>", shared_body + b"</shared>")
    variants.append(("shared-source-set", shared_set, ("shared", "inline", "trackSourceSet")))
    inherited_set = patch(original_xml, [(source_set.start, source_set.end,
                                         b'<CSceneSource xs.n="super" xs.ref="scene-super"/>')])
    inherited_set = inherited_set.replace(b"</shared>", b'<CSceneSource xs.id="scene-super">'
                                           + body + b"</CSceneSource></shared>")
    variants.append(("inherited-source-set", inherited_set, ("super", "inline", "trackSourceSet")))
    track = definitions(nodes)["track"]
    effect = named(track, "keyParamEffect")
    body = original_xml[effect.start:effect.end]
    shared_effect = patch(original_xml, [(effect.start, effect.end,
                                         b'<CMvKeyParamEffect xs.n="keyParamEffect" xs.ref="shared-effect"/>')])
    shared_effect = shared_effect.replace(b"</shared>",
                                           body.replace(b'xs.n="keyParamEffect"', b'xs.id="shared-effect"', 1)
                                           + b"</shared>")
    variants.append(("shared-effect", shared_effect, ("shared", "inline", "keyParamEffect")))
    inherited = patch(original_xml, [(effect.start, effect.end,
                                      b'<CMvTrack_Live2DModel_Source xs.n="super" xs.ref="track-super"/>')])
    inherited = inherited.replace(b"</shared>",
                                  b'<CMvTrack_Live2DModel_Source xs.id="track-super">' + body
                                  + b"</CMvTrack_Live2DModel_Source></shared>")
    variants.append(("inherited-effect", inherited, ("super", "inherited", "inline", "keyParamEffect")))
    for label, data, needles in variants:
        path = run / f"{label}.can3"
        path.write_bytes(CURVES.caff_pack(data))
        _, model = reopen(host, label + "-open", path)
        assert [t["guid"] for t in model["scenes"][0]["tracks"]] == ["track-guid", "track2-guid"]
        refuse(host, label, "live2d_animation_track_delete", delete, needles)
        if label in ("shared-source-set", "inherited-source-set"):
            refuse(host, label + "-add", "live2d_animation_track_add", add, needles)
        _, model = KEYS.edit(host, label + "-legacy-key", "live2d_animation_key_set",
                             sceneGuid="scene-guid", trackGuid="track-guid",
                             curveId="live2dParam_ParamTest", anchorFrame=0, value=0.25)
        assert find_track(model, "scene-guid", "track-guid")["parameterCurves"][0]["points"][0]["anchor"]["value"] == 0.25
        persisted = run / f"{label}-saved.can3"
        CURVES.save_and_reopen(host, label + "-key-roundtrip", persisted, model, path.read_bytes())


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
    evidence = ROOT / "build" / "verification" / "ani145-tracks"
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="mcp-", dir=evidence))
    result = {"passed": False, "run": str(run), "checks": ["byte-offset-xml-oracle-negative-self-tests"],
              "scope": "Synthetic native CAN3 model-track HTTP MCP; native GUI SKIP; vendor NOT_RUN; CI skipped"}
    host = None
    paths = [args.core.resolve(), args.njc.resolve(), CURVES.FIXTURE, *sorted(proof_helpers()),
             *[ROOT / "rust" / "src" / name for name in ("lib.rs", "mcp.rs", "animation_source.rs")],
             ROOT / "doc" / "commands.manifest.json"]
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
