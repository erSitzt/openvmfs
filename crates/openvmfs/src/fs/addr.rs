//! Block addresses: the 64-bit values in address arrays and pointer blocks, and the 32-bit file descriptor
//! addresses in directories.
//!
//! Format description: `docs/format/30-addresses-resources.md`.

use crate::error::{Error, Result};

/// A decoded block address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Addr {
    /// Zero: nothing is allocated here. In a file this is a hole and reads as zeros.
    Null,
    /// Small file block (1 MiB), numbered by cluster and position inside the cluster.
    SmallFileBlock { cluster: u64, resource: u64, tbz: u8 },
    /// Large file block (512 MiB).
    LargeFileBlock { block: u64, tbz: u8 },
    /// Sub-block in the sub-block resource file.
    SubBlock { cluster: u64, resource: u64 },
    /// Pointer block in the pointer-block resource file.
    PointerBlock { cluster: u64, resource: u64 },
    /// Pointer block in the second pointer-block resource file.
    PointerBlock2 { cluster: u64, resource: u64 },
}

const TYPE_MASK: u64 = 0b111;
const TYPE_SFB: u64 = 1;
const TYPE_SB: u64 = 2;
const TYPE_PB: u64 = 3;
const TYPE_FD: u64 = 4;
/// The file descriptor type as it appears in 32-bit addresses.
const FD_TYPE: u32 = 4;
const TYPE_PB2: u64 = 5;
const TYPE_LFB: u64 = 7;

const COW_BIT: u64 = 1 << 5;

/// Bits of a small file block address with a known meaning: type, copy-on-write, to-be-zeroed bitmap (7..=14),
/// cluster (15..=45) and resource (51..=63).
const SFB_KNOWN: u64 = TYPE_MASK | COW_BIT | (0xFF << 7) | (0x7FFF_FFFF << 15) | (0x1FFF << 51);
/// Known bits of a large file block address: type, copy-on-write, to-be-zeroed bitmap and block (15..=45).
const LFB_KNOWN: u64 = TYPE_MASK | COW_BIT | (0xFF << 7) | (0x7FFF_FFFF << 15);
/// Known bits of sub-block and pointer-block addresses: type, copy-on-write, cluster (6..=41), resource (56..=63).
const CLUSTERED_KNOWN: u64 = TYPE_MASK | COW_BIT | (0xF_FFFF_FFFF << 6) | (0xFF << 56);

impl Addr {
    /// Decodes a 64-bit address as VMFS6 stores it.
    ///
    /// Addresses with bits whose meaning is unknown are refused: reading them anyway would be a guess.
    pub(crate) fn decode(raw: u64) -> Result<Addr> {
        if raw == 0 {
            return Ok(Addr::Null);
        }
        let kind = raw & TYPE_MASK;
        let known = match kind {
            TYPE_SFB => SFB_KNOWN,
            TYPE_LFB => LFB_KNOWN,
            TYPE_SB | TYPE_PB | TYPE_PB2 => CLUSTERED_KNOWN,
            _ => {
                return Err(Error::corrupt(format!(
                    "block address {raw:#018x} has unexpected type {kind}"
                )));
            }
        };
        if raw & !known != 0 {
            return Err(Error::unsupported(format!(
                "block address {raw:#018x} (type {kind}) uses bits {:#018x} whose meaning is unknown",
                raw & !known
            )));
        }
        if raw & COW_BIT != 0 {
            return Err(Error::unsupported(format!(
                "block address {raw:#018x} is marked copy-on-write, which has not been verified"
            )));
        }
        let tbz = ((raw >> 7) & 0xFF) as u8;
        let clustered = || ((raw >> 6) & 0xF_FFFF_FFFF, raw >> 56);
        Ok(match kind {
            TYPE_SFB => Addr::SmallFileBlock {
                cluster: (raw >> 15) & 0x7FFF_FFFF,
                resource: raw >> 51,
                tbz,
            },
            TYPE_LFB => Addr::LargeFileBlock {
                block: (raw >> 15) & 0x7FFF_FFFF,
                tbz,
            },
            TYPE_SB => {
                let (cluster, resource) = clustered();
                Addr::SubBlock { cluster, resource }
            }
            TYPE_PB => {
                let (cluster, resource) = clustered();
                Addr::PointerBlock { cluster, resource }
            }
            _ => {
                let (cluster, resource) = clustered();
                Addr::PointerBlock2 { cluster, resource }
            }
        })
    }
}

/// The address of a file descriptor: the "inode number" stored in directory entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FdAddr(u32);

impl FdAddr {
    /// The root directory.
    pub const ROOT: FdAddr = FdAddr(0x0000_0004);

    /// Checks and wraps a raw 32-bit file descriptor address.
    pub fn from_raw(raw: u32) -> Result<FdAddr> {
        if u64::from(raw) & TYPE_MASK != TYPE_FD {
            return Err(Error::corrupt(format!(
                "{raw:#010x} is not a file descriptor address (type {})",
                raw & 7
            )));
        }
        if raw & 0b11_1000 != 0 {
            return Err(Error::unsupported(format!(
                "file descriptor address {raw:#010x} uses bits whose meaning is unknown"
            )));
        }
        Ok(FdAddr(raw))
    }

