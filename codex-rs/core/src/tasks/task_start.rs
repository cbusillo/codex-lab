use std::future::Future;
use std::sync::Arc;

use codex_protocol::protocol::TokenUsage;
use futures::FutureExt;
use futures::future::BoxFuture;
use futures::future::Shared;

use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

use super::AnySessionTask;

/// Runs turn-start lifecycle exactly once after prior finalization completes.
///
/// The shared future is intentionally awaited by both the normal task driver
/// and abort cleanup so a parked task still emits start before its abort.
#[derive(Clone)]
pub(crate) struct TaskStart {
    start: Shared<BoxFuture<'static, ()>>,
}

impl TaskStart {
    pub(crate) fn new(
        prior_finalization: impl Future<Output = ()> + Send + 'static,
        session: Arc<Session>,
        turn_context: Arc<TurnContext>,
        task: Arc<dyn AnySessionTask>,
        token_usage_at_turn_start: TokenUsage,
    ) -> Self {
        let start = async move {
            prior_finalization.await;
            session
                .emit_turn_start_lifecycle(turn_context.as_ref(), &token_usage_at_turn_start)
                .await;
            task.start(session, turn_context).await;
        }
        .boxed()
        .shared();
        Self { start }
    }

    pub(crate) async fn run(&self) {
        self.start.clone().await;
    }
}
