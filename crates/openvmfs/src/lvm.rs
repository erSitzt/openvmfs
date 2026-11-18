//! The VMFS logical volume manager (LVM6): turns a partition into the logical volume that holds the filesystem.
//!
//! Format description: `docs/format/10-partition-lvm.md`.

use std::sync::Arc;

use crate::bytes::{add, mul, View};
use crate::error::{Error, Result, ResultExt};
use crate::io::{check_range, read_vec, BlockSource};
use crate::Uuid;

/// Offset of the LVM device header inside the partition.
pub const DEVICE_HEADER_OFFSET: u64 = 0x10_0000;
/// Magic number of the LVM device header.
pub const LVM_MAGIC: u32 = 0xC001_D00D;

const DEVICE_HEADER_SIZE: u64 = 0xD6;
const VOLUME_DESCRIPTOR_SIZE: u64 = 512;
/// Sectors reserved for the volume table and what follows it, in 512-byte units.
const VOLUME_AREA_SECTORS: u64 = 1024;
const DEVICE_NAME_TABLE_SIZE: u64 = 32 * 256;
const PE_ENTRIES_PER_TABLE: u64 = 8192;
const PE_ENTRY_STRIDE: u64 = 128;
const PE_ENTRY_STRIDE_BYTES: usize = 128;
const PE_ENTRY_SIZE: u64 = 0x25;
const _: () = assert!(PE_ENTRY_SIZE <= PE_ENTRY_STRIDE);
/// Upper bound for physical-extent entries we are willing to walk in one table.
const MAX_PE_ENTRIES: u64 = PE_ENTRIES_PER_TABLE;

/// LVM major version written by ESXi 6.5 and later (VMFS6).
const LVM6_MAJOR: u32 = 6;
const VOLUME_STATE_NORMAL: u32 = 1;

/// Facts from the LVM device header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LvmDevice {
    /// LVM major version (6 for VMFS6).
    pub major_version: u32,
    /// LVM minor version.
    pub minor_version: u32,
    /// UUID of this device.
    pub device_uuid: Uuid,
    /// Size of the device as recorded in the header.
    pub total_bytes: u64,
    /// Metadata alignment of the header area.
    pub md_alignment: u32,
    /// Vendor, model and serial as ESXi recorded them when the volume was created. ESXi compares this with the
    /// device it sees to detect snapshot LUNs.
    pub scsi_vendor: String,
    /// See [`LvmDevice::scsi_vendor`].
    pub scsi_model: String,
}

/// Facts about the logical volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LvmVolume {
    /// UUID of the logical volume.
    pub uuid: Uuid,
    /// Snapshot id of the logical volume.
    pub snap_id: u32,
    /// Name (ESXi uses the UUID text).
    pub name: String,
    /// Size of the logical volume in bytes.
    pub logical_size: u64,
    /// Number of devices the volume spans.
    pub device_count: u32,
    /// Generation counter.
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    logical: u64,
    physical: u64,
    len: u64,
}

/// The logical volume: a [`BlockSource`] whose offsets are volume offsets.
#[derive(Debug)]
pub struct LogicalVolume {
    device: Arc<dyn BlockSource>,
    device_info: LvmDevice,
    volume_info: LvmVolume,
    runs: Vec<Run>,
}

impl LogicalVolume {
    /// Whether `partition` carries an LVM device header.
    pub fn probe(partition: &dyn BlockSource) -> Result<bool> {
        if partition.len() < add(DEVICE_HEADER_OFFSET, DEVICE_HEADER_SIZE, "LVM header end")? {
            return Ok(false);
        }
        let raw = read_vec(partition, DEVICE_HEADER_OFFSET, 4, "LVM device header")?;
        Ok(View::new("LVM device header", DEVICE_HEADER_OFFSET, &raw).u32(0)? == LVM_MAGIC)
    }

