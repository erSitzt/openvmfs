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

/// An image file or a block device, opened read-only.
pub struct FileSource {
    file: std::fs::File,
    len: u64,
    name: String,
}

impl fmt::Debug for FileSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FileSource({}, {} bytes)", self.name, self.len)
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
        Ok(FileSource { file, len, name })
    }

    /// Wraps an already opened file or device whose size the caller knows.
    pub fn with_len(file: std::fs::File, len: u64, name: impl Into<String>) -> Self {
        FileSource {
            file,
            len,
            name: name.into(),
        }
    }

    #[cfg(unix)]
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        std::os::unix::fs::FileExt::read_exact_at(&self.file, buf, offset)
    }

    #[cfg(windows)]
    fn read_at(&self, mut offset: u64, mut buf: &mut [u8]) -> std::io::Result<()> {
        use std::os::windows::fs::FileExt;
        while !buf.is_empty() {
            let n = self.file.seek_read(buf, offset)?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "unexpected end of file",
                ));
            }
            buf = buf.get_mut(n..).unwrap_or_default();
            offset = offset.saturating_add(n as u64);
        }
        Ok(())
    }
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
