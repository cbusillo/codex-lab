# Convergence contracts

This file is the checked-in contract matrix for issue #126. It defines the
minimum gates that an upstream-first candidate in #428 must satisfy before an
Every Code-owned difference can survive.

The matrix is versioned with the code; issue #126 remains the recovery record
for current status and open product decisions.

## Current integration snapshot

- Local baseline: `8add494682f7c0674672e8dc5b38a4565cd7629b`.
- Upstream snapshot: `4462b9deef211723b781b426f5e5d36a5777115f`.
- Shared merge base: `b89ce9a2bcedcfddf3a48f387b7912d602d6d87c`.
- Candidate anchor: `9d2eea2238c09c995e200d6ec0ad2492d2fada3b`.
- Real `ort` inventory: 295 unresolved paths (279 content, 9 add/add,
  7 modify/delete) and 773 touched paths before aborting the trial merge.
- The deterministic merge-tree inventory also identifies 478 non-conflicting
  residual paths where a normal merge would silently retain local influence.
  `upstream/openai-codex/<snapshot>/residuals.json` lists each one with its lane.

The anchor has the upstream snapshot as its first parent, the local baseline as
its second parent, and a tree identical to the upstream snapshot. Product-owned
behavior is restored only through contract-tagged follow-up commits.

## Refresh guard

The ours-tree anchor deleted every owned path upstream did not carry, silently
and unrecoverably. `upstream/convergence-guard.json` pins the pre-anchor local
baseline as the ownership record, and `repo-checks.yml` fails any candidate
where an `intentionally_owned` or `red_manual_review` path is absent or
byte-identical to the recorded upstream blob. Only
`upstream/convergence-waivers.json` can clear one, and each waiver must name a
violation, a disposition, the deciding issue, and a reason. See
`upstream/README.md` for the full procedure.

Ownership is classified by path pattern, not by an enumerated file list. Owned
features are declared by filename stem, and
`upstream_convergence_inventory.py` expands each stem across the conventional
implementation and integration-proof roots, so an owned implementation and the
proof that pins it are always guarded together. The suite registry modules
(`tests/suite/mod.rs`) are guarded too: reverting one unregisters every owned
proof in that crate while leaving each proof file in the tree, which no
per-file check would notice.

## Lanes

- **Upstream-owned:** adopt the upstream implementation by default. Local
  changes require a named contract or a new product decision.
- **Contract-adapted:** implementation may differ, but the named behavior and
  executable gate must remain intact.
- **Red-risk:** do not resolve through bulk conflict handling. A product or
  migration decision and focused validation are required.
- **Intentionally owned:** Every Code controls the behavior and release
  boundary. Upstream may be used as implementation input, not authority.

## Ownership matrix

