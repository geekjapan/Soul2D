#!/usr/bin/env python3
"""Compare native CMO3 reads with an independent XML oracle and all transports."""
import argparse
from collections import Counter, defaultdict
import ctypes
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import struct
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET
import zlib

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_INPUTS = ROOT / "build/verification/live2d-inputs-vjq5llby"
sys.dont_write_bytecode = True


def import_tool(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


CAFF = import_tool("verify_caff_authoring", ROOT / "tools/verify-caff-authoring.py")
CORE = import_tool("verify_rust_core", ROOT / "tools/verify-rust-core.py")


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False, allow_nan=False) + "\n")


class XMLGraph:
    def __init__(self, xml):
        self.root = ET.fromstring(xml)
        self.ids = {}
        for node in self.root.iter():
            identifier = node.get("xs.id")
            if identifier is not None:
                if not identifier or identifier in self.ids:
                    raise ValueError("duplicate or empty authoring xs.id")
                self.ids[identifier] = node

    def resolve(self, node):
        seen = set()
        for _ in range(65):
            reference = node.get("xs.ref")
            if reference is None:
                return node
            if node in seen:
                raise ValueError("authoring xs.ref alias cycle")
            seen.add(node)
            node = self.ids[reference]
        raise ValueError("authoring xs.ref chain exceeds 64 links")

    def field(self, node, name):
        node = self.resolve(node)
        seen = set()
        for _ in range(65):
            if node in seen:
                raise ValueError("authoring super cycle")
            seen.add(node)
            field = next((child for child in node if child.get("xs.n") == name), None)
            if field is not None:
                return self.resolve(field)
            parent = next((child for child in node if child.get("xs.n") == "super"), None)
            if parent is None:
                return None
            node = self.resolve(parent)
        raise ValueError("authoring super chain exceeds 64 nodes")

    def required(self, node, name):
        result = self.field(node, name)
        if result is None:
            raise ValueError(f"authoring field {name} is missing")
        return result

    def items(self, node):
        node = self.resolve(node)
        children = [self.resolve(child) for child in node if isinstance(child.tag, str)]
        count = node.get("count")
        if count is not None and int(count) != len(children):
            raise ValueError("authoring array count does not match content")
        return children

    def named_items(self, node, name):
        return self.items(self.required(node, name))

    def source_items(self, model, name):
        return self.items(self.required(self.required(model, name), "_sources"))

    def text(self, node, name, default=None):
        field = self.field(node, name)
        return default if field is None or field.text is None else field.text

    def number(self, node, name, default=None):
        text = self.text(node, name)
        if text is None:
            if default is None:
                raise ValueError(f"authoring field {name} lacks number")
            return default
        value = float(text.strip())
        if not (float("-inf") < value < float("inf")):
            raise ValueError(f"non-finite authoring number {name}")
        return value

    def integer(self, node, name, default=None):
        text = self.text(node, name)
        if text is None:
            if default is None:
                raise ValueError(f"authoring field {name} lacks integer")
            return default
        return int(text)

    def unsigned(self, node, name):
        value = self.integer(node, name)
        if not 0 <= value <= 0xFFFFFFFF:
            raise ValueError(f"invalid authoring unsigned integer {name}")
        return value

    def boolean(self, node, name, default):
        text = self.text(node, name)
        if text is None:
            return default
        if text == "true":
            return True
        if text == "false":
            return False
        raise ValueError(f"invalid authoring boolean {name}")

    def enum(self, node, name, default=None):
        field = self.field(node, name)
        if field is None:
            if default is None:
                raise ValueError(f"authoring field {name} lacks enum value")
            return default
        value = field.get("v")
        if value is None:
            raise ValueError(f"authoring field {name} lacks enum value")
        return value

    def guid(self, node, name, required=False):
        value = self.field(node, name)
        if value is None or value.tag == "null":
            if required:
                raise ValueError(f"authoring source lacks {name}")
            return None
        guid = value.get("uuid")
        if guid is None or not guid.strip():
            raise ValueError(f"authoring {name} lacks uuid")
        return guid

    def floats(self, node):
        node = self.resolve(node)
        if node.tag == "array_list":
            values = []
            for child in self.items(node):
                if child.tag != "f" or child.text is None:
                    raise ValueError("authoring numeric list contains a non-float")
                values.append(finite(child.text))
            return values
        if node.tag != "float-array":
            raise ValueError("authoring positions must be a float-array")
        values = [finite(value) for value in (node.text or "").split()]
        self.check_count(node, len(values))
        return values

    def indices(self, node):
        node = self.resolve(node)
        if node.tag != "int-array":
            raise ValueError("authoring indices must be an int-array")
        values = [int(value) for value in (node.text or "").split()]
        if any(value < 0 or value > 0xFFFFFFFF for value in values):
            raise ValueError("invalid authoring index")
        self.check_count(node, len(values))
        return values

    @staticmethod
    def check_count(node, actual):
        count = node.get("count")
        if count is not None and int(count) != actual:
            raise ValueError("authoring array count does not match content")


def finite(text):
    value = float(text.strip())
    if not (float("-inf") < value < float("inf")):
        raise ValueError("invalid finite authoring number")
    return value


def color(graph, node, name, default):
    value = graph.field(node, name)
    if value is None:
        return default
    return [finite(value.get(channel)) for channel in ("red", "green", "blue", "alpha")]


def guid_list(graph, node, name):
    field = graph.field(node, name)
    if field is None:
        return []
    output = []
    for item in graph.items(field):
        guid = item.get("uuid")
        if guid is None or not guid.strip():
            raise ValueError("authoring GUID list lacks uuid")
        output.append(guid)
    return output


def grid(graph, source, parameters, field_name="keyformGridSource"):
    root = graph.field(source, field_name)
    if root is None or root.tag == "null":
        return None
    bindings = graph.named_items(root, "keyformBindings")
    axes = []
    axis_index = {}
    for ref in bindings:
        ref = graph.resolve(ref)
        binding_ref = ref.get("xs.id")
        if ref.tag != "KeyformBindingSource" or not binding_ref or binding_ref in axis_index:
            raise ValueError("invalid or duplicate authoring keyform binding")
        parameter_guid = graph.guid(ref, "parameterGuid", required=True)
        # Only extended grids may keep bindings to parameters the editor deleted.
        if parameter_guid not in parameters and field_name != "extendedKeyformGridSource":
            raise ValueError(f"unknown authoring parameter GUID {parameter_guid!r}")
        keys = graph.floats(graph.required(ref, "keys"))
        if not keys or any(left >= right for left, right in zip(keys, keys[1:])):
            raise ValueError("authoring keyform keys are empty or unsorted")
        axis_index[binding_ref] = len(axes)
        axes.append({
            "bindingRef": binding_ref,
            "parameterGuid": parameter_guid,
            "parameterId": parameters.get(parameter_guid),
            "keys": keys,
            "interpolation": graph.enum(ref, "interpolationType"),
            "extendedInterpolation": graph.enum(ref, "extendedInterpolationType"),
            "insertPointCount": graph.unsigned(ref, "insertPointCount"),
            "extendedInterpolationScale": graph.number(ref, "extendedInterpolationScale"),
        })
    cells = []
    seen = set()
    for cell in graph.named_items(root, "keyformsOnGrid"):
        coordinate_items = graph.named_items(graph.required(cell, "accessKey"), "_keyOnParameterList")
        indices = [None] * len(axes)
        for coordinate in coordinate_items:
            binding = graph.resolve(graph.required(coordinate, "binding"))
            binding_ref = binding.get("xs.id")
            if binding_ref not in axis_index:
                raise ValueError("authoring keyform cell refers to unknown binding")
            index = axis_index[binding_ref]
            key = graph.unsigned(coordinate, "keyIndex")
            if key >= len(axes[index]["keys"]) or indices[index] is not None:
                raise ValueError("invalid or repeated authoring keyform coordinate")
            indices[index] = key
        if len(coordinate_items) != len(axes) or any(value is None for value in indices):
            raise ValueError("authoring keyform cell has the wrong coordinate count")
        coordinates = tuple(indices)
        if coordinates in seen:
            raise ValueError("duplicate authoring keyform cell coordinate")
        seen.add(coordinates)
        cells.append({"indices": indices, "formGuid": graph.guid(cell, "keyformGuid", required=True)})
    return {"axes": axes, "cells": cells}


def morph_targets(graph, source, parameters, forms):
    target_set = graph.field(source, "keyformMorphTargetSet")
    if target_set is None or target_set.tag == "null":
        return None
    if target_set.tag != "KeyFormMorphTargetSet":
        raise ValueError("invalid authoring morph target set")
    form_guids = {form["guid"] for form in forms}
    targets = []
    for target in graph.named_items(target_set, "_morphTargets"):
        if target.tag != "KeyFormMorphTarget":
            raise ValueError("invalid authoring morph target")
        if graph.resolve(graph.required(target, "owner")) is not graph.resolve(source):
            raise ValueError("authoring morph target belongs to another source")
        parameter_guid = graph.guid(target, "parameterGuid", required=True)
        if parameter_guid not in parameters:
            raise ValueError(f"unknown authoring morph parameter GUID {parameter_guid!r}")
        form_guid = graph.guid(target, "keyformGuid", required=True)
        if form_guid not in form_guids:
            raise ValueError("authoring morph target references a form outside its source")
        edit_base = graph.required(target, "editBaseParameterMap")
        if edit_base.tag != "MorphTargetEditBaseParameterMap":
            raise ValueError("invalid authoring morph edit base parameter map")
        base_map = graph.required(edit_base, "baseParameterMap")
        if base_map.tag != "null":
            raise ValueError("unsupported authoring morph edit base parameter map")
        targets.append({
            "parameterGuid": parameter_guid,
            "parameterId": parameters[parameter_guid],
            "keyValue": graph.number(target, "keyValue"),
            "formGuid": form_guid,
            "editBaseParameterMap": None,
        })

    target_parameter_guids = {target["parameterGuid"] for target in targets}
    constraint_set = graph.required(target_set, "blendWeightConstraintSet")
    if constraint_set.tag != "MorphTargetBlendWeightConstraintSet":
        raise ValueError("invalid authoring morph blend weight constraint set")
    constraints = []
    for constraint in graph.named_items(constraint_set, "_constraints"):
        if constraint.tag != "MorphTargetBlendWeightConstraint":
            raise ValueError("invalid authoring morph blend weight constraint")
        morph_parameter_guid = graph.guid(constraint, "morphTargetParameterGuid", required=True)
        constraint_parameter_guid = graph.guid(constraint, "constraintParameterGuid", required=True)
        if morph_parameter_guid not in parameters or constraint_parameter_guid not in parameters:
            raise ValueError("authoring morph references an unknown parameter")
        if morph_parameter_guid not in target_parameter_guids:
            raise ValueError(
                "authoring blend constraint references a parameter without a morph target in its source")
        constraints.append({
            "morphTargetParameterGuid": morph_parameter_guid,
            "morphTargetParameterId": parameters[morph_parameter_guid],
            "constraintParameterGuid": constraint_parameter_guid,
            "constraintParameterId": parameters[constraint_parameter_guid],
            "constraintParameterValue": graph.number(constraint, "constraintParameterValue"),
            "blendWeight": graph.number(constraint, "blendWeight"),
        })
    return {"targets": targets, "blendWeightConstraints": constraints}


