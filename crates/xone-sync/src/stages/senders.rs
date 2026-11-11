//! Senders stage: recover the sender of every transaction of the staged bodies, in batches of one
//! body batch, over `Limits::workers` threads. A transaction that does not decode or whose
//! signature does not recover stops the pipeline before execution, naming block and index.
//!
//! The executor recovers again: `BlockExecutor` takes the raw body. Handing the recovered senders
//! to it is a change of that trait, listed as not covered in `spec/sync/stages.md`.

use super::{span, Limits, SyncError};
use crate::{
    chain::{Chain, HeaderCheck},
    executor::BlockExecutor,
    store::{Stage, Store},
};
use alloy_primitives::Bytes;
use xone_executor::tx;

pub fn run<S: Store, E: BlockExecutor, H: HeaderCheck>(
    chain: &Chain<S, E, H>,
    lim: &Limits,
) -> Result<u64, SyncError> {
    let mut done = 0;
    loop {
        let (from, n, items) = {
            let g = chain.rd();
            let Some((from, n)) = span(
                g.checkpoint(Stage::Senders),
                g.head().0,
                g.checkpoint(Stage::Bodies),
                lim.body_blocks,
            ) else {
                return Ok(done);
            };
            let mut items: Vec<(u64, usize, Bytes)> = Vec::new();
            for k in from..from + n {
                let b = g.staged_body(k).ok_or(SyncError::Missing { number: k })?;
                items.extend(
                    b.transactions
                        .into_iter()
                        .enumerate()
                        .map(|(i, t)| (k, i, t)),
                );
            }
            (from, n, items)
        };
        let id = chain.spec.chain_id;
        let per = items.len().div_ceil(lim.workers.max(1)).max(1);
        let first_error = std::thread::scope(|s| {
            let jobs: Vec<_> = items
                .chunks(per)
                .map(|c| {
                    s.spawn(move || {
                        c.iter()
                            .find_map(|(k, i, t)| tx::decode(t, id).err().map(|e| (*k, *i, e)))
                    })
                })
                .collect();
            jobs.into_iter()
                .filter_map(|j| j.join().unwrap_or(None))
                .next() // chunks are in block order, so the first is the lowest
        });
        if let Some((number, index, error)) = first_error {
            return Err(SyncError::Sender {
                number,
                index,
                error,
            });
        }
        chain.wr().set_checkpoint(Stage::Senders, from + n - 1)?;
        done += n;
    }
}
