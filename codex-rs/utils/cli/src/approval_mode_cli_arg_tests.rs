use super::ApprovalModeCliArg;
use clap::Parser;
use clap::ValueEnum;
use codex_protocol::protocol::AskForApproval;
use pretty_assertions::assert_eq;

#[derive(Parser, Debug)]
struct TestCli {
    #[arg(long = "ask-for-approval", short = 'a')]
    approval_policy: Option<ApprovalModeCliArg>,
}

fn parse(args: &[&str]) -> Option<ApprovalModeCliArg> {
    TestCli::try_parse_from(args)
        .expect("approval mode should parse")
        .approval_policy
}

#[test]
fn deprecated_on_failure_alias_maps_to_on_request() {
    for args in [
        ["test", "--ask-for-approval", "on-failure"],
        ["test", "-a", "on-failure"],
    ] {
        let parsed = parse(&args).expect("approval policy present");
        assert_eq!(AskForApproval::from(parsed), AskForApproval::OnRequest);
    }
}

#[test]
fn documented_values_still_parse() {
    let parsed = [
        ("untrusted", AskForApproval::UnlessTrusted),
        ("on-request", AskForApproval::OnRequest),
        ("never", AskForApproval::Never),
    ]
    .map(|(value, expected)| {
        let parsed = parse(&["test", "--ask-for-approval", value]).expect("approval policy");
        (AskForApproval::from(parsed), expected)
    });

    assert_eq!(
        parsed,
        [
            (AskForApproval::UnlessTrusted, AskForApproval::UnlessTrusted),
            (AskForApproval::OnRequest, AskForApproval::OnRequest),
            (AskForApproval::Never, AskForApproval::Never),
        ]
    );
}

/// The alias exists only for backward compatibility, so it must stay out of
/// `--help` and shell completions.
#[test]
fn deprecated_alias_is_not_advertised() {
    let advertised: Vec<String> = ApprovalModeCliArg::value_variants()
        .iter()
        .filter_map(ValueEnum::to_possible_value)
        .filter(|value| !value.is_hide_set())
        .map(|value| value.get_name().to_string())
        .collect();

    assert_eq!(advertised, vec!["untrusted", "on-request", "never"]);
}
