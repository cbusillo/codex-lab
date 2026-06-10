# Codex Exec Harness

`tools/codex-exec-harness/harness.py` runs isolated `codex exec --json`
scenarios and saves evidence under `.tmp/codex-exec-harness/`.

This is a small Codex-native proof harness for prompt and request-composition
regressions. It is intentionally narrower than Every Code Lab's exec harness:
there is no fake GitHub, no auth inheritance, and no local-model fallback logic.
Multi-turn scenarios are supported only through explicit `turns` fixtures that
resume the captured Codex thread id.

## Run

Run the full harness suite against a freshly built local Codex binary:

```sh
just exec-harness-test
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
- `config_toml`: isolated `CODEX_HOME/config.toml` contents
- `config_overrides`: `-c key=value` arguments passed to `codex exec`
- `responses_api`: start a local fake Responses API and point Codex at it
- `expect`: assertions over return code, turn count, captured thread id, and
  fake Responses request bodies
- `timeout_seconds`: per-run timeout, defaulting to 90 seconds

The fake Responses API is for request-shape proof only. Use live or local model
runs separately when the question depends on model behavior rather than prompt
assembly.

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
