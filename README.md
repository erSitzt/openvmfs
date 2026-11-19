# OpenVmfs

Read VMware VMFS6 datastores, and the virtual disks on them, straight from a block device or an image file. No ESXi
is involved, and nothing is ever written to the device.

OpenVmfs exists for backup and recovery software that wants to read VM disks from storage-array snapshots instead of
through the hypervisor. It is licensed under Apache-2.0, so it can be embedded in open and closed software alike.

**Status: proof of concept.** It reads real VMFS 6.82 volumes, but only the parts listed under "What works" have
been verified. Do not rely on it for production backups yet.

## What works

Verified against volumes written by ESXi 7.0.3 and ESXi 8.0 U3 (both VMFS 6.82): against reference data whose position is known,
against the hashes and block maps ESXi itself reports, and file by file against an independent reader.

- Finding the volume: bare VMFS partition or a disk with a GUID partition table.
- The logical volume manager (LVM 6) with one device.
- Files of every size class: stored inside the descriptor, in sub-blocks, in file blocks, and through one or two
  levels of pointer blocks (tested up to a 42 TiB thin disk).
- Directories, including names with non-ASCII characters; symbolic links; system files.
- Datastores of any size seen so far, including above 16 TiB (where older tools read from the wrong place), and
  datastores that span several devices.
- Unbuffered reading of LUNs (`O_DIRECT`), without the caller having to align anything.
- Snapshot chains: a disk with VM snapshots is read through its SESparse deltas down to the base, at any
  snapshot level, including ranges the guest discarded (checked against ESXi's own consolidation of each level,
  and against QEMU).
