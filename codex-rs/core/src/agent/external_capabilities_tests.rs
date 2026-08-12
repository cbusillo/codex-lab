use super::*;
use pretty_assertions::assert_eq;

fn backend(args: &[&str]) -> ExternalCommandAgentBackendConfig {
    ExternalCommandAgentBackendConfig {
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
        ..Default::default()
    }
}

#[test]
fn claude_capabilities_use_current_catalog_and_mark_fable_explicit() {
    let capabilities = claude_capabilities(
        Some("2.1.220 (Claude Code)".to_string()),
        b"--model <model>\n--effort <level>\n",
        /*help_truncated*/ false,
    );

    assert!(capabilities.supports_model_selection);
    assert!(capabilities.supports_effort_selection);
    assert_eq!(
        capabilities.effort_levels,
        ["low", "medium", "high", "xhigh", "max"].map(str::to_string)
    );
    assert!(capabilities.models.iter().any(|model| {
        model.selector == "claude-opus-5" && model.model == "claude-opus-5" && !model.explicit_only
    }));
    assert!(capabilities.models.iter().any(|model| {
        model.selector == "claude-fable-5" && model.model == "claude-fable-5" && model.explicit_only
    }));
}

#[test]
fn antigravity_capabilities_produce_provider_qualified_selectors() {
    let capabilities = antigravity_capabilities(
        Some("1.1.9".to_string()),
        b"gemini-3.6-flash-high\ngemini-3.1-pro-low\n",
        /*models_truncated*/ false,
        b"--model Model\n--effort low|medium|high\n--sandbox\n--mode\n--dangerously-skip-permissions\n",
        /*help_truncated*/ false,
    );

    assert_eq!(
        capabilities.models,
        vec![
            ExternalAgentModelCapability {
                selector: "antigravity-gemini-3.6-flash-high".to_string(),
                model: "gemini-3.6-flash-high".to_string(),
                explicit_only: false,
            },
            ExternalAgentModelCapability {
                selector: "antigravity-gemini-3.1-pro-low".to_string(),
                model: "gemini-3.1-pro-low".to_string(),
                explicit_only: false,
            },
        ]
    );
    assert_eq!(
        capabilities.effort_levels,
        ["low", "medium", "high"].map(str::to_string)
    );
    assert_eq!(capabilities.source, ExternalAgentCapabilitySource::LocalCli);
}

#[test]
fn antigravity_read_only_requires_safety_flags() {
    let capabilities = antigravity_capabilities(
        Some("1.1.9".to_string()),
        b"gemini-3.6-flash-high\n",
        /*models_truncated*/ false,
        b"--model Model\n--effort low|medium|high\n",
        /*help_truncated*/ false,
    );

    let mut backend = backend(&[]);
    backend.launch_family = Some("antigravity".to_string());
    let error =
        validate_requested_capabilities(&backend, &[], /*is_read_only*/ true, &capabilities)
            .expect_err("missing AGY sandbox flags should fail read-only preflight");

    assert_eq!(error.kind, ExternalAgentFailureKind::UnsupportedMode);
    assert!(error.to_string().contains("--sandbox"));
}

#[test]
fn antigravity_read_only_preserves_probe_failure_diagnostic() {
    let capabilities = ExternalAgentCapabilities::conservative(
        "antigravity",
        Some("1.1.9".to_string()),
        ExternalAgentFailureDetail::new(
            ExternalAgentFailureKind::MalformedOutput,
            "Antigravity returned malformed model output",
        ),
    );
    let mut backend = backend(&[]);
    backend.launch_family = Some("antigravity".to_string());

    let error =
        validate_requested_capabilities(&backend, &[], /*is_read_only*/ true, &capabilities)
            .expect_err("probe failure should win over missing safety flags");

    assert_eq!(error.kind, ExternalAgentFailureKind::MalformedOutput);
    assert!(error.to_string().contains("malformed model output"));
}

