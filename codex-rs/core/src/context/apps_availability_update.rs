use codex_mcp::CODEX_APPS_MCP_SERVER_NAME;

use super::AppsInstructions;
use super::ContextualUserFragment;

pub(crate) const APPS_UPDATE_OPEN_TAG: &str = "<apps_update>";
pub(crate) const APPS_UPDATE_CLOSE_TAG: &str = "</apps_update>";
const APPS_AVAILABLE_STATE: &str = "state: available";
const APPS_UNAVAILABLE_STATE: &str = "state: unavailable";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppsAvailability {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppsAvailabilityUpdate {
    availability: AppsAvailability,
}

impl AppsAvailabilityUpdate {
    pub(crate) fn new(availability: AppsAvailability) -> Self {
        Self { availability }
    }

    pub(crate) fn availability_from_body(body: &str) -> Option<AppsAvailability> {
        if body.contains(APPS_AVAILABLE_STATE) {
            Some(AppsAvailability::Available)
        } else if body.contains(APPS_UNAVAILABLE_STATE) {
            Some(AppsAvailability::Unavailable)
        } else {
            None
        }
    }
}

impl ContextualUserFragment for AppsAvailabilityUpdate {
    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        (APPS_UPDATE_OPEN_TAG, APPS_UPDATE_CLOSE_TAG)
    }

    fn body(&self) -> String {
        match self.availability {
            AppsAvailability::Available => format!(
                "\n{APPS_AVAILABLE_STATE}\nApps are now available. Follow this guidance while they remain available:{}",
                AppsInstructions::instructions_body()
            ),
            AppsAvailability::Unavailable => format!(
                "\n{APPS_UNAVAILABLE_STATE}\nApps are no longer available. Do not call tools from the `{CODEX_APPS_MCP_SERVER_NAME}` MCP server unless a later apps update says they are available.\n"
            ),
        }
    }
}
