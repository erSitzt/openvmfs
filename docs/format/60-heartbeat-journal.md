# Heartbeats, the journal and what a snapshot of a live datastore means

## Heartbeat region

The heartbeat region starts at **volume offset 0x300000** and has 1024 slots of 4096 bytes **[2]**. A host that has
the datastore mounted owns a slot and refreshes it every few seconds.

| Offset | Size | Field |
|---|---|---|
| 0x00 | 4 | state: 0xABCDEF01 free, 0xABCDEF02 in use **[V]**; other values appear during journal replay **[1]** |
| 0x04 | 8 | volume offset of the slot **[V]** |
| 0x0C | 8 | generation |
| 0x14 | 8 | time stamp |
| 0x1C | 16 | owner UUID |
| 0x2C | 4 | address of the owner's journal |
| 0x64 | 8 | volume offset of the owner's journal |

Only the state field is used by OpenVmfs; the other fields are **[1]**, except that the text at 0x36 is the
host's IP address **[V]**. It stays in the slot after an unmount, so an image of a datastore names the host that
used it.

What the state means in practice **[V]**:

- A slot is "in use" while its host holds locks on the datastore, for example while it runs a VM whose disks are
  there: an image taken while a guest was running on our test datastore has exactly one slot in use
  (`fixture-guest-v1`). One of the public samples was imaged in that state too.
- A datastore that is mounted but idle has **no** slot in use: an image taken while our test datastore was
  mounted, with nothing open, shows all 1024 slots free, exactly like the image taken after unmounting it.

- A copy taken while only a script on the ESXi shell was writing files had no slot in use either.

So `VolumeInfo::active_heartbeats` says how many hosts were holding long-lived locks, such as those of running
VMs, when the image was taken. **Zero means neither "unmounted" nor "idle".**

## Journal

VMFS is a journalling filesystem. A host writes a metadata change to its journal first and to its final place
later; after a crash, another host replays the journal. Nothing about the format is published. What follows was
worked out from two images of one datastore (ESXi 7.0.3, VMFS 6.82): one taken while two block allocations were
still waiting in the journal, one taken seven minutes later, when they had reached their place **[V]**. It is the
basis of the experimental journal replay (`OpenOptions::replay_journal`), which refuses whatever looks different.
Journals written by ESXi 8.0 U3 have the same format: replaying them made five of five images of a writing VM's
disk complete **[V]**.

### Where a journal is

- The filesystem descriptor holds the size of a journal at 0x148: 0x200000 (2 MiB) **[V]**.
- A heartbeat slot holds its owner's journal: a journal block address at 0x2C and the journal's volume offset at
  0x64 **[V]**. Slots that are free hold zeros there **[V]**.
- The journals themselves are resources of `.jbc.sf`.

### Layout of a journal

The first 4096 bytes are a header: the text `6JSF` at 0, then 0x10000, an 8-byte id and a 1; the text `HJ6J` at
0x20 **[V]**. Then come 511 slots of 4096 bytes, each holding at most one transaction.

A slot starts with:

| Offset | Size | Field |
|---|---|---|
| 0x00 | 4 | `HT6J`: a transaction that waits to be written to its place. `TO6J`: nothing waiting (the slot is unused, or its transaction is done) **[V]** |
| 0x04 | 2 | number of the slot, from 1 |
| 0x06 | 2 | flags: 0 on unused slots; 0x0002 and 0x0272 on waiting transactions; 0x0272 became 0x0270 when the transaction was done **[V]** |
| 0x08 | 8 | an id of the transaction, repeated at 0x20 |
| 0x18 | 8 | sequence number, repeated at 0x40 and 0x48; counts up by one per transaction **[V]** |
| 0x30 | 4 | length of the transaction in bytes (0x3E0 in all 16 seen) |
| 0x38 | 8 | generation of the owner's heartbeat slot **[V]** |
| 0x50 | 4 | offset of the first lock entry in the slot (0xE0) |
| 0x54 | 4 | offset of the first update entry (0x100) |
| 0x58 | 2 | number of lock entries |
| 0x5A | 2 | number of update entries |

A transaction has two lists; each entry starts with its own offset in the slot and the offset of the next entry (0
at the end) **[V]**.

**Lock entries** name the lock blocks that the transaction holds: type 0x200 (a file descriptor's lock block) or
0x10200 (a resource cluster's), at offset 8, then the volume offset of the lock block.

**Update entries** carry the changes to one 4096-byte metadata block:

| Offset | Size | Field |
|---|---|---|
| 0x00 | 4 | own offset in the slot |
| 0x04 | 4 | offset of the next entry |
| 0x08 | 4 | type: 0x105, or 0x10105 for a block of a resource cluster |
| 0x0C | 8 | volume offset of the block that is changed |
| 0x14 | 4 | size of the changes that follow, in bytes; repeated at 0x1C |
| 0x18 | 4 | size of the block: 4096 |
| 0x20 | 8 | 0x10000 or 0x10001, then 0 |
| 0x28 | | the changes, back to back: offset in the block (4 bytes), length (4 bytes), the new bytes |

The transaction that gives a thin file one more block has three update entries **[V]**: one 8-byte address in the
file's pointer block, a few fields of the file descriptor (block count, times), and counters and bits in the
allocation bitmap of the block's cluster. The data of the new block is written directly; it does not pass through
the journal.

### Replay

Take the slots that start with `HT6J`, order their transactions by sequence number, and apply their changes.

How this was checked on the two images **[V]**:

- of the 16 transactions in the journal, 14 were done. Every byte that they were the last to write (240 bytes) was
  in place;
- 2 were waiting. Applying them to the first image gives the block map of the second one: 134 of their 136 bytes
  are what the second image holds, and the other two (the low bytes of two time stamps) were changed again by a
  later transaction;
- with the journal replayed, the virtual disk read from the first image has the same SHA-256 as the one read from
  the second image. Read without replay it differs: its last two megabytes are missing (`fixture-journal-v1`).

Four further images of a writing VM, with 5 to 15 transactions waiting, became consistent the same way
(`docs/consistency.md`). The idle journals of the public sample images of dissect.vmfs, made by another ESXi
installation, have the same signatures.

### Not known

- Whether a transaction is protected by a checksum. A journal slot that was only partly written when the image
  was taken would be recognised only if its lists no longer fit together.
- Transactions that need more than one slot, and entry types other than the four above. OpenVmfs refuses them.
- The order between the journals of two hosts. OpenVmfs refuses to replay when two journals change the same block.
- When a host moves a transaction to its place. In the lab it had not happened 5 seconds after the last write and
  had happened after several minutes.

## Consequences for reading snapshots

A storage-array snapshot of a mounted datastore is what the disk would look like after a power failure. Metadata
changes of the last seconds exist only in a journal. A reader that ignores the journals sees a file that a host had
open for writing in an older state: a thin disk without its newest blocks, for example, while data that was written
later to blocks it already had is present. We have measured exactly that; see
[`../consistency.md`](../consistency.md).

What OpenVmfs does about it:

- by default it refuses virtual disks whose files a host had open for writing (`ErrorKind::InUse`). The lock state
  is in every file descriptor. Disks below a VM snapshot are not affected: taking the VM snapshot closes them for
  writing, and they read correctly without the journal in 9 of 9 lab runs;
- `Volume::journal_status` says how many transactions were waiting;
- with `OpenOptions::replay_journal` it applies them and then opens such disks as well. This is experimental.

## Never write, never reserve

The older tools send a SCSI reservation to the device when they open it, even read-only, and can thereby disturb a
datastore that ESXi hosts are using. OpenVmfs opens the device read-only and issues nothing but reads.