| Contract        | Surface                                                   | Lane                                         | Required behavior                                                                                                                                                                                                                                     | Current gate                                                                                                                                                                                                                                                     |
| --------------- | --------------------------------------------------------- | -------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `HOME-1`        | Runtime state home                                        | Intentionally owned migration boundary       | Until a migration is approved, Codex Lab uses `CODEX_LAB_HOME` or `~/.codex-lab` and must not read or write Every Code or upstream homes implicitly. A #428 candidate may adopt `CODE_HOME` only with an explicit state migration and isolation test. | `find_codex_home_without_env_uses_codex_lab_default`; `add_uses_codex_lab_home_when_legacy_homes_are_set`                                                                                                                                                        |
| `IDENTITY-1`    | Product name, executable, wire identity, and visible copy | Red-risk                                     | The current tree contains mixed Codex Lab, Codex, and Every Code identity. #428 must not pick a winner through bulk conflict resolution. Canonical identity requires focused CLI help, TUI snapshot, auth, telemetry, and installer validation.       | Live validation required after the canonical identity decision; current mixed identity is not the target contract.                                                                                                                                               |
| `AUTH-1`        | Credential persistence                                    | Contract-adapted                             | `auth.json` and the multi-account catalog remain inside the selected state home; credential files are private on Unix, including after an existing file is overwritten.                                                                               | `file_storage_save_repairs_private_auth_file_permissions`; `saved_accounts_file_is_private_after_rewrite`                                                                                                                                                        |
| `AUTH-2`        | Account selection                                         | Contract-adapted                             | The account used by the current session overrides a stale on-disk active-account marker and is excluded from automatic fallback selection.                                                                                                            | `current_account_override_takes_precedence_over_stored_active_account`; `current_account_override_is_not_reselected`                                                                                                                                             |
| `AUTH-3`        | Account login and settings TUI                            | Contract-adapted                             | Add-account emits the typed ChatGPT login action while keeping the account pane active; the request preserves the existing account. Switch, refresh, disconnect, cancel, and API-key paths remain in the pane.                                        | `add_account_chatgpt_enter_starts_login`; `serialize_account_login_chatgpt_preserves_existing_account`; `loaded_account_list_enter_switches_server_account`; `loaded_account_list_refresh_reopens_accounts`; `loaded_account_list_disconnect_emits_remove_event` |
| `HISTORY-1`     | History, resume, and persisted session state              | Contract-adapted                             | Resume restores persisted turns, model settings, dynamic tools, and permissions without forwarding stale implicit settings.                                                                                                                           | `resume_restores_dynamic_tools_from_rollout_with_sqlite_enabled`; `thread_resume_params_can_restore_persisted_model_settings`; `resume_replays_permissions_messages`; `just test -p codex-state`                                                                 |
| `PROTOCOL-1`    | App-server and protocol compatibility                     | Upstream-owned with additive Every Code APIs | Upstream schemas are adopted by default. Every Code-only account, review, external-agent, and remote-control APIs must remain additive and regenerate checked-in JSON and TypeScript fixtures.                                                        | `just test -p codex-app-server-protocol`; `just test -p codex-app-server`                                                                                                                                                                                        |
| `SANDBOX-1`     | Sandbox and approval semantics                            | Contract-adapted                             | A candidate must preserve approval prompts, policy enforcement, and platform sandbox behavior; weakening an approval boundary requires a separate security decision.                                                                                  | `codex-rs/core/tests/suite/approvals.rs`; `codex-rs/core/tests/suite/skill_approval.rs`; platform sandbox suites                                                                                                                                                 |
| `AGENT-1`       | Auto Drive, external agents, and Background Review        | Intentionally owned                          | Every Code orchestration, explicit external-agent preflight, durable review state, duplicate suppression, and budget cancellation remain product-owned.                                                                                               | `codex-rs/core/tests/suite/external_agent_preflight.rs` (`wrong_copilot_executable_fails_explicit_preflight`, `missing_external_agent_command_fails_explicit_preflight_with_install_hint`, `logged_out_claude_agent_fails_explicit_preflight_as_authentication_required`, `external_command_agent_routes_through_spawn_agent_with_provider_provenance`); `codex-rs/core/src/agent/external_preflight_tests.rs`; `codex-rs/core/src/agent/provider_routing_tests.rs`; `codex-rs/core/tests/suite/background_review.rs`. `codex-rs/core/tests/suite/auto_review.rs` and `guardian_review.rs` are currently waived `reverted_to_upstream` under #428, so they are guarded but are not yet AGENT-1 evidence.                                                                                                                           |
| `INTEGRATION-1` | Code Bridge, browser, and remote control                  | Intentionally owned                          | Model-visible bridge tools stay bounded; browser control and remote-control APIs retain their protocol and state boundaries.                                                                                                                          | `code_bridge_screenshot_returns_bounded_metadata_and_image_output`; `code_bridge_status_tool_is_model_visible_and_bounded_when_unavailable`; `code_bridge_javascript_returns_bounded_control_output`; app-server remote-control suite                            |
| `VALIDATION-1`  | Project Validation                                        | Intentionally owned                          | Automatic post-turn validation stays Every Code-owned: providers are selected per project, status and skip reasons stay typed on the app-server wire, and validation failures reach the model as a bounded context fragment.                           | `codex-rs/core/tests/suite/project_validation.rs`; `codex-rs/exec/tests/suite/project_validation_event.rs`; `codex-rs/core/src/session/project_validation_tests.rs`; `codex-rs/core/src/session/validation_provider_tests.rs`                                     |
| `RELEASE-1`     | Distribution and updates                                  | Intentionally owned                          | GitHub Releases remains the canonical release authority. OpenAI R2, OpenAI package names, credentials, and domains are not inherited. A future mirror needs a separate Every Code-owned signing, rollback, installer, and credential contract.        | Workflow validation plus a live release/install smoke test; offline tests alone cannot prove external release authority.                                                                                                                                         |
| `MODEL-1`       | Model catalog and defaults                                | Red-risk                                     | Upstream catalog changes may be adopted, but default models, effort labels, account availability, and Every Code UX require an explicit catalog comparison and focused TUI snapshots.                                                                 | Live catalog comparison plus model-selection and status-card tests on the pinned candidate.                                                                                                                                                                      |
| `GOVERNANCE-1`  | Repository planning and convergence evidence              | Intentionally owned                          | GitHub issues remain the durable plan graph, issue #28 remains the recovery point, and checked-in convergence evidence must identify immutable local, upstream, and merge-base commits.                                                               | Root `AGENTS.md` planning instructions plus issue #428 ancestry and tree-identity verification.                                                                                                                                                                  |

## Known missing behavior routing

The six actionable rows retained from #407 remain visible without making the
historical ledger the integration unit:

| Upstream commit                            | Required route                                                                                                            |
| ------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------- |
| `ee6c91d5cfd0e63239c75b41f4a2dc14130d5688` | #429 owns removal of the free-form analytics error subreason; #428 must run its privacy contract.                         |
| `daf76a57d2564be85b6e34c25a29380b3d4315b4` | #428 must validate stale curated plugin-cache pruning while preserving user configuration, or create a bounded follow-up. |
| `381f0de531e0bc7759863295fc333dd0087b4faf` | #428 must validate cached global plugin listing and deduplicated asynchronous refresh, or create a bounded follow-up.     |
| `feca160da47b678b73b33dd8a08e010e86b81786` | #428 must adopt or explicitly route description-aware OTEL counters before the dependent gauge row.                       |
| `51b3cd51f6f94488c0e05564cbcad9512f73e3db` | #428 must validate MCP server visibility and constructor boundaries, or create a bounded follow-up.                       |
| `7e5e41daea443bac9df2af36d86a5332efa7b4d7` | #428 must adopt or explicitly route described i64 gauges after the counter-description dependency.                        |

## Candidate rule

Every local modification that survives in an upstream-owned file must name one
of the contract IDs above in its PR evidence. If no contract applies, the
candidate must drop the modification or record a new explicit product decision.
