# SESparse snapshot deltas

SESparse ("space-efficient sparse") is the format of snapshot deltas on VMFS6. VMware publishes no description of
it. What follows was first learned from the read-only support in QEMU **[1]** and then checked against deltas
written by ESXi 7.0.3, with ESXi's own consolidation of each chain (`vmkfstools -i`) as the reference **[V]**.

A delta holds the blocks ("grains", 4 KiB) written since the snapshot. A read of a grain goes to the newest delta
first and falls through to its parent, down to the base disk.

## Constant header

At byte 0, 512 bytes, all fields 64-bit; offsets and sizes are in sectors.

| Offset | Field | Value on ESXi 7.0.3 **[V]** |
|---|---|---|
| 0 | magic | 0x00000000CAFEBABE |
| 8 | version | 0x0000000200000001 |
| 16 | capacity | the disk size |
| 24 | grain size | 8 (4 KiB) |
| 32 | grain table size | 64 (4096 entries) |
| 40 | flags | 0 |
| 48–79 | four reserved fields | 0 |
| 80, 88 | volatile header offset, size | 1, 1 |
| 96, 104 | journal header offset, size | 2, 2 |
| 112, 120 | journal offset, size | 2048, 2048 |
| 128, 136 | grain directory offset, size | 4096, 2048 (room for 131072 entries) |
| 144, 152 | grain tables offset, size | 6144, 2048 at creation |
| 160, 168 | free bitmap offset, size | 8192, 2048 |
| 176, 184 | back map offset, size | 10240, 2048 |
| 192, 200 | grains offset, size | 12288 (6 MiB), the disk size |
| 208 | padding | zero |

An empty delta is a 6 MiB file: metadata only.

## Volatile header

At its offset, 512 bytes: magic 0x00000000CAFECAFE **[V]**, number of grain tables in use, next transaction number,
and a "replay journal" flag, followed by zero padding. The flag was 0 on every delta of a powered-off VM **[V]**. A
non-zero flag means the file was not closed cleanly **[1]**; OpenVmfs refuses such a delta, because it cannot
replay the journal.

## Grain directory and grain tables

- A directory entry is 0 (no table) or `0x10000000` in its upper half and a table index in its lower half **[V]**.
  Table `i` is at `grain tables offset + i × 64` sectors.
- A table has 4096 entries and covers 16 MiB of the disk.
- A table entry carries its type in the top four bits:

| Type | Meaning | How it reads |
|---|---|---|
| 0 | unallocated; the whole entry is 0 | from the parent **[V]** |
| 1 | discarded by the guest (SCSI unmap) | **zeros**, even where the parent holds data **[V]** |
| 2 | zero grain | zeros **[1]**; never seen, refused |
| 3 | allocated | from the delta **[V]** |

- For type 3, the grain index is split: bits 48–59 hold its low 12 bits, bits 0–47 the rest. `0x3001000000000000`
  is index 1 **[V]**. Data of grain `i` is at `grains offset + i × 8` sectors.
- A type 1 entry may keep the index bits of the grain that used to hold the data (`0x1100000000000000` for a grain
  that was written and then discarded in the same delta) **[V]**. They are not needed for reading.

How type 1 was settled: a base disk had data at 10 MiB. After a snapshot, the guest discarded that MiB. ESXi's
consolidation of the chain has nothing there, and neither does QEMU's conversion; a reader that falls through to
the parent would return the old data. One level up, 8 sectors written into the discarded MiB read back, and the
rest of the MiB stays zero.

## Chains

A link names its parent with `parentFileNameHint`. ESXi writes a bare file name for a parent in the same directory
**[V]** (`thin-g-000002.vmdk` names `thin-g-000001.vmdk`, which names `thin-g.vmdk`), and an absolute
`/vmfs/volumes/<datastore>/...` path when the parent is elsewhere **[1]**. The child's `parentCID` equals the
parent's `CID` **[V]**; a mismatch means the parent was changed afterwards or the chain is broken.

OpenVmfs follows a chain down to its base, checks the content ids and that all links have the same size, and reads
each range from the newest link that defines it. It refuses:

- a parent on another datastore;
- a delta that was not closed cleanly, which is what the active delta of a running VM looks like;
- zero grains (type 2) and any header value not listed above;
- deltas in the older COWD format (`VMFSSPARSE`), which VMFS6 does not create.

Each delta's tables are read completely when the disk is opened and kept as a list of runs. A delta with very
scattered writes needs memory in proportion (one run is 32 bytes).

## Not covered

Disks larger than 2 TiB, deltas written by other ESXi versions, linked clones, and the journal of a delta.
