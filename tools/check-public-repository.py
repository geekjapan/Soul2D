#!/usr/bin/env python3
"""Read-only publication hygiene scan; never prints matched credential values."""
import argparse
import json
from pathlib import Path
import re
import subprocess
import sys


SECRET_PATTERNS = {
    'private_key': rb'-----BEGIN (?:RSA |EC |OPENSSH |DSA |ENCRYPTED )?PRIVATE KEY-----',
    'github_token': rb'\b(?:gh[pousr]_[A-Za-z0-9]{36,255}|github_pat_[A-Za-z0-9_]{60,255})\b',
    'aws_access_key': rb'\b(?:AKIA|ASIA)[A-Z0-9]{16}\b',
    'slack_token': rb'\bxox[baprs]-[A-Za-z0-9-]{20,255}\b',
    'openai_key': rb'\bsk-(?:proj-|svcacct-)?[A-Za-z0-9_-]{40,255}\b',
}
PATTERNS = {name: re.compile(pattern) for name, pattern in SECRET_PATTERNS.items()}
LOCAL_PATH = re.compile(rb'(?:/Users/[^/\s]+/|/home/[^/\s]+/|[A-Z]:/Users/[^/\s]+/)')
RESTRICTED = {'.cmo3', '.can3', '.moc3', '.inp', '.inx', '.psd', '.zip',
              '.sqlite', '.db', '.pem', '.key', '.p12', '.pfx'}


def findings(data):
    return [{'kind': name, 'line': data.count(b'\n', 0, match.start()) + 1}
            for name, pattern in PATTERNS.items() for match in pattern.finditer(data)]


def risky_path(path):
    name = Path(path).name.lower()
    return (Path(path).suffix.lower() in RESTRICTED or
            name == '.env' or name.startswith('.env.') and name != '.env.example')


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args])


def scan(root, history):
    result = {'scope': 'tracked and untracked nonignored worktree files', 'worktree_files': 0,
              'credential_candidates': [], 'restricted_files': [], 'local_path_files': [],
              'history': None,
              'limits': ['Pattern matching is not proof that secrets are absent.',
                         'Issues, PR comments, releases, artifacts, and remote-only refs are outside this scan.',
                         'License ownership and dependency vulnerability review require separate checks.']}
    for raw_path in git(root, 'ls-files', '--cached', '--others', '--exclude-standard', '-z').split(b'\0'):
        if not raw_path:
            continue
        path = raw_path.decode('utf-8', errors='surrogateescape')
        file = root / path
        if not file.is_file() or file.is_symlink():
            continue
        result['worktree_files'] += 1
        if risky_path(path):
            result['restricted_files'].append(path)
        data = file.read_bytes()
        if b'\0' in data:
            continue
        result['credential_candidates'].extend({'path': path, **item} for item in findings(data))
        if LOCAL_PATH.search(data):
            result['local_path_files'].append(path)
    if history:
        result['history'] = scan_history(root)
    return result


def scan_history(root):
    result = {'scope': 'unique blobs reachable from all locally available Git refs',
              'blobs': 0, 'binary_blobs': 0, 'credential_candidates': [], 'restricted_paths': []}
    objects = {}
    for record in git(root, 'rev-list', '--objects', '--all').splitlines():
        oid, _, path = record.partition(b' ')
        objects[oid] = path.decode('utf-8', errors='surrogateescape')
    process = subprocess.Popen(['git', '-C', str(root), 'cat-file', '--batch'],
                               stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    try:
        for oid, path in objects.items():
            process.stdin.write(oid + b'\n')
            process.stdin.flush()
            header = process.stdout.readline().split()
            if len(header) != 3:
                raise RuntimeError('Unexpected git cat-file response')
            size = int(header[2])
            data = process.stdout.read(size)
            if len(data) != size or process.stdout.read(1) != b'\n':
                raise RuntimeError('Truncated git object')
            if header[1] != b'blob':
                continue
            result['blobs'] += 1
            if path and risky_path(path):
                result['restricted_paths'].append({'blob': oid.decode(), 'path': path})
            if b'\0' in data:
                result['binary_blobs'] += 1
                continue
            result['credential_candidates'].extend(
                {'blob': oid.decode(), 'path': path, **item} for item in findings(data))
    finally:
        process.stdin.close()
        process.stdout.close()
        status = process.wait()
    if status:
        raise RuntimeError('git cat-file failed')
    return result


def self_test():
    token = b'gh' + b'p_' + b'A' * 36
    assert findings(b'example\n' + token) == [{'kind': 'github_token', 'line': 2}]
    assert token.decode() not in json.dumps(findings(token))
    assert findings(b'api_key = YOUR_KEY\npassword = example') == []
    assert risky_path('samples/model.cmo3') and risky_path('.env.local')
    assert not risky_path('.env.example') and not risky_path('source/tokenizer.d')
    assert LOCAL_PATH.search(b'/Users/example/project')
    print('PASS publication scan self-test')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--history', action='store_true', help='Scan all locally reachable Git blobs too')
    parser.add_argument('--output', type=Path, help='Write JSON instead of printing it')
    parser.add_argument('--self-test', action='store_true')
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return 0
    root = Path(__file__).resolve().parents[1]
    result = scan(root, args.history)
    output = json.dumps(result, indent=2, ensure_ascii=True) + '\n'
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(output, encoding='utf-8')
        print('Publication scan report written')
    else:
        print(output, end='')
    history = result['history'] or {}
    return int(bool(result['credential_candidates'] or result['restricted_files'] or
                    history.get('credential_candidates') or history.get('restricted_paths')))


if __name__ == '__main__':
    sys.exit(main())
