#!/usr/bin/env python3

import argparse
import fnmatch
import json
import re
import subprocess
from collections import Counter
from dataclasses import dataclass
from pathlib import Path


LANE_PRIORITY = {
    "green_bulk_adopt": 0,
    "amber_contract_adapt": 1,
    "intentionally_owned": 2,
    "red_manual_review": 3,
}

SCHEMA_VERSION = 2
GUARD_SCHEMA_VERSION = 1

# Lanes whose local content may not silently disappear or silently revert to the
# upstream blob during a refresh. `upstream_convergence_guard.py` enforces this.
GUARDED_LANES = ("intentionally_owned", "red_manual_review")


@dataclass(frozen=True)
class Rule:
    patterns: tuple[str, ...]
    lane: str
    contracts: tuple[str, ...]
    reason: str


# Owned features follow a repo-wide naming convention: the implementation modules,
# their `*_tests.rs` siblings, and the integration proofs that pin them all share a
# filename stem. Deriving patterns from that stem keeps implementation and proof
# coverage in lockstep, so a refresh cannot drop the proof while keeping the code
# (or keep the code while quietly unregistering the proof).
IMPLEMENTATION_ROOTS = (
    "codex-rs/core/src/agent",
    "codex-rs/core/src/context",
    "codex-rs/core/src/session",
    "codex-rs/core/src/tools/handlers",
    "codex-rs/app-server/src/request_processors",
    "codex-rs/app-server-protocol/src/protocol/v2",
    "codex-rs/tui/src/history_cell",
)

# Integration-proof roots. These carry the executable evidence for owned behavior,
# which is exactly what an upstream-first merge deletes without a conflict marker.
PROOF_ROOTS = (
    "codex-rs/core/tests/suite",
    "codex-rs/exec/tests/suite",
    "codex-rs/app-server/tests/suite",
    "codex-rs/app-server/tests/suite/v2",
)


def feature_paths(*stems: str) -> tuple[str, ...]:
    """Conventional implementation and proof patterns for owned feature stems.

    A stem must be specific enough that no upstream-owned module shares the
    prefix; `project_validation` is a feature, `validation` is not.
    """

    return tuple(
        f"{root}/{stem}*"
        for stem in stems
        for root in (*IMPLEMENTATION_ROOTS, *PROOF_ROOTS)
    )


# Shared upstream files that carry owned deltas. They exist upstream too, so the
# stem convention cannot reach them, and a wholesale revert to the upstream blob
# would silently drop owned coverage. Guarding them still only forbids deletion
# and byte-identical reversion, never ordinary upstream edits.
SHARED_PROOF_REGISTRIES = (
    # Registers every owned `core` suite module. Reverting this file unregisters
    # the owned proofs while leaving their files in the tree, so the suites stop
    # running without any path going missing.
    "codex-rs/core/tests/suite/mod.rs",
    "codex-rs/exec/tests/suite/mod.rs",
    "codex-rs/app-server/tests/suite/mod.rs",
    # Every Every Code-owned app-server proof is a v2 suite, so this nested
    # registry -- not the crate-level one above -- is the file that actually
    # registers Code Bridge, remote control, Background Review control, and
    # Project Validation coverage.
    "codex-rs/app-server/tests/suite/v2/mod.rs",
)


