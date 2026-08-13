//! Shared command-line flags used by both interactive and non-interactive Codex entry points.

use crate::CliConfigOverrides;
use crate::SandboxModeCliArg;
use clap::Args;
use codex_protocol::config_types::ProfileV2Name;
use std::path::PathBuf;

#[derive(Args, Clone, Debug, Default)]
pub struct SharedCliOptions {
    /// Optional image(s) to attach to the initial prompt.
    #[arg(
        long = "image",
        short = 'i',
        value_name = "FILE",
        value_delimiter = ',',
        num_args = 1..
    )]
    pub images: Vec<PathBuf>,

    /// Model the agent should use.
    #[arg(long, short = 'm')]
    pub model: Option<String>,

    /// Use open-source provider.
    #[arg(long = "oss", default_value_t = false)]
    pub oss: bool,

    /// Specify which local provider to use (lmstudio or ollama).
    /// If not specified with --oss, will use config default or show selection.
    #[arg(long = "local-provider")]
    pub oss_provider: Option<String>,

    /// Layer $CODEX_LAB_HOME/<name>.config.toml on top of the base user config.
    #[arg(long = "profile", short = 'p')]
    pub config_profile_v2: Option<ProfileV2Name>,

    /// Use credentials from $CODEX_LAB_HOME/auth-profiles/<name>/auth.json for this invocation.
    #[arg(long = "auth-profile", value_name = "NAME")]
    pub auth_profile: Option<String>,

    /// Select the sandbox policy to use when executing model-generated shell
    /// commands.
    #[arg(long = "sandbox", short = 's')]
    pub sandbox_mode: Option<SandboxModeCliArg>,

    /// Route approval requests through automatic review using the workspace-write sandbox.
    #[arg(
        long = "approve-for-me",
        alias = "not-so-yolo",
        default_value_t = false,
        conflicts_with_all = ["sandbox_mode", "dangerously_bypass_approvals_and_sandbox"]
    )]
    pub auto_review: bool,

    /// Skip all confirmation prompts and execute commands without sandboxing.
    /// EXTREMELY DANGEROUS. Intended solely for running in environments that are externally sandboxed.
    #[arg(
        long = "dangerously-bypass-approvals-and-sandbox",
        alias = "yolo",
        default_value_t = false
    )]
    pub dangerously_bypass_approvals_and_sandbox: bool,

    /// Run enabled hooks without requiring persisted hook trust for this invocation.
    /// DANGEROUS. Intended only for automation that already vets hook sources.
    #[arg(long = "dangerously-bypass-hook-trust", default_value_t = false)]
    pub bypass_hook_trust: bool,

    /// Tell the agent to use the specified directory as its working root.
    #[clap(long = "cd", short = 'C', value_name = "DIR")]
    pub cwd: Option<PathBuf>,

    /// Additional directories that should be writable alongside the primary workspace.
    #[arg(long = "add-dir", value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
    pub add_dir: Vec<PathBuf>,

    /// Exact runtime workspace roots. Unlike --add-dir, this replaces the
    /// implicit cwd root and is intended for split-root workspaces.
    #[arg(
        long = "workspace-root",
        value_name = "DIR",
        value_hint = clap::ValueHint::DirPath,
        conflicts_with = "add_dir"
    )]
    pub workspace_root: Vec<PathBuf>,
}

