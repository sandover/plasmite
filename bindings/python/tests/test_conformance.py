"""
Purpose: Run Python conformance manifests as part of tests.
Key Exports: None (unittest module).
Role: Ensure Python binding conforms to the manifest suite.
Invariants: Uses local libplasmite and plasmite CLI binaries.
Notes: Requires PLASMITE_LIB_DIR and PLASMITE_BIN to be resolvable.
"""

from __future__ import annotations

import os
import subprocess
import sys
import unittest
from pathlib import Path


class ConformanceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        repo_root = Path(__file__).resolve().parents[3]
        cls.repo_root = repo_root
        cls.bin_path = os.environ.get("PLASMITE_BIN") or str(repo_root / "target" / "debug" / "plasmite")
        cls.lib_dir = os.environ.get("PLASMITE_LIB_DIR") or str(repo_root / "target" / "debug")

        if not Path(cls.bin_path).exists():
            raise RuntimeError("plasmite binary not found; set PLASMITE_BIN or build target/debug/plasmite")

    def run_manifest(self, name: str) -> None:
        manifest = self.repo_root / "conformance" / name
        env = os.environ.copy()
        env["PLASMITE_BIN"] = self.bin_path
        env["PLASMITE_LIB_DIR"] = self.lib_dir
        if sys.platform == "darwin":
            env["DYLD_LIBRARY_PATH"] = (
                f"{self.lib_dir}:{env.get('DYLD_LIBRARY_PATH', '')}"
                if env.get("DYLD_LIBRARY_PATH")
                else self.lib_dir
            )
        elif sys.platform != "win32":
            env["LD_LIBRARY_PATH"] = (
                f"{self.lib_dir}:{env.get('LD_LIBRARY_PATH', '')}"
                if env.get("LD_LIBRARY_PATH")
                else self.lib_dir
            )

        subprocess.run(
            [
                sys.executable,
                str(self.repo_root / "bindings" / "python" / "cmd" / "plasmite_conformance.py"),
                str(manifest),
            ],
            check=True,
            env=env,
        )

    def test_sample(self) -> None:
        self.run_manifest("sample-v0.json")

    def test_negative(self) -> None:
        self.run_manifest("negative-v0.json")

    def test_multiprocess(self) -> None:
        self.run_manifest("multiprocess-v0.json")


if __name__ == "__main__":
    unittest.main()