def read_form(graph, node, data, generated=False):
    coordinate = graph.field(node, "coordType")
    coordinate_type = None
    if coordinate is not None:
        coordinate_type = graph.text(coordinate, "coordName")
        if coordinate_type is None:
            raise ValueError("authoring coordType lacks coordName")
    vertex_count_matches_source = True
    if data["type"] == "ArtMesh" and node.tag == "CArtMeshForm":
        positions = graph.floats(graph.required(node, "positions"))
        if generated and (not positions or len(positions) % 2):
            raise ValueError("invalid generated authoring mesh form geometry")
        vertex_count_matches_source = len(positions) == len(data["positions"])
        if not generated and not vertex_count_matches_source:
            raise ValueError("authoring mesh form vertex count differs from source")
        form_data = {"type": "ArtMesh", "positions": positions}
    elif data["type"] == "Warp" and node.tag == "CWarpDeformerForm":
        positions = graph.floats(graph.required(node, "positions"))
        expected = (data["columns"] + 1) * (data["rows"] + 1) * 2
        if generated and (not positions or len(positions) % 2):
            raise ValueError("invalid generated authoring warp form geometry")
        vertex_count_matches_source = len(positions) == expected
        if not generated and not vertex_count_matches_source:
            raise ValueError("authoring warp form grid size differs from source")
        form_data = {"type": "Warp", "positions": positions}
    elif data["type"] == "Rotation" and node.tag == "CRotationDeformerForm":
        form_data = {
            "type": "Rotation", "angle": finite(node.get("angle")),
            "origin": [finite(node.get("originX")), finite(node.get("originY"))],
            "scale": finite(node.get("scale")),
            "reflectX": parse_bool(node.get("isReflectX")),
            "reflectY": parse_bool(node.get("isReflectY")),
        }
    elif data["type"] == "Part" and node.tag == "CPartForm":
        form_data = {"type": "Part"}
    elif data["type"] == "Glue" and node.tag == "CGlueForm":
        form_data = {"type": "Glue", "intensity": graph.number(node, "intensity")}
    elif data["type"] == "ArtPath" and node.tag == "CArtPathForm":
        points = []
        for point in graph.named_items(node, "positions"):
            if point.tag != "CArtPathPoint":
                raise ValueError("authoring ArtPath form holds a non-point")
            curve = graph.required(point, "curvePointPosition")
            vectors = {name: graph.required(curve, field) for name, field in (
                ("position", "point"), ("startVelocity", "startVelocity"), ("endVelocity", "endVelocity"))}
            corner = graph.boolean(point, "isCorner", None)
            if graph.required(point, "ctrlPtId").tag != "CArtPathPointGuid" or corner is None:
                raise ValueError("invalid authoring ArtPath point")
            points.append({
                "guid": graph.guid(point, "ctrlPtId", required=True),
                **{name: [graph.number(vector, "x"), graph.number(vector, "y")] for name, vector in vectors.items()},
                "width": graph.number(point, "width"), "opacity": graph.number(point, "opacity"),
                "color": [graph.number(point, f"color{channel}") for channel in ("Red", "Green", "Blue", "Alpha")],
                "corner": corner,
            })
        form_data = {"type": "ArtPath", "points": points}
    else:
        raise ValueError("authoring form type does not match source")
    if form_data["type"] not in ("Part", "Glue") and coordinate_type is None:
        raise ValueError("authoring geometry form lacks coordinate type")
    draw_order = graph.text(node, "drawOrder")
    result = {
        "guid": graph.guid(node, "guid", required=True),
        "coordinateType": coordinate_type,
        "animated": graph.boolean(node, "isAnimatedForm", False),
        "localAnimated": graph.boolean(node, "isLocalAnimatedForm", False),
        "opacity": graph.number(node, "opacity", 1.0),
        "drawOrder": None if draw_order is None else int(draw_order),
        "multiplyColor": color(graph, node, "multiplyColor", [1.0] * 4),
        "screenColor": color(graph, node, "screenColor", [0.0, 0.0, 0.0, 1.0]),
        "data": form_data,
    }
    if generated:
        result["vertexCountMatchesSource"] = vertex_count_matches_source
    return result


def read_source_forms(graph, source, rows, data, generated=False):
    forms = []
    seen_form_ids = set()
    seen_objects = set()
    for form in graph.items(rows):
        if form in seen_objects:
            continue
        seen_objects.add(form)
        owner = graph.required(form, "_source")
        if graph.resolve(owner) is not graph.resolve(source):
            raise ValueError("authoring form belongs to another source")
        parsed = read_form(graph, form, data, generated=generated)
        identity = (parsed["guid"], parsed["coordinateType"])
        if identity in seen_form_ids:
            raise ValueError("duplicate authoring form GUID and coordinate type")
        seen_form_ids.add(identity)
        forms.append(parsed)
    return forms


def extended_morph_targets(graph, extension, parameters, available_form_guids):
    target_set = graph.field(extension, "_extendedKeyformMorphTargetSet")
    if target_set is None or target_set.tag == "null":
        return None
    if target_set.tag != "ExtendedKeyFormMorphTargetSet":
        raise ValueError("invalid authoring extended morph target set")

    targets = []
    for target in graph.named_items(target_set, "_morphTargets"):
        if target.tag != "ExtendedKeyFormMorphTarget":
            raise ValueError("invalid authoring extended morph target")
        parameter_guid = graph.guid(target, "parameterGuid", required=True)
        if parameter_guid not in parameters:
            raise ValueError(f"unknown authoring extended morph parameter GUID {parameter_guid!r}")
        form_guid = graph.guid(target, "keyformGuid", required=True)
        targets.append({
            "parameterGuid": parameter_guid,
            "parameterId": parameters[parameter_guid],
            "keyValue": graph.number(target, "keyValue"),
            "formGuid": form_guid,
            "formAvailable": form_guid in available_form_guids,
        })

    settings = []
    settings_node = graph.required(target_set, "extendedInterpolationSettings")
    seen_parameters = set()
    if settings_node is not None:
        if settings_node.tag != "hash_map":
            raise ValueError("invalid authoring extended interpolation settings")
        for entry in graph.items(settings_node):
            if entry.tag != "entry":
                raise ValueError("invalid authoring extended interpolation setting entry")
            parameter_guid = graph.guid(entry, "key", required=True)
            if parameter_guid not in parameters:
                raise ValueError(
                    f"unknown authoring extended interpolation setting parameter GUID {parameter_guid!r}")
            if parameter_guid in seen_parameters:
                raise ValueError("duplicate authoring extended interpolation setting parameter GUID")
            seen_parameters.add(parameter_guid)
            value = graph.required(entry, "value")
            if value.tag != "ExtendedInterpolationSetting":
                raise ValueError("invalid authoring extended interpolation setting")
            settings.append({
                "parameterGuid": parameter_guid,
                "parameterId": parameters[parameter_guid],
                "interpolation": graph.enum(value, "type"),
                "insertPointCount": graph.unsigned(value, "insertPointCount"),
                "extendedInterpolationScale": graph.number(value, "extendedInterpolationScale"),
            })
    return {"targets": targets, "settings": settings}


def extended_interpolations(graph, source, parameters, data, forms):
    extensions = graph.field(source, "_extensions")
    if extensions is None or extensions.tag == "null":
        return []

    staged = []
    extension_guids = set()
    for extension in graph.items(extensions):
        if extension.tag != "CExtendedInterpolationExtension":
            continue
        parent = graph.required(extension, "super")
        owner = graph.required(parent, "_owner")
        if graph.resolve(owner) is not graph.resolve(source):
            raise ValueError("authoring extended interpolation belongs to another source")
        legacy_forms = graph.field(extension, "extendedKeyforms")
        if legacy_forms is not None:
            if (graph.field(extension, "normalExtendedKeyforms") is not None
                    or graph.field(extension, "morphTargetExtendedKeyforms") is not None):
                raise ValueError("mixed historical and modern extended form lists")
            normal_forms = read_source_forms(graph, source, legacy_forms, data, generated=True)
            morph_forms = []
        else:
            normal_forms = read_source_forms(
                graph, source, graph.required(extension, "normalExtendedKeyforms"),
                data, generated=True)
            morph_forms = read_source_forms(
                graph, source, graph.required(extension, "morphTargetExtendedKeyforms"),
                data, generated=True)
        guid = graph.guid(parent, "guid", required=True)
        if guid in extension_guids:
            raise ValueError("duplicate authoring extended interpolation GUID")
        extension_guids.add(guid)
        staged.append((extension, {
            "guid": guid,
            "grid": None,
            "normalForms": normal_forms,
            "morphForms": morph_forms,
            "morphTargets": None,
        }))

    for extension, record in staged:
        available_form_guids = {form["guid"] for form in forms + record["normalForms"] + record["morphForms"]}
        keyform_grid = grid(graph, extension, parameters, "extendedKeyformGridSource")
        missing = []
        if keyform_grid is not None:
            axes = keyform_grid["axes"]
            unresolved = any(axis["parameterId"] is None for axis in axes)
            for cell in keyform_grid["cells"]:
                if cell["formGuid"] in available_form_guids:
                    continue
                # Only generated inserted points may lack a stored form; primary positions stay mandatory.
                inserted = unresolved or any(
                    axis["parameterId"] is not None and index % (axis["insertPointCount"] + 1) != 0
                    for index, axis in zip(cell["indices"], axes))
                if not inserted:
                    raise ValueError("authoring extended grid references a form outside its source")
                missing.append(cell["formGuid"])
        if missing:
            record["missingFormGuids"] = missing
        record["grid"] = keyform_grid
        record["morphTargets"] = extended_morph_targets(
            graph, extension, parameters, available_form_guids)
    return [record for _, record in staged]


