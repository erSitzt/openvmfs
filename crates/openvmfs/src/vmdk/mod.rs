//! Virtual disks (VMDK) stored on a VMFS volume.
//!
//! A virtual disk on a datastore is a small text descriptor (`disk.vmdk`) that names one extent file. For a base
//! disk that is a flat file (`disk-flat.vmdk`) with the disk's content at the same offsets; whether the disk is
//! thin or thick shows only in that file's block map. A snapshot adds a link: a descriptor (`disk-000001.vmdk`)
//! that names its parent and a delta file holding what was written since.

mod descriptor;
mod sesparse;

use std::ops::Range;
use std::sync::Arc;

pub use descriptor::{Access, Descriptor, ExtentLine, ExtentType, NO_PARENT_CID};

use crate::bytes::{add, mul};
use crate::error::{Error, Result, ResultExt, UnsupportedReason};
use crate::fs::{Extent, ExtentKind, File, FileKind, LockMode, Volume};
use crate::io::BlockSource;

use sesparse::{RunKind, SeSparse};

/// Largest descriptor file that is accepted. Real descriptors are a few hundred bytes long.
const MAX_DESCRIPTOR_SIZE: u64 = 1 << 20;
const SECTOR_SIZE: u64 = 512;
/// Longest snapshot chain that is followed. ESXi allows 32 snapshots per VM.
const MAX_CHAIN_LENGTH: usize = 64;

/// One link of a disk's snapshot chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainLink {
    /// Path of the link's descriptor inside the datastore.
    pub descriptor_path: String,
    /// Its parsed descriptor.
    pub descriptor: Descriptor,
}

#[derive(Debug)]
enum Layer {
    /// The base disk: a flat file.
    Flat(File),
    /// A snapshot delta, and the file it is stored in.
    SeSparse(SeSparse, File),
}

/// What backs a range of a virtual disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backing {
    /// The range reads as zeros: unallocated, never written, or discarded.
    Zero,
    /// The bytes are stored on a device, at this offset from its start. `device` is the device's position in the
    /// list the volume was opened with; 0 for a datastore on one device.
    Device { device: usize, offset: u64 },
}

/// A range of a virtual disk and what backs it. See [`Disk::map`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MappedRange {
    /// Offset on the virtual disk.
    pub offset: u64,
    pub len: u64,
    pub backing: Backing,
}

/// Collects mapped ranges in order, merging neighbours, up to a limit.
struct MapCollector {
    ranges: Vec<MappedRange>,
    limit: usize,
}

impl MapCollector {
    /// Adds a range that starts where the last one ended. Returns `false` when the limit is reached and the range
    /// was not added.
    fn push(&mut self, offset: u64, len: u64, backing: Backing) -> bool {
        if len == 0 {
            return true;
        }
        if let Some(last) = self.ranges.last_mut() {
            let continues = match (last.backing, backing) {
                (Backing::Zero, Backing::Zero) => true,
                (
                    Backing::Device { device: a, offset: at },
                    Backing::Device {
                        device: b,
                        offset: next,
                    },
                ) => a == b && at.checked_add(last.len) == Some(next),
                _ => false,
            };
            if continues {
                last.len = last.len.saturating_add(len);
                return true;
            }
        }
        if self.ranges.len() >= self.limit {
            return false;
        }
        self.ranges.push(MappedRange { offset, len, backing });
        true
    }
}

