//! Test doubles: [`MemStore`], an in-memory store that follows the ADR-005 protocol step by step
//! and can crash or fail at every kill point, and [`VectorExecutor`], which replays recorded
//! executions. Not for a release build of the node.

mod exec;
mod mem;

pub use exec::VectorExecutor;
pub use mem::{Cut, Disk, Fault, MemStore};
