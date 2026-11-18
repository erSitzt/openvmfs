//! GUID partition table: just enough to find VMFS partitions on a whole-disk image or LUN.
//!
//! Written from the UEFI specification (GPT header and partition entry layout).

use crate::bytes::{add, mul, View};
use crate::error::Result;
use crate::io::{read_vec, BlockSource};

/// Partition type GUID of VMFS datastores, `AA31E02A-400F-11DB-9590-000C2911D1B8`, in its on-disk byte order.
pub const VMFS_PARTITION_TYPE: [u8; 16] = [
    0x2A, 0xE0, 0x31, 0xAA, 0x0F, 0x40, 0xDB, 0x11, 0x95, 0x90, 0x00, 0x0C, 0x29, 0x11, 0xD1, 0xB8,
];

const SIGNATURE: &[u8; 8] = b"EFI PART";
const MAX_ENTRIES: u32 = 1024;

/// One partition of a GPT disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    /// Position in the partition table, starting at 1 (as tools like `sgdisk` and ESXi's `:1` suffix count).
    pub number: u32,
    /// Type GUID in on-disk byte order.
    pub type_guid: [u8; 16],
    /// First byte of the partition on the disk.
    pub start: u64,
    /// Length in bytes.
    pub len: u64,
}

impl Partition {
    /// Whether this is a VMFS datastore partition.
    pub fn is_vmfs(&self) -> bool {
        self.type_guid == VMFS_PARTITION_TYPE
    }
}

/// Reads the partition table of `disk`. Returns `None` when the disk has no valid GPT.
///
/// The logical sector size is not known for image files, so 512 and 4096 are tried in turn.
pub fn read_partitions(disk: &dyn BlockSource) -> Result<Option<Vec<Partition>>> {
    for sector_size in [512u64, 4096] {
        if let Some(partitions) = read_with_sector_size(disk, sector_size)? {
            return Ok(Some(partitions));
        }
    }
    Ok(None)
}

fn read_with_sector_size(disk: &dyn BlockSource, sector_size: u64) -> Result<Option<Vec<Partition>>> {
    // The header lives in LBA 1.
    if disk.len() < mul(sector_size, 3, "GPT size")? {
        return Ok(None);
    }
    let raw = read_vec(disk, sector_size, sector_size, "GPT header")?;
    let header = View::new("GPT header", sector_size, &raw);
    if header.bytes(0, 8)? != SIGNATURE {
        return Ok(None);
    }
    let header_size = header.u32(12)?;
    if !(92..=sector_size).contains(&u64::from(header_size)) {
        return Ok(None);
    }
    let stored_crc = header.u32(16)?;
    let mut for_crc = header.bytes(0, header_size as usize)?.to_vec();
    if let Some(field) = for_crc.get_mut(16..20) {
        field.fill(0);
    }
    if crc32(&for_crc) != stored_crc {
        return Ok(None);
    }

    let entries_lba = header.u64(72)?;
    let entry_count = header.u32(80)?;
    let entry_size = header.u32(84)?;
    if entry_count > MAX_ENTRIES || !(128..=4096).contains(&entry_size) {
        return Ok(None);
    }
    let table_offset = mul(entries_lba, sector_size, "GPT entry table offset")?;
    let table_len = mul(u64::from(entry_count), u64::from(entry_size), "GPT entry table size")?;
    if add(table_offset, table_len, "GPT entry table end")? > disk.len() {
        return Ok(None);
    }
    let table = read_vec(disk, table_offset, table_len, "GPT entry table")?;
    if crc32(&table) != header.u32(88)? {
        return Ok(None);
    }

    let mut partitions = Vec::new();
    for (index, raw_entry) in table.chunks_exact(entry_size as usize).enumerate() {
        let entry = View::new("GPT entry", table_offset, raw_entry);
        let type_guid: [u8; 16] = entry.bytes(0, 16)?.try_into().unwrap_or([0; 16]);
        if type_guid == [0u8; 16] {
            continue;
        }
        let first_lba = entry.u64(32)?;
        let last_lba = entry.u64(40)?;
        if last_lba < first_lba {
            continue;
        }
        let start = mul(first_lba, sector_size, "partition start")?;
        let sectors = add(last_lba.saturating_sub(first_lba), 1, "partition sectors")?;
        let len = mul(sectors, sector_size, "partition length")?;
        if add(start, len, "partition end")? > disk.len() {
            // A partition that does not fit on the device (for example a truncated image) is not usable.
            continue;
        }
        let number = u32::try_from(index).unwrap_or(u32::MAX).saturating_add(1);
        partitions.push(Partition {
            number,
            type_guid,
            start,
            len,
        });
    }
    Ok(Some(partitions))
}

