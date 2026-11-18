//! The VMFS6 filesystem: volumes, files and directories.

mod addr;
mod blockmap;
mod descriptor;
mod dir;
mod inode;
mod resource;

use std::path::Path;
use std::sync::Arc;

pub use addr::FdAddr;
pub use blockmap::{Extent, ExtentKind};
pub use descriptor::FsDescriptor;
pub use dir::DirEntry;
pub use inode::{AddressingMode, FileKind};

use crate::bytes::{to_usize, View};
use crate::error::{Error, ErrorKind, Result, ResultExt};
use crate::gpt;
use crate::io::{read_vec, BlockSource, FileSource, SubSource};
use crate::lvm::{LogicalVolume, LvmDevice, LvmVolume};
use crate::vmdk::Disk;
use crate::Uuid;

use addr::Addr;
use blockmap::{AddressSpace, BlockMap, MapError, SystemFile};
use descriptor::Geometry;
use inode::Inode;
use resource::ResourceFile;

/// Volume offset of the heartbeat region.
const HEARTBEAT_OFFSET: u64 = 0x30_0000;
const HEARTBEAT_SLOTS: u64 = 1024;
const HEARTBEAT_FREE: u32 = 0xABCD_EF01;

/// Positions of the system files in the file-descriptor resource file (cluster 0).
const SFD_FILE_BLOCKS: u32 = 1;
const SFD_FILE_DESCRIPTORS: u32 = 2;
const SFD_POINTER_BLOCKS: u32 = 3;
const SFD_SUB_BLOCKS: u32 = 4;
const SFD_POINTER_BLOCKS2: u32 = 6;

/// How to open a volume.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenOptions {
    /// Open this partition (counted from 1) of a disk with a GUID partition table, instead of detecting it.
    pub partition: Option<u32>,
    /// Read structures whose interpretation is known from other implementations but has not been verified against
    /// data written by ESXi, instead of refusing them with [`ErrorKind::Unsupported`]. Currently: "to be zeroed"
    /// bitmaps other than the one ESXi was seen to write, and zero grains in snapshot deltas.
    ///
    /// Meant for verification work. Data read this way may be wrong.
    pub accept_unverified: bool,
}

/// Facts about an opened volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    /// Datastore label.
    pub label: String,
    /// Datastore UUID, as in `/vmfs/volumes/<uuid>`.
    pub uuid: Uuid,
    /// Filesystem version as ESXi prints it, for example `6.82`.
    pub version: String,
    /// Size of the logical volume in bytes.
    pub capacity: u64,
    /// Size of a file block.
    pub file_block_size: u64,
    /// Offset of the VMFS partition on the device that was opened (0 for a bare partition).
    pub partition_offset: u64,
    /// Heartbeat slots marked in use: hosts that were holding locks on the datastore (running VMs, open files)
    /// when the image was taken, so metadata may have been in the middle of an update.
    ///
    /// 0 does **not** mean the datastore was unmounted: a mounted but idle datastore shows 0 as well.
    pub active_heartbeats: u32,
    /// The filesystem descriptor.
    pub descriptor: FsDescriptor,
    /// The LVM device header.
    pub lvm_device: LvmDevice,
    /// The LVM volume.
    pub lvm_volume: LvmVolume,
}

/// Metadata of a file or directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    /// Address of the file descriptor (the "inode number").
    pub addr: FdAddr,
    /// What it is.
    pub kind: FileKind,
    /// Whether it is a directory (true for directories and for system files that are directories).
    pub is_dir: bool,
    /// Length in bytes.
    pub len: u64,
    /// Number of blocks allocated to the file.
    pub blocks: u64,
    /// Number of allocated blocks that have never been written.
    pub tbz_blocks: u64,
    /// How the file's blocks are addressed.
    pub addressing: AddressingMode,
    /// Modification time, seconds since 1970.
    pub mtime: u32,
    /// Change time, seconds since 1970.
    pub ctime: u32,
    /// Access time, seconds since 1970.
    pub atime: u32,
    /// Owner.
    pub uid: u32,
    /// Group.
    pub gid: u32,
    /// Permission bits (and the directory bit for directories).
    pub mode: u32,
    /// Link count.
    pub links: u32,
    /// Generation of the descriptor.
    pub generation: u32,
}

