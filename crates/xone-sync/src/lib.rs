//! Chain manager of xOne (XB-13): block import, side branches, XDC fork choice, reorg, set-head and
//! restart recovery. Specification: `spec/chain/`.
//!
//! Everything here is deterministic and runs over traits: a [`store::Store`] (headers, bodies,
//! receipts, canonical index, side index and the state of `xone-state`, published by ADR-005
//! steps), a [`executor::BlockExecutor`] and a [`chain::HeaderCheck`]. There is no I/O, no clock and
//! no global state; `now` is an argument. [`testkit`] holds the in-memory store with the ADR-005
//! kill points and the test executor.
#![forbid(unsafe_code)]

pub mod chain;
pub mod executor;
pub mod store;
pub mod testkit;
