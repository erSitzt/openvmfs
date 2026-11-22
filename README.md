# OpenVmfs

> [!WARNING]
> **Experimental.** OpenVmfs was written without a format specification (VMware publishes none). It has been
> tested in one small lab (two ESXi versions running nested, on virtual disks) and, so far, on one production
> datastore on one storage array ([#1](https://github.com/erSitzt/openvmfs/issues/1)). It never writes to a device,
> so it cannot damage a datastore, but data it returns may be wrong on layouts nobody has tested. Do not make it
> your only backup path: keep a second way to read the data, and compare the two from time to time. The tables
> under [What has been tested](#what-has-been-tested) and
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

Everything in these tables is checked by the test suite against data captured from ESXi
([`fixtures/vmfs6/README.md`](fixtures/vmfs6/README.md)), or was measured in the lab and written down
([`docs/consistency.md`](docs/consistency.md), [`docs/guide/backup.md`](docs/guide/backup.md)).

**By ESXi version.** ✓ means: read by OpenVmfs and equal to what ESXi reports. "not run" means that nobody
tried, not that it fails. Both versions format VMFS 6.82. Any other ESXi version would be a new column: none has
been tested.

| What was read | ESXi 7.0.3 | ESXi 8.0 U3 | Evidence |
|---|:---:|:---:|---|
| Small files, directories, symbolic links | ✓ | ✓ | `fixture-zero-v1`, `fixture-basic-esxi8-v1` |
| Thin disks, with holes | ✓ | ✓ | same |
| Lazy-zeroed thick disks over stale data, eager-zeroed disks | ✓ | ✓ | same |
| Thick disks made of large file blocks (512 MiB) | ✓ | ✓ | `fixture-lfb-v1`, `fixture-big-esxi8-v1` |
| Disks over 2.5 TiB (two levels of pointer blocks, up to 42 TiB thin) | ✓ | ✓ | `fixture-double-v1`; on 8.0 U3 read once, not kept as a fixture |
| Disks written by a Linux guest (paravirtual SCSI) | ✓ | ✓ | `fixture-guest-v1`, `fixture-chain-esxi8-v1` |
| Thin disks filled by a guest front to back and at random | ✓ | ✓ | measured, [`docs/guide/backup.md`](docs/guide/backup.md): block maps identical to ESXi's |
| Snapshot chain of three deltas with guest discards | ✓ | ✓ | `fixture-chain-v2`, `fixture-chain-esxi8-v1` |
| Chain of eleven deltas, snapshots deleted in the middle while the VM ran (deltas merged, zero grains) | ✓ | ✓ | `fixture-merge-esxi8-v1`; on 7.0.3 read once, not kept as a fixture |
| Chain of 31 deltas, the deepest ESXi allows | not run | ✓ | measured: top and middle level equal ESXi's consolidation |
| Multi-writer disk: read while no VM has it open, refused (`InUse`) while one does | not run | ✓ | measured |
| Disks on LSI Logic SAS, SATA and NVMe controllers, and a disk with changed-block tracking, each with that chain | ✓ | ✓ | measured: 20 reads (7.0.3) and 25 reads (8.0 U3) at four or five levels equal ESXi's consolidation |
| Snapshot delta of a million random 4 KiB writes | ✓ | ✓ | measured, same guide: SHA-256 equal to ESXi's consolidation |
| Datastore above 16 TiB | ✓ 17 TiB | ✓ 40 TiB | `fixture-big-v1`, `fixture-big-esxi8-v1` |
| Datastore on two devices | ✓ | ✓ | `fixture-span-v1`, `fixture-span-esxi8-v1` |
| Datastore on three devices, the first one enlarged and grown in place afterwards | not run | ✓ | `fixture-span3-esxi8-v1` |
| Datastore on one device that was enlarged and grown in place | not run | ✓ | `fixture-grow-esxi8-v1` |
| More than 100,000 small files on one datastore | ✓ 132,000 | ✓ 100,697 | `fixture-pb2-v1`, `fixture-many-esxi8-v1`, `fixture-pb2-esxi8-v1` |
| More than 131,072 sub-blocks | ✓ | ✓ | `fixture-pb2-v1`, `fixture-pb2-esxi8-v1` |
| Disk below a VM snapshot, on a storage snapshot of the running VM | ✓ 9 of 9 | ✓ 7 of 7 | [`docs/consistency.md`](docs/consistency.md) |
| Disk of a writing VM without a VM snapshot: incomplete on the storage snapshot, so OpenVmfs refuses it | 10 of 10 incomplete | 5 of 5 incomplete | same |
| The same disk with the journal replayed (experimental) | ✓ 5 of 5 | ✓ 5 of 5 | same, `fixture-journal-v1` |

**Everything else was the same in all lab tests:**

| Area | Tested with |
|---|---|
| Where ESXi ran | Nested, as a virtual machine; one host per datastore |
| Devices under the datastore | Virtual disks with 512-byte sectors; images of them, read from files, buffered and unbuffered |
| Reference for "correct" | Hashes and block maps printed by ESXi, self-describing test sectors, and an independent reader (dissect.vmfs) file by file |
| Where OpenVmfs ran | Linux x64 and Windows x64 (every push runs the Rust tests on both); the C interface and the .NET 10 binding on Linux on every push, on Windows with `tools/windows-remote-test.py` |
| Raw disks on Windows | Opening `\\.\PhysicalDriveN` and reading it unbuffered works; a datastore has not yet been read that way |

**Field reports.** Runs outside our lab, by people who use OpenVmfs:

| Report | Setup | Result |
|---|---|---|
| [#1](https://github.com/erSitzt/openvmfs/issues/1) | ESXi 7.0.3 (build 24784741), two hosts sharing a 1 TiB datastore with 17 running VMs; read-only array snapshot over iSCSI, 512-byte sectors, presented to Windows Server 2022 with MPIO; OpenVmfs 0.3.0 and 0.4.0 through the .NET binding, the device read by the caller's own code | 5 backup runs of a Linux guest (paravirtual SCSI) and 1 of a Windows guest (LSI Logic SAS), thin disks with changed-block tracking, VM snapshot first: every compared region equal to a read through ESXi, files written in the guests read back with the same SHA-256, nothing refused |

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

| Not tested in our lab | What a report would show | Reports so far |
|---|---|---|
| Any ESXi version other than 7.0.3 and 8.0 U3: 6.7, earlier updates of 7.0 and 8.0, ESXi 9 | A new column in the matrix above. VMFS 6.81 is refused today for lack of a sample | Another build of 7.0.3 (#1); no other version |
| Datastores that were upgraded with their hosts over the years | Whether older formatting leaves layouts a fresh datastore does not have | none |
| Real storage arrays: Fibre Channel, iSCSI, NVMe over fabrics, any vendor | That an array snapshot presented to another machine reads like the images in our lab | One iSCSI array (#1). Fibre Channel, NVMe and other arrays: none |
| Devices with 4096-byte sectors (4Kn; 512e should behave like 512n) | 4Kn is refused today; `openvmfs info` output of such a datastore is the first step | none |
| Datastores shared by a cluster of hosts, with many running VMs | Lock states and journals of several hosts at once; our datastores had one host | Two hosts, 17 running VMs (#1) |
| Long-lived, fragmented, nearly full datastores | Block maps and resource pools that a lab does not produce in a week | none |
| Datastores extended over more than three devices, or grown in place several times | Extent tables beyond the ones we saw | none |
| Windows guests | Whether they leave anything different in thin disks and deltas | Windows Server 2022, one run (#1) |
| Snapshot chains made by backup products | SESparse features ESXi never wrote for us. Deleting snapshots in our lab produced one we had refused until then (zero grains) | none |
| Multi-writer disks shared by several VMs, linked clones, Horizon and vSphere Replication disks | Whether they open, and which refusal they hit | none |
| Linux multipath devices, Windows with MPIO, arm64 | Opening and unbuffered reading on those device types | Windows with MPIO, read by the caller's own device code (#1) |
| The backup order under real load, on real arrays | More runs than our 16, and with shorter or longer gaps between VM snapshot and storage snapshot | 5 runs on one array, all equal (#1) |

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
| Deltas in the older COWD format, chains across datastores | Not created by VMFS6, or the parent is not on the volume |
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
