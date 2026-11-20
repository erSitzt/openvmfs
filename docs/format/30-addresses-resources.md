# Addresses and resource files

## Addresses

Everything a file refers to is named by an address. The low three bits give the type. Address arrays and pointer
blocks hold 64-bit addresses; directory entries hold 32-bit file descriptor addresses.

| Type | Name | Width | Refers to |
|---|---|---|---|
| 0 | null (whole value 0) | | nothing: a hole |
| 1 | SFB, small file block | 64 | a 1 MiB block of the volume |
| 2 | SB, sub-block | 64 | a 64 KiB resource in `.sbc.sf` |
| 3 | PB, pointer block | 64 | a resource in `.pbc.sf` |
| 4 | FD, file descriptor | 32 | a resource in `.fdc.sf` |
| 5 | PB2, pointer block | 64 | a resource in `.pb2.sf` |
| 6 | JB, journal block | 32 | a resource in `.jbc.sf` |
| 7 | LFB, large file block | 64 | a 512 MiB block of the volume |

### Bit layouts

Bit 0 is the least significant bit; ranges are inclusive.

| Type | Bits | Field | Confidence |
|---|---|---|---|
| SFB | 5 | copy-on-write | **[1]** |
| | 7–14 | "to be zeroed" bitmap | **[V]** for bit 7, others never seen |
| | 15–45 | cluster | **[V]** for small numbers |
| | 51–63 | resource (block inside the cluster) | **[V]** up to 511 |
| LFB | 5 | copy-on-write | **[1]** |
| | 7–14 | "to be zeroed" bitmap | **[V]** for bit 7, others never seen |
| | 15–45 | block number | **[V]** |
| SB, PB, PB2 | 5 | copy-on-write | **[1]** |
| | 6–41 | cluster | **[V]** for small numbers |
| | 56–63 | resource | **[V]** |
| FD | 6–21 | cluster | **[V]** |
| | 22–31 | resource | **[V]** |
| JB | 3–15 | cluster | **[1]** |
| | 26–31 | resource | **[1]** |

Bit 5 is read as copy-on-write by one implementation family and as the VMFS5 form of "to be zeroed" by the
other; it was never seen set on VMFS6. All other bits have no known meaning. OpenVmfs refuses an address in which one of them is set, any address
with bit 5, addresses in `.pb2.sf` other than the first, and pointer block
addresses of type 3: for those the implementations disagree or no sample exists.

Sub-block clusters above 255 are **[V]**: on a datastore with 100,697 small files ESXi 8.0 U3 used clusters 0 to 260,
and the cluster number continues in bits 6–41 (cluster 256 is address `0x4002`). See `fixture-many-esxi8-v1`.

The two implementation families read SFB and SB addresses differently once numbers get large
(see [open questions](90-open-questions.md)). The layout above is the wider one; both agree on the values seen so
far.

### Examples from a real volume **[V]**

| Raw | Decoded | Volume offset |
|---|---|---|
| `0x0040000000000001` | SFB, cluster 0, resource 8 | 8 MiB |
| `0x06F0000000008001` | SFB, cluster 1, resource 222 | (1 × 512 + 222) MiB |
| `0x0000000000000005` | PB2, cluster 0, resource 0 | through `.pb2.sf` |
| `0x0000000000420007` | LFB, block 132 | 132 × 512 MiB |
| `0x0000000000400087` | LFB, block 128, "to be zeroed" bit 7 | reads as zeros |
| `0x0610000000C08081` | SFB, cluster 385, resource 194, "to be zeroed" | reads as zeros |
| `0x0300000000000042` | SB, cluster 1, resource 3 | a pointer block, in `.sbc.sf` |
| `0x00000004` | FD, cluster 0, resource 0 | the root directory |
| `0x00800B04` | FD, cluster 44, resource 2 | printed by ESXi as `<4, 44, 2>` |

### From address to volume offset

