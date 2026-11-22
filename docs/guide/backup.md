# Reading virtual disks for a backup

This page is for people who build backup or recovery software on OpenVmfs: what to snapshot in which order, which
file to open, what the errors mean for the job, and what OpenVmfs does not do for you.

## The idea

A backup that reads through the hypervisor (NBD, HotAdd) loads the ESXi host and its network. Reading the datastore
from a storage-array snapshot does not: the array presents a frozen copy of the LUN to the backup machine, and that
machine reads the virtual disks out of it. OpenVmfs is the part that understands what is on the LUN.

## The order of things

```
1. VM snapshot            (vSphere; without memory; quiesced if you want application consistency)
2. short pause
3. storage snapshot       (on the array, of every LUN the datastore lives on)
4. present the storage snapshot to the backup machine, read-only
5. read the disks BELOW the VM snapshot with OpenVmfs
6. remove the storage snapshot's presentation, then the storage snapshot
7. remove the VM snapshot
```

**Step 1 is not optional.** A virtual disk that ESXi has open for writing has its newest block allocations in the
host's memory and journal, not in the block map on disk. Read from a storage snapshot, such a disk lacks data that
the guest was told is safely written, while later writes to existing blocks are present. In our lab that happened
in 15 of 15 attempts. Taking the VM snapshot closes the disk for writing; from then on ESXi writes to a new delta
file, and the closed disk read correctly in 9 of 9 attempts. [../consistency.md](../consistency.md) has the
measurements and their limits.

OpenVmfs enforces this: it reads the lock state that ESXi records in every file and refuses a disk that was open
for writing, with the error kind `InUse`.

**Step 2:** the shortest gap between the two snapshots in our runs was 0.6 seconds. We have no run in which a
shorter gap failed, and none that proves it safe. A few seconds cost nothing.

**Step 3:** if the datastore spans several LUNs, snapshot them at the same instant (a consistency group on the
array), and give OpenVmfs all of them.

**Step 4:** never present the copy to an ESXi host that has the original datastore mounted.

## Which descriptor to open

A VM's disk is named in its configuration, for example `vm1/vm1.vmdk`. After your VM snapshot, the VM writes to
`vm1/vm1-000001.vmdk`, and `vm1/vm1.vmdk` is the frozen state you want. So: **open the descriptor that was the
VM's current disk before your snapshot.**

| The VM before your snapshot | Its disk is | After your snapshot, open |
|---|---|---|
| has no snapshots | `vm1.vmdk` | `vm1.vmdk` |
| already has two snapshots | `vm1-000002.vmdk` | `vm1-000002.vmdk` (OpenVmfs reads through `-000001` down to the base) |

You get the name from vSphere: read the disk's backing file name from the VM's configuration before step 1, or
take the parent of the new backing after it. OpenVmfs does not read `.vmx` or `.vmsd` files to work it out.

The descriptor you must *not* open is the new one (`-000001` in the first row): its delta is being written, and
OpenVmfs refuses it.

## What you get

- `capacity`: the size of the disk.
- `data_ranges`: where the disk holds data, sorted and merged. Everything else is zeros; skip it. For a base disk
  the ranges follow VMFS blocks (1 MiB); with snapshot deltas they are as fine as 4 KiB.
- `read(offset, buffer)`: positioned reads that fill the buffer or fail. Any offset, any length, from any number of
  threads. Blocks of a thick disk that were never written read as zeros, although the LUN holds stale data there.

- `map(offset, length)`: for callers that would rather read the LUN themselves, where every part of the disk is
  stored (a device and an offset on it, or zeros), through all snapshot deltas. OpenVmfs then reads only metadata.
  Useful when the LUN is behind a slow or asynchronous transport.

A full backup reads the data ranges. For an **incremental** backup you need to know what changed, and that is not
on the datastore in a form OpenVmfs reads: ask vSphere (`QueryChangedDiskAreas`, which needs changed-block
tracking and the VM snapshot from step 1) and read those areas with OpenVmfs. Intersect them with `data_ranges` if
you like; areas outside it are zeros.

## Errors, from the job's point of view

| Kind | What it means for the backup |
|---|---|
| `InUse` | The disk was open for writing when the storage snapshot was taken: step 1 did not happen for this disk, or it is an independent disk that VM snapshots leave out. Do not read it from this storage snapshot. Back it up another way |
| `Unsupported` | OpenVmfs will not read this disk or volume: a raw device mapping, an encrypted disk, a layout it has not verified. The data may be fine. Back it up another way |
| `Corrupt` | The metadata contradicts itself. Do not trust anything read from this object; alert someone |
| `Io` | The device failed. Retry or fail the job |
| `NotFound` | The descriptor path is wrong, or the file is on another datastore |

