# Upstream convergence inventory

- Merge base: `cbfd999db78cb088d2bd89b52051efe6f44555a4`
- Upstream snapshot: `2c79ee6dacb6deccb7e19ac5acffb3e379bbe895`
- Local baseline: `de048f15ed752d5110f4f4f08fb6a6730e891043`
- Conflicts: 115
- Residual local-influence paths retained by an upstream-first merge: 1115

Residual paths merge cleanly, so no reviewer sees them. The merge keeps
local content there instead of upstream content; it does not reject it.
`residuals.json` lists every one with its contract lane.

## Counts

| Dimension | Value |
| --- | ---: |
| Conflict `content` | 115 |
| Lane `amber_contract_adapt` | 35 |
| Lane `green_bulk_adopt` | 52 |
| Lane `intentionally_owned` | 28 |
| Residual lane `amber_contract_adapt` | 300 |
| Residual lane `green_bulk_adopt` | 468 |
| Residual lane `intentionally_owned` | 344 |
| Residual lane `red_manual_review` | 3 |

## Contract-reviewed conflicts

Green paths are intentionally omitted from this table because the candidate
takes upstream unchanged. The JSON companion records every conflict path.

| Lane | Contracts | Path | Reason |
| --- | --- | --- | --- |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/ServerNotification.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/codex_app_server_protocol.schemas.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/codex_app_server_protocol.v2.schemas.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadForkResponse.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadListResponse.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadMetadataUpdateResponse.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadReadResponse.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadResumeResponse.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadRollbackResponse.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadStartResponse.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadStartedNotification.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadUnarchiveResponse.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/precomputed/app-server-exports-experimental.json.zst` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/precomputed/app-server-exports-stable.json.zst` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/ClientRequest.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/ServerNotification.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/ServerNotificationEnvelope.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/v2/Thread.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/v2/ThreadItem.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/v2/index.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `HISTORY-1`, `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/thread_history_projection_tests.rs` | durable history and resume semantics; app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/v2/thread_data.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/README.md` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/bespoke_event_handling.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/in_process.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/request_processors/apps_processor/installed.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/request_processors/thread_processor.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/request_processors/turn_processor.rs` | app-server and wire compatibility |
| `intentionally_owned` | `AGENT-1`, `INTEGRATION-1`, `PROTOCOL-1`, `VALIDATION-1` | `codex-rs/app-server/tests/suite/v2/mod.rs` | app-server and wire compatibility; registration point for owned integration proofs |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/realtime_conversation.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/review.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/thread_rollback.rs` | app-server and wire compatibility |
| `intentionally_owned` | `IDENTITY-1` | `codex-rs/cli/src/main.rs` | invariant Codex Lab product identity |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/agent/control.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/agent/control/spawn.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/agent/control_tests.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/context/guardian_review_evidence.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `CONTEXT-1` | `codex-rs/core/src/context/token_budget_context.rs` | model-visible context bounds and history-rewrite exceptions |
| `intentionally_owned` | `HISTORY-1` | `codex-rs/core/src/context/world_state/environment.rs` | durable environment baseline across resume and fork |
| `intentionally_owned` | `HISTORY-1` | `codex-rs/core/src/context/world_state/environment_render_tests.rs` | durable environment baseline across resume and fork |
| `intentionally_owned` | `CONTEXT-1` | `codex-rs/core/src/context_manager/history.rs` | model-visible context bounds and history-rewrite exceptions |
| `intentionally_owned` | `CONTEXT-1` | `codex-rs/core/src/context_manager/history_tests.rs` | model-visible context bounds and history-rewrite exceptions |
| `intentionally_owned` | `HISTORY-1` | `codex-rs/core/src/session/rollout_reconstruction.rs` | durable environment baseline across resume and fork |
| `intentionally_owned` | `CONTEXT-1` | `codex-rs/core/src/session/turn.rs` | model-visible context bounds and history-rewrite exceptions |
| `intentionally_owned` | `HISTORY-1` | `codex-rs/core/src/session/turn_context.rs` | durable environment baseline across resume and fork |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/state/session.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/tasks/review.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/tools/handlers/multi_agents/spawn.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/tools/handlers/multi_agents_tests.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/tools/handlers/multi_agents_v2/spawn.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1`, `INTEGRATION-1`, `VALIDATION-1` | `codex-rs/core/tests/suite/mod.rs` | registration point for owned integration proofs |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/tests/suite/multi_agent_mode.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/tests/suite/spawn_agent_description.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/tests/suite/subagent_notifications.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `INTEGRATION-1` | `codex-rs/core/tests/suite/tools.rs` | Code Bridge, browser, and remote control |
| `intentionally_owned` | `HOOKS-1` | `codex-rs/hooks/src/engine/discovery.rs` | hook identity and persisted hook state |
| `amber_contract_adapt` | `SANDBOX-1` | `codex-rs/linux-sandbox/src/proxy_routing.rs` | approval and sandbox policy |
| `amber_contract_adapt` | `SANDBOX-1` | `codex-rs/linux-sandbox/tests/suite/managed_proxy.rs` | approval and sandbox policy |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/login/src/auth/manager.rs` | credential persistence and account selection |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/protocol/src/protocol.rs` | app-server and wire compatibility |
| `intentionally_owned` | `IDENTITY-1` | `codex-rs/tui/src/app.rs` | invariant Codex Lab product identity |
| `intentionally_owned` | `AGENT-1` | `codex-rs/tui/src/app/test_support.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `IDENTITY-1` | `codex-rs/tui/src/lib.rs` | invariant Codex Lab product identity |
