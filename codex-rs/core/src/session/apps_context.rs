use super::*;
use crate::compact::insert_initial_context_before_last_real_user_or_summary;
use crate::context::AppsInstructions;
use crate::context::ContextualUserFragment;

impl Session {
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "MCP app context rendering reads through the session-owned manager guard"
    )]
    pub(crate) async fn insert_current_apps_instructions(
        &self,
        turn_context: &TurnContext,
        prompt_input: Vec<ResponseItem>,
    ) -> Vec<ResponseItem> {
        if !turn_context.config.include_apps_instructions || !turn_context.apps_enabled() {
            return prompt_input;
        }

        let mcp_connection_manager = self.services.mcp_connection_manager.read().await;
        let accessible_and_enabled_connectors =
            connectors::list_accessible_and_enabled_connectors_from_manager(
                &mcp_connection_manager,
                &turn_context.config,
            )
            .await;
        let Some(apps_instructions) =
            AppsInstructions::from_connectors(&accessible_and_enabled_connectors)
        else {
            return prompt_input;
        };

        insert_initial_context_before_last_real_user_or_summary(
            prompt_input,
            vec![ContextualUserFragment::into(apps_instructions)],
        )
    }
}
