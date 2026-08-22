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


def run_git(*args: str) -> str:
    result = subprocess.run(
        ["git", *args], capture_output=True, text=True, check=True
    )
    return result.stdout.strip()


def parse_timestamp(value: str) -> datetime:
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("timestamp must include a timezone")
    return parsed.astimezone(timezone.utc)


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

    compact = {
        "schemaVersion": 1,
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
