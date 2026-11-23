//! File descriptors: the inodes of VMFS.
//!
//! Format description: `docs/format/40-file-descriptor.md`.

use crate::bytes::{to_usize, View};
use crate::error::{Error, Result};
use crate::fs::addr::FdAddr;
use crate::fs::descriptor::Geometry;

/// Lock type that marks a file descriptor.
const LOCK_TYPE_FILE_DESCRIPTOR: u32 = 0x10C0_0001;
/// "fdmd": signature of the file metadata block.
const FILE_METADATA_SIGNATURE: u32 = 0x6664_6D64;

const S_IFMT: u32 = 0o170_000;
const S_IFDIR: u32 = 0o040_000;

/// What a file descriptor describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FileKind {
    /// A directory.
    Directory,
    /// A regular file.
    Regular,
    /// A symbolic link; its content is the target.
    Symlink,
    /// A system file such as `.fdc.sf`. `.sdd.sf` is a system file that is also a directory.
    System,
    /// A raw device mapping: the data lives on another LUN and cannot be read from this volume.
    RawDeviceMap,
}

/// How a file descriptor's address array is to be read ("zero level address type").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AddressingMode {
    /// The array holds file block addresses.
    FileBlocks,
    /// The array holds sub-block addresses.
    SubBlocks,
    /// The array holds pointer blocks, which hold the block addresses.
    Indirect,
    /// Like [`AddressingMode::Indirect`], recorded with the second pointer-block type.
    Indirect2,
    /// The array holds pointer blocks that point to pointer blocks.
    DoubleIndirect,
    /// The data is stored inside the file descriptor.
    Resident,
    /// The descriptor addresses no blocks at all, as for raw device mappings.
    None,
}

impl AddressingMode {
    fn from_raw(raw: u32) -> Result<Self> {
        Ok(match raw {
            0 => AddressingMode::None,
            1 => AddressingMode::FileBlocks,
            2 => AddressingMode::SubBlocks,
            3 => AddressingMode::Indirect,
            5 => AddressingMode::Indirect2,
            0x10D0 => AddressingMode::DoubleIndirect,
            0x10D1 => AddressingMode::Resident,
            other => {
                return Err(Error::unsupported(format!(
                    "file descriptor uses unknown addressing mode {other:#x}"
                )));
            }
        })
    }

    /// Number of pointer-block levels between the descriptor and the data.
    pub(crate) fn levels(self) -> u32 {
        match self {
            AddressingMode::FileBlocks
            | AddressingMode::SubBlocks
            | AddressingMode::Resident
            | AddressingMode::None => 0,
            AddressingMode::Indirect | AddressingMode::Indirect2 => 1,
            AddressingMode::DoubleIndirect => 2,
        }
    }
}

/// How ESXi hosts had a file locked, as recorded on disk.
///
/// In an image of a datastore that was in use, this tells whether a host had the file open, and how. A file that
/// was open for writing may have newer content in the host's memory and in the VMFS journal than in the place a
/// reader looks; see `docs/consistency.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    /// Nobody held the file.
    Free,
    /// One host had it open for writing: a running VM's current disk, for example.
    Exclusive,
    /// Open for reading only, possibly by several hosts: a disk below a VM snapshot, for example.
    ReadOnly,
    /// Open for writing by several hosts (shared disks of cluster software).
    MultiWriter,
    /// A value this library does not know.
    Other(u32),
}

impl LockMode {
    fn from_raw(raw: u32) -> Self {
        match raw {
            0 => LockMode::Free,
            1 => LockMode::Exclusive,
            2 => LockMode::ReadOnly,
            3 => LockMode::MultiWriter,
            other => LockMode::Other(other),
        }
    }

    /// Whether a host had the file open for writing.
    pub fn is_open_for_writing(self) -> bool {
        matches!(self, LockMode::Exclusive | LockMode::MultiWriter | LockMode::Other(_))
    }
}