#[derive(Debug)]
struct DiskInner {
    capacity: u64,
    /// The descriptor the disk was opened from, and its path.
    descriptor: Descriptor,
    descriptor_path: String,
    /// Newest link first, the base disk last.
    chain: Vec<ChainLink>,
    /// One per link, in the same order.
    layers: Vec<Layer>,
    /// The volume the disk lives on, for translating volume offsets into device offsets.
    volume: Volume,
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
    /// `path` may name a base disk or a snapshot link; a link is read through its whole chain, down to the base.
    /// To read a disk as it was when a snapshot was taken, pass the descriptor that was current at that time, not
    /// the one the running VM writes to.
    ///
    /// Raw device mappings, encrypted disks, deltas in the older COWD format, chains that leave the datastore and
    /// anything else that has not been verified are refused with [`crate::ErrorKind::Unsupported`].
    ///
    /// A disk whose flat file or delta an ESXi host had open for writing when the image was taken is refused with
    /// [`crate::ErrorKind::InUse`]: the current disk of a running VM. The disks below a VM snapshot are not.
    pub fn open(volume: &Volume, path: &str) -> Result<Disk> {
        Self::open_chain(volume, path).context(format!("virtual disk {path:?}"))
    }

    fn open_chain(volume: &Volume, path: &str) -> Result<Disk> {
        let mut chain: Vec<ChainLink> = Vec::new();
        let mut layers: Vec<Layer> = Vec::new();
        let mut capacity = None;
        let mut next = Some(path.to_owned());

        while let Some(link_path) = next.take() {
            if chain.len() >= MAX_CHAIN_LENGTH {
                return Err(Error::unsupported(format!(
                    "the snapshot chain is longer than {MAX_CHAIN_LENGTH} links"
                )));
            }
            if chain.iter().any(|link| link.descriptor_path == link_path) {
                return Err(Error::corrupt(format!(
                    "the snapshot chain loops back to {link_path:?}"
                )));
            }
            let descriptor = read_descriptor(volume, &link_path).context(format!("descriptor {link_path:?}"))?;
            let extent = single_extent(&descriptor).context(format!("descriptor {link_path:?}"))?;
            let link_capacity = mul(extent.sectors, SECTOR_SIZE, "disk capacity")?;
            match capacity {
                None => capacity = Some(link_capacity),
                Some(expected) if expected != link_capacity => {
                    return Err(Error::unsupported(format!(
                        "{link_path:?} describes a disk of {link_capacity} bytes, its child one of {expected} bytes; \
                         chains whose links differ in size are not supported"
                    )));
                }
                Some(_) => {}
            }
            // A child records the content id its parent had when the link was made.
            if let Some(child) = chain.last() {
                if child.descriptor.parent_cid != descriptor.cid {
                    return Err(Error::corrupt(format!(
                        "{:?} expects its parent to have content id {:08x}, but {link_path:?} has {:08x}: the \
                         parent was changed after the snapshot was taken, or the chain is broken",
                        child.descriptor_path, child.descriptor.parent_cid, descriptor.cid
                    )));
                }
            }

            let extent_path = sibling_path(&link_path, &extent.file_name)?;
            let file = volume
                .open_file(&extent_path)
                .context(format!("extent file {extent_path:?}"))?;
            if file.stat().kind != FileKind::Regular {
                return Err(Error::unsupported(format!(
                    "extent file {extent_path:?} is a {:?}",
                    file.stat().kind
                )));
            }
            // A file that a host had open for writing has its newest block allocations in the host's memory and in
            // the VMFS journal, not where a reader looks. Reading it gives a mix of old and new.
            let lock = file.stat().lock;
            if lock.is_open_for_writing() && !volume.accepts_in_use() && !volume.journal_replayed() {
                if lock == LockMode::MultiWriter {
                    return Err(Error::in_use(format!(
                        "{extent_path:?} is a multi-writer disk and was open on host {} when this image was taken \
                         (lock {lock:?}); its newest writes may be missing while later ones are present. VM snapshots \
                         leave such disks out: read it from an image taken while no VM had it open, or back it up \
                         another way",
                        file.stat().lock_owner
                    )));
                }
                return Err(Error::in_use(format!(
                    "{extent_path:?} was open for writing on host {} when this image was taken (lock {lock:?}); its \
                     newest writes may be missing while later ones are present. Take a VM snapshot before the \
                     storage snapshot and read the disk below it",
                    file.stat().lock_owner
                )));
            }
            match &extent.kind {
                ExtentType::Vmfs => {
                    if descriptor.has_parent() {
                        return Err(Error::unsupported(format!(
                            "{link_path:?} has a flat extent but names a parent; this combination is not supported"
                        )));
                    }
                    if file.len() != link_capacity {
                        return Err(Error::unsupported(format!(
                            "the descriptor says the disk has {link_capacity} bytes, but {extent_path:?} is {} bytes \
                             long",
                            file.len()
                        )));
                    }
                    layers.push(Layer::Flat(file));
                }
                ExtentType::SeSparse => {
                    let hint = descriptor.parent_file_name_hint.clone().ok_or_else(|| {
                        Error::unsupported(format!("{link_path:?} is a snapshot delta without a parent"))
                    })?;
                    if descriptor.parent_cid == NO_PARENT_CID {
                        return Err(Error::corrupt(format!(
                            "{link_path:?} names the parent {hint:?} but has no parent content id"
                        )));
                    }
                    let delta = SeSparse::open(Arc::new(file.clone()), link_capacity, volume.accepts_unverified())
                        .context(format!("delta file {extent_path:?}"))?;
                    layers.push(Layer::SeSparse(delta, file));
                    next = Some(parent_path(volume, &link_path, &hint)?);
                }
                other => return Err(unsupported_extent(other)),
            }
            chain.push(ChainLink {
                descriptor_path: link_path,
                descriptor,
            });
        }

        let capacity = capacity.unwrap_or(0);
        let top = chain.first().ok_or_else(|| Error::corrupt("empty snapshot chain"))?;
        let (descriptor, descriptor_path) = (top.descriptor.clone(), top.descriptor_path.clone());
        let data_ranges = chain_data_ranges(&layers);
        Ok(Disk {
            inner: Arc::new(DiskInner {
                capacity,
                descriptor,
                descriptor_path,
                chain,
                layers,
                volume: volume.clone(),
                data_ranges,
            }),
        })
    }