def parse_bool(value):
    if value == "true":
        return True
    if value == "false":
        return False
    raise ValueError("invalid authoring boolean")


def read_texture(graph, node, entries):
    image = graph.required(node, "srcImageResource")
    file_node = graph.required(image, "imageFileBuf")
    entry = file_node.get("path")
    if not entry or entry not in entries:
        raise ValueError(f"authoring image entry {entry!r} is missing")
    image_size = [int(image.get(name)) for name in ("width", "height")]
    if any(value <= 0 or value > 0xFFFFFFFF for value in image_size):
        raise ValueError("invalid authoring image dimensions")
    transform = graph.required(node, "transformImageResource01toLogical01")
    image_to_logical = None
    if transform.tag != "null":
        image_to_logical = [finite(transform.get(name)) for name in
                            ("m00", "m01", "m02", "m10", "m11", "m12")]
    filter_node = graph.required(node, "filterMode")
    return {
        "guid": graph.guid(node, "guid", required=True),
        "name": graph.text(node, "name", ""),
        "imageEntry": entry,
        "imageSize": image_size,
        "imageToLogical": image_to_logical,
        "wrapMode": graph.enum(node, "wrapMode", "CLAMP_TO_BORDER"),
        "minFilter": graph.enum(filter_node, "minFilter", "LINEAR"),
        "magFilter": graph.enum(filter_node, "magFilter", "LINEAR"),
        "premultiplied": graph.boolean(node, "isPremultiplied", True),
    }


def validate_hierarchy(root, sources):
    sentinel = "71fae776-e218-4aee-873e-78e8ac0cb48a"
    by_guid = {source["guid"]: source for source in sources}
    if root == sentinel:
        raise ValueError("authoring root part guid conflicts with root deformer")
    if sentinel in by_guid:
        raise ValueError("authoring source shadows root deformer")
    if len(by_guid) != len(sources):
        raise ValueError("authoring source guids are not unique")
    if by_guid[root]["parentPartGuid"] is not None:
        raise ValueError("authoring root part has a parent")
    for field, allowed, missing, wrong, cycle in (
        ("parentPartGuid", {"Part"}, "authoring parent part does not resolve",
         "authoring parent guid does not identify a part", "authoring parent hierarchy contains a cycle"),
        ("targetDeformerGuid", {"Warp", "Rotation"}, "authoring target deformer does not resolve",
         "authoring target guid does not identify a deformer", "authoring deformer targets contain a cycle"),
    ):
        children = {guid: [] for guid in by_guid}
        indegrees = dict.fromkeys(by_guid, 0)
        for source in sources:
            target = source[field]
            if target is None or (field == "targetDeformerGuid" and target == sentinel):
                continue
            if (field == "targetDeformerGuid" and source["data"]["type"] == "Part"
                    and target == "00000000-0000-0000-0000-000000000000"):
                continue
            if target not in by_guid:
                raise ValueError(missing)
            if by_guid[target]["data"]["type"] not in allowed:
                raise ValueError(wrong)
            if field == "targetDeformerGuid" and target == source["guid"]:
                raise ValueError("authoring deformer targets itself")
            children[target].append(source["guid"])
            indegrees[source["guid"]] += 1
        ready = [guid for guid, degree in indegrees.items() if degree == 0]
        visited = 0
        while ready:
            visited += 1
            for child in children[ready.pop()]:
                indegrees[child] -= 1
                if indegrees[child] == 0:
                    ready.append(child)
        if visited != len(sources):
            raise ValueError(cycle)


def verbatim_element(xml, node):
    """The element's XML exactly as the file holds it, found in the text by its xs.id, or by its xs.n when inline."""
    text = xml if isinstance(xml, bytes) else xml.encode()
    key, value = ("xs.id", node.get("xs.id")) if node.get("xs.id") else ("xs.n", node.get("xs.n"))
    tag = re.escape(node.tag.encode())
    opening = re.compile(rb"<" + tag + rb"\b[^>]*\b" + re.escape(key.encode()) + rb"=([\"'])" + re.escape(value.encode())
                         + rb"\1[^>]*?(/?)>")
    found = list(opening.finditer(text))
    if len(found) != 1:
        raise ValueError(f"{len(found)} elements match the {node.tag} to copy verbatim")
    start = found[0]
    if start.group(2):
        return text[start.start():start.end()].decode()
    depth = 1
    for mark in re.compile(rb"<(/?)" + tag + rb"\b[^>]*?(/?)>").finditer(text, start.end()):
        if mark.group(1):
            depth -= 1
        elif not mark.group(2):
            depth += 1
        if depth == 0:
            return text[start.start():mark.end()].decode()
    raise ValueError(f"the {node.tag} to copy verbatim is not closed")


BRUSH_GROUPS = (
    ("textureUvData", ("up", "upInter", "center", "downInter", "down", "inStart", "repeatStart", "repeatEnd", "outEnd")),
    ("fixWidthData", ("upFixedWidth", "downFixedWidth")),
    ("spriteData", ("density", "useTextureMagnification")),
)


def read_brushes(graph, entries):
    # Located by tag, independently of the model field the Rust reader follows.
    settings = [node for node in graph.root.iter("CArtPathBrushSetting") if node.get("xs.ref") is None]
    if len(settings) > 1:
        raise ValueError("multiple authoring CArtPathBrushSetting definitions")
    lists = [child for setting in settings for child in setting if child.tag == "carray_list"]
    if len(lists) > 1:
        raise ValueError("authoring brush setting has several brush lists")
    brushes = []
    for node in graph.items(lists[0]) if lists else []:
        if node.tag != "CArtPathBrush":
            raise ValueError("authoring brush list contains a non-brush")
        if graph.required(node, "guid").tag != "CArtPathBrushGuid":
            raise ValueError("authoring guid must use CArtPathBrushGuid")
        brush_id = graph.field(node, "id")
        brush = {
            "guid": graph.guid(node, "guid", required=True),
            "id": "" if brush_id is None else brush_id.get("idstr", ""),
            "name": graph.text(node, "name", ""),
        }
        source = graph.field(node, "textureInput")
        if source is not None:
            texture_input = {}
            texture = graph.field(source, "texture")
            if texture is not None and texture.tag != "null":
                texture_input["texture"] = read_texture(graph, texture, entries)
            for name in ("textureState", "colorComposition"):
                value = graph.field(source, name)
                if value is not None and value.tag != "null":
                    if value.get("v") is None:
                        raise ValueError("authoring brush texture enum lacks v")
                    texture_input[name] = value.get("v")
            current = graph.field(source, "currentTextureInputData")
            if current is not None and current.tag != "null":
                data = {}
                guid = graph.field(current, "_modelImageGuid")
                if guid is not None and guid.tag != "null":
                    if guid.get("uuid") is None:
                        raise ValueError("authoring brush model image guid lacks uuid")
                    data["modelImageGuid"] = guid.get("uuid")
                transform = graph.field(current, "optionalTransformOnCanvas")
                if transform is not None:
                    data["optionalTransformOnCanvas"] = None if transform.tag == "null" else {
                        name: finite(transform.get(name)) for name in ("m00", "m01", "m02", "m10", "m11", "m12")
                        if transform.get(name) is not None}
                texture_input["currentTextureInputData"] = data
            brush["textureInput"] = None if source.tag == "null" else texture_input
        kind = graph.field(node, "drawingCurveType")
        if kind is not None:
            if kind.get("v") is None:
                raise ValueError("authoring brush drawingCurveType lacks v")
            brush["drawingCurveType"] = kind.get("v")
        if graph.text(node, "widthCoefficient") is not None:
            brush["widthCoefficient"] = graph.number(node, "widthCoefficient")
        for group, keys in BRUSH_GROUPS:
            element = graph.field(node, group)
            if element is not None:
                brush[group] = {key: graph.number(element, key) for key in keys if graph.text(element, key) is not None}
        brushes.append(brush)
    return brushes


