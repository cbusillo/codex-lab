#!/usr/bin/env python3
"""Validate bounded repair cycles and emit an exact-SHA handoff."""

import argparse
import json
import os
import re
import sys
from pathlib import Path


MAX_LEDGER_BYTES = 256 * 1024
MAX_INPUT_BYTES = 1024 * 1024
MAX_HANDOFF_BYTES = 8 * 1024
MAX_UNRESOLVED = 24
MAX_CYCLES = 64
MAX_PATHS = 64
MAX_STRING = 1024
MAX_CANDIDATE = 256
MAX_UNIT_ID = 640
MAX_HANDOFF_UNIT_ID = 160
TOKEN_BUDGET = 40_000
SHA_RE = re.compile(r"[0-9a-f]{40}")
REPAIR_STATUSES = {"failed", "repaired"}
REPAIR_AGENTS = {"human", "model", "script"}
MODEL_TIERS = {"budget", "frontier", "none"}
ACCOUNTING_CONFIDENCE = {"explicit_zero", "provider_reported"}
FORBIDDEN_KEYS = {
    "command",
    "commands",
    "credential",
    "credentials",
    "exec",
    "execute",
    "function",
    "process",
    "pr",
    "push",
    "secret",
    "secrets",
    "shell",
    "subprocess",
    "test",
    "tests",
    "write",
}
MONEY_PARTS = ("cost", "currency", "dollar", "money", "price")


class LedgerError(ValueError):
    pass


def flat(value: object, limit: int) -> str:
    return str(value).replace("\r", " ").replace("\n", " ").replace("\x00", " ")[:limit]


def read_object(
    path: Path, label: str, limit: int = MAX_LEDGER_BYTES
) -> dict[str, object]:
    try:
        if path.stat().st_size > limit:
            raise LedgerError(f"{label} exceeds {limit} bytes")
        value = json.loads(path.read_text(encoding="utf-8"))
    except LedgerError:
        raise
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise LedgerError(f"unable to read {label}: {flat(error, 256)}") from error
    if not isinstance(value, dict):
        raise LedgerError(f"{label} must be a JSON object")
    return value


def validate_strings(value: object, key: str = "") -> None:
    if isinstance(value, dict):
        for name, child in value.items():
            lowered = str(name).casefold()
            if any(part in lowered for part in MONEY_PARTS):
                raise LedgerError("monetary fields are not permitted")
            if lowered in FORBIDDEN_KEYS:
                raise LedgerError(f"execution field is not permitted: {name}")
            validate_strings(child, lowered)
    elif isinstance(value, list):
        for child in value:
            validate_strings(child, key)
    elif isinstance(value, str):
        limit = MAX_CANDIDATE if "candidate" in key else MAX_STRING
        if len(value) > limit or "\x00" in value:
            raise LedgerError("ledger contains an oversized or unsafe string")


def exact_ref(value: object, label: str) -> str:
    if not isinstance(value, str) or not SHA_RE.fullmatch(value):
        raise LedgerError(f"{label} must be an exact lowercase 40-hex SHA")
    return value


def evidence_refs(evidence: dict[str, object]) -> dict[str, str]:
    refs = evidence.get("refs")
    if not isinstance(refs, dict):
        raise LedgerError("candidate evidence has no refs object")
    return {
        name: exact_ref(refs.get(name), f"evidence refs.{name}")
        for name in ("base", "upstream", "local")
    }


def workflow_info(evidence: dict[str, object]) -> dict[str, str]:
    workflow = evidence.get("workflow")
    if not isinstance(workflow, dict):
        return {"sha": evidence_refs(evidence)["local"], "runId": "unavailable"}
    sha = workflow.get("sha", evidence_refs(evidence)["local"])
    run_id = workflow.get("runId", "unavailable")
    if (
        not isinstance(run_id, str)
        or not run_id
        or len(run_id) > 128
        or "\n" in run_id
        or "\r" in run_id
    ):
        raise LedgerError("workflow run id is invalid")
    return {"sha": exact_ref(sha, "workflow sha"), "runId": flat(run_id, 128)}


