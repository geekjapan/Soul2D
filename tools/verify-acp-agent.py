#!/usr/bin/env python3
"""Run a real ACP agent (the claude CLI through tools/acp-claude.py) against a headless soul2d-core and check the
result from the outside (#99 section 5, #83).

Each family (one per map in .cursor/skills/operate-soul2d/families) has one workflow: an edit, its reversal (`undo`,
or the reset of a runtime playback family), the edit again, `save`, and `render` to a PNG. It runs three times on a
fresh document:

- a negative control: the setup alone, saved and reopened, must fail the family's check, so the check is not vacuous;
- a headless MCP client: the harness makes the workflow's calls itself with `njc`;
- a real ACP agent: the agent is told the steps and makes the calls through the MCP server its ACP session offers.

After each run the harness confirms the result over the same MCP endpoint with `njc`, never from the agent's own
words: the family's check reads MCP resources where one holds the value (the snapshot, node, texture, binding and
animation-source resources) and tools otherwise (an atlas build, the node list); the revision must have moved, the
saved file is reopened with `open` and checked again, and the render must be a PNG. The live2d-expression,
live2d-physics and live2d-pose families check runtime playback state, which no file keeps: they save nothing, so the
revision, save and reopen checks do not apply to them. The transcript of the ACP session (every update the agent
streamed) and the checks go to `--out` as JSON, one file per family.

The editor-state family (`--editor <soul2d-editor>`) edits the editor's view, which a headless host refuses: its
control and scripted client use the editor's stdio MCP, and its agent runs in the editor's agent panel, driven through
the accessibility tree with tools/verify-panels-gui.py (this needs an unlocked screen; a locked one makes the run
NOT_RUN). `ACP.Command` is preset to the bridge with `ACP_CLAUDE_LOG`, whose log of the session's updates replaces
the transcript; nothing is saved, the revision must not move, and the window screenshot shows the final view.

The assets a family needs (PNG layers, a motion3 file) are generated into the run's directory; the live2d family
opens the synthetic `rust/tests/fixtures/physics3-editor.cmo3`. Nothing here reads a third-party model.

usage: python3 -I tools/verify-acp-agent.py --core <soul2d-core> --njc <njc> [--editor <soul2d-editor>]
       [--families nodes-inspector,history,...] [--model claude-haiku-5-5] [--no-agent] [--out <dir>]

--no-agent runs the negative control and the scripted MCP client only (no agent, no subscription use).
A run that cannot be made (a locked screen, a control the accessibility tree does not offer) is NOT_RUN, which is
not a pass: the exit status is 0 only when every run passed.
"""
import argparse
import contextlib
import functools
import importlib.util
import json
import os
import re
import shlex
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import types
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BRIDGE = ROOT / "tools/acp-claude.py"

STEPS = (
    "You operate a soul2d document through the soul2d MCP tools only. Do these steps in order, one tool call each, "
    "and read every answer. {steps} {finish}Finally reply with one line: the final value and the revision from "
    "history_summary."
)
SAVE_AND_RENDER = ("Then save with path {dir}/{save}, call bounds, and render with width 64, height 64, bounds from "
                   "the bounds answer, path {dir}/agent.png. ")
SAVE = "Then save with path {dir}/{save}. "
RENDER = "Then call bounds, and render with width 64, height 64, bounds from the bounds answer, path {dir}/agent.png. "
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"


def parse(text):
    try:
        return json.loads(text)
    except ValueError:
        return text


def snapshot(client):
    return client.read("resource://vsoul/snapshot")


def model_resource(client):
    return snapshot(client)["model"]


def tree(node):
    """Every node of a snapshot node tree."""
    yield node
    for child in node.get("children", []):
        yield from tree(child)


def node_names(client):
    """The names of every node, read from the snapshot resource."""
    return [node["name"] for node in tree(model_resource(client)["nodes"])]


def node_item(client, name):
    """The resource item {name, typeId, uuid, data} of the node named `name` (found through the snapshot resource),
    or None while there is none."""
    uuid = next((node["uuid"] for node in tree(model_resource(client)["nodes"]) if node.get("name") == name), None)
    return None if uuid is None else client.read(f"resource://soul2d/resources/{uuid}")["item"]


def node_data(client, name):
    item = node_item(client, name)
    return {} if item is None else item["data"]


def node_named(core, name):
    """The nodes_list row of the node named `name` (a tool read, for the scripted calls)."""
    return next(n for n in core.call("nodes_list") if n.get("name") == name)


def write_png(path, width, height, rgba):
    """One flat-colour RGBA PNG, written with the stdlib so the run needs no image library."""
    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF)
    raw = b"".join(b"\x00" + bytes(rgba) * width for _ in range(height))
    path.write_bytes(PNG_SIGNATURE + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
                     + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))


def write_layers(workdir):
    """A 16x16 layer folder and a 32x32 extra image; returns (layers dir, extra PNG)."""
    layers = workdir / "layers"
    layers.mkdir(exist_ok=True)
    write_png(layers / "face.png", 16, 16, (200, 80, 80, 255))
    write_png(workdir / "extra.png", 32, 32, (40, 40, 200, 255))
    return layers, workdir / "extra.png"


def import_layers(workdir):
    return [("import_folder", {"path": str(write_layers(workdir)[0])})]


def layers_only(workdir):
    write_layers(workdir)
    return []


def write_motion3(path):
    """A one-curve motion3 (ParamAngleX, one linear segment from 0 to 10 over one second)."""
    path.write_text(json.dumps({
        "Version": 3,
        "Meta": {"Duration": 1.0, "Fps": 30.0, "Loop": True, "AreBeziersRestricted": True, "CurveCount": 1,
                 "TotalSegmentCount": 1, "TotalPointCount": 2, "UserDataCount": 0},
        "Curves": [{"Target": "Parameter", "Id": "ParamAngleX", "Segments": [0, 0, 0, 1, 10]}],
    }))


def import_motion(workdir):
    write_motion3(workdir / "in.motion3.json")
    return [("motion3_import", {"name": "Agent motion", "path": str(workdir / "in.motion3.json")})]


def live2d_fixture(workdir):
    """The committed synthetic CMO3 (parameter ParamTest, physics group 0 named Sway)."""
    fixture = workdir / "fixture.cmo3"
    shutil.copy(ROOT / "rust/tests/fixtures/physics3-editor.cmo3", fixture)
    return fixture


