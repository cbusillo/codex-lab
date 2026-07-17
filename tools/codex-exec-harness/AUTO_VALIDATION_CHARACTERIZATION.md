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
command execution. Project commands targeting the same Git repository are
serialized across root sessions in one runtime. Debounce, cross-process locking,
and result caching remain deferred.

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

## Provider Decision Matrix

The provider decision uses four outcomes:

- **ADOPT** preserves the provider and its current command contract.
- **ADAPT** preserves the capability but reworks discovery, trust, or lifecycle
  around Codex Lab's terminal root-turn validation boundary.
- **DEFER** records the contract but does not include it in the MVP runtime.
- **RETIRE** removes a duplicate provider adapter while retaining the behavior
  in its authoritative subsystem.

| Provider | Class and trigger | Discovery and trust source | Command and bounds | Decision |
| --- | --- | --- | --- | --- |
| JSON parser | Built-in functional parser for added or updated `*.json` content | In-process `serde_json`; no executable or repository trust | Patch-local; 12 surfaced findings with total count and truncation metadata | **ADOPT**; already implemented |
| TOML parser | Built-in functional parser for added or updated `*.toml` content | In-process `toml`; no executable or repository trust | Same patch-local finding bound | **ADOPT**; already implemented |
| YAML parser | Built-in functional parser for added or updated `*.yml` and `*.yaml` content | In-process `serde_yaml`; no executable or repository trust | Same patch-local finding bound | **ADOPT**; already implemented |
| `actionlint` | Hook-backed functional check for GitHub workflow changes | `maybe_run_actionlint` plus GitHub integration configuration, not the generic executable registry | Up to 24 lines in the authoritative adapter | **RETIRE** the duplicate validation-provider adapter; retain workflow validation in its hook/plugin owner |
| `shellcheck` | Functional executable for changed `*.sh` files and files beginning with `#!/` | Built-in provider definition; default `shellcheck` from the active user/system `PATH`; only user, system, managed, or runtime config may override argv | `shellcheck -f gcc <files>`; 6-second provider timeout; existing 8 KiB event and 960-byte correction-context caps | **ADOPT** as the first MVP executable provider |
| `markdownlint` | Stylistic executable for changed `*.md` files | `markdownlint` with `markdownlint-cli2` fallback plus repository configuration through a workspace overlay | 6-second authoritative timeout and bounded output | **ADAPT** after MVP; preserve repository-aware execution without dual implicit fallbacks |
| `hadolint` | Stylistic executable for `Dockerfile` and `Dockerfile.*` | Fixed executable from trusted `PATH`; no project command | 6-second authoritative timeout and bounded output | **DEFER**; simple follow-on after the functional MVP slice |
| `yamllint` | Stylistic executable for changed YAML files | Fixed executable plus repository configuration through a workspace overlay | `yamllint -f parsable <files>` with a 6-second authoritative timeout | **ADAPT** after MVP |
| `cargo-check` | Functional workspace check when Rust files change | Trusted Cargo toolchain plus discovered `Cargo.toml` manifests and target hints | `cargo check --quiet`, `RUSTFLAGS=-Dwarnings`, at least 30 seconds per manifest in the authoritative source | **ADAPT** after MVP; too broad and expensive for the first provider |
| `shfmt` | Stylistic executable for the same shell-file trigger as `shellcheck` | Fixed executable from trusted `PATH` | `shfmt -d <files>` with a 6-second authoritative timeout | **DEFER**; functional `shellcheck` proves the shared shell-file resolver first |
| `prettier` | Stylistic executable for supported web/data/markup files and Prettier config files | Nearest project root, local `node_modules/.bin` preferred, global fallback, repository config through an overlay | `prettier --check <files>` with bounded output | **ADAPT** after MVP; requires Node project grouping and trusted local-tool rules |
| `tsc` | Functional executable for changed `*.ts` and `*.tsx` files | Nearest TypeScript project, local Node tool preferred, optional discovered `tsconfig` | `tsc --noEmit --pretty false`, at least 20 seconds in the authoritative source | **ADAPT** after MVP |
| `eslint` | Functional executable for changed JS/TS-family files when an ESLint config exists | Nearest configured project root, local Node tool preferred | `eslint --max-warnings 0 <files>`, at least 15 seconds in the authoritative source | **ADAPT** after MVP |
| `phpstan` | Functional executable for changed PHP files when PHPStan configuration exists | Trusted `PATH` plus nearest `phpstan.neon*` repository configuration | `phpstan analyse --error-format=raw --no-progress`, at least 20 seconds | **DEFER** pending a PHP dogfood repository |
| `psalm` | Functional executable for changed PHP files when Psalm configuration exists | Trusted `PATH` plus nearest `psalm.xml*` repository configuration | Compact no-progress output, two threads, at least 20 seconds | **DEFER** pending a PHP dogfood repository |
| `mypy` | Functional executable for changed Python files | Nearest virtual environment preferred, trusted `PATH` fallback, repository typing configuration | `mypy --no-color-output --hide-error-context <files>`, at least 20 seconds | **ADAPT** after MVP; requires explicit virtual-environment trust rules |
| `pyright` | Functional executable for changed Python files | Nearest virtual environment preferred, trusted `PATH` fallback, repository typing configuration | `pyright --warnings <files>`, at least 20 seconds | **ADAPT** after MVP; share the Python resolver with `mypy` |
| `golangci-lint` | Functional executable for changed Go files in a repository with `go.mod` | Trusted `PATH` plus module-root discovery | `golangci-lint run ./...`, at least 20 seconds | **ADAPT** after MVP; module-wide scope needs stronger elapsed-time and deduplication policy |

