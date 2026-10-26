#!/usr/bin/env python3
"""Fail when a packaging file disagrees with the version in rust/Cargo.toml."""
from pathlib import Path
import plistlib
import re
import sys
import tomllib

root = Path(__file__).resolve().parents[1]
version = tomllib.loads((root / 'rust/Cargo.toml').read_text())['package']['version']
num = ','.join((version.split('-')[0].split('+')[0].split('.') + ['0'])[:4])  # 0.9.1 -> 0,9,1,0
errors = []


def expect(label, found, wanted=version):
    if found != wanted:
        errors.append(f'{label}: {found!r}, expected {wanted!r}')


lock = tomllib.loads((root / 'rust/Cargo.lock').read_text())
expect('rust/Cargo.lock soul2d-core',
       next(p['version'] for p in lock['package'] if p['name'] == 'soul2d-core'))

info = plistlib.loads((root / 'build-aux/osx/Info.plist').read_bytes())
for key in ('CFBundleShortVersionString', 'CFBundleVersion'):
    expect(f'Info.plist {key}', info.get(key))

wxs = (root / 'build-aux/windows/wix/Product.wxs').read_text()
m = re.search(r'<Product\b[^>]*\bVersion="([^"]*)"', wxs)
expect('Product.wxs Product/@Version', m and m.group(1))

header = (root / 'build-aux/windows/version.h').read_text()
for name in ('FILEVERSION', 'PRODUCTVERSION'):
    m = re.search(rf'#define VER_{name}\s+(\S+)', header)
    expect(f'version.h VER_{name}', m and m.group(1), num)
    m = re.search(rf'#define VER_{name}_STR\s+"([^"]*)"', header)
    expect(f'version.h VER_{name}_STR', m and m.group(1), version + '\\0')

if errors:
    sys.exit('FAIL version mismatch (source of truth: rust/Cargo.toml):\n  ' + '\n  '.join(errors))
print(f'PASS version {version} agrees across Cargo and packaging files')
