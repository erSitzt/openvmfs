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

VMFS is a journalling filesystem. Each host writes metadata changes to its journal first and to their final place
afterwards; after a crash, another host replays the journal. On VMFS6 the journals live in `.jbc.sf`.

**No public implementation reads the journal, and neither does OpenVmfs.** Its record format is unknown.

## Consequences for reading snapshots

A storage-array snapshot of a mounted datastore is what the disk would look like after a power failure. Metadata
that was being changed at that moment may exist only in a journal. A reader that ignores the journal can then see:

- a file descriptor or pointer block in its old state (for example a thin disk without its newest blocks);
- a directory without an entry that was just created;
- half of a multi-block update.

What limits the risk for backups: backup software takes a VM snapshot first and the array snapshot afterwards. From
that moment ESXi writes only to the new delta file. The base disk (and older deltas) are no longer changed, so their
metadata has had time to reach its final place. The file that is in flux is the active delta, and that one is not
part of the backup.

Two lab probes support this: files closed a tenth of a second before the snapshot, and base disks frozen by a VM
snapshot 0.6 s before it, all read identical to ESXi from a crash-consistent copy without journal replay. See
[`../consistency.md`](../consistency.md) for the method and for what the probes do not show. Keep an independent
check in the backup software all the same (for example comparing blocks that changed-block tracking reports as
unchanged with the previous backup).

## Never write, never reserve

The older tools send a SCSI reservation to the device when they open it, even read-only, and can thereby disturb a
datastore that ESXi hosts are using. OpenVmfs opens the device read-only and issues nothing but reads.
