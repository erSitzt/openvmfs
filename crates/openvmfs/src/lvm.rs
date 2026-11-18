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
const DEVICE_NAME_SIZE: u64 = 256;
const DEVICE_NAME_SIZE_BYTES: usize = 256;
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

/// Size of the units ("physical extents") in which a volume descriptor counts a device's share of the volume.
const PE_SIZE: u64 = 256 << 20;
/// Most devices a datastore may span.
const MAX_DEVICES: usize = 32;

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
    /// Where this device's share of the volume starts, in bytes of the volume. 0 on the first device.
    pub volume_start: u64,
    /// Size of this device's share of the volume in bytes.
    pub volume_len: u64,
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
    /// The devices of the volume as ESXi named them when they were added (for example `naa.6000...:1`), in volume
    /// order. Recorded on the first device.
    pub device_names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    logical: u64,
    /// Index of the device, in the order the devices were passed in.
    device: usize,
    physical: u64,
    len: u64,
}

/// What one device says about itself and about the volume.
#[derive(Debug)]
struct DeviceMetadata {
    info: LvmDevice,
    volume: LvmVolume,
    /// First and last 256 MiB unit of the volume on this device.
    first_pe: u64,
    last_pe: u64,
    /// Extents on this device as (logical offset, offset in the partition, length).
    extents: Vec<(u64, u64, u64)>,
}

