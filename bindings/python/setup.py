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
from pathlib import Path

from setuptools import setup
from setuptools.command.build_py import build_py

try:
    from setuptools.command.bdist_wheel import bdist_wheel
except ImportError:  # pragma: no cover - fallback for older setuptools
    from wheel.bdist_wheel import bdist_wheel


class BuildPyWithNativeBundle(build_py):
    """Copy native artifacts into the package before wheel build."""

    _NATIVE_FILES = (
        "libplasmite.dylib",
        "libplasmite.so",
        "libplasmite.a",
        "plasmite",
    )

    def run(self) -> None:
        self._bundle_native_assets()
        super().run()

    def _bundle_native_assets(self) -> None:
        project_root = Path(__file__).resolve().parent
        package_native_dir = project_root / "plasmite" / "_native"
        package_native_dir.mkdir(parents=True, exist_ok=True)

        for filename in self._NATIVE_FILES:
            candidate = package_native_dir / filename
            if candidate.exists():
                candidate.unlink()

        for src in self._native_candidates(project_root):
            if src.exists():
                dst = package_native_dir / src.name
                shutil.copy2(src, dst)
                if src.name == "plasmite":
                    dst.chmod(0o755)

    def _native_candidates(self, project_root: Path) -> list[Path]:
        sdk_dir_env = os.environ.get("PLASMITE_SDK_DIR")
        if sdk_dir_env:
            sdk_dir = Path(sdk_dir_env)
            return [
                sdk_dir / "lib" / "libplasmite.dylib",
                sdk_dir / "lib" / "libplasmite.so",
                sdk_dir / "lib" / "libplasmite.a",
                sdk_dir / "bin" / "plasmite",
            ]

        repo_root = project_root.parent.parent
        target_debug = repo_root / "target" / "debug"
        return [
            target_debug / "libplasmite.dylib",
            target_debug / "libplasmite.so",
            target_debug / "libplasmite.a",
            target_debug / "plasmite",
        ]


class BdistWheelWithNativeBundle(bdist_wheel):
    """Emit platform-tagged wheels because bundled assets are platform-specific."""

    def finalize_options(self) -> None:
        super().finalize_options()
        self.root_is_pure = False


setup(cmdclass={"build_py": BuildPyWithNativeBundle, "bdist_wheel": BdistWheelWithNativeBundle})