/// A parsed file descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Inode {
    pub addr: FdAddr,
    /// Volume offset of the descriptor itself.
    pub volume_offset: u64,
    pub kind: FileKind,
    pub generation: u32,
    pub link_count: u32,
    pub length: u64,
    pub block_size: u64,
    pub block_count: u64,
    pub mtime: u32,
    pub ctime: u32,
    pub atime: u32,
    pub uid: u32,
    pub gid: u32,
    pub mode: u32,
    pub addressing: AddressingMode,
    pub unwritten_blocks: u64,
    pub block_shift: u8,
    /// How the file was locked, and by which host. A multi-writer lock has its owners in a list; this is the first.
    pub lock_mode: LockMode,
    pub lock_owner: crate::Uuid,
    /// The address array: block addresses or pointer-block addresses, depending on `addressing`.
    pub addrs: Vec<u64>,
}

impl Inode {
    /// Parses the descriptor `raw`, read from `volume_offset`, which must be the descriptor with address `addr`.
    pub(crate) fn parse(raw: &[u8], addr: FdAddr, volume_offset: u64, geo: &Geometry) -> Result<Inode> {
        let what = |problem: String| format!("file descriptor {addr} at volume offset {volume_offset:#x}: {problem}");
        if raw.len() as u64 != geo.descriptor_size {
            return Err(Error::corrupt(what(format!(
                "{} bytes read, expected {}",
                raw.len(),
                geo.descriptor_size
            ))));
        }
        let lock = View::new("file descriptor lock block", volume_offset, raw);
        let lock_type = lock.u32(0x00)?;
        if lock_type != LOCK_TYPE_FILE_DESCRIPTOR {
            return Err(Error::corrupt(what(format!(
                "lock type {lock_type:#010x}, expected {LOCK_TYPE_FILE_DESCRIPTOR:#010x} (not a file descriptor, or unused)"
            ))));
        }
        let own_offset = lock.u64(0x04)?;
        if own_offset != volume_offset {
            return Err(Error::corrupt(what(format!(
                "it records its own position as {own_offset:#x}"
            ))));
        }

        let meta_offset = to_usize(geo.metadata_offset, "file metadata offset")?;
        let meta_raw = raw.get(meta_offset..).unwrap_or_default();
        let meta = View::new(
            "file metadata",
            volume_offset.saturating_add(geo.metadata_offset),
            meta_raw,
        );
        let recorded_addr = meta.u32(0x00)?;
        if recorded_addr != addr.raw() {
            return Err(Error::corrupt(what(format!(
                "it records its own address as {recorded_addr:#010x}"
            ))));
        }
        let signature = meta.u32(0x64)?;
        if signature != FILE_METADATA_SIGNATURE {
            return Err(Error::unsupported(what(format!(
                "metadata signature {signature:#010x}, expected {FILE_METADATA_SIGNATURE:#010x}"
            ))));
        }
        let kind = match meta.u32(0x0C)? {
            2 => FileKind::Directory,
            3 => FileKind::Regular,
            4 => FileKind::Symlink,
            5 => FileKind::System,
            6 => FileKind::RawDeviceMap,
            other => return Err(Error::corrupt(what(format!("descriptor type {other}")))),
        };
        let copy_on_write_blocks = u64::from(meta.u32(0x4C)?) | (u64::from(meta.u32(0x58)?) << 32);
        if copy_on_write_blocks != 0 {
            return Err(Error::unsupported(what(format!(
                "{copy_on_write_blocks} copy-on-write blocks; copy-on-write files have not been verified"
            ))));
        }

        let addrs_offset = to_usize(geo.address_array_offset, "address array offset")?;
        let addr_count = to_usize(geo.fd_addr_count, "address array length")?;
        let addrs_raw = View::new("file descriptor address array", volume_offset, raw)
            .bytes(addrs_offset, addr_count.saturating_mul(8))?;
        let addrs = addrs_raw
            .chunks_exact(8)
            .map(|chunk| u64::from_le_bytes(chunk.try_into().unwrap_or([0; 8])))
            .collect();

        let lock_mode = LockMode::from_raw(lock.u32(0x24)?);
        // A multi-writer lock leaves the owner field empty and lists its owners further down: their number at
        // 0x40, the first one's UUID at 0x54.
        let mut lock_owner = lock.uuid(0x28)?;
        if lock_mode == LockMode::MultiWriter && lock_owner == crate::Uuid::default() && lock.u32(0x40)? > 0 {
            lock_owner = lock.uuid(0x54)?;
        }
        Ok(Inode {
            addr,
            volume_offset,
            kind,
            generation: meta.u32(0x04)?,
            link_count: meta.u32(0x08)?,
            length: meta.u64(0x14)?,
            block_size: meta.u64(0x1C)?,
            block_count: meta.u64(0x24)?,
            mtime: meta.u32(0x2C)?,
            ctime: meta.u32(0x30)?,
            atime: meta.u32(0x34)?,
            uid: meta.u32(0x38)?,
            gid: meta.u32(0x3C)?,
            mode: meta.u32(0x40)?,
            addressing: AddressingMode::from_raw(meta.u32(0x44)?)
                .map_err(|e| e.context(format!("file descriptor {addr} at volume offset {volume_offset:#x}")))?,
            unwritten_blocks: u64::from(meta.u32(0x48)?) | (u64::from(meta.u32(0x54)?) << 32),
            block_shift: meta.u8(0x89)?,
            lock_mode,
            lock_owner,
            addrs,
        })
    }