- Base disks, thin and thick: `disk.vmdk` descriptor plus `disk-flat.vmdk`, with the list of ranges that hold
  data. Blocks of a lazy-zeroed disk that were never written read as zeros, although stale data is on disk there
  (checked against datastores that were filled with junk first, and against ESXi's own hashes and block maps).
  This includes thick disks made of large file blocks (512 MiB), as ESXi creates them on datastores above
  128 GiB.

## What is refused

OpenVmfs refuses what it has not verified, with an error of kind `Unsupported`, instead of guessing. A caller that
has another way to read the data should fall back to it.

| Refused | Why |
|---|---|
| A virtual disk that an ESXi host had open for writing when the image was taken (error kind `InUse`) | Its newest writes may be missing while later ones are present |
| "To be zeroed" markers other than the one ESXi was seen to write | Never seen |
| A snapshot delta that was not closed cleanly (the active delta of a running VM) | Its journal would have to be replayed |
| Zero grains in snapshot deltas, deltas in the older COWD format, chains across datastores | Never seen, or not created by VMFS6 |
| Journals that hold anything but the transaction shape seen so far (only when replay is asked for) | Not understood |
| VMFS5, VMFS-L, VMFS 6.81, 4Kn devices | No verified sample yet |
| Extended LVM extent metadata | Never seen; datastores of 3 TiB and 17 TiB do not use it |
| Raw device mappings, encrypted disks | The data is not on the volume, or not readable |

`OpenOptions::accept_unverified` (CLI: `OPENVMFS_ACCEPT_UNVERIFIED=1`) reads the first of these anyway. It is meant
for verification work; the data may be wrong.

## A note on consistency

A snapshot of a mounted datastore is crash-consistent at best. VMFS keeps a journal that no public tool understands,
so metadata that was changing when the snapshot was taken may be read in an old state. We measured both sides of
this ([`docs/consistency.md`](docs/consistency.md)):

- The disk of a VM that was writing, with no VM snapshot: wrong in 10 of 10 runs. The newest 1 to 13 MiB of newly
  allocated data were missing while a later in-place write was present.
- The same with a VM snapshot taken first, reading the disk below it: correct in 16 of 16 runs, on ESXi 7.0.3
  and 8.0 U3.

ESXi records on disk which files a host has open for writing. OpenVmfs refuses to open a virtual disk in that
state (error kind `InUse`), so the first case is caught rather than read. Sixteen runs in one small lab are
encouraging, not a guarantee.

What such a disk is missing waits in the host's journal. As an experiment, OpenVmfs can replay the journal
(`OpenOptions::replay_journal`, CLI `OPENVMFS_REPLAY_JOURNAL=1`): five images of a writing VM that read wrong
without it read correctly with it. The format was worked out from those images
([`docs/format/60-heartbeat-journal.md`](docs/format/60-heartbeat-journal.md)); anything that looks different is
refused.
`VolumeInfo::active_heartbeats` tells how many hosts were holding locks at that moment; zero does not prove the
datastore was unmounted. Keep an independent check in the software that uses OpenVmfs.

## Command line

```
openvmfs info   /dev/mapper/snapshot-lun
openvmfs ls     /dev/mapper/snapshot-lun vm1
openvmfs vmdk info   /dev/mapper/snapshot-lun vm1/vm1.vmdk
openvmfs vmdk map    /dev/mapper/snapshot-lun vm1/vm1.vmdk
openvmfs vmdk export /dev/mapper/snapshot-lun vm1/vm1.vmdk vm1.raw
openvmfs vmdk info   /dev/mapper/snapshot-lun vm1/vm1-000002.vmdk    # the disk as of a snapshot
openvmfs info        /dev/sdc,/dev/sdd                              # a datastore on two devices
OPENVMFS_DIRECT=1 openvmfs vmdk export /dev/sdc vm1/vm1.vmdk vm1.raw  # without the page cache
```

Exit code 3 means "unsupported": the data may be fine, but the tool refuses to guess. Exit code 4 means "in use":
an ESXi host was writing to the disk when the image was taken. All commands: [`docs/guide/cli.md`](docs/guide/cli.md).

## Library

```rust
use openvmfs::{BlockSource, Volume};

let volume = Volume::open_path("/dev/mapper/snapshot-lun")?;
let disk = volume.open_disk("vm1/vm1.vmdk")?;
for range in disk.data_ranges() {
    // Everything outside these ranges reads as zeros and can be skipped.
}
let mut sector = [0u8; 512];
disk.read_exact_at(0, &mut sector)?;
```

Volumes, files and disks are cheap to clone and can be read from many threads at once. All metadata is resolved when
a file is opened; a read is a lookup plus one positioned read on the device.

The same is available from C (`openvmfs_c`, header
[`openvmfs.h`](crates/openvmfs-capi/include/openvmfs.h)) and from .NET (`bindings/dotnet`). Guides:
[Rust](docs/guide/rust.md), [C](docs/guide/c.md), [.NET](docs/guide/dotnet.md).

## Documentation

[`docs/README.md`](docs/README.md) is the map. The main entry points:

| For | Read |
|---|---|
| A first try | [Getting started](docs/guide/getting-started.md) |
| Building a backup or recovery program on OpenVmfs | [Reading virtual disks for a backup](docs/guide/backup.md) |
| Errors and refusals | [Errors and refusals](docs/guide/errors.md) |
| Understanding VMFS | [Reading a virtual disk by hand](docs/internals/walkthrough.md), then the [format description](docs/format/00-overview.md) |
| What a storage snapshot of a datastore in use holds | [Consistency](docs/consistency.md) |
| Working on OpenVmfs | [Design](docs/design.md), [testing](docs/internals/testing.md), [the lab](docs/internals/lab.md) |

## Building and testing

```
cargo build --release
fixtures/fetch-external.sh          # optional: public sample images for some of the tests
cargo test --workspace --release
```

Windows builds are cross-compiled with `tools/build-windows.sh` and tested on a Windows machine with
`tools/windows-remote-test.py`. [`docs/internals/testing.md`](docs/internals/testing.md) describes every check.

Comparing with an independent reader (needs Docker):

```
docker build -t openvmfs-oracle tools/oracle
tools/compare-with-oracle.sh target/fixture-cache/dissect/vmfs6.bin
```

## How the format knowledge was obtained

VMware publishes no VMFS specification. We read the existing open-source implementations to learn the format, wrote
it down in our own words in [`docs/format`](docs/format/00-overview.md), and wrote our own code from that. No code
was copied or translated. Where the implementations disagree, volumes written by ESXi decide.
See [`CONTRIBUTING.md`](CONTRIBUTING.md) and [`NOTICE`](NOTICE).

## Licence

Apache-2.0. See [`LICENSE`](LICENSE).