def stage3c_units(
    packets_path: Path, telemetry_path: Path
) -> tuple[list[str], list[str], list[str]]:
    packets = read_object(packets_path, "model packets", MAX_INPUT_BYTES)
    telemetry = read_object(telemetry_path, "model telemetry", MAX_INPUT_BYTES)
    if packets.get("schemaVersion") != 1 or packets.get("stage") != "3c":
        raise LedgerError("model packets are not stage 3c schema 1")
    if telemetry.get("schemaVersion") != 1 or telemetry.get("stage") != "3c":
        raise LedgerError("model telemetry is not stage 3c schema 1")
    if (
        telemetry.get("modelFree") is not True
        or telemetry.get("invocation") != "not-invoked"
    ):
        raise LedgerError("stage 3c telemetry claims model invocation")
    for field in (
        "calls",
        "promptTokens",
        "completionTokens",
        "totalTokens",
        "actualTotalTokens",
    ):
        value = telemetry.get(field)
        if not isinstance(value, int) or isinstance(value, bool) or value != 0:
            raise LedgerError("stage 3c telemetry must have explicit zero actual usage")
    units: list[str] = []
    projected: list[str] = []
    seen: set[str] = set()
    for key in ("packets", "deferredPackets"):
        entries = packets.get(key, [])
        if not isinstance(entries, list) or len(entries) > MAX_CYCLES:
            raise LedgerError(f"model packets {key} is invalid")
        for entry in entries:
            if not isinstance(entry, dict) or not isinstance(
                entry.get("packetId"), str
            ):
                raise LedgerError("model packet has no bounded packetId")
            unit_id = entry["packetId"]
            if (
                not unit_id
                or len(unit_id) > MAX_UNIT_ID
                or any(character in unit_id for character in "\r\n\x00")
                or unit_id in seen
            ):
                raise LedgerError("model packet IDs must be unique and bounded")
            units.append(unit_id)
            projected.append(unit_id)
            seen.add(unit_id)
    warnings = packets.get("warnings", [])
    if not isinstance(warnings, list):
        raise LedgerError("model packet warnings are invalid")
    return units, projected, [flat(warning, 256) for warning in warnings[:16]]


def ledger_path(path: Path | None) -> Path:
    runner_temp = Path(os.environ.get("RUNNER_TEMP", "/tmp")).resolve()
    selected = (
        (runner_temp / "upstream-convergence-repair-ledger.json")
        if path is None
        else path
    )
    try:
        selected.resolve().relative_to(runner_temp)
    except ValueError as error:
        raise LedgerError("ledger path must be under RUNNER_TEMP") from error
    return selected


def accounting(value: object, label: str) -> dict[str, int]:
    if value is None:
        value = {}
    if not isinstance(value, dict):
        raise LedgerError(f"{label} accounting is invalid")
    result = {}
    aliases = {
        "promptTokens": "actualPromptTokens",
        "completionTokens": "actualCompletionTokens",
        "totalTokens": "actualTotalTokens",
    }
    for name in ("calls", "promptTokens", "completionTokens", "totalTokens"):
        number = value.get(name, value.get(aliases.get(name), 0))
        if (
            name in aliases
            and name in value
            and aliases[name] in value
            and value[name] != value[aliases[name]]
        ):
            raise LedgerError(f"{label} accounting aliases disagree for {name}")
        if not isinstance(number, int) or isinstance(number, bool) or number < 0:
            raise LedgerError(f"{label} accounting has invalid {name}")
        result[name] = number
    if result["totalTokens"] != result["promptTokens"] + result["completionTokens"]:
        raise LedgerError(
            f"{label} accounting total does not equal prompt plus completion"
        )
    return result


