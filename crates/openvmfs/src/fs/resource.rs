//! Resource files: the system files that hold file descriptors, sub-blocks and pointer blocks.
//!
//! A resource file starts with a header that describes its layout: resources are grouped into clusters, clusters
//! into cluster groups. An address names a cluster and a resource inside it.
//!
//! Format description: `docs/format/30-addresses-resources.md`.

use crate::bytes::{add, mul, View};
use crate::error::{Error, Result};
use crate::fs::blockmap::{BlockMap, SystemFile};
use crate::fs::descriptor::Geometry;
use crate::io::BlockSource;

/// "rfmd": signature of a resource file header.
const RESOURCE_FILE_SIGNATURE: u32 = 0x7266_6D64;
pub(crate) const RESOURCE_HEADER_SIZE: usize = 0x58;
/// Flag bit: the cluster groups of this file are nested inside those of a parent resource.
const FLAG_HAS_PARENT: u32 = 0x2;

/// The layout header of a resource file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResourceHeader {
    pub resources_per_cluster: u64,
    pub clusters_per_group: u64,
    pub cluster_group_offset: u64,
    pub resource_size: u64,
    pub cluster_group_size: u64,
    pub num_cluster_groups: u64,
    pub flags: u32,
    pub child_meta_offset: u32,
}

impl ResourceHeader {
    pub(crate) fn parse(raw: &[u8], file: &str) -> Result<Self> {
        let h = View::new("resource file header", 0, raw);
        let signature = h.u32(0x20)?;
        if signature != RESOURCE_FILE_SIGNATURE {
            return Err(Error::corrupt(format!(
                "{file}: resource header signature {signature:#010x}, expected {RESOURCE_FILE_SIGNATURE:#010x}"
            )));
        }
        let header = ResourceHeader {
            resources_per_cluster: u64::from(h.u32(0x00)?),
            clusters_per_group: u64::from(h.u32(0x04)?),
            cluster_group_offset: u64::from(h.u32(0x08)?),
            resource_size: u64::from(h.u32(0x0C)?),
            cluster_group_size: u64::from(h.u32(0x10)?),
            num_cluster_groups: u64::from(h.u32(0x18)?),
            flags: h.u32(0x28)?,
            child_meta_offset: h.u32(0x34)?,
        };
        // A resource file may be empty: on VMFS6 the pointer-block file `.pbc.sf` holds no resources, because
        // pointer blocks are stored as sub-blocks. Addresses into an empty file are rejected in `resource_offset`.
        Ok(header)
    }

    /// Offset of resource `resource` of cluster `cluster` inside the resource file.
    ///
    /// A cluster group starts with one header (a lock block and a metadata block) per cluster; the resources of
    /// all its clusters follow.
    pub(crate) fn resource_offset(&self, cluster: u64, resource: u64, geo: &Geometry, file: &str) -> Result<u64> {
        if self.flags & FLAG_HAS_PARENT != 0 {
            return Err(Error::unsupported(format!(
                "{file}: resource file with nested cluster groups (flags {:#x}) has not been verified",
                self.flags
            )));
        }
        // An empty file has 0 clusters per group; `u64::MAX` then fails the range check below.
        let group = cluster.checked_div(self.clusters_per_group).unwrap_or(u64::MAX);
        let cluster_in_group = cluster.checked_rem(self.clusters_per_group).unwrap_or(0);
        if resource >= self.resources_per_cluster || group >= self.num_cluster_groups {
            return Err(Error::corrupt(format!(
                "{file}: address <cluster {cluster}, resource {resource}> is outside the file's {} cluster groups \
                 of {} clusters with {} resources each",
                self.num_cluster_groups, self.clusters_per_group, self.resources_per_cluster
            )));
        }
        let cluster_header_size = mul(geo.md_alignment, 2, "cluster header size")?;
        let cluster_size = mul(self.resources_per_cluster, self.resource_size, "cluster size")?;
        let mut offset = add(
            self.cluster_group_offset,
            mul(group, self.cluster_group_size, "cluster group offset")?,
            "cluster group offset",
        )?;
        offset = add(
            offset,
            mul(self.clusters_per_group, cluster_header_size, "cluster headers")?,
            "resource area",
        )?;
        offset = add(
            offset,
            mul(cluster_in_group, cluster_size, "cluster offset")?,
            "cluster offset",
        )?;
        add(
            offset,
            mul(resource, self.resource_size, "resource offset")?,
            "resource offset",
        )
    }
}

/// An opened resource file: its block map and its layout header.
#[derive(Debug, Clone)]
pub(crate) struct ResourceFile {
    pub which: SystemFile,
    pub map: BlockMap,
    pub header: ResourceHeader,
}

impl ResourceFile {
    /// Reads the layout header of a system file whose block map is already known.
    pub(crate) fn open(which: SystemFile, map: BlockMap, volume: &dyn BlockSource) -> Result<Self> {
        let mut raw = [0u8; RESOURCE_HEADER_SIZE];
        map.read_metadata_at(volume, 0, &mut raw)
            .map_err(|e| e.context(format!("{} header", which.name())))?;
        let header = ResourceHeader::parse(&raw, which.name())?;
        Ok(ResourceFile { which, map, header })
    }