    /// Size of the disk in bytes.
    pub fn capacity(&self) -> u64 {
        self.inner.capacity
    }

    /// The descriptor the disk was opened from.
    pub fn descriptor(&self) -> &Descriptor {
        &self.inner.descriptor
    }

    /// Path of the descriptor this disk was opened from.
    pub fn descriptor_path(&self) -> &str {
        &self.inner.descriptor_path
    }

    /// The snapshot chain: the descriptor the disk was opened from first, the base disk last. A base disk has a
    /// chain of one.
    pub fn chain(&self) -> &[ChainLink] {
        &self.inner.chain
    }

    /// The ranges of the disk that hold written data, sorted and merged. Everything outside them reads as zeros,
    /// so a backup can skip it.
    pub fn data_ranges(&self) -> &[Range<u64>] {
        &self.inner.data_ranges
    }

    /// The block map of the **base** disk's flat file: for every range, whether it is data, allocated but never
    /// written, or unallocated, and where on the volume it lives. Snapshot deltas are not part of it; use
    /// [`Disk::data_ranges`] for what the whole chain holds.
    pub fn extents(&self) -> &[Extent] {
        match self.inner.layers.last() {
            Some(Layer::Flat(file)) => file.extents(),
            _ => &[],
        }
    }

    /// Where the bytes of `range` are stored: for every part of it, either an offset on a device or "zeros".
    ///
    /// With this map a caller can read the disk's data from the devices itself, in whatever way suits it
    /// (asynchronously, batched, over a network), and leave only the metadata to this library. Reading a
    /// [`Backing::Device`] range from its device gives exactly the bytes [`Disk::read_exact_at`] returns for it.
    ///
    /// The result starts at `range.start`, is contiguous, and has at most `limit` entries; neighbouring ranges
    /// are merged. If it ends before `range.end`, call again from where it ended. A base disk needs an entry per
    /// run of file blocks; a disk with snapshots can need one per 4 KiB where a delta is fragmented.
    ///
    /// Not available on a volume opened with `replay_journal`: there, what is read differs from what the devices
    /// hold.
    pub fn map(&self, range: Range<u64>, limit: usize) -> Result<Vec<MappedRange>> {
        if range.start > range.end || range.end > self.inner.capacity {
            return Err(Error::invalid_input(format!(
                "range {:#x}..{:#x} is not inside the disk ({:#x} bytes)",
                range.start, range.end, self.inner.capacity
            )));
        }
        if self.inner.volume.journal_replayed() {
            return Err(Error::invalid_input(
                "a device map is not available on a volume that is read with its journal replayed",
            ));
        }
        let mut out = MapCollector {
            ranges: Vec::new(),
            limit,
        };
        self.map_layer(0, range.start, range.end, &mut out)?;
        Ok(out.ranges)
    }

