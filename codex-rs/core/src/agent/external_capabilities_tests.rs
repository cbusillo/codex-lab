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
        b"--model <model>\n--effort <level>\n--verbose\n--output-format <format>\n",
        /*help_truncated*/ false,
    );

    assert!(capabilities.supports_model_selection);
    assert!(capabilities.supports_effort_selection);
    assert!(capabilities.supported_flags.contains("--verbose"));
    assert!(capabilities.supported_flags.contains("--output-format"));
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
fn claude_stream_json_requires_both_capability_flags() {
    for help_output in [b"--verbose\n".as_slice(), b"--output-format <format>\n"] {
        let capabilities = claude_capabilities(
            /*cli_version*/ None,
            help_output,
            /*help_truncated*/ false,
        );

        assert_ne!(
            capabilities.supported_flags.contains("--verbose"),
            capabilities.supported_flags.contains("--output-format")
        );
    }
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
fn antigravity_models_parse_tab_separated_rows() {
    let capabilities = antigravity_capabilities(
        Some("1.1.12".to_string()),
        b"gemini-3.6-flash-high\tGemini 3.6 Flash (High)\ngemini-3.1-pro-low\tGemini 3.1 Pro (Low)\n",
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
    assert!(capabilities.failure.is_none());
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
fn antigravity_models_normalize_decorations_and_reject_unbounded_results() {
    let normalized = antigravity_capabilities(
        /*cli_version*/ None,
        b"Available models\n| gemini-3.1-pro | Gemini 3.1 Pro |\n- gemini-3.6-flash (default)\n\x1b[32mgemini-3.6-flash\x1b[0m\n",
        /*models_truncated*/ false,
        b"--model\n",
        /*help_truncated*/ false,
    );
    assert_eq!(
        normalized.models,
        vec![
            ExternalAgentModelCapability {
                selector: "antigravity-gemini-3.1-pro".to_string(),
                model: "gemini-3.1-pro".to_string(),
                explicit_only: false,
            },
            ExternalAgentModelCapability {
                selector: "antigravity-gemini-3.6-flash".to_string(),
                model: "gemini-3.6-flash".to_string(),
                explicit_only: false,
            },
        ]
    );

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
    assert!(malformed.failure.as_ref().is_some_and(|failure| {
        failure.message.as_deref().is_some_and(|message| {
            message.contains("Antigravity selector `antigravity-Gemini 3.1 Pro`")
                && message.contains("rule:")
                && message.contains("Remediation:")
        })
    }));

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
fn antigravity_models_parse_markdown_and_box_tables_without_advertising_headers() {
    let markdown = antigravity_capabilities(
        /*cli_version*/ None,
        b"| Tier | Description |\n| --- | --- |\n| gemini-3.1-pro | Gemini 3.1 Pro |\n",
        /*models_truncated*/ false,
        b"--model\n",
        /*help_truncated*/ false,
    );
    let box_table = antigravity_capabilities(
        /*cli_version*/ None,
        "┌──────────────────┬───────────────┐\n│ Model            │ Description   │\n├──────────────────┼───────────────┤\n│ gemini-3.6-flash │ Gemini Flash  │\n└──────────────────┴───────────────┘\n"
            .as_bytes(),
        /*models_truncated*/ false,
        b"--model\n",
        /*help_truncated*/ false,
    );

    assert_eq!(
        markdown
            .models
            .iter()
            .map(|model| model.model.as_str())
            .collect::<Vec<_>>(),
        vec!["gemini-3.1-pro"]
    );
    assert_eq!(
        box_table
            .models
            .iter()
            .map(|model| model.model.as_str())
            .collect::<Vec<_>>(),
        vec!["gemini-3.6-flash"]
    );
}

#[test]
fn antigravity_models_parse_ascii_grid_tables_without_dropping_rows() {
    let capabilities = antigravity_capabilities(
        /*cli_version*/ None,
        b"+----------------+---------------+\n| Model          | Description   |\n+----------------+---------------+\n| gemini-3-pro   | Gemini 3 Pro  |\n| gemini-3-flash | Gemini Flash  |\n+----------------+---------------+\n",
        /*models_truncated*/ false,
        b"--model\n",
        /*help_truncated*/ false,
    );

    assert_eq!(
        capabilities
            .models
            .iter()
            .map(|model| model.model.as_str())
            .collect::<Vec<_>>(),
        vec!["gemini-3-pro", "gemini-3-flash"]
    );
}

#[test]
fn antigravity_models_do_not_corrupt_non_csi_escape_sequences() {
    let capabilities = antigravity_capabilities(
        /*cli_version*/ None,
        b"\x1b(Bgemini-3.1-pro\n",
        /*models_truncated*/ false,
        b"--model\n",
        /*help_truncated*/ false,
    );

    assert_eq!(capabilities.models.len(), 1);
    assert_eq!(capabilities.models[0].model, "gemini-3.1-pro");
}

#[test]
fn antigravity_models_strip_osc_and_dcs_sequences() {
    let capabilities = antigravity_capabilities(
        /*cli_version*/ None,
        b"\x1b]8;;https://example.com\x1b\\gemini-3.1-pro\x1b]8;;\x07\n\x1bPmetadata\x1b\\gemini-3.6-flash\n",
        /*models_truncated*/ false,
        b"--model\n",
        /*help_truncated*/ false,
    );

    assert_eq!(
        capabilities
            .models
            .iter()
            .map(|model| model.model.as_str())
            .collect::<Vec<_>>(),
        vec!["gemini-3.1-pro", "gemini-3.6-flash"]
    );
    assert!(capabilities.failure.is_none());
}

#[test]
fn antigravity_model_heading_only_output_is_empty() {
    let capabilities = antigravity_capabilities(
        /*cli_version*/ None,
        b"Available models:\n",
        /*models_truncated*/ false,
        b"--model\n",
        /*help_truncated*/ false,
    );

    assert_eq!(
        capabilities.failure.as_ref().map(|failure| failure.kind),
        Some(ExternalAgentFailureKind::EmptyOutput)
    );
}

#[test]
fn active_catalog_age_is_fresh_through_the_ttl_boundary() {
    assert!(active_catalog_age_is_fresh(CAPABILITY_CACHE_TTL));
    assert!(!active_catalog_age_is_fresh(
        CAPABILITY_CACHE_TTL + Duration::from_nanos(1)
    ));
}

#[test]
fn antigravity_models_reject_single_token_status_noise() {
    let capabilities = antigravity_capabilities(
        /*cli_version*/ None,
        b"Loading...\nError:\nLoading\nDone\nUnauthenticated\nNone\n",
        /*models_truncated*/ false,
        b"--model\n",
        /*help_truncated*/ false,
    );

    assert_eq!(
        capabilities.failure.as_ref().map(|failure| failure.kind),
        Some(ExternalAgentFailureKind::MalformedOutput)
    );
    assert!(capabilities.models.is_empty());
}

#[test]
fn antigravity_models_do_not_drop_plain_models_before_dividers() {
    let capabilities = antigravity_capabilities(
        /*cli_version*/ None,
        b"gemini-3.1-pro\n---\ngemini-3.6-flash\n",
        /*models_truncated*/ false,
        b"--model\n",
        /*help_truncated*/ false,
    );

    assert_eq!(
        capabilities
            .models
            .iter()
            .map(|model| model.model.as_str())
            .collect::<Vec<_>>(),
        vec!["gemini-3.1-pro", "gemini-3.6-flash"]
    );
}

#[test]
fn antigravity_models_do_not_treat_data_rows_before_dividers_as_headers() {
    let capabilities = antigravity_capabilities(
        /*cli_version*/ None,
        b"gemini-3-pro   | Gemini 3 Pro\ngemini-3-flash | Gemini Flash\n--------------------------------\n",
        /*models_truncated*/ false,
        b"--model\n",
        /*help_truncated*/ false,
    );

    assert_eq!(
        capabilities
            .models
            .iter()
            .map(|model| model.model.as_str())
            .collect::<Vec<_>>(),
        vec!["gemini-3-pro", "gemini-3-flash"]
    );
}

#[test]
fn antigravity_catalog_requires_launchable_model_selection() {
    clear_capability_cache();
    let workspace = tempfile::tempdir().expect("tempdir");
    let mut backend = backend(&[]);
    backend.command = "catalog-requires-model-selection".to_string();
    backend.launch_family = Some("antigravity".to_string());
    let mut capabilities = antigravity_capabilities(
        /*cli_version*/ None,
        b"gemini-3.1-pro\n",
        /*models_truncated*/ false,
        b"--effort high\n",
        /*help_truncated*/ false,
    );

    capabilities.supports_model_selection = true;
    record_active_capability_catalog(&backend, workspace.path(), &capabilities);
    assert_eq!(
        discovered_antigravity_selectors(&backend, workspace.path()).len(),
        1
    );

    capabilities.supports_model_selection = false;
    record_active_capability_catalog(&backend, workspace.path(), &capabilities);
    assert!(discovered_antigravity_selectors(&backend, workspace.path()).is_empty());

    capabilities.supports_model_selection = true;
    capabilities.failure = None;
    record_active_capability_catalog(&backend, workspace.path(), &capabilities);
    assert_eq!(
        discovered_antigravity_selectors(&backend, workspace.path()).len(),
        1
    );

    capabilities.failure = Some(ExternalAgentFailureDetail::new(
        ExternalAgentFailureKind::MalformedOutput,
        "failed discovery",
    ));
    record_active_capability_catalog(&backend, workspace.path(), &capabilities);
    assert!(discovered_antigravity_selectors(&backend, workspace.path()).is_empty());
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
