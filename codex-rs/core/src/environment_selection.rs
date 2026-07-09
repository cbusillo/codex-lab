use std::collections::HashSet;
use std::sync::Arc;

use codex_exec_server::EnvironmentManager;
use codex_exec_server::ExecutorFileSystem;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::TurnEnvironmentSelection;
use codex_utils_absolute_path::AbsolutePathBuf;

use crate::session::turn_context::TurnEnvironment;

pub(crate) const MAX_TURN_ENVIRONMENTS: usize = 8;

pub(crate) fn default_thread_environment_selections(
    environment_manager: &EnvironmentManager,
    cwd: &AbsolutePathBuf,
) -> Vec<TurnEnvironmentSelection> {
    environment_manager
        .default_environment_ids()
        .into_iter()
        .take(MAX_TURN_ENVIRONMENTS)
        .map(|environment_id| TurnEnvironmentSelection {
            environment_id,
            cwd: cwd.clone(),
        })
        .collect()
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ResolvedTurnEnvironments {
    pub(crate) turn_environments: Vec<TurnEnvironment>,
}

impl ResolvedTurnEnvironments {
    pub(crate) fn to_selections(&self) -> Vec<TurnEnvironmentSelection> {
        self.turn_environments
            .iter()
            .map(TurnEnvironment::selection)
            .collect()
    }

    pub(crate) fn primary(&self) -> Option<&TurnEnvironment> {
        self.turn_environments.first()
    }

    pub(crate) fn primary_environment(&self) -> Option<Arc<codex_exec_server::Environment>> {
        self.primary()
            .map(|environment| Arc::clone(&environment.environment))
    }

    pub(crate) fn primary_filesystem(&self) -> Option<Arc<dyn ExecutorFileSystem>> {
        self.primary()
            .map(|environment| environment.environment.get_filesystem())
    }

    pub(crate) fn single_local_environment_cwd(&self) -> Option<&AbsolutePathBuf> {
        let [environment] = self.turn_environments.as_slice() else {
            return None;
        };

        (!environment.environment.is_remote()).then_some(&environment.cwd)
    }
}

pub(crate) fn resolve_environment_selections(
    environment_manager: &EnvironmentManager,
    environments: &[TurnEnvironmentSelection],
) -> CodexResult<ResolvedTurnEnvironments> {
    if environments.len() > MAX_TURN_ENVIRONMENTS {
        return Err(CodexErr::InvalidRequest(format!(
            "turn environments must be at most {MAX_TURN_ENVIRONMENTS}"
        )));
    }

    let mut seen_environment_ids = HashSet::with_capacity(environments.len());
    let mut turn_environments = Vec::with_capacity(environments.len());
    for selected_environment in environments {
        if !seen_environment_ids.insert(selected_environment.environment_id.as_str()) {
            return Err(CodexErr::InvalidRequest(format!(
                "duplicate turn environment id `{}`",
                selected_environment.environment_id
            )));
        }
        let environment_id = selected_environment.environment_id.clone();
        let environment = environment_manager
            .get_environment(&environment_id)
            .ok_or_else(|| {
                CodexErr::InvalidRequest(format!("unknown turn environment id `{environment_id}`"))
            })?;
        turn_environments.push(TurnEnvironment {
            environment_id,
            environment,
            cwd: selected_environment.cwd.clone(),
            shell: None,
        });
    }

    Ok(ResolvedTurnEnvironments { turn_environments })
}

pub(crate) fn resolve_stored_environment_selections(
    environment_manager: &EnvironmentManager,
    environments: &[TurnEnvironmentSelection],
) -> CodexResult<ResolvedTurnEnvironments> {
    let bounded_environments: Vec<_> = environments
        .iter()
        .take(MAX_TURN_ENVIRONMENTS)
        .cloned()
        .collect();
    resolve_environment_selections(environment_manager, &bounded_environments)
}

#[cfg(test)]
mod tests {
    use codex_exec_server::ExecServerRuntimePaths;
    use codex_exec_server::LOCAL_ENVIRONMENT_ID;
    use codex_exec_server::REMOTE_ENVIRONMENT_ID;
    use codex_protocol::protocol::TurnEnvironmentSelection;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use pretty_assertions::assert_eq;

    use super::*;

    fn test_runtime_paths() -> ExecServerRuntimePaths {
        ExecServerRuntimePaths::new(
            std::env::current_exe().expect("current exe"),
            /*codex_linux_sandbox_exe*/ None,
        )
        .expect("runtime paths")
    }

    #[tokio::test]
    async fn default_thread_environment_selections_use_manager_default_id() {
        let cwd = AbsolutePathBuf::current_dir().expect("cwd");
        let manager = EnvironmentManager::create_for_tests(
            Some("ws://127.0.0.1:8765".to_string()),
            Some(test_runtime_paths()),
        )
        .await;

        assert_eq!(
            default_thread_environment_selections(&manager, &cwd),
            vec![TurnEnvironmentSelection {
                environment_id: REMOTE_ENVIRONMENT_ID.to_string(),
                cwd,
            }]
        );
    }

    #[tokio::test]
    async fn toml_default_thread_environment_selections_include_local_and_remote() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            temp_dir.path().join("environments.toml"),
            r#"
[[environments]]
id = "remote"
url = "ws://127.0.0.1:8765"
"#,
        )
        .expect("write environments.toml");
        let cwd = AbsolutePathBuf::current_dir().expect("cwd");
        let manager =
            EnvironmentManager::from_codex_home(temp_dir.path(), Some(test_runtime_paths()))
                .await
                .expect("environment manager");

        assert_eq!(
            default_thread_environment_selections(&manager, &cwd),
            vec![
                TurnEnvironmentSelection {
                    environment_id: LOCAL_ENVIRONMENT_ID.to_string(),
                    cwd: cwd.clone(),
                },
                TurnEnvironmentSelection {
                    environment_id: REMOTE_ENVIRONMENT_ID.to_string(),
                    cwd,
                },
            ]
        );
    }

    #[tokio::test]
    async fn default_thread_environment_selections_empty_when_default_disabled() {
        let cwd = AbsolutePathBuf::current_dir().expect("cwd");
        let manager = EnvironmentManager::without_environments();

        assert_eq!(
            default_thread_environment_selections(&manager, &cwd),
            Vec::<TurnEnvironmentSelection>::new()
        );
    }

    #[tokio::test]
    async fn default_thread_environment_selections_caps_configured_defaults_in_order() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let environments_toml = (0..=MAX_TURN_ENVIRONMENTS)
            .map(|idx| {
                format!(
                    r#"[[environments]]
id = "remote-{idx}"
url = "ws://127.0.0.1:{}"
"#,
                    8765 + idx
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(temp_dir.path().join("environments.toml"), environments_toml)
            .expect("write environments.toml");
        let cwd = AbsolutePathBuf::current_dir().expect("cwd");
        let manager =
            EnvironmentManager::from_codex_home(temp_dir.path(), Some(test_runtime_paths()))
                .await
                .expect("environment manager");

        let selections = default_thread_environment_selections(&manager, &cwd);

        assert_eq!(selections.len(), MAX_TURN_ENVIRONMENTS);
        assert_eq!(
            selections
                .iter()
                .map(|selection| selection.environment_id.as_str())
                .collect::<Vec<_>>(),
            vec![
                LOCAL_ENVIRONMENT_ID,
                "remote-0",
                "remote-1",
                "remote-2",
                "remote-3",
                "remote-4",
                "remote-5",
                "remote-6",
            ]
        );
    }

    #[tokio::test]
    async fn resolve_environment_selections_rejects_duplicate_ids() {
        let cwd = AbsolutePathBuf::current_dir().expect("cwd");
        let manager = EnvironmentManager::default_for_tests();

        let err = resolve_environment_selections(
            &manager,
            &[
                TurnEnvironmentSelection {
                    environment_id: "local".to_string(),
                    cwd: cwd.clone(),
                },
                TurnEnvironmentSelection {
                    environment_id: "local".to_string(),
                    cwd: cwd.join("other"),
                },
            ],
        )
        .expect_err("duplicate environment id should fail");

        assert!(err.to_string().contains("duplicate"));
    }

    #[tokio::test]
    async fn resolve_environment_selections_rejects_too_many_environments() {
        let cwd = AbsolutePathBuf::current_dir().expect("cwd");
        let manager = EnvironmentManager::default_for_tests();
        let selections: Vec<_> = (0..=MAX_TURN_ENVIRONMENTS)
            .map(|idx| TurnEnvironmentSelection {
                environment_id: format!("environment-{idx}"),
                cwd: cwd.clone(),
            })
            .collect();

        let err = resolve_environment_selections(&manager, &selections)
            .expect_err("too many environments should fail before lookup");

        assert!(
            err.to_string()
                .contains("turn environments must be at most")
        );
    }

    #[tokio::test]
    async fn resolve_stored_environment_selections_caps_before_lookup() {
        let cwd = AbsolutePathBuf::current_dir().expect("cwd");
        let manager = EnvironmentManager::default_for_tests();
        for idx in 1..MAX_TURN_ENVIRONMENTS {
            manager
                .upsert_environment(
                    format!("remote-{idx}"),
                    format!("ws://127.0.0.1:{}", 8765 + idx),
                )
                .expect("register environment");
        }
        let mut selections: Vec<_> = std::iter::once(TurnEnvironmentSelection {
            environment_id: LOCAL_ENVIRONMENT_ID.to_string(),
            cwd: cwd.clone(),
        })
        .chain(
            (1..MAX_TURN_ENVIRONMENTS).map(|idx| TurnEnvironmentSelection {
                environment_id: format!("remote-{idx}"),
                cwd: cwd.clone(),
            }),
        )
        .collect();
        selections.push(TurnEnvironmentSelection {
            environment_id: "unknown-after-cap".to_string(),
            cwd,
        });

        let resolved = resolve_stored_environment_selections(&manager, &selections)
            .expect("stored environments should be capped before lookup");

        assert_eq!(resolved.turn_environments.len(), MAX_TURN_ENVIRONMENTS);
        assert_eq!(
            resolved
                .turn_environments
                .last()
                .expect("last resolved environment")
                .environment_id,
            format!("remote-{}", MAX_TURN_ENVIRONMENTS - 1)
        );
    }

    #[tokio::test]
    async fn resolve_stored_environment_selections_keeps_duplicate_validation_within_cap() {
        let cwd = AbsolutePathBuf::current_dir().expect("cwd");
        let manager = EnvironmentManager::default_for_tests();
        let selections = vec![
            TurnEnvironmentSelection {
                environment_id: LOCAL_ENVIRONMENT_ID.to_string(),
                cwd: cwd.clone(),
            },
            TurnEnvironmentSelection {
                environment_id: LOCAL_ENVIRONMENT_ID.to_string(),
                cwd,
            },
        ];

        let err = resolve_stored_environment_selections(&manager, &selections)
            .expect_err("duplicate environment id should fail within cap");

        assert!(err.to_string().contains("duplicate"));
    }

    #[tokio::test]
    async fn resolved_environment_selections_use_first_selection_as_primary() {
        let cwd = AbsolutePathBuf::current_dir().expect("cwd");
        let selected_cwd = cwd.join("selected");
        let manager = EnvironmentManager::default_for_tests();

        let resolved = resolve_environment_selections(
            &manager,
            &[TurnEnvironmentSelection {
                environment_id: "local".to_string(),
                cwd: selected_cwd,
            }],
        )
        .expect("environment selections should resolve");

        assert_eq!(
            resolved
                .primary()
                .expect("primary environment")
                .environment_id,
            "local"
        );
        assert_eq!(resolved.primary().expect("primary environment").shell, None);
    }

    #[tokio::test]
    async fn single_local_environment_cwd_requires_exactly_one_local_environment() {
        let cwd = AbsolutePathBuf::current_dir().expect("cwd");
        let local_manager = EnvironmentManager::default_for_tests();
        let local = resolve_environment_selections(
            &local_manager,
            &[TurnEnvironmentSelection {
                environment_id: LOCAL_ENVIRONMENT_ID.to_string(),
                cwd: cwd.clone(),
            }],
        )
        .expect("local environment should resolve");
        let remote_manager = EnvironmentManager::create_for_tests(
            Some("ws://127.0.0.1:8765".to_string()),
            Some(test_runtime_paths()),
        )
        .await;
        let remote = resolve_environment_selections(
            &remote_manager,
            &[TurnEnvironmentSelection {
                environment_id: REMOTE_ENVIRONMENT_ID.to_string(),
                cwd: cwd.clone(),
            }],
        )
        .expect("remote environment should resolve");
        local_manager
            .upsert_environment(
                REMOTE_ENVIRONMENT_ID.to_string(),
                "ws://127.0.0.1:8765".to_string(),
            )
            .expect("remote environment should register");
        let multiple = resolve_environment_selections(
            &local_manager,
            &[
                TurnEnvironmentSelection {
                    environment_id: LOCAL_ENVIRONMENT_ID.to_string(),
                    cwd: cwd.clone(),
                },
                TurnEnvironmentSelection {
                    environment_id: REMOTE_ENVIRONMENT_ID.to_string(),
                    cwd: cwd.clone(),
                },
            ],
        )
        .expect("multiple environments should resolve");

        assert_eq!(local.single_local_environment_cwd(), Some(&cwd));
        assert_eq!(remote.single_local_environment_cwd(), None);
        assert_eq!(multiple.single_local_environment_cwd(), None);
    }
}
