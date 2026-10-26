#!/usr/bin/env python3
import argparse
import base64
from functools import partial
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import traceback

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('split_quality_context', ROOT / 'tools/verify-part-split-context.py')
CONTEXT = importlib.util.module_from_spec(spec)
spec.loader.exec_module(CONTEXT)
REAL = CONTEXT.REAL
FIXTURES = {
    '6aa379f3b83634e85fff5387b866fce56ef58601b3e22e04a1494e7ba151a08a': {
        'id':'alien', 'size':[225,175], 'mergedChannels':4, 'description':'nonhuman side-view Alien PSD',
        'page':'https://lpc.opengameart.org/content/2d-sprite-shadow-alien-t-rex-monster-imagesanimationsscmlpsd',
        'background':'All18 artist pixel layers retained; no exclusions or semantic relabeling',
        'baselineRoot':'issue100-quality', 'operatorRoot':'issue100-operator-quality',
        'indices':[1,2,5,6,7,8,9,11,13,14,15,16,19,22,25,28,29,32],
        'paths':[
            '/tail/tail1', '/tail/tail2', '/leg1 copy/arms back', '/leg1 copy/Layer 10',
            '/leg1 copy/Layer 14', '/leg1 copy/Layer 13', '/leg1 copy/Layer 7', '/body',
            '/leg1 frt/Layer 10', '/leg1 frt/Layer 14', '/leg1 frt/Layer 13', '/leg1 frt/Layer 7',
            '/arm4 frt/Layer 7 copy', '/arm2 frt/Layer 7 copy', '/mini arm1/Layer 7 copy',
            '/head/head copy 3', '/head/head copy 2', '/arm1 frt/Layer 7 copy'],
    },
    '505b9b3a670ca14e0cf2e1bb953698791fdce08ef79e56383e1891549cb53dcd': {
        'id':'soldier', 'size':[101,161], 'mergedChannels':3, 'description':'human side-view Soldier PSD',
        'page':'https://opengameart.org/content/2d-soldier',
        'background':'All14 layers retained, including uniform opaque black full-canvas background; no exclusions or semantic relabeling',
        'baselineRoot':'issue100-soldier-quality', 'operatorRoot':'issue100-soldier-operator-quality',
        'indices':list(range(14)),
        'paths':['/Слой 6', '/Слой 4', '/Слой 15 (копия)', '/Слой 15', '/Слой 10 (копия)', '/Слой 10',
                 '/Слой 12', '/Слой 7 (копия)', '/Слой 3', '/weapon', '/left arm', '/right arm', '/Слой 7', '/Слой 8'],
    },
}
OPERATORS = {'flood': {'op':'flood', 'tolerance':16, 'mode':'add'},
             'bounded_fill': {'op':'boundedFill', 'lineThreshold':80, 'mode':'add'}}
SEED_POLICY = ('Most frequent exact RGB among eligible uncovered pixels; color ties use first row-major '
               'occurrence; within that color use highest alpha, then first row-major pixel')


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def iou(a, b):
    union = sum(x > 127 or y > 127 for x, y in zip(a, b))
    return sum(x > 127 and y > 127 for x, y in zip(a, b)) / union if union else None


def composite(layers, pixels):
    out = [0.0] * (pixels * 4)
    for rgba in layers:
        assert len(rgba) == pixels * 4
        for at in range(0, len(rgba), 4):
            alpha = rgba[at + 3] / 255
            for channel in range(3):
                out[at + channel] = rgba[at + channel] * alpha + out[at + channel] * (1 - alpha)
            out[at + 3] = alpha + out[at + 3] * (1 - alpha)
    result = bytearray(pixels * 4)
    for at in range(0, len(result), 4):
        alpha = out[at + 3]
        if alpha:
            for channel in range(3):
                result[at + channel] = round(out[at + channel] / alpha)
        result[at + 3] = round(alpha * 255)
    return bytes(result)


