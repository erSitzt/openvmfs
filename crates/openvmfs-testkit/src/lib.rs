//! Development helpers for OpenVmfs tests: the fixture cache, and block sources that trace or corrupt reads.
//!
//! Nothing here is part of the library; this crate is never published.

// Test support code: panics and unchecked arithmetic are acceptable here.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use openvmfs::{BlockSource, FileSource, Result};
use sha2::{Digest, Sha256};

const CHUNK: usize = 1 << 20;

/// Root of the repository.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

/// Directory where unpacked fixture images are cached. Override with `OPENVMFS_FIXTURE_CACHE`.
pub fn cache_dir() -> PathBuf {
    match std::env::var_os("OPENVMFS_FIXTURE_CACHE") {
        Some(dir) => PathBuf::from(dir),
        None => repo_root().join("target/fixture-cache"),
    }
}

/// Returns the unpacked image of one of the third-party sample images (`fixtures/fetch-external.sh`), for example
/// `"vmfs/vmfs6"`. Returns `None` when the sample has not been downloaded, so that tests can skip.
pub fn external_image(name: &str) -> Option<PathBuf> {
    let packed = repo_root()
        .join("fixtures/external/dissect.vmfs")
        .join(format!("{name}.bin.gz"));
    if !packed.is_file() {
        eprintln!(
            "skipping: {} is missing (run fixtures/fetch-external.sh)",
            packed.display()
        );
        return None;
    }
    let file_name = Path::new(name)
        .file_name()
        .expect("fixture name")
        .to_string_lossy()
        .into_owned();
    let unpacked = cache_dir().join("dissect").join(format!("{file_name}.bin"));
    unpack_gz_sparse(&packed, &unpacked);
    Some(unpacked)
}

/// Returns the unpacked image of one of our own fixtures in `fixtures/vmfs6`, for example `"fixture-zero-v1"`.
pub fn own_image(name: &str) -> PathBuf {
    let packed = repo_root().join("fixtures/vmfs6").join(format!("{name}.img.zst"));
    assert!(packed.is_file(), "fixture {} is missing", packed.display());
    let unpacked = cache_dir().join("ours").join(format!("{name}.fixture.img"));
    unpack_sparse(&packed, &unpacked, |file| {
        Box::new(ruzstd::decoding::StreamingDecoder::new(file).expect("zstd header"))
    });
    unpacked
}

/// A device image of which only some ranges were captured (`fixtures/recipes/capture_partial.py`).
///
/// Reading outside the captured ranges is an error, not zeros: a test must never pass because the reader looked at a
/// place that was simply not captured.
///
/// The captured ranges are kept back to back in one file, so the device may be larger than any file the local
/// filesystem can hold (a 17 TiB device does not fit into a sparse file on ext4).
#[derive(Debug)]
pub struct PartialSource {
    /// The captured ranges, back to back.
    parts: FileSource,
    /// Size of the device.
    size: u64,
    /// Captured ranges as `(offset, length)`, sorted, with neighbours merged.
    ranges: Vec<(u64, u64)>,
    /// Where each range starts in `parts`.
    starts: Vec<u64>,
}

impl PartialSource {
    /// The captured ranges as `(offset, length)`.
    pub fn ranges(&self) -> &[(u64, u64)] {
        &self.ranges
    }

    /// Whether `[offset, offset + len)` was captured.
    pub fn covers(&self, offset: u64, len: u64) -> bool {
        self.locate(offset, len).is_some()
    }

    /// Offset in the parts file of a range that was captured in full.
    fn locate(&self, offset: u64, len: u64) -> Option<u64> {
        let index = self
            .ranges
            .partition_point(|&(start, _)| start <= offset)
            .checked_sub(1)?;
        let (start, length) = self.ranges[index];
        (offset.checked_add(len)? <= start + length).then(|| self.starts[index] + (offset - start))
    }
}

impl BlockSource for PartialSource {
    fn len(&self) -> u64 {
        self.size
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        match self.locate(offset, buf.len() as u64) {
            Some(position) => self.parts.read_exact_at(position, buf),
            None => Err(openvmfs::Error::new(
                openvmfs::ErrorKind::Io,
                format!(
                    "read of {} bytes at {offset:#x} is outside the captured parts of this fixture",
                    buf.len()
                ),
            )),
        }
    }
}