impl SharedCliOptions {
    pub fn take_auto_review_config_overrides(&mut self, overrides: &mut CliConfigOverrides) {
        if self.auto_review {
            overrides
                .raw_overrides
                .push(r#"approvals_reviewer="auto_review""#.to_string());
            overrides
                .raw_overrides
                .push(r#"approval_policy="on-request""#.to_string());
            overrides
                .raw_overrides
                .push(r#"sandbox_mode="workspace-write""#.to_string());
            self.auto_review = false;
        }
    }

    pub fn validate_workspace_root_mode(&self) -> Result<(), &'static str> {
        if !self.workspace_root.is_empty() && !self.add_dir.is_empty() {
            return Err("--workspace-root cannot be combined with --add-dir");
        }
        if !self.workspace_root.is_empty() && self.dangerously_bypass_approvals_and_sandbox {
            return Err(
                "--workspace-root cannot be combined with --dangerously-bypass-approvals-and-sandbox",
            );
        }
        if !self.workspace_root.is_empty()
            && !self.auto_review
            && !matches!(self.sandbox_mode, Some(SandboxModeCliArg::WorkspaceWrite))
        {
            return Err("--workspace-root requires --sandbox workspace-write");
        }
        Ok(())
    }

    pub fn inherit_exec_root_options(&mut self, root: &Self) {
        let self_selected_sandbox_mode = self.sandbox_mode.is_some()
            || self.auto_review
            || self.dangerously_bypass_approvals_and_sandbox;
        let Self {
            images,
            model,
            oss,
            oss_provider,
            config_profile_v2,
            auth_profile,
            sandbox_mode,
            auto_review,
            dangerously_bypass_approvals_and_sandbox,
            bypass_hook_trust,
            cwd,
            add_dir,
            workspace_root,
        } = self;
        let Self {
            images: root_images,
            model: root_model,
            oss: root_oss,
            oss_provider: root_oss_provider,
            config_profile_v2: root_config_profile_v2,
            auth_profile: root_auth_profile,
            sandbox_mode: root_sandbox_mode,
            auto_review: root_auto_review,
            dangerously_bypass_approvals_and_sandbox: root_dangerously_bypass_approvals_and_sandbox,
            bypass_hook_trust: root_bypass_hook_trust,
            cwd: root_cwd,
            add_dir: root_add_dir,
            workspace_root: root_workspace_root,
        } = root;

        if model.is_none() {
            model.clone_from(root_model);
        }
        if *root_oss {
            *oss = true;
        }
        if oss_provider.is_none() {
            oss_provider.clone_from(root_oss_provider);
        }
        if config_profile_v2.is_none() {
            config_profile_v2.clone_from(root_config_profile_v2);
        }
        if auth_profile.is_none() {
            auth_profile.clone_from(root_auth_profile);
        }
        if !self_selected_sandbox_mode {
            *sandbox_mode = *root_sandbox_mode;
            *auto_review = *root_auto_review;
            *dangerously_bypass_approvals_and_sandbox =
                *root_dangerously_bypass_approvals_and_sandbox;
        }
        if !*bypass_hook_trust {
            *bypass_hook_trust = *root_bypass_hook_trust;
        }
        if cwd.is_none() {
            cwd.clone_from(root_cwd);
        }
        if !root_images.is_empty() {
            let mut merged_images = root_images.clone();
            merged_images.append(images);
            *images = merged_images;
        }
        if !root_add_dir.is_empty() {
            let mut merged_add_dir = root_add_dir.clone();
            merged_add_dir.append(add_dir);
            *add_dir = merged_add_dir;
        }
        if !root_workspace_root.is_empty() {
            let mut merged_workspace_root = root_workspace_root.clone();
            merged_workspace_root.append(workspace_root);
            *workspace_root = merged_workspace_root;
        }
    }

    pub fn apply_subcommand_overrides(&mut self, subcommand: Self) {
        let subcommand_selected_sandbox_mode = subcommand.sandbox_mode.is_some()
            || subcommand.auto_review
            || subcommand.dangerously_bypass_approvals_and_sandbox;
        let Self {
            images,
            model,
            oss,
            oss_provider,
            config_profile_v2,
            auth_profile,
            sandbox_mode,
            auto_review,
            dangerously_bypass_approvals_and_sandbox,
            bypass_hook_trust,
            cwd,
            add_dir,
            workspace_root,
        } = subcommand;

        if let Some(model) = model {
            self.model = Some(model);
        }
        if oss {
            self.oss = true;
        }
        if let Some(oss_provider) = oss_provider {
            self.oss_provider = Some(oss_provider);
        }
        if let Some(config_profile_v2) = config_profile_v2 {
            self.config_profile_v2 = Some(config_profile_v2);
        }
        if let Some(auth_profile) = auth_profile {
            self.auth_profile = Some(auth_profile);
        }
        if subcommand_selected_sandbox_mode {
            self.sandbox_mode = sandbox_mode;
            self.auto_review = auto_review;
            self.dangerously_bypass_approvals_and_sandbox =
                dangerously_bypass_approvals_and_sandbox;
        }
        if bypass_hook_trust {
            self.bypass_hook_trust = true;
        }
        if let Some(cwd) = cwd {
            self.cwd = Some(cwd);
        }
        if !images.is_empty() {
            self.images = images;
        }
        if !add_dir.is_empty() {
            self.add_dir.extend(add_dir);
        }
        if !workspace_root.is_empty() {
            self.workspace_root = workspace_root;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(flatten)]
        shared: SharedCliOptions,
    }

    #[test]
    fn workspace_root_is_repeatable() {
        let cli = TestCli::try_parse_from([
            "test",
            "--sandbox",
            "workspace-write",
            "--workspace-root",
            "tenant",
            "--workspace-root",
            "devkit",
        ])
        .expect("workspace roots should parse");

        assert_eq!(
            cli.shared.workspace_root,
            vec![PathBuf::from("tenant"), PathBuf::from("devkit")]
        );
        assert!(cli.shared.validate_workspace_root_mode().is_ok());
    }

    #[test]
    fn workspace_root_conflicts_with_add_dir() {
        let error =
            TestCli::try_parse_from(["test", "--workspace-root", "tenant", "--add-dir", "extra"])
                .expect_err("workspace-root and add-dir should conflict");

        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn inherited_root_modes_cannot_be_mixed_across_exec_scopes() {
        let mut subcommand = SharedCliOptions {
            add_dir: vec![PathBuf::from("extra")],
            ..Default::default()
        };
        let root = SharedCliOptions {
            sandbox_mode: Some(SandboxModeCliArg::WorkspaceWrite),
            workspace_root: vec![PathBuf::from("tenant")],
            ..Default::default()
        };

        subcommand.inherit_exec_root_options(&root);

        assert_eq!(
            subcommand.validate_workspace_root_mode(),
            Err("--workspace-root cannot be combined with --add-dir")
        );
    }

    #[test]
    fn workspace_root_requires_explicit_workspace_write() {
        let shared = SharedCliOptions {
            workspace_root: vec![PathBuf::from("tenant")],
            ..Default::default()
        };

        assert_eq!(
            shared.validate_workspace_root_mode(),
            Err("--workspace-root requires --sandbox workspace-write")
        );
    }

    #[test]
    fn workspace_root_rejects_dangerous_sandbox_bypass() {
        let shared = SharedCliOptions {
            sandbox_mode: Some(SandboxModeCliArg::WorkspaceWrite),
            dangerously_bypass_approvals_and_sandbox: true,
            workspace_root: vec![PathBuf::from("tenant")],
            ..Default::default()
        };

        assert_eq!(
            shared.validate_workspace_root_mode(),
            Err(
                "--workspace-root cannot be combined with --dangerously-bypass-approvals-and-sandbox"
            )
        );
    }

    #[test]
    fn auth_profile_parses_and_is_inherited_by_subcommand_scopes() {
        let cli = TestCli::try_parse_from(["test", "--auth-profile", "work"])
            .expect("auth profile should parse");
        assert_eq!(cli.shared.auth_profile.as_deref(), Some("work"));

        let mut subcommand = SharedCliOptions::default();
        subcommand.inherit_exec_root_options(&cli.shared);
        assert_eq!(subcommand.auth_profile.as_deref(), Some("work"));
    }

    #[test]
    fn subcommand_auth_profile_overrides_root_auth_profile() {
        let mut root = SharedCliOptions {
            auth_profile: Some("work".to_string()),
            ..Default::default()
        };
        let subcommand = SharedCliOptions {
            auth_profile: Some("personal".to_string()),
            ..Default::default()
        };

        root.apply_subcommand_overrides(subcommand);

        assert_eq!(root.auth_profile.as_deref(), Some("personal"));
    }
}
