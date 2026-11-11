//! XDPoS consensus rules of xOne: the V2 signer safety unit and the exact header
//! and body validation.
#![forbid(unsafe_code)]

pub mod chain;
pub mod penalty;
pub mod reward;
pub mod v2;
pub mod verify;