/// Opens one of our own partially captured fixtures in `fixtures/vmfs6`, for example `"fixture-lfb-v1"`.
pub fn own_partial_image(name: &str) -> PartialSource {
    let dir = repo_root().join("fixtures/vmfs6");
    let packed = dir.join(format!("{name}.parts.zst"));
    let listing = std::fs::read_to_string(dir.join(format!("{name}.ranges"))).expect("ranges file");
    let mut lines = listing.lines();
    let size: u64 = lines
        .next()
        .and_then(|line| line.strip_prefix("size "))
        .and_then(|text| text.parse().ok())
        .expect("the ranges file starts with `size BYTES`");
    // The packed file holds the ranges in the listed order. Neighbours are merged, so that a read may cross them.
    let mut ranges: Vec<(u64, u64)> = Vec::new();
    let mut starts: Vec<u64> = Vec::new();
    let mut total = 0u64;
    for line in lines {
        let (offset, length) = line.split_once(' ').expect("OFFSET LENGTH");
        let (offset, length): (u64, u64) = (offset.parse().expect("offset"), length.parse().expect("length"));
        match ranges.last_mut() {
            Some(last) if last.0 + last.1 == offset => last.1 += length,
            Some(last) => {
                assert!(last.0 + last.1 < offset, "the ranges of {name} are not sorted");
                ranges.push((offset, length));
                starts.push(total);
            }
            None => {
                ranges.push((offset, length));
                starts.push(total);
            }
        }
        total += length;
    }

    let unpacked = cache_dir().join("ours").join(format!("{name}.fixture.parts"));
    unpack_sparse(&packed, &unpacked, |file| {
        Box::new(ruzstd::decoding::StreamingDecoder::new(file).expect("zstd header"))
    });
    let parts = FileSource::with_len(File::open(&unpacked).expect("open cache file"), total, name);
    assert_eq!(
        unpacked.metadata().expect("cache file").len(),
        total,
        "{name}: the packed parts do not match the ranges file"
    );
    PartialSource {
        parts,
        size,
        ranges,
        starts,
    }
}

/// Unpacks a gzip-compressed image into a sparse file, unless that was already done for this source file.
///
/// Chunks that are all zeros are skipped, so a 2 GiB image with 100 MiB of data uses 100 MiB on disk.
pub fn unpack_gz_sparse(packed: &Path, unpacked: &Path) {
    unpack_sparse(packed, unpacked, |file| {
        Box::new(flate2::read::MultiGzDecoder::new(file))
    });
}

/// Unpacks a compressed image into a sparse file through the decoder that `open` creates.
fn unpack_sparse(packed: &Path, unpacked: &Path, open: impl FnOnce(File) -> Box<dyn Read>) {
    // Unpacking is not atomic across test binaries; serialise it within the process and use a rename.
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let source = packed.metadata().expect("packed fixture metadata");
    let stamp = format!("{} {:?}", source.len(), source.modified().ok());
    let stamp_path = unpacked.with_extension("stamp");
    if unpacked.is_file() && std::fs::read_to_string(&stamp_path).is_ok_and(|existing| existing == stamp) {
        return;
    }
    std::fs::create_dir_all(unpacked.parent().expect("cache directory")).expect("create cache directory");
    let partial = unpacked.with_extension(format!("partial-{}", std::process::id()));
    let mut out = File::create(&partial).expect("create cache file");
    mark_sparse(&partial);
    let mut input = open(File::open(packed).expect("open packed fixture"));
    let mut buf = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        let mut filled = 0;
        while filled < buf.len() {
            let n = input.read(&mut buf[filled..]).expect("decompress fixture");
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled == 0 {
            break;
        }
        if buf[..filled].iter().any(|&b| b != 0) {
            out.seek(SeekFrom::Start(total)).expect("seek");
            out.write_all(&buf[..filled]).expect("write");
        }
        total += filled as u64;
    }
    out.set_len(total).expect("set length");
    drop(out);
    std::fs::rename(&partial, unpacked).expect("move cache file into place");
    std::fs::write(&stamp_path, stamp).expect("write stamp");
}

