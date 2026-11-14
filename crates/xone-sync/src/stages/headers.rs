//! Headers stage: fetch headers from the source in a window of at most Q8 (2,048) headers ahead of
//! the head, check that they link, and store them with the stage checkpoint. The headers are never
//! held beyond one batch. The consensus check of each header runs once, in `Chain::insert`.

use super::{Limits, Source, SyncError};
use crate::{
    chain::{Chain, HeaderCheck},
    executor::BlockExecutor,
    store::{Stage, Store},
};

/// Returns the number of headers staged.
pub fn run<S: Store, E: BlockExecutor, H: HeaderCheck>(
    chain: &Chain<S, E, H>,
    src: &mut impl Source,
    lim: &Limits,
) -> Result<u64, SyncError> {
    let mut staged = 0;
    loop {
        let (ck, anchor, room) = {
            let g = chain.rd();
            let (head, head_hash) = g.head();
            let ck = g.checkpoint(Stage::Headers).max(head);
            let anchor = if ck == head {
                head_hash
            } else {
                g.staged_header(ck)
                    .ok_or(SyncError::Missing { number: ck })?
                    .hash()
            };
            let ahead = usize::try_from(ck - head).unwrap_or(usize::MAX);
            (ck, anchor, lim.headers.saturating_sub(ahead))
        };
        if room == 0 {
            return Ok(staged);
        }
        let mut hs = src.headers(ck + 1, room)?;
        hs.truncate(room);
        if hs.is_empty() {
            return Ok(staged);
        }
        let mut parent = anchor;
        for (n, h) in (ck + 1..).zip(&hs) {
            if h.number != n || h.parent_hash != parent {
                return Err(SyncError::Link { number: n });
            }
            chain.check_required(h)?;
            parent = h.hash();
        }
        chain.wr().stage_headers(&hs)?;
        staged += u64::try_from(hs.len()).unwrap_or(u64::MAX);
    }
}
