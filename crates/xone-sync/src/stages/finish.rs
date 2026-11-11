//! Finish stage: the staged rows up to the head are consumed, so they are deleted, and the
//! `Finish` checkpoint moves to the head, in one transaction. The head itself was published by the
//! commit of the last executed block.

use super::SyncError;
use crate::{
    chain::{Chain, HeaderCheck},
    executor::BlockExecutor,
    store::{Stage, Store},
};

/// True when the checkpoint moved.
pub fn run<S: Store, E: BlockExecutor, H: HeaderCheck>(
    chain: &Chain<S, E, H>,
) -> Result<bool, SyncError> {
    let head = chain.head().0;
    if chain.rd().checkpoint(Stage::Finish) >= head {
        return Ok(false);
    }
    chain.wr().finish(head)?;
    Ok(true)
}
