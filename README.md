# OpenVmfs

> [!WARNING]
> **Experimental.** OpenVmfs was written without a format specification (VMware publishes none) and has been
> tested in one small lab: two ESXi versions running nested, on virtual disks. It has not yet read a datastore on a
> real storage array. It never writes to a device, so it cannot damage a datastore, but data it returns may be
> wrong on layouts nobody has tested. Do not make it your only backup path: keep a second way to read the data,
> and compare the two from time to time. The tables under [What has been tested](#what-has-been-tested) and
> [Tests we could not run](#tests-we-could-not-run-reports-welcome) say where the line is today.

Read VMware VMFS6 datastores, and the virtual disks on them, straight from a block device or an image file. No ESXi
is involved, and nothing is ever written to the device.

OpenVmfs exists for backup and recovery software that wants to read VM disks from storage-array snapshots instead of
through the hypervisor. It is licensed under Apache-2.0, so it can be embedded in open and closed software alike.

## What works

Verified against volumes written by ESXi 7.0.3 and ESXi 8.0 U3 (both VMFS 6.82): against reference data whose
position is known,
against the hashes and block maps ESXi itself reports, and file by file against an independent reader.

- Finding the volume: bare VMFS partition or a disk with a GUID partition table.
- The logical volume manager (LVM 6), with one device or several.
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

## What has been tested

Everything in this table is checked by the test suite against data captured from ESXi
([`fixtures/vmfs6/README.md`](fixtures/vmfs6/README.md)), or was measured in the lab and written down
([`docs/consistency.md`](docs/consistency.md), [`docs/guide/backup.md`](docs/guide/backup.md)).

| Area | Tested with |
|---|---|
| ESXi version | 7.0.3 and 8.0 U3, both formatting VMFS 6.82 |
| Where ESXi ran | Nested, as a virtual machine; one host per datastore |
| Devices under the datastore | Virtual disks with 512-byte sectors; images of them, read from files, buffered and unbuffered |
| Datastore size | 4 GiB to 40 TiB on one device (one to three extents), and one datastore on two devices |
| Number of files | 100,000 small files on one datastore; sub-block pool grown past 131,072 sub-blocks |
| Base disks | Thin up to 42 TiB (two levels of pointer blocks); thick lazy-zeroed and eager-zeroed, with small and with large file blocks; thin disks filled by a guest front to back and at random |
| Who wrote the data | The ESXi shell, `vmkfstools`, and a Linux guest through a paravirtual SCSI controller |
| Snapshot chains | Up to three SESparse deltas over a base, with guest discards in between; a delta of a million random 4 KiB writes. Compared with ESXi's own consolidation and with QEMU |
| Datastore in use | Storage snapshot of a running VM after a VM snapshot: correct in 16 of 16 runs. Without a VM snapshot: refused; with journal replay, correct for 10 of 10 images |
| Reference for "correct" | Hashes and block maps printed by ESXi, self-describing test sectors, and an independent reader (dissect.vmfs) file by file |
| Where OpenVmfs ran | Linux x64 and Windows x64 (every push runs the Rust tests on both); the C interface and the .NET 10 binding on both as well, the latter on Windows by hand before a release |
| Raw disks on Windows | Opening `\\.\PhysicalDriveN` and reading it unbuffered works; a datastore has not yet been read that way |

## Tests we could not run: reports welcome

We have one lab. If you have access to something in this table, a run of the read-only command line tool on a
storage snapshot tells a lot, and takes minutes:

```
openvmfs info      DEVICE                    # does the datastore open?
openvmfs ls        DEVICE vm1                # is the directory what ESXi shows?
openvmfs vmdk info DEVICE vm1/vm1.vmdk       # does the disk open, or why is it refused?
openvmfs vmdk hash DEVICE vm1/vm1.vmdk       # SHA-256 of the disk
```

For a powered-off VM without snapshots, the hash must equal `sha256sum vm1-flat.vmdk` on the ESXi shell. Please
open an issue with the outputs, the ESXi version and build, the storage (model, protocol) and the operating
system OpenVmfs ran on. A run in which everything matches is as useful to us as a failure. Do not post VM names or
other data you would not publish; the messages of `openvmfs` contain file names from the datastore.

| Not tested yet | What a report would show |
|---|---|
| ESXi 6.7, 7.0 before U3, 8.0 before U3, ESXi 9 | Whether their datastores look the same. VMFS 6.81 is refused today for lack of a sample |
| Datastores that were upgraded with their hosts over the years | Whether older formatting leaves layouts a fresh datastore does not have |
| Real storage arrays: Fibre Channel, iSCSI, NVMe over fabrics, any vendor | That an array snapshot presented to another machine reads like the images in our lab |
| Devices with 4096-byte sectors (4Kn; 512e should behave like 512n) | 4Kn is refused today; `openvmfs info` output of such a datastore is the first step |
| Datastores shared by a cluster of hosts, with many running VMs | Lock states and journals of several hosts at once; our datastores had one host |
| Long-lived, fragmented, nearly full datastores | Block maps and resource pools that a lab does not produce in a week |
| Datastores extended over more than two devices, or grown in place | Extent tables beyond the ones we saw |
| Windows guests, guests with other virtual controllers (LSI, NVMe, SATA) | Whether they leave anything different in thin disks and deltas |
| Snapshot chains made by backup products, long chains, deltas after a consolidation | SESparse features we refuse because ESXi never wrote them for us (zero grains) |
| Disks with changed-block tracking, multi-writer disks, linked clones, Horizon and vSphere Replication disks | Whether they open, and which refusal they hit |
| Linux multipath devices, Windows with MPIO, arm64 | Opening and unbuffered reading on those device types |
| The backup order under real load, on real arrays | More runs than our 16, and with shorter or longer gaps between VM snapshot and storage snapshot |

If you can reproduce a layout on a small test datastore, [`docs/internals/lab.md`](docs/internals/lab.md) describes
how we capture one as a test fixture; that is what turns a refusal into support.

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
| Extended LVM extent metadata | Never seen; datastores of 3 TiB, 17 TiB and 40 TiB do not use it |
| Raw device mappings, encrypted disks | The data is not on the volume, or not readable |

`OpenOptions::accept_unverified` (CLI: `OPENVMFS_ACCEPT_UNVERIFIED=1`) reads the first of these anyway. It is meant
for verification work; the data may be wrong.

## A note on consistency

A snapshot of a mounted datastore is crash-consistent at best. VMFS keeps a journal that no public tool understands,
so metadata that was changing when the snapshot was taken may be read in an old state. We measured both sides of
this ([`docs/consistency.md`](docs/consistency.md)):

- The disk of a VM that was writing, with no VM snapshot: wrong in 15 of 15 runs. The newest 1 to 13 MiB of newly
  allocated data were missing while a later in-place write was present.
- The same with a VM snapshot taken first, reading the disk below it: correct in 16 of 16 runs, on ESXi 7.0.3
  and 8.0 U3.

ESXi records on disk which files a host has open for writing. OpenVmfs refuses to open a virtual disk in that
state (error kind `InUse`), so the first case is caught rather than read. Sixteen runs in one small lab are
encouraging, not a guarantee.

What such a disk is missing waits in the host's journal. As an experiment, OpenVmfs can replay the journal
(`OpenOptions::replay_journal`, CLI `OPENVMFS_REPLAY_JOURNAL=1`): ten images of a writing VM (ESXi 7.0.3 and
8.0 U3) that read wrong without it read correctly with it. The format was worked out from those images
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
