//! Block maps: where on the volume each part of a file lives.
//!
//! A map is built once, when a file is opened, by walking the file descriptor's address array and its pointer
//! blocks. Afterwards it is immutable, so reads need no locks.
//!
//! Format description: `docs/format/40-file-descriptor.md`.

use crate::bytes::{add, mul, to_usize};
use crate::error::{Error, Result};
use crate::fs::addr::Addr;
use crate::fs::descriptor::Geometry;
use crate::fs::inode::{AddressingMode, Inode};
use crate::io::{check_range, BlockSource};

/// What backs a range of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExtentKind {
    /// Data stored at this volume offset.
    Data {
        /// Offset on the logical volume.
        volume_offset: u64,
    },
    /// Space is allocated at this volume offset but has never been written ("to be zeroed"). It reads as zeros;
    /// what is on disk there is stale data of whoever used the space before.
    ToBeZeroed {
        /// Offset on the logical volume.
        volume_offset: u64,
    },
    /// Nothing is allocated. Reads as zeros.
    Hole,
}

/// A contiguous range of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Extent {
    /// Offset inside the file.
    pub offset: u64,
    /// Length in bytes.
    pub len: u64,
    /// What backs the range.
    pub kind: ExtentKind,
}

impl Extent {
    fn end(&self) -> u64 {
        self.offset.saturating_add(self.len)
    }
}

/// The system files a block map can depend on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SystemFile {
    FileDescriptors,
    SubBlocks,
    PointerBlocks,
    PointerBlocks2,
}

impl SystemFile {
    pub(crate) fn name(self) -> &'static str {
        match self {
            SystemFile::FileDescriptors => ".fdc.sf",
            SystemFile::SubBlocks => ".sbc.sf",
            SystemFile::PointerBlocks => ".pbc.sf",
            SystemFile::PointerBlocks2 => ".pb2.sf",
        }
    }
}

/// Why a block map could not be built.
#[derive(Debug)]
pub(crate) enum MapError {
    /// A system file that is needed has not been opened yet. Only happens while a volume is being opened.
    Needs(SystemFile),
    /// A real error.
    Fatal(Error),
}

impl From<Error> for MapError {
    fn from(error: Error) -> Self {
        MapError::Fatal(error)
    }
}

/// Access to the structures that addresses refer to.
pub(crate) trait AddressSpace {
    /// Reads the pointer block at `raw` and returns its entries.
    fn read_pointer_block(&self, raw: u64) -> std::result::Result<Vec<u64>, MapError>;

    /// Volume offset of a sub-block.
    fn sub_block_offset(&self, cluster: u64, resource: u64) -> std::result::Result<u64, MapError>;
}

/// The immutable map of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlockMap {
    len: u64,
    extents: Box<[Extent]>,
}

impl BlockMap {
    /// Builds the map of the file described by `inode`.
    pub(crate) fn build(
        inode: &Inode,
        geo: &Geometry,
        space: &dyn AddressSpace,
    ) -> std::result::Result<Self, MapError> {
        let mut builder = Builder {
            geo,
            space,
            extents: Vec::new(),
            covered: 0,
            span: 0,
            blocks: 0,
            length: inode.length,
        };
        builder.run(inode).map_err(|e| match e {
            MapError::Fatal(error) => MapError::Fatal(error.context(format!("file descriptor {}", inode.addr))),
            needs => needs,
        })?;
        Ok(BlockMap {
            len: inode.length,
            extents: builder.extents.into_boxed_slice(),
        })
    }

    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    pub(crate) fn extents(&self) -> &[Extent] {
        &self.extents
    }

    fn extent_at(&self, offset: u64) -> Result<&Extent> {
        let index = self
            .extents
            .partition_point(|extent| extent.offset <= offset)
            .checked_sub(1);
        index
            .and_then(|i| self.extents.get(i))
            .filter(|extent| offset < extent.end())
            .ok_or_else(|| Error::corrupt(format!("file offset {offset:#x} is not covered by the block map")))
    }