    /// Volume offset of a resource, which must be written data in one piece.
    pub(crate) fn locate(&self, cluster: u64, resource: u64, geo: &Geometry) -> Result<u64> {
        let file_offset = self.header.resource_offset(cluster, resource, geo, self.which.name())?;
        let end = add(file_offset, self.header.resource_size, "resource end")?;
        if end > self.map.len() {
            return Err(Error::corrupt(format!(
                "{}: resource <cluster {cluster}, resource {resource}> ends at {end:#x}, beyond the file's {:#x} bytes",
                self.which.name(),
                self.map.len()
            )));
        }
        self.map.locate(file_offset, self.header.resource_size).map_err(|e| {
            e.context(format!(
                "{} <cluster {cluster}, resource {resource}>",
                self.which.name()
            ))
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::fs::inode::tests::geometry;

    /// Builds a resource file header.
    pub(crate) fn build_header(rpc: u32, cpg: u32, cgo: u32, size: u32, cg_size: u32, groups: u32) -> Vec<u8> {
        let mut raw = vec![0u8; RESOURCE_HEADER_SIZE];
        let mut put = |offset: usize, value: u32| raw[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        put(0x00, rpc);
        put(0x04, cpg);
        put(0x08, cgo);
        put(0x0C, size);
        put(0x10, cg_size);
        put(0x18, groups);
        put(0x20, RESOURCE_FILE_SIGNATURE);
        raw
    }

    #[test]
    fn locates_resources() {
        let geo = geometry();
        // File descriptors as on a real volume: 8 KiB each, 8 clusters per group, group offset 64 KiB.
        let raw = build_header(8192, 8, 0x1_0000, 8192, 0x2000_0000, 4);
        let h = ResourceHeader::parse(&raw, ".fdc.sf").unwrap();
        // The first descriptor follows the 8 cluster headers of 8 KiB each.
        assert_eq!(h.resource_offset(0, 0, &geo, ".fdc.sf").unwrap(), 0x2_0000);
        assert_eq!(h.resource_offset(0, 3, &geo, ".fdc.sf").unwrap(), 0x2_0000 + 3 * 8192);
        assert_eq!(
            h.resource_offset(1, 0, &geo, ".fdc.sf").unwrap(),
            0x2_0000 + 8192 * 8192
        );
        // Cluster 8 is the first cluster of the second group.
        assert_eq!(
            h.resource_offset(8, 1, &geo, ".fdc.sf").unwrap(),
            0x1_0000 + 0x2000_0000 + 0x1_0000 + 8192
        );
    }

    #[test]
    fn rejects_addresses_outside_the_layout() {
        let geo = geometry();
        let h = ResourceHeader::parse(&build_header(16, 4, 0x1_0000, 65536, 0x40_0000, 2), ".sbc.sf").unwrap();
        assert_eq!(
            h.resource_offset(0, 16, &geo, ".sbc.sf").unwrap_err().kind(),
            ErrorKind::Corrupt
        );
        assert_eq!(
            h.resource_offset(8, 0, &geo, ".sbc.sf").unwrap_err().kind(),
            ErrorKind::Corrupt
        );
        assert!(h.resource_offset(7, 15, &geo, ".sbc.sf").is_ok());
    }

    #[test]
    fn rejects_bad_headers_and_unverified_layouts() {
        let geo = geometry();
        let mut raw = build_header(16, 4, 0x1_0000, 65536, 0x40_0000, 2);
        raw[0x28] = 2;
        let nested = ResourceHeader::parse(&raw, ".sbc.sf").unwrap();
        assert_eq!(
            nested.resource_offset(0, 0, &geo, ".sbc.sf").unwrap_err().kind(),
            ErrorKind::Unsupported
        );

        let mut bad_signature = build_header(16, 4, 0x1_0000, 65536, 0x40_0000, 2);
        bad_signature[0x20] = 0;
        assert_eq!(
            ResourceHeader::parse(&bad_signature, "x").unwrap_err().kind(),
            ErrorKind::Corrupt
        );
        // An empty file (as `.pbc.sf` on VMFS6) parses, but no address resolves into it.
        for raw in [
            build_header(0, 8, 0x1_0000, 65536, 65536, 0),
            build_header(16, 0, 0x1_0000, 65536, 65536, 1),
        ] {
            let empty = ResourceHeader::parse(&raw, ".pbc.sf").unwrap();
            assert_eq!(
                empty.resource_offset(0, 0, &geo, ".pbc.sf").unwrap_err().kind(),
                ErrorKind::Corrupt
            );
        }
        assert_eq!(
            ResourceHeader::parse(&[0u8; 8], "x").unwrap_err().kind(),
            ErrorKind::Corrupt
        );
    }
}
