//! Restart reconciliation (ADR-005 section 7): a pure decision over what the
//! caller read from MDBX and from the segment tails. The caller performs the
//! returned actions; this function never touches a file.
use crate::manifest::{DecodeError, Manifest, Segment};

/// A tail with more extra frames than any natural unwind is kept, not deleted.
/// ponytail: fixed 128; make it a chain-spec value if a reorg bound is derived.
pub const QUARANTINE_OVER_FRAMES: u64 = 128;

pub enum ManifestState {
    Missing,
    Invalid(DecodeError),
    Ok(Box<Manifest>),
}

/// What the scanner saw in one segment file, relative to the committed manifest entry.
#[derive(Clone, Copy, Debug)]
pub struct SegmentTail {
    pub segment: Segment,
    pub present: bool,
    pub file_len: u64,
    /// File holds `len` bytes and keccak256 of the frame ending at `len` equals `tail_digest`.
    pub committed_tail_ok: bool,
    /// Complete, CRC-valid frames after `len`. Bytes beyond them are a torn tail.
    pub ahead_frames: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Action {
    pub segment: Segment,
    pub to_len: u64,
    pub dropped_frames: u64,
    /// Copy the cut bytes to `quarantine/` (synced) before truncating.
    pub quarantine: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refuse {
    /// No manifest but a generation, or MDBX has no manifest while generation > 0.
    ManifestMissing,
    /// Segment files exist but MDBX is empty: no authority to cut them.
    OrphanSegments,
    ManifestCorrupt,
    FormatVersion(u16),
    GenerationMismatch {
        mdbx: u64,
        manifest: u64,
    },
    ManifestInconsistent,
    /// MDBX is ahead: the file is gone, shorter than committed, or its committed tail does not verify.
    SegmentMissing(Segment),
    SegmentShort(Segment),
    SegmentCorrupt(Segment),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Recovery {
    /// New datadir.
    Fresh,
    /// Consistent. Only idempotent publication remains.
    Ready,
    /// Apply, then open.
    Repair(Vec<Action>),
    Refuse(Refuse),
}

pub fn reconcile(
    mdbx_generation: u64,
    manifest: &ManifestState,
    tails: &[SegmentTail],
) -> Recovery {
    use Recovery::Refuse as No;
    let m = match manifest {
        ManifestState::Missing if mdbx_generation > 0 => return No(Refuse::ManifestMissing),
        ManifestState::Missing if tails.iter().any(|t| t.present) => {
            return No(Refuse::OrphanSegments)
        }
        ManifestState::Missing => return Recovery::Fresh,
        ManifestState::Invalid(DecodeError::Version(v)) => return No(Refuse::FormatVersion(*v)),
        ManifestState::Invalid(_) => return No(Refuse::ManifestCorrupt),
        ManifestState::Ok(m) => m,
    };
    if m.generation != mdbx_generation {
        return No(Refuse::GenerationMismatch {
            mdbx: mdbx_generation,
            manifest: m.generation,
        });
    }
    if m.entries.iter().any(|e| e.last_block != m.head_number) {
        return No(Refuse::ManifestInconsistent);
    }
    let mut actions = Vec::new();
    for e in &m.entries {
        let Some(t) = tails.iter().find(|t| t.segment == e.segment && t.present) else {
            return No(Refuse::SegmentMissing(e.segment));
        };
        if t.file_len < e.len {
            return No(Refuse::SegmentShort(e.segment));
        }
        if !t.committed_tail_ok {
            return No(Refuse::SegmentCorrupt(e.segment));
        }
        if t.file_len > e.len {
            actions.push(Action {
                segment: e.segment,
                to_len: e.len,
                dropped_frames: t.ahead_frames,
                quarantine: t.ahead_frames > QUARANTINE_OVER_FRAMES,
            });
        }
    }
    if actions.is_empty() {
        Recovery::Ready
    } else {
        Recovery::Repair(actions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::sample;

    fn good(m: &Manifest) -> Vec<SegmentTail> {
        m.entries
            .iter()
            .map(|e| SegmentTail {
                segment: e.segment,
                present: true,
                file_len: e.len,
                committed_tail_ok: true,
                ahead_frames: 0,
            })
            .collect()
    }
    fn run(m: &Manifest, tails: &[SegmentTail]) -> Recovery {
        reconcile(m.generation, &ManifestState::Ok(Box::new(m.clone())), tails)
    }

    // D1
    #[test]
    fn fresh_datadir() {
        let none: Vec<_> = good(&sample(1))
            .into_iter()
            .map(|t| SegmentTail {
                present: false,
                ..t
            })
            .collect();
        assert_eq!(
            reconcile(0, &ManifestState::Missing, &none),
            Recovery::Fresh
        );
        assert_eq!(reconcile(0, &ManifestState::Missing, &[]), Recovery::Fresh);
    }
    // D2
    #[test]
    fn segments_without_mdbx_are_not_cut() {
        assert_eq!(
            reconcile(0, &ManifestState::Missing, &good(&sample(1))),
            Recovery::Refuse(Refuse::OrphanSegments)
        );
    }
    // D3
    #[test]
    fn manifest_missing() {
        assert_eq!(
            reconcile(4, &ManifestState::Missing, &good(&sample(4))),
            Recovery::Refuse(Refuse::ManifestMissing)
        );
    }
    // D4, D5
    #[test]
    fn manifest_invalid_or_wrong_version() {
        let t = good(&sample(4));
        for e in [
            DecodeError::BadMagic,
            DecodeError::BadLength,
            DecodeError::BadChecksum,
            DecodeError::BadLayout,
        ] {
            assert_eq!(
                reconcile(4, &ManifestState::Invalid(e), &t),
                Recovery::Refuse(Refuse::ManifestCorrupt)
            );
        }
        assert_eq!(
            reconcile(4, &ManifestState::Invalid(DecodeError::Version(9)), &t),
            Recovery::Refuse(Refuse::FormatVersion(9))
        );
    }
    // D6
    #[test]
    fn generation_mismatch() {
        let m = sample(4);
        assert_eq!(
            reconcile(5, &ManifestState::Ok(Box::new(m.clone())), &good(&m)),
            Recovery::Refuse(Refuse::GenerationMismatch {
                mdbx: 5,
                manifest: 4
            })
        );
    }
    // D7
    #[test]
    fn entries_disagree_with_head() {
        let mut m = sample(4);
        m.entries[1].last_block = 6;
        assert_eq!(
            run(&m, &good(&m)),
            Recovery::Refuse(Refuse::ManifestInconsistent)
        );
    }
    // D8 (MDBX ahead of segments)
    #[test]
    fn segment_file_missing() {
        let m = sample(4);
        let mut t = good(&m);
        t[2].present = false;
        assert_eq!(
            run(&m, &t),
            Recovery::Refuse(Refuse::SegmentMissing(Segment::Receipts))
        );
        t.pop();
        assert_eq!(
            run(&m, &t),
            Recovery::Refuse(Refuse::SegmentMissing(Segment::Receipts))
        );
    }
    // D9 (MDBX ahead of segments)
    #[test]
    fn segment_shorter_than_manifest() {
        let m = sample(4);
        let mut t = good(&m);
        t[0].file_len -= 1;
        assert_eq!(
            run(&m, &t),
            Recovery::Refuse(Refuse::SegmentShort(Segment::Headers))
        );
    }
    // D10 (torn or reordered write inside the committed region)
    #[test]
    fn committed_tail_digest_mismatch() {
        let m = sample(4);
        let mut t = good(&m);
        t[1].committed_tail_ok = false;
        assert_eq!(
            run(&m, &t),
            Recovery::Refuse(Refuse::SegmentCorrupt(Segment::Bodies))
        );
        t[1].file_len += 50; // extra bytes do not excuse a bad committed tail
        assert_eq!(
            run(&m, &t),
            Recovery::Refuse(Refuse::SegmentCorrupt(Segment::Bodies))
        );
    }
    // D11
    #[test]
    fn consistent() {
        let m = sample(4);
        assert_eq!(run(&m, &good(&m)), Recovery::Ready);
    }
    // D12 (torn tail, no complete extra frame)
    #[test]
    fn torn_tail_is_truncated() {
        let m = sample(4);
        let mut t = good(&m);
        t[2].file_len += 13;
        assert_eq!(
            run(&m, &t),
            Recovery::Repair(vec![Action {
                segment: Segment::Receipts,
                to_len: 3000,
                dropped_frames: 0,
                quarantine: false
            }])
        );
    }
    // D13 (segments ahead of MDBX: staged but uncommitted block, or unwind remnant)
    #[test]
    fn one_staged_block_is_truncated() {
        let m = sample(4);
        let mut t = good(&m);
        for x in &mut t {
            x.file_len += 90;
            x.ahead_frames = 1;
        }
        match run(&m, &t) {
            Recovery::Repair(a) => {
                assert_eq!(a.len(), 3);
                assert!(a.iter().all(|x| x.dropped_frames == 1 && !x.quarantine));
                assert_eq!(a[0].to_len, 1000);
            }
            other => panic!("{other:?}"),
        }
    }
    // D14 (segments far ahead: stale MDBX restore or rollback)
    #[test]
    fn far_ahead_is_quarantined() {
        let m = sample(4);
        let mut t = good(&m);
        t[0].file_len += 9999;
        t[0].ahead_frames = QUARANTINE_OVER_FRAMES + 1;
        t[1].file_len += 9;
        t[1].ahead_frames = QUARANTINE_OVER_FRAMES;
        assert_eq!(
            run(&m, &t),
            Recovery::Repair(vec![
                Action {
                    segment: Segment::Headers,
                    to_len: 1000,
                    dropped_frames: QUARANTINE_OVER_FRAMES + 1,
                    quarantine: true
                },
                Action {
                    segment: Segment::Bodies,
                    to_len: 2000,
                    dropped_frames: QUARANTINE_OVER_FRAMES,
                    quarantine: false
                },
            ])
        );
    }
    // Refusal dominates repair.
    #[test]
    fn refuse_beats_repair() {
        let m = sample(4);
        let mut t = good(&m);
        t[0].file_len += 5;
        t[2].file_len -= 1;
        assert_eq!(
            run(&m, &t),
            Recovery::Refuse(Refuse::SegmentShort(Segment::Receipts))
        );
    }
}
