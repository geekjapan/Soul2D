import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name('d-to-rust-inventory.py')
SPEC = importlib.util.spec_from_file_location('inventory', SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class InventoryTests(unittest.TestCase):
    def test_walk_errors_are_not_silently_counted_as_complete(self):
        def denied(root, followlinks=False, onerror=None):
            onerror(PermissionError('source directory is not readable'))
            return iter(())

        with tempfile.TemporaryDirectory() as directory:
            with patch.object(MODULE.os, 'walk', side_effect=denied):
                with self.assertRaisesRegex(PermissionError, 'not readable'):
                    MODULE.inventory(Path(directory))

    def test_nested_sources_are_complete_sorted_and_hashed_without_parity_claims(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'source/soul2d/commands/parameter/animedit.d'
            source.parent.mkdir(parents=True)
            source.write_bytes(b'module example;\n')
            extra = root / 'vendor/extra.d'
            extra.parent.mkdir()
            extra.write_bytes(b'module extra;\n\n')
            for name in MODULE.EXCLUDED:
                omitted = root / name
                omitted.mkdir()
                (omitted / 'generated.d').write_bytes(b'excluded')
            link = root / 'linked.d'
            link.symlink_to(extra)
            (root / 'linked-directory').symlink_to(extra.parent, target_is_directory=True)
            report = MODULE.inventory(root)
            self.assertEqual(report['dFileCount'], 2)
            self.assertEqual(report['groups'], {'source/soul2d/commands': 1, 'vendor': 1})
            self.assertFalse(report['localEnginePresent'])
            self.assertEqual([row['source'] for row in report['files']],
                             ['source/soul2d/commands/parameter/animedit.d', 'vendor/extra.d'])
            first, second = report['files']
            self.assertEqual(first['sha256'], hashlib.sha256(b'module example;\n').hexdigest())
            self.assertEqual(first['lines'], 1)
            self.assertEqual(second['lines'], 2)
            self.assertEqual(first['candidate']['missingFiles'],
                             MODULE.CANDIDATES[first['source']]['rust']
                             + MODULE.CANDIDATES[first['source']]['checks'])
            self.assertIsNone(second['candidate'])
            self.assertEqual([row['parity'] for row in report['files']], ['unverified', 'unverified'])
            self.assertEqual(MODULE.inventory(root), report)

    def test_removal_order_lists_unimported_files_first_with_their_importers(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            files = {
                'source/app.d': 'import soul2d.core, std.stdio;\n// import soul2d.gone;\n',
                'source/soul2d/core/package.d': 'module soul2d.core;\npublic import soul2d.core.json;\n',
                'source/soul2d/core/json.d': 'module soul2d.core.json;\nimport parse = std.json : JSONValue;\n',
                'source/soul2d/gone.d': 'module soul2d.gone;\n',
            }
            for name, text in files.items():
                (root / name).parent.mkdir(parents=True, exist_ok=True)
                (root / name).write_text(text)
            rows = MODULE.removal_order(root)
            self.assertEqual(
                [(row['source'], row['importers']) for row in rows],
                [('source/app.d', []),
                 ('source/soul2d/gone.d', []),
                 ('source/soul2d/core/json.d', ['source/soul2d/core/package.d']),
                 ('source/soul2d/core/package.d', ['source/app.d'])])

    def test_cli_does_not_overwrite_existing_reports(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'main.d'
            source.write_bytes(b'module main;')
            output = root / 'inventory.json'
            command = [sys.executable, str(SCRIPT), '--root', str(root), '--output', str(output)]
            first = subprocess.run(command, capture_output=True, text=True, timeout=10)
            self.assertEqual(first.returncode, 0, first.stderr)
            data = output.read_bytes()
            self.assertEqual(json.loads(data)['dFileCount'], 1)
            second = subprocess.run(command, capture_output=True, text=True, timeout=10)
            self.assertNotEqual(second.returncode, 0)
            self.assertEqual(output.read_bytes(), data)


if __name__ == '__main__':
    unittest.main()
