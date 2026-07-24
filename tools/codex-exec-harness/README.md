# Codex Exec Harness

`tools/codex-exec-harness/harness.py` runs isolated `codex exec --json`
scenarios and saves evidence under `.tmp/codex-exec-harness/`.

This is a small Codex-native proof harness for prompt and request-composition
regressions. It is intentionally narrower than Every Code Lab's exec harness:
there is no fake GitHub, and no local-model fallback logic.
Multi-turn scenarios are supported only through explicit `turns` fixtures that
resume the captured Codex thread id.

## Run

Run the full harness suite against a freshly built local Codex binary:

```sh
just exec-harness-test
```

On Linux, this recipe also builds and stages the bundled
`codex-resources/bwrap` helper required to enforce writable roots outside the
launch working directory.

The full runner verifies the binary once before any scenario starts. Its
aggregate report records the exact executable path, source commit, dirty state,
build profile/channel, and binary digest under `provenance`. A stale or
unverifiable binary stops the run before scenario evidence is accepted.

Verify an existing binary without running scenarios:

```sh
python3 scripts/local/codex_lab_provenance.py \
  --repo-root . \
  --binary codex-rs/target/debug/codex \
  --verify-only \
  --json
```

Run one scenario against an existing binary:

```sh
python3 tools/codex-exec-harness/harness.py \
  tools/codex-exec-harness/scenarios/skills-guidance-binding-triggers.json \
  --codex-bin codex-rs/target/debug/codex
```

Each run writes:

- `artifacts/command.json`: command under test for single-turn scenarios
- `artifacts/stdout.jsonl`: raw `codex exec --json` events for single-turn
  scenarios
- `artifacts/stderr.log`: stderr from the run for single-turn scenarios
- `artifacts/turn-XX/`: per-turn command, stdout, and stderr for multi-turn
  scenarios
- `artifacts/responses-requests.json`: fake `/v1/responses` POST bodies
- `artifacts/summary.json`: return code, request count, and assertion failures

## Scenario Shape

Scenarios are JSON files. Supported fields:

- `name`: run name used in artifact paths
- `model`: optional model argument passed with `-m`
- `prompt`: prompt passed to `codex exec`
- `turns`: ordered turn objects; turn 1 runs `codex exec`, later turns resume
  the captured thread id with `codex exec resume`
- `files`: workspace files created before the run
- `external_files`: files created under the run-local `external/` directory
- `symlinks`: workspace-relative links to run-local fixture paths; targets may
  use `{workspace}`, `{external}`, or `{run_dir}`
- `add_dirs`: run-local directories passed to Codex with `--add-dir`
- `workspace_roots`: exact run-local workspace roots passed to Codex with
  repeatable `--workspace-root`; unlike `add_dirs`, these replace the implicit
  writable launch CWD and require `sandbox: workspace-write`
- `config_toml`: isolated `CODEX_LAB_HOME/config.toml` contents
- `config_overrides`: `-c key=value` arguments passed to `codex exec`
- `responses_api`: start a local fake Responses API and point Codex at it
- `inherit_auth`: copy the caller's `CODEX_LAB_HOME` or `~/.codex-lab` into the
  isolated harness home for opt-in live model scenarios
- `inherit_host_home`: preserve the caller's `HOME` for opt-in live external
  provider scenarios while keeping Codex Lab, shell, XDG config, and cache
  paths isolated; this lets provider CLIs reuse their existing authentication
- `skip_run_all`: omit a scenario from `run_all.py` and CI's all-scenario sweep
- `workspace_outside_git`: materialize the workspace under
  `~/.codex-exec-harness-workspaces` or
  `CODEX_EXEC_HARNESS_EXTERNAL_WORKSPACE_ROOT`, and fail unless Git reports no
  containing worktree and no ancestor `AGENTS.md` or `AGENTS.override.md`
- `expect`: assertions over return code, turn count, captured thread id, and
  fake Responses request bodies, captured agent messages and commands, and
  optional durable Background Review target/currentness metadata;
  `launch_command` checks candidate argv and `workspace_paths` verifies
  post-run file, directory, symlink, or absence evidence; `workspace_git`
  checks recorded Git metadata such as `git_root`, `branch`, or `head_sha`
- `timeout_seconds`: per-run timeout, defaulting to 90 seconds

The fake Responses API is for request-shape proof only. Use direct scenario runs
with `inherit_auth` for opt-in live model checks when the question depends on
model behavior rather than prompt assembly. Mark those scenarios
`skip_run_all: true` unless they are safe for unauthenticated CI.

