# Auto-Validation Characterization

Issue #284 is the durable plan for restoring Every Code auto-validation. This
document records the source audit and the first executable behavior contract;
it is not a replacement for the GitHub issue.

The authoritative source was audited at
`../code-prealign-new-skills/code-rs` revision
`4339c3743917725b3b685864b3384af259a35964`.

## Source Inventory

### Trigger

`core/src/apply_patch.rs` invokes `run_patch_harness` synchronously for each
`apply_patch` tool call, before safety assessment and the actual write. Findings
are advisory and never block the patch.

Codex Lab keeps the patch-local trigger but adapts execution to the current
runtime boundary: safe in-memory checks run after approval and a successful
write, using the committed patch delta. Turn-finish orchestration comes later.

### Command Selection

`core/src/patch_harness.rs` runs built-in JSON, TOML, and YAML parsers plus a
fixed registry of detected external tools. `[validation.groups]`, per-tool
toggles, `tools_allowlist`, and `timeout_seconds` control the registry.

Configured `projects.<cwd>.commands` and project hooks are separate execution
systems. The patch harness does not consume either one. Codex Lab will keep the
group and tool policy for source parity, then add project-defined commands
through a deliberate contract rather than hard-coding repository commands.

### Debounce, Cancellation, and Deduplication

The authoritative source has no debounce, unchanged-tree deduplication,
validation-run lock, or session cancellation contract. Each external process
has a timeout and is killed when it expires. Paths and check labels are
deduplicated only inside one harness invocation.

Codex Lab now suppresses the initial turn-finish project command when a real Git
worktree fingerprint is unchanged across a tool-free root turn. Model tool
activity, non-Git workspaces, and unreadable fingerprints fail open and preserve
command execution. Debounce and concurrent run locking remain deferred.

### Retry and Fix Loop

Validation does not retry automatically. Structured findings are returned in
the tool output, allowing the active model turn to decide whether and how to fix
them.

Codex Lab will keep correction model-driven and bounded. It will not introduce
an unbounded hidden retry loop.

### Result Model

The tool output contains `validation.issues`, `validation.checks`,
`validation.issue_count`, and `validation.truncated`. A timeout is represented
as a finding. Cancellation, configuration errors, and infrastructure failures
are not distinct result classes.

Codex Lab will preserve the structured payload, then introduce explicit
terminal result classes before turn-wide orchestration.

### Bounds

`core/src/apply_patch.rs` caps surfaced findings at 12, rejects oversized tool
and message fields, and truncates status text. Several external-tool adapters
also cap captured lines at 24.

Codex Lab will keep hard bounds. The first scenario proves that 13 findings
surface as 12 with `issue_count = 13` and `truncated = true`.

### Persistence

Validation results are transient. Only configuration is persisted. There is no
validation ledger.

Codex Lab will keep validation separate from the durable Auto Review ledger.
Only policy should be persisted until a concrete history requirement exists.

### Model Feedback

`core/src/codex/streaming.rs` appends compact validation JSON to the
`apply_patch` function-call output used by the next Responses request. There is
no extra static prompt fragment.

Codex Lab will keep feedback in the tool result so failures stay local to the
action that produced them.

### TUI and Status

`tui/src/bottom_pane/validation_settings_view.rs` exposes group and tool
toggles. `/validation` reports or changes settings. `apply_patch` emits a
background status message for clean and failing runs.

Codex Lab will defer TUI work until the headless contract passes and prefer
quiet clean runs when adapting the status surface.

### Auto Drive and Third-Party Agents

`code-auto-drive-core/src/auto_coordinator.rs` requires claimed validation
evidence before finish, but it does not execute validation commands. Agent
registration has a separate smoke test that does not validate agent-produced
code.

Codex Lab will not treat either behavior as proof of automatic project
validation. Real results must exist before completion policy or agent workflows
consume them.

### Source Caveats

- The documentation says functional validation defaults on, while the Rust
  `Default` implementation sets both groups off when the full validation config
  is absent. Characterization therefore enables the group explicitly.
- TUI tool detection checks `PATH`, while runtime execution can discover some
  local Node and Python tools. Displayed installation state can disagree with
  execution behavior.
- Live session updates cover only the older subset of tool toggles. Some newer
  tool settings persist but do not update the current session immediately.

The current Codex Lab runtime implements two safe subsets: configurable
functional validation for committed JSON, TOML, and YAML patch results, plus
one user-owned project validation command at the terminal root-turn boundary.
The project command skips unchanged supported Git worktrees only on tool-free
turns and retains a single bounded correction-and-rerun cycle for actionable
failures. It does not persist a validation ledger or provide TUI controls.

