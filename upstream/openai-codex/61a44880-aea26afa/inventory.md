# Upstream convergence inventory

- Merge base: `61a44880a85d2fd0d8770908dea5733495e571c8`
- Upstream snapshot: `aea26afaee177d3fe40721ef261a29f89879d505`
- Local baseline: `a65d78ad29fbe0994a5de25a7d521cfb90047a45`
- Conflicts: 50
- Residual local-influence paths retained by an upstream-first merge: 873

Residual paths merge cleanly, so no reviewer sees them. The merge keeps
local content there instead of upstream content; it does not reject it.
`residuals.json` lists every one with its contract lane.

## Counts

| Dimension | Value |
| --- | ---: |
| Conflict `content` | 50 |
| Lane `amber_contract_adapt` | 19 |
| Lane `green_bulk_adopt` | 23 |
| Lane `intentionally_owned` | 7 |
| Lane `red_manual_review` | 1 |
| Residual lane `amber_contract_adapt` | 240 |
| Residual lane `green_bulk_adopt` | 321 |
| Residual lane `intentionally_owned` | 298 |
| Residual lane `red_manual_review` | 14 |

## Contract-reviewed conflicts

Green paths are intentionally omitted from this table because the candidate
takes upstream unchanged. The JSON companion records every conflict path.

| Lane | Contracts | Path | Reason |
| --- | --- | --- | --- |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/ClientRequest.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/codex_app_server_protocol.schemas.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/codex_app_server_protocol.v2.schemas.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadListParams.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/ClientRequest.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/v2/Thread.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `HISTORY-1`, `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/thread_history_projection_tests.rs` | durable history and resume semantics; app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/src/schema_fixtures.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/src/schema_fixtures_tests.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-test-client/src/lib.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/README.md` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/message_processor_tracing_tests.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/executor_skills.rs` | app-server and wire compatibility |
| `intentionally_owned` | `HISTORY-1` | `codex-rs/core/src/context/world_state/environment_tests.rs` | durable environment baseline across resume and fork |
| `intentionally_owned` | `HISTORY-1` | `codex-rs/core/src/context/world_state/mod.rs` | durable environment baseline across resume and fork |
| `intentionally_owned` | `CONTEXT-1` | `codex-rs/core/src/session/turn.rs` | model-visible context bounds and history-rewrite exceptions |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/tools/handlers/multi_agents/spawn.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/tools/handlers/multi_agents_v2/spawn.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/exec/src/lib.rs` | bounded headless Background Review completion restored after the upstream anchor |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/Cargo.toml` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/src/migrations_tests.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/src/model/thread_metadata.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/src/runtime/threads.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/local/read_thread.rs` | durable history and resume semantics |
| `intentionally_owned` | `AGENT-1` | `codex-rs/tui/src/app/thread_routing.rs` | Every Code orchestration and review behavior |
| `red_manual_review` | `IDENTITY-1` | `codex-rs/tui/src/lib.rs` | visible or executable product identity |
| `amber_contract_adapt` | `SANDBOX-1` | `codex-rs/windows-sandbox-rs/src/unified_exec/tests.rs` | approval and sandbox policy |