/// The logical volume: a [`BlockSource`] whose offsets are volume offsets.
#[derive(Debug)]
pub struct LogicalVolume {
    /// The partitions, in the order they were passed in.
    devices: Vec<Arc<dyn BlockSource>>,
    /// Device headers in volume order.
    device_infos: Vec<LvmDevice>,
    /// Header of the volume's first device.
    head_info: LvmDevice,
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
        Self::open_devices(vec![partition])
    }

    /// Opens the logical volume stored on the given partitions: one for an ordinary datastore, all of them, in any
    /// order, for a datastore that spans several devices.
    pub fn open_devices(partitions: Vec<Arc<dyn BlockSource>>) -> Result<Self> {
        if partitions.is_empty() {
            return Err(Error::invalid_input("no device given"));
        }
        if partitions.len() > MAX_DEVICES {
            return Err(Error::invalid_input(format!(
                "{} devices given; a datastore spans at most {MAX_DEVICES}",
                partitions.len()
            )));
        }
        let mut metadata = Vec::with_capacity(partitions.len());
        for (index, partition) in partitions.iter().enumerate() {
            let device = read_device(&**partition);
            metadata.push(if partitions.len() == 1 {
                device?
            } else {
                device.context(format!("device {index}"))?
            });
        }

        // The devices in volume order, each with its position in `partitions`.
        let mut order: Vec<(usize, &DeviceMetadata)> = metadata.iter().enumerate().collect();
        order.sort_by_key(|(_, device)| device.first_pe);
        let (_, head) = *order.first().ok_or_else(|| Error::invalid_input("no device given"))?;

        for (index, device) in &order {
            if (device.volume.uuid, device.volume.snap_id) != (head.volume.uuid, head.volume.snap_id) {
                return Err(Error::invalid_input(format!(
                    "the devices belong to different volumes: device {index} holds a part of {}, another one a part \
                     of {}",
                    device.volume.uuid, head.volume.uuid
                )));
            }
            if device.volume.logical_size != head.volume.logical_size
                || device.volume.device_count != head.volume.device_count
            {
                return Err(Error::corrupt(format!(
                    "the devices disagree about the volume: device {index} says {} bytes on {} devices, another one \
                     {} bytes on {} devices",
                    device.volume.logical_size,
                    device.volume.device_count,
                    head.volume.logical_size,
                    head.volume.device_count
                )));
            }
        }
        for pair in order.windows(2) {
            if let [(_, a), (index, b)] = pair {
                if a.info.device_uuid == b.info.device_uuid || a.first_pe == b.first_pe {
                    return Err(Error::invalid_input(format!(
                        "device {index} was given twice, or is a copy of another one (device id {})",
                        b.info.device_uuid
                    )));
                }
            }
        }
        let expected = usize::try_from(head.volume.device_count).unwrap_or(usize::MAX);
        if expected != partitions.len() || head.first_pe != 0 {
            let names = if head.first_pe == 0 && !head.volume.device_names.is_empty() {
                format!(" ({})", head.volume.device_names.join(", "))
            } else {
                String::new()
            };
            return Err(Error::invalid_input(format!(
                "the datastore spans {expected} devices{names}, but {} {} given{}; all of them are needed to read it",
                partitions.len(),
                if partitions.len() == 1 { "was" } else { "were" },
                if head.first_pe == 0 {
                    ""
                } else {
                    ", and the first one is missing"
                },
            )));
        }
        // Each device continues where the one before it ends.
        let mut next_pe = 0u64;
        for (index, device) in &order {
            if device.first_pe != next_pe {
                return Err(Error::corrupt(format!(
                    "device {index} starts at extent {} of the volume, expected {next_pe}",
                    device.first_pe
                )));
            }
            next_pe = add(device.last_pe, 1, "LVM extent number")?;
        }

        let mut runs = Vec::new();
        for (index, device) in &order {
            let partition_len = partitions.get(*index).map_or(0, |partition| partition.len());
            for &(logical, physical, len) in &device.extents {
                check_range("LVM extent", partition_len, physical, len).context(format!("device {index}"))?;
                runs.push(Run {
                    logical,
                    device: *index,
                    physical,
                    len,
                });
            }
        }
        let runs = merge_runs(runs, head.volume.logical_size)?;
        let volume_info = head.volume.clone();
        let head_info = head.info.clone();
        let device_infos = order.iter().map(|(_, device)| device.info.clone()).collect();
        Ok(LogicalVolume {
            devices: partitions,
            device_infos,
            head_info,
            volume_info,
            runs,
        })
    }

    /// Facts from the header of the volume's first device.
    pub fn device_info(&self) -> &LvmDevice {
        &self.head_info
    }

    /// Facts from the device headers, in volume order.
    pub fn devices(&self) -> &[LvmDevice] {
        &self.device_infos
    }

    /// Facts about the logical volume.
    pub fn volume_info(&self) -> &LvmVolume {
        &self.volume_info
    }

    /// Translates a volume offset into a device (its position in the list the volume was opened with), an offset
    /// on that device's partition, and the number of bytes that follow it contiguously.
    pub fn translate(&self, offset: u64) -> Result<(usize, u64, u64)> {
        let index = self.runs.partition_point(|run| run.logical <= offset).checked_sub(1);
        let run = index.and_then(|i| self.runs.get(i)).ok_or_else(|| outside(offset))?;
        let delta = offset.checked_sub(run.logical).ok_or_else(|| outside(offset))?;
        if delta >= run.len {
            return Err(outside(offset));
        }
        Ok((
            run.device,
            add(run.physical, delta, "LVM physical offset")?,
            run.len.saturating_sub(delta),
        ))
    }
}

