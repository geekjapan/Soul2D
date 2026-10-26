#!/usr/bin/env python3
"""Capture reference MOC data with a user-supplied Core and matching header."""
import argparse
import hashlib
import json
import math
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / 'tools/cubism-core-capture.c'
MODELS = (
    'hiyori_ja/hiyori_free/runtime/hiyori_free_t08.model3.json',
    'hiyori_ja/hiyori_pro/runtime/hiyori_pro_t11.model3.json',
    'ren_ja/runtime/ren.model3.json',
    'zundamon_ja/runtime/zundamon.model3.json',
)
# The collector appends this many poses with every parameter away from its default at once.
COMBINED_POSES = 12
COMBINED = object()


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def strict_json(text):
    def invalid(value):
        raise ValueError(f'Nonfinite JSON value: {value}')
    def finite(value):
        number = float(value)
        if not math.isfinite(number):
            raise ValueError('JSON number overflow')
        return number
    return json.loads(text, parse_constant=invalid, parse_float=finite)


def numeric(value):
    assert type(value) in (int, float) and math.isfinite(value)


def integer(value, minimum, maximum):
    assert type(value) is int and minimum <= value <= maximum


def vectors(rows):
    assert isinstance(rows, list)
    for row in rows:
        assert isinstance(row, list) and len(row) == 2
        for value in row:
            numeric(value)