RULES = (
    Rule(
        patterns=("AGENTS.md",),
        lane="intentionally_owned",
        contracts=("GOVERNANCE-1",),
        reason="Codex Lab planning and convergence authority",
    ),
    Rule(
        patterns=(
            "codex-rs/codex-home/**",
            "codex-rs/utils/home-dir/**",
        ),
        lane="red_manual_review",
        contracts=("HOME-1",),
        reason="state-home migration boundary",
    ),
    Rule(
        patterns=(
            "README.md",
            "codex-cli/package.json",
            "codex-rs/cli/src/main.rs",
            "codex-rs/cli/src/login.rs",
            "codex-rs/tui/src/app.rs",
            "codex-rs/tui/src/lib.rs",
            "codex-rs/tui/src/status/**",
        ),
        lane="red_manual_review",
        contracts=("IDENTITY-1",),
        reason="visible or executable product identity",
    ),
    Rule(
        patterns=(
            "codex-rs/model-provider-info/**",
            "codex-rs/models-manager/**",
            "codex-rs/core/src/models_manager/**",
            "codex-rs/tui/src/model*",
            "codex-rs/tui/src/bottom_pane/model*",
        ),
        lane="red_manual_review",
        contracts=("MODEL-1",),
        reason="model catalog, default, or selection UX",
    ),
    Rule(
        patterns=(
            "codex-rs/login/**",
            "codex-rs/secrets/**",
            "codex-rs/core/src/account_switching*",
            "codex-rs/core/src/account_usage*",
            "codex-rs/tui/src/account_label*",
            "codex-rs/tui/src/bottom_pane/*account*",
            "codex-rs/app-server/tests/suite/auth.rs",
        ),
        lane="amber_contract_adapt",
        contracts=("AUTH-1", "AUTH-2", "AUTH-3"),
        reason="credential persistence and account selection",
    ),
    Rule(
        patterns=(
            "codex-rs/state/**",
            "codex-rs/thread-store/**",
            "codex-rs/app-server-protocol/src/protocol/thread_history*",
            "codex-rs/core/tests/suite/sqlite_state.rs",
        ),
        lane="amber_contract_adapt",
        contracts=("HISTORY-1",),
        reason="durable history and resume semantics",
    ),
    Rule(
        patterns=(
            "codex-rs/app-server-protocol/**",
            "codex-rs/app-server/**",
            "codex-rs/app-server-client/**",
            "codex-rs/app-server-test-client/**",
            "codex-rs/protocol/**",
        ),
        lane="amber_contract_adapt",
        contracts=("PROTOCOL-1",),
        reason="app-server and wire compatibility",
    ),
    Rule(
        patterns=(
            "codex-rs/bwrap/**",
            "codex-rs/execpolicy/**",
            "codex-rs/linux-sandbox/**",
            "codex-rs/sandboxing/**",
            "codex-rs/shell-escalation/**",
            "codex-rs/windows-sandbox-rs/**",
            "codex-rs/core/tests/suite/approvals.rs",
            "codex-rs/core/tests/suite/skill_approval.rs",
        ),
        lane="amber_contract_adapt",
        contracts=("SANDBOX-1",),
        reason="approval and sandbox policy",
    ),
    Rule(
        patterns=(
            "codex-rs/auto-review/**",
            "codex-rs/external-agent-migration/**",
            "codex-rs/external-agent-sessions/**",
            "codex-rs/core/src/agent/**",
            "codex-rs/core/src/review_persistence.rs",
            # Implementation and integration proofs for owned orchestration and
            # review behavior, including the explicit external-agent preflight and
            # provider-routing evidence and the Background Review suites.
            *feature_paths(
                "agent_jobs",
                "auto_review",
                "background_auto_review",
                "background_review",
                "external_agent",
                "external_preflight",
                "guardian_review",
                "multi_agent",
                "provider_routing",
                "session_provenance",
                "spawn_agent",
                "subagent",
            ),
            # Background Review status replay and its summary claiming live in
            # this shared TUI routing module and its inline test module, which
            # upstream also owns, so the stem convention cannot reach them.
            "codex-rs/tui/src/app/thread_routing.rs",
            "codex-rs/tui/src/app/test_support.rs",
        ),
        lane="intentionally_owned",
        contracts=("AGENT-1",),
        reason="Every Code orchestration and review behavior",
    ),
    Rule(
        patterns=(
            "codex-rs/browser/**",
            "codex-rs/code-bridge-*/**",
            # Model-facing bridge and browser handlers plus their integration
            # proofs, including the app-server Code Bridge and remote-control
            # suites.
            *feature_paths("browser", "code_bridge", "remote_control"),
            # The three named model-facing Code Bridge proofs live in the shared
            # upstream tool suite, so the stem convention cannot reach them.
            "codex-rs/core/tests/suite/tools.rs",
        ),
        lane="intentionally_owned",
        contracts=("INTEGRATION-1",),
        reason="Code Bridge, browser, and remote control",
    ),
    Rule(
        patterns=(
            # Project Validation is Every Code-owned: no upstream module carries
            # any of these stems.
            *feature_paths(
                "cargo_validation_provider",
                "project_validation",
                "validation_provider",
            ),
            "codex-rs/app-server-protocol/src/protocol/v2/validation*",
            "codex-rs/app-server-protocol/schema/json/v2/ProjectValidation*",
            "codex-rs/app-server-protocol/schema/typescript/v2/ProjectValidation*",
        ),
        lane="intentionally_owned",
        contracts=("VALIDATION-1",),
        reason="Project Validation providers, status, and failure feedback",
    ),
    Rule(
        patterns=(
            # Model-visible context safety: every agent-authored or tool-authored
            # string that reaches history is bounded, and the one narrow history
            # rewrite (dropping an image the Responses API cannot read) is
            # checkpointed instead of replayed. Upstream owns these filenames, so
            # the stem convention cannot reach them.
            *feature_paths("token_budget_context"),
            "codex-rs/core/src/context_manager/history.rs",
            "codex-rs/core/src/context_manager/history_tests.rs",
            "codex-rs/core/src/session_prefix.rs",
            "codex-rs/core/src/session_prefix_tests.rs",
            "codex-rs/core/src/session/turn.rs",
            "codex-rs/core/tests/suite/view_image.rs",
        ),
        lane="intentionally_owned",
        contracts=("CONTEXT-1",),
        reason="model-visible context bounds and history-rewrite exceptions",
    ),
    Rule(
        patterns=(
            # Hook handler ids anchor the persisted enable/disable and
            # `trusted_hash` state, and `hooks.json` tolerates extension keys
            # while still rejecting misplaced event tables. Both are Every
            # Code-only behavior inside shared upstream modules.
            "codex-rs/config/src/hook_config.rs",
            "codex-rs/config/src/hooks_tests.rs",
            "codex-rs/hooks/src/declarations.rs",
            "codex-rs/hooks/src/engine/discovery.rs",
            "codex-rs/hooks/src/engine/mod_tests.rs",
            "codex-rs/hooks/src/lib.rs",
        ),
        lane="intentionally_owned",
        contracts=("HOOKS-1",),
        reason="hook identity and persisted hook state",
    ),
    Rule(
        patterns=(
            # The durable environment baseline: the turn-context writer, the
            # world-state reader that rebuilds from it, and the reconstruction
            # entry point, plus their proofs.
            *feature_paths("rollout_reconstruction", "turn_context_environments"),
            "codex-rs/core/src/session/turn_context.rs",
            "codex-rs/core/src/context/world_state/environment.rs",
            "codex-rs/core/src/context/world_state/mod.rs",
        ),
        lane="intentionally_owned",
        contracts=("HISTORY-1",),
        reason="durable environment baseline across resume and fork",
    ),
    Rule(
        patterns=(
            # `--auth-profile` has no upstream counterpart, and
            # `--workspace-root` only accepts the workspace-write sandbox here.
            *feature_paths("shared_cli_options"),
            "codex-rs/utils/cli/src/shared_options.rs",
        ),
        lane="intentionally_owned",
        contracts=("AUTH-1", "SANDBOX-1"),
        reason="Every Code shared CLI options for auth profiles and workspace roots",
    ),
    Rule(
        patterns=SHARED_PROOF_REGISTRIES,
        lane="intentionally_owned",
        contracts=("AGENT-1", "INTEGRATION-1", "VALIDATION-1"),
        reason="registration point for owned integration proofs",
    ),
    Rule(
        patterns=(
            ".github/workflows/codex-lab-*",
            "codex-rs/cli/src/bin/codex-lab.rs",
            "codex-rs/version/**",
            "scripts/codex_lab_package/**",
            "scripts/*codex_lab*",
        ),
        lane="intentionally_owned",
        contracts=("RELEASE-1",),
        reason="Every Code distribution authority",
    ),
    Rule(
        patterns=("tools/codex-exec-harness/**",),
        lane="intentionally_owned",
        contracts=("AGENT-1", "INTEGRATION-1", "VALIDATION-1"),
        reason="executable proof harness for owned product contracts",
    ),
)