    /// Maps `start..end` of the disk as layer `index` and the layers below it see it. Returns `false` when the
    /// collector is full.
    fn map_layer(&self, index: usize, start: u64, end: u64, out: &mut MapCollector) -> Result<bool> {
        match self.inner.layers.get(index) {
            Some(Layer::Flat(file)) => self.map_file(file, start, end, start, out),
            Some(Layer::SeSparse(delta, file)) => {
                let runs = delta.runs();
                let mut offset = start;
                let mut next = runs.partition_point(|run| run.offset.saturating_add(run.len) <= offset);
                while offset < end {
                    let run = runs.get(next);
                    // Up to the next run of the delta, the layers below decide.
                    let below_until = match run {
                        Some(run) if run.offset <= offset => offset,
                        Some(run) => run.offset.min(end),
                        None => end,
                    };
                    if below_until > offset {
                        if !self.map_layer(index.saturating_add(1), offset, below_until, out)? {
                            return Ok(false);
                        }
                        offset = below_until;
                        continue;
                    }
                    let Some(run) = run else { break };
                    let within = offset.saturating_sub(run.offset);
                    let until = run.offset.saturating_add(run.len).min(end);
                    let more = match run.kind {
                        RunKind::Zero => out.push(offset, until.saturating_sub(offset), Backing::Zero),
                        RunKind::Allocated { file_offset } => {
                            let from = add(file_offset, within, "delta file offset")?;
                            let to = add(from, until.saturating_sub(offset), "delta file offset")?;
                            self.map_file(file, from, to, offset, out)?
                        }
                    };
                    if !more {
                        return Ok(false);
                    }
                    offset = until;
                    next = next.saturating_add(1);
                }
                Ok(true)
            }
            None => Err(Error::corrupt("snapshot chain ends without a base disk")),
        }
    }

    /// Maps `start..end` of a file, which holds the disk's bytes from `disk_offset` on.
    fn map_file(
        &self,
        file: &File,
        start: u64,
        end: u64,
        mut disk_offset: u64,
        out: &mut MapCollector,
    ) -> Result<bool> {
        let extents = file.extents();
        let mut offset = start;
        let mut index = extents.partition_point(|extent| extent.offset.saturating_add(extent.len) <= offset);
        while offset < end {
            let extent = extents
                .get(index)
                .filter(|extent| extent.offset <= offset)
                .ok_or_else(|| {
                    Error::corrupt(format!(
                        "the block map of an extent file ends before offset {offset:#x}"
                    ))
                })?;
            let within = offset.saturating_sub(extent.offset);
            let mut left = extent.offset.saturating_add(extent.len).min(end).saturating_sub(offset);
            match extent.kind {
                ExtentKind::Data { volume_offset } => {
                    let mut volume_offset = add(volume_offset, within, "volume offset")?;
                    // A run of blocks can cross from one device of a spanned datastore to the next.
                    while left > 0 {
                        let (device, device_offset, available) = self.inner.volume.device_run(volume_offset)?;
                        let take = left.min(available);
                        if take == 0 {
                            return Err(Error::corrupt(format!(
                                "volume offset {volume_offset:#x} maps to nothing"
                            )));
                        }
                        let backing = Backing::Device {
                            device,
                            offset: device_offset,
                        };
                        if !out.push(disk_offset, take, backing) {
                            return Ok(false);
                        }
                        left = left.saturating_sub(take);
                        offset = add(offset, take, "file offset")?;
                        disk_offset = add(disk_offset, take, "disk offset")?;
                        volume_offset = add(volume_offset, take, "volume offset")?;
                    }
                }
                ExtentKind::ToBeZeroed { .. } | ExtentKind::Hole => {
                    if !out.push(disk_offset, left, Backing::Zero) {
                        return Ok(false);
                    }
                    offset = add(offset, left, "file offset")?;
                    disk_offset = add(disk_offset, left, "disk offset")?;
                }
            }
            index = index.saturating_add(1);
        }
        Ok(true)
    }

