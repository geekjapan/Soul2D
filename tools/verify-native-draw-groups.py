#!/usr/bin/env python3
"""Check native group order against an independent XML hierarchy oracle."""
import importlib.util
import json
from pathlib import Path
import tempfile
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / "tools" / filename)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def main():
    playback = module("playback", "verify-native-playback.py")
    xml_oracle = module("xml_oracle", "verify-cmo-source.py")
    evidence = ROOT / "build/verification/native-resume-20261011"
    directory = Path(tempfile.mkdtemp(prefix="draw-group-check-", dir=evidence))
    records = []
    for baseline in json.loads((evidence / "draw-group-baseline.json").read_text()):
        original = Path(baseline["model"])
        original_bytes = original.read_bytes()
        entries, xml = xml_oracle.CAFF.decode(original_bytes)
        path = original
        normalized = "ren" in original.stem
        if normalized:
            # Isolate group order from advanced GPU blend and offscreen memory limits.
            graph = xml_oracle.XMLGraph(xml)
            for node in graph.root.iter():
                if node.tag not in ("CPartSource", "CArtMeshSource") or node.get("xs.ref"):
                    continue
                for field, value in (("colorComposition", "NORMAL"), ("alphaComposition", "OVER")):
                    item = graph.field(node, field)
                    if item is not None:
                        item.set("v", value)
                item = graph.field(node, "useOffscreen")
                if item is not None:
                    item.text = "false"
            xml = ET.tostring(graph.root, encoding="utf-8")
            path = directory / "ren-order-only.cmo3"
            path.write_bytes(xml_oracle.repack_main_xml(original_bytes, entries, xml))
        source = xml_oracle.read_scene(xml, {entry[0][0] for entry in entries})
        by_guid = {s["guid"]: s for s in source["sources"]}
        ranks = {}

        def palette(guid):
            assert guid not in ranks
            ranks[guid] = len(ranks)
            for child in by_guid.get(guid, {}).get("data", {}).get("childGuids", []):
                palette(child)

        palette(source["rootPartGuid"])
        for use_mcp in ([False, True] if normalized else [False]):
            rpc = playback.Rpc(ROOT / "out/rust/release/soul2d-core", 120, use_mcp)
            try:
                initial = rpc.call("open", path=str(path))
                authored = rpc.call("live2d.authoring.read")
                assert [s["guid"] for s in authored["sources"]] == [s["guid"] for s in source["sources"]]
                evaluated = rpc.call("live2d.authoring.morph_forms")
                forms = {s["guid"]: s["evaluatedForms"] for s in evaluated["sources"]}

                def order(guid):
                    selected = forms[guid]
                    if len(selected) != 1:
                        selected = [f for f in selected if f["coordinateType"] == "DeformerLocal"]
                    if not selected:
                        return by_guid[guid]["data"].get("defaultDrawOrder", 500)
                    assert len(selected) == 1, guid
                    return selected[0].get("drawOrder", by_guid[guid]["data"].get("defaultDrawOrder", 500))

                def key(guid):
                    chain = [guid]
                    parent = by_guid[guid]["parentPartGuid"]
                    while parent:
                        ancestor = by_guid[parent]
                        if ancestor["data"].get("useOffscreen"):
                            break
                        if ancestor["data"].get("drawOrderGroup"):
                            chain.append(parent)
                        parent = ancestor["parentPartGuid"]
                    return [(order(g), -ranks[g]) for g in reversed(chain)]

                packet = rpc.call("render.packet")
                draws = {d["id"]: d for d in packet["draws"]}
                guids = {i + 2: s["guid"] for i, s in enumerate(source["sources"])}
                for scope in packet["scopes"]:
                    expected = sorted(scope, key=lambda identity: key(guids[identity]))
                    assert scope == expected, (original.name, scope, expected)
                for identity, draw in draws.items():
                    assert identity in guids
                    assert all(mask["source"] in guids for mask in draw.get("masks", []))
                assert rpc.call("snapshot") == initial
                saved = directory / (path.stem + str(use_mcp) + ".saved.cmo3")
                rpc.call("save", path=str(saved))
                assert saved.read_bytes() == path.read_bytes()
                records.append({"model": str(original), "normalizedBlendFixture": normalized,
                                "transport": "mcp" if use_mcp else "jsonl",
                                "offscreenDisabledFixture": normalized,
                                "drawsChecked": len(draws), "scopesChecked": len(packet["scopes"])})
            finally:
                rpc.close()
        assert original.read_bytes() == original_bytes
    (directory / "result.json").write_text(json.dumps({"status": "PASS", "records": records}, indent=2) + "\n")
    print("PASS independent XML group ordering, stable IDs, source preservation:", directory)


if __name__ == "__main__":
    main()