def run_git(repo: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        check=check,
        capture_output=True,
        text=True,
    )


def resolve_commit(repo: Path, ref: str) -> str:
    return run_git(repo, "rev-parse", f"{ref}^{{commit}}").stdout.strip()


def changed_paths(repo: Path, base: str, tip: str) -> set[str]:
    result = run_git(
        repo,
        "diff",
        "--name-only",
        "--no-renames",
        f"{base}..{tip}",
    )
    return {path for path in result.stdout.splitlines() if path}


def tree_objects(repo: Path, ref: str) -> dict[str, str]:
    result = subprocess.run(
        ["git", "-C", str(repo), "ls-tree", "-r", "-z", ref],
        check=True,
        capture_output=True,
    )
    objects: dict[str, str] = {}
    for record in result.stdout.split(b"\0"):
        if not record:
            continue
        metadata, raw_path = record.split(b"\t", 1)
        object_id = metadata.split()[2].decode()
        objects[raw_path.decode()] = object_id
    return objects


def parse_conflict_message(line: str) -> tuple[str, str]:
    match = re.fullmatch(r"CONFLICT \(([^)]+)\): (.+)", line)
    if match is None:
        raise ValueError(f"not a conflict message: {line}")
    conflict_type, detail = match.groups()
    prefix = "Merge conflict in "
    if detail.startswith(prefix):
        return conflict_type, detail.removeprefix(prefix)
    deleted_marker = " deleted in "
    if deleted_marker in detail:
        return conflict_type, detail.split(deleted_marker, 1)[0]
    raise ValueError(f"unsupported conflict message: {line}")


