# Upstream convergence inventory

- Merge base: `b89ce9a2bcedcfddf3a48f387b7912d602d6d87c`
- Upstream snapshot: `4462b9deef211723b781b426f5e5d36a5777115f`
- Local baseline: `8add494682f7c0674672e8dc5b38a4565cd7629b`
- Conflicts: 295
- Residual local-influence paths retained by an upstream-first merge: 478

Residual paths merge cleanly, so no reviewer sees them. The merge keeps
local content there instead of upstream content; it does not reject it.
`residuals.json` lists every one with its contract lane.

## Counts

| Dimension | Value |
| --- | ---: |
| Conflict `add/add` | 9 |
| Conflict `content` | 279 |
| Conflict `modify/delete` | 7 |
| Lane `amber_contract_adapt` | 107 |
| Lane `green_bulk_adopt` | 163 |
| Lane `intentionally_owned` | 18 |
| Lane `red_manual_review` | 7 |
| Residual lane `amber_contract_adapt` | 141 |
| Residual lane `green_bulk_adopt` | 197 |
| Residual lane `intentionally_owned` | 137 |
| Residual lane `red_manual_review` | 3 |

## Contract-reviewed conflicts

Green paths are intentionally omitted from this table because the candidate
takes upstream unchanged. The JSON companion records every conflict path.