    /// Reads file content: holes and to-be-zeroed ranges read as zeros.
    pub(crate) fn read_exact_at(&self, volume: &dyn BlockSource, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.read(volume, offset, buf, false)
    }

    /// Reads metadata stored in a file. Metadata must be backed by written blocks; a hole there means the reference
    /// that led here is wrong.
    pub(crate) fn read_metadata_at(&self, volume: &dyn BlockSource, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.read(volume, offset, buf, true)
    }

    fn read(&self, volume: &dyn BlockSource, mut offset: u64, mut buf: &mut [u8], data_only: bool) -> Result<()> {
        match offset.checked_add(buf.len() as u64) {
            Some(end) if end <= self.len => {}
            _ => {
                return Err(Error::invalid_input(format!(
                    "read of {} bytes at {offset:#x} goes beyond the end of the file ({:#x} bytes)",
                    buf.len(),
                    self.len
                )));
            }
        }
        while !buf.is_empty() {
            let extent = self.extent_at(offset)?;
            let within = offset.saturating_sub(extent.offset);
            let available = extent.len.saturating_sub(within);
            let take = usize::try_from(available).unwrap_or(usize::MAX).min(buf.len());
            let (head, tail) = buf.split_at_mut(take);
            match extent.kind {
                ExtentKind::Data { volume_offset } => {
                    volume.read_exact_at(add(volume_offset, within, "volume offset")?, head)?;
                }
                ExtentKind::ToBeZeroed { .. } | ExtentKind::Hole if data_only => {
                    return Err(Error::corrupt(format!(
                        "metadata at file offset {offset:#x} lies in a block that is unallocated or was never written"
                    )));
                }
                ExtentKind::ToBeZeroed { .. } | ExtentKind::Hole => head.fill(0),
            }
            buf = tail;
            offset = add(offset, take as u64, "file offset")?;
        }
        Ok(())
    }

    /// Volume offset of the `len` bytes at file offset `offset`, which must be written data in one piece.
    pub(crate) fn locate(&self, offset: u64, len: u64) -> Result<u64> {
        let extent = self.extent_at(offset)?;
        let within = offset.saturating_sub(extent.offset);
        match extent.kind {
            ExtentKind::Data { volume_offset } if extent.len.saturating_sub(within) >= len => {
                add(volume_offset, within, "volume offset")
            }
            ExtentKind::Data { .. } => Err(Error::unsupported(format!(
                "a {len:#x}-byte metadata structure at file offset {offset:#x} is split across non-adjacent blocks"
            ))),
            _ => Err(Error::corrupt(format!(
                "metadata at file offset {offset:#x} lies in a block that is unallocated or was never written"
            ))),
        }
    }
}

/// The error for structures whose reading rules are known from other implementations only.
fn unverified(what: String) -> Error {
    Error::unsupported(format!(
        "{what}: how to read this has not been verified against data written by ESXi yet; \
         it is refused by default (see OpenOptions::accept_unverified)"
    ))
}

struct Builder<'a> {
    geo: &'a Geometry,
    space: &'a dyn AddressSpace,
    extents: Vec<Extent>,
    /// Bytes of the file covered so far.
    covered: u64,
    /// Bytes of file covered by one entry of the innermost array.
    span: u64,
    /// Number of innermost entries the file needs.
    blocks: u64,
    /// Length of the file in bytes.
    length: u64,
}