#[test]
fn malformed_and_unbounded_antigravity_models_fail_conservatively() {
    let malformed = antigravity_capabilities(
        /*cli_version*/ None,
        b"Gemini 3.1 Pro\n",
        /*models_truncated*/ false,
        b"--model\n",
        /*help_truncated*/ false,
    );
    assert_eq!(
        malformed.failure.as_ref().map(|failure| failure.kind),
        Some(ExternalAgentFailureKind::MalformedOutput)
    );
    assert!(malformed.models.is_empty());

    let leading_dash = antigravity_capabilities(
        /*cli_version*/ None,
        b"--dangerous-flag\n",
        /*models_truncated*/ false,
        b"--model\n",
        /*help_truncated*/ false,
    );
    assert_eq!(
        leading_dash.failure.as_ref().map(|failure| failure.kind),
        Some(ExternalAgentFailureKind::MalformedOutput)
    );
    assert!(leading_dash.models.is_empty());

    let oversized = (0..=MAX_DISCOVERED_MODELS)
        .map(|index| format!("gemini-test-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let oversized = antigravity_capabilities(
        /*cli_version*/ None,
        oversized.as_bytes(),
        /*models_truncated*/ false,
        b"--model\n",
        /*help_truncated*/ false,
    );
    assert_eq!(
        oversized.failure.as_ref().map(|failure| failure.kind),
        Some(ExternalAgentFailureKind::MalformedOutput)
    );
    assert!(oversized.models.is_empty());
}

#[test]
fn explicit_model_and_effort_requests_are_validated() {
    let capabilities = antigravity_capabilities(
        Some("1.1.9".to_string()),
        b"gemini-3.6-flash-high\n",
        /*models_truncated*/ false,
        b"--model Model\n--effort low|medium|high\n",
        /*help_truncated*/ false,
    );

    validate_requested_capabilities(
        &backend(&["--model", "gemini-3.6-flash-high", "--effort=high"]),
        &[],
        /*is_read_only*/ false,
        &capabilities,
    )
    .expect("reported model and effort should validate");

    let error = validate_requested_capabilities(
        &backend(&["--model", "gemini-missing"]),
        &[],
        /*is_read_only*/ false,
        &capabilities,
    )
    .expect_err("unreported model should fail");
    assert_eq!(error.kind, ExternalAgentFailureKind::UnsupportedMode);
    assert!(error.to_string().contains("gemini-missing"));
    assert!(error.to_string().contains("gemini-3.6-flash-high"));

    let error = validate_requested_capabilities(
        &backend(&["--effort", "max"]),
        &[],
        /*is_read_only*/ false,
        &capabilities,
    )
    .expect_err("unsupported effort should fail");
    assert_eq!(error.kind, ExternalAgentFailureKind::UnsupportedMode);
    assert!(error.to_string().contains("low, medium, high"));
}

#[test]
fn capability_cache_only_reuses_successful_reports() {
    clear_capability_cache();
    let key = ExternalAgentCapabilityCacheKey::new(
        Path::new("/tmp/fake-agy"),
        &[],
        "antigravity",
        Some("1.1.9"),
    );
    let capabilities = antigravity_capabilities(
        Some("1.1.9".to_string()),
        b"gemini-3.6-flash-high\n",
        /*models_truncated*/ false,
        b"--model\n--effort\n",
        /*help_truncated*/ false,
    );

    cache_capabilities(key.clone(), &capabilities);
    let cached = cached_capabilities(&key).expect("successful report should be cached");
    assert_eq!(cached.freshness, ExternalAgentCapabilityFreshness::Cached);
    assert_eq!(cached.models, capabilities.models);

    clear_capability_cache();
    let failed = ExternalAgentCapabilities::conservative(
        "antigravity",
        Some("1.1.9".to_string()),
        ExternalAgentFailureDetail::new(ExternalAgentFailureKind::MalformedOutput, "bad models"),
    );
    cache_capabilities(key.clone(), &failed);
    assert!(cached_capabilities(&key).is_none());
}

#[test]
fn discovery_cache_reuses_failures_during_backoff() {
    clear_capability_cache();
    let backend = backend(&[]);
    let key = ExternalAgentDiscoveryCacheKey::new(&backend, Path::new("/tmp/workspace"));
    let failed = ExternalAgentCapabilities::conservative(
        "antigravity",
        /*cli_version*/ None,
        ExternalAgentFailureDetail::new(ExternalAgentFailureKind::CommandMissing, "missing agy"),
    );

    cache_discovery(key.clone(), &failed);
    let cached = cached_discovery(&key).expect("failed discovery should be backoff-cached");

    assert_eq!(cached.failure, failed.failure);
    assert_eq!(cached.freshness, ExternalAgentCapabilityFreshness::Cached);
    clear_capability_cache();
}