def pixel_error(a, b):
    assert len(a) == len(b) and len(a) % 4 == 0
    alpha = [abs(a[i] - b[i]) for i in range(3, len(a), 4)]
    rgb = [abs(a[i + c] * a[i + 3] - b[i + c] * b[i + 3]) / 255
           for i in range(0, len(a), 4) for c in range(3)]
    worst = max(range(len(rgb)), key=rgb.__getitem__) // 3 * 4
    return {'alphaMax': max(alpha), 'alphaMean': sum(alpha) / len(alpha),
            'premultipliedRgbMax': max(rgb), 'premultipliedRgbMean': sum(rgb) / len(rgb),
            'worstPremultipliedPixel': {'pixelIndex': worst // 4, 'referenceRgba': list(a[worst:worst + 4]),
                                        'actualRgba': list(b[worst:worst + 4])}}


def require_quantized_reconstruction(source, rendered):
    assert source[3::4] == rendered[3::4], 'rendered alpha differs'
    for at in range(0, len(source), 4):
        alpha = source[at + 3]
        for channel in range(3):
            stored = source[at + channel] * alpha // 255
            encoded = rendered[at + channel] * alpha / 255
            assert abs(encoded - stored) <= alpha / 510 + 1e-5, (at // 4, channel, stored, encoded)


def read_image(host, njc, run, label, tail):
    command = [str(njc), '--port', str(host.port), *tail]
    completed = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, timeout=30)
    assert completed.returncode == 0, completed.stderr
    response = json.loads(completed.stdout)['result']
    items = response.get('contents', response.get('content', []))
    item = next(i for i in items if 'blob' in i or i.get('type') == 'image')
    encoded = item.pop('blob', None) or item.pop('data')
    png = base64.b64decode(encoded, validate=True)
    item['sha256'] = hashlib.sha256(png).hexdigest()
    (run / (label + '.png')).write_bytes(png)
    (run / (label + '.json')).write_text(json.dumps(response, indent=2) + '\n')
    return REAL.decode_png(png)