    /// Opens the logical volume stored on one partition.
    pub fn open(partition: Arc<dyn BlockSource>) -> Result<Self> {
        let raw = read_vec(
            &*partition,
            DEVICE_HEADER_OFFSET,
            DEVICE_HEADER_SIZE,
            "LVM device header",
        )?;
        let header = View::new("LVM device header", DEVICE_HEADER_OFFSET, &raw);

        let magic = header.u32(0x00)?;
        if magic != LVM_MAGIC {
            return Err(Error::corrupt(format!(
                "LVM device header at {DEVICE_HEADER_OFFSET:#x}: magic {magic:#010x}, expected {LVM_MAGIC:#010x}"
            )));
        }
        let major_version = header.u32(0x04)?;
        if major_version != LVM6_MAJOR {
            return Err(Error::unsupported(format!(
                "LVM major version {major_version} (only LVM {LVM6_MAJOR}, as used by VMFS6, is supported)"
            )));
        }
        let md_alignment = header.u32(0xCA)?;
        if md_alignment != 4096 {
            return Err(Error::unsupported(format!(
                "LVM metadata alignment {md_alignment} (only 4096 has been verified)"
            )));
        }
        let flags = header.u32(0xD2)?;
        if flags != 0 {
            return Err(Error::unsupported(format!(
                "LVM device flags {flags:#x} (1 marks a VMFS-L volume; only plain VMFS6 volumes are supported)"
            )));
        }
        let num_volumes = header.u32(0x66)?;
        if num_volumes != 1 {
            return Err(Error::unsupported(format!(
                "LVM device holds {num_volumes} logical volumes (exactly 1 is expected for VMFS6)"
            )));
        }
        let num_pes = u64::from(header.u32(0x6A)?);
        let num_pe_maps = header.u32(0xBE)?;
        let ext_metadata_offset = header.u64(0xC2)?;
        if ext_metadata_offset != 0 || num_pe_maps > 1 {
            return Err(Error::unsupported(format!(
                "LVM device uses extended extent metadata (next region at {ext_metadata_offset:#x}, \
                 {num_pe_maps} extent maps), as volumes above 16 TiB do; this has not been verified yet"
            )));
        }
        if num_pes == 0 || num_pes > MAX_PE_ENTRIES {
            return Err(Error::corrupt(format!(
                "LVM device header: {num_pes} physical extent entries"
            )));
        }

        let scsi = View::new("LVM device SCSI id", DEVICE_HEADER_OFFSET, header.bytes(0x0C, 78)?);
        let device_info = LvmDevice {
            major_version,
            minor_version: header.u32(0x08)?,
            device_uuid: header.uuid(0x82)?,
            total_bytes: header.u64(0x5E)?,
            md_alignment,
            scsi_vendor: scsi.text(6, 8)?.trim().to_owned(),
            scsi_model: scsi.text(14, 16)?.trim().to_owned(),
        };

        // The volume table follows the device metadata, which occupies one alignment unit.
        let md_alignment = u64::from(md_alignment);
        let volume_offset = add(DEVICE_HEADER_OFFSET, md_alignment, "LVM volume table offset")?;
        let raw_volume = read_vec(
            &*partition,
            volume_offset,
            VOLUME_DESCRIPTOR_SIZE,
            "LVM volume descriptor",
        )?;
        let vol = View::new("LVM volume descriptor", volume_offset, &raw_volume);

        let state = vol.u32(0x10)?;
        if state != VOLUME_STATE_NORMAL {
            return Err(Error::unsupported(format!(
                "LVM volume state {state} (1 = normal is supported; 2 and 3 mean a resignature is pending or done)"
            )));
        }
        let volume_id = vol.u32(0x70)?;
        let first_pe = vol.u64(0x78)?;
        let device_count = vol.u32(0x90)?;
        if device_count != 1 || first_pe != 0 {
            return Err(Error::unsupported(format!(
                "logical volume spans {device_count} devices (first extent {first_pe} on this one); \
                 spanned volumes are not supported yet"
            )));
        }
        let volume_info = LvmVolume {
            uuid: vol.uuid(0x54)?,
            snap_id: vol.u32(0x64)?,
            name: vol.text(0x14, 64)?,
            logical_size: vol.u64(0x00)?,
            device_count,
            generation: vol.u64(0x08)?,
        };
        if volume_info.logical_size == 0 {
            return Err(Error::corrupt("LVM volume descriptor: logical size 0"));
        }

        // Header area: device metadata, the volume table, a reserved area, the device name table, then pairs of
        // physical-extent bitmap and physical-extent table. For LVM6 the volume table has a single entry.
        let volume_area = mul(VOLUME_AREA_SECTORS, 512, "LVM volume area")?;
        let bitmap_offset = add(volume_offset, volume_area, "LVM extent bitmap offset")?;
        let bitmap_size = md_alignment.max(1024);
        let table_offset = add(bitmap_offset, bitmap_size, "LVM extent table offset")?;
        let table_len = mul(num_pes, PE_ENTRY_STRIDE, "LVM extent table size")?;
        debug_assert!(DEVICE_NAME_TABLE_SIZE < volume_area);
        let raw_table = read_vec(&*partition, table_offset, table_len, "LVM extent table")?;

        let mut runs = Vec::new();
        for (index, raw_entry) in raw_table.chunks_exact(PE_ENTRY_STRIDE_BYTES).enumerate() {
            let entry_offset = add(
                table_offset,
                mul(index as u64, PE_ENTRY_STRIDE, "extent entry")?,
                "extent entry",
            )?;
            let entry = View::new("LVM extent entry", entry_offset, raw_entry);
            if entry.u8(0x00)? == 0 || entry.u32(0x05)? != volume_id {
                continue;
            }
            let run = Run {
                physical: entry.u64(0x09)?,
                logical: entry.u64(0x11)?,
                len: entry.u64(0x19)?,
            };
            if run.len == 0 {
                return Err(Error::corrupt(format!(
                    "LVM extent entry at {entry_offset:#x}: length 0"
                )));
            }
            check_range("LVM extent", partition.len(), run.physical, run.len)
                .context(format!("LVM extent entry at {entry_offset:#x}"))?;
            runs.push(run);
        }

        let runs = merge_runs(runs, volume_info.logical_size)?;
        Ok(LogicalVolume {
            device: partition,
            device_info,
            volume_info,
            runs,
        })
    }

