"""
Purpose: Expose a Python console script that forwards to the bundled plasmite CLI.
Key Exports: main()
Role: Keep `plasmite` command available from Python wheel installs.
Invariants: Uses package-local binary first, then a native SDK executable on PATH.
Invariants: Never launches a console wrapper as its fallback.
Invariants: Exit code mirrors the invoked process status.
Notes: This wrapper does not interpret arguments; it forwards them verbatim.
"""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import sys
import zipfile


def _bundled_cli_path() -> Path:
    cli_name = "plasmite.exe" if os.name == "nt" else "plasmite"
    return Path(__file__).resolve().parent / "_native" / cli_name


def _is_python_console_launcher(candidate: Path) -> bool:
    # Older setuptools launchers execute a companion script. Distlib launchers
    # put __main__.py in a ZIP appended to the PE executable instead.
    if candidate.with_name(candidate.stem + "-script.py").exists():
        return True
    try:
        with zipfile.ZipFile(candidate) as payload:
            return "__main__.py" in payload.namelist()
    except zipfile.BadZipFile:
        return False  # An ordinary native executable has no appended ZIP.
    except OSError:
        return True  # An unreadable candidate cannot establish a safe fallback.


def _system_cli_path() -> Path | None:
    # Source installs provide this same console command. Search past wrappers
    # to the SDK binary instead of recursively invoking another Python entrypoint.
    excluded = {Path(sys.argv[0]).resolve(), Path(sys.executable).resolve()}
    native_magic = {
        b"\x7fELF",
        b"\xfe\xed\xfa\xce", b"\xce\xfa\xed\xfe",  # Mach-O
        b"\xfe\xed\xfa\xcf", b"\xcf\xfa\xed\xfe",
        b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca",  # universal Mach-O
        b"\xca\xfe\xba\xbf", b"\xbf\xba\xfe\xca",
    }
    for directory in os.get_exec_path():
        discovered = shutil.which("plasmite", path=directory)
        if not discovered:
            continue
        candidate = Path(discovered).resolve()
        if candidate in excluded:
            continue
        try:
            with candidate.open("rb") as executable:
                magic = executable.read(4)
        except OSError:
            continue
        if magic in native_magic:
            return candidate
        if magic.startswith(b"MZ") and not _is_python_console_launcher(candidate):
            return candidate
    return None


def main() -> int:
    bundled = _bundled_cli_path()
    if bundled.exists():
        cli_path = str(bundled)
    else:
        discovered = _system_cli_path()
        if discovered is None:
            print(
                "plasmite native CLI not found; install the Plasmite system SDK "
                "and add its bin directory to PATH, or reinstall a wheel with bundled assets",
                file=sys.stderr,
            )
            return 1
        cli_path = str(discovered)
    completed = subprocess.run([cli_path, *sys.argv[1:]], check=False)
    return int(completed.returncode)


if __name__ == "__main__":
    raise SystemExit(main())
