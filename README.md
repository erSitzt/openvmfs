# OpenVmfs

Read VMware VMFS6 datastores, and the virtual disks on them, straight from a block device or an image file. No ESXi
is involved, and nothing is ever written to the device.

OpenVmfs exists for backup and recovery software that wants to read VM disks from storage-array snapshots instead of
through the hypervisor. It is licensed under Apache-2.0, so it can be embedded in open and closed software alike.

**Status: proof of concept.** It reads real VMFS 6.82 volumes, but only the parts listed under "What works" have
been verified. Do not rely on it for production backups yet.

## What works

Verified against volumes written by ESXi 7.0.3 (VMFS 6.82): against reference data whose position is known,
against the hashes and block maps ESXi itself reports, and file by file against an independent reader.

- Finding the volume: bare VMFS partition or a disk with a GUID partition table.
- The logical volume manager (LVM 6) with one device.
- Files of every size class: stored inside the descriptor, in sub-blocks, in file blocks, and through one or two
  levels of pointer blocks (tested up to a 42 TiB thin disk).
- Directories, including names with non-ASCII characters; symbolic links; system files.
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
| "To be zeroed" markers other than the one ESXi was seen to write | Never seen |
| A snapshot delta that was not closed cleanly (the active delta of a running VM) | Its journal would have to be replayed |
| Zero grains in snapshot deltas, deltas in the older COWD format, chains across datastores | Never seen, or not created by VMFS6 |
| Volumes spanning several devices, volumes over 16 TiB | Planned |
| VMFS5, VMFS-L, VMFS 6.81, 4Kn devices | No verified sample yet |
| Raw device mappings, encrypted disks | The data is not on the volume, or not readable |

`OpenOptions::accept_unverified` (CLI: `OPENVMFS_ACCEPT_UNVERIFIED=1`) reads the first of these anyway. It is meant
for verification work; the data may be wrong.

## A note on consistency

A snapshot of a mounted datastore is crash-consistent at best. VMFS keeps a journal that no public tool understands,
so metadata that was changing when the snapshot was taken may be read in an old or half-updated state. In two lab
probes, disks frozen by a VM snapshot 0.6 s before the storage snapshot read identical to ESXi
([`docs/consistency.md`](docs/consistency.md)); that is encouraging, not a guarantee.
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
```

Exit code 3 means "unsupported": the data may be fine, but the tool refuses to guess.

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

A C interface and a .NET binding are planned (see `docs/design.md`).

## Building and testing

```
cargo build --release
cargo test --workspace
fixtures/fetch-external.sh          # optional: public sample images for the integration tests
cargo test -p openvmfs-testkit
```

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