| Lane | Contracts | Path | Reason |
| --- | --- | --- | --- |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/ClientRequest.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/codex_app_server_protocol.schemas.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/codex_app_server_protocol.v2.schemas.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/RawResponseItemCompletedNotification.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadForkResponse.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadListParams.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadListResponse.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadMetadataUpdateResponse.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadReadResponse.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadResumeParams.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadResumeResponse.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadRollbackResponse.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadStartParams.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadStartResponse.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadStartedNotification.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/json/v2/ThreadUnarchiveResponse.json` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/ClientRequest.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/ResponseItem.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/ServerNotification.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/v2/ConfiguredHookHandler.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/v2/LoginAccountParams.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/v2/Thread.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/v2/ThreadItem.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/schema/typescript/v2/ThreadListParams.ts` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/src/export.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/common.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `HISTORY-1`, `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/thread_history.rs` | durable history and resume semantics; app-server and wire compatibility |
| `amber_contract_adapt` | `HISTORY-1`, `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/thread_history_projection.rs` | durable history and resume semantics; app-server and wire compatibility |
| `amber_contract_adapt` | `HISTORY-1`, `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/thread_history_projection_tests.rs` | durable history and resume semantics; app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/v2/account.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/v2/item.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/v2/tests.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/v2/thread.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-protocol/src/protocol/v2/thread_data.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server-test-client/src/lib.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/README.md` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/bespoke_event_handling.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/main.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/message_processor.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/request_processors.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/request_processors/account_processor.rs` | app-server and wire compatibility |
| `intentionally_owned` | `AGENT-1`, `PROTOCOL-1` | `codex-rs/app-server/src/request_processors/external_agent_config_processor.rs` | app-server and wire compatibility; Every Code orchestration and review behavior |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/request_processors/thread_processor.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/request_processors/thread_processor_tests.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/request_processors/thread_resume_redaction.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/request_processors/thread_summary.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/request_processors/turn_processor.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/src/thread_status.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/common/lib.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/common/rollout.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/common/test_app_server.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3`, `PROTOCOL-1` | `codex-rs/app-server/tests/suite/auth.rs` | credential persistence and account selection; app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/conversation_summary.rs` | app-server and wire compatibility |
| `intentionally_owned` | `AGENT-1`, `INTEGRATION-1`, `PROTOCOL-1`, `VALIDATION-1` | `codex-rs/app-server/tests/suite/mod.rs` | app-server and wire compatibility; registration point for owned integration proofs |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/connection_handling_websocket.rs` | app-server and wire compatibility |
| `intentionally_owned` | `AGENT-1`, `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/external_agent_config.rs` | app-server and wire compatibility; Every Code orchestration and review behavior |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/plugin_list.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/remote_thread_store.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/review.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/skills_list.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/thread_fork.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/thread_list.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/thread_read.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/thread_resume.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/thread_settings_update.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/thread_start.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/thread_unarchive.rs` | app-server and wire compatibility |
| `red_manual_review` | `IDENTITY-1` | `codex-rs/cli/src/login.rs` | visible or executable product identity |
| `red_manual_review` | `IDENTITY-1` | `codex-rs/cli/src/main.rs` | visible or executable product identity |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/agent/control.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/agent/control/spawn.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/agent/control_tests.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/agent/registry.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/agent/role_tests.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/tools/handlers/multi_agents_common.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/tools/handlers/multi_agents_spec.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/tools/handlers/multi_agents_spec_tests.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/tools/handlers/multi_agents_tests.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/tools/handlers/multi_agents_v2/close_agent.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/src/tools/handlers/multi_agents_v2/spawn.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1`, `INTEGRATION-1`, `VALIDATION-1` | `codex-rs/core/tests/suite/mod.rs` | registration point for owned integration proofs |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/tests/suite/multi_agent_resume.rs` | Every Code orchestration and review behavior |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/core/tests/suite/sqlite_state.rs` | durable history and resume semantics |
| `intentionally_owned` | `AGENT-1` | `codex-rs/core/tests/suite/subagent_notifications.rs` | Every Code orchestration and review behavior |
| `intentionally_owned` | `AGENT-1` | `codex-rs/external-agent-migration/src/sessions/export.rs` | Every Code orchestration and review behavior |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/login/Cargo.toml` | credential persistence and account selection |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/login/src/auth/auth_tests.rs` | credential persistence and account selection |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/login/src/auth/manager.rs` | credential persistence and account selection |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/login/src/auth/mod.rs` | credential persistence and account selection |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/login/src/device_code_auth.rs` | credential persistence and account selection |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/login/src/device_code_auth_tests.rs` | credential persistence and account selection |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/login/src/lib.rs` | credential persistence and account selection |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/login/src/server.rs` | credential persistence and account selection |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/login/tests/suite/auth_refresh.rs` | credential persistence and account selection |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/login/tests/suite/login_server_e2e.rs` | credential persistence and account selection |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/login/tests/suite/logout.rs` | credential persistence and account selection |
| `red_manual_review` | `MODEL-1` | `codex-rs/model-provider-info/src/lib.rs` | model catalog, default, or selection UX |
| `red_manual_review` | `MODEL-1` | `codex-rs/model-provider-info/src/model_provider_info_tests.rs` | model catalog, default, or selection UX |
| `red_manual_review` | `MODEL-1` | `codex-rs/models-manager/models.json` | model catalog, default, or selection UX |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/protocol/src/items.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/protocol/src/models.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/protocol/src/openai_models.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/protocol/src/protocol.rs` | app-server and wire compatibility |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/secrets/src/lib.rs` | credential persistence and account selection |
| `amber_contract_adapt` | `AUTH-1`, `AUTH-2`, `AUTH-3` | `codex-rs/secrets/src/local.rs` | credential persistence and account selection |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/Cargo.toml` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/src/bin/logs_client.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/src/extract.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/src/lib.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/src/migrations.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/src/migrations_tests.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/src/model/thread_metadata.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/src/runtime.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/src/runtime/memories.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/src/runtime/test_support.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/src/runtime/threads.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/state/thread_history_migrations/0001_thread_history.sql` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/Cargo.toml` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/in_memory.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/lib.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/live_thread.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/local/create_thread.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/local/helpers.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/local/live_writer.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/local/mod.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/local/read_thread.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/local/update_thread_metadata.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/store.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/thread_metadata_sync.rs` | durable history and resume semantics |
| `amber_contract_adapt` | `HISTORY-1` | `codex-rs/thread-store/src/types.rs` | durable history and resume semantics |
| `red_manual_review` | `IDENTITY-1` | `codex-rs/tui/src/app.rs` | visible or executable product identity |
| `red_manual_review` | `IDENTITY-1` | `codex-rs/tui/src/lib.rs` | visible or executable product identity |
