//! The contract between the agent loop and a language-model provider.
//!
//! Layer L2. `contract` is the frozen surface owned by the consumer; the
//! OpenAI-compatible SSE client is added in W2 by the provider unit, behind
//! that contract. Provider-specific options stay in the provider module.

#![forbid(unsafe_op_in_unsafe_fn)]

mod contract;
mod openai;
mod sse;

pub use contract::*;
pub use openai::*;