/// Reads the LVM header area of one partition.
fn read_device(partition: &dyn BlockSource) -> Result<DeviceMetadata> {
    let raw = read_vec(partition, DEVICE_HEADER_OFFSET, DEVICE_HEADER_SIZE, "LVM device header")?;
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
    // Volumes of 3 TiB and of 17 TiB made by ESXi 7.0.3 have neither; no volume that has them has been seen.
    if ext_metadata_offset != 0 || num_pe_maps > 1 {
        return Err(Error::unsupported(format!(
            "LVM device uses extended extent metadata (next region at {ext_metadata_offset:#x}, {num_pe_maps} extent \
             maps); this layout has not been verified"
        )));
    }
    if num_pes == 0 || num_pes > MAX_PE_ENTRIES {
        return Err(Error::corrupt(format!(
            "LVM device header: {num_pes} physical extent entries"
        )));
    }

    // The volume table follows the device metadata, which occupies one alignment unit.
    let md_alignment_bytes = u64::from(md_alignment);
    let volume_offset = add(DEVICE_HEADER_OFFSET, md_alignment_bytes, "LVM volume table offset")?;
    let raw_volume = read_vec(
        partition,
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
    let last_pe = vol.u64(0x80)?;
    let device_count = vol.u32(0x90)?;
    if last_pe < first_pe {
        return Err(Error::corrupt(format!(
            "LVM volume descriptor: extents {first_pe} to {last_pe}"
        )));
    }
    if device_count == 0 || usize::try_from(device_count).unwrap_or(usize::MAX) > MAX_DEVICES {
        return Err(Error::corrupt(format!(
            "LVM volume descriptor: the volume spans {device_count} devices"
        )));
    }
    let logical_size = vol.u64(0x00)?;
    if logical_size == 0 {
        return Err(Error::corrupt("LVM volume descriptor: logical size 0"));
    }

    // Header area: device metadata, the volume table, a reserved area, the device name table, then pairs of
    // physical-extent bitmap and physical-extent table. For LVM6 the volume table has a single entry.
    let volume_area = mul(VOLUME_AREA_SECTORS, 512, "LVM volume area")?;
    let bitmap_offset = add(volume_offset, volume_area, "LVM extent bitmap offset")?;
    let bitmap_size = md_alignment_bytes.max(1024);
    let table_offset = add(bitmap_offset, bitmap_size, "LVM extent table offset")?;
    let table_len = mul(num_pes, PE_ENTRY_STRIDE, "LVM extent table size")?;
    let raw_table = read_vec(partition, table_offset, table_len, "LVM extent table")?;

    let mut extents = Vec::new();
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
        let (physical, logical, len) = (entry.u64(0x09)?, entry.u64(0x11)?, entry.u64(0x19)?);
        if len == 0 {
            return Err(Error::corrupt(format!(
                "LVM extent entry at {entry_offset:#x}: length 0"
            )));
        }
        extents.push((logical, physical, len));
    }

    // The device name table is filled on the first device only.
    let mut device_names = Vec::new();
    if first_pe == 0 {
        debug_assert!(DEVICE_NAME_TABLE_SIZE < volume_area);
        let names_offset = bitmap_offset.saturating_sub(DEVICE_NAME_TABLE_SIZE);
        let names_len = mul(u64::from(device_count), DEVICE_NAME_SIZE, "LVM device name table")?;
        let raw_names = read_vec(partition, names_offset, names_len, "LVM device name table")?;
        for raw_name in raw_names.chunks_exact(DEVICE_NAME_SIZE_BYTES) {
            let name = View::new("LVM device name", names_offset, raw_name).text(0, DEVICE_NAME_SIZE_BYTES)?;
            device_names.push(name);
        }
    }

    let scsi = View::new("LVM device SCSI id", DEVICE_HEADER_OFFSET, header.bytes(0x0C, 78)?);
    let pe_count = add(last_pe.saturating_sub(first_pe), 1, "LVM extent count")?;
    Ok(DeviceMetadata {
        info: LvmDevice {
            major_version,
            minor_version: header.u32(0x08)?,
            device_uuid: header.uuid(0x82)?,
            total_bytes: header.u64(0x5E)?,
            md_alignment,
            scsi_vendor: scsi.text(6, 8)?.trim().to_owned(),
            scsi_model: scsi.text(14, 16)?.trim().to_owned(),
            volume_start: mul(first_pe, PE_SIZE, "LVM device start")?,
            volume_len: mul(pe_count, PE_SIZE, "LVM device share")?,
        },
        volume: LvmVolume {
            uuid: vol.uuid(0x54)?,
            snap_id: vol.u32(0x64)?,
            name: vol.text(0x14, 64)?,
            logical_size,
            device_count,
            generation: vol.u64(0x08)?,
            device_names,
        },
        first_pe,
        last_pe,
        extents,
    })
}

