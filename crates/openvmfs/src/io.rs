//! Block sources: where the bytes come from.

use std::fmt;
use std::io::{Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use crate::bytes::{add, to_usize};
use crate::error::{Error, Result};

/// A readable, fixed-size run of bytes: a device, an image file, a partition, a logical volume or a file inside the
/// filesystem.
///
/// Implementations are safe to call from many threads at once.
pub trait BlockSource: Send + Sync + fmt::Debug {
    /// Size in bytes.
    fn len(&self) -> u64;

    /// Whether the source has no bytes.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Fills `buf` with the bytes at `offset`, or fails. Never returns a short read.
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<()>;
}

impl<T: BlockSource + ?Sized> BlockSource for Arc<T> {
    fn len(&self) -> u64 {
        (**self).len()
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        (**self).read_exact_at(offset, buf)
    }
}

/// Checks that `[offset, offset + len)` lies inside a source of `size` bytes.
pub(crate) fn check_range(what: &str, size: u64, offset: u64, len: u64) -> Result<()> {
    match offset.checked_add(len) {
        Some(end) if end <= size => Ok(()),
        _ => Err(Error::corrupt(format!(
            "{what}: range {offset:#x}+{len:#x} lies outside the {size:#x}-byte source"
        ))),
    }
}

/// Largest single metadata read. Guards allocations that are sized by on-disk values.
pub(crate) const MAX_METADATA_READ: u64 = 64 << 20;

/// Reads `len` bytes at `offset` into a new buffer. `what` names the structure for error messages.
pub(crate) fn read_vec(source: &dyn BlockSource, offset: u64, len: u64, what: &str) -> Result<Vec<u8>> {
    if len > MAX_METADATA_READ {
        return Err(Error::corrupt(format!(
            "{what}: a {len:#x}-byte metadata read is larger than this library accepts"
        )));
    }
    check_range(what, source.len(), offset, len)?;
    let mut buf = vec![0u8; to_usize(len, what)?];
    source
        .read_exact_at(offset, &mut buf)
        .map_err(|e| e.context(format!("{what} at {offset:#x}")))?;
    Ok(buf)
}

/// Block size that unbuffered reads are aligned to. It suits devices with 512-byte and with 4096-byte sectors.
const DIRECT_ALIGNMENT: u64 = 4096;
/// Bounce buffers kept for reuse, and the largest one that is kept.
const POOLED_BUFFERS: usize = 8;
const POOLED_BUFFER_LIMIT: usize = 8 << 20;

/// An image file or a block device, opened read-only.
pub struct FileSource {
    file: std::fs::File,
    len: u64,
    name: String,
    /// Set when the file was opened for unbuffered I/O: offsets, lengths and buffers must then be multiples of this.
    alignment: Option<u64>,
    /// Aligned buffers for unbuffered reads.
    pool: std::sync::Mutex<Vec<Vec<u8>>>,
}

impl fmt::Debug for FileSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let how = if self.alignment.is_some() { ", unbuffered" } else { "" };
        write!(f, "FileSource({}, {} bytes{how})", self.name, self.len)
    }
}