    fn read_layer(&self, index: usize, offset: u64, buf: &mut [u8]) -> Result<()> {
        match self.inner.layers.get(index) {
            Some(Layer::Flat(file)) => file.read_exact_at(offset, buf),
            Some(Layer::SeSparse(delta, _)) => {
                let below = index.saturating_add(1);
                delta.read_exact_at(offset, buf, &|offset, buf| self.read_layer(below, offset, buf))
            }
            None => Err(Error::corrupt("snapshot chain ends without a base disk")),
        }
    }
}

impl BlockSource for Disk {
    fn len(&self) -> u64 {
        self.inner.capacity
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        match offset.checked_add(buf.len() as u64) {
            Some(end) if end <= self.inner.capacity => self.read_layer(0, offset, buf),
            _ => Err(Error::invalid_input(format!(
                "read of {} bytes at {offset:#x} goes beyond the end of the disk ({:#x} bytes)",
                buf.len(),
                self.inner.capacity
            ))),
        }
    }
}

/// Reads and checks a descriptor: what applies to every link of a chain.
fn read_descriptor(volume: &Volume, path: &str) -> Result<Descriptor> {
    let file = volume.open_file(path)?;
    if file.len() > MAX_DESCRIPTOR_SIZE {
        return Err(Error::invalid_input(format!(
            "the file is {} bytes long; a descriptor is a small text file (pass the descriptor, not the -flat or \
             -sesparse file)",
            file.len()
        )));
    }
    let descriptor = Descriptor::parse(&file.read_all(MAX_DESCRIPTOR_SIZE)?)?;
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
        return Err(Error::unsupported_because(
            UnsupportedReason::Encrypted,
            format!("the disk uses an I/O filter or encryption ({marker}); its content cannot be read directly"),
        ));
    }
    Ok(descriptor)
}

/// The one extent of a descriptor, with the checks that do not depend on its type.
fn single_extent(descriptor: &Descriptor) -> Result<&ExtentLine> {
    let extent = match descriptor.extents.as_slice() {
        [extent] => extent,
        extents => {
            return Err(Error::unsupported(format!(
                "the descriptor has {} extents; disks on VMFS have exactly one",
                extents.len()
            )));
        }
    };
    if extent.access == Access::NoAccess {
        return Err(Error::unsupported("the extent is marked NOACCESS"));
    }
    if extent.offset.unwrap_or(0) != 0 {
        return Err(Error::unsupported(format!(
            "the extent starts at sector offset {:?}",
            extent.offset
        )));
    }
    // Refuse before trying to open a file that may not be a file at all.
    match &extent.kind {
        ExtentType::Vmfs | ExtentType::SeSparse => Ok(extent),
        other => Err(unsupported_extent(other)),
    }
}

