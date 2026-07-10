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

There is no debounce, unchanged-tree deduplication, validation-run lock, or
session cancellation contract. Each external process has a timeout and is
killed when it expires. Paths and check labels are deduplicated only inside one
harness invocation.

Codex Lab will not claim these semantics in the first slice. They are required
before validation expands beyond one patch tool call.

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
It does not persist a validation ledger, feed command failures back into a
correction turn, or provide TUI controls.

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

`scenarios/auto-validation-project-command-failure.json` drives one fake
Responses turn with a user-configured `[validation.project_command]`:

1. The model reaches terminal completion without requesting a tool or a second
   response.
2. The configured direct-argv command runs once after stop and legacy
   after-agent hooks have allowed completion and before `turn.completed`.
3. A nonzero exit produces one bounded `validation.completed` event with
   `actionable_failure`, exit code `7`, and captured output.
4. The command result remains advisory for this slice: the turn and `codex
exec` process still complete successfully, and no correction request is
   issued.

Executable validation configuration is ignored in repository-local
`.codex/config.toml`; it must come from user, system, managed, or runtime
configuration. Safe `[validation.groups]` project configuration remains
supported. The runtime uses the active permission profile, requires exactly
one local turn environment, caps retained output at 8 KiB, and classifies pass,
actionable failure, configuration error, timeout, and infrastructure failure.
Turn cancellation aborts the turn rather than emitting a potentially
misclassified validation result.

## Deferred Contracts

- clean-run silence and status/history presentation;
- typed cancellation results distinct from turn abortion;
- debounce, unchanged-tree deduplication, and concurrent-run suppression;
- resumed-turn and third-party-agent behavior;
- bounded turn-finish correction policy and retry limits;
- TUI settings and active/terminal status rendering.