## First Contract

`scenarios/auto-validation-bounded-apply-patch-feedback.json` drives one fake
Responses turn:

1. The model calls `apply_patch` with 13 invalid JSON files.
2. Functional validation is enabled explicitly through `[validation.groups]`.
3. The continuation request must contain a structured `json-parse` result.
4. Exactly 12 findings are surfaced, while `issue_count` remains 13 and
   `truncated` is true.
5. The matching tool output contains 25 `bad-` path occurrences: 13 in the
   successful patch summary and 12 in the validation result. This avoids
   depending on the hash-map iteration order that chooses which finding is
   omitted.
6. The validation JSON must remain attached to the matching tool output. The
   scenario parses the final output line and does not pass if equivalent text
   appears in another message or in malformed JSON.

The scenario has `characterization.status = "runtime-covered"` and runs in the
default all-scenario gate.

## Project Command Contract

`scenarios/auto-validation-project-command-failure.json` drives one fake root
turn with a user-configured `[validation.project_command]`:

1. The model reaches initial terminal completion without requesting a tool.
2. The configured direct-argv command runs once after stop and legacy
   after-agent hooks have allowed completion and before `turn.completed`.
3. A nonzero exit produces one bounded `validation.completed` event with
   `actionable_failure`, exit code `7`, and captured output.
4. The failure remains advisory but triggers the bounded correction contract
   below before the turn and `codex exec` process complete successfully.

Executable validation configuration is ignored in repository-local
`.codex/config.toml`; it must come from user, system, managed, or runtime
configuration. Safe `[validation.groups]` project configuration remains
supported. The runtime uses the active permission profile, requires exactly
one local turn environment, caps retained output at 8 KiB, and classifies pass,
actionable failure, configuration error, timeout, and infrastructure failure.
Turn cancellation aborts the turn rather than emitting a potentially
misclassified validation result.

## Bounded Correction Contract

An actionable project-command failure now remains advisory but receives one
runtime-owned correction cycle:

1. The first `actionable_failure` event is emitted before correction begins.
2. One marked `<project_validation_failure>` user-context fragment is recorded.
   Its fully rendered payload is hard-capped at 960 bytes, conservatively below
   1K tokens, and includes only a fixed instruction, command, exit code,
   truncation state, and bounded head/tail output.
3. The active root turn runs one additional model correction cycle. Pending
   steering is recorded after the failure fragment and joins this same cycle.
4. The configured project command runs one final time after the correction
   cycle becomes quiescent. Its result is terminal even when it is another
   actionable failure.
5. The runtime therefore permits at most one correction cycle and two automatic
   project-command executions per root turn. Cancellation before or during the
   correction cycle prevents the final rerun.
6. Once recorded, the fragment follows normal incremental history and rollout
   semantics. Cancellation does not rewrite already recorded model context.

`scenarios/auto-validation-project-command-failure.json` characterizes the
headless success path: the first command run fails, the second Responses request
contains exactly one marked correction fragment, the single rerun passes, two
`validation.completed` events are emitted, and one `turn.completed` closes the
turn.

## Unchanged Worktree Admission Contract

The initial project-command attempt is admitted only when a supported Git
worktree changed during the root turn:

1. Before the first model request, the runtime captures the repository root's
   current `HEAD` and a fingerprint of the diff against it, including untracked
   files across the checkout.
2. After a tool-free turn becomes quiescent, the runtime captures the same state
   again.
3. An exact match skips the initial command silently: no
   `validation.completed` event and no correction fragment are emitted.
   The skip remains provisional while input is pending; admission is retried
   with the original fingerprint and cumulative tool activity after the
   continuation becomes quiescent.
4. Any model tool activity fails open so writes to ignored paths or external
   tools cannot be hidden by Git fingerprinting.
5. Non-Git workspaces, unborn repositories, and unreadable fingerprints fail
   open and preserve command execution.
6. Configuration, executable-resolution, and environment errors remain visible
   even when the worktree is unchanged.
7. Once an actionable failure starts a correction cycle, the final rerun is
   owned by that cycle and is never suppressed by the admission gate.

## Deferred Contracts

- clean-run silence and status/history presentation;
- typed cancellation results distinct from turn abortion;
- debounce and concurrent-run suppression;
- steering that arrives after the final rerun starts;
- resumed-turn cleanup/consumed-state and third-party-agent behavior;
- TUI settings and active/terminal status rendering.