def merge_conflicts(
    repo: Path, upstream: str, local: str
) -> tuple[str, list[dict[str, object]]]:
    result = run_git(
        repo,
        "merge-tree",
        "--write-tree",
        "--messages",
        upstream,
        local,
        check=False,
    )
    if result.returncode not in (0, 1):
        raise RuntimeError(result.stderr.strip() or result.stdout.strip())
    output_lines = result.stdout.splitlines()
    if not output_lines:
        raise ValueError("merge-tree did not report a result tree")
    result_tree = output_lines[0]
    conflicts: dict[str, str] = {}
    for line in output_lines[1:]:
        if not line.startswith("CONFLICT ("):
            continue
        conflict_type, path = parse_conflict_message(line)
        if previous := conflicts.get(path):
            raise ValueError(
                f"duplicate conflict path {path}: {previous} and {conflict_type}"
            )
        conflicts[path] = conflict_type
    classified = [classify(path, conflicts[path]) for path in sorted(conflicts)]
    return result_tree, classified


def classify_path(path: str) -> dict[str, object]:
    lane = "green_bulk_adopt"
    contracts: set[str] = set()
    reasons: list[str] = []
    for rule in RULES:
        if not any(fnmatch.fnmatchcase(path, pattern) for pattern in rule.patterns):
            continue
        contracts.update(rule.contracts)
        if rule.reason not in reasons:
            reasons.append(rule.reason)
        if LANE_PRIORITY[rule.lane] > LANE_PRIORITY[lane]:
            lane = rule.lane
    if not reasons:
        reasons.append("upstream-owned surface with no named local contract")
    return {
        "path": path,
        "lane": lane,
        "contracts": sorted(contracts),
        "reason": "; ".join(reasons),
    }


def classify(path: str, conflict_type: str) -> dict[str, object]:
    classified = classify_path(path)
    return {
        "path": classified["path"],
        "conflictType": conflict_type,
        "lane": classified["lane"],
        "contracts": classified["contracts"],
        "reason": classified["reason"],
    }


def build_inventory(repo: Path, base_ref: str, upstream_ref: str, local_ref: str) -> dict[str, object]:
    base = resolve_commit(repo, base_ref)
    upstream = resolve_commit(repo, upstream_ref)
    local = resolve_commit(repo, local_ref)
    actual_base = run_git(repo, "merge-base", upstream, local).stdout.strip()
    if actual_base != base:
        raise ValueError(f"expected merge base {base}, found {actual_base}")

    result_tree, conflicts = merge_conflicts(repo, upstream, local)
    conflict_paths = {entry["path"] for entry in conflicts}
    local_paths = changed_paths(repo, base, local)
    upstream_paths = changed_paths(repo, base, upstream)
    shared_paths = local_paths & upstream_paths
    upstream_objects = tree_objects(repo, upstream)
    local_objects = tree_objects(repo, local)
    result_objects = tree_objects(repo, result_tree)
    identical_paths = {
        path
        for path in shared_paths
        if upstream_objects.get(path) == local_objects.get(path)
    }
    mergeable_divergent_paths = shared_paths - conflict_paths - identical_paths
    # Paths where the non-conflicting merge result keeps local content instead of
    # the upstream blob. The merge *retains* this local influence silently; it
    # does not reject it, which is why every path here needs a contract lane.
    residual_paths = sorted(
        path
        for path in local_paths - conflict_paths
        if result_objects.get(path) != upstream_objects.get(path)
    )
    residuals = [classify_path(path) for path in residual_paths]

    return {
        "schemaVersion": SCHEMA_VERSION,
        "repository": "openai/codex",
        "refs": {
            "base": base,
            "upstream": upstream,
            "local": local,
        },
        "policy": {
            "defaultLane": "green_bulk_adopt",
            "rule": "Upstream wins unless a named convergence contract applies.",
        },
        "summary": {
            "conflicts": len(conflicts),
            "localChangedOnly": len(local_paths - upstream_paths),
            "sharedIdentical": len(identical_paths),
            "sharedMergeableDivergent": len(mergeable_divergent_paths),
            "residualLocalInfluence": len(residuals),
        },
        "conflictTypeCounts": dict(
            sorted(Counter(entry["conflictType"] for entry in conflicts).items())
        ),
        "laneCounts": dict(sorted(Counter(entry["lane"] for entry in conflicts).items())),
        "residualLaneCounts": dict(
            sorted(Counter(entry["lane"] for entry in residuals).items())
        ),
        "conflicts": conflicts,
        "residuals": residuals,
    }


