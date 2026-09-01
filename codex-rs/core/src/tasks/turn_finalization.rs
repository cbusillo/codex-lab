use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Mutex;

use futures::FutureExt;
use futures::future::BoxFuture;
use futures::future::Shared;
use tracing::error;

type TurnFinalization = Shared<BoxFuture<'static, ()>>;

pub(crate) struct TurnFinalizationQueue {
    tail: Mutex<TurnFinalization>,
}

impl Default for TurnFinalizationQueue {
    fn default() -> Self {
        Self {
            tail: Mutex::new(futures::future::ready(()).boxed().shared()),
        }
    }
}

impl TurnFinalizationQueue {
    pub(crate) fn enqueue(&self, finalization: impl Future<Output = ()> + Send + 'static) {
        let next = {
            let mut tail = self
                .tail
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let previous = tail.clone();
            let next = async move {
                previous.await;
                if AssertUnwindSafe(finalization).catch_unwind().await.is_err() {
                    error!("turn finalization panicked");
                }
            }
            .boxed()
            .shared();
            *tail = next.clone();
            next
        };
        tokio::spawn(next);
    }

    pub(crate) fn current(&self) -> TurnFinalization {
        self.tail
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) async fn wait(&self) {
        self.current().await;
    }
}
