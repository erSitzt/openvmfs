# Open questions

Each of these blocks a refusal from being lifted, or is a risk. Most can be settled with a small datastore written
by ESXi that holds reference data whose position is known.

## Blocks OpenVmfs refuses today

| # | Question | How to settle it |
|---|---|---|
| 1 | **"To be zeroed" bitmaps other than bit 7 on a small file block.** Bit 7 is settled (see [40-file-descriptor.md](40-file-descriptor.md)). Are other bits ever set, for example with another granularity? | Refused until seen. |
| 4 | **Sub-block and small-file-block addresses with large numbers.** The two implementation families disagree: cluster in bits 6–41 and resource in bits 56–63, versus fewer cluster bits; cluster in bits 15–45 and resource in bits 51–63, versus 17 and 9 bits. They agree up to sub-block cluster 255 and small-block cluster 131071. | Small file blocks: both readings agree below 64 TiB. Sub-blocks: **settled.** A datastore with 100,697 files of 8 KiB (ESXi 8.0 U3, `fixture-many-esxi8-v1`) used sub-block clusters 0 to 260; with the cluster in bits 6–41 all files read as written, so OpenVmfs no longer refuses clusters above 255. Addresses in `.pb2.sf`: they agree only on the first one; OpenVmfs refuses others. The same datastore still used only the first, so this stays open; small files do not get there. |
| 5 | **Resource files with nested cluster groups** (flag 0x2) for descriptors, sub-blocks or pointer blocks. | Not seen yet outside `.fbb.sf`; refuse until seen. |
| 6 | **Pointer blocks stored in file blocks or in `.pbc.sf`.** | Not seen on VMFS6 (`.pbc.sf` is empty even at 17 TiB); refused. |

## Volumes OpenVmfs refuses today

| # | Question |
|---|---|
| 7 | Extended LVM metadata regions and more than one extent map: when does ESXi create them? Not at 17 TiB (ESXi 7.0.3) and not at 40 TiB (ESXi 8.0 U3). Refused. |
| 8 | Volumes on several devices. Sample images exist. |
| 9 | VMFS 6.81: what differs from 6.82? |
| 10 | Devices with 4096-byte logical sectors, and metadata alignments other than 4096. |

## Risks in what is enabled

| # | Question |
|---|---|
| 12 | **The journal.** Files that a host has open for writing need journal replay to be read correctly; closed and read-only files did not in 9 of 9 runs of the backup order (VM snapshot, then storage snapshot); see [`../consistency.md`](../consistency.md). The transaction format for block allocations is known ([`60-heartbeat-journal.md`](60-heartbeat-journal.md)). Open: checksums, transactions larger than one slot, other entry types (file creation, deletion, directory changes), several hosts, other ESXi versions. |
| 13 | Directory allocation map: bit 2 is set on entry blocks that still have free slots (a block with live entries has code 5), so it must not be used to skip blocks **[V]**. More than one allocation map block (above about 112,000 entries) has not been seen. |
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
| How is a volume above 16 TiB laid out? | One extent entry per 16 TiB, 16 MiB apart on the device; no extended metadata at 17 TiB or 40 TiB. | `fixture-big-v1`, `fixture-big-esxi8-v1` |
| How is a datastore on several devices laid out? | One LVM header per device, extent entries with volume-wide logical offsets; files may lie on any device. | `fixture-span-v1` |
| What does the delta look like that a running VM writes to? | Its "replay journal" flag is 1, and up to 1.3 s after its creation its tables showed none of the data written since. Refused. | `probe-series1` |
| Can a disk be read from a storage snapshot while its VM writes to it? | No: block allocations of the last seconds are missing from the block map. The on-disk lock mode tells such files apart. | `fixture-inuse-v1` |
| Where are the block allocations that an image of a writing VM lacks? | In the journal of the host, as byte-range changes to metadata blocks. Applying them makes the disk consistent (5 of 5 images). | `fixture-journal-v1` |
