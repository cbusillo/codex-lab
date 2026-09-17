use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::tools::registry::ToolRegistry;
use crate::tools::requested_tool_mode;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::WarningEvent;
use codex_tools::LoadableToolSpec;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ToolExposure;
use codex_tools::ToolSpec;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;

const NAMESPACES: u8 = 1;
const CUSTOM_TOOLS: u8 = 2;
const CODE_MODE: u8 = 4;

/// Bounded, session-scoped diagnostics collected during synchronous tool planning
/// and delivered through the client event stream before the model request.
#[derive(Debug, Default)]
pub(crate) struct DroppedToolSurfaceWarnings {
    detected: AtomicU8,
    emitted: AtomicU8,
}

impl DroppedToolSurfaceWarnings {
    pub(crate) fn custom_tool_dropped(&self) {
        self.detected.fetch_or(CUSTOM_TOOLS, Ordering::Relaxed);
    }

    pub(crate) fn filter_search_sources(
        &self,
        turn_context: &TurnContext,
        registry: &mut ToolRegistry,
    ) {
        if turn_context.provider.capabilities().custom_tools {
            return;
        }
        for tool in registry
            .entries_mut()
            .filter(|tool| tool.exposure.is_deferred())
        {
            let contains_custom_tool = tool.runtime.search_info().is_some_and(|info| {
                match info.entry.to_loadable_spec() {
                    LoadableToolSpec::Namespace(namespace) => namespace
                        .tools
                        .iter()
                        .any(|tool| matches!(tool, ResponsesApiNamespaceTool::Custom(_))),
                    LoadableToolSpec::Function(_) => false,
                }
            });
            if contains_custom_tool {
                tool.exposure = ToolExposure::Hidden;
                self.custom_tool_dropped();
            }
        }
    }

    pub(crate) fn filter_specs(
        &self,
        turn_context: &TurnContext,
        mut specs: Vec<ToolSpec>,
    ) -> Vec<ToolSpec> {
        let capabilities = turn_context.provider.capabilities();
        specs.retain_mut(|spec| match spec {
            ToolSpec::Namespace(namespace) => {
                if !capabilities.namespace_tools {
                    self.detected.fetch_or(NAMESPACES, Ordering::Relaxed);
                    return false;
                }
                if !capabilities.custom_tools {
                    namespace.tools.retain(|tool| match tool {
                        ResponsesApiNamespaceTool::Custom(_) => {
                            self.custom_tool_dropped();
                            false
                        }
                        ResponsesApiNamespaceTool::Function(_) => true,
                    });
                }
                !namespace.tools.is_empty()
            }
            ToolSpec::Freeform(_) if !capabilities.custom_tools => {
                self.custom_tool_dropped();
                false
            }
            ToolSpec::Function(_)
            | ToolSpec::Freeform(_)
            | ToolSpec::ToolSearch { .. }
            | ToolSpec::WebSearch { .. } => true,
        });
        specs
    }
}

pub(crate) async fn emit_pending_warnings(
    session: &Session,
    turn_context: &TurnContext,
    model_info: &ModelInfo,
) {
    if !turn_context.config.tools_enabled {
        return;
    }
    let warnings = session
        .services
        .thread_extension_data
        .get_or_init(DroppedToolSurfaceWarnings::default);
    if !turn_context.provider.capabilities().custom_tools
        && requested_tool_mode(turn_context, model_info) != ToolMode::Direct
    {
        warnings.detected.fetch_or(CODE_MODE, Ordering::Relaxed);
    }
    let detected = warnings.detected.load(Ordering::Relaxed);
    let pending = detected & !warnings.emitted.fetch_or(detected, Ordering::Relaxed);
    for (kind, explanation) in [
        (
            NAMESPACES,
            "Namespaced tool groups, including MCP tools, are unavailable because this provider does not support namespace tools. Supported tools remain available as flat functions.",
        ),
        (
            CUSTOM_TOOLS,
            "Freeform custom tools, including apply_patch when enabled, are unavailable because this provider does not support custom tools. Use shell tools for file edits.",
        ),
        (
            CODE_MODE,
            "Code Mode requires custom tool support. Using direct function tools instead for this provider.",
        ),
    ] {
        if pending & kind != 0 {
            let message = format!("{}: {explanation}", turn_context.provider.info().name);
            tracing::warn!("{message}");
            session
                .send_event(turn_context, EventMsg::Warning(WarningEvent { message }))
                .await;
        }
    }
}

#[cfg(test)]
#[path = "provider_tool_surface_tests.rs"]
mod tests;