fn unsupported_extent(kind: &ExtentType) -> Error {
    match kind {
        ExtentType::VmfsSparse => Error::unsupported_because(
            UnsupportedReason::LegacySparse,
            "the extent is a snapshot delta in the older COWD format (VMFSSPARSE), which VMFS6 does not create and \
             which is not supported",
        ),
        ExtentType::VmfsRdm | ExtentType::VmfsRaw => Error::unsupported_because(
            UnsupportedReason::RawDeviceMapping,
            "the disk is a raw device mapping; its data lives on another device",
        ),
        other => Error::unsupported(format!("extent type {other:?}")),
    }
}

/// Resolves an extent file name relative to the directory of its descriptor.
fn sibling_path(descriptor_path: &str, file_name: &str) -> Result<String> {
    if file_name.starts_with('/') {
        return Err(Error::unsupported_because(
            UnsupportedReason::ChainLeavesDatastore,
            format!("the extent file {file_name:?} is given as an absolute path, which may point to another datastore"),
        ));
    }
    if file_name.split('/').any(|part| part == "..") {
        return Err(Error::unsupported_because(
            UnsupportedReason::ChainLeavesDatastore,
            format!("the extent file name {file_name:?} leaves the disk's directory"),
        ));
    }
    let directory = descriptor_path.rsplit_once('/').map_or("", |(directory, _)| directory);
    Ok(format!("{directory}/{file_name}"))
}

/// Resolves a parent hint. ESXi writes a bare file name for a parent in the same directory, and
/// `/vmfs/volumes/<datastore>/...` when the parent is elsewhere.
fn parent_path(volume: &Volume, descriptor_path: &str, hint: &str) -> Result<String> {
    if let Some(rest) = hint.strip_prefix("/vmfs/volumes/") {
        let (datastore, path) = rest.split_once('/').unwrap_or((rest, ""));
        let info = volume.info();
        if datastore == info.uuid.to_string() || datastore == info.label {
            if path.split('/').any(|part| part == "..") {
                return Err(Error::unsupported_because(
                    UnsupportedReason::ChainLeavesDatastore,
                    format!("the parent hint {hint:?} contains \"..\""),
                ));
            }
            return Ok(format!("/{path}"));
        }
        return Err(Error::unsupported_because(
            UnsupportedReason::ChainLeavesDatastore,
            format!(
                "the parent {hint:?} is on another datastore ({datastore}); chains across datastores are not supported"
            ),
        ));
    }
    sibling_path(descriptor_path, hint).map_err(|_| {
        Error::unsupported_because(
            UnsupportedReason::ChainLeavesDatastore,
            format!("the parent hint {hint:?} cannot be resolved inside this datastore"),
        )
    })
}

/// The ranges that hold data when a disk is read through all its layers.
///
/// Starting from the base disk, each delta adds the ranges it holds and removes the ranges it declares zero.
fn chain_data_ranges(layers: &[Layer]) -> Box<[Range<u64>]> {
    let mut ranges: Vec<Range<u64>> = Vec::new();
    for layer in layers.iter().rev() {
        match layer {
            Layer::Flat(file) => ranges = flat_data_ranges(file.extents()),
            Layer::SeSparse(delta, _) => {
                let mut allocated = Vec::new();
                let mut zero = Vec::new();
                for run in delta.runs() {
                    let range = run.offset..run.offset.saturating_add(run.len);
                    match run.kind {
                        RunKind::Allocated { .. } => push_range(&mut allocated, range),
                        RunKind::Zero => push_range(&mut zero, range),
                    }
                }
                ranges = union(&subtract(&ranges, &zero), &allocated);
            }
        }
    }
    ranges.into_boxed_slice()
}

/// Merges the data extents of a block map into ranges of the file.
fn flat_data_ranges(extents: &[Extent]) -> Vec<Range<u64>> {
    let mut ranges: Vec<Range<u64>> = Vec::new();
    for extent in extents {
        if matches!(extent.kind, ExtentKind::Data { .. }) {
            push_range(&mut ranges, extent.offset..extent.offset.saturating_add(extent.len));
        }
    }
    ranges
}

