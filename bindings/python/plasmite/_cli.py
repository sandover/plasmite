"""
Purpose: Expose a Python console script that forwards to the bundled plasmite CLI.
Key Exports: main()
Role: Keep `plasmite` command available from Python wheel installs.
Invariants: Uses package-local binary first, then falls back to PATH `plasmite`.
Invariants: Exit code mirrors the invoked process status.
Notes: This wrapper does not interpret arguments; it forwards them verbatim.
"""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import sys


def _bundled_cli_path() -> Path:
    cli_name = "plasmite.exe" if os.name == "nt" else "plasmite"
    return Path(__file__).resolve().parent / "_native" / cli_name


def main() -> int:
    bundled = _bundled_cli_path()
    if bundled.exists():
        cli_path = str(bundled)
    else:
        discovered = shutil.which("plasmite")
        if not discovered:
            print(
                "plasmite CLI not found; reinstall wheel with bundled assets or install system plasmite",
                file=sys.stderr,
            )
            return 1
        cli_path = discovered
    completed = subprocess.run([cli_path, *sys.argv[1:]], check=False)
    return int(completed.returncode)


if __name__ == "__main__":
    raise SystemExit(main())
