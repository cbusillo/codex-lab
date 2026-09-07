#!/usr/bin/env python3
"""Analyze a bounded, preregistered busy-host A/A latency manifest."""

import argparse
import json
import math
import os
import re
import stat
import sys
from pathlib import Path
from statistics import median
from typing import Any

MAX_BYTES = 256 * 1024
MAX_ATTEMPTS = 40
LABEL = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,119}\Z")


class BaselineError(ValueError):
    """Raised when an input is outside the bounded analysis contract."""


class EvidenceUnavailable(BaselineError):
    """Raised when a referenced evidence file cannot be read."""


def safe_label(value: str) -> str:
    if LABEL.fullmatch(value) is None:
        raise argparse.ArgumentTypeError("expected a public-safe label")
    return value


def read_json(path: Path, limit: int = MAX_BYTES) -> dict[str, Any]:
    fd = None
    try:
        flags = os.O_RDONLY | os.O_NONBLOCK | getattr(os, "O_NOFOLLOW", 0)
        fd = os.open(path, flags)
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise BaselineError(f"refusing unsafe or oversized input: {path}")
        with os.fdopen(fd, "rb") as handle:
            fd = None
            raw = handle.read(limit + 1)
        if len(raw) > limit:
            raise BaselineError(f"refusing unsafe or oversized input: {path}")
        value = json.loads(raw.decode("utf-8"))
    except (OSError, UnicodeError) as error:
        raise EvidenceUnavailable(f"evidence unavailable: {path}") from error
    except (BaselineError, json.JSONDecodeError) as error:
        if isinstance(error, BaselineError):
            raise
        raise BaselineError(f"invalid JSON input: {path}") from error
    finally:
        if fd is not None:
            os.close(fd)
    if not isinstance(value, dict):
        raise BaselineError(f"JSON root must be an object: {path}")
    return value


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--evidence-root", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--lane", required=True, type=safe_label)
    parser.add_argument("--configuration", required=True, type=safe_label)
    return parser.parse_args(argv)


def evidence_path(root: Path, name: object) -> Path:
    if not isinstance(name, str) or not name or "\0" in name:
        raise BaselineError("evidence must be a non-empty relative filename")
    if Path(name).is_absolute() or Path(name).parts[0] == "..":
        raise BaselineError("evidence path must be relative")
    candidate = root / name
    if candidate.is_symlink():
        raise BaselineError("symlink evidence is not accepted")
    path = candidate.resolve()
    try:
        path.relative_to(root.resolve())
    except ValueError as error:
        raise BaselineError("evidence path escapes --evidence-root") from error
    return path


