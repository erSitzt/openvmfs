# Reading a storage snapshot of a datastore that is in use

A storage-array snapshot of a mounted datastore is crash-consistent: it is what the disk would hold after a power
failure. VMFS journals its metadata, and OpenVmfs (like every other public reader) does not replay the journal. This
note records what we measured about that risk. It is evidence from a small lab, not a proof.

## Summary

| Case | Result | What OpenVmfs does |
|---|---|---|
| VM snapshot first, storage snapshot 0.6 to about 2 s later; read the disk **below** the VM snapshot | Correct in 9 of 9 runs | Reads it |
| No VM snapshot; the VM was writing or had just written | **Wrong in 5 of 5 runs**: the newest 8 to 11 MiB of newly allocated data are missing, while a later write to an existing block is present | Refuses the disk (`InUse`) |
| The delta a running VM writes to | Shows none of the recent data in 7 of 7 runs | Refuses the disk (`InUse`, and `Unsupported` for the journal flag) |
| Files that were closed before the storage snapshot | Correct, 47 of 47 | Reads them |

The rule that follows: **a file that an ESXi host had open for writing when the storage snapshot was taken cannot
be read reliably; a file that was closed, or open read-only, can.** Taking a VM snapshot is what turns a VM's
current disk from the first kind into the second. ESXi records on disk how each file is locked, so a reader can
tell the two apart, and OpenVmfs does.

## Setup

- ESXi 7.0.3 running nested; small VMFS 6.82 datastores on its virtual disks.
- "Storage snapshot" = a snapshot of the nested host's VM taken from the outside (no memory, no quiescing), which
  freezes its virtual disks at one instant. The frozen disk was copied and downloaded **without ever showing it to
  an ESXi host**, so no journal was replayed on it.
- OpenVmfs read the frozen copy. References: what the workload is known to have written, the order in which it
  wrote, and ESXi's own reading of the same files later.

The recipes are `fixtures/recipes/consistency_probe.py` and `consistency_marker.py`; the records are in
`fixtures/vmfs6/probe-series1/` and `fixtures/vmfs6/fixture-inuse-v1/truth/`.

## Files finished shortly before the snapshot

A script on the ESXi shell finished one file after another, about ten per second: create, extend to 16 MiB, write
5 MiB of pattern data, `fsync`, close. The snapshot was taken while it ran.

| | |
|---|---|
| Files present on the frozen copy | 47 (`c000` to `c046`) |
| Files identical to what was written | 47 |
| Newest file on the copy | closed about 0.1 s before the snapshot instant |
| Next file (`c047`) | absent: it was created after the snapshot |

## The backup order: VM snapshot, then storage snapshot

A guest copied data to a thin disk in 256 KiB writes with `fsync`, about 9 MiB/s, so that new blocks were allocated
all the time. While it wrote:

1. a VM snapshot was taken. From then on ESXi writes to a new SESparse delta; what was the current disk until then
   is closed for writing.
2. as soon as that returned, the storage snapshot was taken.

Seven runs in a row on one VM, so the disk frozen in run *n* is the base plus *n* − 1 deltas. Afterwards the VM was
powered off and ESXi consolidated every level into a clone (`vmkfstools -i`).

| Run | Guest had been writing for | Storage snapshot after the VM snapshot | Disk below the VM snapshot, read from the frozen copy |
|---|---|---|---|
| 1 | 0.9 s | 0.6 s | identical to ESXi's clone, writes in order |
| 2 | 1.9 s | 0.9 s | identical, in order |
| 3 | 3.0 s | 0.8 s | identical, in order |
| 4 | 4.0 s | 0.8 s | identical, in order |
| 5 | 1.0 s | 0.9 s | identical, in order |
| 6 | 2.3 s | 1.3 s | identical, in order |
| 7 | 3.0 s | 0.9 s | identical, in order |

"In order" means: the guest wrote sequentially and waited for each write, and the frozen disk held an unbroken
prefix of the copy, with at most one partly written MiB at its end.

One run before this series (1 MiB writes as fast as the guest could) gave the same result, and so did one run with
the mixed workload described below: 9 of 9.

In all seven runs the delta that the VM was writing to at that moment looked like this on the frozen copy: its
"replay journal" flag set, its file as short as an empty delta, and none of the data in its tables that the guest
had written to it in the 0.6 to 1.3 s since it was created. It cannot be read, and a backup does not need it.

## No VM snapshot: the disk of a writing VM

The workload that exposes the problem mixes two kinds of writes, each followed by `fsync`:

