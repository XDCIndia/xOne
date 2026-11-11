//! Execution stage: `Chain::insert` of each staged block, at most Q10 (64) blocks per batch. The
//! stage checkpoint is the head, written by the commit of each block (S5), so it cannot disagree
//! with it. A block that is not accepted stops the pipeline with the typed insert error.

use super::{span, Limits, SyncError};
use crate::{
    chain::{Chain, HeaderCheck, Outcome},
    executor::BlockExecutor,
    store::{Block, Stage, Store},
};

/// `now` is the clock in seconds for the header check. Returns the blocks executed.
pub fn run<S: Store, E: BlockExecutor, H: HeaderCheck>(
    chain: &Chain<S, E, H>,
    lim: &Limits,
    now: u64,
) -> Result<u64, SyncError> {
    let mut done = 0;
    loop {
        let Some((from, n)) = ({
            let g = chain.rd();
            span(0, g.head().0, g.checkpoint(Stage::Senders), lim.exec_blocks)
        }) else {
            return Ok(done);
        };
        for number in from..from + n {
            let (header, body) = {
                let g = chain.rd();
                (
                    g.staged_header(number)
                        .ok_or(SyncError::Missing { number })?,
                    g.staged_body(number).ok_or(SyncError::Missing { number })?,
                )
            };
            match chain.insert(Block::new(header, body), now) {
                Ok(Outcome::Extended) => {}
                Ok(outcome) => return Err(SyncError::Unexpected { number, outcome }),
                Err(error) => return Err(SyncError::Insert { number, error }),
            }
            done += 1;
        }
    }
}