    /// Facts from the device header.
    pub fn device_info(&self) -> &LvmDevice {
        &self.device_info
    }

    /// Facts about the logical volume.
    pub fn volume_info(&self) -> &LvmVolume {
        &self.volume_info
    }

    /// Translates a volume offset into an offset on the partition, with the number of bytes that follow it
    /// contiguously.
    pub fn translate(&self, offset: u64) -> Result<(u64, u64)> {
        let index = self.runs.partition_point(|run| run.logical <= offset).checked_sub(1);
        let run = index.and_then(|i| self.runs.get(i)).ok_or_else(|| outside(offset))?;
        let delta = offset.checked_sub(run.logical).ok_or_else(|| outside(offset))?;
        if delta >= run.len {
            return Err(outside(offset));
        }
        Ok((
            add(run.physical, delta, "LVM physical offset")?,
            run.len.saturating_sub(delta),
        ))
    }
}

fn outside(offset: u64) -> Error {
    Error::corrupt(format!("volume offset {offset:#x} is not backed by any LVM extent"))
}

/// Sorts the extents by logical offset, checks that they cover the volume without gaps or overlaps, and merges
/// extents that are adjacent on the device as well.
fn merge_runs(mut runs: Vec<Run>, logical_size: u64) -> Result<Vec<Run>> {
    runs.sort_by_key(|run| run.logical);
    let mut merged: Vec<Run> = Vec::with_capacity(runs.len());
    let mut expected = 0u64;
    for run in runs {
        if run.logical != expected {
            return Err(Error::corrupt(format!(
                "LVM extents do not cover the volume contiguously: expected logical offset {expected:#x}, found {:#x}",
                run.logical
            )));
        }
        expected = add(run.logical, run.len, "LVM extent end")?;
        match merged.last_mut() {
            Some(last) if last.physical.checked_add(last.len) == Some(run.physical) => {
                last.len = add(last.len, run.len, "LVM run length")?;
            }
            _ => merged.push(run),
        }
    }
    if expected < logical_size {
        return Err(Error::corrupt(format!(
            "LVM extents cover {expected:#x} bytes, but the volume is {logical_size:#x} bytes long"
        )));
    }
    Ok(merged)
}

