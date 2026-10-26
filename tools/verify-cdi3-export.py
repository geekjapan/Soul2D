#!/usr/bin/env python3
"""Compare `live2d.cdi3.export` with a derivation written apart from it, and with the vendor `.cdi3.json` files.

For every CMO3 below the given directories the export is compared with the display information this script derives from
the XML. Where exactly one `.cdi3.json` sits below the folder of a CMO3, the export is compared with that file after
parsing. The files are read where they are, and only counts are printed.

Exits 0 when every export equals the derivation and every vendor file is equal or differs in one of two known ways:
it lists one Part ID the CMO3 does not hold, or it has no `CombinedParameters` although the CMO3 pairs parameters.
A file the export or the derivation refuses is counted and fails the run.
"""
import argparse
import collections
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


def derive(xml: bytes) -> dict:
    graph = ORACLE.XMLGraph(xml)
    model = next(node for node in graph.root.iter('CModelSource')
        if node.get('xs.ref') is None and any(isinstance(child.tag, str) for child in node))
    identifier = lambda node: graph.required(node, 'id').get('idstr')

    groups = graph.named_items(graph.required(model, 'parameterGroupSet'), '_groups')
    group_of = {graph.guid(node, 'guid', required=True): node for node in groups}
    parameter_of = {graph.guid(node, 'guid', required=True): node
        for node in graph.source_items(model, 'parameterSourceSet')}
    (top,) = [node for node in groups if graph.guid(node, 'parentGroupGuid') is None]

    def row(node: object, group: object) -> dict:
        assert group_of[graph.guid(node, 'parentGroupGuid', required=True)] is group, 'a member names another parent'
        return {'Id': identifier(node), 'GroupId': '' if group is top else identifier(group),
            'Name': graph.text(node, 'name', '')}

    parameters, parameter_groups, pairs = [], [], []

    def walk(group: object) -> None:
        members = graph.named_items(group, '_childGuids')
        for member, after in zip(members, members[1:] + [None]):
            guid = member.get('uuid')
            if member.tag == 'CParameterGroupGuid':
                parameter_groups.append(row(group_of[guid], group))
                walk(group_of[guid])
            else:
                assert member.tag == 'CParameterGuid', 'a group member is neither a parameter nor a group'
                parameters.append(row(parameter_of[guid], group))
                if graph.boolean(parameter_of[guid], 'combined', False):
                    assert after is not None and after.tag == 'CParameterGuid', 'a combined parameter has no partner'
                    assert not (pairs and pairs[-1][1] == parameters[-1]['Id']), 'combined parameters overlap'
                    pairs.append([parameters[-1]['Id'], identifier(parameter_of[after.get('uuid')])])

    walk(top)
    assert len(parameters) == len(parameter_of) and len(parameter_groups) + 1 == len(groups), 'a member is outside the tree'

    part_of = {graph.guid(node, 'guid', required=True): node for node in graph.source_items(model, 'partSourceSet')}
    parts, level = [], [part_of[graph.guid(graph.required(model, 'rootPart'), 'guid', required=True)]]
    while level:
        children = [part_of[child.get('uuid')] for part in level
            for child in graph.named_items(part, '_childGuids') if child.tag == 'CPartGuid']
        parts += [{'Id': identifier(part), 'Name': graph.text(part, 'localName', '')} for part in children]
        level = children
    assert len(parts) + 1 == len(part_of), 'a Part is outside the tree'
    result = {'Version': 3, 'Parameters': parameters, 'ParameterGroups': parameter_groups, 'Parts': parts}
    return result | ({'CombinedParameters': pairs} if pairs else {})


def exported(binary: Path, cmo3: Path) -> dict | None:
    requests = [{'op': 'open', 'path': str(cmo3)}, {'op': 'live2d.cdi3.export'}]
    done = subprocess.run([binary], input=''.join(json.dumps(request) + '\n' for request in requests),
        capture_output=True, text=True, check=True)
    answer = json.loads(done.stdout.splitlines()[-1])
    return answer['result'] if answer['succeeded'] else None


def against_vendor(mine: dict, theirs: dict, counts: collections.Counter) -> bool:
    known = True
    for name in ('Version', 'Parameters', 'ParameterGroups'):
        equal = mine[name] == theirs.get(name)
        counts[f'{name}: {"equal" if equal else "DIFFERS"}'] += 1
        known &= equal
    parts, vendor_parts = mine['Parts'], theirs.get('Parts', [])
    held = {part['Id'] for part in parts}
    if parts == vendor_parts:
        counts['Parts: equal'] += 1
    elif [part for part in vendor_parts if part['Id'] in held] == parts and len(vendor_parts) == len(parts) + 1:
        counts['Parts: the vendor file lists one Part ID the CMO3 does not hold, the rest is equal'] += 1
    else:
        counts['Parts: DIFFERS'] += 1
        known = False
    pairs, vendor_pairs = mine.get('CombinedParameters'), theirs.get('CombinedParameters')
    if pairs == vendor_pairs:
        counts[f'CombinedParameters: {"equal" if pairs else "absent from both"}'] += 1
    elif vendor_pairs is None:
        counts['CombinedParameters: absent from the vendor file, present in the export'] += 1
    else:
        counts['CombinedParameters: DIFFERS'] += 1
        known = False
    counts['vendor files equal as a whole'] += mine == theirs
    return known


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('inputs', nargs='+', type=Path, help='directories searched for CMO3 files')
    parser.add_argument('--binary', type=Path, default=ROOT / 'out/rust/debug/soul2d-core')
    args = parser.parse_args()
    counts = collections.Counter()
    passed = True
    for cmo3 in sorted(path for base in args.inputs for path in base.rglob('*.cmo3')):
        counts['CMO3 files'] += 1
        mine = exported(args.binary, cmo3)
        try:
            derived = derive(ORACLE.CAFF.decode(cmo3.read_bytes())[1])
        except (AssertionError, KeyError, ValueError) as refusal:
            derived = None
            counts[f'derivation refused: {type(refusal).__name__} {refusal if isinstance(refusal, AssertionError) else ""}'] += 1
        equal = mine is not None and mine == derived
        counts[f'export against the derivation: {"equal" if equal else "DIFFERS" if mine else "EXPORT REFUSED"}'] += 1
        passed &= equal
        vendor = sorted(cmo3.parent.rglob('*.cdi3.json'))
        # The derivation reaches every Part, so an equal export lists every Part ID the CMO3 holds.
        if equal and len(vendor) == 1:
            counts['CMO3 files with one vendor file'] += 1
            passed &= against_vendor(mine, json.loads(vendor[0].read_text(encoding='utf-8-sig')), counts)
    for name, count in sorted(counts.items()):
        print(f'{count:4} {name}')
    passed &= counts['CMO3 files'] > 0
    print('PASS' if passed else 'FAIL')
    return 0 if passed else 1


if __name__ == '__main__':
    sys.exit(main())
