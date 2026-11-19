//! VMFS6 directories.
//!
//! A directory is a file: a 64 KiB header block, followed by blocks of one metadata alignment unit each. Blocks
//! hold directory entries, hash-collision links or the allocation map that says what each block is.
//!
//! Format description: `docs/format/50-directories.md`.

use crate::bytes::{add, mul, to_usize, View};
use crate::error::{Error, Result};
use crate::fs::addr::FdAddr;
use crate::fs::descriptor::Geometry;
use crate::fs::inode::FileKind;
use crate::io::{read_vec, BlockSource};

const HEADER_BLOCK_SIZE: u64 = 0x1_0000;
const HEADER_SIZE: u64 = 0x5F8;
const HEADER_VERSION: u32 = 0x00F5_0001;
const MAX_ALLOCATION_MAP_BLOCKS: u32 = 128;
const BLOCK_HEADER_SIZE: u64 = 0x40;
const ENTRY_SIZE: u64 = 0x120;
const ENTRY_BYTES: usize = 0x120;
const BLOCK_HEADER_BYTES: usize = 0x40;
const NAME_SIZE: usize = 256;

const BLOCK_TYPE_ENTRIES: u16 = 1;
const BLOCK_TYPE_ALLOCATION_MAP: u16 = 3;

/// One entry of a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// File name.
    pub name: String,
    /// Address of the file descriptor.
    pub addr: FdAddr,
    /// What the entry is, as recorded in the directory.
    pub kind: FileKind,
}

fn entry_kind(raw: u32, name: &str) -> Result<FileKind> {
    Ok(match raw {
        2 => FileKind::Directory,
        3 => FileKind::Regular,
        4 => FileKind::Symlink,
        5 => FileKind::System,
        6 => FileKind::RawDeviceMap,
        other => {
            return Err(Error::corrupt(format!(
                "directory entry {name:?} has unknown type {other}"
            )));
        }
    })
}

fn parse_entry(entry: &View<'_>) -> Result<Option<DirEntry>> {
    let raw_addr = entry.u32(0x04)?;
    if raw_addr == 0 {
        return Ok(None);
    }
    let raw_name = entry.bytes(0x18, NAME_SIZE)?;
    let end = raw_name.iter().position(|&b| b == 0).unwrap_or(raw_name.len());
    let name = std::str::from_utf8(raw_name.get(..end).unwrap_or_default())
        .map_err(|_| Error::corrupt("directory entry name is not valid UTF-8"))?
        .to_owned();
    if name.is_empty() || name.contains('/') {
        return Err(Error::corrupt(format!("directory entry has an invalid name {name:?}")));
    }
    let kind = entry_kind(entry.u32(0x00)?, &name)?;
    Ok(Some(DirEntry {
        addr: FdAddr::from_raw(raw_addr)?,
        kind,
        name,
    }))
}

/// Reads all entries of the directory whose content is `dir`. "." and ".." are not returned.
pub(crate) fn read_dir(dir: &dyn BlockSource, geo: &Geometry) -> Result<Vec<DirEntry>> {
    if dir.len() < HEADER_BLOCK_SIZE {
        return Err(Error::corrupt(format!(
            "directory is {:#x} bytes long, shorter than its {HEADER_BLOCK_SIZE:#x}-byte header block",
            dir.len()
        )));
    }
    let raw_header = read_vec(dir, 0, HEADER_SIZE, "directory header")?;
    let header = View::new("directory header", 0, &raw_header);
    let version = header.u32(0x00)?;
    if version != HEADER_VERSION {
        return Err(Error::unsupported(format!(
            "directory header version {version:#010x} (only {HEADER_VERSION:#010x} has been verified)"
        )));
    }
    let entry_count = header.u32(0x04)?;
    let num_map_blocks = header.u32(0x0C)?;
    if num_map_blocks > MAX_ALLOCATION_MAP_BLOCKS {
        return Err(Error::corrupt(format!(
            "directory header lists {num_map_blocks} allocation map blocks"
        )));
    }

    // The header's count includes "." and "..", which are stored in the header itself.
    let in_header = [0x3B8usize, 0x4D8]
        .into_iter()
        .map(|offset| {
            Ok(u64::from(
                header.bytes(offset, ENTRY_BYTES)?.get(4..8) != Some(&[0u8; 4][..]),
            ))
        })
        .sum::<Result<u64>>()?;
    let expected = u64::from(entry_count).checked_sub(in_header).ok_or_else(|| {
        Error::corrupt(format!(
            "directory header counts {entry_count} entries, fewer than it holds itself"
        ))
    })?;

    let mut entries = Vec::new();
    for block in entry_blocks(dir, &header, num_map_blocks, geo)? {
        read_entry_block(dir, block, geo, &mut entries)?;
    }
    if entries.len() as u64 != expected {
        return Err(Error::corrupt(format!(
            "directory header counts {entry_count} entries, but its blocks hold {} besides the header's {in_header}",
            entries.len()
        )));
    }
    Ok(entries)
}