impl FileStat {
    fn new(inode: &Inode) -> Self {
        FileStat {
            addr: inode.addr,
            kind: inode.kind,
            is_dir: inode.is_dir(),
            len: inode.length,
            blocks: inode.num_blocks,
            tbz_blocks: inode.num_tbz_blocks,
            addressing: inode.addressing,
            mtime: inode.mtime,
            ctime: inode.ctime,
            atime: inode.atime,
            uid: inode.uid,
            gid: inode.gid,
            mode: inode.mode,
            links: inode.link_count,
            generation: inode.generation,
        }
    }
}

/// The resource files a volume needs for reading.
#[derive(Debug, Default)]
struct Resources {
    file_descriptors: Option<ResourceFile>,
    sub_blocks: Option<ResourceFile>,
    pointer_blocks: Option<ResourceFile>,
    pointer_blocks2: Option<ResourceFile>,
}

impl Resources {
    fn slot(&mut self, which: SystemFile) -> &mut Option<ResourceFile> {
        match which {
            SystemFile::FileDescriptors => &mut self.file_descriptors,
            SystemFile::SubBlocks => &mut self.sub_blocks,
            SystemFile::PointerBlocks => &mut self.pointer_blocks,
            SystemFile::PointerBlocks2 => &mut self.pointer_blocks2,
        }
    }

    fn get(&self, which: SystemFile) -> std::result::Result<&ResourceFile, MapError> {
        let slot = match which {
            SystemFile::FileDescriptors => &self.file_descriptors,
            SystemFile::SubBlocks => &self.sub_blocks,
            SystemFile::PointerBlocks => &self.pointer_blocks,
            SystemFile::PointerBlocks2 => &self.pointer_blocks2,
        };
        slot.as_ref().ok_or(MapError::Needs(which))
    }
}

/// Resolves addresses against the resource files opened so far.
struct Resolver<'a> {
    volume: &'a dyn BlockSource,
    geo: &'a Geometry,
    resources: &'a Resources,
}

impl AddressSpace for Resolver<'_> {
    fn read_pointer_block(&self, raw: u64) -> std::result::Result<Vec<u64>, MapError> {
        let (which, cluster, resource) = match Addr::decode(raw)? {
            Addr::SubBlock { cluster, resource } => (SystemFile::SubBlocks, cluster, resource),
            Addr::PointerBlock { cluster, resource } => (SystemFile::PointerBlocks, cluster, resource),
            Addr::PointerBlock2 { cluster, resource } => (SystemFile::PointerBlocks2, cluster, resource),
            Addr::SmallFileBlock { .. } | Addr::LargeFileBlock { .. } => {
                return Err(Error::unsupported(format!(
                    "pointer block stored in a file block (address {raw:#018x}) has not been verified"
                ))
                .into());
            }
            Addr::Null => return Err(Error::corrupt("null address where a pointer block is expected").into()),
        };
        let file = self.resources.get(which)?;
        if file.header.resource_size != self.geo.ptr_block_size {
            return Err(Error::unsupported(format!(
                "{}: pointer block of {} bytes (only {} has been verified)",
                which.name(),
                file.header.resource_size,
                self.geo.ptr_block_size
            ))
            .into());
        }
        let offset = file.locate(cluster, resource, self.geo)?;
        let raw_block = read_vec(self.volume, offset, self.geo.ptr_block_size, "pointer block")?;
        Ok(raw_block
            .chunks_exact(8)
            .map(|chunk| u64::from_le_bytes(chunk.try_into().unwrap_or([0; 8])))
            .collect())
    }

    fn sub_block_offset(&self, cluster: u64, resource: u64) -> std::result::Result<u64, MapError> {
        let file = self.resources.get(SystemFile::SubBlocks)?;
        if file.header.resource_size != self.geo.sub_block_size {
            return Err(Error::corrupt(format!(
                ".sbc.sf: resources are {} bytes, but the filesystem's sub-block size is {}",
                file.header.resource_size, self.geo.sub_block_size
            ))
            .into());
        }
        Ok(file.locate(cluster, resource, self.geo)?)
    }
}

fn fatal(error: MapError) -> Error {
    match error {
        MapError::Fatal(error) => error,
        MapError::Needs(which) => Error::corrupt(format!("{} is needed but could not be opened", which.name())),
    }
}

#[derive(Debug)]
struct VolumeInner {
    volume: LogicalVolume,
    partition_offset: u64,
    descriptor: FsDescriptor,
    geo: Geometry,
    resources: Resources,
    root: Inode,
    active_heartbeats: u32,
}

