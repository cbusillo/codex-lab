//! Opt-in limits for one external worker invocation. Native ownership stays in the runner.

use crate::config::Config;
use crate::context::BoundedWorkerInstructions;
use crate::context::ContextualUserFragment;
use crate::session::step_context::StepContext;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashSet;
use std::path::Component;
use std::path::Path;
use std::time::Duration;
use tokio::time::Instant;

const MAX_INPUT_BYTES: usize = 8 * 1024;
const MAX_RESULT_BYTES: usize = 8 * 1024;
const MAX_TIMEOUT_MS: u64 = 300_000;

/// Bound cancellation-safe preparation; committed engine registrations must still be finalized.
pub(crate) async fn prepare<T>(
    worker: Option<&BoundedWorker>,
    preparation: impl std::future::Future<Output = T>,
) -> codex_protocol::error::Result<T> {
    let Some(worker) = worker else {
        return Ok(preparation.await);
    };
    let expired = || {
        codex_protocol::error::CodexErr::UnsupportedOperation(
            "bounded worker deadline expired during preparation; no process was launched"
                .to_string(),
        )
    };
    if Instant::now() >= worker.deadline {
        return Err(expired());
    }
    tokio::time::timeout_at(worker.deadline, preparation)
        .await
        .map_err(|_| expired())
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BoundedWorkerRequest {
    pub(crate) timeout_ms: u64,
    pub(crate) max_input_bytes: usize,
    pub(crate) max_result_bytes: usize,
    pub(crate) context: WorkerContext,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum WorkerContext {
    Text,
    Code { paths: Vec<String> },
}

#[derive(Clone, Debug)]
pub(crate) struct BoundedWorker {
    pub(crate) deadline: Instant,
    pub(crate) limits: BoundedWorkerLimits,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct BoundedWorkerLimits {
    pub(crate) timeout_ms: u64,
    pub(crate) max_input_bytes: usize,
    pub(crate) max_result_bytes: usize,
}

impl BoundedWorkerRequest {
    pub(crate) fn start(&self, started: Instant) -> Result<BoundedWorker, &'static str> {
        if !(1..=MAX_TIMEOUT_MS).contains(&self.timeout_ms)
            || !(1..=MAX_INPUT_BYTES).contains(&self.max_input_bytes)
            || !(256..=MAX_RESULT_BYTES).contains(&self.max_result_bytes)
        {
            return Err(
                "bounded_worker requires timeout_ms 1..300000, max_input_bytes 1..8192, and max_result_bytes 256..8192",
            );
        }
        if let WorkerContext::Code { paths } = &self.context
            && (paths.is_empty()
                || paths.len() > 32
                || paths.iter().any(|path| {
                    path.is_empty()
                        || path.len() > 1024
                        || Path::new(path)
                            .components()
                            .any(|part| !matches!(part, Component::Normal(_)))
                }))
        {
            return Err(
                "bounded code workers require 1..32 workspace-relative file paths without parent traversal",
            );
        }
        Ok(BoundedWorker {
            deadline: started + Duration::from_millis(self.timeout_ms),
            limits: BoundedWorkerLimits {
                timeout_ms: self.timeout_ms,
                max_input_bytes: self.max_input_bytes,
                max_result_bytes: self.max_result_bytes,
            },
        })
    }

    pub(crate) async fn message(
        &self,
        step: &StepContext,
        config: &Config,
        task: &str,
    ) -> Result<String, &'static str> {
        if task.len() > self.max_input_bytes {
            return Err("bounded worker task exceeds max_input_bytes; nothing was launched");
        }
        let message = match &self.context {
            WorkerContext::Text => task.to_string(),
            WorkerContext::Code { paths } => {
                let loaded = step.loaded_agents_md.as_deref();
                if loaded.is_some_and(|loaded| !loaded.is_complete()) {
                    return Err(
                        "bounded worker instruction loading was incomplete; nothing was launched",
                    );
                }
                let environment = step
                    .environments
                    .primary()
                    .ok_or("bounded worker instruction environment is unavailable")?;
                if environment.cwd().to_abs_path().ok().as_ref() != Some(&config.cwd) {
                    return Err(
                        "bounded worker cannot prove instructions for a different workspace",
                    );
                }
                let filesystem = environment.environment.get_filesystem();
                let sandbox = (!environment
                    .permission_profile()
                    .file_system_sandbox_policy()
                    .has_full_disk_read_access())
                .then(|| environment.sandbox_context(/*additional_permissions*/ None));
                let sources = loaded
                    .into_iter()
                    .flat_map(crate::agents_md::LoadedAgentsMd::sources)
                    .collect::<HashSet<_>>();
                let names = crate::agents_md::candidate_filenames(config);
                for path in paths {
                    let path = Path::new(path);
                    if path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| names.contains(&name))
                    {
                        return Err(
                            "bounded worker cannot prove baseline instructions for instruction-file changes",
                        );
                    }
                    let directory = config
                        .cwd
                        .join(path.parent().unwrap_or(Path::new("")))
                        .map_err(|_| "bounded worker task path is invalid")?;
                    let required = crate::agents_md::agents_md_paths(
                        config,
                        &codex_utils_path_uri::PathUri::from_abs_path(&directory),
                        filesystem.as_ref(),
                        sandbox.as_ref(),
                        codex_file_system::FindUpErrorPolicy::Propagate,
                    )
                    .await
                    .map_err(|_| "bounded worker could not discover applicable instructions")?;
                    if required.iter().any(|path| !sources.contains(path)) {
                        return Err(
                            "bounded worker is missing applicable instructions; nothing was launched",
                        );
                    }
                }
                let context = BoundedWorkerInstructions {
                    paths: paths.clone(),
                    instructions: loaded.map(|loaded| loaded.text()).unwrap_or_default(),
                    developer_instructions: config.developer_instructions.clone(),
                };
                format!("{}\n\n{task}", context.render())
            }
        };
        if message.len() > self.max_input_bytes {
            return Err(
                "complete bounded worker instructions and task exceed max_input_bytes; nothing was launched",
            );
        }
        Ok(message)
    }
}

impl BoundedWorker {
    pub(crate) fn restrict_timeout(&mut self, started: Instant, configured_ms: u64) {
        self.limits.timeout_ms = self.limits.timeout_ms.min(configured_ms);
        self.deadline = self
            .deadline
            .min(started + Duration::from_millis(configured_ms));
    }

    pub(crate) fn bound_result(&self, message: &str) -> String {
        if message.len() <= self.limits.max_result_bytes {
            return message.to_string();
        }
        let marker = "[bounded worker result truncated]\n";
        let mut start = message.len() - (self.limits.max_result_bytes - marker.len());
        while !message.is_char_boundary(start) {
            start += 1;
        }
        format!("{marker}{}", &message[start..])
    }
}

#[cfg(test)]
#[path = "bounded_worker_tests.rs"]
mod tests;
