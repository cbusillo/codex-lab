import argparse
import json
import os
import subprocess
import sys
from datetime import datetime
from datetime import timezone
from pathlib import Path


DEFAULT_MAX_LAG_COMMITS = 50
DEFAULT_MAX_MERGE_BASE_AGE_HOURS = 24
DEFAULT_MAX_REPORT_AGE_HOURS = 8
DEFAULT_CANDIDATE_MAX_AGE_HOURS = 72
DEFAULT_CANDIDATE_MAX_UPSTREAM_COMMITS = 75
MODEL_ACCOUNTING_CONFIDENCE = "explicit_zero"
INTEGRATION_REF = "refs/remotes/origin/main"
EVIDENCE_ROOT = "upstream/openai-codex"


def run_git(*args: str) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=repo_root(),
        capture_output=True,
        text=True,
        check=True,
    )
    return result.stdout.strip()


def run_git_result(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args], cwd=repo_root(), capture_output=True, text=True
    )


def parse_timestamp(value: str) -> datetime:
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("timestamp must include a timezone")
    return parsed.astimezone(timezone.utc)


def repo_root() -> Path:
    return Path(__file__).resolve().parents[2]


def commit_exists(commit: str) -> bool:
    return run_git_result("cat-file", "-e", f"{commit}^{{commit}}").returncode == 0


def ref_is_ancestor(ancestor: str, descendant: str) -> bool:
    return run_git_result("merge-base", "--is-ancestor", ancestor, descendant).returncode == 0


def snapshot_inventory(path: str, integration_ref: str) -> dict[str, object]:
    document = json.loads(run_git("show", f"{integration_ref}:{path}"))
    if not isinstance(document, dict):
        raise ValueError(f"snapshot inventory must be a JSON object: {path}")
    refs = document.get("refs")
    if not isinstance(refs, dict):
        raise ValueError(f"snapshot inventory missing refs: {path}")
    base = refs.get("base")
    upstream = refs.get("upstream")
    local = refs.get("local")
    if not all(isinstance(ref, str) and ref for ref in (base, upstream, local)):
        raise ValueError(f"snapshot inventory has incomplete refs: {path}")
    recorded_at = run_git(
        "log",
        "--first-parent",
        "--diff-filter=A",
        "--format=%cI",
        integration_ref,
        "--",
        path,
    )
    if not recorded_at:
        raise ValueError(
            f"snapshot is not present in first-parent integration history: {path}"
        )
    return {
        "path": str(Path(path).parent),
        "base": base,
        "upstream": upstream,
        "local": local,
        "recordedAt": parse_timestamp(recorded_at.splitlines()[-1]),
    }


def unique_ancestry_tip(records: list[dict[str, object]]) -> dict[str, object]:
    newest_by_upstream: dict[str, dict[str, object]] = {}
    for record in records:
        upstream = str(record["upstream"])
        existing = newest_by_upstream.get(upstream)
        if existing is None or record["recordedAt"] > existing["recordedAt"]:
            newest_by_upstream[upstream] = record
    records = list(newest_by_upstream.values())
    tips = []
    for record in records:
        upstream = str(record["upstream"])
        if not any(
            upstream != str(other["upstream"]) and ref_is_ancestor(upstream, str(other["upstream"]))
            for other in records
        ):
            tips.append(record)
    if len(tips) != 1:
        raise ValueError("ambiguous snapshot provenance: latest integrated snapshot is not unique")
    return tips[0]


def resolve_latest_integrated_snapshot(
    integration_ref: str = INTEGRATION_REF,
) -> dict[str, object]:
    if not commit_exists(integration_ref):
        raise ValueError(f"integration ref is unavailable: {integration_ref}")
    tracked_paths = run_git(
        "ls-tree", "-r", "--name-only", integration_ref, "--", EVIDENCE_ROOT
    ).splitlines()
    inventory_paths = sorted(
        path
        for path in tracked_paths
        if path.startswith(f"{EVIDENCE_ROOT}/") and path.endswith("/inventory.json")
    )
    if not inventory_paths:
        raise ValueError(f"no convergence snapshots found in {integration_ref}")
    records = [snapshot_inventory(path, integration_ref) for path in inventory_paths]
    for record in records:
        upstream = str(record["upstream"])
        if not commit_exists(upstream):
            raise ValueError(f"recorded upstream is unavailable: {upstream}")
    tip = unique_ancestry_tip(records)
    return {
        "snapshot": str(tip["path"]),
        "refs": {
            "base": str(tip["base"]),
            "upstream": str(tip["upstream"]),
            "local": str(tip["local"]),
        },
        "recordedAt": tip["recordedAt"].isoformat().replace("+00:00", "Z"),
    }