fn block_offset(block: u64, geo: &Geometry) -> Result<u64> {
    add(
        HEADER_BLOCK_SIZE,
        mul(block, geo.metadata_block_size, "directory block offset")?,
        "directory block offset",
    )
}

/// The numbers of the blocks that hold entries, from the allocation maps: four bits per block, the block's type in
/// the low two. (Bit 2 is set on blocks that still have free slots, bit 3 on blocks never written.)
fn entry_blocks(dir: &dyn BlockSource, header: &View<'_>, map_count: u32, geo: &Geometry) -> Result<Vec<u64>> {
    let block_size = geo.metadata_block_size;
    let payload = block_size.saturating_sub(BLOCK_HEADER_SIZE);
    let mut blocks = Vec::new();
    for map_index in 0..u64::from(map_count) {
        let listed = to_usize(mul(map_index, 4, "allocation map list")?, "allocation map list")?;
        let map_block = u64::from(header.u32(0x10usize.saturating_add(listed))?);
        let map_offset = block_offset(map_block, geo)?;
        let raw_map = read_vec(dir, map_offset, block_size, "directory allocation map")?;
        let map = View::new("directory allocation map", map_offset, &raw_map);
        let map_type = map.u16(0x02)?;
        if map_type != BLOCK_TYPE_ALLOCATION_MAP {
            return Err(Error::corrupt(format!(
                "directory block {map_block} is listed as an allocation map but has type {map_type}"
            )));
        }
        // Every map describes the same number of blocks: two per byte.
        let first = mul(
            map_index,
            mul(payload, 2, "blocks per allocation map")?,
            "directory block number",
        )?;
        let nibbles = map.bytes(BLOCK_HEADER_BYTES, to_usize(payload, "map size")?)?;
        let codes = nibbles.iter().flat_map(|byte| [byte >> 4, byte & 0x0F]);
        for (position, code) in codes.enumerate() {
            if u16::from(code & 0b0011) == BLOCK_TYPE_ENTRIES {
                blocks.push(add(first, position as u64, "directory block number")?);
            }
        }
    }
    Ok(blocks)
}

