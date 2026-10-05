"""
Purpose: Extend setuptools build to bundle native SDK artifacts into Python wheels.
Key Exports: build_py override that stages libplasmite + plasmite CLI under plasmite/_native.
Role: Packaging bridge for batteries-included Python distributions.
Invariants: PLASMITE_LIB_DIR remains a runtime override; bundled assets are optional for source installs.
Invariants: Wheels should include at most one shared library and one CLI binary.
Notes: Prefers PLASMITE_SDK_DIR (SDK layout), then falls back to repo-local target/debug.
"""

from __future__ import annotations

import os
import shutil
import sys
from pathlib import Path

from setuptools import setup
from setuptools.command.build_py import build_py
sys.path.insert(0, str(Path(__file__).resolve().parent))
from wheel_platform import macos_platform_tag

try:
    from setuptools.command.bdist_wheel import bdist_wheel
except ImportError:  # pragma: no cover - fallback for older setuptools
    from wheel.bdist_wheel import bdist_wheel


class BuildPyWithNativeBundle(build_py):
    """Copy native artifacts into the package before wheel build."""

    _NATIVE_FILES = (
        "plasmite.dll",
        "libplasmite.dylib",
        "libplasmite.so",
        "libplasmite.a",
        "plasmite.exe",
        "plasmite",
    )

    def run(self) -> None:
        super().run()
        self._bundle_native_assets()

    def _bundle_native_assets(self) -> None:
        project_root = Path(__file__).resolve().parent
        package_native_dir = Path(self.build_lib) / "plasmite" / "_native"
        package_native_dir.mkdir(parents=True, exist_ok=True)

        for filename in self._NATIVE_FILES:
            candidate = package_native_dir / filename
            if candidate.exists():
                candidate.unlink()

        for src in self._native_candidates(project_root):
            if src.is_file():
                dst = package_native_dir / src.name
                shutil.copy2(src, dst)
                if src.name == "plasmite":
                    dst.chmod(0o755)

    def _native_candidates(self, project_root: Path) -> list[Path]:
        if sys.platform == "win32":
            lib_name, cli_name = "plasmite.dll", "plasmite.exe"
        elif sys.platform == "darwin":
            lib_name, cli_name = "libplasmite.dylib", "plasmite"
        elif sys.platform.startswith("linux"):
            lib_name, cli_name = "libplasmite.so", "plasmite"
        else:
            return []

        sdk_dir_env = os.environ.get("PLASMITE_SDK_DIR")
        if sdk_dir_env:
            sdk_dir = Path(sdk_dir_env)
            return [
                sdk_dir / "lib" / lib_name,
                sdk_dir / "bin" / cli_name,
            ]

        repo_root = project_root.parent.parent
        target_debug = repo_root / "target" / "debug"
        return [
            target_debug / lib_name,
            target_debug / cli_name,
        ]


class BdistWheelWithNativeBundle(bdist_wheel):
    """Emit platform-tagged wheels because bundled assets are platform-specific."""

    def finalize_options(self) -> None:
        super().finalize_options()
        self.root_is_pure = False

    def get_tag(self) -> tuple[str, str, str]:
        python_tag, abi_tag, platform_tag = super().get_tag()
        if platform_tag.startswith("macosx_"):
            install = self.get_finalized_command("install")
            native_dir = Path(install.install_lib) / "plasmite" / "_native"
            platform_tag = macos_platform_tag(native_dir, platform_tag)
        return python_tag, abi_tag, platform_tag


setup(cmdclass={"build_py": BuildPyWithNativeBundle, "bdist_wheel": BdistWheelWithNativeBundle})