def validate_ledger(
    ledger: dict[str, object], refs: dict[str, str], units: set[str], require_live: bool
) -> tuple[str, list[dict[str, object]], dict[str, int]]:
    validate_strings(ledger)
    if ledger.get("schemaVersion") != 1 or ledger.get("stage") != "3d":
        raise LedgerError("repair ledger is not schema 1 stage 3d")
    provenance = ledger.get("provenance")
    if not isinstance(provenance, str) or provenance not in {
        "workflow-executor",
        "synthetic-fixture",
    }:
        raise LedgerError("repair ledger provenance is invalid")
    if require_live and provenance != "workflow-executor":
        raise LedgerError("live checkpoint requires workflow-executor provenance")
    ledger_refs = ledger.get("refs")
    if not isinstance(ledger_refs, dict) or set(ledger_refs) != set(refs):
        raise LedgerError("repair ledger refs do not match candidate evidence")
    for name, ref in refs.items():
        if ledger_refs.get(name) != ref:
            raise LedgerError(
                f"repair ledger refs.{name} does not match candidate evidence"
            )
        exact_ref(ledger_refs.get(name), f"ledger refs.{name}")
    cycles = ledger.get("cycles", [])
    if not isinstance(cycles, list) or len(cycles) > MAX_CYCLES:
        raise LedgerError("repair ledger cycles are invalid or unbounded")
    normalized: list[dict[str, object]] = []
    seen_heads: set[str] = set()
    totals = {
        name: 0 for name in ("calls", "promptTokens", "completionTokens", "totalTokens")
    }
    for index, cycle in enumerate(cycles):
        if not isinstance(cycle, dict):
            raise LedgerError(f"repair cycle {index} is not an object")
        unit_id = cycle.get("repairUnitId")
        if not isinstance(unit_id, str) or unit_id not in units:
            raise LedgerError(f"repair cycle {index} names an unknown unit")
        paths = cycle.get("touchedPaths", [])
        if not isinstance(paths, list) or not paths or len(paths) > MAX_PATHS:
            raise LedgerError(f"repair cycle {index} touchedPaths are invalid")
        clean_paths = []
        for path in paths:
            if (
                not isinstance(path, str)
                or not path
                or len(path) > 512
                or "\n" in path
                or "\r" in path
                or "\x00" in path
            ):
                raise LedgerError(f"repair cycle {index} has an unsafe touched path")
            clean_paths.append(path)
        status = cycle.get("status")
        invocation = cycle.get("invocation")
        not_invoked = status == "not-invoked" or invocation == "not-invoked"
        if (
            not isinstance(status, str)
            or len(status) > 64
            or "\n" in status
            or "\r" in status
        ):
            raise LedgerError(f"repair cycle {index} status is invalid")
        if not_invoked:
            raise LedgerError("not-invoked records are not repair cycles")
        if status not in REPAIR_STATUSES:
            raise LedgerError(f"repair cycle {index} status is not a repair outcome")
        resolved = cycle.get("resolved")
        repair_agent = cycle.get("repairAgent")
        model_tier = cycle.get("modelTier")
        confidence = cycle.get("accountingConfidence")
        started_at, finished_at = cycle.get("startedAt"), cycle.get("finishedAt")
        duration_ms = cycle.get("durationMs")
        if (
            not isinstance(resolved, bool)
            or repair_agent not in REPAIR_AGENTS
            or model_tier not in MODEL_TIERS
        ):
            raise LedgerError(f"repair cycle {index} has invalid repair routing")
        if resolved != (status == "repaired"):
            raise LedgerError(f"repair cycle {index} resolution disagrees with status")
        if confidence not in ACCOUNTING_CONFIDENCE:
            raise LedgerError(f"repair cycle {index} has invalid accounting confidence")
        if not all(
            isinstance(value, str) and value and len(value) <= 128
            for value in (started_at, finished_at)
        ):
            raise LedgerError(f"repair cycle {index} has invalid timing")
        if (
            not isinstance(duration_ms, int)
            or isinstance(duration_ms, bool)
            or duration_ms < 0
        ):
            raise LedgerError(f"repair cycle {index} has invalid duration")
        cycle_accounting_value = cycle.get("accounting")
        if not isinstance(cycle_accounting_value, dict) or not all(
            name in cycle_accounting_value
            for name in ("calls", "promptTokens", "completionTokens", "totalTokens")
        ):
            raise LedgerError(f"repair cycle {index} must carry explicit accounting")
        cycle_accounting = accounting(cycle_accounting_value, f"repair cycle {index}")
        for name, number in cycle_accounting.items():
            totals[name] += number
        head = cycle.get("repairHead")
        if (
            not isinstance(head, str)
            or not SHA_RE.fullmatch(head)
            or head in refs.values()
            or head in seen_heads
        ):
            raise LedgerError("real repair cycles require a changed exact repair head")
        seen_heads.add(head)
        if repair_agent == "model":
            if (
                model_tier == "none"
                or cycle_accounting["calls"] < 1
                or confidence != "provider_reported"
            ):
                raise LedgerError(
                    "model repair cycles require a model tier and reported accounting"
                )
        elif (
            model_tier != "none"
            or any(cycle_accounting.values())
            or confidence != "explicit_zero"
        ):
            raise LedgerError(
                "human and script repair cycles require explicit zero model accounting"
            )
        normalized.append(
            {
                "repairUnitId": unit_id,
                "status": flat(status, 64),
                "repairHead": head,
                "touchedPaths": sorted(set(clean_paths)),
                "resolved": resolved,
                "repairAgent": repair_agent,
                "modelTier": model_tier,
                "accountingConfidence": confidence,
                "startedAt": flat(started_at, 128),
                "finishedAt": flat(finished_at, 128),
                "durationMs": duration_ms,
                "accounting": cycle_accounting,
            }
        )
    declared = accounting(ledger.get("accounting"), "ledger")
    if declared != totals and ledger.get("accounting") is not None:
        raise LedgerError("ledger accounting does not equal cycle accounting")
    claimed_repairs = ledger.get("repairsPerformed")
    if claimed_repairs is not None and (
        not isinstance(claimed_repairs, bool)
        or claimed_repairs != any(cycle["repairHead"] for cycle in normalized)
    ):
        raise LedgerError("repair ledger repairsPerformed claim is inconsistent")
    claimed_head = ledger.get("repairHead")
    if (normalized and claimed_head != normalized[-1]["repairHead"]) or (
        not normalized and claimed_head is not None
    ):
        raise LedgerError("repair ledger repairHead claim is inconsistent")
    return provenance, normalized, totals


