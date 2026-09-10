# Upstream convergence inventory

- Merge base: `2c79ee6dacb6deccb7e19ac5acffb3e379bbe895`
- Upstream snapshot: `a62e98d18c6550e3bea152ed1b89d1e931dca961`
- Local baseline: `59ad223e64338d906ab2ada233bc6f227520cf04`
- Conflicts: 139
- Residual local-influence paths retained by an upstream-first merge: 1170

Residual paths merge cleanly, so no reviewer sees them. The merge keeps
local content there instead of upstream content; it does not reject it.
`residuals.json` lists every one with its contract lane.

## Counts

| Dimension | Value |
| --- | ---: |
| Conflict `content` | 120 |
| Conflict `modify/delete` | 19 |
| Lane `amber_contract_adapt` | 38 |
| Lane `green_bulk_adopt` | 80 |
| Lane `intentionally_owned` | 21 |
| Residual lane `amber_contract_adapt` | 310 |
| Residual lane `green_bulk_adopt` | 496 |
| Residual lane `intentionally_owned` | 361 |
| Residual lane `red_manual_review` | 3 |

## Contract-reviewed conflicts

Green paths are intentionally omitted from this table because the candidate
takes upstream unchanged. The JSON companion records every conflict path.

| Lane | Contracts | Path | Reason |
| --- | --- | --- | --- |
| `intentionally_owned` | `GOVERNANCE-1` | `.github/workflows/repo-checks.yml` | upstream convergence policy, evidence, and enforcement |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-client/src/lib.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-client/src/remote.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/precomputed/app-server-exports-experimental.json.zst` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/precomputed/app-server-exports-stable.json.zst` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/ClientRequest.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/common.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `HISTORY-1`, `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/thread_history.rs` | durable history and resume semantics; app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/v2/mod.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/README.md` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/in_process.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/lib.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/message_processor.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/message_processor_tracing_tests.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/models_refresh_worker_tests.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/request_processors/apps_processor/installed.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/request_processors/initialize_processor.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/request_processors/thread_processor.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/request_processors/turn_processor.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/logging.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/config_rpc.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/connection_handling_websocket.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/dynamic_tools.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/mcp_resource.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/remote_thread_store.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/skills_list.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/thread_rollback.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/turn_start.rs` | app-server and wire compatibility |
| `intentionally_owned` | `IDENTITY-1` | `codex-rs/cli/src/main.rs` | invariant Codex Lab product identity |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/agent/control/spawn.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/context/guardian_review_evidence.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `HISTORY-1` | `codex-rs/core/src/context/world_state/environment.rs` | durable environment baseline across resume and fork |
| `intentionally_owned` | `CONTEXT-1` | `codex-rs/core/src/context_manager/history.rs` | model-visible context bounds and history-rewrite exceptions |
| `intentionally_owned` | `CONTEXT-1` | `codex-rs/core/src/context_manager/history_tests.rs` | model-visible context bounds and history-rewrite exceptions |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/session/multi_agents.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `HISTORY-1` | `codex-rs/core/src/session/rollout_reconstruction.rs` | durable environment baseline across resume and fork |
| `intentionally_owned` | `CONTEXT-1` | `codex-rs/core/src/session/turn.rs` | model-visible context bounds and history-rewrite exceptions |
| `intentionally_owned` | `HISTORY-1` | `codex-rs/core/src/session/turn_context.rs` | durable environment baseline across resume and fork |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/tools/handlers/multi_agents_tests.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/tests/suite/spawn_agent_description.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/tests/suite/subagent_notifications.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/exec/src/lib.rs` | bounded headless Background Review completion restored after the upstream anchor |
| `intentionally_owned` | `AGENT-1`, `INTEGRATION-1`, `VALIDATION-1` | `codex-rs/exec/tests/suite/mod.rs` | registration point for owned integration proofs |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/login/src/auth/manager.rs` | credential persistence and account selection |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/login/src/auth/mod.rs` | credential persistence and account selection |
| `amber_contract_adapt` | `MODEL-1` | `codex-rs/models-manager/Cargo.toml` | upstream model catalog and defaults with Codex Lab compatibility |
| `amber_contract_adapt` | `MODEL-1` | `codex-rs/models-manager/models.json` | upstream model catalog and defaults with Codex Lab compatibility |
| `amber_contract_adapt` | `MODEL-1` | `codex-rs/models-manager/src/manager.rs` | upstream model catalog and defaults with Codex Lab compatibility |
| `amber_contract_adapt` | `MODEL-1` | `codex-rs/models-manager/src/manager_tests.rs` | upstream model catalog and defaults with Codex Lab compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/protocol/src/permission_profile_intersection_tests.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/src/extract.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/src/model/thread_metadata.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/src/runtime/memories.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/src/runtime/threads.rs` | durable history and resume semantics |
| `intentionally_owned` | `IDENTITY-1` | `codex-rs/tui/src/app.rs` | invariant Codex Lab product identity |
| `intentionally_owned` | `AGENT-1` | `codex-rs/tui/src/app/test_support.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/tui/src/app/thread_routing.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `IDENTITY-1` | `codex-rs/tui/src/lib.rs` | invariant Codex Lab product identity |
| `intentionally_owned` | `IDENTITY-1` | `codex-rs/tui/src/status/card.rs` | invariant Codex Lab product identity |
