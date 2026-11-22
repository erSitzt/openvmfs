# SESparse snapshot deltas

SESparse ("space-efficient sparse") is the format of snapshot deltas on VMFS6. VMware publishes no description of
it. What follows was first learned from the read-only support in QEMU **[1]** and then checked against deltas
written by ESXi 7.0.3 and ESXi 8.0 U3, with ESXi's own consolidation of each chain (`vmkfstools -i`) as the
reference **[V]**. The two versions differ in one header field.

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
| 48 | unknown | 0 on ESXi 7.0.3, **0x200 on ESXi 8.0 U3** **[V]** |
| 56–79 | three reserved fields | 0 |
| 80, 88 | volatile header offset, size | 1, 1 |
| 96, 104 | journal header offset, size | 2, 2 |
| 112, 120 | journal offset, size | 2048, 2048 |
| 128, 136 | grain directory offset, size | 4096, 2048 (room for 131072 entries) |
| 144, 152 | grain tables offset, size | 6144, 2048 at creation |
| 160, 168 | free bitmap offset, size | 8192, 2048 |
| 176, 184 | back map offset, size | 10240, 2048 |
| 192, 200 | grains offset, size | 12288 (6 MiB), the disk size |
| 208 | padding | zero |

An empty delta is a 6 MiB file: metadata only. These sizes are from deltas of small disks (up to 1 GiB). The table
area, the back map and the free bitmap grow with the disk: the delta of a 64 GiB disk had 128 MiB of grain tables
(room for all 4096), a 2 MiB free bitmap and a 128 MiB back map, with the grains starting at 261 MiB **[V]**
(ESXi 8.0 U3). Much of that is not allocated on the datastore until it is used.

## Volatile header

At its offset, 512 bytes: magic 0x00000000CAFECAFE **[V]**, number of grain tables in use, next transaction number,
and a "replay journal" flag, followed by zero padding.

| State of the delta | Flag |
|---|---|
| VM powered off | 0 **[V]** |
| Closed by a later VM snapshot while the VM keeps running | 0 **[V]** (7 of 7 storage snapshots taken 0.6 to 1.3 s after the VM snapshot) |
| The delta a running VM is writing to | 1 **[V]** (7 of 7) |
| Just created, no guest I/O on the disk yet | 0 (seen once) |
| Newest delta of a disk that the running guest only reads | 1 **[V]** |

A delta with the flag set cannot be trusted: in all seven cases its grain tables on disk showed none of the data
the guest had written to it in the 0.6 to 1.3 s since it was created, and the file was still as short as an empty
delta. OpenVmfs refuses such a delta, because it cannot replay the journal. The journal header (sector 2) starts
with the magic 0xDEAFC0DE **[V]**; the journal itself has not been analysed.

## Grain directory and grain tables

- A directory entry is 0 (no table) or `0x10000000` in its upper half and a table index in its lower half **[V]**.
  Table `i` is at `grain tables offset + i × 64` sectors.
- A table has 4096 entries and covers 16 MiB of the disk.
- Table indices are handed out in the order in which a guest first writes to each 16 MiB of the disk **[V]**:
  after random writes over a whole 64 GiB disk the directory named the 4096 tables in no order at all. In the
  file they still lie next to each other, so OpenVmfs reads the tables in the order of the file, up to 4 MiB per
  read, and sorts the result by disk offset.
  That disk (1,048,576 random 4 KiB writes, one run each) read with the same SHA-256 as ESXi's own
  consolidation of it. Its image is not in the repository: the tables alone are 128 MiB.
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