    /// Whether this descriptor is a directory. System files carry the directory bit in their mode.
    pub(crate) fn is_dir(&self) -> bool {
        match self.kind {
            FileKind::Directory => true,
            FileKind::System => self.mode & S_IFMT == S_IFDIR,
            _ => false,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::fs::descriptor::tests::write_descriptor;
    use crate::fs::descriptor::FsDescriptor;
    use crate::io::MemorySource;

    pub(crate) fn geometry() -> Geometry {
        geometry_with(false)
    }

    /// Geometry of a 4 GiB test volume; `accept_unverified` switches the refusal of unverified structures off.
    pub(crate) fn geometry_with(accept_unverified: bool) -> Geometry {
        let mut volume = vec![0u8; 3 << 20];
        write_descriptor(&mut volume);
        let d = FsDescriptor::read(&MemorySource::new(volume)).unwrap();
        Geometry::new(&d, 4 << 30, accept_unverified).unwrap()
    }

    /// Builds a file descriptor with the given properties.
    pub(crate) fn build_fd(
        addr: FdAddr,
        at: u64,
        kind: u32,
        zla: u32,
        len: u64,
        block: (u64, u8),
        addrs: &[u64],
    ) -> Vec<u8> {
        let mut raw = vec![0u8; 8192];
        raw[0..4].copy_from_slice(&LOCK_TYPE_FILE_DESCRIPTOR.to_le_bytes());
        raw[4..12].copy_from_slice(&at.to_le_bytes());
        let m = 4096;
        raw[m..m + 4].copy_from_slice(&addr.raw().to_le_bytes());
        raw[m + 0x0C..m + 0x10].copy_from_slice(&kind.to_le_bytes());
        raw[m + 0x14..m + 0x1C].copy_from_slice(&len.to_le_bytes());
        raw[m + 0x1C..m + 0x24].copy_from_slice(&block.0.to_le_bytes());
        raw[m + 0x44..m + 0x48].copy_from_slice(&zla.to_le_bytes());
        raw[m + 0x64..m + 0x68].copy_from_slice(&FILE_METADATA_SIGNATURE.to_le_bytes());
        raw[m + 0x89] = block.1;
        for (i, a) in addrs.iter().enumerate() {
            raw[0x1600 + i * 8..0x1600 + i * 8 + 8].copy_from_slice(&a.to_le_bytes());
        }
        raw
    }

    #[test]
    fn parses_fields_and_addresses() {
        let geo = geometry();
        let addr = FdAddr::system(3);
        let raw = build_fd(addr, 0x72_6000, 3, 1, 3 << 20, (1 << 20, 20), &[0x11, 0x21, 0x31]);
        let inode = Inode::parse(&raw, addr, 0x72_6000, &geo).unwrap();
        assert_eq!(inode.kind, FileKind::Regular);
        assert_eq!(inode.addressing, AddressingMode::FileBlocks);
        assert_eq!(inode.length, 3 << 20);
        assert_eq!(inode.block_shift, 20);
        assert_eq!(inode.addrs.len(), 320);
        assert_eq!(&inode.addrs[..4], &[0x11, 0x21, 0x31, 0]);
        assert!(!inode.is_dir());
    }

    #[test]
    fn a_descriptor_must_identify_itself() {
        let geo = geometry();
        let addr = FdAddr::system(3);
        let good = build_fd(addr, 0x72_6000, 3, 1, 0, (1 << 20, 20), &[]);
        let kind = |raw: &[u8], a: FdAddr, at: u64| Inode::parse(raw, a, at, &geo).unwrap_err().kind();

        assert_eq!(kind(&good, addr, 0x72_8000), ErrorKind::Corrupt); // read from the wrong place
        assert_eq!(kind(&good, FdAddr::system(4), 0x72_6000), ErrorKind::Corrupt); // wrong descriptor
        assert_eq!(kind(&vec![0u8; 8192], addr, 0x72_6000), ErrorKind::Corrupt); // unused slot
        assert_eq!(kind(&good[..4096], addr, 0x72_6000), ErrorKind::Corrupt); // short read

        let mut no_signature = good.clone();
        no_signature[4096 + 0x64] = 0;
        assert_eq!(kind(&no_signature, addr, 0x72_6000), ErrorKind::Unsupported);
        let mut cow = good.clone();
        cow[4096 + 0x4C] = 1;
        assert_eq!(kind(&cow, addr, 0x72_6000), ErrorKind::Unsupported);
        let mut odd_zla = good.clone();
        odd_zla[4096 + 0x44] = 9;
        assert_eq!(kind(&odd_zla, addr, 0x72_6000), ErrorKind::Unsupported);
        let mut odd_type = good;
        odd_type[4096 + 0x0C] = 1;
        assert_eq!(kind(&odd_type, addr, 0x72_6000), ErrorKind::Corrupt);
    }

    #[test]
    fn a_multi_writer_lock_names_its_first_owner() {
        let geo = geometry();
        let addr = FdAddr::system(3);
        let owner = [
            0x51, 0xfb, 0xfd, 0x6a, 0x18, 0xb1, 0x95, 0xa9, 0x8a, 0x09, 0x00, 0x50, 0x56, 0x8b, 0x55, 0x83,
        ];
        // As ESXi 7.0.3 and 8.0 U3 write it: mode 3, the owner field empty, one owner in the list.
        let mut raw = build_fd(addr, 0x72_6000, 3, 1, 0, (1 << 20, 20), &[]);
        raw[0x24] = 3;
        raw[0x40] = 1;
        raw[0x54..0x64].copy_from_slice(&owner);
        let inode = Inode::parse(&raw, addr, 0x72_6000, &geo).unwrap();
        assert_eq!(inode.lock_mode, LockMode::MultiWriter);
        assert_eq!(inode.lock_owner, crate::Uuid::from_bytes(owner));
        assert_eq!(inode.lock_owner.to_string(), "6afdfb51-a995b118-098a-0050568b5583");

        // An exclusive lock keeps its owner where it always was, whatever follows.
        raw[0x24] = 1;
        raw[0x28] = 7;
        let inode = Inode::parse(&raw, addr, 0x72_6000, &geo).unwrap();
        assert_eq!(inode.lock_owner.to_string(), "00000007-00000000-0000-000000000000");
    }

    #[test]
    fn system_files_can_be_directories() {
        let geo = geometry();
        let addr = FdAddr::system(7);
        let mut raw = build_fd(addr, 0x72_E000, 5, 1, 0, (1 << 20, 20), &[]);
        raw[4096 + 0x40..4096 + 0x44].copy_from_slice(&0o040_700u32.to_le_bytes());
        assert!(Inode::parse(&raw, addr, 0x72_E000, &geo).unwrap().is_dir());
    }
}
