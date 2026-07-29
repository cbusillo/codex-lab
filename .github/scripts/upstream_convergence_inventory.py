#!/usr/bin/env python3

import argparse
import fnmatch
import json
import locale
import os
import queue
import re
import subprocess
import tempfile
import threading
import time
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
POLICY_VERSION = 2
LEGACY_POLICY_VERSION = 1
SUPPORTED_POLICY_VERSIONS = (LEGACY_POLICY_VERSION, POLICY_VERSION)

# Lanes whose local content may not silently disappear or silently revert to the
# upstream blob during a refresh. `upstream_convergence_guard.py` enforces this.
GUARDED_LANES = ("intentionally_owned", "red_manual_review")
GIT_ENVIRONMENT_KEYS = {
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_REPLACE_REF_BASE",
    "GIT_WORK_TREE",
}
GIT_TIMEOUT_SECONDS = 300
MAX_GIT_OUTPUT_BYTES = 128 * 1024 * 1024

# Marks a manifest row whose content is upstream's, so only its absence is a
# violation. `upstream_convergence_guard.py` reads this field.
PRESENCE_ONLY_GUARD = "presence_only"


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

# Crate-level test binaries that declare `mod suite;`. They are the only edge
# from the compiled test binary to the owned suites, so deleting one silently
# stops every owned proof in that crate from running while no proof file goes
# missing. Their content is upstream's, though, so a content comparison says
# nothing: they are guarded for presence only.
#
# `codex-rs/app-server/tests/all.rs` is deliberately absent. Its pre-anchor
# version also installed a test keyring store, so it carries a real content
# question that belongs to `PROTOCOL-1` review rather than to a presence check.
PRESENCE_ONLY_PROOF_REGISTRIES = (
    "codex-rs/core/tests/all.rs",
    "codex-rs/exec/tests/all.rs",
)