/// CRC-32 (IEEE 802.3, reflected), as used by GPT.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::io::MemorySource;

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    /// Builds a small GPT disk image with the given partitions (type, first LBA, last LBA).
    pub(crate) fn build_gpt_disk(sector_size: usize, sectors: usize, parts: &[([u8; 16], u64, u64)]) -> Vec<u8> {
        let mut disk = vec![0u8; sector_size * sectors];
        let entry_size = 128usize;
        let entry_count = 128usize;
        let mut table = vec![0u8; entry_size * entry_count];
        for (i, (type_guid, first, last)) in parts.iter().enumerate() {
            let e = &mut table[i * entry_size..(i + 1) * entry_size];
            e[0..16].copy_from_slice(type_guid);
            e[16] = 1 + i as u8;
            e[32..40].copy_from_slice(&first.to_le_bytes());
            e[40..48].copy_from_slice(&last.to_le_bytes());
        }
        let table_off = 2 * sector_size;
        disk[table_off..table_off + table.len()].copy_from_slice(&table);

        let mut header = vec![0u8; 92];
        header[0..8].copy_from_slice(SIGNATURE);
        header[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        header[12..16].copy_from_slice(&92u32.to_le_bytes());
        header[24..32].copy_from_slice(&1u64.to_le_bytes());
        header[72..80].copy_from_slice(&2u64.to_le_bytes());
        header[80..84].copy_from_slice(&(entry_count as u32).to_le_bytes());
        header[84..88].copy_from_slice(&(entry_size as u32).to_le_bytes());
        header[88..92].copy_from_slice(&crc32(&table).to_le_bytes());
        let crc = crc32(&header);
        header[16..20].copy_from_slice(&crc.to_le_bytes());
        disk[sector_size..sector_size + 92].copy_from_slice(&header);
        disk
    }

    #[test]
    fn finds_the_vmfs_partition() {
        let other = [7u8; 16];
        let disk = build_gpt_disk(512, 4096, &[(other, 100, 199), (VMFS_PARTITION_TYPE, 2048, 4000)]);
        let parts = read_partitions(&MemorySource::new(disk)).unwrap().unwrap();
        assert_eq!(parts.len(), 2);
        assert!(!parts[0].is_vmfs());
        assert!(parts[1].is_vmfs());
        assert_eq!(parts[1].number, 2);
        assert_eq!(parts[1].start, 2048 * 512);
        assert_eq!(parts[1].len, (4000 - 2048 + 1) * 512);
    }

    #[test]
    fn supports_4k_sectors() {
        let disk = build_gpt_disk(4096, 600, &[(VMFS_PARTITION_TYPE, 256, 500)]);
        let parts = read_partitions(&MemorySource::new(disk)).unwrap().unwrap();
        assert_eq!(parts[0].start, 256 * 4096);
    }

    #[test]
    fn a_damaged_header_is_not_a_gpt() {
        let mut disk = build_gpt_disk(512, 4096, &[(VMFS_PARTITION_TYPE, 2048, 4000)]);
        disk[512 + 40] ^= 0xff;
        assert!(read_partitions(&MemorySource::new(disk)).unwrap().is_none());
        assert!(read_partitions(&MemorySource::new(vec![0u8; 8192])).unwrap().is_none());
        assert!(read_partitions(&MemorySource::new(vec![0u8; 100])).unwrap().is_none());
    }

    #[test]
    fn partitions_beyond_the_device_are_dropped() {
        let disk = build_gpt_disk(512, 4096, &[(VMFS_PARTITION_TYPE, 2048, 999_999)]);
        let parts = read_partitions(&MemorySource::new(disk)).unwrap().unwrap();
        assert!(parts.is_empty());
    }
}
