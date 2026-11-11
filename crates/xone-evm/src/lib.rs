//! revm with the XDC execution rules.
//!
//! Every switch is read from [`xone_chainspec::ChainSpec`] in [`spec`]; nothing in this crate
//! compares a chain id or a block number to a literal. The couplings are expressed as:
//!
//! * [`spec`]: block number and chain spec to a revm `SpecId` and an [`spec::XdcRules`];
//! * [`gas`]: revm `GasParams` and the intrinsic gas that geth-XDC charges;
//! * [`precompiles`]: the precompile set per fork row;
//! * [`handler`]: the revm `Handler` override and the instruction overrides;
//! * [`journal`]: the journal wrapper that gives revm geth-XDC wide-balance semantics.
#![forbid(unsafe_code)]

pub mod gas;
pub mod handler;
pub mod journal;
pub mod precompiles;
pub mod spec;