- **SFB:** `(cluster × 512 + resource) × 1 MiB` **[V]**. A cluster of small file blocks is exactly one large file
  block. The 512 is "small file blocks per cluster" in the header of `.fbb.sf`; OpenVmfs checks that header
  instead of trusting the constant.
- **LFB:** `block × 512 MiB` **[V]** (blocks 128 to 135 on a 256 GiB datastore).
- **SB, PB, PB2, FD:** look the resource up in its resource file (below).

## Resource files

`.fdc.sf`, `.sbc.sf`, `.pbc.sf` and `.pb2.sf` are files of equally sized resources. Resources are grouped into
clusters, clusters into cluster groups. Each file starts with a header:

| Offset | Size | Field |
|---|---|---|
| 0x00 | 4 | resources per cluster |
| 0x04 | 4 | clusters per group |
| 0x08 | 4 | offset of the first cluster group |
| 0x0C | 4 | resource size |
| 0x10 | 4 | cluster group size |
| 0x14 | 4 | number of resources, low half |
| 0x18 | 4 | number of cluster groups |
| 0x1C | 4 | number of resources, high half |
| 0x20 | 4 | signature `rfmd` = 0x72666D64 **[V]** |
| 0x24 | 4 | version |
| 0x28 | 4 | flags: 0x2 nested in a parent's cluster groups, 0x20 has an unmap bitmap **[1]** |
| 0x30 | 4 | bits per resource |
| 0x34 | 4 | offset of a second ("child") header in the same file |
| 0x38 | 12 | parent's resources per cluster, clusters per group, cluster group size |

A cluster group starts with one header per cluster. A cluster header is a lock block plus a metadata block, so
2 × 4096 bytes on VMFS6. The resources of all clusters of the group follow. The file offset of resource `r` in
cluster `c` is therefore **[V]**:

```
group, in_group = c / clusters_per_group, c % clusters_per_group
offset = first_group_offset
       + group × cluster_group_size
       + clusters_per_group × 8192
       + in_group × resources_per_cluster × resource_size
       + r × resource_size
```

That file offset is translated through the resource file's own block map, like any file offset.

Seen on VMFS 6.82 **[V]**:

| File | Resources per cluster | Clusters per group | Resource size | Cluster group size | Groups | Flags |
|---|---|---|---|---|---|---|
| `.fdc.sf` | 256 | 8 | 8192 | 0x1010000 | 8 | 0x4 |
| `.sbc.sf` | 256 | 8 | 65536 | 0x8010000 | 8 | 0x4 |
| `.pb2.sf` | 32 | 8 | 65536 | 0x1010000 | 1 | 0x4 |
| `.pbc.sf` | 0 | 8 | 65536 | 0x10000 | 0 | 0x4 |
| `.fbb.sf`, large file blocks | 16 | 8 | 8192 | 0x110000 | 1 | 0x35 |
| `.fbb.sf`, small file blocks (child header at 0x1000) | 512 | 8 | 0 | 0x10000 | 1 | 0x2A |

The first cluster group is at 0x10000 in every file. Notes:

- `.pbc.sf` is **empty**: VMFS6 stores pointer blocks as sub-blocks, or in `.pb2.sf`.
- `.sbc.sf` is a sparse file of about 1 GiB; `.pb2.sf` is 17 MiB and addressed directly.
- `.fbb.sf` counts 4 large file blocks and 2048 small ones on a 2 GiB volume. The small-block header is the "child"
  of the large-block header and carries flag 0x2.

OpenVmfs refuses a resource file with flag 0x2 when an address has to be resolved in it, because the nested layout
has not been verified. It does not read the allocation bitmaps in the cluster headers: a read-only reader follows
addresses and does not need to know what is free.

## Opening order

The block map of one system file can need another system file. On the samples, `.sbc.sf` is addressed through a
pointer block that lives in `.pb2.sf` **[V]**. The older tools open the files in a fixed order and fail on volumes
where that order is wrong. OpenVmfs tries all of them in rounds: a file whose dependencies are not open yet waits
for the next round, and a round without progress is an error.
