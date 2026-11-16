//! A minimal shared cancellation token.
//!
//! The frozen `forge_llm` contract is synchronous: one turn owns one thread and
//! `Stream::close` is the cancellation door. This token is the consumer-owned
//! half of that design — the HTTP layer (or a test) sets it, the agent checks
//! it between deltas and closes the stream exactly once.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A cheap, cloneable "stop now" flag.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}