impl Builder<'_> {
    fn run(&mut self, inode: &Inode) -> std::result::Result<(), MapError> {
        if inode.length == 0 {
            return Ok(());
        }
        if inode.addressing == AddressingMode::Resident {
            if inode.length > self.geo.fd_data_size {
                return Err(Error::corrupt(format!(
                    "resident file of {} bytes, but a descriptor holds at most {}",
                    inode.length, self.geo.fd_data_size
                ))
                .into());
            }
            let volume_offset = add(inode.volume_offset, self.geo.fd_data_offset, "resident data offset")?;
            self.push(inode.length, ExtentKind::Data { volume_offset })?;
            return Ok(());
        }

        if inode.addressing == AddressingMode::None {
            return Err(Error::unsupported(format!(
                "a {:?} of {} bytes that addresses no blocks",
                inode.kind, inode.length
            ))
            .into());
        }
        if 1u64.checked_shl(u32::from(inode.block_offset_shift)) != Some(inode.block_size) {
            return Err(Error::unsupported(format!(
                "block size {} does not match block offset shift {}",
                inode.block_size, inode.block_offset_shift
            ))
            .into());
        }
        // Files made of sub-blocks count in sub-block units, all others in file blocks.
        if inode.block_size != self.geo.file_block_size && inode.block_size != self.geo.sub_block_size {
            return Err(Error::unsupported(format!(
                "per-file block size {} (verified: {} and {})",
                inode.block_size, self.geo.file_block_size, self.geo.sub_block_size
            ))
            .into());
        }
        self.span = inode.block_size;
        self.blocks = inode.length.div_ceil(self.span);

        let levels = inode.addressing.levels();
        let per_entry = self.blocks_per_entry(levels)?;
        let needed = self.blocks.div_ceil(per_entry);
        if needed > inode.addrs.len() as u64 {
            return Err(Error::corrupt(format!(
                "a file of {} bytes needs {needed} entries in its address array with {:?} addressing, \
                 but the array has {}",
                inode.length,
                inode.addressing,
                inode.addrs.len()
            ))
            .into());
        }
        let entries = inode
            .addrs
            .get(..to_usize(needed, "address array length")?)
            .unwrap_or_default();
        self.walk(levels, entries, 0)?;

        // The last block usually extends beyond the end of the file.
        self.clip(inode.length);
        Ok(())
    }

    /// Number of innermost entries covered by one entry at pointer level `level`.
    fn blocks_per_entry(&self, level: u32) -> Result<u64> {
        let mut blocks = 1u64;
        for _ in 0..level {
            blocks = mul(blocks, self.geo.ptrs_per_block, "blocks per pointer entry")?;
        }
        Ok(blocks)
    }

    fn walk(&mut self, level: u32, entries: &[u64], first_block: u64) -> std::result::Result<(), MapError> {
        let per_entry = self.blocks_per_entry(level)?;
        for (index, &raw) in entries.iter().enumerate() {
            let start = add(first_block, mul(index as u64, per_entry, "block index")?, "block index")?;
            if start >= self.blocks {
                break;
            }
            let blocks = per_entry.min(self.blocks.saturating_sub(start));
            if level == 0 {
                self.leaf(raw, start)?;
            } else if raw == 0 {
                // A missing pointer block: everything below it is unallocated.
                self.push(mul(blocks, self.span, "hole length")?, ExtentKind::Hole)?;
            } else {
                let child = self.space.read_pointer_block(raw)?;
                let child_per_entry = self.blocks_per_entry(level.saturating_sub(1))?;
                let needed = to_usize(blocks.div_ceil(child_per_entry), "pointer block entries")?;
                let child_entries = child.get(..needed).ok_or_else(|| {
                    Error::corrupt(format!(
                        "pointer block {raw:#018x} has {} entries, {needed} are needed",
                        child.len()
                    ))
                })?;
                self.walk(level.saturating_sub(1), child_entries, start)?;
            }
        }
        Ok(())
    }

    fn leaf(&mut self, raw: u64, block: u64) -> std::result::Result<(), MapError> {
        let file_offset = mul(block, self.span, "file offset")?;
        let geo = self.geo;
        let (base, block_size, tbz) = match Addr::decode(raw)? {
            Addr::Null => {
                self.push(self.span, ExtentKind::Hole)?;
                return Ok(());
            }
            Addr::SmallFileBlock { cluster, resource, tbz } => {
                if resource >= geo.sfb_per_cluster {
                    return Err(Error::corrupt(format!(
                        "small file block address {raw:#018x}: resource {resource} in a cluster of {}",
                        geo.sfb_per_cluster
                    ))
                    .into());
                }
                let number = add(
                    mul(cluster, geo.sfb_per_cluster, "file block number")?,
                    resource,
                    "file block number",
                )?;
                (
                    mul(number, geo.file_block_size, "file block offset")?,
                    geo.file_block_size,
                    tbz,
                )
            }
            Addr::LargeFileBlock { block, tbz } => {
                // Verified on a 256 GiB datastore written by ESXi 7.0.3: a large file block appears in every
                // entry it backs (512 of them), and each entry carries its own to-be-zeroed bit.
                let size = 1u64 << geo.lfb_shift;
                (mul(block, size, "large file block offset")?, size, tbz)
            }
            Addr::SubBlock { cluster, resource } => {
                (self.space.sub_block_offset(cluster, resource)?, geo.sub_block_size, 0)
            }
            Addr::PointerBlock { .. } | Addr::PointerBlock2 { .. } => {
                return Err(Error::corrupt(format!(
                    "pointer block address {raw:#018x} where a data block is expected"
                ))
                .into());
            }
        };

        // An entry covers `span` bytes of the file. A block larger than that (a large file block) appears in every
        // entry it backs, each time at the matching position inside the block.
        let within = file_offset & block_size.saturating_sub(1);
        let remaining_in_file = self.file_remaining(file_offset);
        let len = self.span.min(remaining_in_file);
        if add(within, len, "block range")? > block_size {
            return Err(Error::corrupt(format!(
                "address {raw:#018x} backs {len:#x} bytes at file offset {file_offset:#x}, more than its \
                 {block_size:#x}-byte block holds"
            ))
            .into());
        }
        let volume_offset = add(base, within, "volume offset")?;
        check_range("block address", geo.volume_size, volume_offset, len)
            .map_err(|e| e.context(format!("address {raw:#018x}")))?;
        // Verified on volumes written by ESXi 7.0.3: an entry of a lazy-zeroed disk whose MiB was never written
        // carries exactly bit 7 (bitmap value 1), for small and for large file blocks alike. The first write into
        // that MiB, however small, zeroes the rest of it on disk and clears the bit. Other values were never seen.
        let kind = match tbz {
            0 => ExtentKind::Data { volume_offset },
            1 => ExtentKind::ToBeZeroed { volume_offset },
            _ if geo.accept_unverified => ExtentKind::ToBeZeroed { volume_offset },
            _ => {
                return Err(unverified(format!("to-be-zeroed bitmap {tbz:#04x} in block address {raw:#018x}")).into());
            }
        };
        // Push the full span so that offsets stay block-aligned; `clip` trims the tail of the file.
        self.push(self.span, kind)?;
        Ok(())
    }

    fn file_remaining(&self, file_offset: u64) -> u64 {
        self.length.saturating_sub(file_offset)
    }

    fn push(&mut self, len: u64, kind: ExtentKind) -> Result<()> {
        let offset = self.covered;
        self.covered = add(self.covered, len, "file coverage")?;
        if let Some(last) = self.extents.last_mut() {
            let adjacent = match (last.kind, kind) {
                (ExtentKind::Hole, ExtentKind::Hole) => true,
                (ExtentKind::Data { volume_offset: a }, ExtentKind::Data { volume_offset: b })
                | (ExtentKind::ToBeZeroed { volume_offset: a }, ExtentKind::ToBeZeroed { volume_offset: b }) => {
                    a.checked_add(last.len) == Some(b)
                }
                _ => false,
            };
            if adjacent {
                last.len = add(last.len, len, "extent length")?;
                return Ok(());
            }
        }
        self.extents.push(Extent { offset, len, kind });
        Ok(())
    }

    fn clip(&mut self, length: u64) {
        while let Some(last) = self.extents.last_mut() {
            if last.offset >= length {
                self.extents.pop();
            } else {
                last.len = last.len.min(length.saturating_sub(last.offset));
                break;
            }
        }
        self.covered = self.covered.min(length);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::error::ErrorKind;
    use crate::fs::addr::FdAddr;
    use crate::fs::inode::tests::{build_fd, geometry_with};
    use crate::io::MemorySource;

    const MIB: u64 = 1 << 20;

    fn sfb(number: u64) -> u64 {
        ((number / 512) << 15) | ((number % 512) << 51) | 1
    }

    fn lfb(number: u64) -> u64 {
        (number << 15) | 7
    }

    #[derive(Default)]
    struct FakeSpace {
        pointer_blocks: HashMap<u64, Vec<u64>>,
        sub_blocks: HashMap<(u64, u64), u64>,
        missing: Option<SystemFile>,
    }

    impl AddressSpace for FakeSpace {
        fn read_pointer_block(&self, raw: u64) -> std::result::Result<Vec<u64>, MapError> {
            if let Some(missing) = self.missing {
                return Err(MapError::Needs(missing));
            }
            let mut block = self
                .pointer_blocks
                .get(&raw)
                .cloned()
                .ok_or_else(|| Error::corrupt("no such pointer block"))?;
            block.resize(8192, 0);
            Ok(block)
        }

        fn sub_block_offset(&self, cluster: u64, resource: u64) -> std::result::Result<u64, MapError> {
            Ok(*self
                .sub_blocks
                .get(&(cluster, resource))
                .ok_or_else(|| Error::corrupt("no such sub-block"))?)
        }
    }

    fn map(zla: u32, len: u64, addrs: &[u64], space: &FakeSpace) -> std::result::Result<BlockMap, MapError> {
        map_with(true, zla, len, addrs, space)
    }

    fn map_with(
        accept_unverified: bool,
        zla: u32,
        len: u64,
        addrs: &[u64],
        space: &FakeSpace,
    ) -> std::result::Result<BlockMap, MapError> {
        let geo = geometry_with(accept_unverified);
        let addr = FdAddr::system(20);
        let raw = build_fd(addr, 0x80_0000, 3, zla, len, (MIB, 20), addrs);
        let inode = Inode::parse(&raw, addr, 0x80_0000, &geo).unwrap();
        BlockMap::build(&inode, &geo, space)
    }

    fn fatal_kind(result: std::result::Result<BlockMap, MapError>) -> ErrorKind {
        match result {
            Err(MapError::Fatal(e)) => e.kind(),
            other => panic!("expected a fatal error, got {other:?}"),
        }
    }

    fn data(offset: u64, len: u64, volume_offset: u64) -> Extent {
        Extent {
            offset,
            len,
            kind: ExtentKind::Data { volume_offset },
        }
    }

    fn hole(offset: u64, len: u64) -> Extent {
        Extent {
            offset,
            len,
            kind: ExtentKind::Hole,
        }
    }

    #[test]
    fn direct_blocks_merge_when_adjacent_and_the_tail_is_clipped() {
        let space = FakeSpace::default();
        let m = map(1, 3 * MIB + 5, &[sfb(8), sfb(9), sfb(700), sfb(701)], &space).unwrap();
        assert_eq!(
            m.extents(),
            &[data(0, 2 * MIB, 8 * MIB), data(2 * MIB, MIB + 5, 700 * MIB)]
        );
        assert_eq!(m.len(), 3 * MIB + 5);
    }

    #[test]
    fn zero_entries_are_holes_not_the_end() {
        let space = FakeSpace::default();
        let m = map(1, 4 * MIB, &[sfb(8), 0, 0, sfb(11)], &space).unwrap();
        assert_eq!(
            m.extents(),
            &[data(0, MIB, 8 * MIB), hole(MIB, 2 * MIB), data(3 * MIB, MIB, 11 * MIB)]
        );
    }

    #[test]
    fn to_be_zeroed_blocks_are_kept_apart_from_data() {
        let space = FakeSpace::default();
        let tbz = sfb(9) | (0xFF << 7);
        let m = map(1, 3 * MIB, &[sfb(8), tbz, sfb(10)], &space).unwrap();
        assert_eq!(
            m.extents(),
            &[
                data(0, MIB, 8 * MIB),
                Extent {
                    offset: MIB,
                    len: MIB,
                    kind: ExtentKind::ToBeZeroed { volume_offset: 9 * MIB }
                },
                data(2 * MIB, MIB, 10 * MIB),
            ]
        );
    }

    #[test]
    fn stale_bytes_under_to_be_zeroed_blocks_never_reach_the_reader() {
        let mut volume = vec![0xEEu8; 16 << 20];
        volume[(8 << 20)..(9 << 20)].fill(0x11);
        let volume = MemorySource::new(volume);
        let space = FakeSpace::default();
        let m = map(1, 3 * MIB, &[sfb(8), sfb(9) | (1 << 7), 0], &space).unwrap();
        let mut buf = vec![0xAAu8; 3 << 20];
        m.read_exact_at(&volume, 0, &mut buf).unwrap();
        assert!(buf[..1 << 20].iter().all(|&b| b == 0x11));
        assert!(
            buf[1 << 20..].iter().all(|&b| b == 0),
            "to-be-zeroed block and hole must read as zeros"
        );
        // Metadata must not be read from such blocks.
        let mut small = [0u8; 16];
        assert_eq!(
            m.read_metadata_at(&volume, MIB, &mut small).unwrap_err().kind(),
            ErrorKind::Corrupt
        );
        assert_eq!(m.locate(2 * MIB, 16).unwrap_err().kind(), ErrorKind::Corrupt);
        assert_eq!(m.locate(4096, 8192).unwrap(), 8 * MIB + 4096);
    }

    #[test]
    fn single_indirect() {
        let mut space = FakeSpace::default();
        let pb0 = (1u64 << 56) | 2; // a pointer block stored as a sub-block
        let mut entries = vec![0u64; 8192];
        entries[0] = sfb(100);
        entries[1] = sfb(101);
        entries[8191] = sfb(300);
        space.pointer_blocks.insert(pb0, entries);
        // Second top-level entry is missing entirely: 8192 MiB of hole, clipped by the file length.
        let len = 8192 * MIB + 10 * MIB;
        let m = map(3, len, &[pb0, 0], &space).unwrap();
        assert_eq!(
            m.extents(),
            &[
                data(0, 2 * MIB, 100 * MIB),
                hole(2 * MIB, 8189 * MIB),
                data(8191 * MIB, MIB, 300 * MIB),
                hole(8192 * MIB, 10 * MIB),
            ]
        );
    }

    #[test]
    fn double_indirect() {
        let mut space = FakeSpace::default();
        let top = (1u64 << 56) | 2;
        let mid = (2u64 << 56) | 2;
        let mut top_entries = vec![0u64; 8192];
        top_entries[1] = mid;
        space.pointer_blocks.insert(top, top_entries);
        let mut mid_entries = vec![0u64; 8192];
        mid_entries[5] = sfb(42);
        space.pointer_blocks.insert(mid, mid_entries);
        // Block index 8192 + 5 holds data; the file is 3 * 8192 MiB long.
        let len = 3 * 8192 * MIB;
        let m = map(0x10D0, len, &[top], &space).unwrap();
        assert_eq!(
            m.extents(),
            &[
                hole(0, (8192 + 5) * MIB),
                data((8192 + 5) * MIB, MIB, 42 * MIB),
                hole((8192 + 6) * MIB, len - (8192 + 6) * MIB),
            ]
        );
    }

    #[test]
    fn a_large_file_block_backs_many_entries() {
        let space = FakeSpace::default();
        // Three consecutive entries carry the same large file block: one contiguous extent.
        let m = map(1, 3 * MIB, &[lfb(2), lfb(2), lfb(2)], &space).unwrap();
        assert_eq!(m.extents(), &[data(0, 3 * MIB, 2 * 512 * MIB)]);
    }

    #[test]
    fn only_verified_structures_are_read_by_default() {
        let space = FakeSpace::default();
        // Bit 7 is what ESXi writes for a never-written MiB: read as zeros.
        let lazy = map_with(false, 1, 2 * MIB, &[sfb(8) | (1 << 7), sfb(9)], &space).unwrap();
        assert_eq!(
            lazy.extents(),
            &[
                Extent {
                    offset: 0,
                    len: MIB,
                    kind: ExtentKind::ToBeZeroed { volume_offset: 8 * MIB }
                },
                data(MIB, MIB, 9 * MIB)
            ]
        );
        // The same inside a large file block: the entry for each MiB has its own bit.
        let large = map_with(false, 1, 3 * MIB, &[lfb(2), lfb(2) | (1 << 7), lfb(2)], &space).unwrap();
        let base = 2 * 512 * MIB;
        assert_eq!(
            large.extents(),
            &[
                data(0, MIB, base),
                Extent {
                    offset: MIB,
                    len: MIB,
                    kind: ExtentKind::ToBeZeroed {
                        volume_offset: base + MIB
                    }
                },
                data(2 * MIB, MIB, base + 2 * MIB)
            ]
        );
        // Other bitmap values have never been seen on a volume written by ESXi.
        assert_eq!(
            fatal_kind(map_with(false, 1, MIB, &[sfb(8) | (2 << 7)], &space)),
            ErrorKind::Unsupported
        );
        assert_eq!(
            fatal_kind(map_with(false, 1, MIB, &[sfb(8) | (0xFF << 7)], &space)),
            ErrorKind::Unsupported
        );
        assert_eq!(
            fatal_kind(map_with(false, 1, MIB, &[lfb(2) | (3 << 7)], &space)),
            ErrorKind::Unsupported
        );
        assert!(map_with(true, 1, MIB, &[lfb(2) | (3 << 7)], &space).is_ok());
    }

    #[test]
    fn sub_blocks_and_resident_data() {
        let mut space = FakeSpace::default();
        space.sub_blocks.insert((3, 7), 0x123_0000);
        let m = map(2, 5000, &[(7u64 << 56) | (3 << 6) | 2], &space).unwrap();
        assert_eq!(m.extents(), &[data(0, 5000, 0x123_0000)]);

        let resident = map(0x10D1, 100, &[], &space).unwrap();
        assert_eq!(resident.extents(), &[data(0, 100, 0x80_0000 + 0x1200)]);
        assert_eq!(fatal_kind(map(0x10D1, 4000, &[], &space)), ErrorKind::Corrupt);

        let empty = map(1, 0, &[sfb(8)], &space).unwrap();
        assert!(empty.extents().is_empty());
    }

    #[test]
    fn a_sub_block_cannot_back_more_than_its_size() {
        let mut space = FakeSpace::default();
        space.sub_blocks.insert((0, 0), 0x10_0000);
        assert_eq!(fatal_kind(map(2, 70_000, &[2], &space)), ErrorKind::Corrupt);
    }

    #[test]
    fn bad_addresses_are_errors() {
        let space = FakeSpace::default();
        // Beyond the end of the 4 GiB volume.
        assert_eq!(fatal_kind(map(1, MIB, &[sfb(5000)], &space)), ErrorKind::Corrupt);
        // Resource number outside its cluster.
        assert_eq!(
            fatal_kind(map(1, MIB, &[(600u64 << 51) | 1], &space)),
            ErrorKind::Corrupt
        );
        // A pointer block where data is expected.
        assert_eq!(fatal_kind(map(1, MIB, &[5], &space)), ErrorKind::Corrupt);
        // Too large for direct addressing.
        assert_eq!(fatal_kind(map(1, 321 * MIB, &[sfb(8)], &space)), ErrorKind::Corrupt);
        // Unknown bits.
        assert_eq!(fatal_kind(map(1, MIB, &[sfb(8) | 8], &space)), ErrorKind::Unsupported);
    }

    #[test]
    fn a_missing_system_file_is_reported_as_a_dependency() {
        let space = FakeSpace {
            missing: Some(SystemFile::PointerBlocks2),
            ..FakeSpace::default()
        };
        assert!(matches!(
            map(5, 2 * MIB, &[5], &space),
            Err(MapError::Needs(SystemFile::PointerBlocks2))
        ));
    }

    #[test]
    fn reads_beyond_the_end_are_refused() {
        let volume = MemorySource::new(vec![0u8; 16 << 20]);
        let m = map(1, MIB, &[sfb(8)], &FakeSpace::default()).unwrap();
        let mut buf = [0u8; 2];
        assert_eq!(
            m.read_exact_at(&volume, MIB - 1, &mut buf).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
        assert!(m.read_exact_at(&volume, MIB - 2, &mut buf).is_ok());
    }
}