def validate_capture(path, texture_count):
    with path.open() as stream:
        metadata = strict_json(next(stream))
        assert metadata['kind'] == 'metadata'
        integer(metadata['coreVersion'], 1, 0xffffffff)
        integer(metadata['mocVersion'], 1, 0xffffffff)
        vectors(metadata['canvas']); vectors(metadata['canvasOrigin'])
        assert len(metadata['canvas']) == len(metadata['canvasOrigin']) == 1
        assert all(v > 0 for v in metadata['canvas'][0])
        numeric(metadata['pixelsPerUnit']); assert metadata['pixelsPerUnit'] > 0
        parameters, drawables = metadata['parameters'], metadata['drawables']
        assert isinstance(parameters, list) and 0 < len(parameters) <= 4096
        assert isinstance(drawables, list) and 0 < len(drawables) <= 65536
        ids = [p['id'] for p in parameters]
        assert all(isinstance(p, str) and p for p in ids)
        assert len(set(ids)) == len(ids)
        assert all(isinstance(d['id'], str) and d['id'] for d in drawables)
        assert len({d['id'] for d in drawables}) == len(drawables)
        for p in parameters:
            for field in ('minimum', 'maximum', 'default'):
                numeric(p[field])
            assert p['minimum'] <= p['default'] <= p['maximum']
        total_vertices = total_indices = total_masks = 0
        for drawable in drawables:
            integer(drawable['textureIndex'], 0, texture_count - 1)
            integer(drawable['constantFlags'], 0, 255)
            vectors(drawable['uvs'])
            assert isinstance(drawable['indices'], list) and isinstance(drawable['masks'], list)
            assert len(drawable['indices']) % 3 == 0
            for index in drawable['indices']:
                integer(index, 0, len(drawable['uvs']) - 1)
            for mask in drawable['masks']:
                integer(mask, 0, len(drawables) - 1)
            total_vertices += len(drawable['uvs'])
            total_indices += len(drawable['indices']); total_masks += len(drawable['masks'])
        assert total_vertices <= 2000000 and total_indices <= 6000000 and total_masks <= 1000000
        expected = [(None, None)] + [(p['id'], edge) for p in parameters
            for edge in (p['minimum'], p['maximum']) if edge != p['default']] + [COMBINED] * COMBINED_POSES
        count = 0
        for line in stream:
            pose = strict_json(line)
            assert count < len(expected) and pose['kind'] == 'pose'
            assert len(pose['parameterValues']) == len(parameters)
            if expected[count] is COMBINED:
                assert (pose['parameterId'], pose['sample']) == (None, None)
                assert pose['parameterValues'] != [p['default'] for p in parameters]
                for sample, p in zip(pose['parameterValues'], parameters):
                    numeric(sample)
                    assert p['minimum'] <= sample <= p['maximum']
            else:
                parameter, value = expected[count]
                assert (pose['parameterId'], pose['sample']) == (parameter, value)
                for i, p in enumerate(parameters):
                    numeric(pose['parameterValues'][i])
                    assert pose['parameterValues'][i] == (value if p['id'] == parameter else p['default'])
            assert len(pose['drawables']) == len(drawables)
            for actual, source in zip(pose['drawables'], drawables):
                vectors(actual['positions'])
                assert len(actual['positions']) == len(source['uvs'])
                numeric(actual['opacity'])
                # Core reports the authored value unclamped: 1.00005 was observed on Core 6.0.1.
                assert -0.001 <= actual['opacity'] <= 1.001
                integer(actual['drawOrder'], -0x80000000, 0x7fffffff)
                integer(actual['renderOrder'], 0, 0x7fffffff)
                integer(actual['dynamicFlags'], 0, 255)
            # Render orders are the painter order: distinct, with gaps where a version 6 file's offscreen Parts sit.
            assert len({d['renderOrder'] for d in pose['drawables']}) == len(drawables)
            count += 1
        assert count == len(expected)
    return {'poses': count, 'parameters': len(parameters), 'drawables': len(drawables),
        'coreVersion': metadata['coreVersion'], 'mocVersion': metadata['mocVersion']}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--core-library', type=Path)
    parser.add_argument('--core-header', type=Path)
    parser.add_argument('--inputs', type=Path, default=ROOT / 'build/verification/live2d-inputs-vjq5llby')
    parser.add_argument('--self-check', action='store_true', help='check collector helpers without loading Core')
    args = parser.parse_args()
    if not args.self_check:
        if not args.core_library or not args.core_header:
            parser.error('--core-library and --core-header are required; no SDK is downloaded or installed')
        args.core_library, args.core_header = args.core_library.resolve(), args.core_header.resolve()
        if not args.core_library.is_file() or not args.core_header.is_file():
            parser.error('Core library and header must be existing files')
        if args.core_header.name != 'Live2DCubismCore.h':
            parser.error('Provide the matching Live2DCubismCore.h')
    compiler = shutil.which('cc')
    if not compiler:
        parser.error('An existing C compiler is required')
    verification = ROOT / 'build/verification'
    verification.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='cubism-reference-', dir=verification))
    binary = run / 'capture'
    result = {'status': 'FAIL', 'evidence': str(run), 'comparison': 'NOT_RUN',
        'scope': 'Default, individual-edge and combined pseudo-random Core data, each pose from a settled default '
            'pose; dynamic flags describe the transition from it. No single-parameter midpoints, animation samples, material colors, '
            'offscreen ordering, CMO writes, '
            'rasterization, SDK physics/pose, or equivalence claim.'}
    try:
        command = [compiler, '-std=c11', '-Wall', '-Wextra', '-Werror', str(SOURCE), '-o', str(binary)]
        hashes = {str(SOURCE.relative_to(ROOT)): sha(SOURCE), str(Path(__file__).relative_to(ROOT)): sha(Path(__file__))}
        if args.self_check:
            command.append('-DCORE_CAPTURE_SELF_CHECK')
        else:
            command += ['-I', str(args.core_header.parent), str(args.core_library),
                '-Wl,-rpath,' + str(args.core_library.parent)]
            hashes[str(args.core_header)] = sha(args.core_header)
            hashes[str(args.core_library)] = sha(args.core_library)
        compiled = subprocess.run(command, text=True, capture_output=True, timeout=120)
        (run / 'build.log').write_text(compiled.stdout + compiled.stderr)
        assert compiled.returncode == 0, 'Collector build failed; see build.log'
        if args.self_check:
            checked = subprocess.run([str(binary)], text=True, capture_output=True, timeout=30)
            assert checked.returncode == 0, checked.stderr
            assert strict_json(checked.stdout) == {'string': 'quote" slash\\ line\n tab\t', 'number': 0.375, 'count': 10}
            for malformed in ('nan', 'count', 'size'):
                checked = subprocess.run([str(binary), malformed], text=True, capture_output=True, timeout=30)
                assert checked.returncode != 0 and checked.stderr, malformed
            # Synthetic rows check the receipt parser, never vendor output.
            metadata = {'kind': 'metadata', 'coreVersion': 1, 'mocVersion': 1,
                'canvas': [[100, 200]], 'canvasOrigin': [[50, 100]], 'pixelsPerUnit': 100,
                'parameters': [{'id': 'P', 'minimum': -1, 'maximum': 1, 'default': 0}],
                'drawables': [{'id': 'D', 'textureIndex': 0, 'indices': [0, 0, 0],
                    'uvs': [[0, 0]], 'constantFlags': 0, 'masks': []}]}
            rows = [metadata] + [{'kind': 'pose', 'parameterId': parameter, 'sample': value,
                'parameterValues': [value if parameter else 0],
                'drawables': [{'positions': [[0, 0]], 'opacity': 1, 'drawOrder': 0, 'renderOrder': 0, 'dynamicFlags': 1}]}
                for parameter, value in ((None, None), ('P', -1), ('P', 1))]
            rows += [dict(rows[1], parameterValues=[0.5]) for _ in range(COMBINED_POSES)]
            fixture = run / 'parser-fixture.jsonl'
            fixture.write_text(''.join(json.dumps(r) + '\n' for r in rows))
            assert validate_capture(fixture, 1)['poses'] == 3 + COMBINED_POSES
            bad_rows = [rows[:-1], rows + [rows[-1]]] + [json.loads(json.dumps(rows)) for _ in range(14)]
            bad_rows[14][-1]['parameterValues'] = [1.5]
            bad_rows[15][-1]['parameterValues'] = [0]
            bad_rows[2][1]['parameterValues'] = [1]
            bad_rows[3][1]['drawables'][0]['positions'] = []
            bad_rows[4][1]['drawables'][0]['opacity'] = float('nan')
            del bad_rows[5][1]['drawables'][0]['drawOrder']
            bad_rows[6][1]['drawables'][0]['dynamicFlags'] = 256
            bad_rows[7][1]['drawables'][0]['positions'] = [['bad']]
            bad_rows[8][0]['drawables'][0]['uvs'] = [[]]
            bad_rows[9][0]['drawables'][0]['indices'] = [1, 0, 0]
            bad_rows[10][0]['drawables'][0]['masks'] = [-1]
            bad_rows[11][0]['drawables'][0]['constantFlags'] = True
            bad_rows[12][0]['parameters'][0]['default'] = 2
            bad_rows[13][0]['pixelsPerUnit'] = float('inf')
            for malformed in bad_rows:
                fixture.write_text(''.join(json.dumps(r) + '\n' for r in malformed))
                try:
                    validate_capture(fixture, 1)
                except (AssertionError, ValueError, KeyError):
                    pass
                else:
                    raise AssertionError('Malformed capture accepted')
            try:
                strict_json('{"overflow":1e400}')
            except ValueError:
                pass
            else:
                raise AssertionError('JSON overflow accepted')
            result.update(status='HELPERS_PASS', vendorCapture='NOT_RUN')
        else:
            cases = []
            # The four supplied models by name, or every runtime model under another --inputs folder.
            models = [(args.inputs / relative).resolve() for relative in MODELS]
            if not all(model3.is_file() for model3 in models):
                models = sorted(path.resolve() for path in args.inputs.rglob('*.model3.json'))
                assert models, 'no .model3.json under --inputs'
            for model3 in models:
                references = strict_json(model3.read_text())['FileReferences']
                def asset(name):
                    path = (model3.parent / name).resolve()
                    assert path.is_relative_to(model3.parent) and path.is_file(), 'Invalid runtime asset path'
                    return path
                moc = asset(references['Moc'])
                textures = [asset(t) for t in references['Textures']]
                assert textures
                for path in (model3, moc, *textures):
                    hashes[str(path)] = sha(path)
                output = run / (model3.stem + '.jsonl')
                with output.open('w') as stream:
                    captured = subprocess.run([str(binary), str(moc)], stdout=stream,
                        stderr=subprocess.PIPE, text=True, timeout=300)
                (run / (model3.stem + '.stderr')).write_text(captured.stderr)
                assert captured.returncode == 0, f'Core capture failed: {model3}'
                # Core 6 prints its version line on stdout before the first row.
                rows = output.read_text().splitlines(keepends=True)
                first = next((index for index, row in enumerate(rows) if row.startswith('{')), len(rows))
                (run / (model3.stem + '.banner')).write_text(''.join(rows[:first]))
                output.write_text(''.join(rows[first:]))
                counts = validate_capture(output, len(textures))
                cases.append({'model3': str(model3), 'moc': str(moc), 'output': str(output),
                    'outputHash': sha(output), **counts})
            result.update(status='CAPTURED', vendorCapture='CAPTURED', cases=cases)
        for path, expected in hashes.items():
            assert sha(Path(path)) == expected, f'Input changed: {path}'
        result['sourceHashes'] = hashes
        result['collectorHash'] = sha(binary)
        result['buildCommand'] = command
    except Exception as error:
        result.update(status='FAIL', error=repr(error))
    (run / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result))
    return 0 if result['status'] in ('HELPERS_PASS', 'CAPTURED') else 1


if __name__ == '__main__':
    raise SystemExit(main())
