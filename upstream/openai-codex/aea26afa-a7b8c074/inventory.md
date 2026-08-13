# Upstream convergence inventory

- Merge base: `aea26afaee177d3fe40721ef261a29f89879d505`
- Upstream snapshot: `a7b8c074b577f897111c14de3a5e127b91e2a479`
- Local baseline: `fce4249dba82acfe2e165cab56b4643808b8f79d`
- Conflicts: 151
- Residual local-influence paths retained by an upstream-first merge: 962

Residual paths merge cleanly, so no reviewer sees them. The merge keeps
local content there instead of upstream content; it does not reject it.
`residuals.json` lists every one with its contract lane.

## Counts

| Dimension | Value |
| --- | ---: |
| Conflict `content` | 144 |
| Conflict `modify/delete` | 7 |
| Lane `amber_contract_adapt` | 38 |
| Lane `green_bulk_adopt` | 92 |
| Lane `intentionally_owned` | 21 |
| Residual lane `amber_contract_adapt` | 273 |
| Residual lane `green_bulk_adopt` | 376 |
| Residual lane `intentionally_owned` | 310 |
| Residual lane `red_manual_review` | 3 |

## Contract-reviewed conflicts

Green paths are intentionally omitted from this table because the candidate
takes upstream unchanged. The JSON companion records every conflict path.

| Lane | Contracts | Path | Reason |
| --- | --- | --- | --- |
| `intentionally_owned` | `GOVERNANCE-1` | `.github/workflows/repo-checks.yml` | upstream convergence policy, evidence, and enforcement |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/codex_app_server_protocol.schemas.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/codex_app_server_protocol.v2.schemas.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/precomputed/app-server-exports-experimental.json.zst` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/precomputed/app-server-exports-stable.json.zst` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/ClientRequest.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/v2/index.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `HISTORY-1`, `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/thread_history.rs` | durable history and resume semantics; app-server and wire compatibility |
| `amber_contract_adapt` | `HISTORY-1`, `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/thread_history_projection_tests.rs` | durable history and resume semantics; app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/v2/item.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/v2/tests.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/README.md` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/external_agent_migration/processor.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/external_agent_migration/session_importer.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/message_processor.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/request_processors/account_processor.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/request_processors/apps_processor/installed.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/common/rollout.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/command_exec.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/config_rpc.rs` | app-server and wire compatibility |
| `intentionally_owned` | `AGENT-1`, `INTEGRATION-1`, `PROTOCOL-1`, `VALIDATION-1` | `codex-rs/app-server/tests/suite/v2/mod.rs` | app-server and wire compatibility; registration point for owned integration proofs |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/thread_fork.rs` | app-server and wire compatibility |
| `intentionally_owned` | `IDENTITY-1` | `codex-rs/cli/src/login.rs` | invariant Codex Lab product identity |
| `intentionally_owned` | `IDENTITY-1` | `codex-rs/cli/src/main.rs` | invariant Codex Lab product identity |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core-skills/src/render.rs` | binding skill routing restored after the upstream anchor |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/agent/control/spawn.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `CONTEXT-1` | `codex-rs/core/src/context/token_budget_context.rs` | model-visible context bounds and history-rewrite exceptions |
| `intentionally_owned` | `HISTORY-1` | `codex-rs/core/src/context/world_state/environment.rs` | durable environment baseline across resume and fork |
| `intentionally_owned` | `HISTORY-1` | `codex-rs/core/src/context/world_state/mod.rs` | durable environment baseline across resume and fork |
| `intentionally_owned` | `CONTEXT-1` | `codex-rs/core/src/session/turn.rs` | model-visible context bounds and history-rewrite exceptions |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/tasks/review.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/tools/handlers/multi_agents_tests.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/tools/handlers/multi_agents_v2/spawn.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/tools/handlers/multi_agents_v2/wait.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1`, `INTEGRATION-1`, `VALIDATION-1` | `codex-rs/core/tests/suite/mod.rs` | registration point for owned integration proofs |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/tests/suite/spawn_agent_description.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `INTEGRATION-1` | `codex-rs/core/tests/suite/tools.rs` | Code Bridge, browser, and remote control |
| `intentionally_owned` | `AGENT-1` | `codex-rs/exec/src/lib.rs` | bounded headless Background Review completion restored after the upstream anchor |
| `intentionally_owned` | `HOOKS-1` | `codex-rs/hooks/src/engine/discovery.rs` | hook identity and persisted hook state |
| `amber_contract_adapt` | `SANDBOX-1` | `codex-rs/linux-sandbox/tests/suite/managed_proxy.rs` | approval and sandbox policy |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/login/Cargo.toml` | credential persistence and account selection |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/login/src/auth/manager.rs` | credential persistence and account selection |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/login/src/auth/mod.rs` | credential persistence and account selection |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/login/src/server.rs` | credential persistence and account selection |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/protocol/src/protocol.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/secrets/src/local.rs` | credential persistence and account selection |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/src/runtime/threads.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/lib.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/local/archive_thread.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/local/delete_thread.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/local/mod.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/local/paginated_fork.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/local/unarchive_thread.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/local/update_thread_metadata.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/store.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/types.rs` | durable history and resume semantics |
| `intentionally_owned` | `IDENTITY-1` | `codex-rs/tui/src/lib.rs` | invariant Codex Lab product identity |
| `intentionally_owned` | `AUTH-1`, `SANDBOX-1` | `codex-rs/utils/cli/src/shared_options.rs` | Every Code shared CLI options for auth profiles and workspace roots |
| `amber_contract_adapt` | `SANDBOX-1` | `codex-rs/windows-sandbox-rs/src/unified_exec/tests.rs` | approval and sandbox policy |
