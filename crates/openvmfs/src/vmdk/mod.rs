//! Virtual disks (VMDK) stored on a VMFS volume.
//!
//! A virtual disk on a datastore is a small text descriptor (`disk.vmdk`) that names one extent file. For a base
//! disk that is a flat file (`disk-flat.vmdk`) with the disk's content at the same offsets; whether the disk is
//! thin or thick shows only in that file's block map.

mod descriptor;

use std::ops::Range;
use std::sync::Arc;

pub use descriptor::{Access, Descriptor, ExtentLine, ExtentType, NO_PARENT_CID};

use crate::bytes::mul;
use crate::error::{Error, Result, ResultExt};
use crate::fs::{Extent, ExtentKind, File, FileKind, Volume};
use crate::io::BlockSource;

/// Largest descriptor file that is accepted. Real descriptors are a few hundred bytes long.
const MAX_DESCRIPTOR_SIZE: u64 = 1 << 20;
const SECTOR_SIZE: u64 = 512;

#[derive(Debug)]
struct DiskInner {
    capacity: u64,
    descriptor: Descriptor,
    descriptor_path: String,
    flat: File,
    data_ranges: Box<[Range<u64>]>,
}

/// An opened virtual disk: the bytes a guest sees. Cheap to clone; reads can run in parallel.
#[derive(Debug, Clone)]
pub struct Disk {
    inner: Arc<DiskInner>,
}

impl Disk {
    /// Opens the virtual disk described by the descriptor at `path`.
    ///
    /// Only base disks with one flat extent are read. Snapshot links, raw device mappings, encrypted disks and
    /// anything else are refused with [`crate::ErrorKind::Unsupported`].
    pub fn open(volume: &Volume, path: &str) -> Result<Disk> {
        Self::open_inner(volume, path).context(format!("virtual disk {path:?}"))
    }

    fn open_inner(volume: &Volume, path: &str) -> Result<Disk> {
        let descriptor_file = volume.open_file(path)?;
        if descriptor_file.len() > MAX_DESCRIPTOR_SIZE {
            return Err(Error::invalid_input(format!(
                "the file is {} bytes long; a descriptor is a small text file (pass the descriptor, not the \
                 -flat or -sesparse file)",
                descriptor_file.len()
            )));
        }
        let descriptor = Descriptor::parse(&descriptor_file.read_all(MAX_DESCRIPTOR_SIZE)?)?;

        if let Some(encoding) = descriptor.get("encoding") {
            if !encoding.eq_ignore_ascii_case("UTF-8") {
                return Err(Error::unsupported(format!(
                    "descriptor encoding {encoding:?} (only UTF-8 is supported)"
                )));
            }
        }
        if !matches!(descriptor.version, 1 | 3) {
            return Err(Error::unsupported(format!("descriptor version {}", descriptor.version)));
        }
        if let Some(marker) = descriptor.io_filter_marker() {
            return Err(Error::unsupported(format!(
                "the disk uses an I/O filter or encryption ({marker}); its content cannot be read directly"
            )));
        }
        if descriptor.has_parent() {
            return Err(Error::unsupported(format!(
                "the disk is a snapshot link (parent {:?}, parent content id {:08x}); snapshot chains are not \
                 supported yet",
                descriptor.parent_file_name_hint.as_deref().unwrap_or("?"),
                descriptor.parent_cid
            )));
        }
        let extent = match descriptor.extents.as_slice() {
            [extent] => extent,
            extents => {
                return Err(Error::unsupported(format!(
                    "the descriptor has {} extents; disks on VMFS have exactly one",
                    extents.len()
                )));
            }
        };
        match &extent.kind {
            ExtentType::Vmfs => {}
            ExtentType::SeSparse | ExtentType::VmfsSparse => {
                return Err(Error::unsupported(
                    "the extent is a snapshot delta; snapshot chains are not supported yet",
                ));
            }
            ExtentType::VmfsRdm | ExtentType::VmfsRaw => {
                return Err(Error::unsupported(
                    "the disk is a raw device mapping; its data lives on another device",
                ));
            }
            other => return Err(Error::unsupported(format!("extent type {other:?}"))),
        }
        if extent.access == Access::NoAccess {
            return Err(Error::unsupported("the extent is marked NOACCESS"));
        }
        if extent.offset.unwrap_or(0) != 0 {
            return Err(Error::unsupported(format!(
                "the extent starts at sector offset {:?}",
                extent.offset
            )));
        }

        let flat_path = sibling_path(path, &extent.file_name)?;
        let flat = volume
            .open_file(&flat_path)
            .context(format!("extent file {flat_path:?}"))?;
        if flat.stat().kind != FileKind::Regular {
            return Err(Error::unsupported(format!(
                "extent file {flat_path:?} is a {:?}",
                flat.stat().kind
            )));
        }
        let capacity = mul(extent.sectors, SECTOR_SIZE, "disk capacity")?;
        if flat.len() != capacity {
            return Err(Error::unsupported(format!(
                "the descriptor says the disk has {capacity} bytes, but {flat_path:?} is {} bytes long",
                flat.len()
            )));
        }
        let data_ranges = data_ranges(flat.extents());
        Ok(Disk {
            inner: Arc::new(DiskInner {
                capacity,
                descriptor,
                descriptor_path: path.to_owned(),
                flat,
                data_ranges,
            }),
        })
    }