/// Appends a range to a sorted list, merging it with the last one when they touch.
fn push_range(ranges: &mut Vec<Range<u64>>, range: Range<u64>) {
    if range.start >= range.end {
        return;
    }
    match ranges.last_mut() {
        Some(last) if last.end >= range.start => last.end = last.end.max(range.end),
        _ => ranges.push(range),
    }
}

/// `a` without everything in `b`. Both are sorted and merged; so is the result.
fn subtract(a: &[Range<u64>], b: &[Range<u64>]) -> Vec<Range<u64>> {
    let mut result = Vec::new();
    let mut cuts = b.iter().peekable();
    for range in a {
        let mut start = range.start;
        while let Some(cut) = cuts.peek() {
            if cut.end <= start {
                cuts.next();
            } else if cut.start >= range.end {
                break;
            } else {
                push_range(&mut result, start..cut.start.min(range.end));
                start = start.max(cut.end);
                if cut.end > range.end {
                    break;
                }
                cuts.next();
            }
        }
        push_range(&mut result, start..range.end);
    }
    result
}

/// Everything in `a` or `b`. Both are sorted and merged; so is the result.
fn union(a: &[Range<u64>], b: &[Range<u64>]) -> Vec<Range<u64>> {
    let mut all: Vec<Range<u64>> = a.iter().chain(b.iter()).cloned().collect();
    all.sort_by_key(|range| range.start);
    let mut result = Vec::new();
    for range in all {
        push_range(&mut result, range);
    }
    result
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
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
        let outside = sibling_path("vm/a.vmdk", "../other/a-flat.vmdk").unwrap_err();
        assert_eq!(outside.kind(), ErrorKind::Unsupported);
        assert_eq!(
            outside.unsupported_reason(),
            Some(UnsupportedReason::ChainLeavesDatastore)
        );
    }

    #[test]
    fn refusals_say_why() {
        let reason = |kind: &ExtentType| unsupported_extent(kind).unsupported_reason();
        assert_eq!(reason(&ExtentType::VmfsRdm), Some(UnsupportedReason::RawDeviceMapping));
        assert_eq!(reason(&ExtentType::VmfsRaw), Some(UnsupportedReason::RawDeviceMapping));
        assert_eq!(reason(&ExtentType::VmfsSparse), Some(UnsupportedReason::LegacySparse));
        // Everything without a reason of its own is "not verified", and context keeps the reason.
        let plain = Error::unsupported("x").context("disk");
        assert_eq!(plain.unsupported_reason(), Some(UnsupportedReason::Unverified));
        assert_eq!(Error::corrupt("x").unsupported_reason(), None);
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
        assert_eq!(flat_data_ranges(&extents), [0..15, 20..30, 40..41]);
        assert!(flat_data_ranges(&[]).is_empty());
    }

    #[test]
    fn range_sets() {
        let a = [0..10, 20..30, 40..50];
        // Cuts that miss, clip, split and swallow.
        assert_eq!(subtract(&a, &[]), a);
        assert_eq!(subtract(&a, &[10..20]), a);
        assert_eq!(subtract(&a, &[5..25]), [0..5, 25..30, 40..50]);
        assert_eq!(subtract(&a, &[22..24, 26..28]), [0..10, 20..22, 24..26, 28..30, 40..50]);
        assert_eq!(subtract(&a, &[0..100]), Vec::<Range<u64>>::new());
        assert_eq!(subtract(&a, &[45..100]), [0..10, 20..30, 40..45]);
        assert_eq!(subtract(&[0..100], &[10..20, 30..40]), [0..10, 20..30, 40..100]);

        assert_eq!(union(&a, &[10..20]), [0..30, 40..50]);
        assert_eq!(union(&a, &[60..70, 5..8]), [0..10, 20..30, 40..50, 60..70]);
        assert_eq!(union(&[], &[1..2]), [1..2]);
    }
}
