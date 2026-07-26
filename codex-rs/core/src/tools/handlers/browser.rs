use crate::browser::BrowserArgs;
use crate::browser::execute_browser_action;
use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::browser_spec::BROWSER_TOOL_NAME;
use crate::tools::handlers::browser_spec::create_browser_tool;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ImageDetail;
use codex_tools::ToolName;
use codex_tools::ToolSpec;

pub struct BrowserHandler {
    full_cdp_access: bool,
}

impl BrowserHandler {
    pub(crate) fn new(full_cdp_access: bool) -> Self {
        Self { full_cdp_access }
    }
}

impl ToolExecutor<ToolInvocation> for BrowserHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(BROWSER_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        create_browser_tool(self.full_cdp_access)
    }

    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        let full_cdp_access = self.full_cdp_access;
        Box::pin(async move {
            let ToolInvocation { turn, payload, .. } = invocation;
            let arguments = match payload {
                ToolPayload::Function { arguments } => arguments,
                _ => {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "{BROWSER_TOOL_NAME} handler received unsupported payload"
                    )));
                }
            };
            let args: BrowserArgs = parse_arguments(&arguments)?;
            let result =
                execute_browser_action(args, turn.config.http_client_factory(), full_cdp_access)
                    .await;
            let content = serde_json::to_string(&result.response).map_err(|err| {
                FunctionCallError::Fatal(format!(
                    "failed to serialize {BROWSER_TOOL_NAME} response: {err}"
                ))
            })?;
            let mut items = vec![FunctionCallOutputContentItem::InputText { text: content }];
            if result.success
                && let Some(image) = result.image
            {
                items.push(FunctionCallOutputContentItem::InputImage {
                    image_url: image.data_url(),
                    detail: Some(ImageDetail::High),
                });
            }
            Ok(boxed_tool_output(FunctionToolOutput::from_content(
                items,
                Some(result.success),
            )))
        })
    }
}

impl CoreToolRuntime for BrowserHandler {}
