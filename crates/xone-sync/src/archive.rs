//! Block files on disk, plain or gzip (`.gz`), the two shapes geth-XDC imports and exports
//! (`cmd/utils/cmd.go:213-222` and `:479-499`: gzip when the name ends in `.gz`).
//!
//! [`FileSource`](crate::import::FileSource) reads bodies again by offset, so it needs `Seek`. A
//! gzip archive is streamed once through the decoder into a spool file in the temporary directory
//! (`TMPDIR`), never into memory; the decoder checks every member's CRC and length and a truncated
//! stream is an error, so a damaged archive fails before one block is imported. The spool is
//! removed when the [`BlockFile`] is dropped, and on a failed decode.

use crate::import::FileError;
use flate2::{read::MultiGzDecoder, write::GzEncoder, Compression};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// The two bytes every gzip member starts with (RFC 1952).
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// A name ending in `.gz`: geth's rule for both directions.
pub fn is_gz_name(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gz"))
}

/// A seekable block file: the file itself, or the decompressed spool of a gzip archive.
pub struct BlockFile {
    r: BufReader<File>,
    spool: Option<PathBuf>,
}

impl BlockFile {
    /// Whether the file was a gzip archive (read through a spool).
    pub fn was_gzip(&self) -> bool {
        self.spool.is_some()
    }
}

impl Read for BlockFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.r.read(buf)
    }
}

impl Seek for BlockFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.r.seek(pos)
    }
}

impl Drop for BlockFile {
    fn drop(&mut self) {
        if let Some(p) = &self.spool {
            let _ = fs::remove_file(p);
        }
    }
}

/// Removes the spool unless it is handed over.
struct SpoolGuard(Option<PathBuf>);

impl Drop for SpoolGuard {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            let _ = fs::remove_file(p);
        }
    }
}

fn spool_path(dir: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    dir.join(format!(
        "xone-import-{}-{}.rlp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Opens the block file `path` for import. A gzip archive (by its magic bytes, whatever its name)
/// is decompressed to a spool first; a `.gz` name without the magic is refused.
pub fn open_block_file(path: &Path) -> Result<BlockFile, FileError> {
    open_spooled_in(path, &std::env::temp_dir())
}

/// [`open_block_file`] with the spool in `dir`.
pub fn open_spooled_in(path: &Path, dir: &Path) -> Result<BlockFile, FileError> {
    let mut f = File::open(path)?;
    let mut magic = [0u8; 2];
    let got = read_up_to(&mut f, &mut magic)?;
    f.seek(SeekFrom::Start(0))?;
    if got < 2 || magic != GZIP_MAGIC {
        if is_gz_name(path) {
            return Err(FileError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: named .gz but not a gzip archive", path.display()),
            )));
        }
        return Ok(BlockFile {
            r: BufReader::new(f),
            spool: None,
        });
    }
    let sp = spool_path(dir);
    let mut guard = SpoolGuard(Some(sp.clone()));
    let out = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&sp)?;
    let mut w = BufWriter::new(out);
    io::copy(&mut MultiGzDecoder::new(BufReader::new(f)), &mut w).map_err(|e| {
        FileError::Io(io::Error::new(
            e.kind(),
            format!("{}: gzip archive is damaged: {e}", path.display()),
        ))
    })?;
    let mut out = w.into_inner().map_err(io::IntoInnerError::into_error)?;
    out.seek(SeekFrom::Start(0))?;
    let spool = guard.0.take();
    Ok(BlockFile {
        r: BufReader::new(out),
        spool,
    })
}

