//! The filesystem descriptor ("FS info", the superblock) and the geometry derived from it.
//!
//! Format description: `docs/format/20-fs-descriptor.md`.

use crate::bytes::{add, log2_exact, mul, View};
use crate::error::{Error, Result};
use crate::io::{read_vec, BlockSource};
use crate::Uuid;

/// Volume offset of the filesystem descriptor.
pub const FS_DESCRIPTOR_OFFSET: u64 = 0x20_0000;
/// Magic number of a VMFS filesystem descriptor.
pub const VMFS_MAGIC: u32 = 0x2FAB_F15E;
/// Magic number of a VMFS-L (local, ESXi system) filesystem descriptor.
pub const VMFSL_MAGIC: u32 = 0x2FAB_F15F;

const FS_DESCRIPTOR_SIZE: u64 = 0x170;
const VMFS6_MAJOR: u32 = 24;
/// Minor versions whose volumes have been read and compared against a reference.
const VERIFIED_MINOR_VERSIONS: [u8; 1] = [82];

/// The filesystem descriptor as stored on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsDescriptor {
    /// Major version: 24 for VMFS6.
    pub major_version: u32,
    /// Minor version: 81 or 82 for VMFS6 (shown by ESXi as 6.81 and 6.82).
    pub minor_version: u8,
    /// UUID of the datastore.
    pub uuid: Uuid,
    /// Configuration flags.
    pub config: u32,
    /// Datastore label.
    pub label: String,
    /// Logical block size of the device when the filesystem was created.
    pub disk_block_size: u32,
    /// Size of a (small) file block.
    pub file_block_size: u64,
    /// Creation time, seconds since 1970.
    pub creation_time: u32,
    /// UUID of the LVM volume this filesystem belongs to.
    pub lvm_volume_uuid: Uuid,
    /// Offset of the first cluster group inside the file-descriptor resource file.
    pub fdc_cluster_group_offset: u32,
    /// Clusters per cluster group of the file-descriptor resource file.
    pub fdc_clusters_per_group: u32,
    /// Size of a sub-block.
    pub sub_block_size: u32,
    /// Physical block size of the device when the filesystem was created.
    pub phys_disk_block_size: u32,
    /// Alignment unit of on-disk metadata.
    pub md_alignment: u32,
    /// A large file block is `file_block_size << sfb_to_lfb_shift` bytes.
    pub sfb_to_lfb_shift: u16,
    /// Granularity of "to be zeroed" tracking.
    pub tbz_granularity: u32,
}

impl FsDescriptor {
    /// Reads and checks the descriptor of the filesystem on `volume`.
    pub fn read(volume: &dyn BlockSource) -> Result<Self> {
        let raw = read_vec(
            volume,
            FS_DESCRIPTOR_OFFSET,
            FS_DESCRIPTOR_SIZE,
            "filesystem descriptor",
        )?;
        let d = View::new("filesystem descriptor", FS_DESCRIPTOR_OFFSET, &raw);

        let magic = d.u32(0x00)?;
        if magic == VMFSL_MAGIC {
            return Err(Error::unsupported(
                "VMFS-L volume (ESXi system volume); only VMFS6 datastores are supported",
            ));
        }
        if magic != VMFS_MAGIC {
            return Err(Error::corrupt(format!(
                "filesystem descriptor at volume offset {FS_DESCRIPTOR_OFFSET:#x}: magic {magic:#010x}, \
                 expected {VMFS_MAGIC:#010x}"
            )));
        }
        let descriptor = FsDescriptor {
            major_version: d.u32(0x04)?,
            minor_version: d.u8(0x08)?,
            uuid: d.uuid(0x09)?,
            config: d.u32(0x19)?,
            label: d.text(0x1D, 128)?,
            disk_block_size: d.u32(0x9D)?,
            file_block_size: d.u64(0xA1)?,
            creation_time: d.u32(0xA9)?,
            lvm_volume_uuid: d.uuid(0xB1)?,
            fdc_cluster_group_offset: d.u32(0xD1)?,
            fdc_clusters_per_group: d.u32(0xD5)?,
            sub_block_size: d.u32(0xD9)?,
            phys_disk_block_size: d.u32(0x130)?,
            md_alignment: d.u32(0x134)?,
            sfb_to_lfb_shift: d.u16(0x138)?,
            tbz_granularity: d.u32(0x144)?,
        };
        descriptor.check_supported()?;
        Ok(descriptor)
    }

    /// Version as ESXi prints it, for example `6.82`.
    pub fn version_string(&self) -> String {
        let major = if self.major_version == VMFS6_MAJOR {
            6
        } else {
            self.major_version
        };
        format!("{major}.{}", self.minor_version)
    }

