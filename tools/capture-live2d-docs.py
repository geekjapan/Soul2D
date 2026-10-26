"""Capture approved public documentation, never SDK packages or executables."""
import hashlib
import json
from datetime import datetime, timezone
from html.parser import HTMLParser
from pathlib import Path
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
URLS = {
    "comparison": "https://www.live2d.com/cubism/comparison/",
    "editor-history": "https://docs.live2d.com/cubism-editor-manual/updates-history5-3/",
    "editor-releases": "https://docs.live2d.com/cubism-editor-manual/new-feature-introduction-title/",
    "editor-new53": "https://docs.live2d.com/cubism-editor-manual/new-function5-3/",
    "editor-manual": "https://docs.live2d.com/cubism-editor-manual/top/",
    "editor-notes53": "https://docs.live2d.com/cubism-editor-manual/cubism5-3-notes/",
    "sdk-new53": "https://docs.live2d.com/cubism-sdk-manual/cubism-5-3-new-functions/",
    "sdk-beta-compatibility": "https://docs.live2d.com/cubism-sdk-manual/compatibility-with-cubism-5-3/",
    "sdk-r5-compatibility": "https://docs.live2d.com/cubism-sdk-manual/compatibility-with-cubism-5-3-official/",
    "sdk-manual": "https://docs.live2d.com/cubism-sdk-manual/top/",
}


def official(url):
    if url not in URLS.values():
        raise ValueError(f"Outside approved documentation URLs: {url}")


class OfficialRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        official(newurl)
        return super().redirect_request(req, fp, code, msg, headers, newurl)


class Text(HTMLParser):
    def __init__(self):
        super().__init__()
        self.parts = []
        self.hidden = None

    def handle_starttag(self, tag, attrs):
        if tag in ("script", "style"):
            self.hidden = tag
        if not self.hidden and tag in ("h1", "h2", "h3", "h4", "h5", "p", "li", "tr", "br", "div"):
            self.parts.append("\n")

    def handle_endtag(self, tag):
        if tag == self.hidden:
            self.hidden = None
        if not self.hidden and tag in ("h1", "h2", "h3", "h4", "h5", "p", "li", "tr", "div"):
            self.parts.append("\n")

    def handle_data(self, data):
        if not self.hidden:
            self.parts.append(data)


def main():
    parent = ROOT / ".audit"
    parent.mkdir(exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="cubism53-official-", dir=parent))
    opener = urllib.request.build_opener(OfficialRedirect())
    receipt = {"retrievedAt": datetime.now(timezone.utc).isoformat(), "sources": [],
               "scope": "Public HTML documentation only; no SDK, installation, or vendor execution",
               "tool": {"path": str(Path(__file__).resolve()),
                        "sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest()},
               "policy": "exact-url-html-only-v1"}
    for name, url in URLS.items():
        official(url)
        with opener.open(url, timeout=45) as response:
            official(response.url)
            if response.headers.get("Content-Type", "").split(";", 1)[0].strip().lower() != "text/html":
                raise ValueError("Only HTML documentation is authorized")
            chunks = []
            size = 0
            while size <= 8_000_000:
                chunk = response.read(8_000_001 - size)
                if not chunk:
                    break
                chunks.append(chunk)
                size += len(chunk)
                if size > 8_000_000:
                    raise ValueError("Documentation response exceeds capture limit")
            raw = b"".join(chunks)
            final_url, status = response.url, response.status
        (directory / f"{name}.html").write_bytes(raw)
        parser = Text()
        parser.feed(raw.decode("utf-8"))
        text = "\n".join(line.strip() for line in "".join(parser.parts).splitlines() if line.strip())
        (directory / f"{name}.txt").write_text(text + "\n")
        receipt["sources"].append({"name": name, "url": url, "finalUrl": final_url, "status": status,
                                   "sha256": hashlib.sha256(raw).hexdigest()})
    (directory / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(directory)


if __name__ == "__main__":
    main()