def render_records(header: dict[str, object], key: str, records: list[object]) -> str:
    lines = ["{"]
    for header_key, value in header.items():
        lines.append(f"  {json.dumps(header_key)}: {json.dumps(value, sort_keys=True)},")
    lines.append(f"  {json.dumps(key)}: [")
    for index, record in enumerate(records):
        comma = "," if index + 1 < len(records) else ""
        lines.append(f"    {json.dumps(record, sort_keys=True)}{comma}")
    lines.extend(("  ]", "}"))
    return "\n".join(lines) + "\n"


def render_json(inventory: dict[str, object]) -> str:
    header = {
        key: value
        for key, value in inventory.items()
        if key not in ("conflicts", "residuals")
    }
    return render_records(header, "conflicts", inventory["conflicts"])


def render_residuals(inventory: dict[str, object]) -> str:
    """Machine-readable list of paths a refresh would silently keep from local."""

    header = {
        "schemaVersion": inventory["schemaVersion"],
        "repository": inventory["repository"],
        "refs": inventory["refs"],
        "policy": {
            "rule": (
                "A residual path is a non-conflicting path whose merge result "
                "differs from upstream, so local content survives without review."
            ),
        },
        "summary": {"residualLocalInfluence": inventory["summary"]["residualLocalInfluence"]},
        "residualLaneCounts": inventory["residualLaneCounts"],
    }
    return render_records(header, "residuals", inventory["residuals"])


def render_markdown(inventory: dict[str, object]) -> str:
    refs = inventory["refs"]
    summary = inventory["summary"]
    lane_counts = inventory["laneCounts"]
    conflict_type_counts = inventory["conflictTypeCounts"]
    lines = [
        "# Upstream convergence inventory",
        "",
        f"- Merge base: `{refs['base']}`",
        f"- Upstream snapshot: `{refs['upstream']}`",
        f"- Local baseline: `{refs['local']}`",
        f"- Conflicts: {summary['conflicts']}",
        f"- Residual local-influence paths retained by an upstream-first merge: {summary['residualLocalInfluence']}",
        "",
        "Residual paths merge cleanly, so no reviewer sees them. The merge keeps",
        "local content there instead of upstream content; it does not reject it.",
        "`residuals.json` lists every one with its contract lane.",
        "",
        "## Counts",
        "",
        "| Dimension | Value |",
        "| --- | ---: |",
    ]
    for key, value in conflict_type_counts.items():
        lines.append(f"| Conflict `{key}` | {value} |")
    for key, value in lane_counts.items():
        lines.append(f"| Lane `{key}` | {value} |")
    for key, value in inventory["residualLaneCounts"].items():
        lines.append(f"| Residual lane `{key}` | {value} |")
    lines.extend(
        (
            "",
            "## Contract-reviewed conflicts",
            "",
            "Green paths are intentionally omitted from this table because the candidate",
            "takes upstream unchanged. The JSON companion records every conflict path.",
            "",
            "| Lane | Contracts | Path | Reason |",
            "| --- | --- | --- | --- |",
        )
    )
    for conflict in inventory["conflicts"]:
        if conflict["lane"] == "green_bulk_adopt":
            continue
        contracts = ", ".join(f"`{item}`" for item in conflict["contracts"])
        lines.append(
            f"| `{conflict['lane']}` | {contracts} | `{conflict['path']}` | {conflict['reason']} |"
        )
    return "\n".join(lines) + "\n"