Use `inherit_host_home` only for direct, opt-in live scenarios that launch a
locally authenticated external provider. It intentionally gives spawned
commands access to the caller's home directory, so it must remain excluded from
`run_all.py` and CI.

## Generated Odoo Workspaces

`odoo_workspace.py` is the bounded consumer for a devkit-generated non-Git
workspace. It does not parse `workspace.lock.toml` or Odoo markers. Instead it
runs the provider-owned command below and fails closed unless the returned
schema, guidance, source materialization, reserved override, and edit roots are
all current:

```sh
uv --directory /path/to/odoo-devkit run platform workspace status \
  --manifest /path/to/tenant/workspace.toml --check
```

Launch an exact Codex Lab candidate in editable mode:

```sh
python3 tools/codex-exec-harness/odoo_workspace.py \
  --devkit /path/to/odoo-devkit \
  --manifest /path/to/tenant/workspace.toml \
  --codex-bin /absolute/path/to/codex-lab \
  --source-repo /path/to/codex-lab \
  --mode exec \
  --access editable \
  --prompt 'Inspect the generated workspace.' \
  --evidence-file /path/to/evidence.json
```

Editable mode selects the built-in workspace-write sandbox and passes each
status-declared editable linked path through repeatable `--workspace-root`.
Those exact roots replace the implicit launch CWD, leaving the generated root
and managed checkouts read-only. Passing the same paths through legacy
`--add-dir` would also make the launch CWD writable, so the bounded adapter
intentionally does not combine the two mechanisms. `--access read-only` uses
the built-in read-only sandbox. Both modes require current candidate provenance
from `scripts/local/codex_lab_provenance.py`; prompt text is hashed and redacted
from evidence.

`generated-odoo-workspace-consumption.json` is the deterministic exact-candidate
proof. It keeps the root non-Git, loads the canonical root guide, permits writes
through tenant/devkit links, and proves managed, generated-root, and outside
writes remain denied.

## Auto-Validation Characterization

Issue #284's first auto-validation contract is documented in
`AUTO_VALIDATION_CHARACTERIZATION.md`. The corresponding
`auto-validation-bounded-apply-patch-feedback.json` scenario runs in the default
suite with `characterization.status = "runtime-covered"`. It protects bounded,
call-id-scoped validation feedback on successful `apply_patch` calls.

`auto-validation-project-command-failure.json` protects the next headless
contract: one user-owned direct-argv validation command runs at root-turn
completion and emits a typed, bounded `validation.completed` event before
`turn.completed`.

`auto-validation-shellcheck-provider.json` proves issue #310's first
automatically selected executable provider. A trusted user-level override
points to a deterministic fake ShellCheck command; the default suite verifies
changed-file selection, fixed argv, one bounded correction cycle, and the final
passing rerun.

`auto-validation-cargo-provider.json` proves issue #372's bounded Rust
provider. A trusted external Cargo executable verifies nearest-target
selection, fixed JSON argv, compact compiler diagnostics, one correction
fragment, and the final passing rerun.

`auto-validation-no-applicable-provider.json` proves issue #371's previously
silent Rust-only path now emits one bounded typed `skipped` disposition with
the changed-file count and `no_applicable_provider` reason.

`gpt-5-6-luna-low-request-shape.json` is the deterministic catalog smoke test
for the lowest-cost GPT-5.6 variant. It proves the local binary sends
`gpt-5.6-luna` with low reasoning effort without making a paid model call.

`agent-capability-self-report.json` protects capability questions from generic
harness answers. With agent metadata explicitly enabled, it proves GPT-5.6 Sol
receives the configured third-party roles, model overrides, concurrency limit,
and self-report guidance without adding that catalog to every default session.

`third-party-agent-current-binary-lifecycle.json` provides the deterministic
current-binary gate for issue #312. It configures a fixture external-command
agent, then proves native spawn, parent-visible completion output, terminal
status listing, and close cleanup without using a paid provider or inherited
credentials.

`provider-routing-high-risk-external.json` proves issue #370's significance-aware
automatic routing with a deterministic external-command fixture.
`provider-preflight-diagnostics.json` proves generic external-command preflight
plus parent-visible provider provenance, classified runtime authentication
failure, elapsed runtime, and cleanup. Provider-specific authentication probes
are deterministic Rust tests; `provider-routing-live-claude-antigravity.json`
is the opt-in current-binary companion that exercises authenticated Claude and
Antigravity CLIs.

