"""
Purpose: Run Python conformance manifests as part of tests.
Key Exports: None (unittest module).
Role: Ensure Python binding conforms to the manifest suite.
Invariants: Uses local libplasmite and plasmite CLI binaries.
Notes: Requires PLASMITE_LIB_DIR and PLASMITE_BIN to be resolvable.
"""

from __future__ import annotations

import json
import os
import runpy
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


class ConformanceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        repo_root = Path(__file__).resolve().parents[3]
        cls.repo_root = repo_root
        cls.bin_path = os.environ.get("PLASMITE_BIN") or str(repo_root / "target" / "debug" / "plasmite")
        cls.lib_dir = os.environ.get("PLASMITE_LIB_DIR") or str(repo_root / "target" / "debug")

        if not Path(cls.bin_path).exists():
            raise RuntimeError("plasmite binary not found; set PLASMITE_BIN or build target/debug/plasmite")

    def run_manifest(self, name: str | Path, *, expect_failure: bool = False) -> subprocess.CompletedProcess[str]:
        manifest = name if isinstance(name, Path) else self.repo_root / "conformance" / name
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

        return subprocess.run(
            [
                sys.executable,
                str(self.repo_root / "bindings" / "python" / "cmd" / "plasmite_conformance.py"),
                str(manifest),
            ],
            check=not expect_failure,
            env=env,
            capture_output=expect_failure,
            text=True,
        )

    def test_sample(self) -> None:
        self.run_manifest("sample-v0.json")

    def test_negative(self) -> None:
        self.run_manifest("negative-v0.json")

    def test_multiprocess(self) -> None:
        self.run_manifest("multiprocess-v0.json")

    def test_pool_admin(self) -> None:
        self.run_manifest("pool-admin-v0.json")

    def test_retention_gap(self) -> None:
        self.run_manifest("retention-gap-v0.json")

    def test_workdir_must_be_a_scratch_name(self) -> None:
        for workdir in (
            "", ".", "..", "src", "work-", "work-..", "work-a.", "../outside",
            "work-../outside", "nested/child", "nested\\child", "C:outside", "bad\0name", 42,
        ):
            with self.subTest(workdir=workdir), tempfile.TemporaryDirectory() as temp_dir:
                root = Path(temp_dir)
                manifest_dir = root / "manifests"
                manifest_dir.mkdir()
                outside = root / "outside"
                outside.mkdir()
                sentinel = outside / "keep"
                sentinel.write_text("keep", encoding="utf-8")
                manifest = manifest_dir / "invalid.json"
                manifest.write_text(
                    json.dumps({"conformance_version": 0, "workdir": workdir, "steps": []}),
                    encoding="utf-8",
                )

                result = self.run_manifest(manifest, expect_failure=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("workdir must be 'work' or a work- name", result.stderr)
                self.assertEqual(sentinel.read_text(encoding="utf-8"), "keep")

    def test_workdir_rejects_existing_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            manifest_dir = root / "manifests"
            manifest_dir.mkdir()
            outside = root / "outside"
            outside.mkdir()
            sentinel = outside / "keep"
            sentinel.write_text("keep", encoding="utf-8")
            link = manifest_dir / "work"
            try:
                link.symlink_to(outside, target_is_directory=True)
            except OSError as err:
                self.skipTest(f"cannot create directory symlink: {err}")

            manifest = manifest_dir / "symlink.json"
            manifest.write_text(
                json.dumps({"conformance_version": 0, "workdir": "work", "steps": []}),
                encoding="utf-8",
            )
            result = self.run_manifest(manifest, expect_failure=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("workdir must not be a symlink", result.stderr)
            self.assertTrue(link.is_symlink())
            self.assertEqual(sentinel.read_text(encoding="utf-8"), "keep")

    def test_tail_closes_stream_after_read_or_parse_failure(self) -> None:
        runner_path = self.repo_root / "bindings" / "python" / "cmd" / "plasmite_conformance.py"
        with patch.dict(os.environ, {"PLASMITE_LIB_DIR": self.lib_dir}):
            run_tail = runpy.run_path(str(runner_path))["run_tail"]

        class Stream:
            def __init__(self, result: bytes | Exception) -> None:
                self.result = result
                self.closed = False

            def next_json(self) -> bytes:
                if isinstance(self.result, Exception):
                    raise self.result
                return self.result

            def __enter__(self) -> Stream:
                return self

            def __exit__(self, *_args: object) -> None:
                self.closed = True

        class Pool:
            def __init__(self, stream: Stream) -> None:
                self.stream = stream
                self.closed = False

            def open_stream(self, *_args: object) -> Stream:
                return self.stream

            def close(self) -> None:
                self.closed = True

        class Client:
            def __init__(self, pool: Pool) -> None:
                self.pool = pool

            def open_pool(self, _name: str) -> Pool:
                return self.pool

        for result, error_type in ((RuntimeError("read failed"), RuntimeError), (b"invalid json", ValueError)):
            with self.subTest(error_type=error_type):
                stream = Stream(result)
                pool = Pool(stream)
                with self.assertRaises(error_type):
                    run_tail(Client(pool), {"pool": "chat", "expect": {"messages": []}}, 0, None)
                self.assertTrue(stream.closed)
                self.assertTrue(pool.closed)


if __name__ == "__main__":
    unittest.main()