fn read_up_to(f: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// A block file being exported: plain, or gzip when the name ends in `.gz`.
pub enum BlockWriter {
    Plain(BufWriter<File>),
    Gzip(GzEncoder<BufWriter<File>>),
}

/// Creates the export file `path`, gzip-compressed when its name ends in `.gz` (as geth does).
pub fn create_block_file(path: &Path) -> io::Result<BlockWriter> {
    let w = BufWriter::new(File::create(path)?);
    Ok(if is_gz_name(path) {
        BlockWriter::Gzip(GzEncoder::new(w, Compression::default()))
    } else {
        BlockWriter::Plain(w)
    })
}

impl BlockWriter {
    /// Writes the gzip trailer, flushes and syncs the file: an export is complete only when this
    /// returns `Ok`.
    pub fn finish(self) -> io::Result<()> {
        let w = match self {
            Self::Plain(w) => w,
            Self::Gzip(g) => g.finish()?,
        };
        w.into_inner()
            .map_err(io::IntoInnerError::into_error)?
            .sync_all()
    }
}

impl Write for BlockWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(w) => w.write(buf),
            Self::Gzip(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(w) => w.flush(),
            Self::Gzip(w) => w.flush(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("xone-archive-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn read_all(mut f: BlockFile) -> Vec<u8> {
        let mut v = vec![];
        f.read_to_end(&mut v).unwrap();
        v
    }

    #[test]
    fn gzip_round_trips_through_a_spool_that_is_removed() {
        let d = dir("roundtrip");
        let data: Vec<u8> = (0..200_000u32).flat_map(|i| i.to_le_bytes()).collect();
        let gz = d.join("blocks.rlp.gz");
        let mut w = create_block_file(&gz).unwrap();
        w.write_all(&data).unwrap();
        w.finish().unwrap();
        assert_eq!(fs::read(&gz).unwrap()[..2], GZIP_MAGIC);
        let mut f = open_block_file(&gz).unwrap();
        assert!(f.was_gzip());
        let spool = f.spool.clone().unwrap();
        assert!(spool.exists());
        // the spool seeks: bodies are read again by offset
        f.seek(SeekFrom::Start(4)).unwrap();
        let mut four = [0u8; 4];
        f.read_exact(&mut four).unwrap();
        assert_eq!(four, 1u32.to_le_bytes());
        f.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(read_all(f), data);
        assert!(!spool.exists());
        // a plain file is read in place
        let plain = d.join("blocks.rlp");
        let mut w = create_block_file(&plain).unwrap();
        w.write_all(&data).unwrap();
        w.finish().unwrap();
        let f = open_block_file(&plain).unwrap();
        assert!(!f.was_gzip());
        assert_eq!(read_all(f), data);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn truncated_corrupt_and_misnamed_archives_fail() {
        let d = dir("damaged");
        let data = vec![0xc0u8; 100_000];
        let gz = d.join("ok.rlp.gz");
        let mut w = create_block_file(&gz).unwrap();
        w.write_all(&data).unwrap();
        w.finish().unwrap();
        let good = fs::read(&gz).unwrap();
        let sp = d.join("spool");
        fs::create_dir_all(&sp).unwrap();
        let spools = || fs::read_dir(&sp).unwrap().count();
        assert!(open_spooled_in(&gz, &sp).unwrap().was_gzip());
        // truncated: the trailer is missing
        let cut = d.join("cut.rlp.gz");
        fs::write(&cut, &good[..good.len() - 6]).unwrap();
        assert!(open_spooled_in(&cut, &sp).is_err());
        // corrupt CRC: the trailer's first byte flipped
        let mut bad = good.clone();
        let at = bad.len() - 8;
        bad[at] ^= 0xff;
        let crc = d.join("crc.rlp.gz");
        fs::write(&crc, &bad).unwrap();
        let e = open_spooled_in(&crc, &sp).err().unwrap().to_string();
        assert!(e.contains("damaged"), "{e}");
        // named .gz, plain inside
        let fake = d.join("fake.rlp.gz");
        fs::write(&fake, &data).unwrap();
        assert!(open_spooled_in(&fake, &sp).is_err());
        // no spool is left behind by a failed decode
        assert_eq!(spools(), 0);
        let _ = fs::remove_dir_all(&d);
    }
}
