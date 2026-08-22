//! Standard type to use with the `--approval-mode` CLI option.

use clap::ValueEnum;

use codex_protocol::protocol::AskForApproval;

#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ApprovalModeCliArg {
    /// The model decides when to ask the user for approval.
    ///
    /// `on-failure` is a deprecated alias kept for backward compatibility with
    /// older command lines. It is accepted but intentionally hidden from help,
    /// mirroring the `on-failure` serde alias on
    /// [`AskForApproval::OnRequest`].
    #[value(alias = "on-failure")]
    OnRequest,

    /// Never ask for user approval
    /// Execution failures are immediately returned to the model.
    Never,
}

#[cfg(test)]
#[path = "approval_mode_cli_arg_tests.rs"]
mod tests;

impl From<ApprovalModeCliArg> for AskForApproval {
    fn from(value: ApprovalModeCliArg) -> Self {
        match value {
            ApprovalModeCliArg::OnRequest => AskForApproval::OnRequest,
            ApprovalModeCliArg::Never => AskForApproval::Never,
        }
    }
}
