//! Sync: the chain manager (XB-13) and bootstrap (XA-09).
//!
//! [`chain`] is block import, side branches, XDC fork choice, reorg, set-head and restart
//! recovery (specification: `spec/chain/`). Everything there is deterministic and runs over traits:
//! a [`store::Store`] (headers, bodies, receipts, canonical index, side index and the state of
//! `xone-state`, published by ADR-005 steps), a [`executor::BlockExecutor`] and a
//! [`chain::HeaderCheck`]. There is no I/O, no clock and no global state; `now` is an argument.
//! [`testkit`] holds the in-memory store with the ADR-005 kill points and the test executor.
//!
//! [`bootstrap`] is what a node must hold, and be able to prove, before it follows the chain from
//! a snapshot: the anchor manifest, the derived retention window and the pivot plan
//! (specification: `spec/bootstrap-anchor.md`, `spec/retention.md`).
#![forbid(unsafe_code)]

pub mod bootstrap;
pub mod chain;
pub mod executor;
pub mod store;
pub mod testkit;
