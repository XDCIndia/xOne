//! Bodies stage: fetch the bodies of the staged headers in batches of at most Q9 (128 blocks and
//! 64 MiB), check each against its header, and store them with the stage checkpoint.

use super::{span, Limits, Source, SyncError};
use crate::{
    chain::{Chain, HeaderCheck},
    executor::BlockExecutor,
    store::{Stage, Store},
};
use xone_consensus::verify::BodyVerifier;
use xone_primitives::Header;

pub fn run<S: Store, E: BlockExecutor, H: HeaderCheck>(
    chain: &Chain<S, E, H>,
    src: &mut impl Source,
    lim: &Limits,
) -> Result<u64, SyncError> {
    let mut stored = 0;
    loop {
        let (from, hs) = {
            let g = chain.rd();
            let Some((from, n)) = span(
                g.checkpoint(Stage::Bodies),
                g.head().0,
                g.checkpoint(Stage::Headers),
                lim.body_blocks,
            ) else {
                return Ok(stored);
            };
            let hs = (from..from + n)
                .map(|k| g.staged_header(k).ok_or(SyncError::Missing { number: k }))
                .collect::<Result<Vec<Header>, _>>()?;
            (from, hs)
        };
        let bodies = src.bodies(&hs, lim.body_bytes)?;
        if bodies.is_empty() {
            return Err(SyncError::Stall {
                stage: Stage::Bodies,
                number: from,
            });
        }
        let verifier = BodyVerifier::new(&chain.spec);
        for ((n, h), b) in (from..).zip(&hs).zip(&bodies) {
            verifier
                .verify_body(h, b)
                .map_err(|error| SyncError::Body { number: n, error })?;
        }
        chain.wr().stage_bodies(from, &bodies)?;
        stored += u64::try_from(bodies.len()).unwrap_or(u64::MAX);
    }
}
