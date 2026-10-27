#!/usr/bin/env python3
"""Measure isolated release regression processes, including fixture construction and assertions."""
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import statistics
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
CASES = [
    "a_pro_size_cmo3_opens_edits_saves_and_reopens",
    "a_pro_size_cmo3_with_many_textures_and_deformers_opens_edits_saves_and_reopens",
    "a_pro_size_can3_curve_opens_edits_saves_and_reopens",
    "a_pro_size_cmo3_large_textures_render_and_roundtrip",
    "a_pro_size_cmo3_keyed_parameters_and_dense_forms_roundtrip",
    "a_capacity_native_texture_limits_refuse_without_state_changes",
]


def windows_peak(child):
    from ctypes import wintypes

    class Counters(ctypes.Structure):
        _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD)] + [
            (name, ctypes.c_size_t) for name in (
                "PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage", "QuotaPagedPoolUsage",
                "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage", "PagefileUsage", "PeakPagefileUsage")]

    counters = Counters()
    counters.cb = ctypes.sizeof(counters)
    query = ctypes.WinDLL("psapi", use_last_error=True).GetProcessMemoryInfo
    query.argtypes = [wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD]
    query.restype = wintypes.BOOL
    # CPython retains this owned process handle until Popen is disposed, including after exit.
    if not query(int(child._handle), ctypes.byref(counters), counters.cb):
        raise ctypes.WinError(ctypes.get_last_error())
    return counters.PeakWorkingSetSize


def measure(command, log, timeout=600):
    start = time.perf_counter()
    with Path(log).open("w", encoding="utf-8") as output:
        child = subprocess.Popen(command, cwd=ROOT, stdout=output, stderr=subprocess.STDOUT)
        timed_out = False
        if os.name == "nt":
            try:
                child.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                timed_out = True
                child.kill()
                child.wait()
            peak = windows_peak(child)
        else:
            while True:
                pid, status, usage = os.wait4(child.pid, os.WNOHANG)
                if pid:
                    child.returncode = os.waitstatus_to_exitcode(status)
                    break
                if time.perf_counter() - start > timeout and not timed_out:
                    timed_out = True
                    try:
                        os.kill(child.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                time.sleep(0.02)
            peak = usage.ru_maxrss * (1 if sys.platform == "darwin" else 1024)
    if peak <= 0:
        raise RuntimeError("OS did not return a process high-water memory counter")
    return {"command": command, "seconds": time.perf_counter() - start,
            "peakResidentBytes": peak, "exitCode": child.returncode, "timedOut": timed_out,
            "log": str(log)}


def capture(*command):
    return subprocess.check_output(command, cwd=ROOT, text=True).strip()


def hardware():
    info = {"platform": platform.platform(), "architecture": platform.machine(), "logicalCpus": os.cpu_count(),
            "runnerImage": os.environ.get("ImageOS"), "runnerImageVersion": os.environ.get("ImageVersion"),
            "loadAverage": list(os.getloadavg()) if hasattr(os, "getloadavg") else None}
    if sys.platform == "darwin":
        info.update(cpu=capture("sysctl", "-n", "machdep.cpu.brand_string"),
                    physicalMemoryBytes=int(capture("sysctl", "-n", "hw.memsize")))
    elif os.name == "nt":
        value = capture("powershell", "-NoProfile", "-Command",
                        "$c=Get-CimInstance Win32_Processor;$m=Get-CimInstance Win32_ComputerSystem;"
                        "@{cpu=$c[0].Name;physicalMemoryBytes=[long]$m.TotalPhysicalMemory}|ConvertTo-Json -Compress")
        info.update(json.loads(value))
    else:
        info.update(cpu=next(line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines()
                             if line.startswith("model name")),
                    physicalMemoryBytes=os.sysconf("SC_PHYS_PAGES") * os.sysconf("SC_PAGE_SIZE"))
    return info


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--executable", type=Path, required=True)
    parser.add_argument("--build-receipt", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--repeats", type=int, default=5)
    args = parser.parse_args()
    if args.repeats < 5:
        parser.error("at least five repetitions are required")
    args.out.mkdir(parents=True, exist_ok=False)
    executable = args.executable.resolve()
    artifacts = [json.loads(line) for line in args.build_receipt.read_text(encoding="utf-8").splitlines()]
    artifact = [row for row in artifacts if row.get("reason") == "compiler-artifact" and row.get("executable")
                and Path(row["executable"]).resolve() == executable]
    if len(artifact) != 1 or artifact[0]["profile"]["opt_level"] != "3" or artifact[0]["profile"]["debug_assertions"]:
        raise RuntimeError("measurement requires the matching optimized release Cargo artifact receipt")
    machine = hardware()
    supported = {"Darwin": {"arm64"}, "Windows": {"AMD64", "x86_64"}, "Linux": {"x86_64"}}
    if machine["architecture"] not in supported.get(platform.system(), set()):
        raise RuntimeError(f"unsupported capacity target: {machine}")
    report = {"passed": False, "scope": __doc__, "profile": artifact[0]["profile"], "hardware": machine,
              "commit": capture("git", "rev-parse", "HEAD"), "rustc": capture("rustc", "-Vv"),
              "cargo": capture("cargo", "--version"), "executableSha256": hashlib.sha256(executable.read_bytes()).hexdigest(),
              "lockSha256": hashlib.sha256((ROOT / "rust/Cargo.lock").read_bytes()).hexdigest(),
              "sourceHashes": {path: hashlib.sha256((ROOT / path).read_bytes()).hexdigest() for path in (
                  "rust/tests/core.rs", "rust/src/caff.rs", "rust/src/authoring_scene.rs", "rust/src/animation_source.rs",
                  "rust/src/native_scene.rs", "rust/src/render.rs", "tools/measure-authoring-capacity.py")},
              "memoryMetric": "Windows peak working set" if os.name == "nt" else "OS child peak resident set",
              "runs": [], "summary": {}}
    try:
        for repeat in range(args.repeats):
            for case in CASES:
                log = args.out / f"{case}-{repeat}.log"
                run = measure([str(executable), "--exact", case, "--include-ignored", "--nocapture", "--test-threads=1"], log)
                run.update(case=case, repeat=repeat)
                output = log.read_text(encoding="utf-8")
                run["assertionsPassed"] = "test result: ok. 1 passed; 0 failed; 0 ignored;" in output
                report["runs"].append(run)
                if run["exitCode"] != 0 or run["timedOut"] or not run["assertionsPassed"]:
                    raise RuntimeError(f"capacity workload failed: {case}; see {log}")
        for case in CASES:
            runs = [run for run in report["runs"] if run["case"] == case]
            report["summary"][case] = {key: {"median": statistics.median(values), "min": min(values), "max": max(values)}
                for key in ("seconds", "peakResidentBytes") for values in [[run[key] for run in runs]]}
        report["passed"] = True
    except Exception as error:
        report["error"] = str(error)
    (args.out / "result.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"passed": report["passed"], "result": str(args.out / "result.json")}))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