def operator_seed(rgba, claimed, method):
    colors = {}
    for index, covered in enumerate(claimed):
        r, g, b, a = rgba[index * 4:index * 4 + 4]
        if covered or not a or (method == 'bounded_fill' and (299*r + 587*g + 114*b)//1000 < 80):
            continue
        color = (r, g, b)
        if color not in colors:
            colors[color] = [0, index, a, index]
        row = colors[color]
        row[0] += 1
        if a > row[2]:
            row[2], row[3] = a, index
    return max(colors.values(), key=lambda row:(row[0], -row[1]))[3] if colors else None


def predict_operator(host, image, run, method):
    width, height, channels, rgba = image('operator-source', ['read', 'resource://soul2d/part-split/source'])
    assert channels == 4 and len(rgba) == width * height * 4
    claimed = bytearray(width * height)
    masks, parts = [], []

    def materialize(name, mask, seed):
        assert any(mask) and set(mask) <= {0,255}
        path = run / ('mask-' + name + '.png')
        path.write_bytes(CONTEXT.gray_png(width, height, mask))
        inspected = host.call('inspect-' + name, 'split_mask_inspect', {'path':str(path)})
        assert (inspected['width'], inspected['height']) == (width, height)
        host.call('raster-' + name, 'split_part_set', {'name':name, 'steps':[inspected['step']]})
        w, h, c, actual = image('prediction-' + name, ['read', 'resource://soul2d/part-split/masks/' + name])
        assert (w, h, c, actual) == (width, height, 1, mask)
        assert not any(a and b for a,b in zip(claimed, mask))
        for i,value in enumerate(mask):
            claimed[i] |= value
        masks.append(mask)
        parts.append({'name':name, 'seed':seed, 'pixels':sum(value > 0 for value in mask),
                      'rasterStep':inspected['step'], 'rasterSha256':digest(path)})

    for index in range(31):
        seed = operator_seed(rgba, claimed, method)
        if seed is None:
            break
        point = [seed % width, seed // width]
        name = f'Region{index:02d}'
        host.call('operator-' + name, 'split_part_set', {'name':name, 'steps':[
            {**OPERATORS[method], 'point':point}, {'op':'alpha', 'threshold':0, 'mode':'intersect'}]})
        w, h, c, raw = image('raw-' + name, ['read', 'resource://soul2d/part-split/masks/' + name])
        assert (w, h, c) == (width, height, 1) and raw[seed] == 255
        mask = bytes(value if not claimed[i] else 0 for i,value in enumerate(raw))
        materialize(name, mask, point)
    residual = bytes(255 if rgba[i*4+3] and not covered else 0 for i,covered in enumerate(claimed))
    if any(residual):
        materialize('Residual', residual, None)
    assert len(masks) <= 32 and bytes(claimed) == bytes(255 if a else 0 for a in rgba[3::4])
    plan = host.call('operator-plan', 'split_plan', {})
    assert plan['unclaimedPixels'] == plan['multiplyClaimedPixels'] == 0
    assert len(plan['plan']['parts']) == len(masks)
    (run / 'operator-plan.json').write_text(json.dumps(plan['plan'], indent=2)+'\n')
    return masks, {'method':method, 'operator':OPERATORS[method], 'seedPolicy':SEED_POLICY,
                   'maxParts':32, 'regionLimit':31, 'parts':parts,
                   'sourceRgbaSha256':hashlib.sha256(rgba).hexdigest()}


def check_operators(core, njc, run, result):
    result.update(scope='Generated fixtures only; no PSD opened or artist-layer IoU computed',
                  inputContract='32 disconnected opaque tiles and one alpha=1 pixel with matching hidden RGB',
                  prediction='Alpha baseline and both fixed operator profiles on generated inputs only',
                  metric='No artist-layer scoring', qualityAcceptance='NOT_EVALUATED_GENERATED_ONLY',
                  generatedCases=[], cleanup=[])
    tiled = bytearray([120,120,120,0] * (40*20))
    for tile in range(32):
        for y in range((tile//8)*5, (tile//8)*5+2):
            for x in range((tile%8)*5, (tile%8)*5+2):
                tiled[(y*40+x)*4:(y*40+x)*4+4] = bytes([255,255,255,255])
    for method in ['alpha_components', *OPERATORS]:
        for case,width,height,rgba in [('cap',40,20,bytes(tiled)),
                                      ('alpha',3,1,bytes([120,120,120,0,120,120,120,1,120,120,120,0]))]:
            folder = run / (method+'-'+case)
            folder.mkdir()
            source = folder / 'source.png'
            source.write_bytes(CONTEXT.PNG.png(width,height,rgba))
            source_hash = digest(source)
            host = CONTEXT.DRIVER.Host(folder,core,njc,CONTEXT.DRIVER.free_port())
            try:
                host.wait_ready()
                host.call('open','split_open',{'path':str(source)})
                image = partial(read_image,host,njc,folder)
                assert image('check-source',['read','resource://soul2d/part-split/source']) == (width,height,4,rgba)
                if method == 'alpha_components':
                    masks, components, groups = predict_components(host,image,width,height)
                    prediction = {'method':method,'components':components,'groups':groups}
                else:
                    masks, prediction = predict_operator(host,image,folder,method)
                    assert prediction['sourceRgbaSha256'] == hashlib.sha256(rgba).hexdigest()
                plan = host.call('checked-plan','split_plan',{})
                assert plan['unclaimedPixels'] == plan['multiplyClaimedPixels'] == 0
                if case == 'cap':
                    assert len(masks) == 32
                    if method != 'alpha_components':
                        assert prediction['parts'][-1]['name'] == 'Residual'
                else:
                    assert masks == [bytes([0,255,0])]
                host.call('apply','split_apply',{})
                w,h,c,rendered = image('applied-render',['tools','call','render','--json',json.dumps({
                    'width':width,'height':height,'bounds':[-(width//2),-(height//2),width-width//2,height-height//2]})])
                assert (w,h,c) == (width,height,4)
                require_quantized_reconstruction(rgba,rendered)
                assert digest(source) == source_hash
                result['generatedCases'].append({'method':method,'case':case,'prediction':prediction,'renderConservation':'PASS'})
            finally:
                host.close()
                result['cleanup'].append({'pid':host.child.pid,'exit':host.child.returncode,'running':host.child.poll() is None})
                assert not result['cleanup'][-1]['running']


def self_check():
    assert iou(bytes([255, 0]), bytes([255, 255])) == 0.5
    assert iou(bytes([0, 255]), bytes([255, 0])) == 0
    assert iou(bytes([0, 0]), bytes([0, 0])) is None
    assert composite([bytes([0, 0, 255, 255]), bytes([255, 0, 0, 128])], 1) == bytes([128, 0, 127, 255])
    assert composite([bytes([90, 40, 10, 0])], 1) == bytes(4)
    error = pixel_error(bytes([200, 0, 0, 0]), bytes(4))
    assert error['alphaMax'] == error['premultipliedRgbMax'] == 0
    require_quantized_reconstruction(bytes([10, 10, 10, 151]), bytes([8, 8, 8, 151]))
    try:
        require_quantized_reconstruction(bytes([255, 0, 0, 255]), bytes([250, 0, 0, 255]))
    except AssertionError:
        pass
    else:
        raise AssertionError('quantization guard accepted changed color')
    rgba = bytes([100,100,100,1,100,100,100,255,200,200,200,255,200,200,200,255,100,100,100,0])
    for method in OPERATORS:
        assert operator_seed(rgba,bytes(5),method) == 1
        assert operator_seed(rgba,bytes([0,255,0,0,0]),method) == 2
        assert operator_seed(rgba,bytes([255]*5),method) is None


def ground_truth(psd, run, fixture):
    data = psd.read_bytes()
    assert FIXTURES.get(hashlib.sha256(data).hexdigest()) == fixture, 'pinned fixture bytes changed'
    doc = REAL.parse_psd(data)
    width, height = doc['width'], doc['height']
    assert ([width, height], doc['depth'], doc['mode']) == (fixture['size'], 8, 3)
    assert all(layer['blend'] == b'norm' and layer['opacity'] == 255 and not layer['clip']
               and not layer['has_mask'] and not layer['hidden'] for layer in doc['layers'])
    paths = {}

    def visit(nodes, parent=''):
        for node in nodes:
            path = parent + '/' + node['layer']['name']
            if node['kind'] == 'part':
                paths[id(node['layer'])] = path
            visit(node['children'], path)

    visit(REAL.expected_tree(doc))
    rows, images = [], []
    for layer_index, layer in enumerate(doc['layers']):
        if id(layer) not in paths:
            continue
        w, h = layer['right'] - layer['left'], layer['bottom'] - layer['top']
        assert not layer['irrelevant'] and w > 0 and h > 0
        planes = {cid: REAL.decode_psd_channel(data, at, size, w, h)
                  for cid, at, size in layer['chan_at'] if cid in (0, 1, 2, -1)}
        assert all(planes.get(c) is not None and len(planes[c]) == w * h for c in (0, 1, 2, -1))
        rgba = bytearray(width * height * 4)
        for y in range(h):
            for x in range(w):
                dx, dy = x + layer['left'], y + layer['top']
                if 0 <= dx < width and 0 <= dy < height:
                    at, source = (dy * width + dx) * 4, y * w + x
                    rgba[at:at + 4] = bytes(planes[c][source] for c in (0, 1, 2, -1))
        mask = bytes(rgba[3::4])
        name = f'groundtruth-{len(rows):02d}'
        (run / (name + '.png')).write_bytes(CONTEXT.gray_png(width, height, mask))
        rows.append({'id': name, 'path': paths[id(layer)], 'fileLayerIndex': layer_index,
                     'order': len(rows), 'rect': [layer['left'], layer['top'], w, h],
                     'artistLayerMask': name + '.png', 'alphaSupportPixels': sum(a > 127 for a in mask)})
        images.append(bytes(rgba))
    assert [row['path'] for row in rows] == fixture['paths']
    assert [row['fileLayerIndex'] for row in rows] == fixture['indices']
    flattened = composite(images, width * height)
    (run / 'flattened-reference.png').write_bytes(CONTEXT.PNG.png(width, height, flattened))
    merged = REAL.psd_merged(doc)
    assert doc['transparency_first'] == (fixture['mergedChannels'] == 4)
    assert len(merged) == fixture['mergedChannels'] and all(len(p) == width * height for p in merged)
    if len(merged) == 3:
        assert all(a == 255 for a in flattened[3::4]), 'RGB diagnostic requires an opaque reference'
        merged.append(bytes([255]) * (width * height))
    merged_rgba = bytes(merged[c][i] for i in range(width * height) for c in range(4))
    (run / 'psd-stored-merged.png').write_bytes(CONTEXT.PNG.png(width, height, merged_rgba))
    return rows, images, flattened, pixel_error(flattened, merged_rgba)


def main():
    parser = argparse.ArgumentParser(description='Measure fixed split methods against pinned artist PSD layers, or check operators on generated inputs.')
    parser.add_argument('--core', type=Path, required=True)
    parser.add_argument('--njc', type=Path, required=True)
    inputs = parser.add_mutually_exclusive_group(required=True)
    inputs.add_argument('--psd', type=Path)
    inputs.add_argument('--check-operators', action='store_true')
    parser.add_argument('--method', choices=['alpha_components', *OPERATORS])
    args = parser.parse_args()
    if args.check_operators and args.method is not None:
        parser.error('--check-operators checks all fixed profiles; do not give --method')
    assert __debug__, 'verification requires assertions'
    self_check()
    core, njc = args.core.resolve(), args.njc.resolve()
    psd = args.psd.resolve() if args.psd else None
    method = args.method or 'alpha_components'
    fixture_hash = digest(psd) if psd else None
    fixture = FIXTURES.get(fixture_hash)
    if psd and fixture is None:
        parser.error('--psd must match a pinned creator-declared CC0 fixture hash')
    files = [core,njc] + ([psd] if psd else [])
    evidence_name = fixture['baselineRoot' if method == 'alpha_components' else 'operatorRoot'] if fixture else 'issue100-operator-quality'
    evidence = ROOT / 'build/verification' / evidence_name
    evidence.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='run-', dir=evidence))
    sources = set((ROOT / 'rust/src').rglob('*.rs')) | {
        Path(__file__).resolve(), Path(CONTEXT.__file__), Path(REAL.__file__),
        Path(CONTEXT.PNG.__file__), Path(CONTEXT.DRIVER.__file__), ROOT / 'rust/Cargo.toml', ROOT / 'rust/Cargo.lock',
        ROOT / 'doc/part-split.md', ROOT / '.cursor/skills/verify-soul2d/features/README.md'}
    hashes = {str(p.relative_to(ROOT)): digest(p) for p in sorted(sources)}
    input_hashes = {str(p): digest(p) for p in files}
    result = {'status': 'FAIL', 'qualityAcceptance': 'UNSET; no agreed semantic quality target',
              'scope': (f"One pinned creator-declared CC0 {fixture['description']}; no provider, accepted human taxonomy or vendor proof"
                        if fixture else 'Generated checks only'),
              'inputContract': f"Harness-generated PNG from all{len(fixture['paths']) if fixture else 0} artist pixel layers in pinned bottom-to-top "
                               'order, normal source-over in RGB byte values, no color management. '
                               'PSD stored/displayed appearance equivalence is unverified; flatten comparisons are diagnostics only.',
              'sourceHashes': hashes, 'inputHashes': input_hashes, 'selfCheck': 'PASS',
              'metric': 'Per artist-layer alpha>127 IoU; each row independently chooses its best predicted mask. '
                        'This bounds one-to-one matching of these fixed predictions only; it is not semantic '
                        'matching or an upper bound on attainable segmentation quality.',
              'prediction': 'Four-connected alpha>0 islands, largest31 retained individually and remaining islands '
                            'unioned if needed to fit the32Part cap; no groundtruth masks or paths supplied to the product'}
    host = None
    try:
        if args.check_operators:
            check_operators(core,njc,run,result)
            result['status'] = 'GENERATED_OPERATORS_VERIFIED'
            return
        rows, images, flattened, merged_error = ground_truth(psd, run, fixture)
        width, height = fixture['size']
        result['fixture'] = {'id':fixture['id'], 'sha256':fixture_hash, 'creatorPage':fixture['page'],
                             'license':'Creator-declared CC0', 'size':fixture['size'],
                             'retainedFileLayerIndices':fixture['indices'], 'background':fixture['background']}
        result['storedMergedVsIndependentFlatten'] = merged_error
        host = CONTEXT.DRIVER.Host(run, core, njc, CONTEXT.DRIVER.free_port())
        host.wait_ready()
        result['hostPid'] = host.child.pid

        image = partial(read_image,host,njc,run)

        if method == 'alpha_components':
            host.call('open-psd', 'split_open', {'path': str(psd)})
            w, h, channels, pixels = image('soul2d-psd-flatten', ['read', 'resource://soul2d/part-split/source'])
            assert (w, h, channels) == (width, height, 4)
            result['productPsdVsIndependentFlatten'] = pixel_error(flattened, pixels)
        host.call('open-independent-flatten', 'split_open', {'path': str(run / 'flattened-reference.png')})
        if method == 'alpha_components':
            masks, components, groups = predict_components(host,image,width,height)
            result.update(components=components,predictionGroups=groups)
        else:
            masks, prediction = predict_operator(host,image,run,method)
            assert prediction['sourceRgbaSha256'] == hashlib.sha256(flattened).hexdigest()
            result.update(operatorPrediction=prediction,
                          prediction='Input-only fixed color/line regions, product selections materialized as disjoint rasters plus alpha residual; no artist metadata sent to the product')
        for row, rgba in zip(rows, images):
            scores = [iou(rgba[3::4], mask) for mask in masks]
            row['bestPredictedMask'] = max(range(len(scores)), key=scores.__getitem__)
            row['bestArtistLayerIou'] = max(scores)
        result.update(groundTruth=rows,predictionCount=len(masks),groundTruthCount=len(rows),
                      meanBestArtistLayerIou=sum(r['bestArtistLayerIou'] for r in rows)/len(rows),method=method)
        host.call('apply', 'split_apply', {})
        render_args = {'width': width, 'height': height,
                       'bounds': [-(width // 2), -(height // 2), width - width // 2, height - height // 2]}
        w, h, channels, rendered = image('applied-render', ['tools', 'call', 'render', '--json', json.dumps(render_args)])
        assert (w, h, channels) == (width, height, 4)
        result['appliedRenderVsIndependentFlatten'] = pixel_error(flattened, rendered)
        require_quantized_reconstruction(flattened, rendered)
        result['renderConservation'] = 'ALPHA_EXACT; RGB follows floor(channel*alpha/255) texture storage '
        result['renderConservation'] += 'and nearest-integer unpremultiplied PNG output, including floating-point ties'
        result['quantizationEvidence'] = {'textureStorage': 'rust/src/part_split.rs:part_image',
                                          'pngOutput': 'rust/src/render.rs:render_png_composited',
                                          'bound': 'At each channel, renderedRGB*alpha/255 must be within alpha/510+1e-5 '
                                                   'of floor(sourceRGB*alpha/255). Alpha bytes must be identical.'}
        result['status'] = 'BASELINE_MEASURED' if method == 'alpha_components' else 'OPERATOR_MEASURED'
    except BaseException:
        result['error'] = traceback.format_exc()
        raise
    finally:
        try:
            if host is not None:
                host.close()
                result['cleanup'] = {'pid': host.child.pid, 'exit': host.child.returncode,
                                     'running': host.child.poll() is None}
                assert not result['cleanup']['running']
            assert {str(p): digest(p) for p in files} == input_hashes
            assert {str(p.relative_to(ROOT)): digest(p) for p in sorted(sources)} == hashes
        except BaseException:
            result.update(status='FAIL', cleanupError=traceback.format_exc())
            raise
        finally:
            (run / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
            print(run)


def predict_components(host,image,width,height):
    before = host.call('before-components', 'snapshot', {})
    components, offset = [], 0
    while True:
        page = host.call('components-' + str(offset), 'split_components', {'offset': offset})
        components.extend(page['components'])
        if page['nextOffset'] is None:
            assert len(components) == page['totalComponents']
            break
        offset = page['nextOffset']
    assert host.call('after-components', 'snapshot', {}) == before
    assert 0 < len(components) <= 64, 'candidate has too many islands for this bounded baseline'
    ordered = sorted(components, key=lambda component: (-component['pixels'], component['seed'][1], component['seed'][0]))
    groups = [[component] for component in ordered[:31]]
    if ordered[31:]:
        groups.append(ordered[31:])
    masks = []
    for index, group in enumerate(groups):
        name = f'Component{index:02d}'
        host.call('select-' + name, 'split_part_set', {'name': name, 'steps': [
            {'op': 'component', 'point': component['seed']} for component in group]})
        w, h, channels, mask = image('prediction-' + name, ['read', 'resource://soul2d/part-split/masks/' + name])
        assert (w, h, channels) == (width, height, 1)
        assert sum(value > 127 for value in mask) == sum(component['pixels'] for component in group)
        masks.append(mask)
    return masks,components,groups


if __name__ == '__main__':
    main()
