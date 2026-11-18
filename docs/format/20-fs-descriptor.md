# Filesystem descriptor

The descriptor (the superblock) is at **volume offset 0x200000** **[V]**. It lies inside the system file `.vh.sf`,
which covers the first 7 MiB of the volume.

| Offset | Size | Field | Seen on VMFS 6.82 |
|---|---|---|---|
| 0x00 | 4 | magic | 0x2FABF15E **[V]**; 0x2FABF15F is VMFS-L **[1]** |
| 0x04 | 4 | major version | 24 **[V]** (14 is VMFS5 **[2]**) |
| 0x08 | 1 | minor version | 82 **[V]**; ESXi prints "6.82" |
| 0x09 | 16 | datastore UUID | **[V]** |
| 0x19 | 4 | configuration flags | 0x4006 **[V]** |
| 0x1D | 128 | label | **[V]** |
| 0x9D | 4 | disk block size | 512 **[V]** |
| 0xA1 | 8 | file block size | 1 MiB **[V]** |
| 0xA9 | 4 | creation time, seconds since 1970 | **[V]** |
| 0xAD | 4 | snapshot id | 1 **[V]** |
| 0xB1 | 32 | LVM volume UUID (16 bytes), then its snapshot id | **[V]** |
| 0xD1 | 4 | offset of the first cluster group in `.fdc.sf` | 0x10000 **[V]** |
| 0xD5 | 4 | clusters per group in `.fdc.sf` | 8 **[V]** |
| 0xD9 | 4 | sub-block size | 64 KiB **[V]** |
| 0xDD | 4 | journal slots per transaction | 64 **[1]** |
| 0xE1 | 8 | volume offset of the descriptor of `.pb2.sf` | 0x72C000 **[V]** |
| 0xE9 | 4 | address of the descriptor of `.pb2.sf` | 0x01800004 **[V]** |
| 0x105 | 8 | volume offset of the descriptor of `.sdd.sf` | 0x72E000 **[V]** |
| 0x10D | 4 | address of the descriptor of `.sdd.sf` | 0x01C00004 **[V]** |
| 0x130 | 4 | physical disk block size | 512 **[V]** |
| 0x134 | 4 | metadata alignment | 4096 **[V]** |
| 0x138 | 2 | shift from small to large file block | 9, so a large file block is 512 MiB **[V]** |
| 0x13E | 2 | pointer block shift | 16 **[V]** |
| 0x140 | 2 | small file block address bits | 44 **[V]** |
| 0x144 | 4 | "to be zeroed" granularity | 1 MiB **[V]** |
| 0x148 | 4 | journal block size | 2 MiB **[V]** |

Known configuration flags **[1]**: 0x4 more sub-blocks per cluster, 0x100 local, 0x400 shared, 0x800 system data,
0x1000 vSAN.

A UUID prints as `%08x-%08x-%04x-` followed by six bytes in hex: time (low, high), a random part and the MAC
address of the creating host **[V]**.

## What OpenVmfs accepts

Exactly the values in the "Seen" column for: major version, minor version, file block size, sub-block size,
metadata alignment, the small-to-large shift, both disk block sizes and the "to be zeroed" granularity. Anything
else is refused as unsupported. VMFS 6.81 (reported for ESXi 6.5 **[1]**) is refused until a sample has been read.

The LVM volume UUID at 0xB1 must equal the UUID of the LVM volume the filesystem was found on.

## Derived sizes

With an alignment unit `A` (4096):

| Quantity | Rule | Value |
|---|---|---|
| File descriptor size | 2 × A | 8192 |
| Lock block | at 0, size A | |
| File metadata | at A | 0x1000 |
| Resident data | the last A − 512 bytes | at 0x1200, 3584 bytes |
| Address array | the last 2560 bytes | at 0x1600, 320 entries of 8 bytes |
| Pointer block | 64 KiB | 8192 entries of 8 bytes |
| Small file blocks per cluster | 1 << shift | 512 |

The rules for other alignments exist in one implementation **[1]** and are not used.

## Finding the system file descriptors

The file descriptors live in `.fdc.sf`, but that file's own descriptor has to be found first. System files have
fixed descriptor numbers, and their descriptors sit at a position that follows from the descriptor **[V]**:

```
base   = round_down_to_MiB(A × 1024 + 0x3FFFFF) + offset of the first cluster group      (0x700000 + 0x10000)
first  = base + clusters per group × 2A                                                  (+ 8 × 8192 = 0x720000)
number n is at first + n × 2A
```

| n | File | Role |
|---|---|---|
| 0 | root directory | |
| 1 | `.fbb.sf` | file block allocation |
| 2 | `.fdc.sf` | file descriptors |
| 3 | `.pbc.sf` | pointer blocks; empty on VMFS6 **[V]** |
| 4 | `.sbc.sf` | sub-blocks, which also hold the pointer blocks |
| 5 | `.vh.sf` | volume header: the first 7 MiB of the volume |
| 6 | `.pb2.sf` | second pointer-block file |
| 7 | `.sdd.sf` | a system directory |
| 8 | `.jbc.sf` | journal blocks |
| 9 | `.unmap.sf` | unmap bookkeeping |
| 10 | `.dfd.sf` | unused on the samples |

The address of descriptor `n` is `(n << 22) | 4` (see [addresses](30-addresses-resources.md)). OpenVmfs checks that
the root directory is found at the same place through `.fdc.sf` as through this rule.