fn outside(offset: u64) -> Error {
    Error::corrupt(format!("volume offset {offset:#x} is not backed by any LVM extent"))
}

/// Sorts the extents by logical offset, checks that they cover the volume without gaps or overlaps, and merges
/// extents that are adjacent on the same device as well.
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
            Some(last) if last.device == run.device && last.physical.checked_add(last.len) == Some(run.physical) => {
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
            let (device, physical, available) = self.translate(offset)?;
            let device = self.devices.get(device).ok_or_else(|| outside(offset))?;
            let take = usize::try_from(available).unwrap_or(usize::MAX).min(buf.len());
            let (head, tail) = buf.split_at_mut(take);
            device.read_exact_at(physical, head)?;
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
        assert_eq!(lv.translate(0).unwrap(), (0, 4 * MIB, MIB));
        assert_eq!(lv.translate(MIB + 5).unwrap(), (0, 6 * MIB + 5, MIB - 5));
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
        assert_eq!(lv.translate(0).unwrap(), (0, 4 * MIB, 2 * MIB));
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

        // One device of a volume that spans two.
        let mut spanned = base.clone();
        put(&mut spanned, h + 0x1000 + 0x90, &2u32.to_le_bytes());
        assert_eq!(open(spanned), ErrorKind::InvalidInput);

        let mut resignature = base.clone();
        put(&mut resignature, h + 0x1000 + 0x10, &2u32.to_le_bytes());
        assert_eq!(open(resignature), ErrorKind::Unsupported);

        let mut bad_magic = base;
        bad_magic[h] = 0;
        assert_eq!(open(bad_magic), ErrorKind::Corrupt);
    }

    const PE: u64 = 256 << 20;

    /// One device of a volume that spans two devices of 256 MiB each (one extent unit per device).
    fn span_device(position: u64, device_id: u8, volume_id: u8) -> Vec<u8> {
        let mut img = build_lvm(8 << 20, 2 * PE, &[(position * PE, 4 * MIB, 2 * MIB)]);
        let h = DEVICE_HEADER_OFFSET as usize;
        img[h + 0x82] = device_id;
        let v = h + 0x1000;
        img[v + 0x54] = volume_id;
        put(&mut img, v + 0x78, &position.to_le_bytes());
        put(&mut img, v + 0x80, &position.to_le_bytes());
        put(&mut img, v + 0x90, &2u32.to_le_bytes());
        if position == 0 {
            put(&mut img, h + 0x7F000, b"naa.first:1");
            put(&mut img, h + 0x7F000 + 256, b"naa.second:1");
        }
        img[4 << 20] = 0xA0 + device_id;
        img
    }

    fn open_all(images: Vec<Vec<u8>>) -> Result<LogicalVolume> {
        LogicalVolume::open_devices(
            images
                .into_iter()
                .map(|img| Arc::new(MemorySource::new(img)) as Arc<dyn BlockSource>)
                .collect(),
        )
    }

    /// Extents of the test devices are 2 MiB long, so the volume is cut down to what they cover.
    fn shrink(mut img: Vec<u8>, logical_size: u64) -> Vec<u8> {
        let v = DEVICE_HEADER_OFFSET as usize + 0x1000;
        put(&mut img, v, &logical_size.to_le_bytes());
        img
    }

    #[test]
    fn a_volume_is_assembled_from_its_devices_in_any_order() {
        // Device 1 continues the volume directly after device 0's extent.
        let first = shrink(span_device(0, 1, 7), 4 * MIB);
        let mut second = shrink(span_device(1, 2, 7), 4 * MIB);
        let e = TABLE;
        put(&mut second, e + 0x11, &(2 * MIB).to_le_bytes());
        for order in [[0usize, 1], [1, 0]] {
            let images = [first.clone(), second.clone()];
            let lv = open_all(order.iter().map(|&i| images[i].clone()).collect()).unwrap();
            assert_eq!(lv.len(), 4 * MIB);
            assert_eq!(lv.volume_info().device_count, 2);
            assert_eq!(lv.volume_info().device_names, ["naa.first:1", "naa.second:1"]);
            assert_eq!(lv.devices().len(), 2);
            assert_eq!(lv.devices()[1].volume_start, PE);
            // The device index is the position in the list that was passed in.
            let at_first = order.iter().position(|&i| i == 0).unwrap();
            let at_second = order.iter().position(|&i| i == 1).unwrap();
            assert_eq!(lv.translate(0).unwrap(), (at_first, 4 * MIB, 2 * MIB));
            assert_eq!(lv.translate(2 * MIB).unwrap(), (at_second, 4 * MIB, 2 * MIB));
            let mut buf = [0u8; 1];
            lv.read_exact_at(0, &mut buf).unwrap();
            assert_eq!(buf, [0xA1]);
            lv.read_exact_at(2 * MIB, &mut buf).unwrap();
            assert_eq!(buf, [0xA2]);
        }
    }

    #[test]
    fn a_read_may_cross_from_one_device_to_the_next() {
        let mut first = shrink(span_device(0, 1, 7), 4 * MIB);
        let mut second = shrink(span_device(1, 2, 7), 4 * MIB);
        put(&mut second, TABLE + 0x11, &(2 * MIB).to_le_bytes());
        first[(6 << 20) - 1] = 0x11;
        second[4 << 20] = 0x22;
        let lv = open_all(vec![first, second]).unwrap();
        let mut buf = [0u8; 2];
        lv.read_exact_at(2 * MIB - 1, &mut buf).unwrap();
        assert_eq!(buf, [0x11, 0x22]);
    }

    #[test]
    fn missing_duplicate_and_foreign_devices_are_refused() {
        let first = span_device(0, 1, 7);
        let second = span_device(1, 2, 7);

        let alone = open_all(vec![first.clone()]).unwrap_err();
        assert_eq!(alone.kind(), ErrorKind::InvalidInput);
        assert!(alone.to_string().contains("spans 2 devices"), "{alone}");
        assert!(alone.to_string().contains("naa.second:1"), "{alone}");

        let tail_only = open_all(vec![second.clone()]).unwrap_err();
        assert_eq!(tail_only.kind(), ErrorKind::InvalidInput);
        assert!(tail_only.to_string().contains("first one is missing"), "{tail_only}");

        assert_eq!(
            open_all(vec![first.clone(), first.clone()]).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
        assert_eq!(
            open_all(vec![first.clone(), span_device(1, 2, 9)]).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
        assert_eq!(open_all(vec![]).unwrap_err().kind(), ErrorKind::InvalidInput);

        // A volume that says it has two devices but whose second device does not continue the first.
        let mut gap = span_device(1, 2, 7);
        let v = DEVICE_HEADER_OFFSET as usize + 0x1000;
        put(&mut gap, v + 0x78, &2u64.to_le_bytes());
        put(&mut gap, v + 0x80, &2u64.to_le_bytes());
        assert_eq!(open_all(vec![first, gap]).unwrap_err().kind(), ErrorKind::Corrupt);
    }

    #[test]
    fn probe_detects_the_header() {
        let img = build_lvm(8 << 20, MIB, &[(0, 4 * MIB, MIB)]);
        assert!(LogicalVolume::probe(&MemorySource::new(img)).unwrap());
        assert!(!LogicalVolume::probe(&MemorySource::new(vec![0u8; 2 << 20])).unwrap());
        assert!(!LogicalVolume::probe(&MemorySource::new(vec![0u8; 16])).unwrap());
    }
}
