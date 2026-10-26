"""Offline behavior checks for the public-documentation capture helper."""
import hashlib
import importlib.util
import io
import json
import sys
import tempfile
import unittest
import urllib.request
from contextlib import redirect_stdout
from datetime import datetime
from pathlib import Path
from unittest import mock

sys.dont_write_bytecode = True

MODULE_PATH = Path(__file__).resolve().parent / "capture-live2d-docs.py"
APPROVED = "https://www.live2d.com/cubism/comparison/"
RAW = b"<h1>Title</h1><script>hidden</script><p>Visible&amp;data</p>"
EXPECTED_TEXT = "Title\nVisible&data\n"
EXPECTED_SCOPE = "Public HTML documentation only; no SDK, installation, or vendor execution"


def load_helper():
    spec = importlib.util.spec_from_file_location("capture_live2d_docs", MODULE_PATH)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class FakeResponse:
    def __init__(self, url, content_type="text/html", read_error=None):
        self.url = url
        self.status = 200
        self.headers = {"Content-Type": content_type}
        self._read_error = read_error
        self._stream = io.BytesIO(RAW)

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False

    def read(self, limit=-1):
        if self._read_error is not None:
            raise self._read_error
        return self._stream.read(limit)


class FakeOpener:
    def __init__(self, content_type="text/html", read_error=None, fail_urls=None):
        self._content_type = content_type
        self._read_error = read_error
        self._fail_urls = fail_urls or {}

    def open(self, url, timeout=None):
        if url in self._fail_urls:
            raise self._fail_urls[url]
        return FakeResponse(url, self._content_type, self._read_error)