impl FileSource {
    /// Opens an image file or a block device for reading.
    ///
    /// The size comes from the file metadata, or for block devices on Unix from seeking to the end. For Windows raw
    /// devices use [`FileSource::with_len`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let name = path.display().to_string();
        let file = std::fs::File::open(path).map_err(|e| Error::io(format!("cannot open {name}"), e))?;
        Self::measure(file, name)
    }

    /// Opens a block device or an image file for unbuffered reading: `O_DIRECT` on Linux,
    /// `FILE_FLAG_NO_BUFFERING` on Windows.
    ///
    /// Reads then bypass the page cache of the operating system. That is what a backup of a LUN wants: the data is
    /// read once, and caching it would only push other things out of memory. It also makes sure that what is read
    /// comes from the device and not from a stale cache of an earlier state of a snapshot LUN.
    ///
    /// Callers need not align anything: every read is widened to whole blocks of 4096 bytes and goes through an
    /// aligned buffer.
    pub fn open_direct(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let name = path.display().to_string();
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        direct_flag(&mut options)?;
        let file = options.open(path).map_err(|e| {
            Error::io(
                format!("cannot open {name} for unbuffered reading (not every filesystem supports it; devices do)"),
                e,
            )
        })?;
        Ok(Self::measure(file, name)?.with_alignment(DIRECT_ALIGNMENT))
    }

    fn measure(file: std::fs::File, name: String) -> Result<Self> {
        let metadata = file
            .metadata()
            .map_err(|e| Error::io(format!("cannot stat {name}"), e))?;
        let len = if metadata.is_file() {
            metadata.len()
        } else {
            (&file)
                .seek(SeekFrom::End(0))
                .map_err(|e| Error::io(format!("cannot determine the size of {name}"), e))?
        };
        if len == 0 {
            return Err(Error::invalid_input(format!(
                "{name} has size 0; for a raw device whose size cannot be detected, supply it with FileSource::with_len"
            )));
        }
        Ok(Self::with_len(file, len, name))
    }

    /// Wraps an already opened file or device whose size the caller knows.
    pub fn with_len(file: std::fs::File, len: u64, name: impl Into<String>) -> Self {
        FileSource {
            file,
            len,
            name: name.into(),
            alignment: None,
            pool: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Declares that the file was opened for unbuffered I/O, which needs offsets, lengths and buffers that are
    /// multiples of `alignment` (a power of two, at least the sector size of the device). Reads are widened
    /// accordingly. [`FileSource::open_direct`] does this itself; this is for files the caller opened.
    pub fn with_alignment(mut self, alignment: u64) -> Self {
        self.alignment = (alignment > 1 && alignment.is_power_of_two()).then_some(alignment);
        self
    }

    /// Reads into `buf` as far as the file goes; returns the number of bytes read.
    fn read_some(&self, mut offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut done = 0usize;
        while let Some(rest) = buf.get_mut(done..).filter(|rest| !rest.is_empty()) {
            #[cfg(unix)]
            let n = std::os::unix::fs::FileExt::read_at(&self.file, rest, offset);
            #[cfg(windows)]
            let n = std::os::windows::fs::FileExt::seek_read(&self.file, rest, offset);
            match n {
                Ok(0) => break,
                Ok(n) => {
                    done = done.saturating_add(n);
                    offset = offset.saturating_add(n as u64);
                    // An unbuffered read that ends inside a block has reached the end of the file. Asking again
                    // would mean an unaligned offset, which Windows rejects.
                    if self
                        .alignment
                        .is_some_and(|alignment| offset & alignment.saturating_sub(1) != 0)
                    {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(done)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        let Some(alignment) = self.alignment else {
            return match self.read_some(offset, buf)? {
                n if n == buf.len() => Ok(()),
                _ => Err(unexpected_end()),
            };
        };
        // Widen the read to whole blocks and read them into an aligned buffer. The last block of a file whose
        // size is not a multiple of the alignment comes back short, which is fine as long as it covers the request.
        let mask = alignment.saturating_sub(1);
        let start = offset & !mask;
        let end = offset.saturating_add(buf.len() as u64).saturating_add(mask) & !mask;
        let head = usize::try_from(offset.saturating_sub(start)).map_err(|_| too_large())?;
        let wide = usize::try_from(end.saturating_sub(start)).map_err(|_| too_large())?;
        let align = usize::try_from(alignment).map_err(|_| too_large())?;

        let mut bounce = self.take_buffer(wide.saturating_add(align));
        let pad = bounce.as_ptr().align_offset(align);
        let result = match bounce.get_mut(pad..pad.saturating_add(wide)) {
            Some(aligned) => self.read_some(start, aligned).and_then(|n| {
                let wanted = aligned
                    .get(head..head.saturating_add(buf.len()))
                    .filter(|_| n >= head.saturating_add(buf.len()));
                match wanted {
                    Some(bytes) => {
                        buf.copy_from_slice(bytes);
                        Ok(())
                    }
                    None => Err(unexpected_end()),
                }
            }),
            None => Err(too_large()),
        };
        self.return_buffer(bounce);
        result
    }

    fn take_buffer(&self, len: usize) -> Vec<u8> {
        let pooled = self.pool.lock().ok().and_then(|mut pool| {
            let index = pool.iter().position(|buffer| buffer.len() >= len)?;
            Some(pool.swap_remove(index))
        });
        pooled.unwrap_or_else(|| vec![0u8; len])
    }

    fn return_buffer(&self, buffer: Vec<u8>) {
        if buffer.len() <= POOLED_BUFFER_LIMIT {
            if let Ok(mut pool) = self.pool.lock() {
                if pool.len() < POOLED_BUFFERS {
                    pool.push(buffer);
                }
            }
        }
    }
}

fn unexpected_end() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "unexpected end of file")
}

fn too_large() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, "read too large")
}

/// Adds the flag that asks the operating system for unbuffered I/O.
#[cfg(target_os = "linux")]
fn direct_flag(options: &mut std::fs::OpenOptions) -> Result<()> {
    // O_DIRECT is not the same number everywhere; these are the values of the Linux kernel headers.
    let flag: Option<i32> = if cfg!(any(
        target_arch = "x86_64",
        target_arch = "x86",
        target_arch = "riscv64"
    )) {
        Some(0o40000)
    } else if cfg!(any(target_arch = "aarch64", target_arch = "arm")) {
        Some(0o200000)
    } else {
        None
    };
    let flag = flag.ok_or_else(|| Error::unsupported("unbuffered reading is not wired up for this processor type"))?;
    std::os::unix::fs::OpenOptionsExt::custom_flags(options, flag);
    Ok(())
}

#[cfg(windows)]
fn direct_flag(options: &mut std::fs::OpenOptions) -> Result<()> {
    const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
    std::os::windows::fs::OpenOptionsExt::custom_flags(options, FILE_FLAG_NO_BUFFERING);
    Ok(())
}

#[cfg(not(any(target_os = "linux", windows)))]
fn direct_flag(_options: &mut std::fs::OpenOptions) -> Result<()> {
    Err(Error::unsupported(
        "unbuffered reading is not available on this operating system",
    ))
}

impl BlockSource for FileSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        check_range(&self.name, self.len, offset, buf.len() as u64)?;
        self.read_at(offset, buf).map_err(|e| {
            Error::io(
                format!("reading {} bytes at {offset:#x} from {}", buf.len(), self.name),
                e,
            )
        })
    }
}

