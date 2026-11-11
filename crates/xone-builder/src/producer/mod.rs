//! Block production (XB-23): who builds, what goes in, how it is sealed and when. Pure functions over explicit
//! inputs; the driver (`xone-node::services::producer`) reads the chain and the clock, executes the
//! transactions and sends the result. The key is reachable only through [`xone_consensus::unit::Sealer`],
//! which the safety unit implements: this crate does not depend on `xone-signer`.
//!
//! * [`schedule`]: V1 in-turn and out-of-turn, the V2 leader and the mined-round guard;
//! * [`build`]: transaction selection and the header from the executed roots;
//! * [`seal_v1`], [`seal_v2`]: `Prepare` and `Seal`;
//! * [`signtx`]: the 0x89 sign transaction;
//! * [`gaslimit`], [`timing`]: the gas limit and the base fee; the clock rules.
//!
//! Rules and geth lines: `spec/producer/`.

pub mod build;
pub mod gaslimit;
pub mod schedule;
pub mod seal_v1;
pub mod seal_v2;
pub mod signtx;
pub mod timing;
