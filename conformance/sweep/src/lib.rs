//! Replay of one archived transaction through xone-evm and comparison with the archive.
//!
//! A [`Case`] holds what the Apothem archive says about one transaction: the block header fields,
//! the transaction, its receipt, the prestate (`prestateTracer`) and the state diff
//! (`prestateTracer`, `diffMode`). Nothing in a case is typed by hand; [`replay`] executes the
//! transaction on the prestate and [`compare`] names the first field that differs.
//!
//! What the archive cannot show: `prestateTracer` drops accounts that are empty before the
//! transaction (geth `eth/tracers/native/prestate.go:OnTxEnd`), so an empty account and a missing
//! one are the same here, and the EIP-161 deletion of a touched empty account is invisible.
//! `conformance/vectors/evm/static_call_touch.json` covers that rule from the oracle.

#![forbid(unsafe_code)]

pub mod case;
pub mod db;
pub mod replay;

pub use case::{Case, ERAS};
pub use replay::{compare, replay, replay_with, Outcome, ReplayError};
pub mod fetch;
pub mod mutants;