/// A window into another source, for example one partition of a disk.
#[derive(Debug, Clone)]
pub struct SubSource {
    inner: Arc<dyn BlockSource>,
    start: u64,
    len: u64,
}

impl SubSource {
    /// Creates a view of `len` bytes starting at `start` inside `inner`.
    pub fn new(inner: Arc<dyn BlockSource>, start: u64, len: u64) -> Result<Self> {
        check_range("partition view", inner.len(), start, len)?;
        Ok(SubSource { inner, start, len })
    }

    /// Offset of this view inside its parent.
    pub fn start(&self) -> u64 {
        self.start
    }
}

impl BlockSource for SubSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        check_range("partition view", self.len, offset, buf.len() as u64)?;
        self.inner
            .read_exact_at(add(self.start, offset, "partition offset")?, buf)
    }
}

/// Bytes held in memory. Useful for tests and for callers that already have an image in memory.
#[derive(Clone)]
pub struct MemorySource {
    data: Arc<[u8]>,
}

impl fmt::Debug for MemorySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MemorySource({} bytes)", self.data.len())
    }
}

impl MemorySource {
    /// Wraps a byte buffer.
    pub fn new(data: impl Into<Arc<[u8]>>) -> Self {
        MemorySource { data: data.into() }
    }
}

impl BlockSource for MemorySource {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        check_range("memory source", self.len(), offset, buf.len() as u64)?;
        let start = to_usize(offset, "memory offset")?;
        let end = start
            .checked_add(buf.len())
            .ok_or_else(|| Error::corrupt("memory range overflows"))?;
        let src = self
            .data
            .get(start..end)
            .ok_or_else(|| Error::corrupt("memory range out of bounds"))?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    #[test]
    fn memory_source_reads_and_rejects_out_of_range() {
        let source = MemorySource::new(vec![1u8, 2, 3, 4]);
        let mut buf = [0u8; 2];
        source.read_exact_at(1, &mut buf).unwrap();
        assert_eq!(buf, [2, 3]);
        assert_eq!(
            source.read_exact_at(3, &mut buf).unwrap_err().kind(),
            ErrorKind::Corrupt
        );
        assert_eq!(
            source.read_exact_at(u64::MAX, &mut buf).unwrap_err().kind(),
            ErrorKind::Corrupt
        );
    }

    #[test]
    fn sub_source_offsets_into_its_parent() {
        let parent: Arc<dyn BlockSource> = Arc::new(MemorySource::new((0u8..32).collect::<Vec<_>>()));
        let sub = SubSource::new(parent.clone(), 8, 8).unwrap();
        let mut buf = [0u8; 3];
        sub.read_exact_at(2, &mut buf).unwrap();
        assert_eq!(buf, [10, 11, 12]);
        assert!(sub.read_exact_at(6, &mut buf).is_err());
        assert!(SubSource::new(parent, 30, 8).is_err());
    }

    #[test]
    fn read_vec_refuses_huge_allocations() {
        let source = MemorySource::new(vec![0u8; 16]);
        assert!(read_vec(&source, 0, MAX_METADATA_READ + 1, "test").is_err());
        assert_eq!(read_vec(&source, 4, 4, "test").unwrap().len(), 4);
    }
}
