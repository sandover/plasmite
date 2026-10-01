"""Test console fallback without requiring an installed native library."""

from __future__ import annotations

import importlib.util
import io
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock
import zipfile


MODULE_PATH = Path(__file__).resolve().parents[1] / "plasmite" / "_cli.py"
SPEC = importlib.util.spec_from_file_location("plasmite_cli_under_test", MODULE_PATH)
cli = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(cli)


class ConsoleCliTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def executable(self, directory: str, content: bytes) -> Path:
        path = self.root / directory / ("plasmite.exe" if os.name == "nt" else "plasmite")
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        path.chmod(0o755)
        return path

    def windows_executable(self, directory: str, launcher: bool = False) -> Path:
        # DOS header points to the PE signature, as in a native Windows binary.
        header = bytearray(128)
        header[:2] = b"MZ"
        header[60:64] = (128).to_bytes(4, "little")
        path = self.root / directory / "plasmite.exe"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(header + b"PE\x00\x00" + bytes(256))
        if launcher:
            with zipfile.ZipFile(path, "a") as payload:
                payload.writestr("__main__.py", "from plasmite._cli import main\nmain()\n")
        return path

    def test_windows_fallback_skips_two_distlib_launchers_before_native_sdk(self) -> None:
        first = self.windows_executable("venv-a", launcher=True)
        second = self.windows_executable("venv-b", launcher=True)
        sdk = self.windows_executable("sdk")
        paths = {str(path.parent): str(path) for path in (first, second, sdk)}
        with mock.patch.object(cli.os, "get_exec_path", return_value=list(paths)), \
                mock.patch.object(cli.shutil, "which", side_effect=lambda name, path: paths[path]):
            # Verify discovery from either wrapper, even when its sibling comes first.
            for current in (first, second):
                with self.subTest(current=current), mock.patch.object(cli.sys, "argv", [str(current)]):
                    self.assertEqual(cli._system_cli_path(), sdk.resolve())

    def test_windows_setuptools_companion_script_is_not_a_native_sdk(self) -> None:
        launcher = self.windows_executable("setuptools")
        launcher.with_name("plasmite-script.py").write_text("from plasmite._cli import main\nmain()\n")
        sdk = self.windows_executable("sdk")
        paths = {str(path.parent): str(path) for path in (launcher, sdk)}
        with mock.patch.object(cli.os, "get_exec_path", return_value=list(paths)), \
                mock.patch.object(cli.shutil, "which", side_effect=lambda name, path: paths[path]), \
                mock.patch.object(cli.sys, "argv", [str(self.root / "other-wrapper.exe")]):
            self.assertEqual(cli._system_cli_path(), sdk.resolve())

    def test_windows_launchers_without_sdk_fail_without_spawning(self) -> None:
        first = self.windows_executable("venv-a", launcher=True)
        second = self.windows_executable("venv-b", launcher=True)
        paths = {str(path.parent): str(path) for path in (first, second)}
        stderr = io.StringIO()
        with mock.patch.object(cli, "_bundled_cli_path", return_value=self.root / "missing"), \
                mock.patch.object(cli.os, "get_exec_path", return_value=list(paths)), \
                mock.patch.object(cli.shutil, "which", side_effect=lambda name, path: paths[path]), \
                mock.patch.object(cli.sys, "argv", [str(first), "--version"]), \
                mock.patch.object(cli.sys, "stderr", stderr), \
                mock.patch.object(cli.subprocess, "run") as run:
            self.assertEqual(cli.main(), 1)
            run.assert_not_called()
        self.assertIn("install the Plasmite system SDK", stderr.getvalue())

    def test_source_install_skips_own_and_other_console_wrappers(self) -> None:
        wrapper = self.executable("venv", b"#!/usr/bin/python\nfrom plasmite._cli import main\n")
        other_wrapper = self.executable("other-venv", wrapper.read_bytes())
        sdk = self.executable("sdk", b"\x7fELFnative-sdk")
        with mock.patch.object(cli.os, "get_exec_path", return_value=[
            str(wrapper.parent), str(other_wrapper.parent), str(sdk.parent),
        ]), mock.patch.object(cli.sys, "argv", [str(wrapper), "--version"]):
            self.assertEqual(cli._system_cli_path(), sdk.resolve())

    def test_recognizes_supported_sdk_executable_formats(self) -> None:
        for magic in (b"\x7fELF", b"\xcf\xfa\xed\xfe", b"\xca\xfe\xba\xbe", b"MZ\x90\x00"):
            with self.subTest(magic=magic):
                sdk = self.executable("sdk", magic + b"native-sdk")
                with mock.patch.object(cli.os, "get_exec_path", return_value=[str(sdk.parent)]):
                    self.assertEqual(cli._system_cli_path(), sdk.resolve())

    def test_never_selects_current_command_or_interpreter(self) -> None:
        command = self.executable("command", b"\x7fELFcommand")
        interpreter = self.executable("interpreter", b"\x7fELFinterpreter")
        with mock.patch.object(cli.os, "get_exec_path", return_value=[
            str(command.parent), str(interpreter.parent),
        ]), mock.patch.object(cli.sys, "argv", [str(command)]), \
                mock.patch.object(cli.sys, "executable", str(interpreter)):
            self.assertIsNone(cli._system_cli_path())

    def test_missing_sdk_fails_without_launching_a_wrapper(self) -> None:
        wrapper = self.executable("venv", b"#!/usr/bin/python\nfrom plasmite._cli import main\n")
        missing = self.root / "missing-bundled"
        stderr = io.StringIO()
        with mock.patch.object(cli, "_bundled_cli_path", return_value=missing), \
                mock.patch.object(cli.os, "get_exec_path", return_value=[str(wrapper.parent)]), \
                mock.patch.object(cli.sys, "argv", [str(wrapper), "--version"]), \
                mock.patch.object(cli.sys, "stderr", stderr), \
                mock.patch.object(cli.subprocess, "run") as run:
            self.assertEqual(cli.main(), 1)
            run.assert_not_called()
        self.assertIn("install the Plasmite system SDK", stderr.getvalue())
        self.assertIn("PATH", stderr.getvalue())

    def test_bundled_cli_takes_precedence_and_forwards_arguments_and_status(self) -> None:
        bundled = self.executable("bundled", b"\x7fELFbundled")
        args = [str(self.root / "wrapper"), "pool", "list", "--json"]
        with mock.patch.object(cli, "_bundled_cli_path", return_value=bundled), \
                mock.patch.object(cli, "_system_cli_path") as fallback, \
                mock.patch.object(cli.sys, "argv", args), \
                mock.patch.object(cli.subprocess, "run", return_value=subprocess.CompletedProcess([], 7)) as run:
            self.assertEqual(cli.main(), 7)
            fallback.assert_not_called()
            run.assert_called_once_with([str(bundled), *args[1:]], check=False)

    def test_system_sdk_fallback_forwards_arguments(self) -> None:
        sdk = self.executable("sdk", b"\x7fELFnative-sdk")
        args = [str(self.root / "wrapper"), "version", "--json"]
        with mock.patch.object(cli, "_bundled_cli_path", return_value=self.root / "missing"), \
                mock.patch.object(cli.os, "get_exec_path", return_value=[str(sdk.parent)]), \
                mock.patch.object(cli.sys, "argv", args), \
                mock.patch.object(cli.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)) as run:
            self.assertEqual(cli.main(), 0)
            run.assert_called_once_with([str(sdk.resolve()), *args[1:]], check=False)


if __name__ == "__main__":
    unittest.main()