    /// Size of the disk in bytes.
    pub fn capacity(&self) -> u64 {
        self.inner.capacity
    }

    /// The parsed descriptor.
    pub fn descriptor(&self) -> &Descriptor {
        &self.inner.descriptor
    }

    /// Path of the descriptor this disk was opened from.
    pub fn descriptor_path(&self) -> &str {
        &self.inner.descriptor_path
    }

    /// The ranges of the disk that hold written data, sorted and merged. Everything outside them reads as zeros,
    /// so a backup can skip it.
    pub fn data_ranges(&self) -> &[Range<u64>] {
        &self.inner.data_ranges
    }

    /// The block map of the disk: for every range, whether it is data, allocated but never written, or
    /// unallocated, and where on the volume it lives.
    pub fn extents(&self) -> &[Extent] {
        self.inner.flat.extents()
    }
}

impl BlockSource for Disk {
    fn len(&self) -> u64 {
        self.inner.capacity
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.inner.flat.read_exact_at(offset, buf)
    }
}

/// Resolves an extent file name relative to the directory of its descriptor.
fn sibling_path(descriptor_path: &str, file_name: &str) -> Result<String> {
    if file_name.starts_with('/') {
        return Err(Error::unsupported(format!(
            "the extent file {file_name:?} is given as an absolute path, which may point to another datastore"
        )));
    }
    if file_name.split('/').any(|part| part == "..") {
        return Err(Error::unsupported(format!(
            "the extent file name {file_name:?} leaves the disk's directory"
        )));
    }
    let directory = descriptor_path.rsplit_once('/').map_or("", |(directory, _)| directory);
    Ok(format!("{directory}/{file_name}"))
}

/// Merges the data extents of a block map into ranges of the file.
fn data_ranges(extents: &[Extent]) -> Box<[Range<u64>]> {
    let mut ranges: Vec<Range<u64>> = Vec::new();
    for extent in extents {
        if !matches!(extent.kind, ExtentKind::Data { .. }) {
            continue;
        }
        let end = extent.offset.saturating_add(extent.len);
        match ranges.last_mut() {
            Some(last) if last.end == extent.offset => last.end = end,
            _ => ranges.push(extent.offset..end),
        }
    }
    ranges.into_boxed_slice()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    #[test]
    fn extent_files_are_looked_up_next_to_the_descriptor() {
        assert_eq!(
            sibling_path("vm1/vm1.vmdk", "vm1-flat.vmdk").unwrap(),
            "vm1/vm1-flat.vmdk"
        );
        assert_eq!(sibling_path("/a/b/c.vmdk", "c-flat.vmdk").unwrap(), "/a/b/c-flat.vmdk");
        assert_eq!(sibling_path("c.vmdk", "c-flat.vmdk").unwrap(), "/c-flat.vmdk");
        assert_eq!(
            sibling_path("vm/a.vmdk", "/vmfs/volumes/x/a-flat.vmdk")
                .unwrap_err()
                .kind(),
            ErrorKind::Unsupported
        );
        assert_eq!(
            sibling_path("vm/a.vmdk", "../other/a-flat.vmdk").unwrap_err().kind(),
            ErrorKind::Unsupported
        );
    }

    #[test]
    fn data_ranges_skip_holes_and_unwritten_blocks_and_merge_neighbours() {
        let data = |offset, len, volume_offset| Extent {
            offset,
            len,
            kind: ExtentKind::Data { volume_offset },
        };
        let extents = [
            data(0, 10, 1000),
            data(10, 5, 5000), // adjacent in the file, not on the volume: still one range
            Extent {
                offset: 15,
                len: 5,
                kind: ExtentKind::Hole,
            },
            data(20, 10, 2000),
            Extent {
                offset: 30,
                len: 10,
                kind: ExtentKind::ToBeZeroed { volume_offset: 3000 },
            },
            data(40, 1, 4000),
        ];
        assert_eq!(&*data_ranges(&extents), &[0..15, 20..30, 40..41]);
        assert!(data_ranges(&[]).is_empty());
    }
}