### Shared Executable-Provider Contract

The Codex Lab provider runtime extends the existing project-command lifecycle
instead of restoring the monolithic patch harness:

1. Providers run only for root sessions after the worktree admission check
   detects relevant work. Selection uses the current changed paths relative to
   `HEAD`, including untracked files, and fixed provider predicates.
2. An explicit `validation.project_command` remains the catch-all contract and
   takes precedence over automatic provider selection. The MVP runs at most one
   executable validation path per turn, preserving existing project-command
   behavior while adding automatic selection when no command is configured.
3. Repository configuration may enable or disable safe provider definitions,
   but it cannot supply executable argv. Executable overrides are accepted only
   from user, system, managed, or runtime configuration and never use shell
   interpretation.
4. The MVP selects at most one executable provider (`shellcheck`) and at most 64
   matching paths whose rendered argv remains within 8 KiB.
5. One provider execution is allowed initially and one final rerun is allowed
   after the existing model correction cycle. There is no hidden retry loop.
6. A provider has a 6-second default timeout. Retained event output is capped at
   8 KiB, and actionable model feedback reuses the existing fully rendered
   960-byte correction-fragment cap.
7. Exit zero is `passed`; a nonzero exit is `actionable_failure`; timeout,
   invalid or missing executable configuration, and execution infrastructure
   failures retain their existing typed terminal states. Turn cancellation
   emits no potentially misleading terminal result.
8. Equivalent unchanged work is suppressed, same-repository execution uses the
   existing cancellation-aware coordinator, and the correction rerun reacquires
   the lease. Resumed or multi-patch turns therefore cannot create duplicate
   provider work beyond the one initial attempt and one owned rerun.
9. Provider results continue to use `validation.completed`; the fixed command
   field identifies the provider without adding a parallel validation ledger or
   protocol surface for the MVP.

### Selected MVP Provider

`shellcheck` is the first provider for issue #310. It is functional rather than
stylistic, has a narrow changed-file predicate, uses a fixed read-only argv,
needs no repository project graph or configuration discovery, and exercises the
same executable resolution, timeout, output, cancellation, correction, and
deduplication contracts needed by later providers.

`scenarios/auto-validation-shellcheck-provider.json` records #309's
deterministic contract for #310 before production implementation. A trusted
user-level provider
override points to a fake direct-argv executable, a model patch adds one shell
file, the first provider run emits one bounded actionable failure, the existing
correction fragment appears exactly once, and the single owned rerun passes.
The scenario is `contract-only` and excluded from `run_all.py` until #310 turns
the contract into runtime-covered proof.

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

## Concurrent Run Contract

Project commands targeting the same Git checkout do not execute concurrently:

1. The runtime shares one project-validation coordinator across root sessions
   created by the same thread manager.
2. The lease key is the canonical Git repository root. Different repositories
   remain independent, while non-Git workspaces preserve fail-open execution
   without coordination.
3. Configuration, executable-resolution, and environment failures remain
   visible before lease acquisition.
4. Lease waiting is cancellation-aware. Cancellation never emits a terminal
   validation result and releasing or cancelling an owner cannot leave a stale
   in-memory lease.
5. A fast unchanged-worktree check can skip before contention. Changed or
   uncertain attempts acquire the lease and repeat the check before execution,
   so a waiter evaluates the current repository state rather than a stale state
   observed while another command was active.
6. The lease covers command execution only. An actionable failure releases it
   during the model correction cycle, and the owned final rerun reacquires it.
   This prevents command overlap without granting one session ownership of all
   repository edits during model latency.
7. Contenders wait rather than silently skip, preserving validation coverage
   for work that may not have been included in the active command.

## Deferred Contracts

- clean-run silence and status/history presentation;
- typed cancellation results distinct from turn abortion;
- debounce, cross-process locking, and concurrent result coalescing;
- steering that arrives after the final rerun starts;
- resumed-turn cleanup/consumed-state and third-party-agent behavior;
- TUI settings and active/terminal status rendering.
