#!/usr/bin/env python3
"""Compare native preview vertex positions with a capture written by capture-cubism-core.py.

Prints counts only, with the worst error of the default and single-parameter poses apart from that of the poses with
every parameter set at once. Exits 0 only when every pose of every model was compared and every drawable is within the
tolerance. A pose the preview refuses is counted and makes its model INCOMPLETE, or NOT_COMPARED when no pose is left.
The preview's painter order is checked against Core's render orders too: every drawable Core paints at a place
other than the preview's counts as misplaced, and any misplaced drawable fails the model.

By default the CMO3 of each case is opened; with `--runtime` the case's `.model3.json` is attached to the CAN3 next to
the CMO3 instead, so the clean-room `.moc3` reader is compared with Core. A runtime model's drawn meshes take the
contiguous run of node ids ending at the highest one, in file order like the capture's drawables.
"""
import argparse
import collections
import json
import math
import struct
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def authored(value: float) -> float:
    """The shortest decimal with the same single-precision value: Core reports 1.2 as 1.20000005."""
    single = struct.unpack('f', struct.pack('f', value))[0]
    for digits in range(1, 10):
        short = float(f'{value:.{digits}g}')
        if struct.unpack('f', struct.pack('f', short))[0] == single:
            return short
    return value


HOST_CAN3_XML = """<root fileFormatVersion="503030001"><main><CAnimation xs.ref="animation"/></main><shared>
<CAnimation xs.id="animation"><s xs.n="name">Host</s>
  <ResourceManager xs.n="resourceManager"><carray_list xs.n="_resourceRefList" count="1">
    <ResourceEntry><CResource_Linked_Model xs.n="resourceRef" xs.ref="resource"/></ResourceEntry></carray_list></ResourceManager>
  <carray_list xs.n="_scenes" count="1"><CSceneSource xs.ref="scene"/></carray_list></CAnimation>
<CResource_Linked_Model xs.id="resource"><CGuid xs.n="guid" uuid="host-resource"/><s xs.n="name">Model</s><s xs.n="srcFile">host.cmo3</s></CResource_Linked_Model>
<CSceneSource xs.id="scene"><CGuid xs.n="guid" uuid="host-scene"/><CAnimation xs.n="_animation" xs.ref="animation"/><s xs.n="sceneName">Host</s>
  <CMovieInfo xs.n="movieInfo"><f xs.n="fps">60</f><i xs.n="duration">1</i></CMovieInfo>
  <CTrackSourceSet xs.n="trackSourceSet"><carray_list xs.n="_sources" count="1"><CMvTrack_Live2DModel_Source xs.ref="track"/></carray_list></CTrackSourceSet></CSceneSource>
<CMvTrack_Live2DModel_Source xs.id="track"><CGuid xs.n="guid" uuid="host-track"/><s xs.n="name">Track</s>
  <CSceneSource xs.n="_sceneSource" xs.ref="scene"/><CModelGuid xs.n="_resourceGuid" uuid="host-resource"/>
  <CMvKeyParamEffect xs.n="keyParamEffect"><CMvTrack_Live2DModel_Source xs.n="track" xs.ref="track"/><carray_list xs.n="attrList" count="0"/></CMvKeyParamEffect>
  <i xs.n="start">0</i><i xs.n="internalOffset">0</i><i xs.n="duration">1</i></CMvTrack_Live2DModel_Source>
</shared></root>"""


def write_host_can3(path: Path) -> None:
    """A CAN3 with one model resource and no curve, so a runtime model without a CMO3 of its own can be attached
    for comparison. Same container layout as the test fixtures of rust/tests/core.rs."""
    import struct
    import zlib
    xml = HOST_CAN3_XML.encode()
    deflated = zlib.compressobj(9, zlib.DEFLATED, -15)
    compressed = deflated.compress(xml) + deflated.flush()
    archive = struct.pack('<IHHHIIII', 0x04034b50, 20, 0x0808, 8, 0, 0, 0, 0) + struct.pack('<HH', 8, 0) + b'contents' + compressed
    archive += b'PK\x07\x08' + struct.pack('<III', zlib.crc32(xml) & 0xffffffff, len(compressed), len(xml))
    seed = 0x562e099f
    mask = seed & 0xff
    entries = [(b'main.xml', b'main_xml', bytes([0x01, 0x21]), archive)]
    out = bytearray(58)
    out[0:4] = b'CAFF'
    out[4:8] = struct.pack('>I', 45)
    out[14:18] = struct.pack('>I', seed)
    out[54:58] = struct.pack('>I', len(entries) ^ seed)
    fields = []
    for name, alias, flags, _ in entries:
        out.append(len(name) ^ mask); out.extend(b ^ mask for b in name)
        out.append(len(alias) ^ mask); out.extend(b ^ mask for b in alias)
        out += struct.pack('>I', 0x12345678 ^ seed)
        fields.append(len(out)); out += struct.pack('>I', seed)
        out += struct.pack('>I', seed)
        out.extend(b ^ mask for b in flags); out.extend(0 ^ mask for _ in range(8))
    for (name, alias, flags, payload), offset_field in zip(entries, fields):
        out[offset_field:offset_field + 4] = struct.pack('>I', len(out) ^ seed)
        out[offset_field + 4:offset_field + 8] = struct.pack('>I', len(payload) ^ seed)
        out.extend(b ^ mask for b in payload)
    out += b'bc'
    path.write_bytes(bytes(out))


