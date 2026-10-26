#!/usr/bin/env python3
"""Check Soul2D package names and optionally locally built macOS artifacts."""
import argparse
from pathlib import Path
import plistlib
import tomllib


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--built', action='store_true', help='also check the macOS build output')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    cargo = tomllib.loads((root / 'rust/Cargo.toml').read_text())
    assert cargo['package']['name'] == 'soul2d-core'
    assert cargo['lib']['name'] == 'soul2d_core'
    assert [target['name'] for target in cargo['bin']] == ['soul2d-core', 'njc', 'soul2d-editor']
    # The D application, its dub recipe and the D engine tree are gone.
    for gone in ('dub.sdl', 'source', 'soul2d-live'):
        assert not (root / gone).exists(), gone
    info = plistlib.loads((root / 'build-aux/osx/Info.plist').read_bytes())
    assert info['CFBundleName'] == info['CFBundleDisplayName'] == 'Soul2D'
    assert info['CFBundleExecutable'] == 'soul2d'
    assert info['CFBundleIdentifier'] == 'io.github.geekjapan.soul2d'
    assert info.get('CFBundleIconFile') in (None, 'soul2d')
    # No stale module imports or resource URIs may remain in active sources.
    stale = []
    for base in ('rust/src', 'tools', 'res'):
        for p in (root / base).rglob('*'):
            if not p.is_file() or p.suffix not in ('.rs', '.py', '.json'):
                continue
            text = p.read_text(errors='ignore')
            if p.name in ('rename-project-names.py', 'prepare-soul2d-live.py',
                          'check-project-artifacts.py'):
                continue
            for needle in ('import nijigenerate', 'import nijilive',
                           'module nijigenerate', 'module nijilive',
                           'resource://nijigenerate'):
                if needle in text:
                    stale.append(f'{p}: {needle}')
    assert not stale, '\n'.join(stale)
    if args.built:
        bins = ('soul2d-core', 'njc', 'soul2d-editor')
        profiles = [p for p in ('debug', 'release')
                    if all((root / 'out/rust' / p / b).is_file() for b in bins)]
        # BuildUniversal.sh stages the lipo-joined binaries in out/macos-bin.
        staged = all((root / 'out/macos-bin' / b).is_file() for b in bins)
        assert profiles or staged, 'no complete out/rust/{debug,release} or out/macos-bin build of ' + ', '.join(bins)
        app = root / 'out/Soul2D.app/Contents'
        if app.parent.exists():
            exe = app / 'MacOS' / info['CFBundleExecutable']
            assert exe.is_file() and exe.stat().st_size > 0, exe
            bundled = plistlib.loads((app / 'Info.plist').read_bytes())
            assert bundled['CFBundleExecutable'] == info['CFBundleExecutable'], 'bundle Info.plist differs'
            icns = app / 'Resources' / f"{info['CFBundleIconFile']}.icns"
            assert icns.is_file() and icns.stat().st_size > 0, icns
    print('PASS Soul2D package and artifact names')


if __name__ == '__main__':
    main()
