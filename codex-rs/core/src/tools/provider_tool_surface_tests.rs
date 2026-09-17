use super::DroppedToolSurfaceWarnings;
use crate::session::tests::make_session_and_context;
use crate::session::tests::update_turn_settings_for_test;
use crate::tools::handlers::ApplyPatchHandler;
use crate::tools::handlers::CurrentTimeHandler;
use crate::tools::registry::ToolRegistry;
use crate::tools::spec_plan::finalize_tool_router;
use codex_model_provider::create_model_provider;
use codex_tools::ToolExposure;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use pretty_assertions::assert_eq;
use std::sync::Arc;

#[tokio::test]
async fn unsupported_custom_tools_cannot_be_loaded_through_search() {
    let (_session, mut turn) = make_session_and_context().await;
    let mut provider = turn.provider.info().clone();
    provider.capabilities.custom_tools = false;
    turn.provider = create_model_provider(provider, turn.auth_manager.clone());
    update_turn_settings_for_test(&mut turn, |settings| {
        Arc::make_mut(&mut settings.model_info).supports_search_tool = true;
    });
    let warnings = DroppedToolSurfaceWarnings::default();
    let mut registry = ToolRegistry::default();
    registry.add_with_exposure(
        ApplyPatchHandler::new(/*include_environment_id*/ false),
        ToolExposure::Deferred,
    );
    registry.add_with_exposure(CurrentTimeHandler, ToolExposure::Deferred);
    let router = finalize_tool_router(
        &turn,
        turn.model_info(),
        registry,
        Vec::new(),
        &Default::default(),
        &warnings,
    )
    .expect("compatible tool plan");
    assert_eq!(
        [
            router.tool_exposure_for_test(&ToolName::plain("apply_patch")),
            router.tool_exposure_for_test(&ToolName::namespaced("clock", "curr_time"))
        ],
        [Some(ToolExposure::Hidden), Some(ToolExposure::Deferred)]
    );
    assert!(matches!(
        router.model_visible_specs().as_ref(),
        [ToolSpec::ToolSearch { .. }]
    ));
}