- chunk *i*: 256 KiB into a part of a thin disk that was never written, so VMFS must allocate a new block;
- the marker: sector 0, which exists already, is overwritten in place with the number *i*.

The guest writes chunk *i* before marker *i*. A crash-consistent image whose marker says *K* therefore holds chunks
1 to *K*. The storage snapshot was taken while the guest wrote, or shortly after it had finished, with no VM
snapshot.

Each run is a burst of 300 chunk-and-marker pairs (100 in run 5), which takes the guest two to three seconds.

| Run | Storage snapshot after the burst started | Guest at that moment | Marker on the frozen copy | Chunks present | Missing |
|---|---|---|---|---|---|
| 1 | 1.7 s | still writing | 168 | 1 to 127 | 128 to 168: 10.25 MiB |
| 2 | 2.7 s | burst finished | 600 | 1 to 559 | 560 to 600: 10.25 MiB |
| 3 | 3.7 s | burst finished | 900 | 1 to 859 | 860 to 900: 10.25 MiB |
| 4 | 4.7 s | burst finished | 1200 | 1 to 1159 | 1160 to 1200: 10.25 MiB |
| 5 | about 5 s | burst finished | 1300 | 1 to 1267 | 1268 to 1300: 8.25 MiB |

In every run the image shows a later write (the marker) without earlier ones (the newest chunks): a state that no
power failure of the guest's disk could produce. A filesystem or database in the guest that trusts `fsync` can be
damaged by exactly this, for example a journal commit that refers to blocks which read as zeros.

What the frozen copy looks like there: the missing megabytes are **unallocated** in the file's block map. The
allocation of the newest blocks had not reached the place where the block map lives. An image taken after the guest
had been idle for several minutes was complete, so the update does arrive on its own, later than a few seconds
after the last write; we did not measure when. ESXi itself is not affected: it has the newer state in memory and, after a crash, in the journal.

The same workload with a VM snapshot taken in the middle of it, and the storage snapshot about a second later, gave
a complete disk below the VM snapshot: marker 1401, chunks 1 to 1401 present (`fixture-inuse-v1`).

## How a reader can tell: locks

Every file descriptor starts with a lock record that ESXi writes in place, because other hosts must see it. On the
frozen copies:

| File | Lock on disk |
|---|---|
| Current disk of a running VM (flat file, or the newest delta) | exclusive, with the id of the host |
| Disk below a VM snapshot of a running VM | read-only |
| Disks of a powered-off VM, closed files | free |

OpenVmfs refuses to open a virtual disk if its flat file or any delta of its chain was locked for writing, with an
error of kind `InUse`. `OpenOptions::accept_in_use` (CLI: `OPENVMFS_ACCEPT_IN_USE=1`) overrides that for looking at
data that is known to be damaged anyway. `FileStat::lock` shows the lock of any file.

A lock can be stale: after a host crash a file stays locked until another host breaks the lock. Such a file was
open for writing when the host died, so refusing it is still right.

## What this does not show

- One small lab: one host, datastores of a few GiB, ESXi 7.0.3, 15 runs in total. Rare timing windows need far
  more runs to show up.
- Shared datastores with several hosts, files that switch addressing mode at that moment, large file blocks, and
  other ESXi versions were not exercised.
- Why the closed disk is complete: we assume that closing a file for writing makes VMFS bring its metadata to its
  place before the VM snapshot returns. We have measured the result, not read the mechanism. The shortest gap
  between the two snapshots in our runs was 0.6 s.
- Replaying the VMFS journal would remove the restriction. Nobody has published its format; we have not analysed
  it.

So a backup program should take the VM snapshot first, leave a short pause before the storage snapshot, read only
disks below that VM snapshot, treat `InUse` as "do not back this up from the storage snapshot", and keep an
independent check, for example comparing blocks that changed-block tracking reports as unchanged with the previous
backup.

## A hazard found on the way

Attaching the copy to the same ESXi host that had the original datastore mounted went wrong: virtual disks without
unique device ids look identical to ESXi, which switched the volume to the copy and unmounted the datastore when
the copy was removed. Nothing was written to the copy (its hash was unchanged), and a rescan brought the datastore
back. Lesson: never present an identical copy of a mounted datastore to that host. On real arrays the LUN ids
differ and ESXi marks the copy as a snapshot volume instead; a Linux proxy reading the LUN is not affected at all.

## Also learned

`VolumeInfo::active_heartbeats` was 1 on the copies taken while a VM was running, and 0 on a copy taken while only
a script on the ESXi shell was writing. It shows hosts that hold long-lived locks, not every kind of activity.