/// An opened VMFS6 volume. Cheap to clone; all methods can be called from many threads at once.
#[derive(Debug, Clone)]
pub struct Volume {
    inner: Arc<VolumeInner>,
}

impl Volume {
    /// Opens the VMFS6 volume in an image file or on a block device.
    ///
    /// The path may name a whole disk with a partition table (for example a LUN) or the VMFS partition itself.
    pub fn open_path(path: impl AsRef<Path>) -> Result<Self> {
        Self::open(Arc::new(FileSource::open(path)?))
    }

    /// Opens the VMFS6 volume on `device`: either a VMFS partition, or a disk with exactly one VMFS partition.
    pub fn open(device: Arc<dyn BlockSource>) -> Result<Self> {
        Self::open_with(device, &OpenOptions::default())
    }

    /// Opens the VMFS6 volume on `device` with explicit options.
    pub fn open_with(device: Arc<dyn BlockSource>, options: &OpenOptions) -> Result<Self> {
        if let Some(number) = options.partition {
            let partitions = gpt::read_partitions(&*device)?
                .ok_or_else(|| Error::not_found("the device has no GUID partition table"))?;
            let partition = partitions
                .iter()
                .find(|p| p.number == number)
                .ok_or_else(|| Error::not_found(format!("the device has no partition {number}")))?;
            let view = SubSource::new(device.clone(), partition.start, partition.len)?;
            return Self::open_at(Arc::new(view), partition.start, options);
        }
        if LogicalVolume::probe(&*device)? {
            return Self::open_at(device, 0, options);
        }
        let partitions = gpt::read_partitions(&*device)?.ok_or_else(|| {
            Error::not_found("no VMFS volume found: the device has neither an LVM header nor a GUID partition table")
        })?;
        let mut candidates = Vec::new();
        for partition in partitions.iter().filter(|p| p.is_vmfs()) {
            let view = SubSource::new(device.clone(), partition.start, partition.len)?;
            if LogicalVolume::probe(&view)? {
                candidates.push((partition.clone(), view));
            }
        }
        match candidates.len() {
            1 => {
                let (partition, view) = candidates.remove(0);
                Self::open_at(Arc::new(view), partition.start, options)
            }
            0 => Err(Error::not_found(
                "no VMFS volume found in the partition table of the device",
            )),
            n => Err(Error::invalid_input(format!(
                "the device has {n} VMFS partitions ({}); choose one with OpenOptions::partition",
                candidates
                    .iter()
                    .map(|(p, _)| p.number.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    fn open_at(partition: Arc<dyn BlockSource>, partition_offset: u64, options: &OpenOptions) -> Result<Self> {
        let volume = LogicalVolume::open(partition)?;
        let descriptor = FsDescriptor::read(&volume)?;
        if descriptor.lvm_volume_uuid != volume.volume_info().uuid {
            return Err(Error::corrupt(format!(
                "the filesystem belongs to LVM volume {}, but it is stored on LVM volume {}",
                descriptor.lvm_volume_uuid,
                volume.volume_info().uuid
            )));
        }
        let geo = Geometry::new(&descriptor, volume.len(), options.accept_unverified)?;
        let resources = open_resources(&volume, &geo)?;

        let mut inner = VolumeInner {
            volume,
            partition_offset,
            descriptor,
            geo,
            resources,
            root: Inode::placeholder(),
            active_heartbeats: 0,
        };
        // The root directory is reachable two ways: through the fixed position used while opening, and through
        // the file-descriptor resource file like every other file. They must agree.
        let root = inner.read_inode(FdAddr::ROOT)?;
        let expected = inner.geo.sfd_offset(0)?;
        if root.volume_offset != expected {
            return Err(Error::unsupported(format!(
                "the root directory's descriptor is at volume offset {:#x}, expected {expected:#x}; \
                 this layout has not been verified",
                root.volume_offset
            )));
        }
        if !root.is_dir() {
            return Err(Error::corrupt("the root descriptor is not a directory"));
        }
        inner.root = root;
        inner.active_heartbeats = count_active_heartbeats(&inner.volume, &inner.geo)?;
        Ok(Volume { inner: Arc::new(inner) })
    }

    /// Facts about the volume.
    pub fn info(&self) -> VolumeInfo {
        let inner = &self.inner;
        VolumeInfo {
            label: inner.descriptor.label.clone(),
            uuid: inner.descriptor.uuid,
            version: inner.descriptor.version_string(),
            capacity: inner.volume.len(),
            file_block_size: inner.descriptor.file_block_size,
            partition_offset: inner.partition_offset,
            active_heartbeats: inner.active_heartbeats,
            descriptor: inner.descriptor.clone(),
            lvm_device: inner.volume.device_info().clone(),
            lvm_volume: inner.volume.volume_info().clone(),
        }
    }

    /// Whether structures that have not been verified against ESXi are read instead of refused.
    pub(crate) fn accepts_unverified(&self) -> bool {
        self.inner.geo.accept_unverified
    }

    /// The logical volume as a block source (volume offsets).
    pub fn logical_volume(&self) -> &dyn BlockSource {
        &self.inner.volume
    }

    /// Translates a volume offset into an offset on the device that was opened.
    pub fn device_offset(&self, volume_offset: u64) -> Result<u64> {
        let (partition_offset, _) = self.inner.volume.translate(volume_offset)?;
        crate::bytes::add(self.inner.partition_offset, partition_offset, "device offset")
    }

    /// Lists a directory. `path` is relative to the datastore root; `""` or `"/"` is the root.
    pub fn read_dir(&self, path: &str) -> Result<Vec<DirEntry>> {
        let inode = self.inner.lookup(path)?;
        self.inner.read_dir(&inode).context(format!("directory {path:?}"))
    }

    /// Metadata of a file or directory.
    pub fn stat(&self, path: &str) -> Result<FileStat> {
        Ok(FileStat::new(&self.inner.lookup(path)?))
    }

    /// Opens a regular file or a system file for reading.
    pub fn open_file(&self, path: &str) -> Result<File> {
        let inode = self.inner.lookup(path)?;
        self.open_inode(inode).context(format!("file {path:?}"))
    }

    /// Opens the file with the given descriptor address.
    pub fn open_file_at(&self, addr: FdAddr) -> Result<File> {
        let inode = self.inner.read_inode(addr)?;
        self.open_inode(inode)
    }

    /// Reads the target of a symbolic link.
    pub fn read_link(&self, path: &str) -> Result<String> {
        let inode = self.inner.lookup(path)?;
        if inode.kind != FileKind::Symlink {
            return Err(Error::invalid_input(format!("{path:?} is not a symbolic link")));
        }
        if inode.length > 4096 {
            return Err(Error::corrupt(format!(
                "symbolic link {path:?} is {} bytes long",
                inode.length
            )));
        }
        let map = self.inner.build_map(&inode)?;
        let mut target = vec![0u8; to_usize(inode.length, "link length")?];
        map.read_exact_at(&self.inner.volume, 0, &mut target)?;
        String::from_utf8(target).map_err(|_| Error::corrupt(format!("target of symbolic link {path:?} is not UTF-8")))
    }

    /// Opens the virtual disk described by the descriptor file at `path` (the small `.vmdk` text file).
    pub fn open_disk(&self, path: &str) -> Result<Disk> {
        Disk::open(self, path)
    }

    fn open_inode(&self, inode: Inode) -> Result<File> {
        match inode.kind {
            FileKind::Regular | FileKind::System if !inode.is_dir() => {}
            FileKind::RawDeviceMap => {
                return Err(Error::unsupported(
                    "raw device mapping: its data lives on another device and cannot be read from this volume",
                ));
            }
            FileKind::Symlink => return Err(Error::invalid_input("is a symbolic link; use read_link")),
            _ => return Err(Error::invalid_input("is a directory")),
        }
        let map = self.inner.build_map(&inode)?;
        Ok(File {
            inner: Arc::new(FileInner {
                volume: self.inner.clone(),
                stat: FileStat::new(&inode),
                map,
            }),
        })
    }
}

impl VolumeInner {
    fn resolver(&self) -> Resolver<'_> {
        Resolver {
            volume: &self.volume,
            geo: &self.geo,
            resources: &self.resources,
        }
    }

    fn build_map(&self, inode: &Inode) -> Result<BlockMap> {
        BlockMap::build(inode, &self.geo, &self.resolver()).map_err(fatal)
    }

    fn read_inode(&self, addr: FdAddr) -> Result<Inode> {
        let file = self.resources.get(SystemFile::FileDescriptors).map_err(fatal)?;
        if file.header.resource_size != self.geo.fd_size {
            return Err(Error::corrupt(format!(
                ".fdc.sf: resources are {} bytes, but a file descriptor is {} bytes",
                file.header.resource_size, self.geo.fd_size
            )));
        }
        let offset = file.locate(addr.cluster(), addr.resource(), &self.geo)?;
        let raw = read_vec(&self.volume, offset, self.geo.fd_size, "file descriptor")?;
        Inode::parse(&raw, addr, offset, &self.geo)
    }

    fn read_dir(&self, inode: &Inode) -> Result<Vec<DirEntry>> {
        if !inode.is_dir() {
            return Err(Error::invalid_input("not a directory"));
        }
        let map = self.build_map(inode)?;
        dir::read_dir(
            &MappedFile {
                volume: &self.volume,
                map: &map,
            },
            &self.geo,
        )
    }

    /// Walks `path` from the root. Symbolic links are not followed.
    fn lookup(&self, path: &str) -> Result<Inode> {
        let mut current = self.root.clone();
        let mut walked = String::new();
        for component in path.split('/').filter(|c| !c.is_empty() && *c != ".") {
            if component == ".." {
                return Err(Error::invalid_input(format!("path {path:?}: \"..\" is not allowed")));
            }
            if !current.is_dir() {
                return Err(Error::not_found(format!(
                    "{walked:?} is not a directory (in path {path:?})"
                )));
            }
            let entries = self.read_dir(&current).context(format!("directory {walked:?}"))?;
            let entry = entries
                .iter()
                .find(|entry| entry.name == component)
                .ok_or_else(|| Error::not_found(format!("{component:?} does not exist in directory {walked:?}")))?;
            current = self
                .read_inode(entry.addr)
                .context(format!("{component:?} in directory {walked:?}"))?;
            walked.push('/');
            walked.push_str(component);
        }
        Ok(current)
    }
}

/// Opens the resource files. Their block maps can depend on each other (the sub-block file's pointer blocks live in
/// the second pointer-block file, for example), so files are opened in rounds until none is left.
fn open_resources(volume: &LogicalVolume, geo: &Geometry) -> Result<Resources> {
    let mut pending = vec![
        (SystemFile::PointerBlocks2, SFD_POINTER_BLOCKS2),
        (SystemFile::PointerBlocks, SFD_POINTER_BLOCKS),
        (SystemFile::SubBlocks, SFD_SUB_BLOCKS),
        (SystemFile::FileDescriptors, SFD_FILE_DESCRIPTORS),
    ];
    let mut resources = Resources::default();
    while !pending.is_empty() {
        let mut still_pending = Vec::new();
        for &(which, number) in &pending {
            match open_resource(volume, geo, &resources, which, number) {
                Ok(file) => *resources.slot(which) = Some(file),
                Err(MapError::Needs(_)) => still_pending.push((which, number)),
                Err(MapError::Fatal(error)) => return Err(error.context(which.name())),
            }
        }
        if still_pending.len() == pending.len() {
            let names: Vec<&str> = still_pending.iter().map(|(which, _)| which.name()).collect();
            return Err(Error::corrupt(format!(
                "the system files {} need each other to be read",
                names.join(", ")
            )));
        }
        pending = still_pending;
    }
    check_file_block_layout(volume, geo, &resources)?;
    Ok(resources)
}

fn read_system_inode(volume: &LogicalVolume, geo: &Geometry, number: u32) -> Result<Inode> {
    let offset = geo.sfd_offset(u64::from(number))?;
    let raw = read_vec(volume, offset, geo.fd_size, "system file descriptor")?;
    let inode = Inode::parse(&raw, FdAddr::system(number), offset, geo)?;
    if inode.kind != FileKind::System {
        return Err(Error::corrupt(format!(
            "descriptor {} at volume offset {offset:#x} should be a system file, but is {:?}",
            inode.addr, inode.kind
        )));
    }
    Ok(inode)
}

fn open_resource(
    volume: &LogicalVolume,
    geo: &Geometry,
    resources: &Resources,
    which: SystemFile,
    number: u32,
) -> std::result::Result<ResourceFile, MapError> {
    let inode = read_system_inode(volume, geo, number)?;
    let map = BlockMap::build(&inode, geo, &Resolver { volume, geo, resources })?;
    Ok(ResourceFile::open(which, map, volume)?)
}

/// Small file block addresses are turned into volume offsets with a formula instead of through the file-block
/// resource file. This checks the one assumption in that formula against the file's header: a cluster of small
/// file blocks is exactly one large file block.
fn check_file_block_layout(volume: &LogicalVolume, geo: &Geometry, resources: &Resources) -> Result<()> {
    let inode = read_system_inode(volume, geo, SFD_FILE_BLOCKS).context(".fbb.sf")?;
    let map = BlockMap::build(&inode, geo, &Resolver { volume, geo, resources })
        .map_err(fatal)
        .context(".fbb.sf")?;
    let mut raw = [0u8; resource::RESOURCE_HEADER_SIZE];
    map.read_metadata_at(volume, 0, &mut raw).context(".fbb.sf header")?;
    let large = resource::ResourceHeader::parse(&raw, ".fbb.sf")?;
    map.read_metadata_at(volume, u64::from(large.child_meta_offset), &mut raw)
        .context(".fbb.sf small block header")?;
    let small = resource::ResourceHeader::parse(&raw, ".fbb.sf (small file blocks)")?;
    if small.resources_per_cluster != geo.sfb_per_cluster {
        return Err(Error::unsupported(format!(
            ".fbb.sf: {} small file blocks per cluster, but a large file block holds {}; \
             this layout has not been verified",
            small.resources_per_cluster, geo.sfb_per_cluster
        )));
    }
    Ok(())
}

fn count_active_heartbeats(volume: &LogicalVolume, geo: &Geometry) -> Result<u32> {
    let slot_size = geo.md_alignment;
    let mut active = 0u32;
    for slot in 0..HEARTBEAT_SLOTS {
        let offset = HEARTBEAT_OFFSET.saturating_add(slot.saturating_mul(slot_size));
        if offset.saturating_add(4) > volume.len() {
            break;
        }
        let raw = read_vec(volume, offset, 4, "heartbeat slot")?;
        let state = View::new("heartbeat slot", offset, &raw).u32(0)?;
        if state != HEARTBEAT_FREE && state != 0 {
            active = active.saturating_add(1);
        }
    }
    Ok(active)
}

impl Inode {
    /// A stand-in used only while a volume is being opened.
    fn placeholder() -> Inode {
        Inode {
            addr: FdAddr::ROOT,
            volume_offset: 0,
            kind: FileKind::Directory,
            generation: 0,
            link_count: 0,
            length: 0,
            block_size: 0,
            num_blocks: 0,
            mtime: 0,
            ctime: 0,
            atime: 0,
            uid: 0,
            gid: 0,
            mode: 0,
            addressing: AddressingMode::FileBlocks,
            num_tbz_blocks: 0,
            block_offset_shift: 0,
            addrs: Vec::new(),
        }
    }
}

/// The content of a file as a block source, for internal readers.
#[derive(Debug)]
struct MappedFile<'a> {
    volume: &'a LogicalVolume,
    map: &'a BlockMap,
}

impl BlockSource for MappedFile<'_> {
    fn len(&self) -> u64 {
        self.map.len()
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        // Directory blocks are metadata: they must be backed by written blocks.
        self.map.read_metadata_at(self.volume, offset, buf)
    }
}

#[derive(Debug)]
struct FileInner {
    volume: Arc<VolumeInner>,
    stat: FileStat,
    map: BlockMap,
}

/// An opened file. Cheap to clone; reads can run in parallel.
///
/// The block map is resolved when the file is opened and does not change afterwards.
#[derive(Debug, Clone)]
pub struct File {
    inner: Arc<FileInner>,
}

impl File {
    /// Metadata of the file.
    pub fn stat(&self) -> &FileStat {
        &self.inner.stat
    }

    /// The block map: sorted, without gaps, covering the whole file.
    pub fn extents(&self) -> &[Extent] {
        self.inner.map.extents()
    }

    /// Reads the whole file into memory. Refuses files larger than `limit` bytes.
    pub fn read_all(&self, limit: u64) -> Result<Vec<u8>> {
        let len = self.inner.map.len();
        if len > limit {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("file is {len} bytes long, more than the {limit} bytes the caller accepts"),
            ));
        }
        let mut data = vec![0u8; to_usize(len, "file length")?];
        self.read_exact_at(0, &mut data)?;
        Ok(data)
    }
}

impl BlockSource for File {
    fn len(&self) -> u64 {
        self.inner.map.len()
    }

    /// Fills `buf` with file content. Holes and blocks that were never written read as zeros.
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.inner.map.read_exact_at(&self.inner.volume.volume, offset, buf)
    }
}