`third-party-agent-live-copilot.json` is the opt-in real-provider companion. It
keeps the parent model deterministic through the fake Responses API, runs
GitHub Copilot read-only, and is excluded from `run_all.py` so CI never consumes
a provider account or depends on local CLI authentication. When Copilot relies
on GitHub CLI authentication, export `GH_TOKEN="$(gh auth token)"` for the
harness process so the spawned provider receives the credential.

## Auto Review Proof Loop

Issue #35 uses this harness as the first Codex-native Auto Review proof loop.
The runtime review systems still live in `codex-rs`, but the harness owns the
artifact semantics that make Auto Review safe to consume in later readiness,
Code Bridge, and Auto Drive work:

- findings are classified against the active branch and head SHA as `current`,
  `stale`, or `detached`;
- only `current` findings are surfaced by the summary helper, with a hard
  summary byte cap and finding-count cap;
- clean runs and stale/detached runs render no noisy summary;
- finding details are recovered by stable id and bounded by a caller-supplied
  byte cap;
- run ids must be safe path components before sidecar save/load.

The focused unit gate is:

```sh
python3 -m unittest discover -s tools/codex-exec-harness -p 'test_harness.py'
```

If a future implementation injects Auto Review artifacts into model-visible
context, move that payload into a `codex-rs/core/src/context` fragment that
implements `ContextualUserFragment` and keep the fragment bounded per the
repository `AGENTS.md` context rules.

When a scenario needs to own its provider config, put the provider in
`config_toml` and use `{responses_base_url}` as the fake Responses server URL:

```toml
model_provider = "local-fixture"

[model_providers.local-fixture]
name = "Local Fixture"
base_url = "{responses_base_url}"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = false
```

The harness only substitutes `{responses_base_url}` when `responses_api` is
present. It does not inherit real provider config or silently fall back to a
cloud provider.

Single-turn scenarios may use top-level `prompt`. Multi-turn scenarios use:

```json
{
  "turns": [{ "prompt": "first turn" }, { "prompt": "second turn" }]
}
```

For multi-turn proof, combine `expect.responses_request_count`,
`expect.turn_count`, `expect.thread_id = "required"`, and per-request assertions
under `expect.responses`.

Use `expect.turns` to assert per-turn metadata such as `returncode`,
`event_count`, `responses_request_count`, and `thread_id = "required"`.

Use `expect.agent_messages` and `expect.commands` to assert text in captured
assistant messages or shell commands from `codex exec --json` events. The same
assertions are available per turn under `expect.turns[]`.

Use `expect.background_review` for a scenario that must leave a durable Background
Review ledger. It can assert `run_count`, `status`, `source`, `freshness`,
`finding_count`, `review_target_type`, `target_matches_workspace`, and
`worktree_clean`. The harness saves the collected ledger and workspace Git
state as `artifacts/background-review-runs.json` and
`artifacts/workspace-git.json`.

Use `expect.tool_outputs[]` with `request` and `call_id` to assert against one
matching `function_call_output` or `custom_tool_call_output`. This is stricter
than flattening the whole request input when a result must remain attached to
the tool call that produced it. Add a `json_suffix` text assertion object when
the final non-empty output line must also parse as JSON.

Fake Responses fixtures may include per-response `usage` values. The harness
forwards them through `response.completed` so scenarios can make deterministic
token and cache assertions without calling a real model:

```json
{
  "responses_api": {
    "responses": [
      {
        "response_id": "resp-1",
        "usage": {
          "input_tokens": 500,
          "cached_input_tokens": 250,
          "output_tokens": 20
        }
      }
    ]
  }
}
```

Under `expect.turns[].token_usage`, use exact field names for equality,
`*_min` / `*_max` for bounds, and `cache_ratio_min` / `cache_ratio_max` for the
derived `cached_input_tokens / input_tokens` ratio. For example:

```json
{
  "expect": {
    "turns": [
      {
        "token_usage": {
          "input_tokens_max": 12000,
          "cached_input_tokens_min": 3000,
          "cache_ratio_min": 0.25
        }
      }
    ]
  }
}
```

Use `expect.responses[].prefix_matches_request` with `prefix_length` to compare
the first characters of one captured request scope with another. This is useful
for prompt-prefix stability checks across resumed turns. By default the
reference request uses the same `scope`; set `prefix_scope` when the reference
request should use a different scope.

Use `expect.responses[].input_prefix_matches_request` when cache safety depends
on preserving the whole previous request input as a structural prefix of a later
request. This compares JSON input items rather than mocked token usage, so it
can catch volatile context inserted before the cached input tail.
