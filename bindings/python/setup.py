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
import platform
import shutil
import sys
import sysconfig
from pathlib import Path

from setuptools import Distribution, Extension, setup
from setuptools.command.build_py import build_py
from setuptools.command.sdist import sdist

sys.path.insert(0, str(Path(__file__).resolve().parent))
from wheel_platform import macos_platform_tag

try:
    from setuptools.command.bdist_wheel import bdist_wheel
except ImportError:  # pragma: no cover - fallback for older setuptools
    from wheel.bdist_wheel import bdist_wheel


class NativeDistribution(Distribution):
    """Place the bundled C ABI library and CLI in platlib, not purelib."""

    def has_ext_modules(self) -> bool:
        return True


class BuildPyWithNativeBundle(build_py):
    """Copy native artifacts into the package before wheel build."""

    _NATIVE_FILES = (
        "plasmite.dll",
        "plasmite2.dll",
        "libplasmite.dylib",
        "libplasmite.2.dylib",
        "libplasmite.so",
        "libplasmite.so.2",
        "libplasmite.a",
        "plasmite.exe",
        "plasmite",
    )

    def run(self) -> None:
        super().run()
        if not self.distribution.ext_modules:
            # A fallback build must not inherit an extension from a prior wheel.
            for artifact in (Path(self.build_lib) / "plasmite").glob("_views_native.*"):
                if artifact.suffix in {".so", ".pyd"}:
                    artifact.unlink()
        self._bundle_native_assets()

    def _bundle_native_assets(self) -> None:
        project_root = Path(__file__).resolve().parent
        package_native_dir = Path(self.build_lib) / "plasmite" / "_native"
        package_native_dir.mkdir(parents=True, exist_ok=True)

        for filename in self._NATIVE_FILES:
            candidate = package_native_dir / filename
            if candidate.exists():
                candidate.unlink()

        for src, name in self._native_candidates(project_root):
            if src.is_file():
                dst = package_native_dir / name
                shutil.copy2(src, dst)
                if src.name == "plasmite":
                    dst.chmod(0o755)

    def _native_candidates(self, project_root: Path) -> list[tuple[Path, str]]:
        if sys.platform == "win32":
            lib_name, cargo_name, cli_name = "plasmite2.dll", "plasmite.dll", "plasmite.exe"
        elif sys.platform == "darwin":
            lib_name, cargo_name, cli_name = "libplasmite.2.dylib", "libplasmite.dylib", "plasmite"
        elif sys.platform.startswith("linux"):
            lib_name, cargo_name, cli_name = "libplasmite.so.2", "libplasmite.so", "plasmite"
        else:
            return []

        sdk_dir_env = os.environ.get("PLASMITE_SDK_DIR")
        if sdk_dir_env:
            sdk_dir = Path(sdk_dir_env)
            return [
                (sdk_dir / "lib" / lib_name, lib_name),
                (sdk_dir / "bin" / cli_name, cli_name),
            ]

        repo_root = project_root.parent.parent
        target_debug = repo_root / "target" / "debug"
        return [
            (target_debug / cargo_name, lib_name),
            (target_debug / cli_name, cli_name),
        ]


class BdistWheelWithNativeBundle(bdist_wheel):
    """Emit platform-tagged wheels because bundled assets are platform-specific."""

    def finalize_options(self) -> None:
        if self.distribution.ext_modules and sys.version_info >= (3, 11):
            self.py_limited_api = "cp311"
        super().finalize_options()
        self.root_is_pure = False

    def get_tag(self) -> tuple[str, str, str]:
        python_tag, abi_tag, platform_tag = super().get_tag()
        if platform_tag.startswith("macosx_"):
            install = self.get_finalized_command("install")
            native_dir = Path(install.install_lib) / "plasmite" / "_native"
            platform_tag = macos_platform_tag(
                native_dir, platform_tag, native_dir.parent
            )
        if self.distribution.ext_modules:
            return python_tag, abi_tag, platform_tag
        # The explicit fallback distribution contains ctypes only.
        return "py3", "none", platform_tag


project = Path(__file__).resolve().parent
sdk = os.environ.get("PLASMITE_SDK_DIR")
header_directories = [
    project / "plasmite" / "_include",
    project.parent.parent / "include",
]
if sdk:
    header_directories.insert(0, Path(sdk) / "include")
header = next(
    (
        directory
        for directory in header_directories
        if (directory / "plasmite.h").is_file()
        and "plsm_pool_fetch_view" in (directory / "plasmite.h").read_text()
    ),
    None,
)
view_mode = os.environ.get("PLASMITE_PYTHON_VIEWS", "auto")
if view_mode not in {"auto", "0", "1"}:
    raise RuntimeError("PLASMITE_PYTHON_VIEWS must be auto, 0 or 1")
native_views = (
    view_mode != "0"
    and platform.python_implementation() == "CPython"
    and not sysconfig.get_config_var("Py_GIL_DISABLED")
    and header is not None
)
if view_mode == "1" and not native_views:
    raise RuntimeError(
        "native views need standard CPython and the owned-view SDK header"
    )


class SourceWithViewHeader(sdist):
    """Carry the public ABI header so an sdist needs no repository checkout."""

    def make_release_tree(self, base_dir, files):
        super().make_release_tree(base_dir, files)
        if header is not None:
            destination = Path(base_dir) / "plasmite" / "_include"
            destination.mkdir(parents=True, exist_ok=True)
            shutil.copy2(header / "plasmite.h", destination / "plasmite.h")


extensions = []
if native_views:
    limited = sys.version_info >= (3, 11)
    extensions.append(
        Extension(
            "plasmite._views_native",
            sources=["native/views.c"],
            include_dirs=[str(header)],
            libraries=["dl"] if sys.platform.startswith("linux") else [],
            define_macros=[("Py_LIMITED_API", "0x030b0000")] if limited else [],
            py_limited_api=limited,
        )
    )

setup(
    ext_modules=extensions,
    distclass=NativeDistribution,
    cmdclass={
        "build_py": BuildPyWithNativeBundle,
        "bdist_wheel": BdistWheelWithNativeBundle,
        "sdist": SourceWithViewHeader,
    },
)
