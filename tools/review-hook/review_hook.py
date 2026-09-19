#!/usr/bin/env python3
"""Background review as a Codex Stop hook.

Spike for cbusillo/codex-lab#950. When a turn ends with uncommitted changes that have not
been reviewed yet, run stock Codex's own reviewer (`codex review --uncommitted`) in the
workspace and hand its findings back to the agent, which then continues and addresses them.

Standard library only. Reads the Stop hook input on stdin and writes the hook output on stdout.
"""

import hashlib
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

REENTRY_ENV = "LAB_REVIEW_HOOK_ACTIVE"
STATE_DIR = Path(os.environ.get("LAB_REVIEW_STATE_DIR", Path.home() / ".cache" / "lab-review-hook"))
REVIEW_TIMEOUT_SECONDS = int(os.environ.get("LAB_REVIEW_TIMEOUT_SECONDS", "540"))
MAX_FEEDBACK_BYTES = 8 * 1024
# `codex review` prints each finding as "- [P1] title — path:line".
FINDING = re.compile(r"^- \[P[0-3]\] ", re.MULTILINE)


def git(cwd: str, *args: str) -> str | None:
    result = subprocess.run(["git", "-C", cwd, *args], capture_output=True, text=True, check=False)
    return result.stdout if result.returncode == 0 else None


def change_fingerprint(cwd: str) -> str | None:
    """Identify the uncommitted change set, or return None when there is nothing to review."""
    status = git(cwd, "status", "--porcelain", "--untracked-files=all")
    if not status or not status.strip():
        return None
    diff = git(cwd, "diff", "HEAD") or ""
    untracked = git(cwd, "ls-files", "--others", "--exclude-standard") or ""
    digest = hashlib.sha256((status + diff).encode())
    for path in untracked.splitlines():
        try:
            digest.update(Path(cwd, path).read_bytes())
        except OSError:
            pass
    return digest.hexdigest()


def main() -> int:
    # The reviewer is itself a Codex run whose own turn end would trigger this hook again.
    if os.environ.get(REENTRY_ENV):
        return 0
    event = json.load(sys.stdin)
    # Codex sets this when the turn is already continuing because a Stop hook blocked it.
    # Reviewing again here could loop forever on a finding the agent cannot fix.
    if event.get("stop_hook_active"):
        return 0
    cwd = event["cwd"]
    fingerprint = change_fingerprint(cwd)
    if fingerprint is None:
        return 0
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    marker = STATE_DIR / fingerprint
    if marker.exists():
        return 0

    started = time.monotonic()
    try:
        review = subprocess.run(
            [os.environ.get("LAB_REVIEW_CODEX", "codex"), "review", "--uncommitted"],
            cwd=cwd,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=REVIEW_TIMEOUT_SECONDS,
            env={**os.environ, REENTRY_ENV: "1"},
            check=False,
        )
    except subprocess.TimeoutExpired:
        print(json.dumps({"systemMessage": f"Background review timed out after {REVIEW_TIMEOUT_SECONDS}s."}))
        return 0
    elapsed = round(time.monotonic() - started)
    if review.returncode != 0:
        print(json.dumps({"systemMessage": f"Background review failed: {review.stderr.strip()[-400:]}"}))
        return 0

    marker.write_text(review.stdout)
    findings = review.stdout.strip()
    if not FINDING.search(findings):
        print(json.dumps({"systemMessage": f"Background review: no issues ({elapsed}s)."}))
        return 0
    if len(findings.encode()) > MAX_FEEDBACK_BYTES:
        findings = findings.encode()[:MAX_FEEDBACK_BYTES].decode(errors="ignore") + "\n[review truncated]"
    print(
        json.dumps(
            {
                "decision": "block",
                "reason": (
                    f"A background review of your uncommitted changes finished in {elapsed}s. "
                    "Address each finding that is correct, and say briefly why for any you reject.\n\n"
                    f"{findings}"
                ),
            }
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