class CaptureLive2dDocsTest(unittest.TestCase):
    def setUp(self):
        self.helper = load_helper()

    def patched_helper(self, root, content_type="text/html", read_error=None):
        return (
            mock.patch.object(self.helper, "ROOT", Path(root)),
            mock.patch.object(self.helper, "URLS", {"comparison": APPROVED}),
            mock.patch.object(
                urllib.request, "build_opener",
                lambda *a, **k: FakeOpener(content_type, read_error)),
        )

    def run_main(self):
        out = io.StringIO()
        with redirect_stdout(out):
            self.helper.main()
        return Path(out.getvalue().strip())

    def assert_snapshot(self, captured):
        self.assertEqual((captured / "comparison.html").read_bytes(), RAW)
        self.assertEqual((captured / "comparison.txt").read_text(), EXPECTED_TEXT)
        receipt = json.loads((captured / "receipt.json").read_text())
        self.assertEqual(receipt["scope"], EXPECTED_SCOPE)
        self.assertEqual(receipt["policy"], "exact-url-html-only-v1")
        self.assertEqual(receipt["tool"], {
            "path": str(MODULE_PATH.resolve()),
            "sha256": hashlib.sha256(MODULE_PATH.read_bytes()).hexdigest(),
        })
        datetime.fromisoformat(receipt["retrievedAt"])
        self.assertEqual(receipt["sources"], [{
            "name": "comparison",
            "url": APPROVED,
            "finalUrl": APPROVED,
            "status": 200,
            "sha256": hashlib.sha256(RAW).hexdigest(),
        }])

    def test_capture_writes_snapshot_and_never_overwrites(self):
        with tempfile.TemporaryDirectory() as tmp:
            root_patch, urls_patch, opener_patch = self.patched_helper(tmp)
            with root_patch, urls_patch, opener_patch:
                first = self.run_main()
                self.assertEqual(first.parent, Path(tmp) / ".audit")
                self.assertTrue(first.is_dir())
                self.assert_snapshot(first)
                first_html = (first / "comparison.html").read_bytes()

                second = self.run_main()
                self.assertNotEqual(second, first)
                self.assertEqual(second.parent, Path(tmp) / ".audit")
                self.assert_snapshot(second)
                self.assertEqual((first / "comparison.html").read_bytes(), first_html)
                self.assertEqual(len(list((Path(tmp) / ".audit").iterdir())), 2)

    def test_short_reads_reassemble_identical_snapshot(self):
        original_read = FakeResponse.read

        def short_read(self, limit=-1):
            return original_read(self, min(limit, 3))

        with tempfile.TemporaryDirectory() as tmp:
            root_patch, urls_patch, opener_patch = self.patched_helper(tmp)
            with root_patch, urls_patch, opener_patch, \
                    mock.patch.object(FakeResponse, "read", short_read):
                self.assert_snapshot(self.run_main())

    def test_over_limit_payload_rejected_without_artifacts(self):
        with tempfile.TemporaryDirectory() as tmp:
            root_patch, urls_patch, opener_patch = self.patched_helper(tmp)
            with root_patch, urls_patch, opener_patch, \
                    mock.patch.dict(globals(), {"RAW": b"x" * 8_000_001}):
                audit_dir = Path(tmp) / ".audit"
                before = set(audit_dir.iterdir()) if audit_dir.exists() else set()
                with self.assertRaises(ValueError) as ctx:
                    self.helper.main()
                self.assertIn("Documentation response exceeds capture limit",
                              str(ctx.exception))
                for new_dir in set(audit_dir.iterdir()) - before:
                    self.assertFalse((new_dir / "comparison.html").exists())
                    self.assertFalse((new_dir / "receipt.json").exists())

    def test_second_source_failure_keeps_first_capture_without_receipt(self):
        sdk_url = "https://docs.live2d.com/cubism-sdk-manual/top/"
        urls = {"comparison": APPROVED, "sdk-manual": sdk_url}
        with tempfile.TemporaryDirectory() as tmp:
            with mock.patch.object(self.helper, "ROOT", Path(tmp)), \
                    mock.patch.object(self.helper, "URLS", urls), \
                    mock.patch.object(
                        urllib.request, "build_opener",
                        lambda *a, **k: FakeOpener(
                            fail_urls={sdk_url: OSError("simulated read failure")})):
                audit_dir = Path(tmp) / ".audit"
                with self.assertRaises(OSError) as ctx:
                    self.helper.main()
                self.assertIn("simulated read failure", str(ctx.exception))
                evidence_dirs = list(audit_dir.iterdir())
                self.assertEqual(len(evidence_dirs), 1)
                evidence = evidence_dirs[0]
                self.assertEqual((evidence / "comparison.html").read_bytes(), RAW)
                self.assertFalse((evidence / "receipt.json").exists())
                self.assertFalse((evidence / "sdk-manual.html").exists())

    def test_nested_markup_inside_script_stays_hidden(self):
        parser = self.helper.Text()
        parser.feed("<script>before<style>inside</style>after</script><p>Visible</p>")
        text = "\n".join(line.strip() for line in "".join(parser.parts).splitlines()
                         if line.strip())
        self.assertEqual(text, "Visible")

    def test_only_approved_documentation_urls_allowed(self):
        helper = self.helper
        with mock.patch.object(helper, "URLS", {"comparison": APPROVED}):
            helper.official(APPROVED)
            for bad in (
                "http://docs.live2d.com/x",
                "https://example.com/x",
                "https://www.live2d.com/cubism/download/sdk/",
                "https://docs.live2d.com/unapproved.bin",
            ):
                with self.assertRaises(ValueError) as ctx:
                    helper.official(bad)
                self.assertIn("Outside approved documentation URLs", str(ctx.exception))
            with self.assertRaises(ValueError) as ctx:
                helper.OfficialRedirect().redirect_request(
                    None, None, 302, "Found", {},
                    "https://www.live2d.com/cubism/download/sdk/")
            self.assertIn("Outside approved documentation URLs", str(ctx.exception))

    def test_binary_payload_refused_before_body_read(self):
        with tempfile.TemporaryDirectory() as tmp:
            root_patch, urls_patch, opener_patch = self.patched_helper(
                tmp, content_type="application/octet-stream",
                read_error=AssertionError("binary payload body must not be read"))
            with root_patch, urls_patch, opener_patch:
                audit_dir = Path(tmp) / ".audit"
                before = set(audit_dir.iterdir()) if audit_dir.exists() else set()
                with self.assertRaises(ValueError) as ctx:
                    self.helper.main()
                self.assertIn("Only HTML documentation is authorized",
                              str(ctx.exception))
                for new_dir in set(audit_dir.iterdir()) - before:
                    self.assertFalse((new_dir / "comparison.html").exists())
                    self.assertFalse((new_dir / "receipt.json").exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
