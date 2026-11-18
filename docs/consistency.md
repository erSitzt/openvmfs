# Reading a storage snapshot of a datastore that is in use

A storage-array snapshot of a mounted datastore is crash-consistent: it is what the disk would hold after a power
failure. VMFS journals its metadata, and OpenVmfs (like every other public reader) does not replay the journal. This
note records what we measured about that risk. It is evidence from a small lab, not a proof.

## Setup

- ESXi 7.0.3 running nested; a 4 GiB VMFS 6.82 datastore on one of its virtual disks.
- "Storage snapshot" = a snapshot of the nested host's VM taken from the outside (no memory, no quiescing), which
  freezes its virtual disks at one instant. The frozen disk was copied and downloaded **without ever showing it to
  an ESXi host**, so no journal was replayed on it.
- OpenVmfs read the frozen copy. The reference was the content the workload is known to have written, and ESXi's
  own reading of the same files later.

## Probe 1: files finished shortly before the snapshot

A script on the ESXi shell finished one file after another, about ten per second: create, extend to 16 MiB, write
5 MiB of pattern data, `fsync`, close. The snapshot was taken while it ran.

| | |
|---|---|
| Files present on the frozen copy | 47 (`c000` to `c046`) |
| Files identical to what was written | 47 |
| Newest file on the copy | closed about 0.1 s before the snapshot instant |
| Next file (`c047`) | absent: it was created after the snapshot |

Every file whose close had returned before the snapshot was complete and correct on the frozen copy, without journal
replay.

## Probe 2: the backup order (VM snapshot, then storage snapshot)

A guest wrote 1 MiB blocks with `fsync` to a thin disk as fast as it could. While it wrote:

1. a VM snapshot was taken (0.6 s). From then on ESXi writes to a new SESparse delta; the base disk is frozen.
2. immediately afterwards the storage snapshot was taken (0.6 s).

The guest kept writing throughout. Afterwards the VM was powered off and ESXi hashed the base disks, which had not
changed since step 1.

| Disk | Read by OpenVmfs from the frozen copy | Read by ESXi |
|---|---|---|
| `grow` (thin, 156 MiB written when frozen, written until the last moment) | `7db8e456…` | `7db8e456…` identical |
| `thin-g`, `lazy-g`, `src` (not written during the probe) | | identical |

The block map was the same too: 156 MiB of data in two pieces, the rest unallocated. The active delta of `grow` was
6 MiB long on the frozen copy and 237 MiB when the guest stopped: it was in flux, as expected, and a backup does not
read it.

## What this supports

With the order "VM snapshot, then storage snapshot", the files a backup reads (base disks and older deltas) had
their metadata on disk when the storage snapshot was taken, 0.6 s after the VM snapshot, under continuous
allocation right up to it.

## What it does not show

- Two runs on one small datastore with one host. Rare timing windows need many runs to show up.
- Shared datastores with several hosts, larger files that switch addressing mode at that moment, large file blocks,
  and other ESXi versions were not exercised.
- A disk that was being written when the storage snapshot was taken (no VM snapshot, or the active delta): we did
  not compare such a file with ESXi's view after journal replay. Treat it as unreliable.

So a backup program should keep an independent check, for example comparing blocks that changed-block tracking
reports as unchanged with the previous backup, and should leave a short pause between the two snapshots.

## A hazard found on the way

Attaching the copy to the same ESXi host that had the original datastore mounted went wrong: virtual disks without
unique device ids look identical to ESXi, which switched the volume to the copy and unmounted the datastore when
the copy was removed. Nothing was written to the copy (its hash was unchanged), and a rescan brought the datastore
back. Lesson: never present an identical copy of a mounted datastore to that host. On real arrays the LUN ids
differ and ESXi marks the copy as a snapshot volume instead; a Linux proxy reading the LUN is not affected at all.

## Also learned

`VolumeInfo::active_heartbeats` was 1 on the copies taken while a VM was running, and 0 on a copy taken while only
a script on the ESXi shell was writing. It shows hosts that hold long-lived locks, not every kind of activity.
