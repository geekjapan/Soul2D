#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("capacity", Path(__file__).with_name("measure-authoring-capacity.py"))
CAPACITY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CAPACITY)


class MeasurementTests(unittest.TestCase):
    def test_peak_is_per_child_and_failed_processes_remain_failed(self):
        with tempfile.TemporaryDirectory() as directory:
            large = CAPACITY.measure([sys.executable, "-c", "data=bytearray(64*1024*1024);print(sum(data))"],
                                     Path(directory) / "large.log")
            small = CAPACITY.measure([sys.executable, "-c", "print('small')"], Path(directory) / "small.log")
            failed = CAPACITY.measure([sys.executable, "-c", "raise SystemExit(7)"], Path(directory) / "failed.log")
            timeout = CAPACITY.measure([sys.executable, "-c", "import time;time.sleep(10)"],
                                       Path(directory) / "timeout.log", timeout=0.2)
            self.assertEqual(large["exitCode"], 0)
            self.assertEqual(small["exitCode"], 0)
            self.assertGreater(large["peakResidentBytes"], small["peakResidentBytes"] + 32 * 1024 * 1024)
            self.assertEqual(failed["exitCode"], 7)
            self.assertTrue(timeout["timedOut"])
            self.assertNotEqual(timeout["exitCode"], 0)


if __name__ == "__main__":
    unittest.main()
