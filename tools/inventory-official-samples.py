#!/usr/bin/env python3
"""Inventory official archive routes and local ZIP contents without downloading or extracting."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import zipfile
import zlib


def catalog(script):
    base = re.search(r'let FILE_BASE_URL = "([^"]+)"', script)
    body = re.search(r'const fileDataArray = \{(.*?)\n    \}', script, re.S)
    if not base or not body or base[1] != "https://cubism.live2d.com/sample-data/bin/":
        raise ValueError("unrecognized official download manifest")
    entries = []
    remainder = re.sub(r'//[^\n]*', '', body[1])
    pattern = r'\s*(\w+):\s*\{\s*models:\s*\[(.*?)\]\s*\},?'
    for match in re.finditer(pattern, remainder, re.S):
        sample, models = match.groups()
        routes = re.findall(r'\{\s*lang:\s*"([^"]+)",\s*url:\s*"([^"]+)"\s*\},?', models)
        if not routes:
            raise ValueError(f"no routes for {sample}")
        leftover = re.sub(r'\{\s*lang:\s*"([^"]+)",\s*url:\s*"([^"]+)"\s*\},?', '', models)
        if leftover.strip():
            raise ValueError(f"unparsed routes for {sample}")
        for locale, route in routes:
            if locale not in {"ja", "en", "zh-Hans", "ko"} or route != f"{sample}/{sample}_{locale}.zip":
                raise ValueError(f"unexpected route {route}")
            entries.append({"sample": sample, "locale": locale, "route": route,
                            "url": base[1] + route})
    if re.sub(pattern, '', remainder, flags=re.S).strip() or not entries:
        raise ValueError("unparsed manifest content")
    if len({row['route'] for row in entries}) != len(entries):
        raise ValueError("duplicate route")
    return entries


def digest(stream):
    result = hashlib.sha256()
    for chunk in iter(lambda: stream.read(1024 * 1024), b''):
        result.update(chunk)
    return result.hexdigest()


def archive_contents(path):
    with path.open('rb') as stream:
        archive_hash = digest(stream)
    files, names = [], set()
    with zipfile.ZipFile(path) as archive:
        for member in archive.infolist():
            name = member.filename
            if name in names:
                raise ValueError(f"duplicate ZIP member: {name}")
            names.add(name)
            if '\\' in name or PurePosixPath(name).is_absolute() or '..' in PurePosixPath(name).parts:
                raise ValueError(f"unsafe ZIP member: {name}")
            if member.is_dir():
                continue
            with archive.open(member) as stream:
                checksum = digest(stream)
            files.append({"path": name, "bytes": member.file_size, "sha256": checksum,
                          "extension": PurePosixPath(name).suffix.lower(),
                          "verification": "not_run"})
    return {"sha256": archive_hash, "files": files, "status": "inventoried_not_tested"}


def inventory(script_path, archive_root):
    script_bytes = script_path.read_bytes()
    rows = catalog(script_bytes.decode('utf-8'))
    for row in rows:
        path = archive_root / row['route']
        row['localPath'] = str(path.resolve())
        row['status'] = 'missing'
        if path.is_file():
            try:
                row.update(archive_contents(path))
            except (OSError, ValueError, RuntimeError, zipfile.BadZipFile, zlib.error) as error:
                row.update(status='invalid_archive', error=str(error))
    return {"format": "vsoul.official-sample-inventory.v1",
            "catalogSource": "https://cubism.live2d.com/sample-data/js/download.js",
            "catalogSha256": hashlib.sha256(script_bytes).hexdigest(),
            "sampleCount": len({row['sample'] for row in rows}), "archiveCount": len(rows),
            "archives": rows}


def self_test():
    fixture = '''let FILE_BASE_URL = "https://cubism.live2d.com/sample-data/bin/"
    const fileDataArray = {
        test: { models: [{ lang: "ja", url: "test/test_ja.zip" },] },
    }'''
    assert catalog(fixture)[0]['route'] == 'test/test_ja.zip'
    for broken in (fixture.replace('test_ja.zip', '../bad.zip'),
                   fixture.replace('models:', 'unknown:'),
                   fixture.replace('lang: "ja"', 'lang: "xx"')):
        try:
            catalog(broken)
        except ValueError:
            pass
        else:
            raise AssertionError('malformed manifest accepted')
    import tempfile
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        script = root / 'download.js'
        script.write_text(fixture)
        assert inventory(script, root)['archives'][0]['status'] == 'missing'
        path = root / 'test/test_ja.zip'
        path.parent.mkdir()
        with zipfile.ZipFile(path, 'w') as archive:
            archive.writestr('model/test.cmo3', b'model')
            archive.writestr('model/test.can3', b'animation')
        row = inventory(script, root)['archives'][0]
        assert row['status'] == 'inventoried_not_tested'
        assert len(row['files']) == 2
        assert row['files'][0]['sha256'] == hashlib.sha256(b'model').hexdigest()
        original = path.read_bytes()
        path.write_bytes(original.replace(b'model', b'broken', 1))
        assert inventory(script, root)['archives'][0]['status'] == 'invalid_archive'
        with zipfile.ZipFile(path, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
            archive.writestr('test.cmo3', b'model' * 100)
        compressed = bytearray(path.read_bytes())
        compressed[30 + len('test.cmo3')] = 7
        path.write_bytes(compressed)
        assert inventory(script, root)['archives'][0]['status'] == 'invalid_archive'
        with zipfile.ZipFile(path, 'w') as archive:
            archive.writestr('../outside.cmo3', b'model')
        assert inventory(script, root)['archives'][0]['status'] == 'invalid_archive'
    print('official sample inventory self-test PASS')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--self-test', action='store_true')
    parser.add_argument('--script', type=Path)
    parser.add_argument('--archives', type=Path)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if not all((args.script, args.archives, args.output)):
        parser.error('--script, --archives and --output are required')
    result = inventory(args.script, args.archives)
    args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n')
    print(f"{result['sampleCount']} samples, {result['archiveCount']} archive routes")
    print(args.output)


if __name__ == '__main__':
    main()
