# Upstream convergence inventory

- Merge base: `a7b8c074b577f897111c14de3a5e127b91e2a479`
- Upstream snapshot: `343074d4207d572809bd8cea15f4be1d09d98e0b`
- Local baseline: `0eed1a86c783c474a009cb8b548ab58e18513614`
- Conflicts: 90
- Residual local-influence paths retained by an upstream-first merge: 1030

Residual paths merge cleanly, so no reviewer sees them. The merge keeps
local content there instead of upstream content; it does not reject it.
`residuals.json` lists every one with its contract lane.

## Counts

| Dimension | Value |
| --- | ---: |
| Conflict `content` | 89 |
| Conflict `modify/delete` | 1 |
| Lane `amber_contract_adapt` | 34 |
| Lane `green_bulk_adopt` | 33 |
| Lane `intentionally_owned` | 23 |
| Residual lane `amber_contract_adapt` | 287 |
| Residual lane `green_bulk_adopt` | 400 |
| Residual lane `intentionally_owned` | 340 |
| Residual lane `red_manual_review` | 3 |

## Contract-reviewed conflicts

Green paths are intentionally omitted from this table because the candidate
takes upstream unchanged. The JSON companion records every conflict path.

| Lane | Contracts | Path | Reason |
| --- | --- | --- | --- |
| `intentionally_owned` | `GOVERNANCE-1` | `.github/workflows/repo-checks.yml` | upstream convergence policy, evidence, and enforcement |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-client/src/lib.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/ServerNotification.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/codex_app_server_protocol.schemas.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/codex_app_server_protocol.v2.schemas.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/precomputed/app-server-exports-experimental.json.zst` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/precomputed/app-server-exports-stable.json.zst` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/ServerNotification.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/ServerNotificationEnvelope.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/v2/Thread.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/v2/index.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `HISTORY-1`, `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/thread_history_projection_tests.rs` | durable history and resume semantics; app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/v2/mod.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/v2/tests.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/v2/thread.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/Cargo.toml` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/README.md` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/config_manager.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/in_process.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/message_processor.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/message_processor_tracing_tests.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/request_processors/account_processor.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/request_processors/catalog_processor.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/request_processors/thread_processor.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/request_processors/thread_processor_tests.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/logging.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/config_rpc.rs` | app-server and wire compatibility |
| `intentionally_owned` | `AGENT-1`, `INTEGRATION-1`, `PROTOCOL-1`, `VALIDATION-1` | `codex-rs/app-server/tests/suite/v2/mod.rs` | app-server and wire compatibility; registration point for owned integration proofs |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/review.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/skills_list.rs` | app-server and wire compatibility |
| `intentionally_owned` | `IDENTITY-1` | `codex-rs/cli/src/login.rs` | invariant Codex Lab product identity |
| `intentionally_owned` | `IDENTITY-1` | `codex-rs/cli/src/main.rs` | invariant Codex Lab product identity |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/agent/control.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/agent/control/spawn.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/agent/control_tests.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/agent/role.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `CONTEXT-1` | `codex-rs/core/src/context_manager/history_tests.rs` | model-visible context bounds and history-rewrite exceptions |
| `intentionally_owned` | `CONTEXT-1` | `codex-rs/core/src/session/turn.rs` | model-visible context bounds and history-rewrite exceptions |
| `intentionally_owned` | `HISTORY-1` | `codex-rs/core/src/session/turn_context.rs` | durable environment baseline across resume and fork |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/tools/handlers/multi_agents/spawn.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/tools/handlers/multi_agents_tests.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/tools/handlers/multi_agents_v2/spawn.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1`, `INTEGRATION-1`, `VALIDATION-1` | `codex-rs/core/tests/suite/mod.rs` | registration point for owned integration proofs |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/tests/suite/spawn_agent_description.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `INTEGRATION-1` | `codex-rs/core/tests/suite/tools.rs` | Code Bridge, browser, and remote control |
| `intentionally_owned` | `AGENT-1` | `codex-rs/exec/src/lib.rs` | bounded headless Background Review completion restored after the upstream anchor |
| `intentionally_owned` | `HOOKS-1` | `codex-rs/hooks/src/engine/discovery.rs` | hook identity and persisted hook state |
| `intentionally_owned` | `HOOKS-1` | `codex-rs/hooks/src/engine/mod_tests.rs` | hook identity and persisted hook state |
| `amber_contract_adapt` | `SANDBOX-1` | `codex-rs/linux-sandbox/tests/suite/managed_proxy.rs` | approval and sandbox policy |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/login/src/auth/auth_tests.rs` | credential persistence and account selection |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/login/src/auth/manager.rs` | credential persistence and account selection |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/protocol/src/protocol.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/src/runtime/threads.rs` | durable history and resume semantics |
| `intentionally_owned` | `IDENTITY-1` | `codex-rs/tui/src/app.rs` | invariant Codex Lab product identity |
| `intentionally_owned` | `AGENT-1` | `codex-rs/tui/src/app/thread_routing.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `IDENTITY-1` | `codex-rs/tui/src/lib.rs` | invariant Codex Lab product identity |
| `amber_contract_adapt` | `SANDBOX-1` | `codex-rs/windows-sandbox-rs/BUILD.bazel` | approval and sandbox policy |