def map_value(value: object, field: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise BaselineError(f"{field} must be an object")
    return value


def required_string(value: object, field: str) -> str:
    if not isinstance(value, str) or not value:
        raise BaselineError(f"{field} must be a non-empty string")
    return value


def identity(record: dict[str, Any]) -> tuple[Any, ...]:
    source = map_value(record.get("source"), "source")
    edit = map_value(record.get("sourceEdit"), "sourceEdit")
    context = map_value(record.get("buildContext"), "buildContext")
    files = map_value(context.get("fileFingerprints"), "fileFingerprints")
    return (
        required_string(source.get("commit"), "source.commit"),
        required_string(
            edit.get("expectedDiffSha256"), "sourceEdit.expectedDiffSha256"
        ),
        required_string(context.get("configuration"), "buildContext.configuration"),
        required_string(
            context.get("invocationFingerprint"), "buildContext.invocationFingerprint"
        ),
        required_string(
            context.get("sourcePathFingerprint"), "buildContext.sourcePathFingerprint"
        ),
        required_string(files.get("lockfile"), "fileFingerprints.lockfile"),
        required_string(files.get("toolchain"), "fileFingerprints.toolchain"),
        required_string(record.get("scenario"), "scenario"),
    )


def qualify(record: dict[str, Any], source_commit: str) -> tuple[bool, str | None]:
    quality = record.get("measurementQuality")
    duration = command_duration(record)
    if record.get("schemaVersion") != 4:
        return False, "feedback-schema-unsupported"
    if map_value(record.get("source"), "source").get("commit") != source_commit:
        return False, "source-commit-mismatch"
    if (
        not isinstance(quality, dict)
        or quality.get("matchedAnalysisEligible") is not True
    ):
        return False, "matched-analysis-ineligible"
    if duration is None:
        return False, "command-duration-missing"
    if record.get("commandStatus") != "completed" or record.get("exitCode") != 0:
        return False, "command-failed"
    return True, None


def command_duration(record: dict[str, Any]) -> float | int | None:
    phases = record.get("phaseDurationsMs")
    value = phases.get("command") if isinstance(phases, dict) else None
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return value if value > 0 and math.isfinite(value) else None


def analyze(args: argparse.Namespace) -> dict[str, Any]:
    manifest = read_json(args.manifest)
    attempts = manifest.get("attempts")
    source_commit = manifest.get("sourceCommit")
    if not isinstance(attempts, list) or not 0 < len(attempts) <= MAX_ATTEMPTS:
        raise BaselineError("manifest attempts must contain 1..40 records")
    source_commit = required_string(source_commit, "manifest sourceCommit")
    grouped: dict[int, list[dict[str, Any]]] = {}
    analyzed: list[dict[str, Any]] = []
    seen: set[tuple[int, str, int]] = set()
    environment_entries: dict[str, list[dict[str, Any]]] = {}
    for item in attempts:
        if not isinstance(item, dict) or not isinstance(item.get("pair"), int):
            raise BaselineError("each manifest attempt needs an integer pair")
        pair, replica, order = item["pair"], item.get("replica"), item.get("orderIndex")
        if (
            pair < 1
            or not isinstance(replica, str)
            or not isinstance(order, int)
            or order not in (0, 1)
        ):
            raise BaselineError("attempt pair, replica, or order is malformed")
        key = (pair, replica, order)
        if (
            key in seen
            or any(existing[0] == pair and existing[1] == replica for existing in seen)
            or any(existing[0] == pair and existing[2] == order for existing in seen)
        ):
            raise BaselineError(f"duplicate pair, replica, or order for pair {pair}")
        seen.add(key)
        path = evidence_path(args.evidence_root, item.get("evidence"))
        try:
            record = read_json(path)
        except BaselineError:
            entry = {
                "pair": pair,
                "replica": replica,
                "orderIndex": order,
                "eligible": False,
                "failure": "evidence-unavailable",
                "integrityReasons": [],
                "durationMs": None,
                "environmentFingerprint": None,
            }
            analyzed.append(entry)
            grouped.setdefault(pair, []).append({"entry": entry, "identity": None})
            continue
        if item.get("harnessExitCode") != record.get("exitCode"):
            raise BaselineError(f"manifest/evidence exit mismatch for pair {pair}")
        edit = map_value(record.get("sourceEdit"), "sourceEdit")
        if item.get("expectedDiffSha256") != edit.get("expectedDiffSha256"):
            raise BaselineError(f"manifest/evidence edit mismatch for pair {pair}")
        eligible, failure = qualify(record, source_commit)
        quality = record.get("measurementQuality", {})
        integrity = quality.get("integrity", {}) if isinstance(quality, dict) else {}
        environment = map_value(record.get("buildContext"), "buildContext")
        environment_fingerprint = required_string(
            environment.get("environmentFingerprint"),
            "buildContext.environmentFingerprint",
        )
        entry = {
            "pair": pair,
            "replica": replica,
            "orderIndex": order,
            "eligible": eligible,
            "failure": failure,
            "integrityReasons": integrity.get("reasons", [])
            if isinstance(integrity, dict)
            else [],
            "durationMs": command_duration(record),
            "environmentFingerprint": environment_fingerprint,
        }
        environment_entries.setdefault(replica, []).append(entry)
        analyzed.append(entry)
        grouped.setdefault(pair, []).append(
            {"entry": entry, "identity": identity(record)}
        )
    for replica, entries in environment_entries.items():
        if len({entry["environmentFingerprint"] for entry in entries}) > 1:
            for entry in entries:
                entry.update(eligible=False, failure="replica-environment-drift")
    ratios: list[dict[str, Any]] = []
    complete = 0
    for pair, members in sorted(grouped.items()):
        if len(members) != 2 or {m["entry"]["orderIndex"] for m in members} != {0, 1}:
            continue
        if any(member["identity"] is None for member in members):
            continue
        complete += 1
        if members[0]["identity"] != members[1]["identity"]:
            for member in members:
                if member["entry"]["eligible"]:
                    member["entry"].update(
                        eligible=False, failure="pair-identity-mismatch"
                    )
            continue
        valid = [m["entry"] for m in members if m["entry"]["eligible"]]
        if len(valid) == 2 and valid[0]["replica"] != valid[1]["replica"]:
            a, b = sorted(valid, key=lambda member: member["replica"])
            ratio = math.log(a["durationMs"]) - math.log(b["durationMs"])
            ratios.append(
                {
                    "pair": pair,
                    "logRatioReplica0OverReplica1": ratio,
                    "replica0": a["replica"],
                    "replica1": b["replica"],
                }
            )
    absolute = [abs(x["logRatioReplica0OverReplica1"]) for x in ratios]
    return {
        "schemaVersion": 1,
        "kind": "busy-host-aa-latency-analysis",
        "lane": args.lane,
        "configuration": args.configuration,
        "sourceCommit": source_commit,
        "manifest": str(args.manifest),
        "attempts": analyzed,
        "summary": {
            "attemptCount": len(analyzed),
            "failureCount": sum(not x["eligible"] for x in analyzed),
            "completePairCount": complete,
            "incompletePairCount": len(grouped) - complete,
            "eligiblePairCount": len(ratios),
            "pairedLogRatios": ratios,
            "medianAbsoluteLogNoise": median(absolute) if absolute else None,
            "worstObserved": max(
                ratios, key=lambda x: abs(x["logRatioReplica0OverReplica1"])
            )
            if ratios
            else None,
        },
        "scope": "Descriptive latency noise only; no cache, storage, or winner claim.",
    }


def main(argv: list[str] | None = None) -> int:
    try:
        args = parse_args(sys.argv[1:] if argv is None else argv)
        if args.output.exists() or args.output.is_symlink():
            raise BaselineError("--output must be a new file")
        if not args.output.parent.is_dir():
            raise BaselineError("--output parent must be an existing directory")
        result = analyze(args)
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
        fd = os.open(args.output, flags, 0o644)
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            fd = None
            handle.write(
                json.dumps(result, indent=2, sort_keys=True, allow_nan=False) + "\n"
            )
        return 0
    except BaselineError as error:
        print(f"busy-host baseline: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
