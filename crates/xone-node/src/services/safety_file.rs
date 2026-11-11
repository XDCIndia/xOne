//! The safety store of ADR-010 section 4 on a file: `<dir>/safety.rec` of two slots of 1024 bytes, outside the
//! chain files, held by an exclusive file lock. A write goes to the slot that does not hold the newest valid
//! frame and is synced before it returns, so a torn write damages only the slot being written and the other one
//! is the last acknowledged state. A frame is `store_seq u64 | intent_len u16 | intent | record_len u16 | record |
//! keccak256 of the preceding bytes`, all little endian. An existing file with no valid slot is corrupt, never empty.

use std::{
    fs::{File, OpenOptions},
    io::ErrorKind,
    os::unix::fs::FileExt,
    path::Path,
};

use alloy_primitives::keccak256;
use xone_db::safety::{Ack, Loaded, SafetyStore, StoreError};

const SLOT: usize = 1024;

pub struct FileSafety {
    file: File,
    /// The newest valid frame: its sequence and slot.
    newest: Option<(u64, usize)>,
}

fn frame(seq: u64, intent: &[u8], record: &[u8]) -> Result<Vec<u8>, StoreError> {
    let (il, rl) = (
        u16::try_from(intent.len()).map_err(|_| StoreError::Io)?,
        u16::try_from(record.len()).map_err(|_| StoreError::Io)?,
    );
    let mut b = Vec::with_capacity(SLOT);
    b.extend_from_slice(&seq.to_le_bytes());
    b.extend_from_slice(&il.to_le_bytes());
    b.extend_from_slice(intent);
    b.extend_from_slice(&rl.to_le_bytes());
    b.extend_from_slice(record);
    let sum = keccak256(&b);
    b.extend_from_slice(sum.as_slice());
    if b.len() > SLOT {
        return Err(StoreError::Io);
    }
    b.resize(SLOT, 0);
    Ok(b)
}

fn parse(slot: &[u8]) -> Option<(u64, Loaded)> {
    let seq = u64::from_le_bytes(slot.get(..8)?.try_into().ok()?);
    let il = usize::from(u16::from_le_bytes(slot.get(8..10)?.try_into().ok()?));
    let intent = slot.get(10..10 + il)?;
    let at = 10 + il;
    let rl = usize::from(u16::from_le_bytes(slot.get(at..at + 2)?.try_into().ok()?));
    let record = slot.get(at + 2..at + 2 + rl)?;
    let end = at + 2 + rl;
    (keccak256(slot.get(..end)?).as_slice() == slot.get(end..end + 32)?).then(|| {
        (
            seq,
            Loaded {
                intent: intent.to_vec(),
                record: record.to_vec(),
            },
        )
    })
}

impl FileSafety {
    /// Opens (and on the first call creates) `<dir>/safety.rec` and takes its exclusive lock: a second process
    /// on the same directory is refused.
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        std::fs::create_dir_all(dir).map_err(|_| StoreError::Io)?;
        let path = dir.join("safety.rec");
        let fresh = !path.exists();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|_| StoreError::Io)?;
        file.try_lock().map_err(|_| StoreError::Io)?;
        if fresh {
            // the file and its directory entry are durable before the first record relies on them
            file.set_len(2 * SLOT as u64).map_err(|_| StoreError::Io)?;
            file.sync_all().map_err(|_| StoreError::Io)?;
            File::open(dir)
                .and_then(|d| d.sync_all())
                .map_err(|_| StoreError::Io)?;
        }
        Ok(Self { file, newest: None })
    }

    fn slots(&self) -> Result<[Option<(u64, Loaded)>; 2], StoreError> {
        let mut buf = vec![0u8; 2 * SLOT];
        match self.file.read_exact_at(&mut buf, 0) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Err(StoreError::Corrupt),
            Err(_) => return Err(StoreError::Io),
        }
        Ok([parse(&buf[..SLOT]), parse(&buf[SLOT..])])
    }
}

impl SafetyStore for FileSafety {
    fn load(&mut self) -> Result<Option<Loaded>, StoreError> {
        let slots = self.slots()?;
        // what is returned is durable: a frame that was only in the page cache is synced first
        self.file.sync_all().map_err(|_| StoreError::Io)?;
        let best = slots
            .into_iter()
            .enumerate()
            .filter_map(|(i, s)| s.map(|(seq, l)| (seq, i, l)))
            .max_by_key(|(seq, _, _)| *seq);
        match best {
            Some((seq, i, l)) => {
                self.newest = Some((seq, i));
                Ok(Some(l))
            }
            None => {
                let mut all = vec![0u8; 2 * SLOT];
                self.file
                    .read_exact_at(&mut all, 0)
                    .map_err(|_| StoreError::Io)?;
                if all.iter().all(|b| *b == 0) {
                    Ok(None)
                } else {
                    Err(StoreError::Corrupt)
                }
            }
        }
    }

    fn persist_intent(&mut self, intent: &[u8], record: &[u8]) -> Result<Ack, StoreError> {
        let (seq, slot) = match self.newest {
            Some((s, i)) => (s + 1, 1 - i),
            None => (1, 0),
        };
        let f = frame(seq, intent, record)?;
        self.file
            .write_all_at(&f, (slot * SLOT) as u64)
            .map_err(|_| StoreError::Io)?;
        self.file.sync_all().map_err(|_| StoreError::Io)?;
        self.newest = Some((seq, slot));
        Ok(Ack { seq })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(n: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("xone-safety-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn writes_alternate_slots_survive_a_torn_write_and_refuse_a_second_holder() {
        let d = dir("a");
        let mut s = FileSafety::open(&d).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.load(), Ok(None));
        s.persist_intent(b"i1", b"r1").unwrap_or_else(|e| panic!("{e}"));
        s.persist_intent(b"i2", b"r2").unwrap_or_else(|e| panic!("{e}"));
        assert!(FileSafety::open(&d).is_err(), "one process holds the directory");
        drop(s);
        // tear the newest slot (the second write went to slot 1): the other one is the last good state
        let f = OpenOptions::new().write(true).open(d.join("safety.rec")).unwrap_or_else(|e| panic!("{e}"));
        f.write_all_at(&[0xff; 40], SLOT as u64 + 8).unwrap_or_else(|e| panic!("{e}"));
        drop(f);
        let mut s = FileSafety::open(&d).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.load().unwrap_or_default().map(|l| l.record), Some(b"r1".to_vec()));
        // the next write goes to the damaged slot and wins
        s.persist_intent(b"i3", b"r3").unwrap_or_else(|e| panic!("{e}"));
        drop(s);
        let mut s = FileSafety::open(&d).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.load().unwrap_or_default().map(|l| l.record), Some(b"r3".to_vec()));
        drop(s);
        // no valid slot in an existing file is corrupt, not empty
        std::fs::write(d.join("safety.rec"), vec![7u8; 2 * SLOT]).unwrap_or_else(|e| panic!("{e}"));
        let mut s = FileSafety::open(&d).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.load(), Err(StoreError::Corrupt));
    }
}
