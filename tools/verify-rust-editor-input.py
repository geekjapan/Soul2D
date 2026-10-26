#!/usr/bin/env python3
"""Drive one isolated Rust editor fixture window and check its saved artifact."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pid", type=int, required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    args = parser.parse_args()
    directory = args.evidence.resolve()
    source, saved = directory / "input.inx", directory / "gui-saved.inx"
    original = source.read_bytes()
    pin = json.loads((directory / "receipt.json").read_text())
    assert hashlib.sha256(original).hexdigest() == pin["sha256"]
    binary = Path(__file__).resolve().parents[1] / "out/rust/debug/soul2d-editor"
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == pin["binarySha256"]
    assert not saved.exists(), "Use a fresh isolated fixture directory"
    session = f"soul2d-editor-input-{args.pid}"
    result = {"status": "FAIL", "scope": "Native INX GUI input only, not full GUI/vendor acceptance",
              "pid": args.pid, "checks": {}, "limits": [
                  "No native CMO3/CAN3 display or playback", "No ACP or vendor proof"]}
    log = (directory / "driver-events.jsonl").open("x")

    def call(tool, payload):
        process = subprocess.run(
            ["cua-driver", "call", tool, json.dumps(payload)],
            capture_output=True, text=True, timeout=45)
        log.write(json.dumps({"tool": tool, "input": payload, "exitCode": process.returncode,
                              "stdout": process.stdout, "stderr": process.stderr}) + "\n")
        log.flush()
        assert process.returncode == 0, (tool, process.stderr, process.stdout)
        body = json.loads(process.stdout)
        assert not body.get("isError", False), body
        return body

    def items(state, role, text):
        return [e for e in state["elements"] if e.get("role") == role
                and e.get("value", e.get("label")) == text]

    try:
        call("start_session", {"session": session})
        windows = call("list_windows", {"pid": args.pid})["windows"]
        matches = [w for w in windows if w["pid"] == args.pid
                   and w["title"].startswith("Soul2D") and w["is_on_screen"]]
        assert len(matches) == 1, matches
        window = matches[0]["window_id"]
        result["windowId"] = window
        target = {"kind": "window", "pid": args.pid, "window_id": window}

        def snapshot(name):
            body = call("get_window_state", {
                "pid": args.pid, "window_id": window, "session": session,
                "max_depth": 2, "max_elements": 60, "timeout_ms": 5000,
                "screenshot_out_file": str(directory / f"{name}.png"),
            })
            assert body["pid"] == args.pid and body["window_id"] == window
            assert body["window_title"].startswith("Soul2D")
            assert body.get("screenshot_frame_valid") is True
            (directory / f"{name}.json").write_text(json.dumps(body, indent=2) + "\n")
            return body

        def click(state, element):
            assert element.get("enabled", True)
            call("click", {"target": target, "element_token": element["element_token"],
                           "session": session})

        def button(state, label):
            found = items(state, "AXButton", label)
            assert len(found) == 1, (label, found)
            click(state, found[0])

        def replace(state, old, new, name):
            fields = items(state, "AXTextField", old)
            assert len(fields) == 1, (old, fields)
            call("hotkey", {"target": target, "element_token": fields[0]["element_token"],
                            "keys": ["cmd", "a"], "delivery_mode": "foreground",
                            "session": session})
            current = snapshot(name + "-selected")
            fields = items(current, "AXTextField", old)
            assert len(fields) == 1
            call("type_text", {"target": target, "element_token": fields[0]["element_token"],
                              "text": new, "delivery_mode": "foreground", "session": session})
            current = snapshot(name)
            assert len(items(current, "AXTextField", new)) == 1
            return current

        state = snapshot("initial")
        assert len(items(state, "AXTextField", str(source))) == 1
        card = items(state, "AXStaticText", "Card")
        assert len(card) == 1
        edit = [e for e in items(state, "AXButton", "Edit")
                if e["element_index"] > card[0]["element_index"]]
        assert edit
        click(state, min(edit, key=lambda e: e["element_index"]))
        state = snapshot("selected")
        assert items(state, "AXStaticText", 'Rename node 2 (was "Card")')
        state = replace(state, "Card", "GuiVerified", "draft")
        assert items(state, "AXStaticText", "Card")
        assert items(state, "AXStaticText", "draft unsaved")
        result["checks"]["draftTypedWithoutApplying"] = True
        button(state, "Save As")
        state = snapshot("save-refused")
        assert items(state, "AXStaticText", "Apply or discard the draft before file operations")
        assert items(state, "AXTextField", "GuiVerified")
        assert source.read_bytes() == original
        result["checks"]["draftSaveRefused"] = True
        button(state, "Apply")
        state = snapshot("applied")
        assert items(state, "AXStaticText", "GuiVerified")
        result["checks"]["applyReadback"] = True
        button(state, "Undo")
        state = snapshot("undone")
        assert items(state, "AXStaticText", "Card")
        button(state, "Redo")
        state = snapshot("redone")
        assert items(state, "AXStaticText", "GuiVerified")
        result["checks"]["undoRedoReadback"] = True
        state = replace(state, str(source), str(saved), "save-target")
        button(state, "Save As")
        state = snapshot("saved")
        assert saved.exists()

        def split(raw):
            assert raw[:8] == b"TRNSRTS\0"
            length = int.from_bytes(raw[8:12], "big")
            return json.loads(raw[12:12 + length]), raw[12 + length:]

        model, tail = split(original)
        edited, edited_tail = split(saved.read_bytes())
        model["nodes"]["children"][0]["name"] = "GuiVerified"
        assert edited == model and edited_tail == tail
        assert source.read_bytes() == original
        result["checks"]["savedOnlyRequestedNameAndPreservedTail"] = True
        button(state, "Open")
        state = snapshot("reopened")
        assert items(state, "AXStaticText", "GuiVerified")
        result["checks"]["reopenReadback"] = True
        close = [e for e in state["elements"] if e.get("role") == "AXButton"
                 and not e.get("label") and e.get("frame", {}).get("y", float("inf"))
                 < state["window_bounds"]["y"] + 32]
        assert close
        click(state, min(close, key=lambda e: e["frame"]["x"]))
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            windows = call("list_windows", {"pid": args.pid})["windows"]
            if not any(w["window_id"] == window for w in windows):
                break
            time.sleep(0.2)
        else:
            raise AssertionError("Clean window did not close")
        result["checks"]["cleanCloseObserved"] = True
        assert hashlib.sha256(binary.read_bytes()).hexdigest() == pin["binarySha256"]
        result["sources"] = {
            "originalSha256": hashlib.sha256(original).hexdigest(),
            "savedSha256": hashlib.sha256(saved.read_bytes()).hexdigest(),
            "binarySha256": pin["binarySha256"],
        }
        result["status"] = "PASS_INX_NATIVE_INPUT_WORKFLOW_ONLY"
    except Exception as error:
        result["error"] = f"{type(error).__name__}: {error}"
    finally:
        try:
            call("end_session", {"session": session})
        except Exception as error:
            result["sessionCleanupError"] = f"{type(error).__name__}: {error}"
        log.close()
        (directory / "native-input-result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))
    return 0 if result["status"].startswith("PASS_") else 1


if __name__ == "__main__":
    raise SystemExit(main())
