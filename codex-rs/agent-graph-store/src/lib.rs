//! Storage-neutral parent/child topology for thread-spawned agents.

mod error;
mod local;
mod store;
mod types;

pub use codex_state::ExternalAgentRun;
pub use codex_state::ExternalAgentRunOutcome;
pub use codex_state::ExternalAgentRunStart;
pub use error::AgentGraphStoreError;
pub use error::AgentGraphStoreResult;
pub use local::LocalAgentGraphStore;
pub use store::AgentGraphStore;
pub use store::AgentGraphStoreFuture;
pub use types::ThreadSpawnEdgeStatus;