def compare(binary: Path, case: dict, cmo3: Path, tolerance: float, runtime: bool = False, can3: Path = None) -> collections.Counter:
    core = subprocess.Popen([binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)

    def call(request: dict) -> dict:
        core.stdin.write(json.dumps(request) + '\n')
        core.stdin.flush()
        return json.loads(core.stdout.readline())

    counts = collections.Counter()
    try:
        if runtime:
            can3 = can3 or next(cmo3.parent.glob('*.can3'))
            assert call({'op': 'open', 'path': str(can3)})['succeeded'], 'the preview could not open the animation'
            attached = call({'op': 'live2d.model.attach', 'path': case['model3']})
            assert attached['succeeded'], attached.get('message', 'the preview could not attach the runtime model')
            counts['attach warnings'] = len(attached['result'].get('warnings') or [])
            ids = None
        else:
            opened = call({'op': 'open', 'path': str(cmo3)})
            assert opened['succeeded'], 'the preview could not open the source model'
            # A native part id is the index of its source plus 2.
            ids = [source['id'] for source in opened['result']['model']['sources']]
        with open(case['output']) as rows:
            metadata = json.loads(next(rows))
            (origin_x, origin_y), scale = metadata['canvasOrigin'][0], metadata['pixelsPerUnit']
            names = [drawable['id'] for drawable in metadata['drawables']]
            known = set(names)
            parameters = [parameter['id'] for parameter in metadata['parameters']]
            for index, row in enumerate(rows):
                pose = json.loads(row)
                # Only the first row is the default pose; a later row without a parameter sets every parameter.
                combined = index > 0 and pose['parameterId'] is None
                kind = 'combined' if combined else 'key'
                if combined:
                    values = {name: authored(sample) for name, sample in zip(parameters, pose['parameterValues'])}
                else:
                    values = {} if index == 0 else {pose['parameterId']: authored(pose['sample'])}
                geometry = call({'op': 'scene.geometry', 'values': values})
                if not geometry['succeeded']:
                    counts['poses refused'] += 1
                    continue
                counts['poses compared'] += 1
                painted = geometry['result']['parts']
                if ids is None:
                    drawn = sorted(painted, key=lambda part: part['id'])[-len(names):]
                    if len(drawn) < len(names) or drawn[-1]['id'] - drawn[0]['id'] != len(names) - 1:
                        counts['poses whose drawn meshes do not line up with the drawables'] += 1
                        continue
                    name_of = {part['id']: name for name, part in zip(names, drawn)}
                else:
                    name_of = {part['id']: ids[part['id'] - 2] for part in painted}
                parts = {name_of[part['id']]: part['vertices'] for part in painted}
                core_order = [name for name, _ in sorted(zip(names, pose['drawables']), key=lambda pair: pair[1]['renderOrder'])]
                painter_order = [name_of[part['id']] for part in painted if name_of[part['id']] in known]
                misplaced = sum(1 for ours, core in zip(painter_order, core_order) if ours != core) + abs(len(painter_order) - len(core_order))
                counts['drawables misplaced in the painter order'] += misplaced
                counts['poses with another painter order'] += misplaced > 0
                for name, drawable in zip(names, pose['drawables']):
                    vertices = parts.get(name)
                    if vertices is None or len(vertices) != len(drawable['positions']):
                        counts['drawables without a matching part'] += 1
                        continue
                    error = max((math.hypot(x - (origin_x + core_x * scale), y - (origin_y - core_y * scale))
                        for (x, y), (core_x, core_y) in zip(vertices, drawable['positions'])), default=0.0)
                    counts['drawables compared'] += 1
                    counts['drawables over tolerance'] += error > tolerance
                    worst = f'worst {kind} pose error in thousandths of a pixel'
                    counts[worst] = max(counts[worst], math.ceil(error * 1000))
    finally:
        core.stdin.close()
        core.wait()
    return counts


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('reference', type=Path, help='run directory written by capture-cubism-core.py')
    parser.add_argument('--inputs', type=Path, default=ROOT / 'build/verification/live2d-inputs-vjq5llby')
    parser.add_argument('--binary', type=Path, default=ROOT / 'out/rust/debug/soul2d-core')
    # The worst error measured against Core 6.0.1 is 0.012 px; doc/live2d-authoring.md says what fails at 0.01.
    parser.add_argument('--tolerance', type=float, default=0.02, help='pixels')
    parser.add_argument('--runtime', action='store_true', help='attach each .model3.json to its CAN3 instead of opening the CMO3')
    parser.add_argument('--host-can3', action='store_true', help='with --runtime, attach a model that has no CMO3 and CAN3 of its own to a synthesized CAN3 without curves')
    args = parser.parse_args()
    result = json.loads((args.reference / 'result.json').read_text())
    assert result['status'] == 'CAPTURED', 'the reference run holds no Core capture'
    sources = sorted(args.inputs.rglob('*.cmo3'))
    failed = False
    for index, case in enumerate(result['cases']):
        stem = Path(case['model3']).name.split('.')[0]
        cmo3 = next((path for path in sources if path.stem.startswith(stem)), None)
        host = None
        if cmo3 is None:
            if not (args.runtime and args.host_can3):
                print(f'model {index}: NOT_COMPARED', json.dumps({'reason': 'no CMO3 for this model; pass --runtime --host-can3'}))
                failed = True
                continue
            host = args.reference / 'host.can3'
            write_host_can3(host)
        counts = compare(args.binary, case, cmo3, args.tolerance, args.runtime, host)
        bad = (counts['drawables over tolerance'] or counts['drawables without a matching part']
            or counts['poses whose drawn meshes do not line up with the drawables'] or counts['drawables misplaced in the painter order'])
        status = ('NOT_COMPARED' if not counts['drawables compared'] else 'FAIL' if bad
            else 'INCOMPLETE' if counts['poses refused'] else 'PASS')
        failed |= status != 'PASS'
        print(f'model {index}: {status}', json.dumps(dict(sorted(counts.items()))))
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())
