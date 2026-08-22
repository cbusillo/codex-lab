import argparse
import json
import os
import subprocess
import sys
from datetime import datetime, timezone

def run_git(*args: str) -> str:
    result = subprocess.run(['git', *args], capture_output=True, text=True, check=True)
    return result.stdout.strip()

def build_report(report_data: dict, now: datetime) -> tuple[str, bool]:
    refs = report_data.get("refs", {})
    base = refs.get("base")
    upstream = refs.get("upstream")
    local = refs.get("local")
    
    if not (base and upstream and local):
        raise ValueError("Invalid report: missing refs")
        
    lag = int(run_git('rev-list', '--count', f"{base}..{upstream}"))
    base_timestamp_str = run_git('show', '-s', '--format=%cI', base)
    base_time = datetime.fromisoformat(base_timestamp_str)
    hours_since_base = max(0.0, (now - base_time).total_seconds() / 3600.0)
    
    summary = report_data.get("inventory", {}).get("summary", {})
    conflicts = summary.get("conflicts", 0)
    residuals = summary.get("residualLocalInfluence", 0)
    
    lane_counts = report_data.get("inventory", {}).get("laneCounts", {})
    lanes_sum = sum(lane_counts.values()) if lane_counts else 0
    
    LAG_THRESHOLD_COMMITS = 75
    LAG_THRESHOLD_HOURS = 72
    
    is_lag_alert = lag > LAG_THRESHOLD_COMMITS
    is_stale_alert = hours_since_base > LAG_THRESHOLD_HOURS
    
    lines = [
        "### Upstream Convergence Inspection",
        "",
        f"- **Local**: `{local}`",
        f"- **Upstream**: `{upstream}`",
        f"- **Base**: `{base}`",
        "",
        f"- **Lag**: {lag} commits",
        f"- **Age**: {hours_since_base:.1f} hours",
        f"- **Conflicts**: {conflicts}",
        f"- **Residuals**: {residuals}",
        f"- **Lanes**: {lanes_sum}",
        ""
    ]
    
    alert_triggered = is_lag_alert or is_stale_alert
    if alert_triggered:
        lines.append("⚠️ **ALERT**: Convergence candidate is required.")
        if is_lag_alert:
            lines.append(f"  - Upstream lag ({lag} commits) exceeds threshold ({LAG_THRESHOLD_COMMITS} commits).")
            print(f"::warning::Upstream lag ({lag} commits) exceeds threshold ({LAG_THRESHOLD_COMMITS} commits).", file=sys.stderr)
        if is_stale_alert:
            lines.append(f"  - Snapshot age ({hours_since_base:.1f} hours) exceeds threshold ({LAG_THRESHOLD_HOURS} hours).")
            print(f"::warning::Snapshot age ({hours_since_base:.1f} hours) exceeds threshold ({LAG_THRESHOLD_HOURS} hours).", file=sys.stderr)
            
    return "\n".join(lines), alert_triggered

def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--report", required=True)
    args = parser.parse_args(argv)
    
    with open(args.report) as f:
        report_data = json.load(f)
        
    try:
        report_text, alert = build_report(report_data, datetime.now(timezone.utc))
    except (ValueError, subprocess.SubprocessError) as error:
        print(f"Failed to generate report: {error}", file=sys.stderr)
        return 1
        
    summary_file = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary_file:
        with open(summary_file, "a") as f:
            f.write(report_text + "\n")
            
    print(report_text)
    return 0

if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