impl BlockSource for LogicalVolume {
    fn len(&self) -> u64 {
        self.volume_info.logical_size
    }

    fn read_exact_at(&self, mut offset: u64, mut buf: &mut [u8]) -> Result<()> {
        check_range("logical volume", self.len(), offset, buf.len() as u64)?;
        while !buf.is_empty() {
            let (physical, available) = self.translate(offset)?;
            let take = usize::try_from(available).unwrap_or(usize::MAX).min(buf.len());
            let (head, tail) = buf.split_at_mut(take);
            self.device.read_exact_at(physical, head)?;
            buf = tail;
            offset = add(offset, take as u64, "volume offset")?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::io::MemorySource;

    fn put(buf: &mut [u8], offset: usize, bytes: &[u8]) {
        buf[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    /// Extent table offset for a 4096-byte alignment.
    pub(crate) const TABLE: usize = 0x10_0000 + 0x1000 + 1024 * 512 + 0x1000;

    /// Builds a partition image with an LVM6 header and the given extents (logical, physical, length).
    pub(crate) fn build_lvm(size: usize, logical_size: u64, extents: &[(u64, u64, u64)]) -> Vec<u8> {
        let mut img = vec![0u8; size];
        let h = DEVICE_HEADER_OFFSET as usize;
        put(&mut img, h, &LVM_MAGIC.to_le_bytes());
        put(&mut img, h + 0x04, &6u32.to_le_bytes());
        put(&mut img, h + 0x08, &21u32.to_le_bytes());
        put(&mut img, h + 0x5E, &(size as u64).to_le_bytes());
        put(&mut img, h + 0x66, &1u32.to_le_bytes());
        put(&mut img, h + 0x6A, &(extents.len() as u32).to_le_bytes());
        put(&mut img, h + 0xCA, &4096u32.to_le_bytes());
        let v = h + 0x1000;
        put(&mut img, v, &logical_size.to_le_bytes());
        put(&mut img, v + 0x10, &1u32.to_le_bytes());
        put(&mut img, v + 0x14, b"test-volume");
        put(&mut img, v + 0x90, &1u32.to_le_bytes());
        for (i, (logical, physical, len)) in extents.iter().enumerate() {
            let e = TABLE + i * 128;
            img[e] = 1;
            put(&mut img, e + 0x09, &physical.to_le_bytes());
            put(&mut img, e + 0x11, &logical.to_le_bytes());
            put(&mut img, e + 0x19, &len.to_le_bytes());
        }
        img
    }

    const MIB: u64 = 1 << 20;

    #[test]
    fn maps_through_extents_in_logical_order() {
        // Two extents stored out of order and not adjacent on the device.
        let mut img = build_lvm(8 << 20, 2 * MIB, &[(MIB, 6 * MIB, MIB), (0, 4 * MIB, MIB)]);
        img[4 << 20] = 0xAA;
        img[(6 << 20) + 5] = 0xBB;
        let lv = LogicalVolume::open(Arc::new(MemorySource::new(img))).unwrap();
        assert_eq!(lv.len(), 2 * MIB);
        assert_eq!(lv.translate(0).unwrap(), (4 * MIB, MIB));
        assert_eq!(lv.translate(MIB + 5).unwrap(), (6 * MIB + 5, MIB - 5));
        let mut buf = [0u8; 1];
        lv.read_exact_at(0, &mut buf).unwrap();
        assert_eq!(buf, [0xAA]);
        lv.read_exact_at(MIB + 5, &mut buf).unwrap();
        assert_eq!(buf, [0xBB]);
        assert!(lv.read_exact_at(2 * MIB, &mut buf).is_err());
    }

    #[test]
    fn a_read_may_cross_extents() {
        let mut img = build_lvm(8 << 20, 2 * MIB, &[(0, 4 * MIB, MIB), (MIB, 6 * MIB, MIB)]);
        img[(5 << 20) - 1] = 1;
        img[6 << 20] = 2;
        let lv = LogicalVolume::open(Arc::new(MemorySource::new(img))).unwrap();
        let mut buf = [0u8; 2];
        lv.read_exact_at(MIB - 1, &mut buf).unwrap();
        assert_eq!(buf, [1, 2]);
    }

    #[test]
    fn adjacent_extents_are_merged() {
        let img = build_lvm(8 << 20, 2 * MIB, &[(0, 4 * MIB, MIB), (MIB, 5 * MIB, MIB)]);
        let lv = LogicalVolume::open(Arc::new(MemorySource::new(img))).unwrap();
        assert_eq!(lv.translate(0).unwrap(), (4 * MIB, 2 * MIB));
    }

    #[test]
    fn gaps_and_short_coverage_are_corrupt() {
        let gap = build_lvm(8 << 20, 2 * MIB, &[(0, 4 * MIB, MIB), (2 * MIB, 6 * MIB, MIB)]);
        assert_eq!(
            LogicalVolume::open(Arc::new(MemorySource::new(gap)))
                .unwrap_err()
                .kind(),
            ErrorKind::Corrupt
        );
        let short = build_lvm(8 << 20, 3 * MIB, &[(0, 4 * MIB, MIB)]);
        assert_eq!(
            LogicalVolume::open(Arc::new(MemorySource::new(short)))
                .unwrap_err()
                .kind(),
            ErrorKind::Corrupt
        );
        let beyond = build_lvm(8 << 20, MIB, &[(0, 7 * MIB + 1, MIB)]);
        assert_eq!(
            LogicalVolume::open(Arc::new(MemorySource::new(beyond)))
                .unwrap_err()
                .kind(),
            ErrorKind::Corrupt
        );
    }

    #[test]
    fn unverified_variants_are_refused() {
        let base = build_lvm(8 << 20, MIB, &[(0, 4 * MIB, MIB)]);
        let h = DEVICE_HEADER_OFFSET as usize;
        let open = |img: Vec<u8>| {
            LogicalVolume::open(Arc::new(MemorySource::new(img)))
                .unwrap_err()
                .kind()
        };

        let mut lvm5 = base.clone();
        put(&mut lvm5, h + 4, &5u32.to_le_bytes());
        assert_eq!(open(lvm5), ErrorKind::Unsupported);

        let mut extended = base.clone();
        put(&mut extended, h + 0xC2, &0x4000_0000u64.to_le_bytes());
        assert_eq!(open(extended), ErrorKind::Unsupported);

        let mut spanned = base.clone();
        put(&mut spanned, h + 0x1000 + 0x90, &2u32.to_le_bytes());
        assert_eq!(open(spanned), ErrorKind::Unsupported);

        let mut resignature = base.clone();
        put(&mut resignature, h + 0x1000 + 0x10, &2u32.to_le_bytes());
        assert_eq!(open(resignature), ErrorKind::Unsupported);

        let mut bad_magic = base;
        bad_magic[h] = 0;
        assert_eq!(open(bad_magic), ErrorKind::Corrupt);
    }

    #[test]
    fn probe_detects_the_header() {
        let img = build_lvm(8 << 20, MIB, &[(0, 4 * MIB, MIB)]);
        assert!(LogicalVolume::probe(&MemorySource::new(img)).unwrap());
        assert!(!LogicalVolume::probe(&MemorySource::new(vec![0u8; 2 << 20])).unwrap());
        assert!(!LogicalVolume::probe(&MemorySource::new(vec![0u8; 16])).unwrap());
    }
}