def components(cycles: list[dict[str, object]]) -> int:
    parent = list(range(len(cycles)))

    def find(index: int) -> int:
        while parent[index] != index:
            parent[index] = parent[parent[index]]
            index = parent[index]
        return index

    def union(left: int, right: int) -> None:
        left_root, right_root = find(left), find(right)
        if left_root != right_root:
            parent[right_root] = left_root

    for left, first in enumerate(cycles):
        for right in range(left):
            second = cycles[right]
            if first["repairUnitId"] == second["repairUnitId"] or set(
                first["touchedPaths"]
            ) & set(second["touchedPaths"]):
                union(left, right)
    return len({find(index) for index in range(len(cycles))})


def write_text(path: Path, text: str, limit: int | None = None) -> None:
    encoded = text.encode("utf-8")
    if limit is not None and len(encoded) > limit:
        raise LedgerError(f"output exceeds {limit} bytes: {path.name}")
    path.write_bytes(encoded)


def merge_evidence(path: Path, checkpoint: dict[str, object]) -> None:
    if not path.exists():
        return
    evidence = read_object(path, "candidate evidence", MAX_INPUT_BYTES)
    evidence["repairLedger"] = {
        "decision": checkpoint["decision"],
        "handoffReason": checkpoint["handoffReason"],
        "checkpointSha": checkpoint["checkpointSha"],
        "checkpointKind": checkpoint["checkpointKind"],
        "repairsPerformed": checkpoint["repairsPerformed"],
        "unresolvedUnitTotal": checkpoint["unresolvedUnitTotal"],
    }
    evidence["repairCheckpoint"] = checkpoint
    evidence["repairHandoff"] = {
        "decision": checkpoint["decision"],
        "checkpointSha": checkpoint["checkpointSha"],
        "unresolvedUnitTotal": checkpoint["unresolvedUnitTotal"],
    }
    path.write_text(
        json.dumps(evidence, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    text_path = path.with_name("candidate-evidence.txt")
    existing = text_path.read_text(encoding="utf-8") if text_path.exists() else ""
    lines = [
        line
        for line in existing.splitlines()
        if not line.startswith("repair checkpoint:")
    ]
    lines.append(
        f"repair checkpoint: {checkpoint['decision']} at {checkpoint['checkpointSha']}"
    )
    write_text(text_path, "\n".join(lines) + "\n")


def emit(args: argparse.Namespace) -> int:
    output_dir = args.output_dir
    output_dir.mkdir(parents=True, exist_ok=True)
    refs = {"base": "0" * 40, "upstream": "0" * 40, "local": "0" * 40}
    workflow = {"sha": refs["local"], "runId": "unavailable"}
    refs_available = False
    try:
        selected_ledger = ledger_path(args.ledger)
        if (
            not args.evidence.exists()
            or not args.packets.exists()
            or not args.telemetry.exists()
        ):
            input_unavailable = True
            if selected_ledger.exists():
                raise LedgerError(
                    "stage 3c packet input is unavailable for an existing repair ledger"
                )
            evidence = (
                read_object(args.evidence, "candidate evidence", MAX_INPUT_BYTES)
                if args.evidence.exists()
                else {}
            )
            if evidence:
                refs = evidence_refs(evidence)
                workflow = workflow_info(evidence)
                refs_available = True
            units, projected_units, packet_warnings = (
                [],
                [],
                ["stage 3c packet input unavailable"],
            )
            provenance, cycles, totals, present = (
                "unavailable",
                [],
                {
                    name: 0
                    for name in (
                        "calls",
                        "promptTokens",
                        "completionTokens",
                        "totalTokens",
                    )
                },
                False,
            )
        else:
            input_unavailable = False
            evidence = read_object(args.evidence, "candidate evidence", MAX_INPUT_BYTES)
            refs = evidence_refs(evidence)
            workflow = workflow_info(evidence)
            refs_available = True
            units, projected_units, packet_warnings = stage3c_units(
                args.packets, args.telemetry
            )
            if selected_ledger.exists():
                ledger = read_object(selected_ledger, "repair ledger")
                provenance, cycles, totals = validate_ledger(
                    ledger, refs, set(units), args.require_live
                )
                present = True
            else:
                provenance, cycles, totals, present = (
                    "absent",
                    [],
                    {
                        name: 0
                        for name in (
                            "calls",
                            "promptTokens",
                            "completionTokens",
                            "totalTokens",
                        )
                    },
                    False,
                )
        resolved = {cycle["repairUnitId"] for cycle in cycles if cycle["resolved"]}
        unresolved = [unit_id for unit_id in projected_units if unit_id not in resolved]
        component_total = components(cycles)
        attempt_total = len(cycles)
        unique_total = len({cycle["repairUnitId"] for cycle in cycles})
        max_attempts = max(
            (
                sum(cycle["repairUnitId"] == unit_id for cycle in cycles)
                for unit_id in set(cycle["repairUnitId"] for cycle in cycles)
            ),
            default=0,
        )
        if not cycles:
            reason = (
                "stage 3c packet input unavailable"
                if input_unavailable
                else "no repair ledger was present"
            )
            decision, handoff_reason, kind = "no-cycles", "none", "pre-repair"
            checkpoint_sha = refs["local"] if refs_available else None
        elif component_total >= 2:
            decision, handoff_reason, reason, kind = (
                "handoff",
                "unrelated_cycle_cap",
                "two unrelated repair components reached",
                "post-repair",
            )
            checkpoint_sha = cycles[-1]["repairHead"]
        elif max_attempts >= 3:
            decision, handoff_reason, reason, kind = (
                "handoff",
                "attempt_cap",
                "three repair attempts reached for one unit",
                "post-repair",
            )
            checkpoint_sha = cycles[-1]["repairHead"]
        elif totals["totalTokens"] >= TOKEN_BUDGET:
            decision, handoff_reason, reason, kind = (
                "handoff",
                "token_budget_exhausted",
                "repair token budget exceeded",
                "post-repair",
            )
            checkpoint_sha = cycles[-1]["repairHead"]
        else:
            decision, handoff_reason, reason, kind, checkpoint_sha = (
                "continue",
                "none",
                "within bounded repair-cycle caps",
                "post-repair",
                cycles[-1]["repairHead"],
            )
        repairs_performed = bool(cycles)
        confidences = {cycle["accountingConfidence"] for cycle in cycles}
        aggregate_confidence = (
            "unavailable"
            if input_unavailable
            else (
                "provider_reported"
                if "provider_reported" in confidences
                else "explicit_zero"
            )
        )
        attestation = (
            "No repair ran: this read-only workflow has no model credentials or write authority."
            if not repairs_performed
            else "Repair cycles are recorded; this checkpoint helper performed no repair and has no model credentials or write authority."
        )
        warnings = sorted(
            set(packet_warnings + (["repair ledger absent"] if not present else []))
        )
        checkpoint = {
            "schemaVersion": 1,
            "stage": "3d",
            "status": "unavailable" if input_unavailable else "recorded",
            "cycleId": args.cycle_id,
            "refs": refs,
            "workflow": workflow,
            "checkpointSha": checkpoint_sha,
            "checkpointKind": kind,
            "decision": decision,
            "handoffReason": handoff_reason,
            "reason": reason,
            "repairsPerformed": repairs_performed,
            "uniqueUnitTotal": unique_total,
            "unrelatedComponentTotal": component_total,
            "attemptTotal": attempt_total,
            "maxAttemptsPerUnit": max_attempts,
            "caps": {
                "unrelatedComponents": 2,
                "attemptsPerUnit": 3,
                "tokens": TOKEN_BUDGET,
            },
            "accounting": {**totals, "accountingConfidence": aggregate_confidence},
            "unresolvedUnitTotal": len(unresolved),
            "unresolvedUnits": unresolved[:MAX_UNRESOLVED],
            "unresolvedTruncated": len(unresolved) > MAX_UNRESOLVED,
            "warnings": warnings,
            "attestation": attestation,
        }
        ledger_output = {
            "schemaVersion": 1,
            "stage": "3d",
            "cycleId": args.cycle_id,
            "provenance": provenance,
            "refs": refs,
            "workflow": workflow,
            "sourceLedgerPresent": present,
            "cycles": cycles,
            "accounting": checkpoint["accounting"],
            "unresolvedUnitTotal": len(unresolved),
            "unresolvedUnits": unresolved[:MAX_UNRESOLVED],
            "unresolvedTruncated": len(unresolved) > MAX_UNRESOLVED,
            "warnings": warnings,
            "attestation": attestation,
        }
        handoff_lines = [
            "Upstream convergence repair checkpoint",
            f"decision: {decision}",
            f"handoff reason: {handoff_reason}",
            f"reason: {reason}",
            f"checkpoint: {kind} {checkpoint_sha}",
            f"refs: base={refs['base']} upstream={refs['upstream']} local={refs['local']}",
            f"repairs performed: {repairs_performed}",
            f"units: unique={unique_total} unrelated={component_total} attempts={attempt_total}",
            f"accounting: calls={totals['calls']} prompt={totals['promptTokens']} completion={totals['completionTokens']} total={totals['totalTokens']}",
            f"unresolved: {len(unresolved)}"
            + (" (truncated)" if len(unresolved) > MAX_UNRESOLVED else ""),
            "unresolved units: "
            + ", ".join(
                flat(unit_id, MAX_HANDOFF_UNIT_ID)
                for unit_id in unresolved[:MAX_UNRESOLVED]
            ),
            f"attestation: {attestation}",
        ]
        handoff = "\n".join(handoff_lines) + "\n"
        write_text(
            output_dir / "repair-ledger.json",
            json.dumps(ledger_output, indent=2, sort_keys=True) + "\n",
        )
        write_text(
            output_dir / "repair-checkpoint.json",
            json.dumps(checkpoint, indent=2, sort_keys=True) + "\n",
        )
        write_text(output_dir / "repair-handoff.txt", handoff, MAX_HANDOFF_BYTES)
        merge_evidence(args.evidence, checkpoint)
        return 0
    except LedgerError as error:
        failure = {
            "schemaVersion": 1,
            "stage": "3d",
            "classification": "red",
            "decision": "error",
            "reason": flat(error, 512),
            "refs": refs,
            "workflow": workflow,
            "repairsPerformed": False,
            "attestation": "Trusted validation failed; no repair was run by this helper.",
        }
        try:
            write_text(
                output_dir / "repair-ledger.json",
                json.dumps(failure, indent=2, sort_keys=True) + "\n",
            )
            write_text(
                output_dir / "repair-checkpoint.json",
                json.dumps(failure, indent=2, sort_keys=True) + "\n",
            )
            write_text(
                output_dir / "repair-handoff.txt",
                f"Upstream convergence repair checkpoint\ndecision: error\nreason: {flat(error, 512)}\n",
                MAX_HANDOFF_BYTES,
            )
        except OSError:
            pass
        print(flat(error, 512), file=sys.stderr)
        return 1


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    checkpoint = commands.add_parser("checkpoint")
    checkpoint.add_argument("--ledger", type=Path)
    checkpoint.add_argument("--packets", type=Path, required=True)
    checkpoint.add_argument("--telemetry", type=Path, required=True)
    checkpoint.add_argument("--evidence", type=Path, required=True)
    checkpoint.add_argument("--output-dir", type=Path, required=True)
    checkpoint.add_argument("--cycle-id", required=True)
    checkpoint.add_argument("--require-live", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if (
        not args.cycle_id
        or len(args.cycle_id) > 128
        or "\n" in args.cycle_id
        or "\r" in args.cycle_id
    ):
        print("cycle id is invalid", file=sys.stderr)
        return 1
    return emit(args)


if __name__ == "__main__":
    raise SystemExit(main())
