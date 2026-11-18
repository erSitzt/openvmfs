# The VMFS6 on-disk format, as OpenVmfs reads it

VMware publishes no specification of VMFS. This description was written by reading the existing open-source
implementations ([sources](sources.md)) and checking what they say against volumes written by ESXi. It is in our
own words and covers what a read-only implementation needs.

## Confidence marks

Every statement that matters carries one of these marks.

| Mark | Meaning |
|---|---|
| **[V]** | Verified by us on a volume written by ESXi: the bytes were inspected, and data read this way matched its known content. |
| **[2]** | Two independent implementation families say the same (dissect.vmfs, and the vmfs-tools line). Not checked by us yet. |
| **[1]** | One implementation says so. |
| **[?]** | Implementations disagree, or nobody knows. See [open questions](90-open-questions.md). |

Only **[V]** behaviour is enabled by default. Everything else is refused, or sits behind
`OpenOptions::accept_unverified`.

Volumes inspected so far:

- three public VMFS 6.82 sample images written by ESXi (2 GiB each, published with dissect.vmfs);
- `fixture-zero-v1`, our own: a 4 GiB disk with a VMFS 6.82 datastore created by ESXi 7.0.3, with reference data
  whose position is known, ESXi's own hashes and block maps, and stale junk under every block that was never
  written (see `fixtures/vmfs6/README.md`).

## Conventions

- All integers are little-endian.
- A **device offset** counts from the start of the disk, a **partition offset** from the start of the VMFS
  partition, a **volume offset** from the start of the logical volume, a **file offset** from the start of a file.
- MiB = 2^20 bytes. "Sector" always means 512 bytes.

## The layers

```
disk ──GPT──▶ VMFS partition ──LVM──▶ logical volume ──VMFS──▶ files ──VMDK──▶ virtual disk
```

| Layer | Document | Code |
|---|---|---|
| Partition table and logical volume manager | [10-partition-lvm.md](10-partition-lvm.md) | `gpt.rs`, `lvm.rs` |
| Filesystem descriptor | [20-fs-descriptor.md](20-fs-descriptor.md) | `fs/descriptor.rs` |
| Addresses and resource files | [30-addresses-resources.md](30-addresses-resources.md) | `fs/addr.rs`, `fs/resource.rs` |
| File descriptors and block maps | [40-file-descriptor.md](40-file-descriptor.md) | `fs/inode.rs`, `fs/blockmap.rs` |
| Directories | [50-directories.md](50-directories.md) | `fs/dir.rs` |
| Heartbeats and the journal | [60-heartbeat-journal.md](60-heartbeat-journal.md) | `fs/mod.rs` |
| VMDK descriptor and flat disks | [70-vmdk-descriptor.md](70-vmdk-descriptor.md) | `vmdk/` |
| SESparse snapshot deltas (not implemented yet) | [80-sesparse.md](80-sesparse.md) | |

## Layout of a small volume

Offsets seen on a 2 GiB VMFS 6.82 volume **[V]**:

| Partition offset | Volume offset | What |
|---|---|---|
| 0x100000 | | LVM device header |
| 0x101000 | | LVM volume descriptor |
| 0x181000 | | Physical-extent bitmap |
| 0x182000 | | Physical-extent table |
| 0x1100000 | 0x0 | Start of the logical volume; also the start of `.vh.sf` |
| 0x1300000 | 0x200000 | Filesystem descriptor |
| 0x1400000 | 0x300000 | Heartbeat region |
| 0x1800000 | 0x700000 | `.fdc.sf`: the file descriptors |
| 0x1820000 | 0x720000 | File descriptor of the root directory |
