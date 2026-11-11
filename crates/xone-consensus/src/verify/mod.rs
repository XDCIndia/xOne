//! Exact header and body validation, as every accepting node runs it. Pure functions
//! with typed errors; no I/O, no clock, no configuration switch. The rules and their
//! geth-XDC lines are in `spec/consensus/`.

mod body;
mod error;
mod header;

pub use body::{tx_recipient, Body, BodyVerifier, Receipt};
pub use error::{BodyError, HeaderError};
pub use header::{Context, HeaderVerifier, V1Context, V2Context};
