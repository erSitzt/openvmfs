# Open questions

Each of these blocks a refusal from being lifted, or is a risk. Most can be settled with a small datastore written
by ESXi that holds reference data whose position is known.

## Blocks OpenVmfs refuses today

| # | Question | How to settle it |
|---|---|---|
| 1 | **"To be zeroed" bitmaps other than bit 7 on a small file block.** Bit 7 is settled (see [40-file-descriptor.md](40-file-descriptor.md)). Are other bits ever set, for example with another granularity? | Refused until seen. |
| 4 | **Sub-block and small-file-block addresses with large numbers.** The two implementation families disagree: cluster in bits 6–41 and resource in bits 56–63, versus fewer cluster bits; cluster in bits 15–45 and resource in bits 51–63, versus 17 and 9 bits. They agree up to sub-block cluster 255 and small-block cluster 131071. | A datastore with more than 65536 sub-blocks in use (many small files), and eventually a volume above 64 TiB. |
| 5 | **Resource files with nested cluster groups** (flag 0x2) for descriptors, sub-blocks or pointer blocks. | Not seen yet outside `.fbb.sf`; refuse until seen. |
| 6 | **Pointer blocks stored in file blocks or in `.pbc.sf`.** | Not seen on VMFS6; refuse until seen. |

## Volumes OpenVmfs refuses today

| # | Question |
|---|---|
| 7 | Volumes above 16 TiB: chained extent metadata and the extra LVM header every 16 TiB. |
| 8 | Volumes on several devices. Sample images exist. |
| 9 | VMFS 6.81: what differs from 6.82? |
| 10 | Devices with 4096-byte logical sectors, and metadata alignments other than 4096. |

## Risks in what is enabled

| # | Question |
|---|---|
| 12 | **The journal.** Two lab probes found no file that needed journal replay when read in the backup order (VM snapshot, then storage snapshot); see [`../consistency.md`](../consistency.md). Open: many more runs, shared datastores, files that are still being written. |
| 13 | Directory allocation map: must the "free" and "never written" bits be honoured? |
| 15 | Copy-on-write addresses: when does ESXi set the bit? (Refused today.) |

## Virtual disks

| # | Question |
|---|---|
| 16 | SESparse zero grains (type 2): when does ESXi write them? Refused until seen. |
| 17 | How ESXi escapes a double quote in an extent file name. |
| 18 | The exact descriptor lines of an encrypted disk. |

## Settled

| Question | Answer | Evidence |
|---|---|---|
| Which bits mark a never-written block, and what happens on a partial write? | Bit 7; the whole block is zeroed on disk at the first write. | `fixture-zero-v1`, disk `lazy` |
| Does a disk with a partition table written by ESXi read? | Yes, partition at sector 2048. | `fixture-zero-v1` |
| What are the offsets printed by `vmkfstools -t0`? | Offsets on the whole disk device (not on the partition or the logical volume). | `fixture-zero-v1`, `truth/lazy.t0.txt` |
| Does zero active heartbeats mean "unmounted"? | No. A mounted, idle datastore has none either. | image taken while mounted |
| How are large file blocks addressed? | One entry per MiB; the block's address is repeated in all 512 entries it backs. Files can mix large and small blocks. | `fixture-lfb-v1` |
| How can eight bits mark 512 MiB as never written? | They do not have to: every per-MiB entry has its own bit 7. | `fixture-lfb-v1`, disk `lfb-lazy` |
| When does ESXi use large file blocks? | For thick disks, on datastores large enough: ESXi set aside up to 256 large blocks (128 GiB) for small-block clusters, so the 4 and 16 GiB datastores had none to give. | `vmkfstools -Ph` and `-D` on 4, 16 and 256 GiB datastores |
| Do writes from a guest leave the same metadata as writes from the ESXi shell? | Yes, for thin and lazy-zeroed disks written through a paravirtual SCSI controller: holes, per-MiB "to be zeroed" marks and the zeroing of a partly written MiB are the same. | `fixture-guest-v1` |
| How does a range read that the guest discarded after a snapshot (SESparse type 1)? | As zeros, not from the parent. ESXi's consolidation and QEMU agree. | `fixture-chain-v2` |
