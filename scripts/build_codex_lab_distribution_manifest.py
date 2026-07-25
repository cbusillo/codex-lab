#!/usr/bin/env python3
"""Generate or validate a Codex Lab distribution manifest."""

from pathlib import Path
import sys


# Some developer environments set PYTHONSAFEPATH=1, which prevents Python from
# adding the script directory to sys.path. Add it explicitly so the local helper
# package remains importable when this executable is launched from any cwd.
sys.path.insert(0, str(Path(__file__).resolve().parent))

from codex_lab_package.distribution_manifest import main


if __name__ == "__main__":
    raise SystemExit(main())