def collect_cycle_telemetry(now: datetime) -> dict[str, object]:
    run_id = os.environ.get("GITHUB_RUN_ID")
    run_attempt = os.environ.get("GITHUB_RUN_ATTEMPT")
    event_name = os.environ.get("GITHUB_EVENT_NAME")
    workflow_name = os.environ.get("GITHUB_WORKFLOW")
    sha = os.environ.get("GITHUB_SHA")
    ref_name = os.environ.get("GITHUB_REF_NAME")
    started_at = os.environ.get("INSPECTION_STARTED_AT")
    finished_at = os.environ.get("INSPECTION_FINISHED_AT")
    duration_ms_raw = os.environ.get("INSPECTION_DURATION_MS")
    missing = [
        name
        for name, value in (
            ("GITHUB_RUN_ID", run_id),
            ("GITHUB_RUN_ATTEMPT", run_attempt),
            ("GITHUB_EVENT_NAME", event_name),
            ("GITHUB_WORKFLOW", workflow_name),
            ("GITHUB_SHA", sha),
            ("GITHUB_REF_NAME", ref_name),
            ("INSPECTION_STARTED_AT", started_at),
            ("INSPECTION_FINISHED_AT", finished_at),
            ("INSPECTION_DURATION_MS", duration_ms_raw),
        )
        if not value
    ]
    if missing:
        raise ValueError(
            f"missing inspection telemetry: {', '.join(missing)}"
        )
    run_attempt_number = int(run_attempt)
    if run_attempt_number < 1:
        raise ValueError("run attempt must be positive")
    duration_ms = int(duration_ms_raw)
    if duration_ms < 0:
        raise ValueError("inspection duration must be non-negative")
    started = parse_timestamp(started_at)
    finished = parse_timestamp(finished_at)
    if finished < started:
        raise ValueError("inspection finish must not precede inspection start")
    cycle_identity = f"{run_id}:{run_attempt_number}:{event_name}"
    return {
        "run": {
            "workflow": workflow_name,
            "runId": run_id,
            "runAttempt": run_attempt_number,
            "eventName": event_name,
            "sha": sha,
            "refName": ref_name,
            "cycleId": cycle_identity,
        },
        "inspection": {
            "startedAt": started_at,
            "finishedAt": finished_at,
            "reportGeneratedAt": now.isoformat().replace("+00:00", "Z"),
            "durationMs": duration_ms,
        },
        "modelTelemetry": {
            "calls": 0,
            "promptTokens": 0,
            "completionTokens": 0,
            "totalTokens": 0,
            "accountingConfidence": MODEL_ACCOUNTING_CONFIDENCE,
            "modelFree": True,
        },
    }


def build_candidate_gate(
    upstream: str,
    now: datetime,
    max_age_hours: int = DEFAULT_CANDIDATE_MAX_AGE_HOURS,
    max_upstream_commits: int = DEFAULT_CANDIDATE_MAX_UPSTREAM_COMMITS,
) -> dict[str, object]:
    latest_snapshot = resolve_latest_integrated_snapshot()
    snapshot_upstream = latest_snapshot["refs"]["upstream"]
    if not commit_exists(snapshot_upstream):
        raise ValueError(f"latest snapshot upstream is unavailable: {snapshot_upstream}")
    if not commit_exists(upstream):
        raise ValueError(f"current upstream is unavailable: {upstream}")
    if not ref_is_ancestor(snapshot_upstream, upstream):
        raise ValueError(
            "latest integrated snapshot is not an ancestor of current upstream"
        )
    upstream_commits = int(run_git("rev-list", "--count", f"{snapshot_upstream}..{upstream}"))
    recorded_at = parse_timestamp(str(latest_snapshot["recordedAt"]))
    age_hours = max(0.0, (now - recorded_at).total_seconds() / 3600.0)
    reasons = []
    if age_hours >= max_age_hours:
        reasons.append(
            f"latest integrated snapshot age {age_hours:.1f} hours meets {max_age_hours}-hour candidate threshold"
        )
    if upstream_commits >= max_upstream_commits:
        reasons.append(
            f"upstream lag {upstream_commits} commits meets {max_upstream_commits}-commit candidate threshold"
        )
    return {
        "status": "ready",
        "due": bool(reasons),
        "thresholds": {
            "maxAgeHours": max_age_hours,
            "maxUpstreamCommits": max_upstream_commits,
        },
        "latestIntegratedSnapshot": {
            **latest_snapshot,
            "ageHours": round(age_hours, 1),
            "upstreamCommits": upstream_commits,
        },
        "reasons": reasons,
    }


