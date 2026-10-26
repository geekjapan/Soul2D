import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import sys


ROOT = Path(__file__).resolve().parents[1]
EXCLUDED = {'.git', '.dub', '.orca', 'out', 'build', 'target', '__pycache__', 'node_modules'}
CANDIDATES = {
    'source/soul2d/commands/parameter/animedit.d': {
        'rust': ['rust/src/animation.rs', 'rust/src/lib.rs', 'rust/src/mcp.rs', 'rust/src/editor_ui.rs'],
        'checks': ['rust/tests/animation_clipboard.rs', 'rust/tests/animation_key_add.rs',
                   'tools/verify-animation-clipboard.py', 'tools/verify-animation-key-edit.py',
                   'tools/verify-animation-key-gui.py'],
        'scope': 'Parameter-axis Add/copy/paste and native controls only; timeline and all legacy aliases remain unverified.',
    },
}
for source in [
    'source/soul2d/core/cv/distancetransform.d',
    'source/soul2d/core/cv/contours.d',
    'source/soul2d/core/math/skeletonize.d',
    'source/soul2d/core/math/path.d',
]:
    CANDIDATES[source] = {
        'rust': ['rust/src/cv.rs'],
        'checks': ['rust/tests/cv.rs', 'tools/verify-cv-mask.py'],
        'scope': 'Owned mask primitives only; automesh, texture providers and GUI integration remain unverified.',
    }
for source in ['source/soul2d/core/cv/image.d', 'source/soul2d/core/colorbleed.d']:
    CANDIDATES[source] = {
        'rust': ['rust/src/pixels.rs'],
        'checks': ['rust/tests/pixels.rs', 'tools/verify-pixel-image.py'],
        'scope': '8-bit storage and owned RGBA bleed only; generic casts and canonical texture editing remain unverified.',
    }
for source in ['source/soul2d/core/actionstack.d']:
    CANDIDATES[source] = {
        'rust': ['rust/src/lib.rs', 'rust/src/editor_ui.rs', 'rust/src/mcp.rs'],
        'checks': ['rust/tests/history_navigation.rs', 'tools/verify-history-navigation.py',
                   'tools/verify-history-navigation-gui.py'],
        'scope': 'Retained-state read/navigation only; clear, descriptions, scopes and full history parity remain unverified.',
    }


def fail_scan(error):
    raise error


def scan(root):
    paths = []
    for directory, children, names in os.walk(root, followlinks=False, onerror=fail_scan):
        children[:] = sorted(
            name for name in children
            if name not in EXCLUDED and not (Path(directory) / name).is_symlink()
        )
        for name in sorted(names):
            path = Path(directory) / name
            if path.suffix == '.d' and not path.is_symlink():
                paths.append(path)
    return sorted(paths)


IMPORT_ROOTS = ('source',)
IMPORT = re.compile(r'\bimport\s+([^;]+);')
COMMENT = re.compile(r'//[^\n]*|/\*.*?\*/|/\+.*?\+/', re.S)


def module_name(relative):
    for prefix in IMPORT_ROOTS:
        if relative.startswith(prefix + '/'):
            parts = relative[len(prefix) + 1:-2].split('/')
            return '.'.join(parts[:-1] if parts[-1] == 'package' else parts)
    return None


def import_graph(root):
    """Map each local D file to the local D files it imports, by module name only."""
    sources = {path.relative_to(root).as_posix(): path for path in scan(root)}
    modules = {module_name(name): name for name in sources if module_name(name)}
    graph = {}
    for name, path in sources.items():
        text = COMMENT.sub('', path.read_text(encoding='utf-8', errors='replace'))
        found = set()
        for clause in IMPORT.findall(text):
            # "import a.b, c = d.e : f, g;" -> a.b and d.e; names after ':' are symbols.
            for item in clause.split(':')[0].split(','):
                target = modules.get(item.split('=')[-1].strip())
                if target and target != name:
                    found.add(target)
        graph[name] = sorted(found)
    return graph


def removal_order(root):
    """List D files by how many local D files still import them; zero is removable first."""
    graph = import_graph(root)
    importers = {name: [] for name in graph}
    for name, targets in graph.items():
        for target in targets:
            importers[target].append(name)
    return [
        {'source': name, 'importers': sorted(importers[name]), 'imports': graph[name],
         'candidate': name in CANDIDATES}
        for name in sorted(graph, key=lambda name: (len(importers[name]), name))
    ]


def inventory(root):
    rows = []
    groups = {}
    for path in scan(root):
        relative = path.relative_to(root).as_posix()
        data = path.read_bytes()
        parts = path.relative_to(root).parts
        group = '/'.join(parts[:3]) if parts[0] == 'source' and len(parts) > 3 else parts[0]
        groups[group] = groups.get(group, 0) + 1
        candidate = CANDIDATES.get(relative)
        rows.append({
            'source': relative,
            'sha256': hashlib.sha256(data).hexdigest(),
            'lines': len(data.splitlines()),
            'group': group,
            'parity': 'unverified',
            'candidate': None if candidate is None else {
                **candidate,
                'missingFiles': [name for name in candidate['rust'] + candidate['checks']
                                 if not (root / name).is_file()],
            },
        })
    return {
        'schemaVersion': 1,
        'scope': 'Local D files outside excluded directories only; no external dependency or runtime parity claim.',
        'excludedDirectoryNames': sorted(EXCLUDED),
        'followSymlinks': False,
        'localEnginePresent': (root / 'soul2d-live' / 'dub.sdl').is_file()
            or (root / 'soul2d-live' / 'dub.json').is_file(),
        'dFileCount': len(rows),
        'groups': dict(sorted(groups.items())),
        'files': rows,
    }


def main():
    parser = argparse.ArgumentParser(
        description='Enumerate every local D source without inferring Rust parity from file names.'
    )
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--removal-order', action='store_true',
                        help='Print each D file with its remaining local D importers instead of the inventory.')
    args = parser.parse_args()
    root = args.root.resolve()
    if not root.is_dir():
        parser.error(f'root is not a directory: {root}')
    if args.removal_order:
        rows = removal_order(root)
        json.dump(rows, sys.stdout, ensure_ascii=False, indent=2)
        sys.stdout.write('\n')
        return
    report = inventory(root)
    text = json.dumps(report, ensure_ascii=False, indent=2) + '\n'
    if args.output is None:
        sys.stdout.write(text)
    else:
        with args.output.open('x', encoding='utf-8') as output:
            output.write(text)
        print(f'{report["dFileCount"]} D files inventoried in {args.output}')


if __name__ == '__main__':
    main()