    /// Refuses every layout that has not been verified against a volume written by ESXi.
    fn check_supported(&self) -> Result<()> {
        if self.major_version != VMFS6_MAJOR {
            return Err(Error::unsupported(format!(
                "filesystem major version {} (only {VMFS6_MAJOR}, VMFS6, is supported; VMFS5 is 14)",
                self.major_version
            )));
        }
        if !VERIFIED_MINOR_VERSIONS.contains(&self.minor_version) {
            return Err(Error::unsupported(format!(
                "VMFS {} has not been verified yet (verified: 6.82)",
                self.version_string()
            )));
        }
        let expect = |name: &str, value: u64, expected: u64| -> Result<()> {
            if value == expected {
                Ok(())
            } else {
                Err(Error::unsupported(format!(
                    "filesystem descriptor: {name} is {value}, only {expected} has been verified"
                )))
            }
        };
        expect("file block size", self.file_block_size, 1 << 20)?;
        expect("sub-block size", u64::from(self.sub_block_size), 64 << 10)?;
        expect("metadata alignment", u64::from(self.md_alignment), 4096)?;
        expect("small-to-large file block shift", u64::from(self.sfb_to_lfb_shift), 9)?;
        expect("disk block size", u64::from(self.disk_block_size), 512)?;
        expect("physical disk block size", u64::from(self.phys_disk_block_size), 512)?;
        expect("to-be-zeroed granularity", u64::from(self.tbz_granularity), 1 << 20)?;
        if self.fdc_clusters_per_group == 0 {
            return Err(Error::corrupt(
                "filesystem descriptor: 0 file-descriptor clusters per group",
            ));
        }
        Ok(())
    }
}

/// Sizes and offsets that follow from the descriptor. All values are validated once, here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Geometry {
    /// Size of the logical volume.
    pub volume_size: u64,
    /// Alignment unit of metadata; also the size of a lock block.
    pub md_alignment: u64,
    /// Size of a file descriptor: a lock block followed by a metadata block.
    pub fd_size: u64,
    /// Offset of the file metadata inside a file descriptor.
    pub fd_meta_offset: u64,
    /// Offset of resident data inside a file descriptor.
    pub fd_data_offset: u64,
    /// Bytes available for resident data.
    pub fd_data_size: u64,
    /// Offset of the block address array inside a file descriptor.
    pub fd_addrs_offset: u64,
    /// Number of 64-bit entries in the block address array.
    pub fd_addr_count: u64,
    /// Size of a small file block.
    pub file_block_size: u64,
    /// log2 of the small file block size.
    pub file_block_shift: u32,
    /// Size of a sub-block.
    pub sub_block_size: u64,
    /// log2 of the large file block size.
    pub lfb_shift: u32,
    /// Small file blocks per large file block; also the number of small file blocks per cluster.
    pub sfb_per_cluster: u64,
    /// Entries in a pointer block.
    pub ptrs_per_block: u64,
    /// Size of a pointer block in bytes.
    pub ptr_block_size: u64,
    /// Volume offset of the first system file descriptor (the root directory's).
    pub sfd_base: u64,
    /// Read structures whose interpretation has not been verified against ESXi instead of refusing them.
    pub accept_unverified: bool,
}

impl Geometry {
    pub(crate) fn new(d: &FsDescriptor, volume_size: u64, accept_unverified: bool) -> Result<Self> {
        let md_alignment = u64::from(d.md_alignment);
        let file_block_shift = log2_exact(d.file_block_size)
            .ok_or_else(|| Error::corrupt("filesystem descriptor: file block size is not a power of two"))?;
        let lfb_shift = file_block_shift
            .checked_add(u32::from(d.sfb_to_lfb_shift))
            .filter(|shift| *shift < 48)
            .ok_or_else(|| Error::corrupt("filesystem descriptor: large file block size out of range"))?;
        let fd_size = mul(md_alignment, 2, "file descriptor size")?;
        // The metadata block starts with 512 bytes of file metadata. Resident data uses the rest of that block; the
        // address array is aligned to the end of the descriptor instead.
        let fd_data_size = md_alignment
            .checked_sub(512)
            .ok_or_else(|| Error::corrupt("filesystem descriptor: metadata alignment below 512"))?;
        let fd_addrs_size = 2560u64;
        let fd_data_offset = fd_size
            .checked_sub(fd_data_size)
            .ok_or_else(|| Error::corrupt("file descriptor layout: data offset"))?;
        let fd_addrs_offset = fd_size
            .checked_sub(fd_addrs_size)
            .ok_or_else(|| Error::corrupt("file descriptor layout: address array offset"))?;

        // System file descriptors are found without the resource files: the file-descriptor cluster group starts
        // at a fixed position, its cluster headers come first, then the descriptors themselves.
        let fdc_base = add(
            mul(md_alignment, 1024, "descriptor area")?,
            0x3F_FFFF,
            "descriptor area",
        )? & !0xF_FFFF;
        let cluster_group = add(
            fdc_base,
            u64::from(d.fdc_cluster_group_offset),
            "descriptor cluster group",
        )?;
        let headers = mul(
            u64::from(d.fdc_clusters_per_group),
            fd_size,
            "descriptor cluster headers",
        )?;
        let sfd_base = add(cluster_group, headers, "system file descriptor base")?;

        let ptr_block_size = 0x1_0000u64;
        Ok(Geometry {
            volume_size,
            md_alignment,
            fd_size,
            fd_meta_offset: md_alignment,
            fd_data_offset,
            fd_data_size,
            fd_addrs_offset,
            fd_addr_count: fd_addrs_size / 8,
            file_block_size: d.file_block_size,
            file_block_shift,
            sub_block_size: u64::from(d.sub_block_size),
            lfb_shift,
            sfb_per_cluster: 1u64 << d.sfb_to_lfb_shift,
            ptrs_per_block: ptr_block_size / 8,
            ptr_block_size,
            sfd_base,
            accept_unverified,
        })
    }