@functools.cache
def caff_pack():
    """caff_pack(xml) of tools/verify-can3-key-structure.py: a synthetic CAFF archive (CMO3 or CAN3) around a main.xml."""
    spec = importlib.util.spec_from_file_location("can3_key_structure", ROOT / "tools/verify-can3-key-structure.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.caff_pack


# ---- animation
def animation_script(client, workdir):
    client.call("animation_rename", {"name": "Agent clip", "new_name": "Agent clip renamed"})
    client.call("undo")
    assert "Agent clip" in model_resource(client)["animations"]
    client.call("animation_rename", {"name": "Agent clip", "new_name": "Agent clip renamed"})
    client.call("animation_set", {"name": "Agent clip renamed", "length": 90})


def animation_done(client):
    clips = model_resource(client)["animations"]
    return clips.get("Agent clip renamed", {}).get("length") == 90 and "Agent clip" not in clips


# ---- atlas-export
ATLAS_PIN = {"atlas": 0, "x": 5, "y": 7}


def face_mapping(client):
    """Where atlas_build (a plan; it writes nothing) puts the Part 'face' on page 0."""
    plan = client.call("atlas_build", {"resolution": 64, "padding": 0})
    return next(p["mapping"] for p in plan["parts"] if p["id"] == node_named(client, "face")["uuid"])


def atlas_pinned(client):
    parts = model_resource(client).get("atlasLayout", {}).get("parts", {})
    return parts.get(str(node_named(client, "face")["uuid"])) == ATLAS_PIN and face_mapping(client) == [5, 7, 16, 16]


def atlas_script(client, workdir):
    face = node_named(client, "face")["uuid"]
    client.call("atlas_layout_set", {"parts": [{"id": face, **ATLAS_PIN}]})
    assert face_mapping(client) == [5, 7, 16, 16]
    client.call("undo")
    assert face_mapping(client) == [0, 0, 16, 16]
    client.call("atlas_layout_set", {"parts": [{"id": face, **ATLAS_PIN}]})
    client.call("export_inp", {"path": str(workdir / "agent.inp"), "atlasResolution": 64, "padding": 0})


def exported_inp_is_pinned(workdir):
    """INP = b'TRNSRTS\\0', a big-endian u32 length, then the puppet JSON, whose Part UVs are atlas-baked."""
    path = workdir / "agent.inp"
    data = path.read_bytes() if path.exists() else b""
    if data[:8] != b"TRNSRTS\0":
        return False
    puppet = json.loads(data[12:12 + struct.unpack(">I", data[8:12])[0]])
    face = next(n for n in puppet["nodes"]["children"] if n["name"] == "face")
    return face["mesh"]["uvs"][:2] == [5 / 64, 7 / 64] and puppet["atlasLayout"]["parts"][str(face["uuid"])] == ATLAS_PIN


# ---- automesh
def part_vertex_count(client):
    """Vertex count of the Part 'face', read from its resource (mesh.verts holds flat x, y pairs)."""
    return len(node_data(client, "face")["mesh"]["verts"]) // 2


def automesh_script(client, workdir):
    # Read-only queries only: the preset store is a per-user file outside the document.
    client.call("cv_analyze", {"method": "distance", "size": [3, 3], "pixels": [0, 0, 0, 0, 1, 0, 0, 0, 0]})
    client.call("automesh_preset_list")
    face = node_named(client, "face")["uuid"]
    client.call("automesh_generate", {"id": face, "algorithm": "grid"})
    client.call("undo")
    client.call("automesh_generate", {"id": face, "algorithm": "grid", "x_segments": 4, "y_segments": 4})


# ---- deformers
def deformers_script(client, workdir):
    grid = {"nodes": [node_named(client, "Agent grid")["uuid"]], "axisX": [-1, 0, 1], "axisY": [-1, 0, 1]}
    client.call("deformer_define_grid", grid)
    client.call("undo")
    client.call("deformer_define_grid", grid)


# ---- depth-bones-rig
def rig_bone(client, name="Agent bone"):
    """The DepthBone named `name`, found with resources/find and read from its resource (None when absent)."""
    def walk(items):
        for item in items:
            if item.get("typeId") == "DepthBone" and item.get("name") == name:
                return item["uuid"]
            if (found := walk(item.get("children", []))) is not None:
                return found
        return None
    uuid = walk(client.read("resource://soul2d/resources/find?selector=*")["items"])
    return None if uuid is None else client.read(f"resource://soul2d/resources/{uuid}")["item"]["data"]


def rig_script(client, workdir):
    root = client.call("DepthBoneCommand_CreateDepthRigRoot", {"name": "Agent rig"})["created"][0]["uuid"]
    bone = client.call("DepthBoneCommand_AddDepthBone", {"parent": root, "boneId": "Agent bone", "restHead": [0, 0, 0],
                                                         "restTail": [0, 10, 0]})["created"][0]["uuid"]
    rest = {"bone": bone, "restHead": [0, 0, 0], "restTail": [0, 20, 0], "restRoll": 0}
    client.call("rig_set_rest", rest)
    client.call("undo")
    client.call("rig_set_rest", rest)
    client.call("rig_drag_bone", {"root": root, "bone": bone, "translation": [3, 0, 0]})


def rig_done(client):
    bone = rig_bone(client)
    return bone is not None and bone["restTail"] == [0.0, 20.0, 0.0] and bone["transform"]["trans"] == [3.0, 0.0, 0.0]


# ---- depth-map-draw
def depth_setup(workdir):
    # Grey 204 maps to depth 0.6 (the layer's back depth is -1, its front depth 1).
    write_png(workdir / "depth.png", 16, 16, (204, 204, 204, 255))
    return [("node_create", {"className": "GridDeformer", "name": "Agent grid"})]


def stored_depths(client):
    """The depths stored on 'Agent grid', read from its resource (None while it has none)."""
    return node_data(client, "Agent grid").get("depths")


def depth_script(client, workdir):
    grid = node_named(client, "Agent grid")["uuid"]
    client.call("depth_draw_load_png", {"path": str(workdir / "depth.png"), "id": "agent-depth"})
    client.call("depth_draw_binding_update", {"layer": "agent-depth", "target": grid})
    client.call("depth_draw_compose", {"target": grid})
    client.call("depth_draw_apply", {"target": grid})
    client.call("undo")
    assert stored_depths(client) is None
    client.call("depth_draw_apply", {"target": grid})


def depths_done(client):
    depths = stored_depths(client)
    return depths is not None and len(depths) == 4 and all(abs(value - 0.6) < 1e-3 for value in depths)


# ---- history and nodes-inspector
def rename_node(client, old, new):
    client.call("node_rename", {"id": node_named(client, old)["uuid"], "name": new})


def history_script(client, workdir):
    rename_node(client, "Agent target", "Agent renamed")
    client.call("history_summary")
    client.call("undo")
    client.call("redo")


def history_done(client):
    state = snapshot(client)
    current = next(entry["label"] for entry in state["history"]["states"] if entry["current"])
    return (node_item(client, "Agent renamed") is not None and current == "node.rename" and state["canUndo"]
            and not state["canRedo"])


def nodes_script(client, workdir):
    rename_node(client, "Agent target", "Agent renamed")
    client.call("undo")
    rename_node(client, "Agent target", "Agent renamed")


def nodes_done(client):
    item = node_item(client, "Agent renamed")
    return item is not None and item["name"] == item["data"]["name"] == "Agent renamed" and node_item(client, "Agent target") is None


# ---- import-merge
def merge_script(client, workdir):
    extra = str(workdir / "extra.png")
    client.call("merge_images", {"paths": [extra]})
    assert "extra.png" in node_names(client)
    client.call("undo")
    assert "extra.png" not in node_names(client)
    client.call("merge_images", {"paths": [extra]})


# ---- live2d-animation
def can3_fixture(workdir):
    """A synthetic .can3 (scene-guid / track-guid / ParamTest curve with keys at frames 0 and 10), packed from the
    committed XML."""
    path = workdir / "fixture.can3"
    path.write_bytes(caff_pack()((ROOT / "rust/tests/fixtures/can3-key-editor.xml").read_bytes()))
    return path


def can3_keys(client):
    scene = client.read("resource://soul2d/document/animation-source")["scenes"][0]
    return [(p["anchor"]["frame"], p["anchor"]["value"]) for p in scene["tracks"][0]["parameterCurves"][0]["points"]]


def can3_key_add(client):
    return client.call("live2d_animation_key_add", {
        "sceneGuid": "scene-guid", "trackGuid": "track-guid", "curveId": "live2dParam_ParamTest", "anchorFrame": 5,
        "value": 0.8, "expectedRevision": client.call("history_summary")["revision"]})


def can3_script(client, workdir):
    can3_key_add(client)
    client.call("undo")
    can3_key_add(client)


def can3_drawable(workdir):
    """A CAN3 session has no scene to draw: attach the saved animation to the CMO3 fixture at the new key's frame."""
    return [("open", {"path": str(live2d_fixture(workdir))}),
            ("live2d_animation_attach", {"path": str(workdir / "agent.can3")}),
            ("live2d_animation_seek", {"sceneGuid": "scene-guid", "frame": 5})]


# ---- live2d-authoring
def document_revision(client):
    return client.read("resource://soul2d/document/state")["revision"]


def authoring_script(client, workdir):
    guid = model_resource(client)["parameters"][0]["guid"]
    client.call("live2d_parameter_edit", {"guid": guid, "maximum": 2, "expectedRevision": document_revision(client)})
    assert model_resource(client)["parameters"][0]["maximum"] == 2
    client.call("undo")
    assert model_resource(client)["parameters"][0]["maximum"] == 1
    client.call("live2d_parameter_edit", {"guid": guid, "maximum": 2, "expectedRevision": document_revision(client)})
    client.call("live2d_physics3_read")
    client.call("live2d_physics3_edit", {"group": 0, "field": "name", "value": "Agent sway",
                                         "expectedRevision": document_revision(client)})


def authoring_done(client):
    # The group name has no resource of its own (the snapshot holds the stored physics source), so the typed read is used.
    return (model_resource(client)["parameters"][0]["maximum"] == 2
            and client.call("live2d_physics3_read")["groups"][0]["name"] == "Agent sway")


# ---- live2d-physics, live2d-expression, live2d-pose: a runtime model the stdlib writes
def runtime_model(workdir):
    """A synthetic CMO3 built the way rust/tests/core.rs builds its runtime models: ParamTest (-1..1) morphs the mesh
    'wing' (part 'root') from a small to a large triangle; the static mesh 'twin' belongs to part 'alternate'."""
    def form(guid, mesh, positions):
        return (f"<CArtMeshForm><ACDrawableForm xs.n='super'><ACForm xs.n='super'><CFormGuid xs.n='guid' uuid='{guid}'/>"
                f"<CArtMeshSource xs.n='_source' xs.ref='{mesh}'/></ACForm><CoordType xs.n='coordType'><s xs.n='coordName'>"
                "Canvas</s></CoordType><CFloatColor xs.n='multiplyColor' red='1' green='1' blue='1' alpha='1'/>"
                f"</ACDrawableForm><float-array xs.n='positions' count='6'>{positions}</float-array></CArtMeshForm>")

    def key(guid, index):
        keys = ("<array_list xs.n='_keyOnParameterList' count='1'><KeyOnParameter><KeyformBindingSource xs.n='binding' "
                f"xs.ref='binding-alias'/><i xs.n='keyIndex'>{index}</i></KeyOnParameter></array_list>") if index is not None \
            else "<array_list xs.n='_keyOnParameterList' count='0'/>"
        return (f"<KeyformOnGrid><KeyformGridAccessKey xs.n='accessKey'>{keys}</KeyformGridAccessKey>"
                f"<CFormGuid xs.n='keyformGuid' uuid='{guid}'/></KeyformOnGrid>")

    def mesh(name, part, forms):
        bound = len(forms) > 1
        bindings = "<KeyformBindingSource xs.ref='binding-alias'/>" if bound else ""
        grid = "".join(key(guid, index if bound else None) for index, (guid, _) in enumerate(forms))
        return (f"<CArtMeshSource xs.id='{name}'><ACDrawableSource xs.n='super'><CArtMeshGuid xs.n='guid' uuid='{name}-guid'/>"
                f"<CArtMeshId xs.n='id' idstr='{name}'/><CPartGuid xs.n='parentGuid' uuid='{part}-guid'/></ACDrawableSource>"
                f"<KeyformGridSource xs.n='keyformGridSource'><array_list xs.n='keyformBindings' count='{int(bound)}'>{bindings}"
                f"</array_list><array_list xs.n='keyformsOnGrid' count='{len(forms)}'>{grid}</array_list></KeyformGridSource>"
                "<float-array xs.n='positions' count='6'>0 0 1 0 1 1</float-array><float-array xs.n='uvs' count='6'>0 0 1 0 1 1"
                "</float-array><int-array xs.n='indices' count='3'>0 1 2</int-array><GTexture2D xs.n='texture' xs.ref='texture'/>"
                f"<carray_list xs.n='keyforms' count='{len(forms)}'>"
                + "".join(form(guid, name, positions) for guid, positions in forms) + "</carray_list></CArtMeshSource>")

    def part(name):
        return (f"<CPartSource xs.id='{name}'><CPartGuid xs.n='guid' uuid='{name}-guid'/><CPartId xs.n='id' idstr='{name}'/>"
                "<carray_list xs.n='keyforms' count='0'/></CPartSource>")

    xml = (
        "<root fileFormatVersion='401000004'><CModelSource><s xs.n='name'>Runtime</s><CImageCanvas xs.n='canvas'>"
        "<i xs.n='pixelWidth'>100</i><i xs.n='pixelHeight'>100</i></CImageCanvas><CParameterSourceSet xs.n='parameterSourceSet'>"
        "<carray_list xs.n='_sources' count='1'><CParameterSource><CParameterGuid xs.n='guid' uuid='parameter-guid'/>"
        "<CParameterId xs.n='id' idstr='ParamTest'/><f xs.n='minValue'>-1</f><f xs.n='maxValue'>1</f><f xs.n='defaultValue'>0</f>"
        "</CParameterSource></carray_list></CParameterSourceSet><CDrawableSourceSet xs.n='drawableSourceSet'>"
        "<carray_list xs.n='_sources' count='2'><CArtMeshSource xs.ref='wing'/><CArtMeshSource xs.ref='twin'/></carray_list>"
        "</CDrawableSourceSet><CDeformerSourceSet xs.n='deformerSourceSet'><carray_list xs.n='_sources' count='0'/>"
        "</CDeformerSourceSet><CPartSourceSet xs.n='partSourceSet'><carray_list xs.n='_sources' count='2'>"
        "<CPartSource xs.ref='root'/><CPartSource xs.ref='alternate'/></carray_list></CPartSourceSet>"
        "<CPartSource xs.n='rootPart' xs.ref='root'/></CModelSource>"
        + mesh("wing", "root", [("wing-low", "10 10 30 10 10 30"), ("wing-high", "10 10 90 10 50 90")])
        + mesh("twin", "alternate", [("twin-form", "60 60 95 60 60 95")])
        + "<KeyformBindingSource xs.id='binding-alias' xs.ref='binding'/><KeyformBindingSource xs.id='binding'>"
        "<CParameterGuid xs.n='parameterGuid' uuid='parameter-guid'/><array_list xs.n='keys' count='2'><f>-1</f><f>1</f>"
        "</array_list><InterpolationType xs.n='interpolationType' v='LINEAR'/><ExtendedInterpolationType "
        "xs.n='extendedInterpolationType' v='LINEAR'/><i xs.n='insertPointCount'>0</i>"
        "<f xs.n='extendedInterpolationScale'>1</f></KeyformBindingSource>" + part("root") + part("alternate")
        + "<GTexture2D xs.id='texture'><GTexture xs.n='super'><GTextureGuid xs.n='guid' uuid='texture-guid'/><FilterMode "
        "xs.n='filterMode'><MinFilter xs.n='minFilter' v='LINEAR'/><MagFilter xs.n='magFilter' v='LINEAR'/></FilterMode>"
        "</GTexture><CImageResource xs.n='srcImageResource' width='2' height='2'><file xs.n='imageFileBuf' path='mesh.png'/>"
        "</CImageResource><null xs.n='transformImageResource01toLogical01'/></GTexture2D></root>")
    path = workdir / "runtime.cmo3"
    path.write_bytes(caff_pack()(xml.encode()))
    return path


def runtime_setup(workdir, name, document):
    """Write one runtime JSON file into the run directory and open the synthetic runtime model."""
    (workdir / name).write_text(json.dumps(document))
    return [("open", {"path": str(runtime_model(workdir))})]


def runtime_state(client, key):
    """One section of the snapshot resource (nativePhysics, nativePose, nativePlayback, values); {} while absent."""
    return snapshot(client).get(key) or {}


PHYSICS3 = {
    "Version": 3,
    "Meta": {"Fps": 60, "PhysicsSettingCount": 1, "TotalInputCount": 1, "TotalOutputCount": 1, "VertexCount": 2,
             "EffectiveForces": {"Gravity": {"X": 0, "Y": -1}, "Wind": {"X": 0, "Y": 0}}},
    "PhysicsSettings": [{
        "Id": "hair",
        "Input": [{"Source": {"Target": "Parameter", "Id": "ParamTest"}, "Weight": 100, "Type": "X", "Reflect": False}],
        "Output": [{"Destination": {"Target": "Parameter", "Id": "ParamTest"}, "VertexIndex": 1, "Scale": 2, "Weight": 100,
                    "Type": "Angle", "Reflect": False}],
        "Vertices": [{"Position": {"X": 0, "Y": 0}, "Mobility": 1, "Delay": 1, "Acceleration": 1, "Radius": 0},
                     {"Position": {"X": 0, "Y": 3}, "Mobility": 0.9, "Delay": 0, "Acceleration": 1, "Radius": 3}],
        "Normalization": {"Position": {"Minimum": -1, "Default": 0, "Maximum": 1},
                          "Angle": {"Minimum": -1, "Default": 0, "Maximum": 1}},
    }],
}
EXPRESSION3 = {"Type": "Live2D Expression", "Parameters": [{"Id": "ParamTest", "Value": 0.3, "Blend": "Add"}]}
POSE3 = {"Type": "Live2D Pose", "Groups": [[{"Id": "root"}, {"Id": "alternate"}]]}


def physics_runtime_script(client, workdir):
    client.call("live2d_physics_attach", {"path": str(workdir / "hair.physics3.json")})
    client.call("live2d_physics_step", {"dt": 0.05})
    client.call("live2d_physics_reset")
    client.call("live2d_physics_options_set", {"gravity": {"x": 0, "y": -1}, "wind": {"x": 1, "y": 0}})
    client.call("live2d_physics_stabilize")


def physics_runtime_done(client):
    state = runtime_state(client, "nativePhysics")
    return (state.get("options", {}).get("wind") == {"x": 1.0, "y": 0.0}
            and abs(state.get("parameterValues", {}).get("ParamTest", 0) + 1) < 1e-6)


def expression_script(client, workdir):
    client.call("live2d_parameter_set", {"parameterId": "ParamTest", "value": -0.5})
    client.call("live2d_expression_import", {"name": "smile", "path": str(workdir / "smile.exp3.json")})
    client.call("live2d_expression_list")
    client.call("live2d_expression_apply", {"name": "smile"})
    client.call("live2d_expression_reset")
    client.call("live2d_expression_apply", {"name": "smile"})


def expression_done(client):
    # -0.5 set by live2d_parameter_set plus the expression's 0.3 (Blend Add).
    return (runtime_state(client, "nativePlayback").get("expressionName") == "smile"
            and abs(runtime_state(client, "values").get("ParamTest", 0) + 0.2) < 1e-6)


def pose_script(client, workdir):
    client.call("live2d_pose_attach", {"path": str(workdir / "arms.pose3.json")})
    client.call("live2d_pose_step", {"dt": 1, "signals": {"root": 0, "alternate": 1}})
    client.call("live2d_pose_reset")
    client.call("live2d_pose_step", {"dt": 1, "signals": {"root": 0, "alternate": 1}})


# ---- mesh-deform
def mesh_deform_state(client):
    """(rest vertices of the Part 'face', its 'deform' binding data on 'Agent param' or None), read from resources."""
    model = model_resource(client)
    face = next(n for n in tree(model["nodes"]) if n["name"] == "face")
    param = next(p for p in model["param"] if p["name"] == "Agent param")
    verts = client.read(f"resource://soul2d/resources/{face['uuid']}")["item"]["data"]["mesh"]["verts"]
    binding = client.read(f"resource://soul2d/bindings/get?parameter={param['uuid']}&target={face['uuid']}&name=deform")
    return verts, (binding.get("item") or {}).get("data")


def mesh_deform_script(client, workdir):
    face = node_named(client, "face")["uuid"]
    param = next(p for p in client.call("parameters") if p["name"] == "Agent param")["uuid"]
    client.call("mesh_edit", {"id": face, "edits": [{"op": "move", "indices": [0], "delta": [1, 0]}]})
    deform = {"id": face, "parameter": param, "key": [2, 0], "tool": "translate", "selected": [0, 1, 2, 3]}
    client.call("deform_edit", {**deform, "delta": [4, 0]})
    client.call("undo")
    client.call("deform_edit", {**deform, "delta": [6, 0]})


def mesh_deform_done(client):
    verts, binding = mesh_deform_state(client)
    return verts[:2] == [-7, -8] and binding is not None and binding["isSet"][2][0] and binding["values"][2][0][0] == [6, 0]


# ---- motion3
def motion3_ends_at(client, value):
    # The snapshot's motion3 is the seek preview (null until a bound seek), so the stored clip is read with the tool.
    return client.call("motion3_read", {"name": "Agent motion"})["Curves"][0]["Segments"][-1] == value


def motion3_script(client, workdir):
    client.call("motion3_list")
    assert motion3_ends_at(client, 10)
    client.call("motion3_point_set", {"name": "Agent motion", "curve": 0, "point": 1, "value": 5})
    client.call("undo")
    assert motion3_ends_at(client, 10)
    client.call("motion3_point_set", {"name": "Agent motion", "curve": 0, "point": 1, "value": 5})
    client.call("motion3_export", {"name": "Agent motion", "path": str(workdir / "agent.motion3.json")})


def exported_motion_ends_at_five(workdir):
    path = workdir / "agent.motion3.json"
    return path.exists() and json.loads(path.read_text())["Curves"][0]["Segments"][-1] == 5


# ---- parameters-bindings
def parameter_item(client, name):
    uuid = next((p["uuid"] for p in model_resource(client)["param"] if p["name"] == name), None)
    return None if uuid is None else client.read(f"resource://soul2d/resources/{uuid}")["item"]


def parameters_script(client, workdir):
    def rename():
        client.call("parameter_rename", {"id": client.call("parameters")[0]["uuid"], "name": "Agent param renamed"})
    rename()
    client.call("undo")
    assert client.call("parameters")[0]["name"] == "Agent param"
    rename()


def parameters_done(client):
    item = parameter_item(client, "Agent param renamed")
    return (item is not None and item["typeId"] == "Parameter" and item["data"]["name"] == "Agent param renamed"
            and parameter_item(client, "Agent param") is None)


# ---- physics
def physics_script(client, workdir):
    nodes = [node_named(client, "Agent physics")["uuid"]]
    client.call("physics_set", {"nodes": nodes, "gravity": 2.5})
    client.call("undo")
    client.call("physics_set", {"nodes": nodes, "gravity": 2.5})
    client.call("physics_step", {"dt": 0.1})


# ---- render-scene
def render_scene_script(client, workdir):
    client.call("sketch_snapshot", {"opacity": 0.5})
    client.call("undo")
    client.call("sketch_snapshot", {"opacity": 0.5})
    client.call("node_visibility_set", {"id": node_named(client, "face")["uuid"], "enabled": False})


def guide_over_hidden_face(client):
    """The 50% opacity Snapshot guide exists and the original Part 'face' is hidden."""
    nodes = {node["name"]: node for node in tree(model_resource(client)["nodes"])}
    return nodes.get("Snapshot", {}).get("opacity") == 0.5 and nodes.get("face", {}).get("enabled") is False


# ---- session
def session_script(client, workdir):
    client.call("puppet_meta_set", {"property": "name", "value": "Agent puppet"})
    client.call("undo")
    client.call("puppet_meta_set", {"property": "name", "value": "Agent puppet"})
    client.call("puppet_meta_set", {"property": "artist", "value": "ACP agent"})


def session_done(client):
    meta = model_resource(client)["meta"]
    return meta.get("name") == "Agent puppet" and meta.get("artist") == "ACP agent"


# ---- split
def split_script(client, workdir):
    client.call("split_open", {"path": str(workdir / "layers/face.png")})
    client.call("split_part_set", {"name": "body", "steps": [{"op": "alpha", "threshold": 1}]})
    assert [p["name"] for p in client.read("resource://soul2d/part-split/plan")["parts"]] == ["body"]
    client.call("split_plan")
    client.call("split_apply")


# ---- textures
def textures_script(client, workdir):
    swap = {"slot": 0, "path": str(workdir / "extra.png"), "allowResize": True}
    client.call("texture_set", swap)
    client.call("undo")
    client.call("texture_set", swap)


FAMILIES = {
    # One workflow per family map. setup: calls (or a function of the run directory that writes assets and answers
    # the calls) run before every run; steps: the agent's instructions ({dir} is the run directory); script: the same
    # workflow as a headless client's calls; must_call: a tool the agent must call besides the edit; check: the final
    # state, read from resources; save: the saved file (None for state a document file does not carry); reopen: open
    # the saved file and check again; artifact: a check of an exported file; render_prep: calls that make the result
    # drawable before the harness renders it (the agent then only saves); render_check: a check after render_prep.
    "animation": {
        "setup": [("animation_create", {"name": "Agent clip"})],
        "steps": "1. snapshot and read model.animations. 2. animation_rename name 'Agent clip' to new_name 'Agent clip "
                 "renamed'. 3. undo. 4. snapshot and confirm the old name is back. 5. animation_rename it again. "
                 "6. animation_set name 'Agent clip renamed' length 90.",
        "script": animation_script,
        "must_call": "undo",
        "check": animation_done,
    },
    "atlas-export": {
        "setup": import_layers,
        "steps": "1. nodes_list and note the uuid of the Part named 'face'. 2. atlas_layout_set with parts [{\"id\": <that "
                 "uuid>, \"atlas\": 0, \"x\": 5, \"y\": 7}]. 3. atlas_build with resolution 64 and padding 0 and confirm "
                 "the face mapping is [5, 7, 16, 16]. 4. undo. 5. atlas_build again and confirm the mapping is [0, 0, 16, "
                 "16]. 6. atlas_layout_set again. 7. export_inp with path {dir}/agent.inp, atlasResolution 64 and padding 0.",
        "script": atlas_script,
        "must_call": "undo",
        "check": atlas_pinned,
        # The INP carries the atlas-baked UVs, which the saved INX does not.
        "artifact": exported_inp_is_pinned,
    },
    "automesh": {
        "setup": import_layers,
        "steps": "1. cv_analyze with method distance, size [3, 3] and pixels [0, 0, 0, 0, 1, 0, 0, 0, 0], then "
                 "automesh_preset_list (read only; never save or delete a preset). 2. nodes_list and note the uuid of the "
                 "Part named 'face'. 3. node_get it and count the vertices in mesh.verts (flat x, y pairs). "
                 "4. automesh_generate with that id and algorithm grid. 5. undo. 6. automesh_generate with that id, "
                 "algorithm grid, x_segments 4 and y_segments 4. 7. node_get it again and count the vertices.",
        "script": automesh_script,
        "must_call": "undo",
        "check": lambda client: part_vertex_count(client) == 49,
    },
    "deformers": {
        "setup": [("node_create", {"className": "GridDeformer", "name": "Agent grid"})],
        "steps": "1. nodes_list. 2. deformer_define_grid with nodes [the uuid of 'Agent grid'], axisX [-1, 0, 1] and "
                 "axisY [-1, 0, 1]. 3. node_get it and confirm grid_axis_x is [-1, 0, 1]. 4. undo. 5. node_get and "
                 "confirm the axes have two values again. 6. deformer_define_grid again with the same axes.",
        "script": deformers_script,
        "must_call": "undo",
        "check": lambda client: node_data(client, "Agent grid").get("grid_axis_x") == [-1.0, 0.0, 1.0],
    },
    "depth-bones-rig": {
        "setup": [],
        "steps": "1. DepthBoneCommand_CreateDepthRigRoot with name 'Agent rig'; note created[0].uuid (the root). "
                 "2. DepthBoneCommand_AddDepthBone with parent the root, boneId 'Agent bone', restHead [0, 0, 0] and "
                 "restTail [0, 10, 0]; note created[0].uuid (the bone). 3. rig_set_rest with bone, restHead [0, 0, 0], "
                 "restTail [0, 20, 0] and restRoll 0. 4. rig_list_bones with root and confirm restTail is [0, 20, 0]. "
                 "5. undo. 6. rig_list_bones and confirm restTail is [0, 10, 0] again. 7. rig_set_rest again with the "
                 "same arguments. 8. rig_drag_bone with root, bone and translation [3, 0, 0].",
        "script": rig_script,
        "must_call": "undo",
        "check": rig_done,
    },
    "depth-map-draw": {
        "setup": depth_setup,
        "steps": "1. nodes_list and note the uuid of 'Agent grid'. 2. depth_draw_load_png with path {dir}/depth.png and id "
                 "'agent-depth'. 3. depth_draw_binding_update with layer 'agent-depth' and target that uuid. "
                 "4. depth_draw_compose with that target. 5. depth_draw_apply with that target. 6. depth_list with that "
                 "target and confirm 4 depths of about 0.6. 7. undo. 8. depth_list and confirm count -1. "
                 "9. depth_draw_apply again.",
        "script": depth_script,
        "must_call": "undo",
        "check": depths_done,
    },
    "history": {
        "setup": [("node_create", {"className": "Node", "name": "Agent target"})],
        "steps": "1. nodes_list. 2. node_rename the node named 'Agent target' to 'Agent renamed'. 3. history_summary. "
                 "4. undo. 5. redo. 6. nodes_list and confirm the name is 'Agent renamed'. 7. history_summary.",
        "script": history_script,
        "must_call": "redo",
        "check": history_done,
        # The undo stack is session state: a reopened file starts a fresh history.
        "reopen": False,
    },
    "import-merge": {
        "setup": import_layers,
        "steps": "1. nodes_list. 2. merge_images with paths [\"{dir}/extra.png\"]. 3. nodes_list and confirm a Part named "
                 "'extra.png'. 4. undo. 5. nodes_list and confirm it is gone. 6. merge_images again.",
        "script": merge_script,
        "must_call": "undo",
        "check": lambda client: "extra.png" in node_names(client),
    },
    "live2d-animation": {
        "setup": lambda workdir: [("open", {"path": str(can3_fixture(workdir))})],
        "steps": "1. history_summary and note the revision. 2. live2d_animation_key_add with sceneGuid 'scene-guid', "
                 "trackGuid 'track-guid', curveId 'live2dParam_ParamTest', anchorFrame 5, value 0.8 and expectedRevision "
                 "the revision from step 1. 3. undo. 4. history_summary. 5. live2d_animation_key_add with the same "
                 "arguments and the new revision.",
        "script": can3_script,
        "must_call": "undo",
        "check": lambda client: can3_keys(client) == [(0, 0.0), (5, 0.8), (10, 1.0)],
        "save": "agent.can3",
        "render_prep": can3_drawable,
        "render_check": lambda client: client.read("resource://soul2d/document/state")["values"].get("ParamTest") == 0.8,
    },
    "live2d-authoring": {
        "setup": lambda workdir: [("open", {"path": str(live2d_fixture(workdir))})],
        "steps": "1. parameters and note the guid and maximum of ParamTest. 2. history_summary and note the revision. "
                 "3. live2d_parameter_edit with that guid, maximum 2 and expectedRevision from step 2. 4. parameters and "
                 "confirm maximum 2. 5. undo. 6. parameters and confirm the old maximum. 7. history_summary, then "
                 "live2d_parameter_edit again with the current revision. 8. live2d_physics3_read, then history_summary, "
                 "then live2d_physics3_edit with group 0, field 'name', value 'Agent sway' and the current expectedRevision.",
        "script": authoring_script,
        "must_call": "live2d_physics3_edit",
        "check": authoring_done,
        "save": "agent.cmo3",
    },
    "live2d-expression": {
        "setup": lambda workdir: runtime_setup(workdir, "smile.exp3.json", EXPRESSION3),
        "steps": "1. live2d_parameter_set with parameterId ParamTest and value -0.5. 2. live2d_expression_import with name "
                 "smile and path {dir}/smile.exp3.json. 3. live2d_expression_list. 4. live2d_expression_apply with name "
                 "smile. 5. live2d_expression_reset, then snapshot and confirm values.ParamTest is back to -0.5. "
                 "6. live2d_expression_apply with name smile again.",
        "script": expression_script,
        "must_call": "live2d_expression_reset",
        "check": expression_done,
        "save": None,
    },
    "live2d-physics": {
        "setup": lambda workdir: runtime_setup(workdir, "hair.physics3.json", PHYSICS3)
        + [("live2d_parameter_set", {"parameterId": "ParamTest", "value": 1})],
        "steps": "1. live2d_physics_attach with path {dir}/hair.physics3.json. 2. live2d_physics_step with dt 0.05. "
                 "3. live2d_physics_reset, then snapshot and confirm nativePhysics.parameterValues is empty and "
                 "values.ParamTest is back to 1. 4. live2d_physics_options_set with gravity {\"x\": 0, \"y\": -1} and wind "
                 "{\"x\": 1, \"y\": 0}. 5. live2d_physics_stabilize.",
        "script": physics_runtime_script,
        "must_call": "live2d_physics_reset",
        "check": physics_runtime_done,
        "save": None,
    },
    "live2d-pose": {
        "setup": lambda workdir: runtime_setup(workdir, "arms.pose3.json", POSE3),
        "steps": "1. live2d_pose_attach with path {dir}/arms.pose3.json. 2. live2d_pose_step with dt 1 and signals "
                 "{\"root\": 0, \"alternate\": 1}. 3. live2d_pose_reset, then snapshot and confirm nativePose.partOpacities "
                 "is back to root-guid 1 and alternate-guid 0. 4. live2d_pose_step again with dt 1 and the same signals.",
        "script": pose_script,
        "must_call": "live2d_pose_reset",
        "check": lambda client: runtime_state(client, "nativePose").get("partOpacities") == {"root-guid": 0.0, "alternate-guid": 1.0},
        "save": None,
    },
    "mesh-deform": {
        "setup": lambda workdir: import_layers(workdir) + [("parameter_create", {"name": "Agent param", "dimensions": 1})],
        "steps": "1. nodes_list for the id of the Part 'face', and parameters for the uuid of 'Agent param'. 2. mesh_edit "
                 "with id the face id and edits [{\"op\": \"move\", \"indices\": [0], \"delta\": [1, 0]}]. 3. deform_edit "
                 "with id the face id, parameter the param uuid, key [2, 0], tool \"translate\", selected [0, 1, 2, 3] and "
                 "delta [4, 0]. 4. undo. 5. snapshot and confirm model.param for 'Agent param' has no bindings left. "
                 "6. deform_edit again with the same arguments but delta [6, 0].",
        "script": mesh_deform_script,
        "must_call": "undo",
        "check": mesh_deform_done,
    },
    "motion3": {
        "setup": import_motion,
        "steps": "1. motion3_list. 2. motion3_read name 'Agent motion' and note Curves[0].Segments (it ends with 10). "
                 "3. motion3_point_set name 'Agent motion', curve 0, point 1, value 5. 4. motion3_read and confirm the "
                 "last segment value is 5. 5. undo. 6. motion3_read and confirm 10 is back. 7. motion3_point_set again. "
                 "8. motion3_export name 'Agent motion' path {dir}/agent.motion3.json.",
        "script": motion3_script,
        "must_call": "motion3_export",
        "check": lambda client: motion3_ends_at(client, 5),
        "artifact": exported_motion_ends_at_five,
    },
    "nodes-inspector": {
        "setup": [("node_create", {"className": "Node", "name": "Agent target"})],
        "steps": "1. nodes_list. 2. node_rename the node named 'Agent target' to 'Agent renamed' (id from step 1). "
                 "3. undo. 4. nodes_list and confirm the name is back. 5. node_rename it to 'Agent renamed' again.",
        "script": nodes_script,
        "must_call": "undo",
        "check": nodes_done,
    },
    "parameters-bindings": {
        "setup": [("parameter_create", {"name": "Agent param", "dimensions": 1})],
        "steps": "1. parameters. 2. parameter_rename the parameter named 'Agent param' to 'Agent param renamed' (id "
                 "from step 1). 3. undo. 4. parameters and confirm the name is back. 5. parameter_rename it again.",
        "script": parameters_script,
        "must_call": "undo",
        "check": parameters_done,
    },
    "physics": {
        "setup": [("node_create", {"className": "SimplePhysics", "name": "Agent physics"})],
        "steps": "1. nodes_list. 2. physics_set with nodes [that uuid] and gravity 2.5. 3. node_get that uuid and "
                 "confirm gravity 2.5. 4. undo. 5. node_get and confirm gravity is back. 6. physics_set gravity 2.5 "
                 "again. 7. physics_step dt 0.1.",
        "script": physics_script,
        "must_call": "physics_step",
        "check": lambda client: node_data(client, "Agent physics").get("gravity") == 2.5,
    },
    "render-scene": {
        "setup": import_layers,
        "steps": "1. sketch_snapshot with opacity 0.5. 2. undo. 3. nodes_list and confirm no node is named 'Snapshot'. "
                 "4. sketch_snapshot with opacity 0.5 again. 5. nodes_list, then node_visibility_set with id of the node "
                 "named 'face' and enabled false.",
        "script": render_scene_script,
        "must_call": "undo",
        "check": guide_over_hidden_face,
    },
    "session": {
        "setup": [],
        "steps": "1. snapshot and read model.meta. 2. puppet_meta_set property 'name' value 'Agent puppet'. 3. undo. "
                 "4. snapshot and confirm meta.name is back. 5. puppet_meta_set property 'name' value 'Agent puppet' "
                 "again. 6. puppet_meta_set property 'artist' value 'ACP agent'.",
        "script": session_script,
        "must_call": "undo",
        "check": session_done,
    },
    "split": {
        "setup": layers_only,
        "steps": "1. split_open with path {dir}/layers/face.png. 2. split_part_set name 'body' with steps "
                 "[{\"op\": \"alpha\", \"threshold\": 1}]. 3. split_plan and confirm one part named 'body'. 4. split_apply. "
                 "5. nodes_list and confirm a Part named 'body'.",
        "script": split_script,
        "must_call": "split_plan",
        "check": lambda client: "body" in node_names(client),
    },
    "textures": {
        "setup": import_layers,
        "steps": "1. texture_get slot 0 and note the width (16). 2. texture_set slot 0, path {dir}/extra.png, "
                 "allowResize true. 3. texture_get slot 0 and confirm width 32. 4. undo. 5. texture_get and confirm "
                 "width 16 again. 6. texture_set slot 0 path {dir}/extra.png allowResize true again.",
        "script": textures_script,
        "must_call": "undo",
        "check": lambda client: client.read("resource://soul2d/textures/0").get("width") == 32,
    },
}



def editor_reset(workdir):
    """The layers, then the view of a fresh window: nothing selected, the grid off at 100, Solo off and zoom 1."""
    return import_layers(workdir) + [("editor_selection_set", {"nodes": []}),
                                     ("editor_grid_set", {"show": False, "spacing": 100}),
                                     ("editor_solo_set", {"show": False}), ("editor_camera_set", {"zoom": 1})]


def editor_script(client, workdir):
    face = node_named(client, "face")["uuid"]
    for tool, args in [("editor_selection_set", {"nodes": [face]}), ("editor_grid_set", {"show": True, "spacing": 25}),
                       ("editor_grid_set", {"show": False}), ("editor_grid_set", {"show": True, "spacing": 25}),
                       ("editor_solo_set", {"show": True}), ("editor_camera_set", {"zoom": 3})]:
        client.call(tool, args)


def editor_view_done(client):
    """resource://soul2d/editor/state: face selected, the grid shown at 25, Solo on and zoom 3."""
    state = client.read("resource://soul2d/editor/state")
    face = node_named(client, "face")["uuid"]
    return (state.get("editor") is True and state["selection"]["nodes"] == [face] and state["grid"]["show"] is True
            and state["grid"]["spacing"] == 25 and state["solo"]["show"] is True and state["camera"]["zoom"] == 3)


# The view tools change no document, so nothing is saved and the revision must stay where the setup left it; the
# harness renders the document afterwards, and the panel run keeps a screenshot of the window as well.
EDITOR_FAMILY = {
    "setup": editor_reset,
    "steps": "1. nodes_list and note the UUID of the Part named 'face'. 2. editor_selection_set with nodes [that UUID]. "
             "3. editor_grid_set show true, spacing 25. 4. editor_grid_set show false. 5. editor_grid_set show true, "
             "spacing 25 again. 6. editor_solo_set show true. 7. editor_camera_set zoom 3.",
    "script": editor_script,
    "must_call": "editor_solo_set",
    "check": editor_view_done,
    "save": None,
    "render_prep": lambda workdir: [],
    "unmoved": True,
}


class Core:
    def __init__(self, binary, njc):
        self.njc = njc
        self.process = subprocess.Popen([binary, "--mcp-port", "0"], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        self.url = None
        self.token = None
        deadline = time.time() + 30
        while time.time() < deadline and (self.url is None or self.token is None):
            line = self.process.stderr.readline()
            if not line:
                break
            if match := re.search(r"listening on (http://\S+/mcp)", line):
                self.url = match.group(1)
            if match := re.search(r"bearer token written to (.+)$", line.strip()):
                self.token = Path(match.group(1)).read_text().splitlines()[1]
        if self.url is None or self.token is None:
            raise SystemExit("soul2d-core did not report its address and token")
        self.port = self.url.rsplit(":", 1)[1].split("/")[0]

    def rpc(self, *command):
        message = json.loads(subprocess.run([self.njc, "--port", self.port, *command], capture_output=True, text=True,
                                            timeout=120).stdout)
        if "error" in message:
            raise RuntimeError(f"{command}: {message['error']}")
        return message["result"]

    def call(self, tool, args=None):
        result = self.rpc("tools", "call", tool, "--json", json.dumps(args or {}))
        text = result["content"][0].get("text", "") if result.get("content") else ""
        if result.get("isError"):
            raise RuntimeError(f"{tool}: {text}")
        if "structuredContent" in result:
            return result["structuredContent"]
        return parse(text)

    def read(self, uri):
        """A resource: its text parsed as JSON (or the text), or {blob, mimeType} for a binary resource."""
        content = self.rpc("resources", "read", uri)["contents"][0]
        if "text" in content:
            return parse(content["text"])
        return {"blob": content.get("blob"), "mimeType": content.get("mimeType")}

    def close(self):
        self.process.terminate()
        self.process.wait(timeout=10)


class Agent:
    def __init__(self, cwd, mcp_url, token, model):
        env = dict(os.environ, ACP_CLAUDE_MODEL=model)
        self.process = subprocess.Popen([sys.executable, "-I", str(BRIDGE)], cwd=cwd, env=env, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.next_id = 1
        self.updates = []
        try:
            self.send("initialize", {"protocolVersion": 1,
                                     "clientCapabilities": {"fs": {"readTextFile": False, "writeTextFile": False}}})
            init = self.wait_response(1)
            assert init["result"]["agentCapabilities"]["mcpCapabilities"]["http"] is True, init
            servers = [{"type": "http", "name": "soul2d", "url": mcp_url,
                        "headers": [{"name": "Authorization", "value": f"Bearer {token}"}]}]
            self.send("session/new", {"cwd": cwd, "mcpServers": servers})
            self.session = self.wait_response(2)["result"]["sessionId"]
        except BaseException:
            self.stop()
            raise

    def send(self, method, params):
        message = {"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params}
        self.next_id += 1
        self.process.stdin.write((json.dumps(message) + "\n").encode())
        self.process.stdin.flush()
        return message["id"]

    def wait_response(self, id, timeout=900):
        deadline = time.time() + timeout
        while time.time() < deadline:
            line = self.process.stdout.readline()
            if not line:
                raise RuntimeError(f"bridge ended: {self.process.stderr.read().decode()[-2000:]}")
            message = json.loads(line)
            if message.get("method") == "session/update":
                self.updates.append(message["params"]["update"])
            elif message.get("id") == id:
                return message
        raise RuntimeError("timed out waiting for the agent")

    def prompt(self, text):
        id = self.send("session/prompt", {"sessionId": self.session, "prompt": [{"type": "text", "text": text}]})
        return self.wait_response(id)["result"]

    def stop(self):
        """End the bridge (closing its input ends the claude process it runs), killing it if it does not exit, so an
        agent whose turn failed cannot go on calling tools on the shared host."""
        try:
            self.process.stdin.close()
        except OSError:
            pass
        try:
            self.process.wait(timeout=30)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()



@functools.cache
def gui_harness():
    """tools/verify-panels-gui.py, whose Editor drives a soul2d-editor window through the accessibility tree."""
    spec = importlib.util.spec_from_file_location("verify_panels_gui", ROOT / "tools/verify-panels-gui.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class EditorMcp:
    """`call` and `read` like Core's, over the stdio MCP of a soul2d-editor (the GUI harness's `Mcp`)."""

    def __init__(self, mcp):
        self.mcp = mcp

    def call(self, tool, args=None):
        result = self.mcp.rpc("tools/call", {"name": tool, "arguments": args or {}})
        text = next((item.get("text", "") for item in result.get("content", []) if item.get("type") == "text"), "")
        if result.get("isError"):
            raise RuntimeError(f"{tool}: {text}")
        return result["structuredContent"] if "structuredContent" in result else parse(text)

    def read(self, uri):
        content = self.mcp.rpc("resources/read", {"uri": uri})["contents"][0]
        if "text" in content:
            return parse(content["text"])
        return {"blob": content.get("blob"), "mimeType": content.get("mimeType")}


@contextlib.contextmanager
def stdio_editor(binary, run):
    """A soul2d-editor with its settings, autosave and recovery data in `run`, driven only over its stdio MCP."""
    gui = gui_harness()
    run.mkdir(parents=True)
    with (run / "mcp-wire.jsonl").open("w") as wire, (run / "editor-stderr.log").open("w") as stderr:
        process = subprocess.Popen([binary, "--mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr,
                                   text=True, cwd=run, env=gui.harness.editor_env(run))
        try:
            mcp = gui.Mcp(process, wire)
            mcp.rpc("initialize", {"protocolVersion": "2025-03-26", "capabilities": {},
                                   "clientInfo": {"name": "verify-acp-agent", "version": "1"}})
            yield EditorMcp(mcp)
        finally:
            gui.stop_owned(process)


def agent_record(updates, stop_reason, seconds, model, spec):
    """The agent's part of a run record, from the session updates it streamed."""
    tool_calls = [u for u in updates if u.get("sessionUpdate") == "tool_call"]
    failed_calls = [u for u in updates if u.get("sessionUpdate") == "tool_call_update" and u.get("status") == "failed"]
    texts = "".join(u["content"]["text"] for u in updates if u.get("sessionUpdate") == "agent_message_chunk")
    usage = next((u for u in updates if u.get("sessionUpdate") == "usage_update"), {})
    called = any(u.get("title", "").endswith("__" + spec["must_call"]) for u in tool_calls)
    return {
        "model": model, "stop_reason": stop_reason, "seconds": round(seconds, 1),
        "tool_calls": [u.get("title") for u in tool_calls],
        "failed_calls": [u["content"][0]["content"]["text"][:300] for u in failed_calls],
        "usage": {"totalCostUsd": usage.get("totalCostUsd"), "numTurns": usage.get("numTurns"), **(usage.get("usage") or {})},
        "agent_text": texts, "checks": {f"{spec['must_call']} was called": called, "no tool call failed": not failed_calls},
        "updates": updates,
    }


def wait_for_stop(log, timeout):
    """The entries of the bridge's ACP_CLAUDE_LOG once a whole line holds the turn's stopReason."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        if log.exists():
            entries = [json.loads(line) for line in log.read_text(encoding="utf-8").split("\n")[:-1] if line.strip()]
            if any("stopReason" in entry for entry in entries):
                return entries
        time.sleep(2)
    raise RuntimeError(f"the bridge logged no stop reason within {timeout} s")


def panel_agent_run(binary, spec, root, model):
    """The workflow as a real agent makes it in the editor's agent panel. The harness sets the document and the view
    up over the editor's stdio MCP, presses the panel's controls through the accessibility tree, types the steps and
    waits for the stop reason in the bridge's log; the checks then read the same editor over stdio. A run the screen
    lock (or a control the tree does not offer) keeps from being made or observed is NOT_RUN, never a FAIL."""
    gui = gui_harness()
    if gui.screen_locked():
        return {"not_run": "the screen is locked, so the agent panel can be neither observed nor pressed"}
    workdir = root / "agent"
    suite = types.SimpleNamespace(run=root / "panel", binary=Path(binary), foreground_drags=False)
    suite.run.mkdir(parents=True)
    log = suite.run / "editor" / "bridge.jsonl"  # the Editor's own directory, made before Start opens the log
    command = " ".join(shlex.quote(part) for part in
                       ["env", f"ACP_CLAUDE_MODEL={model}", f"ACP_CLAUDE_LOG={log}", sys.executable, "-I", str(BRIDGE)])
    daemon = ed = None
    try:
        daemon = gui.harness.ensure_daemon({})
        ed = gui.Editor(suite, "editor", None, {"ACP.Command": command, "ACP.Workdir": str(workdir)})
        client = EditorMcp(ed.mcp)
        before = prepare(client, spec, workdir)
        ed.click(wait_for_control(ed, "Coding agent", "panels", 30))  # a collapsing header: an AXButton, not a text
        ed.click(ed.one(ed.state("opened"), "Enable coding agent (ACP)", "AXCheckBox"))
        state = ed.state("enabled")
        shown = ed.after(state, "Command", "AXTextField").get("value") or ""
        if shown != command:
            return {"checks": {"the Command field shows the preset ACP.Command": False},
                    "errors": {"command field": shown[:200]}}
        ed.click(ed.one(state, "Start", "AXButton"))
        ed.wait_text("Status: Initialized", "initialized", seconds=60)
        prompt = STEPS.format(steps=spec["steps"].replace("{dir}", str(workdir)), finish=finish_text(spec, workdir))
        ed.type_into(ed.after(ed.state("message-box"), "User message:", "AXTextField"), prompt)
        typed = ed.after(ed.state("message-typed"), "User message:", "AXTextField").get("value") or ""
        if typed != prompt:
            raise gui.Check(f"the message field holds {len(typed)} characters, the prompt has {len(prompt)}")
        started = time.time()
        ed.click(ed.one(ed.state("before-send"), "Send", "AXButton"))
        entries = wait_for_stop(log, 900)
        stop = next(entry["stopReason"] for entry in entries if "stopReason" in entry)
        record = agent_record([e["update"] for e in entries if "update" in e], stop, time.time() - started, model, spec)
        record["prompt"] = prompt
        try:
            ed.wait_text("Status: Done", "done", seconds=30)
            record["checks"]["the panel shows Status: Done"] = True
            record["screenshot"] = str(ed.state("final-view")["_png"])
        except (AssertionError, gui.Check, gui.Inconclusive) as error:
            record["panel"] = f"{type(error).__name__}: {error}"[:300]
            if isinstance(error, (gui.Check, gui.Inconclusive)) or gui.screen_locked():  # Check: an incomplete tree
                record["not_run"] = f"the panel could not be observed after the turn: {record['panel']}"
            else:
                record["checks"]["the panel shows Status: Done"] = False
        shared, errors = outcome(client, spec, workdir, before)
        record["checks"].update(shared)
        record["errors"] = errors
        return record
    except (gui.Check, gui.Inconclusive) as reason:
        return {"not_run": f"{type(reason).__name__}: {reason}"[:500]}
    except AssertionError as error:
        if gui.screen_locked():  # a lock mid-run fails the driver's own assertions
            return {"not_run": f"the screen locked during the run: {error}"[:500]}
        raise
    finally:
        if ed is not None:
            ed.stop()
        gui.stop_owned(daemon)


def wait_for_control(ed, label, name, seconds):
    """The first element labelled `label` (any role) once the window shows it."""
    deadline = time.monotonic() + seconds
    while True:
        found = ed.find(ed.state(name), label)
        if found:
            return found[0]
        if time.monotonic() > deadline:
            raise gui_harness().Check(f"{label!r} did not appear within {seconds} s")
        time.sleep(1)


def setup_calls(spec, workdir):
    setup = spec["setup"]
    return setup(workdir) if callable(setup) else setup


def holds(spec, core):
    """The family's check, with an error (a missing node, a refused read) counted as not holding."""
    try:
        return bool(spec["check"](core)), None
    except Exception as error:  # noqa: BLE001 - the error text is kept in the record
        return False, f"{type(error).__name__}: {error}"[:300]


def prepare(core, spec, workdir):
    """A fresh document with the family's setup; answers the revision the run starts from."""
    workdir.mkdir(parents=True)
    core.call("new")
    for tool, args in setup_calls(spec, workdir):
        core.call(tool, args)
    return core.call("history_summary")["revision"]


def harness_render(core, spec, workdir, checks):
    """Bounds and a 64x64 render; with render_prep, first the calls that make the saved result drawable."""
    if "render_prep" in spec:
        for tool, args in spec["render_prep"](workdir):
            core.call(tool, args)
        if "render_check" in spec:
            checks["the drawn result holds the final state"] = bool(spec["render_check"](core))
    bounds = core.call("bounds")["bounds"]
    core.call("render", {"width": 64, "height": 64, "bounds": bounds, "path": str(workdir / "agent.png")})


def outcome(core, spec, workdir, before):
    """The checks every run shares, after the workflow and its save (and, without render_prep, its render)."""
    checks, errors = {}, {}
    checks["final state in the live session"], errors["live"] = holds(spec, core)
    if spec.get("unmoved"):
        checks["revision did not move"] = core.call("history_summary")["revision"] == before
    save = spec.get("save", "agent.inx")
    if save is not None:
        checks["revision moved"] = core.call("history_summary")["revision"] > before
        saved = workdir / save
        checks["saved file exists"] = saved.exists()
        if saved.exists() and spec.get("reopen", True):
            core.call("open", {"path": str(saved)})
            checks["reopened file holds the final state"], errors["reopened"] = holds(spec, core)
    if "artifact" in spec:
        checks["the exported artifact holds the final state"] = bool(spec["artifact"](workdir))
    if "render_prep" in spec:
        harness_render(core, spec, workdir, checks)
    png = workdir / "agent.png"
    checks["render wrote a PNG"] = png.exists() and png.read_bytes()[:8] == PNG_SIGNATURE
    return checks, {key: value for key, value in errors.items() if value}


def negative_control(core, spec, workdir):
    """The setup alone, saved and reopened like a run: the check must not hold, or it proves nothing."""
    prepare(core, spec, workdir)
    save = spec.get("save", "agent.inx")
    if save is not None:
        core.call("save", {"path": str(workdir / save)})
        if spec.get("reopen", True):
            core.call("open", {"path": str(workdir / save)})
    held, error = holds(spec, core)
    return {"checks": {"the check fails without the workflow": not held}, "errors": {"control": error} if error else {}}


def client_run(core, spec, workdir):
    """The workflow as a headless MCP client makes it: the family's scripted calls, then save, bounds and render."""
    before = prepare(core, spec, workdir)
    started = time.time()
    spec["script"](core, workdir)
    save = spec.get("save", "agent.inx")
    if save is not None:
        core.call("save", {"path": str(workdir / save)})
    if "render_prep" not in spec:
        harness_render(core, spec, workdir, {})
    checks, errors = outcome(core, spec, workdir, before)
    return {"checks": checks, "errors": errors, "seconds": round(time.time() - started, 1)}


def finish_text(spec, workdir):
    """What the prompt asks after the steps: save and render, or only save (or nothing) when the harness renders."""
    save = spec.get("save", "agent.inx")
    if "render_prep" in spec:
        return SAVE.format(dir=workdir, save=save) if save is not None else ""
    return (SAVE_AND_RENDER if save is not None else RENDER).format(dir=workdir, save=save)


def agent_run(core, spec, workdir, model):
    """The workflow as a real ACP agent makes it, told the steps in one prompt."""
    before = prepare(core, spec, workdir)
    steps = spec["steps"]
    steps = steps(workdir) if callable(steps) else steps
    agent = Agent(str(workdir), core.url, core.token, model)
    started = time.time()
    try:
        result = agent.prompt(STEPS.format(steps=steps.replace("{dir}", str(workdir)), finish=finish_text(spec, workdir)))
        duration = time.time() - started
    finally:
        agent.stop()
    record = agent_record(agent.updates, result.get("stopReason"), duration, model, spec)
    shared, errors = outcome(core, spec, workdir, before)
    record["checks"].update(shared)
    record["errors"] = errors
    return record


def guarded(run, *args):
    """A run that raised (a refused call, a failed assertion of the script) is recorded as failed, not fatal."""
    try:
        return run(*args)
    except Exception as error:  # noqa: BLE001 - the sweep goes on with the next run
        return {"checks": {"the run completed": False}, "errors": {"raised": f"{type(error).__name__}: {error}"[:500]}}


def run_family(name, spec, core, model, out_dir, with_agent):
    root = Path(tempfile.mkdtemp(prefix=f"acp-{name}-"))
    record = {"family": name, "dir": str(root), "control": guarded(negative_control, core, spec, root / "control"),
              "client": guarded(client_run, core, spec, root / "client")}
    if with_agent:
        record["agent"] = guarded(agent_run, core, spec, root / "agent", model)
    return write_record(record, out_dir)


def run_editor_family(binary, model, out_dir, with_agent):
    """editor-state: the control and the scripted client over one editor's stdio MCP, the agent in another's panel."""
    root = Path(tempfile.mkdtemp(prefix="acp-editor-state-"))
    record = {"family": "editor-state", "dir": str(root)}
    try:
        with stdio_editor(binary, root / "stdio") as client:
            record["control"] = guarded(negative_control, client, EDITOR_FAMILY, root / "control")
            record["client"] = guarded(client_run, client, EDITOR_FAMILY, root / "client")
    except Exception as error:  # noqa: BLE001 - the editor did not start or answer; the sweep goes on
        failed = {"checks": {"the run completed": False}, "errors": {"raised": f"{type(error).__name__}: {error}"[:500]}}
        record.setdefault("control", failed)
        record.setdefault("client", failed)
    if with_agent:
        record["agent"] = guarded(panel_agent_run, binary, EDITOR_FAMILY, root, model)
    return write_record(record, out_dir)


def write_record(record, out_dir):
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / f"run-{record['family']}.json").write_text(json.dumps(record, indent=1, ensure_ascii=False))
    return record


def verdict(run):
    """A failed check outranks NOT_RUN: a run that could not be observed to the end may still be seen to fail."""
    return "FAIL" if not all(run.get("checks", {}).values()) else "NOT_RUN" if "not_run" in run else "ok"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--core", required=True)
    parser.add_argument("--njc", required=True)
    parser.add_argument("--editor", help="a soul2d-editor binary: adds the editor-state family")
    parser.add_argument("--families", help="comma-separated families (default: all, editor-state only with --editor)")
    parser.add_argument("--model", default="claude-haiku-5-5")
    parser.add_argument("--no-agent", action="store_true", help="run the negative control and the scripted client only")
    parser.add_argument("--out", default=str(ROOT / "build/verification/acp-agent"))
    args = parser.parse_args()
    known = list(FAMILIES) + (["editor-state"] if args.editor else [])
    names = args.families.split(",") if args.families else known
    unknown = [name for name in names if name not in known]
    if unknown:
        parser.error(f"unknown families: {', '.join(unknown)} (editor-state needs --editor)")
    core = Core(args.core, args.njc)
    passed = 0
    try:
        for name in names:
            if name == "editor-state":
                record = run_editor_family(str(Path(args.editor).resolve()), args.model, Path(args.out),
                                           not args.no_agent)
            else:
                record = run_family(name, FAMILIES[name], core, args.model, Path(args.out), not args.no_agent)
            runs = {key: record[key] for key in ("control", "client", "agent") if key in record}
            verdicts = {key: verdict(run) for key, run in runs.items()}
            overall = "PASS" if set(verdicts.values()) == {"ok"} else "FAIL" if "FAIL" in verdicts.values() else "NOT_RUN"
            passed += overall == "PASS"
            line = " ".join(f"{key}={value}" for key, value in verdicts.items())
            agent = record.get("agent", {})
            if "stop_reason" in agent:
                line += (f" stop={agent['stop_reason']} tool_calls={len(agent['tool_calls'])} turns={agent['usage'].get('numTurns')}"
                         f" cost_usd={agent['usage'].get('totalCostUsd')} {agent['seconds']}s")
            failing = {key: run["not_run"] if verdicts[key] == "NOT_RUN"
                       else {check: ok for check, ok in run["checks"].items() if not ok} | run.get("errors", {})
                       for key, run in runs.items() if verdicts[key] != "ok"}
            print(f"{overall} {name}: {line}" + (f" failing={failing}" if failing else ""), flush=True)
    finally:
        core.close()
    sys.exit(0 if passed == len(names) else 1)


if __name__ == "__main__":
    main()