def build_guard_manifest(
    repo: Path,
    base_ref: str,
    upstream_ref: str,
    local_ref: str,
    current_ref: str,
) -> dict[str, object]:
    """Record the owned paths a later refresh must not silently drop or revert.

    Two sources contribute, because either one alone leaves a hole:

    `ownership_baseline` covers owned paths that already differed from upstream at
    the pre-anchor local baseline. A path byte-identical to upstream there had no
    local delta to lose. This source must stay pinned to the pre-anchor baseline;
    recomputing it from the candidate would bake the anchor's losses into the
    contract.

    `current_tree` covers owned paths in the candidate itself. Owned work created
    or restored *after* the baseline is invisible to the baseline source, so
    without this the manifest had to be hand-edited to protect new proofs -- and a
    hand-edited generated artifact drifts silently. Adding a path can only
    increase protection, so this source cannot launder an anchor loss.
    """

    base = resolve_commit(repo, base_ref)
    upstream = resolve_commit(repo, upstream_ref)
    local = resolve_commit(repo, local_ref)
    current = resolve_commit(repo, current_ref)
    upstream_objects = tree_objects(repo, upstream)

    guarded: dict[str, dict[str, object]] = {}
    for source, ref in (("ownership_baseline", local), ("current_tree", current)):
        for path, baseline_blob in sorted(tree_objects(repo, ref).items()):
            if path in guarded:
                continue
            classified = classify_path(path)
            if classified["lane"] not in GUARDED_LANES:
                continue
            upstream_blob = upstream_objects.get(path)
            if upstream_blob == baseline_blob:
                continue
            guarded[path] = {
                "path": path,
                "lane": classified["lane"],
                "contracts": classified["contracts"],
                "reason": classified["reason"],
                "source": source,
                "baselineBlob": baseline_blob,
                "upstreamBlob": upstream_blob,
            }

    entries = [guarded[path] for path in sorted(guarded)]
    header = {
        "schemaVersion": GUARD_SCHEMA_VERSION,
        "repository": "openai/codex",
        "ownershipBaseline": {
            "base": base,
            "upstream": upstream,
            "local": local,
            "current": current,
        },
        "policy": {
            "guardedLanes": list(GUARDED_LANES),
            "rule": (
                "An owned path may not be absent from the candidate, and may not "
                "match the recorded upstream blob, without an explicit waiver."
            ),
            "sources": {
                "ownership_baseline": (
                    "Owned path that already differed from upstream at the "
                    "pre-anchor local baseline."
                ),
                "current_tree": (
                    "Owned path in the candidate tree, so owned work created or "
                    "restored after the baseline is guarded without hand-editing."
                ),
            },
        },
        "summary": {
            "guardedPaths": len(entries),
            "guardedLaneCounts": dict(
                sorted(Counter(entry["lane"] for entry in entries).items())
            ),
            "guardedSourceCounts": dict(
                sorted(Counter(entry["source"] for entry in entries).items())
            ),
        },
    }
    return {**header, "guardedPaths": entries}


def render_guard(manifest: dict[str, object]) -> str:
    header = {key: value for key, value in manifest.items() if key != "guardedPaths"}
    return render_records(header, "guardedPaths", manifest["guardedPaths"])


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("format", choices=("json", "markdown", "residuals", "guard"))
    parser.add_argument("base")
    parser.add_argument("upstream")
    parser.add_argument("local")
    parser.add_argument("repo", nargs="?", default=".")
    parser.add_argument(
        "--current",
        default="HEAD",
        help="Candidate ref whose owned paths are guarded alongside the baseline",
    )
    return parser.parse_args()


RENDERERS = {
    "json": render_json,
    "markdown": render_markdown,
    "residuals": render_residuals,
}


def main() -> None:
    args = parse_args()
    repo = Path(args.repo).resolve()
    if args.format == "guard":
        manifest = build_guard_manifest(
            repo, args.base, args.upstream, args.local, args.current
        )
        print(render_guard(manifest), end="")
        return
    inventory = build_inventory(repo, args.base, args.upstream, args.local)
    print(RENDERERS[args.format](inventory), end="")


if __name__ == "__main__":
    main()