/// Appends the entries of one entry block: the slots whose bit in the block's bitmap is clear.
fn read_entry_block(dir: &dyn BlockSource, block: u64, geo: &Geometry, entries: &mut Vec<DirEntry>) -> Result<()> {
    let block_size = geo.metadata_block_size;
    let offset = block_offset(block, geo)?;
    if add(offset, block_size, "directory block end")? > dir.len() {
        return Err(Error::corrupt(format!(
            "directory entry block {block} lies beyond the end of the directory ({:#x} bytes)",
            dir.len()
        )));
    }
    let raw_block = read_vec(dir, offset, block_size, "directory entry block")?;
    let entry_block = View::new("directory entry block", offset, &raw_block);
    let block_type = entry_block.u16(0x02)?;
    if block_type != BLOCK_TYPE_ENTRIES {
        return Err(Error::corrupt(format!(
            "directory block {block} is mapped as an entry block but has type {block_type}"
        )));
    }
    let slots_per_block = block_size.saturating_sub(BLOCK_HEADER_SIZE) / ENTRY_SIZE;
    let total_slots = u64::from(entry_block.u16(0x04)?);
    if total_slots > slots_per_block {
        return Err(Error::corrupt(format!(
            "directory entry block {block} claims {total_slots} slots, at most {slots_per_block} fit"
        )));
    }
    let free = entry_block.bytes(0x08, 56)?;
    for slot in 0..total_slots {
        let is_free = free
            .get(to_usize(slot / 8, "slot")?)
            .is_none_or(|byte| byte & (1u8 << (slot % 8)) != 0);
        if is_free {
            continue;
        }
        let slot_offset = to_usize(
            add(BLOCK_HEADER_SIZE, mul(slot, ENTRY_SIZE, "slot offset")?, "slot offset")?,
            "slot offset",
        )?;
        let slot_view = View::new("directory entry", offset, entry_block.bytes(slot_offset, ENTRY_BYTES)?);
        match parse_entry(&slot_view)? {
            Some(entry) => entries.push(entry),
            // ESXi clears a slot when it frees it. A slot marked in use without an address has not been seen.
            None => {
                return Err(Error::corrupt(format!(
                    "slot {slot} of directory entry block {block} is marked in use but holds no entry"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::fs::inode::tests::geometry;
    use crate::io::MemorySource;

    fn put_entry(buf: &mut [u8], at: usize, kind: u32, addr: u32, name: &str) {
        buf[at..at + 4].copy_from_slice(&kind.to_le_bytes());
        buf[at + 4..at + 8].copy_from_slice(&addr.to_le_bytes());
        buf[at + 0x18..at + 0x18 + name.len()].copy_from_slice(name.as_bytes());
    }

    /// Builds a directory with the given entries (type, raw descriptor address, name), 14 per entry block.
    pub(crate) fn build_dir(entries: &[(u32, u32, &str)]) -> Vec<u8> {
        let entry_blocks = entries.len().div_ceil(14).max(1);
        let mut dir = vec![0u8; 0x1_0000 + (1 + entry_blocks) * 4096];
        dir[0..4].copy_from_slice(&HEADER_VERSION.to_le_bytes());
        dir[4..8].copy_from_slice(&(entries.len() as u32 + 2).to_le_bytes());
        dir[0x0C..0x10].copy_from_slice(&1u32.to_le_bytes());
        // The allocation map is block 0, which is what the zeroed list in the header says.
        put_entry(&mut dir, 0x3B8, 2, 4, ".");
        put_entry(&mut dir, 0x4D8, 2, 4, "..");

        let map = 0x1_0000;
        dir[map + 2..map + 4].copy_from_slice(&3u16.to_le_bytes());
        // Block 0 is the map itself (type 3), blocks 1.. are entry blocks (type 1); the rest is free (0b0100).
        dir[map + 0x40..map + 4096].fill(0x44);
        let mut nibbles = vec![3u8];
        nibbles.extend(std::iter::repeat_n(1u8, entry_blocks));
        for (i, nibble) in nibbles.iter().enumerate() {
            let byte = &mut dir[map + 0x40 + i / 2];
            *byte = if i % 2 == 0 {
                (*byte & 0x0F) | (nibble << 4)
            } else {
                (*byte & 0xF0) | nibble
            };
        }

        for (block_index, chunk) in entries.chunks(14).enumerate() {
            let block = 0x1_0000 + (1 + block_index) * 4096;
            dir[block + 2..block + 4].copy_from_slice(&1u16.to_le_bytes());
            dir[block + 4..block + 6].copy_from_slice(&14u16.to_le_bytes());
            dir[block + 8..block + 8 + 56].fill(0xFF);
            for (slot, (kind, addr, name)) in chunk.iter().enumerate() {
                dir[block + 8 + slot / 8] &= !(1 << (slot % 8));
                put_entry(&mut dir, block + 0x40 + slot * 0x120, *kind, *addr, name);
            }
        }
        if entries.is_empty() {
            let block = 0x1_0000 + 4096;
            dir[block + 2..block + 4].copy_from_slice(&1u16.to_le_bytes());
            dir[block + 4..block + 6].copy_from_slice(&14u16.to_le_bytes());
            dir[block + 8..block + 8 + 56].fill(0xFF);
        }
        dir
    }

    fn fd(resource: u32) -> u32 {
        (resource << 22) | 4
    }

    #[test]
    fn lists_entries_across_blocks() {
        let names: Vec<String> = (0..30).map(|i| format!("file-{i:02}.vmdk")).collect();
        let entries: Vec<(u32, u32, &str)> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (3, fd(20 + i as u32), n.as_str()))
            .collect();
        let dir = build_dir(&entries);
        let listed = read_dir(&MemorySource::new(dir), &geometry()).unwrap();
        assert_eq!(listed.len(), 30);
        assert_eq!(listed[0].name, "file-00.vmdk");
        assert_eq!(listed[29].name, "file-29.vmdk");
        assert_eq!(listed[29].addr.resource(), 49);
        assert_eq!(listed[0].kind, FileKind::Regular);
    }

    #[test]
    fn empty_directory_and_free_slots() {
        assert!(read_dir(&MemorySource::new(build_dir(&[])), &geometry())
            .unwrap()
            .is_empty());

        // Free the middle slot: the header then counts one entry fewer.
        let mut dir = build_dir(&[(3, fd(20), "a"), (2, fd(21), "b"), (3, fd(22), "c")]);
        let block = 0x1_0000 + 4096;
        dir[block + 8] |= 0b010;
        dir[4..8].copy_from_slice(&4u32.to_le_bytes());
        let listed = read_dir(&MemorySource::new(dir), &geometry()).unwrap();
        assert_eq!(listed.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["a", "c"]);
    }

    #[test]
    fn inconsistent_directories_are_rejected() {
        let geo = geometry();
        let good = build_dir(&[(3, fd(20), "a"), (3, fd(21), "b")]);
        let kind = |dir: Vec<u8>| read_dir(&MemorySource::new(dir), &geo).unwrap_err().kind();

        let mut too_many = good.clone();
        too_many[4..8].copy_from_slice(&9u32.to_le_bytes());
        assert_eq!(kind(too_many), ErrorKind::Corrupt);

        let mut version = good.clone();
        version[0] = 2;
        assert_eq!(kind(version), ErrorKind::Unsupported);

        let mut wrong_map_type = good.clone();
        wrong_map_type[0x1_0000 + 2] = 1;
        assert_eq!(kind(wrong_map_type), ErrorKind::Corrupt);

        let mut wrong_block_type = good.clone();
        wrong_block_type[0x1_0000 + 4096 + 2] = 2;
        assert_eq!(kind(wrong_block_type), ErrorKind::Corrupt);

        let mut slots = good.clone();
        slots[0x1_0000 + 4096 + 4] = 15;
        assert_eq!(kind(slots), ErrorKind::Corrupt);

        let mut bad_addr = good.clone();
        bad_addr[0x1_0000 + 4096 + 0x40 + 4] = 1;
        assert_eq!(kind(bad_addr), ErrorKind::Corrupt);

        let mut bad_name = good.clone();
        bad_name[0x1_0000 + 4096 + 0x40 + 0x18] = 0xFF;
        assert_eq!(kind(bad_name), ErrorKind::Corrupt);

        assert_eq!(kind(good[..0x8000].to_vec()), ErrorKind::Corrupt);
        // The map says there is an entry block beyond the end of the file.
        assert_eq!(kind(good[..0x1_0000 + 4096].to_vec()), ErrorKind::Corrupt);
    }
}
