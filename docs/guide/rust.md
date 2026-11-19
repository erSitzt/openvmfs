# Using the Rust library

The crate is `openvmfs`. It has no dependencies, forbids `unsafe` and never writes to a device. The API reference is
`cargo doc --no-deps --open`; this page shows how the pieces fit. Two complete programs are in
[`crates/openvmfs/examples`](../../crates/openvmfs/examples): `inspect` lists a datastore and its disks, `export`
copies a disk to a raw file.

## The objects

```
device or image ──► Volume ──► File      a file of the datastore
                          └──► Disk      a virtual disk: what a guest sees
```

- A [`Volume`] is an opened datastore. Opening it reads the partition table, the logical volume and the filesystem's
  own files once.
- A [`File`] is one file with its block map resolved. A [`Disk`] is a virtual disk: its descriptor, its flat file
  and, for a disk with snapshots, its deltas.
- All three are cheap to clone and can be used from many threads at once. A `File` or `Disk` stays valid after its
  `Volume` is dropped.
- `File` and `Disk` implement [`BlockSource`]: `len()` and `read_exact_at(offset, buffer)`. Reads never return
  short; they fill the buffer or fail.

## Opening a volume

```rust
use openvmfs::{OpenOptions, Volume};

// One device: a LUN with a partition table, a partition, or an image file.
let volume = Volume::open_path("/dev/mapper/snapshot-lun")?;

// A datastore on several devices, read past the page cache.
let options = OpenOptions { direct: true, ..OpenOptions::default() };
let volume = Volume::open_paths(&["/dev/sdc", "/dev/sdd"], &options)?;
```

| Option | Meaning |
|---|---|
| `partition` | Open this partition number instead of detecting the VMFS partition |
| `direct` | Open the devices with `O_DIRECT` / `FILE_FLAG_NO_BUFFERING`. You need not align anything |
| `replay_journal` | Experimental: read the volume with its journals replayed |
| `accept_in_use` | Open disks although a host was writing to them |
| `accept_unverified` | Read structures that were never verified against ESXi |

Your own storage works too: implement [`BlockSource`] for it and pass it to `Volume::open` or
`Volume::open_devices`. The trait has two methods and must be callable from many threads. `MemorySource` and
`SubSource` (a window into another source) are provided.

## Looking around

```rust
let info = volume.info();
println!("{} ({}), {} bytes", info.label, info.uuid, info.capacity);

for entry in volume.read_dir("vm1")? {
    println!("{:?} {}", entry.kind, entry.name);
}

let stat = volume.stat("vm1/vm1-flat.vmdk")?;
println!("{} bytes, lock {:?}", stat.len, stat.lock);

let config = volume.open_file("vm1/vm1.vmx")?.read_all(1 << 20)?;   // at most 1 MiB
```

Paths are relative to the datastore root and use `/`. `read_all` takes a limit so that a damaged length field
cannot make you allocate gigabytes.

## Reading a virtual disk

```rust
use openvmfs::BlockSource;

let disk = volume.open_disk("vm1/vm1.vmdk")?;       // the descriptor, not the -flat file
let mut buffer = vec![0u8; 4 << 20];
for range in disk.data_ranges() {
    // Read range.start..range.end in pieces; everything outside the ranges is zeros.
}
disk.read_exact_at(0, &mut buffer[..512])?;
```

- `capacity()` is the size of the disk.
- `data_ranges()` lists where the disk holds data, sorted and merged. Everything else reads as zeros and can be
  skipped. The ranges are as fine as the format allows: 1 MiB for a base disk, 4 KiB where a snapshot delta is
  involved.
- `chain()` lists the snapshot chain, newest link first. For a disk with snapshots, pass the descriptor of the
  level you want; the levels below it are read through.
- `extents()` is the block map of the base disk's flat file, with volume offsets.
- `map(range, limit)` says where the bytes of a part of the disk are stored: a list of [`MappedRange`]s, each
  either zeros or a device and an offset on it, through all snapshot deltas. With it you can read the data from the
  devices yourself and leave only the metadata to the library. At most `limit` ranges per call; continue from
  where they end.

## Errors

Every error has a kind and a message that names the structure, the field and the offset involved.

```rust
use openvmfs::ErrorKind;

match volume.open_disk(path) {
    Ok(disk) => { /* read it */ }
    Err(e) if e.kind() == ErrorKind::Unsupported => { /* may be fine, but refused: read it another way */ }
    Err(e) if e.kind() == ErrorKind::InUse => { /* a host was writing to it: snapshot the VM first */ }
    Err(e) => return Err(e.into()),
}
```

| Kind | Meaning | What to do |
|---|---|---|
| `Io` | The device could not be read | Retry or give up; the message carries the operating system's error |
| `Corrupt` | The data contradicts the format | Do not trust anything read from this object |
| `Unsupported` | Not verified, refused | Fall back to another way of reading |
| `InUse` | A host had the disk open for writing | Take a VM snapshot before the storage snapshot |
| `NotFound` | No such file, directory or volume | |
| `InvalidInput` | A wrong argument: a read past the end, a device missing from a spanned datastore | |

`ErrorKind` is `#[non_exhaustive]`: keep a catch-all arm. [errors.md](errors.md) lists the refusals one by one.

## What the library guarantees, and what not

- **No panics on any input.** Arithmetic is checked, slices are bounds-checked, and tests feed it thousands of
  damaged images. A panic is a bug.
- **No reads outside the device** and no unbounded allocations, whatever the metadata says.
- **Refusal instead of guessing.** A layout that was not verified against ESXi gives `Unsupported`.
- **Not guaranteed: detecting damage.** VMFS metadata has no checksums that we know of. A flipped bit in a block
  address is read as a different, valid address.

## Building blocks below the filesystem

`openvmfs::gpt::read_partitions`, `openvmfs::lvm::LogicalVolume` and `Volume::logical_volume()` (the volume as a
`BlockSource`) are public, for tools that want to look at a volume at a lower level. `Volume::device_location`
translates a volume offset into a device and an offset on it, and `Volume::read_descriptor` returns the raw file
descriptor of any file.

[`Volume`]: ../../crates/openvmfs/src/fs/mod.rs
[`File`]: ../../crates/openvmfs/src/fs/mod.rs
[`Disk`]: ../../crates/openvmfs/src/vmdk/mod.rs
[`BlockSource`]: ../../crates/openvmfs/src/io.rs
[`MappedRange`]: ../../crates/openvmfs/src/vmdk/mod.rs