def read_scene(xml, entries):
    graph = XMLGraph(xml)
    if graph.root.tag != "root":
        raise ValueError("authoring XML root must be root")
    models = [node for node in graph.root.iter("CModelSource")
              if node.get("xs.ref") is None and any(isinstance(child.tag, str) for child in node)]
    if len(models) != 1:
        raise ValueError("authoring source must contain exactly one model")
    model = models[0]
    canvas = graph.required(model, "canvas")
    canvas_size = [graph.unsigned(canvas, "pixelWidth"), graph.unsigned(canvas, "pixelHeight")]
    parameters = []
    parameter_map = {}
    parameter_ids = set()
    for node in graph.source_items(model, "parameterSourceSet"):
        guid = graph.guid(node, "guid", required=True)
        parameter_id_node = graph.required(node, "id")
        parameter_id = parameter_id_node.get("idstr")
        if not parameter_id or not parameter_id.strip() or parameter_id in parameter_ids or guid in parameter_map:
            raise ValueError("duplicate authoring parameter ID or GUID")
        minimum = graph.number(node, "minValue")
        maximum = graph.number(node, "maxValue")
        default = graph.number(node, "defaultValue")
        if not minimum <= default <= maximum:
            raise ValueError("invalid authoring parameter range")
        parameter_ids.add(parameter_id)
        parameter_map[guid] = parameter_id
        parameters.append({
            "guid": guid, "id": parameter_id, "name": graph.text(node, "name", ""),
            "minimum": minimum, "maximum": maximum, "default": default,
            "repeat": graph.boolean(node, "isRepeat", False),
            "parameterType": graph.enum(node, "paramType", "NORMAL"),
        })

    source_nodes = []
    for name in ("drawableSourceSet", "deformerSourceSet", "partSourceSet"):
        source_nodes.extend(graph.source_items(model, name))
    if graph.field(model, "affecterSourceSet") is not None:
        source_nodes.extend(graph.source_items(model, "affecterSourceSet"))
    sources, textures, texture_nodes = [], [], {}
    source_guids = set()
    for node in source_nodes:
        guid = graph.guid(node, "guid", required=True)
        if guid in source_guids:
            raise ValueError("duplicate authoring source GUID")
        source_guids.add(guid)
        tag = node.tag
        if tag == "CArtMeshSource":
            positions = graph.floats(graph.required(node, "positions"))
            uvs = graph.floats(graph.required(node, "uvs"))
            indices = graph.indices(graph.required(node, "indices"))
            if not positions or len(positions) % 2 or len(positions) != len(uvs) or len(indices) % 3:
                raise ValueError("invalid authoring mesh geometry")
            if any(index >= len(positions) // 2 for index in indices):
                raise ValueError("invalid authoring mesh geometry")
            texture = graph.required(node, "texture")
            texture_guid = graph.guid(texture, "guid", required=True)
            if texture_guid in texture_nodes and texture_nodes[texture_guid] is not texture:
                raise ValueError("duplicate authoring texture GUID")
            if texture_guid not in texture_nodes:
                texture_nodes[texture_guid] = texture
                textures.append(read_texture(graph, texture, entries))
            data = {
                "type": "ArtMesh", "positions": positions, "uvs": uvs, "indices": indices,
                "textureGuid": texture_guid,
                "textureState": graph.enum(node, "textureState", "TEXTURE_ATLAS"),
                "culling": graph.boolean(node, "culling", False),
                "colorComposition": graph.enum(node, "colorComposition", "NORMAL"),
                "alphaComposition": graph.enum(node, "alphaComposition", "OVER"),
                "clipGuids": guid_list(graph, node, "clipGuidList"),
                "invertClippingMask": graph.boolean(node, "invertClippingMask", False),
            }
            extensions = graph.field(node, "_extensions")
            editable = [extension for extension in graph.items(extensions)
                if extension.tag == "CEditableMeshExtension"] if extensions is not None else []
            if len(editable) > 1:
                raise ValueError("duplicate editable mesh extension")
            if editable:
                uid_array = graph.required(graph.required(editable[0], "editableMesh"), "pointUid")
                assert uid_array.tag == "int-array"
                uids = graph.indices(uid_array)
                assert len(uids) == len(positions) // 2 and len(set(uids)) == len(uids)
                data["vertexUids"] = uids
        elif tag == "CWarpDeformerSource":
            columns, rows = graph.unsigned(node, "col"), graph.unsigned(node, "row")
            extensions = graph.field(node, "_extensions")
            bezier = []
            if extensions is not None:
                for extension in graph.items(extensions):
                    if extension.tag == "CWarpDeformerBezierExtension":
                        bezier.append([graph.integer(extension, field)
                                       for field in ("editLevel", "bezierCol", "bezierRow")])
            data = {
                "type": "Warp", "columns": columns, "rows": rows,
                "quadTransform": graph.boolean(node, "isQuadTransform", False),
                "bezierSettings": bezier,
            }
        elif tag == "CRotationDeformerSource":
            data = {"type": "Rotation", "baseAngle": graph.number(node, "baseAngle", 0.0)}
        elif tag == "CPartSource":
            data = {
                "type": "Part",
                "childGuids": guid_list(graph, node, "_childGuids"),
                "drawOrderGroup": graph.boolean(node, "enableDrawOrderGroup", False),
                "defaultDrawOrder": graph.integer(node, "defaultOrder_forEditor", 500),
                "sketch": graph.boolean(node, "isSketch", False),
                "useOffscreen": graph.boolean(node, "useOffscreen", False),
                "clipGuids": guid_list(graph, node, "clipGuidList"),
                "invertClippingMask": graph.boolean(node, "invertClippingMask", False),
                "colorComposition": graph.enum(node, "colorComposition", "NORMAL"),
                "alphaComposition": graph.enum(node, "alphaComposition", "OVER"),
            }
        elif tag == "CGlueSource":
            uid_array = graph.required(node, "bindVertexUids")
            assert uid_array.tag == "long-array"
            uids = [int(value) for value in (uid_array.text or "").split()]
            graph.check_count(uid_array, len(uids))
            assert all(0 <= value <= 0x7FFFFFFFFFFFFFFF for value in uids)
            weights = graph.floats(graph.required(node, "weights"))
            assert len(uids) == len(weights) and len(uids) % 2 == 0
            assert all(0 <= weight <= 1 for weight in weights)
            a = graph.required(node, "targetArtMeshA_guid")
            b = graph.required(node, "targetArtMeshB_guid")
            assert a.tag == b.tag == "CDrawableGuid" and a.get("uuid") != b.get("uuid")
            data = {"type": "Glue", "targetArtMeshAGuid": a.get("uuid"),
                "targetArtMeshBGuid": b.get("uuid"), "bindVertexUids": uids, "weights": weights}
        elif tag == "CArtPathSource":
            closed = graph.boolean(node, "isClosed", None)
            if graph.required(node, "brushGuid").tag != "CArtPathBrushGuid" or closed is None:
                raise ValueError("invalid authoring ArtPath source")
            data = {"type": "ArtPath", "closed": closed,
                "divisionCount": graph.unsigned(node, "divNum"),
                "lineAlgorithm": graph.enum(node, "lineAlgorithm"),
                "lineAlignment": graph.enum(node, "lineAlignment"),
                "brushGuid": graph.guid(node, "brushGuid", required=True),
                "clipGuids": guid_list(graph, node, "clipGuidList"),
                "invertClippingMask": graph.boolean(node, "invertClippingMask", False)}
        else:
            raise ValueError(f"unsupported authoring source {tag}")

        forms = read_source_forms(
            graph, node, graph.required(node, "keyforms"), data)
        extended = extended_interpolations(
            graph, node, parameter_map, data, forms)
        keyform_grid = grid(graph, node, parameter_map)
        form_guids = {form["guid"] for form in forms}
        if keyform_grid is not None and any(
            cell["formGuid"] not in form_guids for cell in keyform_grid["cells"]
        ):
            raise ValueError("authoring grid references a form outside its source")
        source_id = graph.required(node, "id").get("idstr")
        if not source_id or not source_id.strip():
            raise ValueError("authoring source lacks ID")
        sources.append({
            "guid": guid, "id": source_id, "name": graph.text(node, "localName", ""),
            "parentPartGuid": graph.guid(node, "parentGuid"),
            "targetDeformerGuid": graph.guid(node, "targetDeformerGuid"),
            "visible": graph.boolean(node, "isVisible", True),
            "locked": graph.boolean(node, "isLocked", False),
            "forms": forms, "grid": keyform_grid, "extendedInterpolations": extended,
            "morphTargets": morph_targets(graph, node, parameter_map, forms), "data": data,
        })

    root_part = graph.required(model, "rootPart")
    root_part_guid = graph.guid(root_part, "guid", required=True)
    if not any(source["guid"] == root_part_guid and source["data"]["type"] == "Part"
               for source in sources):
        raise ValueError("authoring rootPart is absent from the part source set")
    validate_hierarchy(root_part_guid, sources)
    by_guid = {source["guid"]: source for source in sources}
    for source in sources:
        data = source["data"]
        if data["type"] != "Glue":
            continue
        for index, field in enumerate(("targetArtMeshAGuid", "targetArtMeshBGuid")):
            target = by_guid[data[field]]["data"]
            assert target["type"] == "ArtMesh" and "vertexUids" in target
            assert all(value <= 0xFFFFFFFF for value in data["bindVertexUids"][index::2])
        # The editor leaves pairs for deleted vertices behind; record them instead of failing.
        uids = data["bindVertexUids"]
        known = [set(by_guid[data[field]]["data"]["vertexUids"])
                 for field in ("targetArtMeshAGuid", "targetArtMeshBGuid")]
        stale = [pair for pair in range(len(uids) // 2)
                 if uids[2 * pair] not in known[0] or uids[2 * pair + 1] not in known[1]]
        if stale:
            data["staleBindPairs"] = stale
    scene = {
        "format": "live2d.authoring.source.v1",
        "fileFormatVersion": graph.root.get("fileFormatVersion"),
        "name": graph.text(model, "name", ""),
        "canvas": canvas_size,
        "rootPartGuid": root_part_guid,
        "parameters": parameters,
        "sources": sources,
        "textures": textures,
        "brushes": read_brushes(graph, entries),
    }
    physics = graph.field(model, "physicsSettingsSourceSet")
    if physics is not None:
        scene["physicsSource"] = verbatim_element(xml, physics)
    return scene


def scene_summary(scene):
    source_types = Counter(source["data"]["type"] for source in scene["sources"])
    alpha_types = Counter(source["data"]["alphaComposition"] for source in scene["sources"]
                          if source["data"]["type"] == "ArtMesh")
    coordinate_types = Counter()
    extended_types = Counter()
    extended_setting_types = Counter()
    grids = []
    duplicate_guid_coordinates = []
    reused_guids = []
    form_count = 0
    morph_target_count = 0
    blend_weight_constraint_count = 0
    extended_interpolation_count = 0
    extended_grid_count = 0
    normal_extended_form_count = 0
    morph_target_extended_form_count = 0
    extended_morph_target_count = 0
    extended_setting_count = 0
    generated_form_vertex_count_mismatch_count = 0
    unavailable_extended_target_count = 0
    owners_by_guid = defaultdict(set)
    for source in scene["sources"]:
        forms = source["forms"]
        form_count += len(forms)
        if source["morphTargets"] is not None:
            morph_target_count += len(source["morphTargets"]["targets"])
            blend_weight_constraint_count += len(source["morphTargets"]["blendWeightConstraints"])
        by_identity = Counter((form["guid"], form["coordinateType"]) for form in forms)
        duplicate_guid_coordinates.extend(
            {"sourceGuid": source["guid"], "formGuid": guid,
             "coordinateType": coordinate_type, "count": count}
            for (guid, coordinate_type), count in by_identity.items() if count > 1
        )
        by_guid = defaultdict(set)
        for form in forms:
            coordinate_types[str(form["coordinateType"])] += 1
            by_guid[form["guid"]].add(form["coordinateType"])
            owners_by_guid[form["guid"]].add((source["guid"], form["coordinateType"]))
        reused_guids.extend(
            {"sourceGuid": source["guid"], "formGuid": guid,
             "coordinateTypes": sorted(str(value) for value in values)}
            for guid, values in by_guid.items() if len(values) > 1
        )
        if source["grid"] is not None:
            axes, cells = source["grid"]["axes"], source["grid"]["cells"]
            for axis in axes:
                extended_types[axis["extendedInterpolation"]] += 1
            grids.append({"sourceGuid": source["guid"], "axisCount": len(axes),
                          "cellCount": len(cells)})
        for extension in source["extendedInterpolations"]:
            extended_interpolation_count += 1
            normal_extended_form_count += len(extension["normalForms"])
            morph_target_extended_form_count += len(extension["morphForms"])
            generated_form_vertex_count_mismatch_count += sum(
                form.get("vertexCountMatchesSource") is False
                for form in extension["normalForms"] + extension["morphForms"])
            if extension["grid"] is not None:
                extended_grid_count += 1
            if extension["morphTargets"] is not None:
                targets = extension["morphTargets"]["targets"]
                settings = extension["morphTargets"]["settings"]
                extended_morph_target_count += len(targets)
                unavailable_extended_target_count += sum(not target["formAvailable"] for target in targets)
                extended_setting_count += len(settings)
                extended_setting_types.update(setting["interpolation"] for setting in settings)
    axes_histogram = Counter(str(item["axisCount"]) for item in grids)
    cells_histogram = Counter(str(item["cellCount"]) for item in grids)
    return {
        "parameterCount": len(scene["parameters"]),
        "sourceCount": len(scene["sources"]),
        "sourceTypes": dict(sorted(source_types.items())),
        "artMeshAlphaCompositions": dict(sorted(alpha_types.items())),
        "formCount": form_count,
        "morphTargetCount": morph_target_count,
        "blendWeightConstraintCount": blend_weight_constraint_count,
        "coordinateTypes": dict(sorted(coordinate_types.items())),
        "textureCount": len(scene["textures"]),
        "gridCount": len(grids),
        "extendedInterpolationCount": extended_interpolation_count,
        "extendedGridCount": extended_grid_count,
        "normalExtendedFormCount": normal_extended_form_count,
        "morphTargetExtendedFormCount": morph_target_extended_form_count,
        "generatedFormVertexCountMismatchCount": generated_form_vertex_count_mismatch_count,
        "extendedMorphTargetCount": extended_morph_target_count,
        "unavailableExtendedMorphTargetCount": unavailable_extended_target_count,
        "extendedInterpolationSettingCount": extended_setting_count,
        "extendedInterpolationSettingTypes": dict(sorted(extended_setting_types.items())),
        "gridAxisCounts": dict(sorted(axes_histogram.items(), key=lambda pair: int(pair[0]))),
        "gridCellCounts": dict(sorted(cells_histogram.items(), key=lambda pair: int(pair[0]))),
        "extendedInterpolationTypes": dict(sorted(extended_types.items())),
        "fiveAxis243CellGrids": [item for item in grids
                                 if item["axisCount"] == 5 and item["cellCount"] == 243],
        "duplicateGuidCoordinateCases": duplicate_guid_coordinates,
        "sameGuidDifferentCoordinateTypes": reused_guids,
        "crossOwnerFormGuidCount": sum(len({owner for owner, _ in uses}) > 1
                                       for uses in owners_by_guid.values()),
        "crossOwnerMixedCoordinateGuidCount": sum(len({coord for _, coord in uses}) > 1
                                                  for uses in owners_by_guid.values()),
    }


def repack_main_xml(original, entries, xml):
    rewritten = []
    found_main = False
    for names, unknown, flags, reserved, payload in entries:
        if names[0] == "main.xml":
            if found_main:
                raise ValueError("CAFF contains multiple main.xml entries")
            payload = zip_main_xml(payload, xml)
            found_main = True
        rewritten.append((names, unknown, flags, reserved, payload))
    if not found_main:
        raise ValueError("CAFF lacks a main.xml entry")
    return encode_caff(original, rewritten)


def mutate_morph_xml(xml, kind):
    graph = XMLGraph(xml)
    models = [node for node in graph.root.iter("CModelSource")
              if node.get("xs.ref") is None and any(isinstance(child.tag, str) for child in node)]
    if len(models) != 1:
        raise ValueError("morph negative control requires one primary model")
    model = models[0]
    sources = []
    for name in ("drawableSourceSet", "deformerSourceSet", "partSourceSet"):
        sources.extend(graph.source_items(model, name))
    parameter_guids = [
        graph.guid(node, "guid", required=True)
        for node in graph.source_items(model, "parameterSourceSet")
    ]
    target_record = None
    constraint_record = None
    constraint_without_target_record = None
    for source in sources:
        target_set = graph.field(source, "keyformMorphTargetSet")
        if target_set is None or target_set.tag == "null":
            continue
        targets = graph.named_items(target_set, "_morphTargets")
        constraints = graph.named_items(graph.required(target_set, "blendWeightConstraintSet"), "_constraints")
        if target_record is None and targets:
            target_record = (source, targets[0])
        if constraint_record is None and constraints:
            constraint_record = (source, constraints[0])
        target_parameters = {graph.guid(target, "parameterGuid", required=True) for target in targets}
        if constraint_without_target_record is None:
            unknown_morph_parameter = next(
                (guid for guid in parameter_guids if guid not in target_parameters), None)
            if constraints and unknown_morph_parameter is not None:
                constraint_without_target_record = (constraints[0], unknown_morph_parameter)

    if kind == "wrong-owner":
        if target_record is None:
            raise ValueError("no primary morph target for owner control")
        owner_source, target = target_record
        other_source = next((source for source in sources
                             if source is not owner_source and source.get("xs.id")), None)
        owner_field = next((child for child in target if child.get("xs.n") == "owner"), None)
        if other_source is None or owner_field is None:
            raise ValueError("could not prepare wrong morph owner control")
        owner_field.set("xs.ref", other_source.get("xs.id"))
    elif kind == "dangling-form":
        if target_record is None:
            raise ValueError("no primary morph target for form control")
        field = next((child for child in target_record[1]
                      if child.get("xs.n") == "keyformGuid"), None)
        if field is None:
            raise ValueError("morph target lacks keyformGuid")
        field.attrib.pop("xs.ref", None)
        field.set("uuid", "missing-form-guid")
    elif kind == "unknown-parameter":
        if target_record is None:
            raise ValueError("no primary morph target for parameter control")
        field = next((child for child in target_record[1]
                      if child.get("xs.n") == "parameterGuid"), None)
        if field is None:
            raise ValueError("morph target lacks parameterGuid")
        field.attrib.pop("xs.ref", None)
        field.set("uuid", "missing-parameter-guid")
    elif kind == "nonfinite-weight":
        if constraint_record is None:
            raise ValueError("no primary morph constraint for weight control")
        field = next((child for child in constraint_record[1]
                      if child.get("xs.n") == "blendWeight"), None)
        if field is None:
            raise ValueError("morph constraint lacks blendWeight")
        field.text = "NaN"
    elif kind == "constraint-without-target":
        if constraint_without_target_record is None:
            raise ValueError("no constraint can be redirected outside its source targets")
        constraint, parameter_guid = constraint_without_target_record
        field = next((child for child in constraint
                      if child.get("xs.n") == "morphTargetParameterGuid"), None)
        if field is None:
            raise ValueError("morph constraint lacks morphTargetParameterGuid")
        field.attrib.pop("xs.ref", None)
        field.set("uuid", parameter_guid)
    else:
        raise ValueError(f"unknown morph mutation {kind!r}")
    return ET.tostring(graph.root, encoding="utf-8")


def mutate_extended_xml(xml, kind):
    graph = XMLGraph(xml)
    models = [node for node in graph.root.iter("CModelSource")
              if node.get("xs.ref") is None and any(isinstance(child.tag, str) for child in node)]
    if len(models) != 1:
        raise ValueError("extended negative control requires one primary model")
    model = models[0]
    sources = []
    for name in ("drawableSourceSet", "deformerSourceSet", "partSourceSet"):
        sources.extend(graph.source_items(model, name))
    extensions = []
    for source in sources:
        rows = graph.field(source, "_extensions")
        if rows is not None and rows.tag != "null":
            extensions.extend((source, extension) for extension in graph.items(rows)
                              if extension.tag == "CExtendedInterpolationExtension")

    def raw_field(node, name):
        node = graph.resolve(node)
        seen = set()
        for _ in range(65):
            if node in seen:
                raise ValueError("authoring super cycle")
            seen.add(node)
            field = next((child for child in node if child.get("xs.n") == name), None)
            if field is not None:
                return field
            parent = next((child for child in node if child.get("xs.n") == "super"), None)
            if parent is None:
                return None
            node = graph.resolve(parent)
        raise ValueError("authoring super chain exceeds 64 nodes")

    def redirect_owner(field, source):
        other = next((candidate for candidate in sources
                      if graph.resolve(candidate) is not graph.resolve(source)
                      and candidate.get("xs.id")), None)
        if field is None or other is None:
            return False
        field.set("xs.ref", other.get("xs.id"))
        return True

    def set_missing_guid(field, value):
        if field is None:
            return False
        field.attrib.pop("xs.ref", None)
        field.set("uuid", value)
        return True

    if kind == "extension-owner":
        for source, extension in extensions:
            parent = raw_field(extension, "super")
            if parent is not None and redirect_owner(raw_field(parent, "_owner"), source):
                break
        else:
            raise ValueError("could not prepare extended interpolation owner control")
    elif kind == "generated-form-owner":
        for source, extension in extensions:
            for name in ("normalExtendedKeyforms", "morphTargetExtendedKeyforms"):
                rows = graph.field(extension, name)
                forms = graph.items(rows) if rows is not None else []
                if forms and redirect_owner(raw_field(forms[0], "_source"), source):
                    break
            else:
                continue
            break
        else:
            raise ValueError("could not prepare generated form owner control")
    elif kind == "extended-grid-form":
        for _, extension in extensions:
            keyform_grid = graph.field(extension, "extendedKeyformGridSource")
            if keyform_grid is None or keyform_grid.tag == "null":
                continue
            cells = graph.named_items(keyform_grid, "keyformsOnGrid")
            if cells and set_missing_guid(raw_field(cells[0], "keyformGuid"), "missing-extended-form-guid"):
                break
        else:
            raise ValueError("could not prepare extended grid form control")
    elif kind == "extended-unknown-parameter":
        for _, extension in extensions:
            target_set = graph.field(extension, "_extendedKeyformMorphTargetSet")
            if target_set is None or target_set.tag == "null":
                continue
            targets = graph.named_items(target_set, "_morphTargets")
            if targets and set_missing_guid(
                raw_field(targets[0], "parameterGuid"), "missing-extended-parameter-guid"):
                break
        else:
            raise ValueError("could not prepare extended morph parameter control")
    elif kind == "extended-nonfinite-setting":
        for _, extension in extensions:
            target_set = graph.field(extension, "_extendedKeyformMorphTargetSet")
            if target_set is None or target_set.tag == "null":
                continue
            settings = graph.field(target_set, "extendedInterpolationSettings")
            rows = graph.items(settings) if settings is not None else []
            if not rows:
                continue
            value = graph.field(rows[0], "value")
            scale = raw_field(value, "extendedInterpolationScale") if value is not None else None
            if scale is not None:
                scale.text = "NaN"
                break
        else:
            raise ValueError("could not prepare non-finite extended interpolation setting control")
    else:
        raise ValueError(f"unknown extended interpolation mutation {kind!r}")
    return ET.tostring(graph.root, encoding="utf-8")


def mutate_hierarchy_xml(xml, kind):
    graph = XMLGraph(xml)
    model = next(node for node in graph.root.iter("CModelSource")
                 if node.get("xs.ref") is None and len(node))
    meshes = graph.source_items(model, "drawableSourceSet")
    deformers = graph.source_items(model, "deformerSourceSet")
    parts = graph.source_items(model, "partSourceSet")
    root = graph.guid(graph.required(model, "rootPart"), "guid", required=True)
    nonroot = [node for node in parts if graph.guid(node, "guid", required=True) != root]

    def replace_field(node, name, guid):
        node = graph.resolve(node)
        for _ in range(65):
            field = next((child for child in node if child.get("xs.n") == name), None)
            if field is not None:
                index = list(node).index(field)
                node.remove(field)
                node.insert(index, ET.Element("CGuid", {"xs.n": name, "uuid": guid}))
                return
            parent = next((child for child in node if child.get("xs.n") == "super"), None)
            if parent is None:
                node.append(ET.Element("CGuid", {"xs.n": name, "uuid": guid}))
                return
            node = graph.resolve(parent)
        raise AssertionError("hierarchy mutation exceeded source inheritance")

    field = "parentGuid" if kind.startswith("parent-") or kind == "root-parent" else "targetDeformerGuid"
    owner = nonroot[0] if field == "parentGuid" else deformers[0]
    if kind.endswith("dangling"):
        target = "00000000-0000-4000-8000-000000000001"
    elif kind.endswith("wrong-kind"):
        target = graph.guid(meshes[0], "guid", required=True)
    elif kind == "target-self":
        target = graph.guid(owner, "guid", required=True)
    elif kind.endswith("cycle"):
        second = nonroot[1] if field == "parentGuid" else deformers[1]
        target = graph.guid(second, "guid", required=True)
        replace_field(second, field, graph.guid(owner, "guid", required=True))
    elif kind == "root-parent":
        owner = next(node for node in parts if graph.guid(node, "guid", required=True) == root)
        target = graph.guid(nonroot[0], "guid", required=True)
    else:
        raise AssertionError(f"unknown hierarchy mutation {kind}")
    replace_field(owner, field, target)
    return ET.tostring(graph.root, encoding="utf-8")


def check_hierarchy_negative_control(core, binary, source_path, entries, source_xml,
                                     directory, active_snapshot, kind, expected_error):
    xml = mutate_hierarchy_xml(source_xml, kind)
    path = directory / f"hierarchy-{kind}.cmo3"
    path.write_bytes(repack_main_xml(source_path.read_bytes(), entries, xml))
    decoded_entries, roundtrip = CAFF.decode(path.read_bytes())
    if xml != roundtrip:
        raise AssertionError("hierarchy control XML changed during CAFF repack")
    try:
        read_scene(roundtrip, {entry[0][0] for entry in decoded_entries})
    except ValueError as error:
        if expected_error not in str(error):
            raise AssertionError(f"{kind} failed for unexpected oracle reason: {error}") from error
    else:
        raise AssertionError(f"oracle accepted hierarchy control {kind}")
    responses = {}
    for operation in ("live2d.authoring.read", "live2d.authoring.linear_weights",
                      "live2d.authoring.primary_forms"):
        response = ffi_request(core, operation, path)
        if response.get("succeeded") or expected_error not in str(response.get("message", "")):
            raise AssertionError(f"FFI accepted hierarchy control {kind}: {response}")
        if core.request("snapshot") != active_snapshot:
            raise AssertionError(f"hierarchy control {kind} changed active INX")
        responses[f"ffi:{operation}"] = response
        response, _ = jsonl_request(binary, operation, path)
        if response.get("succeeded") or expected_error not in str(response.get("message", "")):
            raise AssertionError(f"JSONL accepted hierarchy control {kind}: {response}")
        responses[f"jsonl:{operation}"] = response
    response, wire = mcp_request(binary, path, success=False)
    if expected_error not in json.dumps(response):
        raise AssertionError(f"MCP rejected {kind} for unexpected reason: {response}")
    responses["mcp:read"] = response
    log = directory / f"hierarchy-{kind}.responses.json"
    write_json(log, responses)
    (directory / f"hierarchy-{kind}.mcp.jsonl").write_text(wire)
    return {"kind": f"hierarchy-{kind}", "source": str(path), "sha256": sha(path),
            "error": expected_error, "transportChecks": len(responses), "response": str(log)}


def check_morph_negative_control(core, source_path, entries, source_xml, directory,
                                 active_snapshot, kind, oracle_error, ffi_error,
                                 mutator=mutate_morph_xml, prefix="morph"):
    malformed_xml = mutator(source_xml, kind)
    path = directory / f"{prefix}-{kind}.cmo3"
    path.write_bytes(repack_main_xml(source_path.read_bytes(), entries, malformed_xml))
    decoded_entries, roundtrip_xml = CAFF.decode(path.read_bytes())
    if roundtrip_xml != malformed_xml:
        raise AssertionError(f"{kind} CAFF repack did not preserve its source XML")
    try:
        read_scene(roundtrip_xml, {entry[0][0] for entry in decoded_entries})
    except ValueError as error:
        if oracle_error not in str(error):
            raise AssertionError(f"{kind} XML oracle failed for an unexpected reason: {error}") from error
    else:
        raise AssertionError(f"{kind} malformed source passed the independent XML oracle")
    before_bad_read = core.request("snapshot")
    if before_bad_read != active_snapshot:
        raise AssertionError(f"active INX snapshot changed before {kind} control")
    response = ffi_request(core, "live2d.authoring.read", path)
    error = str(response.get("message", ""))
    if response.get("succeeded") or ffi_error not in error:
        raise AssertionError(f"{kind} malformed source unexpectedly passed authoring read: {response}")
    if core.request("snapshot") != before_bad_read:
        raise AssertionError(f"failed {kind} read changed active INX snapshot")
    log = directory / f"morph-{kind}.ffi.json"
    write_json(log, response)
    return {"kind": kind, "source": str(path), "sha256": sha(path),
            "error": error, "containerEntries": len(decoded_entries), "response": str(log)}


def ffi_request(core, operation, path):
    payload = json.dumps({"op": operation, "path": str(path)}).encode()
    raw = core.lib.vsoul_session_request(core.handle, payload)
    if not raw:
        raise RuntimeError("Rust FFI returned a null response")
    try:
        return json.loads(ctypes.string_at(raw))
    finally:
        core.lib.vsoul_string_free(raw)


def jsonl_request(binary, operation, path):
    request = {"op": operation, "path": str(path)}
    process = subprocess.run([str(binary)], input=json.dumps(request) + "\n", text=True,
                             capture_output=True, timeout=120)
    lines = [line for line in process.stdout.splitlines() if line.strip()]
    if process.returncode or len(lines) != 1:
        raise RuntimeError(f"JSONL process failed: {process.stderr}")
    return json.loads(lines[0]), process.stdout + process.stderr


def mcp_request(binary, path, success=True):
    messages = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2024-11-05", "capabilities": {},
            "clientInfo": {"name": "cmo-source-verifier", "version": "1"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
        {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "live2d_authoring_read", "arguments": {"path": str(path)}}},
    ]
    process = subprocess.run([str(binary), "--mcp"],
                             input="".join(json.dumps(row) + "\n" for row in messages),
                             text=True, capture_output=True, timeout=120)
    if process.returncode:
        raise RuntimeError(f"MCP process failed: {process.stderr}")
    responses = {row.get("id"): row for row in
                 (json.loads(line) for line in process.stdout.splitlines() if line.strip())
                 if "id" in row}
    if 1 not in responses or 2 not in responses or 3 not in responses:
        raise RuntimeError("MCP responses lack initialize, tools/list, or source read")
    tools = {tool["name"] for tool in responses[2]["result"]["tools"]}
    if "live2d_authoring_read" not in tools:
        raise RuntimeError("stdio MCP does not advertise live2d_authoring_read")
    result = responses[3]["result"]
    if not success:
        if not result.get("isError"):
            raise AssertionError(f"stdio MCP accepted malformed source: {result}")
        return result, process.stdout + process.stderr
    if result.get("isError"):
        raise RuntimeError(f"stdio MCP source read failed: {result}")
    value = result.get("structuredContent")
    if value is None:
        value = json.loads(next(item["text"] for item in result["content"]
                                if item.get("type") == "text"))
    if isinstance(value, dict) and set(value) == {"result"}:
        value = value["result"]
    return value, process.stdout + process.stderr


def zip_main_xml(payload, xml):
    if payload[:4] != b"PK\x03\x04":
        raise ValueError("CAFF main.xml payload lacks ZIP local header")
    name_size, extra_size = struct.unpack_from("<HH", payload, 26)
    start = 30 + name_size + extra_size
    header = bytearray(payload[:30])
    name_extra = payload[30:start]
    flags, method = struct.unpack_from("<HH", header, 6)
    if method != 8:
        raise ValueError("CAFF main.xml uses an unsupported ZIP method")
    compressor = zlib.compressobj(level=6, wbits=-15)
    compressed = compressor.compress(xml) + compressor.flush()
    checksum = zlib.crc32(xml) & 0xFFFFFFFF
    struct.pack_into("<III", header, 14, 0, 0, 0)
    descriptor = b"PK\x07\x08" + struct.pack("<III", checksum, len(compressed), len(xml))
    if not flags & 8:
        struct.pack_into("<H", header, 6, flags | 8)
    return bytes(header) + name_extra + compressed + descriptor


def encode_caff(original, entries):
    header = bytearray(original[:58])
    seed = int.from_bytes(header[14:18], "big")
    key = seed & 255
    rows, payloads = [], []
    for names, unknown, flags, reserved, payload in entries:
        encoded_names = bytearray()
        for name in names:
            raw = name.encode("utf-8")
            if len(raw) > 255:
                raise ValueError("CAFF entry name exceeds one-byte length")
            encoded_names.append(len(raw) ^ key)
            encoded_names.extend(byte ^ key for byte in raw)
        field_offset = len(encoded_names)
        row = encoded_names + bytes(12)
        row.extend(byte ^ key for byte in flags)
        row.extend(reserved)
        rows.append((row, field_offset, unknown, payload))
        payloads.append(payload)
    offset = 58 + sum(len(row) for row, _, _, _ in rows)
    header[54:58] = (len(rows) ^ seed).to_bytes(4, "big")
    body = bytearray(header)
    for row, field_offset, unknown, payload in rows:
        fields = (unknown ^ seed, offset ^ seed, len(payload) ^ seed)
        struct.pack_into(">III", row, field_offset, *fields)
        body.extend(row)
        offset += len(payload)
    for payload in payloads:
        body.extend(byte ^ key for byte in payload)
    body.extend(original[-2:])
    return bytes(body)


def source_hashes(library, binary):
    paths = [
        ROOT / "tools/verify-cmo-source.py", ROOT / "tools/verify-caff-authoring.py",
        ROOT / "tools/verify-rust-core.py", ROOT / "rust/src/authoring_scene.rs",
        ROOT / "rust/src/authoring_graph.rs", ROOT / "rust/src/authoring_keyforms.rs",
        ROOT / "rust/src/authoring_hierarchy.rs",
        ROOT / "rust/src/authoring_edit.rs", ROOT / "rust/src/document.rs",
        ROOT / "rust/src/authoring_values.rs", ROOT / "rust/src/caff.rs",
        ROOT / "rust/src/authoring.rs",
        ROOT / "rust/src/lib.rs", ROOT / "rust/src/main.rs", library, binary,
    ]
    return {str(path.relative_to(ROOT)) if path.is_relative_to(ROOT) else str(path):
            sha(path) if path.is_file() else None for path in paths}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("inputs", nargs="?", type=Path, default=DEFAULT_INPUTS)
    args = parser.parse_args()
    evidence_root = ROOT / "build/verification"
    evidence_root.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="cmo-source-", dir=evidence_root))
    library = ROOT / "out/rust/debug" / (
        "libsoul2d_core.dylib" if sys.platform == "darwin" else "libsoul2d_core.so")
    binary = ROOT / "out/rust/debug/soul2d-core"
    result = {"status": "FAIL", "evidence": str(directory), "files": [],
              "transportLogs": [], "negativeControls": []}
    core = None
    try:
        paths = sorted(path.resolve() for path in args.inputs.rglob("*.cmo3"))
        can_paths = sorted(path.resolve() for path in args.inputs.rglob("*.can3"))
        if len(paths) != 4:
            raise AssertionError(f"expected 4 CMO3 fixtures, found {len(paths)}")
        if not can_paths:
            raise AssertionError("expected CAN3 no-model controls")
        original_hashes = {str(path): sha(path) for path in paths + can_paths}
        core = CORE.Core(library)
        active_path = directory / "active-document.inx"
        CORE.fixture(active_path)
        core.request("open", path=str(active_path))
        core.request("node.rename", id=2, name="Preserve active authored model")
        active_snapshot = core.request("snapshot")
        active_model = active_snapshot.get("model", {})
        if not active_model or not active_model.get("nodes", {}).get("children"):
            raise AssertionError("active INX fixture is empty")
        if active_model["nodes"]["children"][0]["name"] != "Preserve active authored model":
            raise AssertionError("active INX fixture edit was not retained")
        write_json(directory / "active-before.json", active_snapshot)

        morph_mutation_fixture = None
        extended_mutation_fixtures = {}
        for index, path in enumerate(paths):
            key = path.relative_to(args.inputs.resolve()).as_posix().replace("/", "__")
            entries, xml_bytes = CAFF.decode(path.read_bytes())
            expected = read_scene(xml_bytes, {entry[0][0] for entry in entries})
            for source in expected["sources"]:
                for extension in source["extendedInterpolations"]:
                    fixture = (path, entries, xml_bytes)
                    extended_mutation_fixtures.setdefault("extension-owner", fixture)
                    if extension["normalForms"] or extension["morphForms"]:
                        extended_mutation_fixtures.setdefault("generated-form-owner", fixture)
                    if extension["grid"] is not None and extension["grid"]["cells"]:
                        extended_mutation_fixtures.setdefault("extended-grid-form", fixture)
                    morph_targets_data = extension["morphTargets"]
                    if morph_targets_data is not None:
                        if morph_targets_data["targets"]:
                            extended_mutation_fixtures.setdefault("extended-unknown-parameter", fixture)
                        if morph_targets_data["settings"]:
                            extended_mutation_fixtures.setdefault("extended-nonfinite-setting", fixture)
            if morph_mutation_fixture is None and any(
                source["morphTargets"] is not None
                and source["morphTargets"]["targets"]
                and source["morphTargets"]["blendWeightConstraints"]
                for source in expected["sources"]
            ):
                morph_mutation_fixture = (path, entries, xml_bytes)
            expected_path = directory / f"{key}.oracle.json"
            write_json(expected_path, expected)

            if core.request("snapshot") != active_snapshot:
                raise AssertionError("active INX snapshot changed before authoring read")
            ffi_response = ffi_request(core, "live2d.authoring.read", path)
            if not ffi_response.get("succeeded"):
                raise AssertionError(f"FFI source read failed for {path}: {ffi_response}")
            ffi_value = ffi_response.get("result")
            if ffi_value != expected:
                raise AssertionError(f"independent XML oracle differs from FFI for {path}")
            if core.request("snapshot") != active_snapshot:
                raise AssertionError("successful authoring read changed active INX snapshot")
            ffi_file = directory / f"{key}.ffi.json"
            write_json(ffi_file, ffi_response)

            jsonl_response, jsonl_wire = jsonl_request(binary, "live2d.authoring.read", path)
            if not jsonl_response.get("succeeded") or jsonl_response.get("result") != ffi_value:
                raise AssertionError(f"JSONL source read differs from FFI for {path}")
            jsonl_log = directory / f"{key}.jsonl.log"
            jsonl_log.write_text(jsonl_wire)

            mcp_value, mcp_wire = mcp_request(binary, path)
            if mcp_value != ffi_value:
                raise AssertionError(f"stdio MCP source read differs from FFI for {path}")
            mcp_log = directory / f"{key}.mcp.log"
            mcp_log.write_text(mcp_wire)
            result["transportLogs"].extend([str(jsonl_log), str(mcp_log)])
            result["files"].append({
                "source": str(path), "sha256": original_hashes[str(path)],
                "entryCount": len(entries), "xmlBytes": len(xml_bytes),
                "oracle": str(expected_path), "ffi": str(ffi_file),
                "jsonlLog": str(jsonl_log), "mcpLog": str(mcp_log),
                "summary": scene_summary(expected),
            })
            if sha(path) != original_hashes[str(path)]:
                raise AssertionError(f"original CMO3 was modified: {path}")

        for index, path in enumerate(can_paths):
            if core.request("snapshot") != active_snapshot:
                raise AssertionError("active INX snapshot changed before CAN3 negative control")
            response = ffi_request(core, "live2d.authoring.read", path)
            error = str(response.get("message", ""))
            if response.get("succeeded") or "no model" not in error.lower():
                raise AssertionError(f"CAN3 did not fail for lack of a model: {path}: {response}")
            if core.request("snapshot") != active_snapshot:
                raise AssertionError("failed CAN3 read changed active INX snapshot")
            log = directory / f"can-no-model-{index}.ffi.json"
            write_json(log, response)
            result["negativeControls"].append({"source": str(path),
                                                "sha256": original_hashes[str(path)],
                                                "error": error, "response": str(log)})

        source_entries, source_xml = CAFF.decode(paths[0].read_bytes())
        malformed_xml, count = re.subn(
            rb'(<f\b[^>]*xs\.n="minValue"[^>]*>)[^<]*(</f>)', rb"\1NaN\2", source_xml, count=1)
        if count != 1:
            raise AssertionError("could not prepare malformed finite-number source")
        malformed_path = directory / "malformed-nonfinite.cmo3"
        malformed_path.write_bytes(repack_main_xml(paths[0].read_bytes(), source_entries, malformed_xml))
        decoded_malformed, malformed_roundtrip = CAFF.decode(malformed_path.read_bytes())
        if malformed_roundtrip != malformed_xml:
            raise AssertionError("malformed CAFF repack did not preserve its source XML")
        before_bad_read = core.request("snapshot")
        response = ffi_request(core, "live2d.authoring.read", malformed_path)
        error = str(response.get("message", ""))
        if response.get("succeeded") or "invalid finite authoring number" not in error:
            raise AssertionError("non-finite source unexpectedly passed authoring read")
        if core.request("snapshot") != before_bad_read or before_bad_read != active_snapshot:
            raise AssertionError("failed malformed source read changed active INX snapshot")
        malformed_log = directory / "malformed-nonfinite.ffi.json"
        write_json(malformed_log, response)
        result["negativeControls"].append({
            "source": str(malformed_path), "sha256": sha(malformed_path),
            "error": error, "containerEntries": len(decoded_malformed),
            "response": str(malformed_log),
        })

        for kind, expected_error in (
            ("parent-dangling", "authoring parent part does not resolve"),
            ("parent-wrong-kind", "authoring parent guid does not identify a part"),
            ("parent-cycle", "authoring parent hierarchy contains a cycle"),
            ("root-parent", "authoring root part has a parent"),
            ("target-dangling", "authoring target deformer does not resolve"),
            ("target-wrong-kind", "authoring target guid does not identify a deformer"),
            ("target-self", "authoring deformer targets itself"),
            ("target-cycle", "authoring deformer targets contain a cycle"),
        ):
            result["negativeControls"].append(check_hierarchy_negative_control(
                core, binary, paths[0], source_entries, source_xml, directory,
                active_snapshot, kind, expected_error))

        if morph_mutation_fixture is None:
            raise AssertionError("no valid CMO3 fixture has both morph targets and blend constraints")
        morph_path, morph_entries, morph_xml = morph_mutation_fixture
        for kind, oracle_error, ffi_error in (
            ("wrong-owner", "authoring morph target belongs to another source",
             "authoring morph target belongs to another source"),
            ("dangling-form", "authoring morph target references a form outside its source",
             "authoring morph target references a form outside its source"),
            ("unknown-parameter", "unknown authoring morph parameter GUID",
             "authoring morph references an unknown parameter"),
            ("nonfinite-weight", "non-finite authoring number blendWeight",
             "invalid finite authoring number"),
            ("constraint-without-target",
             "authoring blend constraint references a parameter without a morph target in its source",
             "authoring blend constraint references a parameter without a morph target in its source"),
        ):
            result["negativeControls"].append(check_morph_negative_control(
                core, morph_path, morph_entries, morph_xml, directory, active_snapshot,
                kind, oracle_error, ffi_error))

        for kind, oracle_error, ffi_error in (
            ("extension-owner", "authoring extended interpolation belongs to another source",
             "another source"),
            ("generated-form-owner", "authoring form belongs to another source", "another source"),
            ("extended-grid-form", "authoring extended grid references a form outside its source",
             "outside its source"),
            ("extended-unknown-parameter", "unknown authoring extended morph parameter GUID", "unknown"),
            ("extended-nonfinite-setting", "non-finite authoring number extendedInterpolationScale",
             "finite"),
        ):
            if kind not in extended_mutation_fixtures:
                raise AssertionError(f"no valid CMO3 fixture can exercise {kind}")
            path, entries, xml = extended_mutation_fixtures[kind]
            result["negativeControls"].append(check_morph_negative_control(
                core, path, entries, xml, directory, active_snapshot,
                kind, oracle_error, ffi_error,
                mutator=mutate_extended_xml, prefix="extended"))

        if core.request("snapshot") != active_snapshot:
            raise AssertionError("authoring verification changed active INX snapshot")
        if any(sha(Path(path)) != digest for path, digest in original_hashes.items()):
            raise AssertionError("an original source fixture was modified")
        source_type_totals = Counter()
        extended_type_totals = Counter()
        extended_setting_type_totals = Counter()
        axis_totals = Counter()
        cell_totals = Counter()
        for item in result["files"]:
            summary = item["summary"]
            source_type_totals.update(summary["sourceTypes"])
            extended_type_totals.update(summary["extendedInterpolationTypes"])
            extended_setting_type_totals.update(summary["extendedInterpolationSettingTypes"])
            axis_totals.update(summary["gridAxisCounts"])
            cell_totals.update(summary["gridCellCounts"])
        five_axis_243 = [
            {"file": item["source"], **grid_item}
            for item in result["files"]
            for grid_item in item["summary"]["fiveAxis243CellGrids"]]
        if len(five_axis_243) != 3:
            raise AssertionError("the three authored five-axis 243-cell grids were not retained")
        duplicate_cases = [
            {"file": item["source"], **case}
            for item in result["files"]
            for case in item["summary"]["duplicateGuidCoordinateCases"]]
        coordinate_variants = [
            {"file": item["source"], **case}
            for item in result["files"]
            for case in item["summary"]["sameGuidDifferentCoordinateTypes"]]
        result["corpusSummary"] = {
            "fileCount": len(paths),
            "sourceCount": sum(item["summary"]["sourceCount"] for item in result["files"]),
            "formCount": sum(item["summary"]["formCount"] for item in result["files"]),
            "morphTargetCount": sum(item["summary"]["morphTargetCount"]
                                     for item in result["files"]),
            "blendWeightConstraintCount": sum(item["summary"]["blendWeightConstraintCount"]
                                               for item in result["files"]),
            "gridCount": sum(item["summary"]["gridCount"] for item in result["files"]),
            "extendedInterpolationCount": sum(
                item["summary"]["extendedInterpolationCount"] for item in result["files"]),
            "extendedGridCount": sum(item["summary"]["extendedGridCount"] for item in result["files"]),
            "normalExtendedFormCount": sum(
                item["summary"]["normalExtendedFormCount"] for item in result["files"]),
            "morphTargetExtendedFormCount": sum(
                item["summary"]["morphTargetExtendedFormCount"] for item in result["files"]),
            "extendedMorphTargetCount": sum(
                item["summary"]["extendedMorphTargetCount"] for item in result["files"]),
            "unavailableExtendedMorphTargetCount": sum(
                item["summary"]["unavailableExtendedMorphTargetCount"] for item in result["files"]),
            "extendedInterpolationSettingCount": sum(
                item["summary"]["extendedInterpolationSettingCount"] for item in result["files"]),
            "generatedFormVertexCountMismatchCount": sum(
                item["summary"]["generatedFormVertexCountMismatchCount"] for item in result["files"]),
            "sourceTypes": dict(sorted(source_type_totals.items())),
            "extendedInterpolationTypes": dict(sorted(extended_type_totals.items())),
            "extendedInterpolationSettingTypes": dict(sorted(extended_setting_type_totals.items())),
            "gridAxisCounts": dict(sorted(axis_totals.items(), key=lambda pair: int(pair[0]))),
            "gridCellCounts": dict(sorted(cell_totals.items(), key=lambda pair: int(pair[0]))),
            "fiveAxis243CellGridCount": len(five_axis_243),
            "fiveAxis243CellGrids": five_axis_243,
            "duplicateGuidCoordinateCaseCount": len(duplicate_cases),
            "duplicateGuidCoordinateCases": duplicate_cases,
            "sameGuidDifferentCoordinateTypeCount": len(coordinate_variants),
            "sameGuidDifferentCoordinateTypes": coordinate_variants,
        }
        result["activeSnapshotSha256"] = hashlib.sha256(
            json.dumps(active_snapshot, sort_keys=True).encode()).hexdigest()
        result["sourceAndBinarySha256"] = source_hashes(library, binary)
        result["status"] = "PASS"
    except Exception as error:
        result["error"] = f"{type(error).__name__}: {error}"
        result["sourceAndBinarySha256"] = source_hashes(library, binary)
    finally:
        if core is not None:
            core.close()
        write_json(directory / "result.json", result)
    print(json.dumps({"status": result["status"], "evidence": str(directory),
                      "files": [{"source": Path(item["source"]).name,
                                 "sourceCount": item["summary"]["sourceCount"],
                                 "sourceTypes": item["summary"]["sourceTypes"],
                                 "formCount": item["summary"]["formCount"],
                                 "morphTargetCount": item["summary"]["morphTargetCount"],
                                 "blendWeightConstraintCount": item["summary"]["blendWeightConstraintCount"],
                                 "gridCount": item["summary"]["gridCount"],
                                 "extendedInterpolationCount": item["summary"]["extendedInterpolationCount"],
                                 "extendedGridCount": item["summary"]["extendedGridCount"],
                                 "normalExtendedFormCount": item["summary"]["normalExtendedFormCount"],
                                 "morphTargetExtendedFormCount": item["summary"]["morphTargetExtendedFormCount"],
                                 "extendedMorphTargetCount": item["summary"]["extendedMorphTargetCount"],
                                 "extendedInterpolationSettingCount": item["summary"]["extendedInterpolationSettingCount"],
                                 "generatedFormVertexCountMismatchCount": item["summary"]["generatedFormVertexCountMismatchCount"],
                                 "gridAxisCounts": item["summary"]["gridAxisCounts"],
                                 "gridCellCounts": item["summary"]["gridCellCounts"],
                                 "extendedInterpolationTypes": item["summary"]["extendedInterpolationTypes"],
                                 "extendedInterpolationSettingTypes": item["summary"]["extendedInterpolationSettingTypes"],
                                 "fiveAxis243CellGridCount": len(item["summary"]["fiveAxis243CellGrids"]),
                                 "duplicateGuidCoordinateCaseCount": len(item["summary"]["duplicateGuidCoordinateCases"])}
                                for item in result["files"]],
                      "corpusSummary": result.get("corpusSummary"),
                      "negativeControls": len(result["negativeControls"]),
                      "error": result.get("error")}, indent=2, ensure_ascii=False))
    return 0 if result["status"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