"Another way" is whatever your product has besides storage snapshots, typically NBD. A product that uses OpenVmfs
should always have one: OpenVmfs refuses rather than guesses, by design.

## Things OpenVmfs does not do

- It does not talk to vSphere or to the array. Creating snapshots, presenting LUNs and finding the right device are
  yours.
- It does not make data application-consistent. That is what quiescing in step 1 is for.
- It does not detect silent damage. VMFS metadata carries no checksums that we know of.
- It does not write: no restore to a datastore.
- It does not read VMFS5, VMFS-L, vSAN, vVols or NFS datastores.

## Checks worth building in

Reading a filesystem without its owner's code deserves a second line of defence in the product that does it:

- Compare `capacity` with what vSphere says about the disk.
- After an incremental backup, now and then read a sample of blocks that changed-block tracking reported as
  unchanged and compare them with the previous backup.
- Now and then verify a backup against a read through the hypervisor.
- Record `active_heartbeats` and the journal status of the volume with the job. They cost nothing and help later.

## Performance notes

- Open the volume with direct I/O (`OpenOptions::direct`, `OVMFS_OPEN_DIRECT`, `DirectIo`). The data is read once;
  the page cache only gets in the way, and it can hold blocks of an earlier snapshot that had the same device name.
- Opening a volume reads its metadata once (a few MiB). Opening a disk reads its whole block map: about 8 bytes per
  MiB of a base disk, plus the tables of every delta in the chain. After that a read is a lookup in memory and one
  read from the device.
- Reads on one disk scale with threads until the device is the limit; there are no locks in the read path.
- Read in large pieces (1 to 4 MiB) along `data_ranges`.

### What opening costs

Measured with `crates/openvmfs-testkit/examples/measure.rs` on datastores made by ESXi 7.0.3 and 8.0 U3. A read
is one request to the device; when the device is behind a network, multiply by its round-trip time.

| Step | Reads from the device | Bytes | Notes |
|---|---|---|---|
| Open a volume | 23 or 24 | 4.2 MiB | Nearly the same from 4 GiB to 40 TiB: one more read for each extent of the volume (every 16 TiB). One read is 4 MiB, the rest are small |
| Open a base disk | about 15, plus one per 8 GiB of disk size | 64 KiB per 8 GiB | 200 GiB thick: 42 reads, 1.7 MiB; 4100 GiB thick: 533 reads, 33 MiB. 4 TiB thin: 23 reads, because unallocated parts have no pointer blocks |
| Each snapshot delta in the chain | about 25, plus one per 4 MiB of grain tables | 32 KiB per 16 MiB of disk the delta has touched | A 64 GiB disk with a million random 4 KiB writes all over it: 73 reads, 128 MiB. That is the most a delta of this size needs |

All reads of one open happen one after the other.

Memory that stays allocated per open disk is its block map: 32 bytes per extent of the base disk plus 16 bytes per
data range, and one entry per run of each delta.

| Disk | Extents | Memory |
|---|---|---|
| 200 GiB thick, never written | 4 | a few hundred bytes |
| 4 TiB thin, written in five places | 9 | a few hundred bytes |
| 64 GiB thin with every other MiB written (the worst case for a base disk) | 32,769 | about 1 MiB |
| 64 GiB with a delta of 1,048,576 random 4 KiB writes (one run per write) | 1, and 1,048,576 runs | 57 MiB |

The worst case is one extent per MiB, so 40 bytes per MiB of disk: about 80 MiB for a 2 TiB disk that is fragmented
down to single megabytes. A disk that was filled in larger pieces needs a small fraction of that. A volume keeps
about 1.5 MiB.

A delta costs about 56 bytes for each run, a run being a piece of the disk that lies in one piece in the delta.
Sequential writes give few runs; random 4 KiB writes give one run each, so a delta that holds 4 GiB of them needs
57 MiB, and one that holds 100 GiB of them would need about 1.4 GiB. Opening that delta took 0.16 seconds on a
local image, most of it for reading the tables.

`map` works on these tables and reads nothing from the device: 0.1 to 0.3 microseconds for a 4 MiB section of a
base disk, on one core of a desktop CPU. On the delta with a million random writes it took 3.6 microseconds per
4 MiB section, which had 129 ranges on average and 135 at most.

We have not measured throughput: it is the device's. A read of disk data is one lookup in memory and one read from
the device.

## The experimental alternative: journal replay

OpenVmfs can apply the transactions that wait in the hosts' journals (`OpenOptions::replay_journal`). A disk that
was open for writing then reads as ESXi would see it after a crash: crash-consistent, like a VM that lost power. In
the lab this made 10 of 10 otherwise unreadable images correct. It is meant for the cases where step 1 is impossible,
for example an existing array snapshot that was taken without VM snapshots. It is not a reason to skip step 1: the
journal format is known from a handful of images, and the delta of a running VM stays unreadable either way.
