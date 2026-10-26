#!/usr/bin/env python3
"""Compare `live2d.physics3.export` with a derivation written apart from it, and with the vendor `.physics3.json` files.

For every CMO3 below the given directories the export is compared with the physics document this script derives from
the XML. Where exactly one `.physics3.json` sits below the folder of a CMO3, the export is compared with that file after
parsing. The files are read where they are, and only counts are printed.

Exits 0 when every export equals the derivation, the export of a CMO3 without a physics setting is refused for that
reason, and every vendor file equals the export apart from the setting names, which differ in some sample pairs for a
reason that is not known. The derivation rounds a tie at the fourth decimal to even and the export rounds it away from
zero, so a file holding one is reported as a difference; no sample holds one, and what the vendor does is not known.
"""
import argparse
import collections
import copy
import importlib.util
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location('oracle', ROOT / 'tools/verify-cmo-source.py')
ORACLE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ORACLE)

TYPES = {'SRC_TO_X': 'X', 'SRC_TO_G_ANGLE': 'Angle'}


def derive(xml: bytes) -> dict | None:
    """The physics document, or None when the CMO3 holds no physics setting."""
    graph = ORACLE.XMLGraph(xml)
    model = next(node for node in graph.root.iter('CModelSource')
        if node.get('xs.ref') is None and any(isinstance(child.tag, str) for child in node))
    physics = graph.required(model, 'physicsSettingsSourceSet')
    sources = graph.named_items(physics, '_sourceCubismPhysics')
    if not sources:
        return None
    identifier = {graph.guid(node, 'guid', required=True): graph.required(node, 'id').get('idstr')
        for node in graph.source_items(model, 'parameterSourceSet')}
    number = lambda node, name: round(graph.number(node, name), 3)

    def link(node: object, field: str, key: str) -> dict:
        return {key: {'Target': 'Parameter', 'Id': identifier[graph.guid(node, field, required=True)]},
            'Weight': number(node, 'weight'), 'Type': TYPES[graph.required(node, 'type').get('v')],
            'Reflect': graph.boolean(node, 'isReverse', False)}

    settings = []
    for source in sources:
        vertices = [{'Position': {axis.upper(): number(graph.required(vertex, 'position'), axis) for axis in 'xy'},
            'Mobility': number(vertex, 'mobility'), 'Delay': number(vertex, 'delay'),
            'Acceleration': number(vertex, 'acceleration'), 'Radius': number(vertex, 'radius')}
            for vertex in graph.named_items(source, 'vertices')]
        outputs = [link(output, 'destination', 'Destination') | {'VertexIndex': graph.integer(output, 'vertexIndex'),
            'Scale': number(output, 'angleScale')} for output in graph.named_items(source, 'outputs')]
        assert all(output['Type'] == 'Angle' and 0 < output['VertexIndex'] < len(vertices) for output in outputs)
        inputs = [link(node, 'source', 'Source') for node in graph.named_items(source, 'inputs')]
        assert inputs and outputs, 'a setting lacks an input or an output'
        settings.append({'Id': f'PhysicsSetting{len(settings) + 1}', 'Input': inputs, 'Output': outputs,
            'Vertices': vertices,
            'Normalization': {name: {key: number(source, f'normalized{name}{field}') for key, field in
                (('Minimum', 'ValueMin'), ('Default', 'DefaultValue'), ('Maximum', 'ValueMax'))}
                for name in ('Position', 'Angle')}})
    meta = {'PhysicsSettingCount': len(settings), 'TotalInputCount': sum(len(setting['Input']) for setting in settings),
        'TotalOutputCount': sum(len(setting['Output']) for setting in settings),
        'VertexCount': sum(len(setting['Vertices']) for setting in settings),
        'EffectiveForces': {'Gravity': {'X': 0, 'Y': -1}, 'Wind': {'X': 0, 'Y': 0}},
        'PhysicsDictionary': [{'Id': setting['Id'], 'Name': graph.text(source, 'name', '')}
            for setting, source in zip(settings, sources)]}
    rate = graph.field(physics, 'settingFPS')
    if rate is not None and rate.tag != 'null':
        meta['Fps'] = int(rate.text)
    return {'Version': 3, 'Meta': meta, 'PhysicsSettings': settings}


def exported(binary: Path, cmo3: Path) -> tuple[dict | None, str]:
    """The exported document, or None and the refusal of the export."""
    requests = [{'op': 'open', 'path': str(cmo3)}, {'op': 'live2d.physics3.export'}]
    done = subprocess.run([binary], input=''.join(json.dumps(request) + '\n' for request in requests),
        capture_output=True, text=True, check=True)
    opened, answer = (json.loads(line) for line in done.stdout.splitlines())
    if not opened['succeeded']:
        return None, 'the CMO3 could not be opened'
    return (answer['result'], '') if answer['succeeded'] else (None, answer['message'])


def unnamed(document: dict) -> dict:
    document = copy.deepcopy(document)
    for entry in document['Meta']['PhysicsDictionary']:
        del entry['Name']
    return document


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('inputs', nargs='+', type=Path, help='directories searched for CMO3 files')
    parser.add_argument('--binary', type=Path, default=ROOT / 'out/rust/debug/soul2d-core')
    args = parser.parse_args()
    counts = collections.Counter()
    passed = True
    for cmo3 in sorted(path for base in args.inputs for path in base.rglob('*.cmo3')):
        counts['CMO3 files'] += 1
        mine, refusal = exported(args.binary, cmo3)
        try:
            derived = derive(ORACLE.CAFF.decode(cmo3.read_bytes())[1])
        except (AssertionError, KeyError, TypeError, ValueError) as error:
            counts[f'derivation refused: {type(error).__name__}'] += 1
            passed = False
            continue
        if derived is None:
            expected = refusal == 'authoring model has no physics settings'
            counts[f'no physics setting: the export is {"refused for it" if expected else "NOT REFUSED FOR IT"}'] += 1
            passed &= expected
            continue
        equal = mine == derived
        counts[f'export against the derivation: {"equal" if equal else "DIFFERS" if mine else "EXPORT REFUSED"}'] += 1
        passed &= equal
        vendor = sorted(cmo3.parent.rglob('*.physics3.json'))
        if equal and len(vendor) == 1:
            theirs = json.loads(vendor[0].read_text(encoding='utf-8-sig'))
            counts['CMO3 files with one vendor file'] += 1
            same = unnamed(mine) == unnamed(theirs)
            counts[f'vendor file apart from the setting names: {"equal" if same else "DIFFERS"}'] += 1
            counts['vendor files equal as a whole'] += mine == theirs
            for ours, vendor_entry in zip(mine['Meta']['PhysicsDictionary'], theirs['Meta']['PhysicsDictionary']):
                counts[f'setting names: {"equal" if ours["Name"] == vendor_entry["Name"] else "differ"}'] += 1
            passed &= same
    for name, count in sorted(counts.items()):
        print(f'{count:5} {name}')
    passed &= counts['CMO3 files'] > 0
    print('PASS' if passed else 'FAIL')
    return 0 if passed else 1


if __name__ == '__main__':
    sys.exit(main())