POLICY_V1_RULES = (
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
            # The Background Review engine itself. `tasks/review.rs` drives the
            # background run, its status events, and its budget cancellation;
            # `state/session.rs` holds the durable per-session review state the
            # engine reads back. Upstream owns both filenames with much smaller
            # modules, so the stem convention cannot reach them.
            "codex-rs/core/src/tasks/review.rs",
            "codex-rs/core/src/tasks/review_tests.rs",
            "codex-rs/core/src/state/session.rs",
            "codex-rs/core/src/state/session_tests.rs",
            # The TUI-side agent session environment that carries provenance
            # into spawned agents.
            "codex-rs/tui/src/agent_session_env*",
            "codex-rs/tui/src/chatwidget/snapshots/*background_auto_review*",
            # Every Code-only wire surface for Background Review, Auto Review,
            # and session provenance. These are additive schemas with no
            # upstream counterpart, mirroring how `VALIDATION-1` guards the
            # Project Validation fixtures.
            "codex-rs/app-server-protocol/schema/json/v2/AutoReview*",
            "codex-rs/app-server-protocol/schema/json/v2/BackgroundAutoReview*",
            "codex-rs/app-server-protocol/schema/json/v2/SessionProvenance*",
            "codex-rs/app-server-protocol/schema/typescript/v2/AutoReview*",
            "codex-rs/app-server-protocol/schema/typescript/v2/BackgroundAutoReview*",
            "codex-rs/app-server-protocol/schema/typescript/v2/SessionProvenance*",
            "codex-rs/app-server-protocol/schema/typescript/v2/ReviewStartTarget*",
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
            # The browser control module lives directly under `core/src`, which
            # is not an implementation root, so the stem convention misses it.
            "codex-rs/core/src/browser*",
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
            # The end-to-end proof for that one history rewrite: a turn that
            # carries an image the Responses API rejects must recover through a
            # checkpoint rather than a replay.
            "codex-rs/core/tests/suite/invalid_image_recovery.rs",
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
            # The whole world-state module, not two named files: the reader, its
            # size limits, and its tool surface were restored as siblings and a
            # per-file list silently misses the next one.
            "codex-rs/core/src/context/world_state/**",
        ),
        lane="intentionally_owned",
        contracts=("HISTORY-1",),
        reason="durable environment baseline across resume and fork",
    ),
    Rule(
        patterns=(
            # Approval-vocabulary compatibility shims. Codex Lab keeps parsing
            # the retired `on-failure` policy name and the retired review
            # decisions on every external entry point -- CLI flag, MCP tool
            # param, and protocol payload -- so an upgrade cannot reject a
            # request an older client still sends. Upstream owns the enums
            # these extend, so only the shims and their proofs are guarded.
            "codex-rs/mcp-server/src/approval_response_compat*",
            "codex-rs/protocol/src/review_decision_compat*",
            "codex-rs/utils/cli/src/approval_mode_cli_arg*",
        ),
        lane="intentionally_owned",
        contracts=("SANDBOX-1",),
        reason="approval and review decision compatibility for older clients",
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
        patterns=(*SHARED_PROOF_REGISTRIES, *PRESENCE_ONLY_PROOF_REGISTRIES),
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

GOVERNANCE_RULES = (
    Rule(
        patterns=(
            "upstream/**",
            ".github/CODEOWNERS",
            ".github/scripts/upstream_convergence*.py",
            ".github/scripts/test_upstream_convergence*.py",
            ".github/scripts/verify_upstream_convergence_governance.py",
            ".github/scripts/test_convergence_guard_workflows.py",
            ".github/workflows/blocking-ci.yml",
            ".github/workflows/repo-checks.yml",
        ),
        lane="intentionally_owned",
        contracts=("GOVERNANCE-1",),
        reason="upstream convergence policy, evidence, and enforcement",
    ),
)

POST_ANCHOR_RULES = (
    Rule(
        patterns=feature_paths("apply_patch_validation"),
        lane="intentionally_owned",
        contracts=("VALIDATION-1",),
        reason="bounded structural validation feedback restored after the upstream anchor",
    ),
    Rule(
        patterns=(
            "codex-rs/exec/src/lib.rs",
            "codex-rs/exec/src/lib_tests.rs",
        ),
        lane="intentionally_owned",
        contracts=("AGENT-1",),
        reason="bounded headless Background Review completion restored after the upstream anchor",
    ),
    Rule(
        patterns=(
            "codex-rs/tui/Cargo.toml",
            "codex-rs/tui/src/debug_config.rs",
        ),
        lane="red_manual_review",
        contracts=("RELEASE-1",),
        reason="local build provenance diagnostics",
    ),
    Rule(
        patterns=(
            "scripts/local/cargo-build-env.sh",
            "scripts/local/install-codex-lab-dev.sh",
            "scripts/local/test_install_codex_lab_dev.py",
        ),
        lane="intentionally_owned",
        contracts=("RELEASE-1",),
        reason="Every Code distribution authority",
    ),
)

POLICY_V2_RULES = (*GOVERNANCE_RULES, *POST_ANCHOR_RULES, *POLICY_V1_RULES)


def git_environment(**updates: str) -> dict[str, str]:
    env = {
        key: value
        for key, value in os.environ.items()
        if key not in GIT_ENVIRONMENT_KEYS and not key.startswith("GIT_CONFIG_")
    }
    env["GIT_NO_REPLACE_OBJECTS"] = "1"
    env["GIT_CONFIG_GLOBAL"] = os.devnull
    env["GIT_CONFIG_SYSTEM"] = os.devnull
    env["GIT_ATTR_NOSYSTEM"] = "1"
    env["GIT_TERMINAL_PROMPT"] = "0"
    env.update(updates)
    return env


def run_process_bounded(
    command: list[str],
    *,
    env: dict[str, str],
    operation: str,
    timeout_seconds: int,
    max_output_bytes: int,
    text: bool,
) -> subprocess.CompletedProcess[str] | subprocess.CompletedProcess[bytes]:
    process = subprocess.Popen(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
    )
    if process.stdout is None or process.stderr is None:
        process.kill()
        process.wait()
        raise RuntimeError(f"{operation} did not expose output pipes")

    events: queue.Queue[tuple[str, bytes | OSError | None]] = queue.Queue(maxsize=8)

    def drain(name: str, stream: object) -> None:
        try:
            with stream:
                while chunk := stream.read(64 * 1024):
                    events.put((name, chunk))
        except OSError as error:
            events.put((name, error))
        finally:
            events.put((name, None))

    readers = [
        threading.Thread(target=drain, args=("stdout", process.stdout), daemon=True),
        threading.Thread(target=drain, args=("stderr", process.stderr), daemon=True),
    ]
    for reader in readers:
        reader.start()

    stdout = bytearray()
    stderr = bytearray()
    active_readers = len(readers)
    deadline = time.monotonic() + timeout_seconds
    failure: RuntimeError | None = None
    while active_readers:
        if failure is None and time.monotonic() >= deadline:
            failure = RuntimeError(f"{operation} exceeded {timeout_seconds} seconds")
            if process.poll() is None:
                process.kill()
        try:
            name, payload = events.get(timeout=0.05)
        except queue.Empty:
            continue
        if payload is None:
            active_readers -= 1
            continue
        if isinstance(payload, OSError):
            if failure is None:
                failure = RuntimeError(f"cannot read {operation} output: {payload}")
                if process.poll() is None:
                    process.kill()
            continue
        if failure is not None:
            continue
        if len(stdout) + len(stderr) + len(payload) > max_output_bytes:
            failure = RuntimeError(
                f"{operation} exceeded {max_output_bytes} output bytes"
            )
            if process.poll() is None:
                process.kill()
            continue
        target = stdout if name == "stdout" else stderr
        target.extend(payload)

    for reader in readers:
        reader.join()
    if process.poll() is None:
        try:
            returncode = process.wait(
                timeout=max(0.0, deadline - time.monotonic())
            )
        except subprocess.TimeoutExpired:
            if failure is None:
                failure = RuntimeError(
                    f"{operation} exceeded {timeout_seconds} seconds"
                )
            process.kill()
            returncode = process.wait()
    else:
        returncode = process.wait()
    if failure is not None:
        raise failure

    raw_stdout = bytes(stdout)
    raw_stderr = bytes(stderr)
    if text:
        encoding = locale.getpreferredencoding(False)
        return subprocess.CompletedProcess(
            command,
            returncode,
            raw_stdout.decode(encoding),
            raw_stderr.decode(encoding),
        )
    return subprocess.CompletedProcess(command, returncode, raw_stdout, raw_stderr)


def run_git_process(
    repo: Path,
    *args: str,
    env: dict[str, str] | None = None,
    max_output_bytes: int = MAX_GIT_OUTPUT_BYTES,
    text: bool = True,
) -> subprocess.CompletedProcess[str] | subprocess.CompletedProcess[bytes]:
    operation = f"git {' '.join(args)}"
    return run_process_bounded(
        ["git", "-C", str(repo), *args],
        env=env or git_environment(),
        operation=operation,
        timeout_seconds=GIT_TIMEOUT_SECONDS,
        max_output_bytes=max_output_bytes,
        text=text,
    )


def rules_for_policy(policy_version: int) -> tuple[Rule, ...]:
    if policy_version == LEGACY_POLICY_VERSION:
        return POLICY_V1_RULES
    if policy_version == POLICY_VERSION:
        return POLICY_V2_RULES
    raise ValueError(
        f"unsupported policy version {policy_version}; "
        f"expected one of {SUPPORTED_POLICY_VERSIONS}"
    )


def run_git(repo: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    result = run_git_process(repo, *args)
    if not isinstance(result.stdout, str) or not isinstance(result.stderr, str):
        raise RuntimeError(f"git {' '.join(args)} returned binary output")
    if check and result.returncode != 0:
        raise subprocess.CalledProcessError(
            result.returncode,
            result.args,
            output=result.stdout,
            stderr=result.stderr,
        )
    return result


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


def tree_objects(
    repo: Path, ref: str, env: dict[str, str] | None = None
) -> dict[str, str]:
    result = run_git_process(repo, "ls-tree", "-r", "-z", ref, env=env, text=False)
    if not isinstance(result.stdout, bytes) or not isinstance(result.stderr, bytes):
        raise RuntimeError(f"git ls-tree -r -z {ref} returned text output")
    if result.returncode != 0:
        raise subprocess.CalledProcessError(
            result.returncode,
            result.args,
            output=result.stdout,
            stderr=result.stderr,
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
    repo: Path, upstream: str, local: str, policy_version: int = POLICY_VERSION
) -> tuple[dict[str, str], list[dict[str, object]]]:
    raw_objects = Path(run_git(repo, "rev-parse", "--git-path", "objects").stdout.strip())
    objects = raw_objects if raw_objects.is_absolute() else (repo / raw_objects).resolve()
    with tempfile.TemporaryDirectory(prefix="upstream-convergence-objects-") as temporary:
        env = git_environment(
            GIT_OBJECT_DIRECTORY=temporary,
            GIT_ALTERNATE_OBJECT_DIRECTORIES=str(objects),
        )
        result = run_git_process(
            repo,
            "merge-tree",
            "--write-tree",
            "--messages",
            upstream,
            local,
            env=env,
        )
        if not isinstance(result.stdout, str) or not isinstance(result.stderr, str):
            raise RuntimeError("git merge-tree returned binary output")
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
        result_objects = tree_objects(repo, result_tree, env)
    classified = [
        classify(path, conflicts[path], policy_version) for path in sorted(conflicts)
    ]
    return result_objects, classified


def classify_path(
    path: str, policy_version: int = POLICY_VERSION
) -> dict[str, object]:
    lane = "green_bulk_adopt"
    contracts: set[str] = set()
    reasons: list[str] = []
    for rule in rules_for_policy(policy_version):
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


def classify(
    path: str, conflict_type: str, policy_version: int = POLICY_VERSION
) -> dict[str, object]:
    classified = classify_path(path, policy_version)
    return {
        "path": classified["path"],
        "conflictType": conflict_type,
        "lane": classified["lane"],
        "contracts": classified["contracts"],
        "reason": classified["reason"],
    }


def build_inventory(
    repo: Path,
    base_ref: str,
    upstream_ref: str,
    local_ref: str,
    policy_version: int = POLICY_VERSION,
) -> dict[str, object]:
    base = resolve_commit(repo, base_ref)
    upstream = resolve_commit(repo, upstream_ref)
    local = resolve_commit(repo, local_ref)
    actual_base = run_git(repo, "merge-base", upstream, local).stdout.strip()
    if actual_base != base:
        raise ValueError(f"expected merge base {base}, found {actual_base}")

    result_objects, conflicts = merge_conflicts(repo, upstream, local, policy_version)
    conflict_paths = {entry["path"] for entry in conflicts}
    local_paths = changed_paths(repo, base, local)
    upstream_paths = changed_paths(repo, base, upstream)
    shared_paths = local_paths & upstream_paths
    upstream_objects = tree_objects(repo, upstream)
    local_objects = tree_objects(repo, local)
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
    residuals = [classify_path(path, policy_version) for path in residual_paths]

    policy = {
        "defaultLane": "green_bulk_adopt",
        "rule": "Upstream wins unless a named convergence contract applies.",
    }
    if policy_version != LEGACY_POLICY_VERSION:
        policy["version"] = policy_version

    return {
        "schemaVersion": SCHEMA_VERSION,
        "repository": "openai/codex",
        "refs": {
            "base": base,
            "upstream": upstream,
            "local": local,
        },
        "policy": policy,
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
    current_ref: str = "HEAD",
    policy_version: int = LEGACY_POLICY_VERSION,
) -> dict[str, object]:
    """Record the owned paths a later refresh must not silently drop or revert.

    Two sources contribute, because either one alone leaves a hole:

    `ownership_baseline` covers owned paths that already differed from upstream at
    the pre-anchor local baseline. A path byte-identical to upstream there had no
    local delta to lose. This source must stay pinned to the pre-anchor baseline;
    recomputing it from the candidate would bake the anchor's losses into the
    contract.

    `current_tree` covers owned paths in the candidate itself using the current
    classifier policy. Owned work created or restored *after* the baseline is
    invisible to the baseline source, so without this the manifest had to be
    hand-edited to protect new proofs -- and a hand-edited generated artifact
    drifts silently. Adding a path can only increase protection, so this source
    cannot launder an anchor loss or rewrite historical classification.
    """

    base = resolve_commit(repo, base_ref)
    upstream = resolve_commit(repo, upstream_ref)
    local = resolve_commit(repo, local_ref)
    current = resolve_commit(repo, current_ref)
    upstream_objects = tree_objects(repo, upstream)

    guarded: dict[str, dict[str, object]] = {}
    for source, ref in (("ownership_baseline", local), ("current_tree", current)):
        source_policy_version = (
            policy_version if source == "ownership_baseline" else POLICY_VERSION
        )
        for path, baseline_blob in sorted(tree_objects(repo, ref).items()):
            if path in guarded:
                continue
            classified = classify_path(path, source_policy_version)
            if classified["lane"] not in GUARDED_LANES:
                continue
            upstream_blob = upstream_objects.get(path)
            presence_only = path in PRESENCE_ONLY_PROOF_REGISTRIES
            # A path byte-identical to upstream has no local delta to revert, so
            # it is normally not worth a manifest row. Presence-only registries
            # are the exception: what they carry is the edge that makes the
            # owned suites run at all, so they are recorded regardless of
            # content and the guard checks them for absence alone.
            if upstream_blob == baseline_blob and not presence_only:
                continue
            entry = {
                "path": path,
                "lane": classified["lane"],
                "contracts": classified["contracts"],
                "reason": classified["reason"],
                "source": source,
                "baselineBlob": baseline_blob,
                "upstreamBlob": upstream_blob,
            }
            if presence_only:
                entry["guard"] = PRESENCE_ONLY_GUARD
            guarded[path] = entry

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
    parser.add_argument(
        "--policy-version",
        type=int,
        choices=SUPPORTED_POLICY_VERSIONS,
        help=(
            "Classifier policy version. Defaults to version 1 for guard manifests "
            "and the current version for inventories."
        ),
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
    policy_version = args.policy_version
    if policy_version is None:
        policy_version = (
            LEGACY_POLICY_VERSION if args.format == "guard" else POLICY_VERSION
        )
    if args.format == "guard":
        manifest = build_guard_manifest(
            repo,
            args.base,
            args.upstream,
            args.local,
            args.current,
            policy_version,
        )
        print(render_guard(manifest), end="")
        return
    inventory = build_inventory(
        repo,
        args.base,
        args.upstream,
        args.local,
        policy_version,
    )
    print(RENDERERS[args.format](inventory), end="")


if __name__ == "__main__":
    main()