    /// Volume offset of the descriptor of system file number `resource` (0 is the root directory).
    pub(crate) fn sfd_offset(&self, resource: u64) -> Result<u64> {
        add(
            self.sfd_base,
            mul(resource, self.fd_size, "system file descriptor")?,
            "system file descriptor",
        )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::io::MemorySource;

    /// Writes a VMFS 6.82 descriptor with the verified geometry into `volume`.
    pub(crate) fn write_descriptor(volume: &mut [u8]) {
        let mut put = |offset: usize, bytes: &[u8]| {
            let at = FS_DESCRIPTOR_OFFSET as usize + offset;
            volume[at..at + bytes.len()].copy_from_slice(bytes);
        };
        put(0x00, &VMFS_MAGIC.to_le_bytes());
        put(0x04, &24u32.to_le_bytes());
        put(0x08, &[82]);
        put(0x1D, b"datastore1");
        put(0x9D, &512u32.to_le_bytes());
        put(0xA1, &(1u64 << 20).to_le_bytes());
        put(0xD1, &0x1_0000u32.to_le_bytes());
        put(0xD5, &8u32.to_le_bytes());
        put(0xD9, &65536u32.to_le_bytes());
        put(0x130, &512u32.to_le_bytes());
        put(0x134, &4096u32.to_le_bytes());
        put(0x138, &9u16.to_le_bytes());
        put(0x144, &(1u32 << 20).to_le_bytes());
    }

    fn volume() -> Vec<u8> {
        let mut v = vec![0u8; 3 << 20];
        write_descriptor(&mut v);
        v
    }

    #[test]
    fn parses_the_verified_layout() {
        let d = FsDescriptor::read(&MemorySource::new(volume())).unwrap();
        assert_eq!(d.version_string(), "6.82");
        assert_eq!(d.label, "datastore1");
        let g = Geometry::new(&d, 2 << 30, false).unwrap();
        assert_eq!(g.fd_size, 8192);
        assert_eq!(g.fd_data_offset, 0x1200);
        assert_eq!(g.fd_data_size, 3584);
        assert_eq!(g.fd_addrs_offset, 0x1600);
        assert_eq!(g.fd_addr_count, 320);
        assert_eq!(g.lfb_shift, 29);
        assert_eq!(g.sfb_per_cluster, 512);
        assert_eq!(g.ptrs_per_block, 8192);
        // 7 MiB, plus the cluster group offset, plus 8 cluster headers of 8 KiB each.
        assert_eq!(g.sfd_base, 0x72_0000);
        assert_eq!(g.sfd_offset(2).unwrap(), 0x72_4000);
    }

    #[test]
    fn refuses_what_has_not_been_verified() {
        let read = |patch: &dyn Fn(&mut [u8])| {
            let mut v = volume();
            patch(&mut v[FS_DESCRIPTOR_OFFSET as usize..]);
            FsDescriptor::read(&MemorySource::new(v)).unwrap_err().kind()
        };
        assert_eq!(read(&|d| d[0] = 0), ErrorKind::Corrupt);
        assert_eq!(read(&|d| d[0] = 0x5F), ErrorKind::Unsupported); // VMFS-L
        assert_eq!(read(&|d| d[4] = 14), ErrorKind::Unsupported); // VMFS5
        assert_eq!(read(&|d| d[8] = 81), ErrorKind::Unsupported); // 6.81: no reference volume yet
        assert_eq!(read(&|d| d[0xA3] = 0x20), ErrorKind::Unsupported); // 2 MiB blocks
        assert_eq!(read(&|d| d[0x135] = 0x20), ErrorKind::Unsupported); // 8 KiB alignment
        assert_eq!(read(&|d| d[0x138] = 8), ErrorKind::Unsupported);
        assert_eq!(read(&|d| d[0x131] = 0x10), ErrorKind::Unsupported); // 4Kn
    }
}
