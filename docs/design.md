# Design

## Goals

1. **Never return wrong data silently.** Refuse what has not been verified (`ErrorKind::Unsupported`), so that a
   caller can fall back to another way of reading.
2. **Embeddable.** Apache-2.0, no runtime dependencies in the core crate, no unsafe code, no panics on any input.
3. **Fast enough for backups.** Reads from many threads at once, and a list of the ranges that hold data, so that
   empty space is never read.

## Layers

Each layer turns one `BlockSource` into another (see `docs/format/00-overview.md`):

| Layer | Type | Offsets it understands |
|---|---|---|
| Device or image | `FileSource` | device |
| Partition | `SubSource` | partition |
| Logical volume | `LogicalVolume` | volume |
| File | `File` | file |
| Virtual disk | `Disk` | guest disk |

## Resolve once, then only read

Opening a volume resolves the extent runs, the filesystem descriptor and the block maps of the system files.
Opening a file resolves its complete block map: a sorted list of extents, each data, "to be zeroed" or hole. After
that nothing changes, so a read is a binary search plus one positioned read on the device, without locks or caches.

The cost is memory for very fragmented files (one extent is 32 bytes; a completely fragmented TiB needs about
32 MiB). If that becomes a problem, the map can be resolved per pointer block on first use.

System files can depend on each other. They are opened in rounds: whatever can be resolved with the files opened so
far is resolved, and a round without progress is an error. This replaces the fixed opening order that older tools
get wrong on some volumes.

## Devices

A `FileSource` reads an image file or a block device with positioned reads. Opened with `open_direct` it bypasses
the page cache (`O_DIRECT`, `FILE_FLAG_NO_BUFFERING`): every read is widened to whole 4096-byte blocks and goes
through a pooled, aligned buffer, so callers need not align anything. A datastore on several devices is opened from
all of them; the logical volume maps every volume offset to a device and an offset on it.

## Virtual disks are stacks of layers

A `Disk` is the base disk's flat file plus one layer per snapshot delta, newest first. A delta's tables are resolved
into runs when the disk is opened ("this range is in the delta at this file offset", "this range is zero"); ranges
without a run belong to the layer below. A read walks the runs of the top layer and descends only for the gaps.
`data_ranges` is computed the same way: each delta adds what it holds and removes what it declares zero.

## Datastores in use: locks and journals

An image of a datastore that hosts were using is a crash image. Two mechanisms deal with that:

- **Locks.** Parsing a file descriptor keeps the lock mode that ESXi recorded in it. Opening a virtual disk checks
  every file of its chain and refuses one that a host had open for writing (`ErrorKind::InUse`): its newest block
  allocations are not in its block map yet.
- **Journal replay**, on request. `fs/journal.rs` reads the journals that heartbeat slots point to and collects the
  transactions that wait there: lists of byte-range changes to metadata blocks. `Replayed` is a `BlockSource` that
  wraps the logical volume and applies those changes to whatever is read through it. The filesystem layer reads
  through that wrapper instead of the logical volume, and nothing else changes: block maps, directories and file
  descriptors come out as they would after ESXi's own recovery. Parsing is strict; a journal that holds anything
  unexpected fails the open.

## Errors

`Error` carries a kind and a message naming structure, field, value and offset.

| Kind | Meaning for a backup program |
|---|---|
| `Unsupported` | The data may be fine, but reading it would be a guess. Fall back. |
| `Corrupt` | The on-disk data contradicts the format. Fall back and raise an alarm. |
| `Io` | The device could not be read. |
| `NotFound`, `InvalidInput` | The request was wrong. |

## Surfaces

- **Rust crate** (`openvmfs`): the implementation. No dependencies, no `unsafe`.
- **Command line** (`openvmfs`): a thin layer over the crate, for looking at volumes and for comparing readers.
- **C interface** (`openvmfs_c`): opaque handles, explicit error objects, no global state, no panics across the
  boundary. It covers volumes and virtual disks.
- **.NET binding** (`OpenVmfs`): `VmfsVolume`, `VmdkDisk` with `Read`, `ReadAsync` and `DataExtents`, shaped so
  that a backup product's disk reader interface is a thin adapter.
- Linux x64 and Windows x64, both tested. The core crate uses positioned reads on both.

## Not goals

Writing, repairing, mounting as a filesystem, VMFS5, vSAN and vVols.