    /// Builds the address of descriptor `resource` in cluster `cluster`.
    pub(crate) const fn system(resource: u32) -> FdAddr {
        FdAddr(((resource & 0x3FF) << 22) | FD_TYPE)
    }

    /// The raw 32-bit value.
    pub fn raw(self) -> u32 {
        self.0
    }

    /// Cluster number inside the file-descriptor resource file.
    pub fn cluster(self) -> u64 {
        u64::from((self.0 >> 6) & 0xFFFF)
    }

    /// Position inside the cluster.
    pub fn resource(self) -> u64 {
        u64::from(self.0 >> 22)
    }
}

impl std::fmt::Display for FdAddr {
    /// Prints the address the way `vmkfstools -D` does: `<4, cluster, resource>`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<4, {}, {}>", self.cluster(), self.resource())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    #[test]
    fn null_is_a_hole() {
        assert_eq!(Addr::decode(0).unwrap(), Addr::Null);
    }

    #[test]
    fn small_file_block() {
        // Values taken from a volume written by ESXi: block 8, and cluster 1 / resource 222.
        assert_eq!(
            Addr::decode(0x0040_0000_0000_0001).unwrap(),
            Addr::SmallFileBlock {
                cluster: 0,
                resource: 8,
                tbz: 0
            }
        );
        assert_eq!(
            Addr::decode(0x06F0_0000_0000_8001).unwrap(),
            Addr::SmallFileBlock {
                cluster: 1,
                resource: 222,
                tbz: 0
            }
        );
        let tbz = 0x0040_0000_0000_0001 | (0xFF << 7);
        assert_eq!(
            Addr::decode(tbz).unwrap(),
            Addr::SmallFileBlock {
                cluster: 0,
                resource: 8,
                tbz: 0xFF
            }
        );
    }

    #[test]
    fn large_file_block() {
        assert_eq!(
            Addr::decode((3 << 15) | 7).unwrap(),
            Addr::LargeFileBlock { block: 3, tbz: 0 }
        );
        assert_eq!(
            Addr::decode((3 << 15) | (1 << 7) | 7).unwrap(),
            Addr::LargeFileBlock { block: 3, tbz: 1 }
        );
    }

    #[test]
    fn clustered_addresses() {
        assert_eq!(
            Addr::decode(5).unwrap(),
            Addr::PointerBlock2 {
                cluster: 0,
                resource: 0
            }
        );
        let raw = (9u64 << 56) | (300 << 6) | 2;
        assert_eq!(
            Addr::decode(raw).unwrap(),
            Addr::SubBlock {
                cluster: 300,
                resource: 9
            }
        );
        assert_eq!(
            Addr::decode((raw & !7) | 3).unwrap(),
            Addr::PointerBlock {
                cluster: 300,
                resource: 9
            }
        );
    }

    #[test]
    fn unknown_bits_and_copy_on_write_are_refused() {
        // Bit 3 and bit 46 of a small file block address have no known meaning.
        assert_eq!(
            Addr::decode(0x0040_0000_0000_0009).unwrap_err().kind(),
            ErrorKind::Unsupported
        );
        assert_eq!(Addr::decode((1 << 46) | 1).unwrap_err().kind(), ErrorKind::Unsupported);
        // Bits above the block number of a large file block.
        assert_eq!(Addr::decode((1 << 50) | 7).unwrap_err().kind(), ErrorKind::Unsupported);
        // Bits between cluster and resource of a sub-block.
        assert_eq!(Addr::decode((1 << 44) | 2).unwrap_err().kind(), ErrorKind::Unsupported);
        assert_eq!(Addr::decode(COW_BIT | 1).unwrap_err().kind(), ErrorKind::Unsupported);
        // A file descriptor or journal address is never a data block.
        assert_eq!(Addr::decode(4).unwrap_err().kind(), ErrorKind::Corrupt);
        assert_eq!(Addr::decode(6).unwrap_err().kind(), ErrorKind::Corrupt);
    }

    #[test]
    fn file_descriptor_addresses() {
        assert_eq!(FdAddr::ROOT.raw(), 4);
        let fdc = FdAddr::system(2);
        assert_eq!(fdc.raw(), 0x0080_0004);
        assert_eq!((fdc.cluster(), fdc.resource()), (0, 2));
        let addr = FdAddr::from_raw((100 << 22) | (136 << 6) | 4).unwrap();
        assert_eq!(addr.to_string(), "<4, 136, 100>");
        assert_eq!(FdAddr::from_raw(1).unwrap_err().kind(), ErrorKind::Corrupt);
        assert_eq!(FdAddr::from_raw(0x0C).unwrap_err().kind(), ErrorKind::Unsupported);
    }
}
