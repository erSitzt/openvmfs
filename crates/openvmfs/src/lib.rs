//! Read-only access to VMware VMFS6 volumes and the virtual disks (VMDK) stored on them.
//!
//! OpenVmfs reads a VMFS6 datastore straight from a block device or an image file, without ESXi. It is meant for
//! backup and recovery software that reads virtual disks from storage snapshots.
//!
//! ```no_run
//! use openvmfs::{BlockSource, Volume};
//!
//! # fn main() -> openvmfs::Result<()> {
//! let volume = Volume::open_path("/dev/mapper/snapshot-lun")?;
//! let disk = volume.open_disk("vm1/vm1.vmdk")?;
//! let mut first_sector = [0u8; 512];
//! disk.read_exact_at(0, &mut first_sector)?;
//! # Ok(())
//! # }
//! ```
//!
//! # Refusing instead of guessing
//!
//! VMFS has no public specification. This library accepts only layouts that have been verified against volumes
//! written by ESXi and returns [`ErrorKind::Unsupported`] for everything else, so that a caller can fall back to
//! another way of reading the data. It never writes to the device.
//!
//! The layers, bottom to top: [`io`] (devices and images), [`gpt`] (partition table), [`lvm`] (logical volume),
//! [`fs`] (the filesystem) and [`vmdk`] (virtual disks).

// Tests build images byte by byte and assert on results; the panic-freedom rules below are for library code.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation
    )
)]

mod bytes;
mod error;
pub mod fs;
pub mod gpt;
pub mod io;
pub mod lvm;
pub mod vmdk;

use std::fmt;

pub use error::{Error, ErrorKind, Result};
pub use fs::{DirEntry, Extent, ExtentKind, File, FileKind, FileStat, LockMode, OpenOptions, Volume, VolumeInfo};
pub use io::{BlockSource, FileSource, MemorySource, SubSource};
pub use vmdk::{ChainLink, Disk};

/// A VMFS UUID: creation time, a random part and the MAC address of the host that created the object.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Uuid([u8; 16]);

impl Uuid {
    /// Wraps the 16 on-disk bytes.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Uuid(bytes)
    }

    /// The 16 on-disk bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for Uuid {
    /// Formats the UUID the way ESXi prints it, for example `68626f2d-90e23158-8a2e-bc2411b17e28`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.0;
        let time_lo = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let time_hi = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
        let rand = u16::from_le_bytes([b[8], b[9]]);
        write!(f, "{time_lo:08x}-{time_hi:08x}-{rand:04x}-")?;
        for byte in &b[10..16] {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Uuid({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_prints_like_esxi() {
        let raw = [
            0x2d, 0x6f, 0x62, 0x68, 0x58, 0x31, 0xe2, 0x90, 0x2e, 0x8a, 0xbc, 0x24, 0x11, 0xb1, 0x7e, 0x28,
        ];
        assert_eq!(Uuid::from_bytes(raw).to_string(), "68626f2d-90e23158-8a2e-bc2411b17e28");
    }
}