/// On NTFS a file is only sparse when it is marked so; elsewhere seeking over a range is enough.
fn mark_sparse(path: &Path) {
    if cfg!(windows) {
        let _ = std::process::Command::new("fsutil")
            .args(["sparse", "setflag"])
            .arg(path)
            .output();
    }
}

/// Lower-case hexadecimal SHA-256.
pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// SHA-256 of a whole block source, read in chunks.
pub fn sha256_of(source: &dyn BlockSource) -> String {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut offset = 0u64;
    while offset < source.len() {
        let len = (CHUNK as u64).min(source.len() - offset) as usize;
        source.read_exact_at(offset, &mut buf[..len]).expect("read");
        hasher.update(&buf[..len]);
        offset += len as u64;
    }
    hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Size of a sector of reference data.
pub const SECTOR: usize = 512;
/// First bytes of a reference data sector (see `fixtures/recipes/pattern.py`).
pub const PATTERN_MAGIC: &[u8; 8] = b"OVMFSPAT";
/// First bytes of a junk sector: what the datastore was filled with before the test files were created.
pub const JUNK_MAGIC: &[u8; 8] = b"OVMFSJNK";

/// The reference data sector `sector` of disk `disk_id`, as `fixtures/recipes/pattern.py` writes it.
pub fn pattern_sector(disk_id: u32, generation: u32, sector: u64) -> [u8; SECTOR] {
    let mut out = [((sector * 7 + u64::from(disk_id)) & 0xFF) as u8; SECTOR];
    out[0..8].copy_from_slice(PATTERN_MAGIC);
    out[8..12].copy_from_slice(&disk_id.to_le_bytes());
    out[12..16].copy_from_slice(&generation.to_le_bytes());
    out[16..24].copy_from_slice(&sector.to_le_bytes());
    let crc = crc32(&out[0..24]);
    out[24..28].copy_from_slice(&crc.to_le_bytes());
    out
}

/// CRC-32 as zlib computes it.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

/// A disk that is zero except for reference data in the given sector ranges `(first sector, count)`.
#[derive(Debug, Clone)]
pub struct ExpectedDisk {
    pub disk_id: u32,
    pub generation: u32,
    pub size: u64,
    pub written: Vec<(u64, u64)>,
}

impl ExpectedDisk {
    /// Whether `sector` holds reference data.
    pub fn is_written(&self, sector: u64) -> bool {
        self.written
            .iter()
            .any(|&(start, count)| (start..start + count).contains(&sector))
    }

    /// The content `sector` must have.
    pub fn sector(&self, sector: u64) -> [u8; SECTOR] {
        if self.is_written(sector) {
            pattern_sector(self.disk_id, self.generation, sector)
        } else {
            [0u8; SECTOR]
        }
    }

    /// Compares the whole of `actual` with the expectation, sector by sector, and returns its SHA-256.
    pub fn assert_matches(&self, actual: &dyn BlockSource, name: &str) -> String {
        assert_eq!(actual.len(), self.size, "{name}: size");
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; CHUNK];
        let mut offset = 0u64;
        while offset < self.size {
            let len = (CHUNK as u64).min(self.size - offset) as usize;
            actual.read_exact_at(offset, &mut buf[..len]).expect("read");
            hasher.update(&buf[..len]);
            for (index, sector) in buf[..len].chunks_exact(SECTOR).enumerate() {
                let number = offset / SECTOR as u64 + index as u64;
                if sector != self.sector(number) {
                    let found = if sector.starts_with(JUNK_MAGIC) {
                        "stale junk"
                    } else if sector.starts_with(PATTERN_MAGIC) {
                        "reference data of another place"
                    } else {
                        "other bytes"
                    };
                    panic!(
                        "{name}: sector {number} holds {found}, expected {}",
                        if self.is_written(number) {
                            "its reference data"
                        } else {
                            "zeros"
                        }
                    );
                }
            }
            offset += len as u64;
        }
        hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

/// Records which bytes are read through it.
#[derive(Debug)]
pub struct TracingSource {
    inner: Arc<dyn BlockSource>,
    reads: Mutex<Vec<(u64, u64)>>,
}

impl TracingSource {
    /// Wraps `inner`.
    pub fn new(inner: Arc<dyn BlockSource>) -> Self {
        TracingSource {
            inner,
            reads: Mutex::new(Vec::new()),
        }
    }

    /// The ranges read so far as `(offset, length)`, sorted and merged.
    pub fn ranges(&self) -> Vec<(u64, u64)> {
        let mut reads = self.reads.lock().expect("trace").clone();
        reads.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::new();
        for (offset, len) in reads {
            match merged.last_mut() {
                Some((start, length)) if *start + *length >= offset => {
                    *length = (*length).max(offset + len - *start);
                }
                _ => merged.push((offset, len)),
            }
        }
        merged
    }
}

impl BlockSource for TracingSource {
    fn len(&self) -> u64 {
        self.inner.len()
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.reads.lock().expect("trace").push((offset, buf.len() as u64));
        self.inner.read_exact_at(offset, buf)
    }
}

/// Returns the bytes of `inner` with some of them replaced: the stand-in for a damaged device.
#[derive(Debug)]
pub struct MutatedSource {
    inner: Arc<dyn BlockSource>,
    patches: BTreeMap<u64, u8>,
}

impl MutatedSource {
    /// Wraps `inner`; byte `offset` reads as `value` for every `(offset, value)` in `patches`.
    pub fn new(inner: Arc<dyn BlockSource>, patches: impl IntoIterator<Item = (u64, u8)>) -> Self {
        MutatedSource {
            inner,
            patches: patches.into_iter().collect(),
        }
    }
}

impl BlockSource for MutatedSource {
    fn len(&self) -> u64 {
        self.inner.len()
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.inner.read_exact_at(offset, buf)?;
        let end = offset + buf.len() as u64;
        for (&at, &value) in self.patches.range(offset..end) {
            buf[(at - offset) as usize] = value;
        }
        Ok(())
    }
}

/// A small deterministic generator, so that failing mutation tests can be reproduced from their seed.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// Creates a generator from a seed.
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    /// Next value (xorshift64*).
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value below `bound`.
    pub fn below(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound.max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openvmfs::MemorySource;

    #[test]
    fn pattern_sector_matches_the_recipe_script() {
        // `python3 fixtures/recipes/pattern.py expect 1048576 7 1 0 2 100 3` prints this hash.
        let expected = ExpectedDisk {
            disk_id: 7,
            generation: 1,
            size: 1 << 20,
            written: vec![(0, 2), (100, 3)],
        };
        let mut image = Vec::new();
        for sector in 0..2048 {
            image.extend_from_slice(&expected.sector(sector));
        }
        assert_eq!(
            sha256_hex(&image),
            "4aaa46531c4743e0793b1432e7a42a7a2ec6694ea407de20a8fc554bc0917321"
        );
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        expected.assert_matches(&MemorySource::new(image), "self");
    }

    #[test]
    fn tracing_merges_ranges() {
        let source = TracingSource::new(Arc::new(MemorySource::new(vec![0u8; 100])));
        let mut buf = [0u8; 10];
        source.read_exact_at(20, &mut buf).unwrap();
        source.read_exact_at(0, &mut buf).unwrap();
        source.read_exact_at(25, &mut buf).unwrap();
        assert_eq!(source.ranges(), vec![(0, 10), (20, 15)]);
    }

    #[test]
    fn mutation_replaces_single_bytes() {
        let source = MutatedSource::new(Arc::new(MemorySource::new(vec![7u8; 16])), [(3, 0xFF), (15, 1)]);
        let mut buf = [0u8; 16];
        source.read_exact_at(0, &mut buf).unwrap();
        assert_eq!(buf[3], 0xFF);
        assert_eq!(buf[15], 1);
        assert_eq!(buf[4], 7);
        let mut tail = [0u8; 2];
        source.read_exact_at(14, &mut tail).unwrap();
        assert_eq!(tail, [7, 1]);
    }
}