def candidate_gate_error(error: Exception) -> dict[str, object]:
    message = str(error)[:1000]
    return {
        "status": "error",
        "due": True,
        "thresholds": {
            "maxAgeHours": DEFAULT_CANDIDATE_MAX_AGE_HOURS,
            "maxUpstreamCommits": DEFAULT_CANDIDATE_MAX_UPSTREAM_COMMITS,
        },
        "latestIntegratedSnapshot": None,
        "reasons": ["candidate gate provenance is unavailable"],
        "error": message,
    }


def build_report(
    report_data: dict[str, object],
    now: datetime,
    previous_success_at: datetime | None,
    max_lag_commits: int,
    max_merge_base_age_hours: int,
    max_report_age_hours: int,
) -> tuple[dict[str, object], str]:
    refs = report_data.get("refs")
    if not isinstance(refs, dict):
        raise ValueError("invalid report: missing refs")
    base = refs.get("base")
    upstream = refs.get("upstream")
    local = refs.get("local")
    if not all(isinstance(ref, str) and ref for ref in (base, upstream, local)):
        raise ValueError("invalid report: incomplete refs")

    lag = int(run_git("rev-list", "--count", f"{base}..{upstream}"))
    base_time = parse_timestamp(run_git("show", "-s", "--format=%cI", base))
    merge_base_age_hours = max(0.0, (now - base_time).total_seconds() / 3600.0)
    previous_report_age_hours = (
        None
        if previous_success_at is None
        else max(0.0, (now - previous_success_at).total_seconds() / 3600.0)
    )

    inventory = report_data.get("inventory")
    inventory = inventory if isinstance(inventory, dict) else {}
    inventory_summary = inventory.get("summary")
    inventory_summary = inventory_summary if isinstance(inventory_summary, dict) else {}
    lane_counts = inventory.get("laneCounts")
    lane_counts = lane_counts if isinstance(lane_counts, dict) else {}
    normalized_lane_counts = {
        str(lane): int(count) for lane, count in sorted(lane_counts.items())
    }
    try:
        candidate_gate = build_candidate_gate(upstream, now)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        candidate_gate = candidate_gate_error(error)
    cycle_telemetry = collect_cycle_telemetry(now)

    alerts = []
    if lag > max_lag_commits:
        alerts.append(
            f"upstream lag {lag} commits exceeds {max_lag_commits}-commit threshold"
        )
    if merge_base_age_hours > max_merge_base_age_hours:
        alerts.append(
            "merge-base age "
            f"{merge_base_age_hours:.1f} hours exceeds {max_merge_base_age_hours}-hour threshold"
        )
    if previous_report_age_hours is None:
        alerts.append("no previous successful convergence report was found")
    elif previous_report_age_hours > max_report_age_hours:
        alerts.append(
            "previous successful report age "
            f"{previous_report_age_hours:.1f} hours exceeds {max_report_age_hours}-hour threshold"
        )
    if candidate_gate["due"]:
        alerts.extend(f"candidate due: {reason}" for reason in candidate_gate["reasons"])
    if candidate_gate["status"] == "error":
        alerts.append(f"candidate gate error: {candidate_gate['error']}")

    compact = {
        "schemaVersion": 2,
        "generatedAt": now.isoformat().replace("+00:00", "Z"),
        "refs": {"base": base, "upstream": upstream, "local": local},
        "counts": {
            "upstreamLagCommits": lag,
            "conflicts": int(inventory_summary.get("conflicts", 0)),
            "residualLocalInfluence": int(
                inventory_summary.get("residualLocalInfluence", 0)
            ),
            "laneCounts": normalized_lane_counts,
        },
        "agesHours": {
            "mergeBase": round(merge_base_age_hours, 1),
            "previousSuccessfulReport": (
                None
                if previous_report_age_hours is None
                else round(previous_report_age_hours, 1)
            ),
        },
        "thresholds": {
            "maxLagCommits": max_lag_commits,
            "maxMergeBaseAgeHours": max_merge_base_age_hours,
            "maxReportAgeHours": max_report_age_hours,
        },
        "candidateGate": candidate_gate,
        "cycleTelemetry": cycle_telemetry,
        "alerts": alerts,
    }
    lines = [
        "### Upstream Convergence Inspection",
        "",
        f"- **Local**: `{local}`",
        f"- **Upstream**: `{upstream}`",
        f"- **Merge base**: `{base}`",
        f"- **Upstream lag**: {lag} commits",
        f"- **Merge-base age**: {merge_base_age_hours:.1f} hours",
        f"- **Conflicts**: {compact['counts']['conflicts']}",
        f"- **Residuals**: {compact['counts']['residualLocalInfluence']}",
        f"- **Lane counts**: `{json.dumps(normalized_lane_counts, sort_keys=True)}`",
    ]
    if previous_report_age_hours is not None:
        lines.append(
            f"- **Previous successful report age**: {previous_report_age_hours:.1f} hours"
        )
    gate = candidate_gate
    latest_snapshot = gate["latestIntegratedSnapshot"]
    lines.extend(["", "### Candidate Gate", f"- **Status**: `{gate['status']}`"])
    lines.append(f"- **Due**: {'yes' if gate['due'] else 'no'}")
    if isinstance(latest_snapshot, dict):
        lines.extend(
            [
                f"- **Latest snapshot**: `{latest_snapshot['snapshot']}`",
                f"- **Snapshot recorded at**: {latest_snapshot['recordedAt']}",
                f"- **Snapshot age**: {latest_snapshot['ageHours']:.1f} hours",
                f"- **Upstream commits since snapshot**: {latest_snapshot['upstreamCommits']}",
            ]
        )
    else:
        lines.append(f"- **Error**: {gate['error']}")
    lines.append(
        f"- **Candidate thresholds**: {gate['thresholds']['maxAgeHours']} hours / {gate['thresholds']['maxUpstreamCommits']} commits"
    )
    if gate["reasons"]:
        lines.extend(["", *[f"- {reason}" for reason in gate["reasons"]]])
    telemetry = cycle_telemetry
    run = telemetry["run"]
    inspection = telemetry["inspection"]
    model = telemetry["modelTelemetry"]
    lines.extend(
        [
            "",
            "### Cycle Telemetry",
            f"- **Run**: `{run['cycleId']}`",
            f"- **Workflow**: `{run['workflow']}`",
            f"- **Event**: `{run['eventName']}`",
            f"- **Run ID / attempt**: `{run['runId']}` / `{run['runAttempt']}`",
            f"- **Inspection started**: {inspection['startedAt']}",
            f"- **Inspection duration**: {inspection['durationMs']} ms",
            f"- **Model calls**: {model['calls']}",
            f"- **Token usage**: prompt {model['promptTokens']}, completion {model['completionTokens']}, total {model['totalTokens']}",
            f"- **Token accounting confidence**: `{model['accountingConfidence']}`",
        ]
    )
    if alerts:
        lines.extend(["", "⚠️ **Alerts**", *[f"- {alert}" for alert in alerts]])
    return compact, "\n".join(lines)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--report", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--previous-success-at")
    parser.add_argument("--max-lag-commits", type=int, default=DEFAULT_MAX_LAG_COMMITS)
    parser.add_argument(
        "--max-merge-base-age-hours",
        type=int,
        default=DEFAULT_MAX_MERGE_BASE_AGE_HOURS,
    )
    parser.add_argument(
        "--max-report-age-hours", type=int, default=DEFAULT_MAX_REPORT_AGE_HOURS
    )
    args = parser.parse_args(argv)

    try:
        report_data = json.loads(Path(args.report).read_text(encoding="utf-8"))
        if not isinstance(report_data, dict):
            raise ValueError("report must be a JSON object")
        previous_success_at = (
            parse_timestamp(args.previous_success_at)
            if args.previous_success_at
            else None
        )
        compact, report_text = build_report(
            report_data,
            datetime.now(timezone.utc),
            previous_success_at,
            args.max_lag_commits,
            args.max_merge_base_age_hours,
            args.max_report_age_hours,
        )
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"Failed to generate report: {error}", file=sys.stderr)
        return 1

    Path(args.output).write_text(
        json.dumps(compact, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    summary_file = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary_file:
        with Path(summary_file).open("a", encoding="utf-8") as summary:
            summary.write(report_text + "\n")
    for alert in compact["alerts"]:
        print(f"::warning::{alert}", file=sys.stderr)
    print(report_text)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
