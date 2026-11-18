# Reading a virtual disk by hand

This page follows one read from the first sector of a device to the first sector of a virtual disk, with nothing
but `xxd` and arithmetic. Every hex dump is real: it comes from the test image `fixture-zero-v1`, a 4 GiB device
that ESXi 7.0.3 partitioned and formatted. You can repeat every step:

```bash
zstd -d fixtures/vmfs6/fixture-zero-v1.img.zst -o fz.img
```

The goal: the first sector of the virtual disk `pat/thin.vmdk`, a thin disk of 1 GiB.

All numbers on disk are little-endian: the bytes `00 08 00 00` are the number 0x800. The reference for every
structure is in [`../format/`](../format/00-overview.md); this page is the guided tour.

## The layers

```
device ─► partition ─► logical volume ─► filesystem ─► file ─► virtual disk
          (GPT)        (LVM)             (VMFS6)
```

Each layer has its own offsets. We will meet three kinds:

- **device offset**: from the first byte of the disk;
- **partition offset**: from the first byte of the VMFS partition;
- **volume offset**: from the first byte of the logical volume. Everything inside the filesystem uses these.

## 1. The partition

A GUID partition table has its header in sector 1 and its entries from sector 2.

```console
$ xxd -s 0x200 -l 16 -g 1 fz.img
00000200: 45 46 49 20 50 41 52 54 00 00 01 00 5c 00 00 00  EFI PART....\...
$ xxd -s 0x400 -l 48 -g 1 fz.img
00000400: 2a e0 31 aa 0f 40 db 11 95 90 00 0c 29 11 d1 b8  *.1..@......)...
00000410: e2 d7 59 ff 07 89 54 41 ae bb ee 5c 0e ad dc 2d  ..Y...TA...\...-
00000420: 00 08 00 00 00 00 00 00 de ff 7f 00 00 00 00 00  ................
```

The first 16 bytes of the entry are the partition type, `AA31E02A-400F-11DB-9590-000C2911D1B8` in the mixed byte
order of GUIDs: the type of a VMFS partition. At 0x20 of the entry is its first sector, 0x800 = 2048.

**The partition starts at device offset 2048 × 512 = 0x100000.**

## 2. The logical volume

VMFS does not sit directly in the partition. In between is a small volume manager (LVM), which is how a datastore
can span several devices. Its header is 1 MiB into the partition, so at device offset 0x200000:

```console
$ xxd -s 0x200000 -l 16 -g 4 fz.img
00200000: 0dd001c0 06000000 15000000 000a0000  ................
```

`0xC001D00D` is the magic; then major version 6 (the LVM of VMFS6) and minor version 0x15 = 21.

The header area holds a table of *extents*: pieces of the device that make up the volume. It starts 0x82000 after
the header. Entries are 128 bytes apart:

```console
$ xxd -s 0x282000 -l 48 -g 1 fz.img
00282000: 01 00 00 00 00 00 00 00 00 00 00 10 01 00 00 00  ................
00282010: 00 00 00 00 00 00 00 00 00 00 00 00 f0 00 00 00  ................
00282020: 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00  ................
```

| Bytes | Field | Value |
|---|---|---|
| 0 | in use | 1 |
| 9–16 | where the piece starts in the partition | 0x1100000 (17 MiB) |
| 17–24 | where it starts in the volume | 0 |
| 25–32 | its length | 0xF0000000 (3.75 GiB) |

One entry covers the whole volume. So for this datastore:

**device offset = 0x100000 (partition) + 0x1100000 + volume offset = 0x1200000 + volume offset.**

Never hard-code that. Above 16 TiB a volume has a second entry that starts 16 MiB further along, and a datastore on
two devices has entries on both.

## 3. The filesystem descriptor

The filesystem's descriptor is at volume offset 0x200000, so at device offset 0x1400000:

```console
$ xxd -s 0x1400000 -l 48 -g 1 fz.img
01400000: 5e f1 ab 2f 18 00 00 00 52 fe fb fd 6a 12 07 b9  ^../....R...j...
01400010: 33 a4 da 00 50 56 8b 55 83 06 00 00 00 66 7a 00  3...PV.U.....fz.
```

`0x2FABF15E` is the VMFS magic. Major version 0x18 = 24 and minor version 0x52 = 82: ESXi calls this VMFS 6.82. The
16 bytes from offset 9 are the datastore's UUID, and at 0x1D is its label, `fz`.

Three more fields matter for reading:

```console
$ xxd -s 0x14000a1 -l 8 -g 4 fz.img      # file block size
014000a1: 00001000 00000000                    ........
$ xxd -s 0x14000d1 -l 12 -g 4 fz.img     # position of the first file descriptors, sub-block size
014000d1: 00000100 08000000 00000100           ............
```

A file block is 0x100000 bytes (1 MiB). A sub-block is 0x10000 bytes (64 KiB). The first two numbers of the second
dump (0x10000 and 8) say where the descriptors of the filesystem's own files are; with the metadata alignment of
4096 they put the first of them, the root directory's, at **volume offset 0x720000**.

## 4. A file descriptor

A file descriptor is what other filesystems call an inode. On VMFS6 it is 8192 bytes: a lock block, then a
metadata block. The root directory's, at device offset 0x1920000:

```console
$ xxd -s 0x1920000 -l 16 -g 4 fz.img     # lock block
01920000: 0100c010 00007200 00000000 00803800  ......r.......8.
$ xxd -s 0x1921000 -l 32 -g 4 fz.img     # metadata block
01921000: 04000000 01000000 05000000 02000000  ................
01921010: 00000000 00200100 00000000 00001000  ..... ..........
```

The lock block starts with its type, 0x10C00001 ("this is a file descriptor"), and its own volume offset, 0x720000:
a reader can check that it is where it thinks it is. Later in the lock block is how hosts hold the file; it matters
a great deal when the datastore was in use ([`../consistency.md`](../consistency.md)).

In the metadata block: the descriptor's address (4), a generation, a link count, the type (2 = directory), and from
offset 0x14 the length, 0x12000 bytes.

Where is the content? At 0x44 is the *addressing mode*, and from 0x600 of the metadata block (0x1600 of the
descriptor) an array of 320 addresses:

```console
$ xxd -s 0x1921044 -l 4 -g 4 fz.img
01921044: 01000000                             ....
$ xxd -s 0x1921600 -l 16 -g 8 fz.img
01921600: 0100000000004000 0000000000000000  ................
```

Mode 1 means: the array holds the addresses of the file's blocks. There is one, 0x0040000000000001.

## 5. Addresses

Everything a file refers to is named by an address. The low three bits are its type:

| Type | Name | Refers to |
|---|---|---|
| 1 | SFB, small file block | a 1 MiB block of the volume |
| 2 | SB, sub-block | a 64 KiB piece of the file `.sbc.sf` |
| 4 | FD, file descriptor | a piece of the file `.fdc.sf` |
| 5 | PB2, pointer block | a piece of the file `.pb2.sf` |
| 7 | LFB, large file block | a 512 MiB block of the volume |

0x0040000000000001 has type 1. In a small-file-block address, bits 15–45 are a *cluster* and bits 51–63 a
*resource* inside the cluster; a cluster holds 512 blocks.

```
0x0040000000000001 >> 51          = 8      resource
(0x0040000000000001 >> 15) & ...  = 0      cluster
(0 × 512 + 8) × 1 MiB             = 0x800000
```

**The root directory's content is at volume offset 0x800000**, device offset 0x1A00000.

## 6. A directory

```console
$ xxd -s 0x1a00000 -l 16 -g 4 fz.img
01a00000: 0100f500 0c000000 0c000000 01000000  ................
```

A directory starts with a header of 0x10000 bytes (version 0x00F50001, then the number of entries, 12). After it
come blocks of 4096 bytes, each with a 64-byte header and 14 slots of 288 bytes. The entry for `pat` is in block 1,
slot 8, at directory offset 0x10000 + 1 × 4096 + 64 + 8 × 288 = 0x11940:

```console
$ xxd -s $((0x1a00000 + 0x11940)) -l 32 -g 4 fz.img
01a11940: 02000000 44060000 01000000 48d43790  ....D.......H.7.
01a11950: 40190100 00000000 70617400 00000000  @.......pat.....
```

Type 2 (a directory), then the address of its file descriptor, 0x00000644, and at 0x18 the name.

Directory entries hold 32-bit addresses of type 4. In those, bits 6–21 are the cluster and bits 22–31 the resource:

```
0x644 & 7            = 4     file descriptor
(0x644 >> 6) & 0xFFFF = 25   cluster
0x644 >> 22          = 0     resource
```

ESXi prints this address as `<4, 25, 0>`.

## 7. Resource files

Cluster 25, resource 0 of what? Of the file `.fdc.sf` in the root directory, which holds all file descriptors.
VMFS keeps its metadata in such *resource files*: `.fdc.sf` for file descriptors, `.sbc.sf` for sub-blocks,
`.pb2.sf` for pointer blocks. They are ordinary files of the filesystem, and that is the knot in VMFS: to read a
file you need its descriptor, which is in a file. The knot is cut by the fixed position from step 3: the
descriptors of the root directory and of the resource files themselves are at known places.

A resource file begins with a header:

```console
$ openvmfs cat fz.img .fdc.sf | xxd -l 0x14 -g 4
00000000: 00010000 08000000 00000100 00200000  ............. ..
00000010: 00000101                             ....
```

| Field | Value |
|---|---|
| resources per cluster | 0x100 = 256 |
| clusters per group | 8 |
| offset of the first cluster group | 0x10000 |
| size of a resource | 0x2000 = 8192 |
| size of a cluster group | 0x1010000 |

A group starts with a header of 8192 bytes for each of its 8 clusters, then the resources. Cluster 25 is cluster 1
of group 3:

```
0x10000                     first group
+ 3 × 0x1010000             three groups before ours
+ 8 × 8192                  the cluster headers of our group
+ 1 × 256 × 8192            one cluster before ours
+ 0 × 8192                  resource 0
= 0x3250000                 an offset in the file .fdc.sf
```

A file offset is not a volume offset: `.fdc.sf` has a block map like any file.

```console
$ openvmfs map fz.img .fdc.sf
     file offset           length  kind      volume offset    device offset
             0x0         0x100000  data           0x700000        0x1900000
        0x100000        0x7f90000  data           0xa00000        0x1c00000
```

File offset 0x3250000 is 0x3150000 into the second piece: volume offset 0xA00000 + 0x3150000 = **0x3B50000**.
`openvmfs stat fz.img pat` confirms it: `descriptor: volume offset 0x3b50000`.

(The first megabyte of `.fdc.sf` sits at volume offset 0x700000, and its first cluster group starts 0x10000 into
the file, with 0x10000 of cluster headers: 0x700000 + 0x20000 = 0x720000, the fixed position from step 3. The two
ways of finding the root directory agree, and OpenVmfs checks that they do.)

## 8. The disk's file

The same steps for `pat` find the entry `thin-flat.vmdk` with the address 0x00000F04, which is `<4, 60, 0>`, and
the arithmetic of step 7 puts its descriptor at volume offset 0x8190000. `openvmfs fd` prints a descriptor without
the arithmetic:

```console
$ openvmfs fd fz.img pat/thin-flat.vmdk | xxd -s 0x1000 -l 0x48 -g 4
00001000: 040f0000 01000000 01000000 03000000  ................
00001010: 09000000 00000040 00000000 00001000  .......@........
00001020: 00000000 05000000 00000000 7cfcfd6a  ............|..j
00001030: 7cfcfd6a 93fcfd6a 00000000 00000000  |..j...j........
00001040: 80010000 03000000                    ........
```

Type 3 (a regular file), length 0x40000000 (1 GiB), block size 0x100000, 5 blocks allocated, and at 0x44 the
addressing mode: **3, indirect**. A file of 1 GiB needs 1024 block addresses, and the array has room for 320. So
the array holds the addresses of *pointer blocks*, each of which holds 8192 block addresses.

```console
$ openvmfs fd fz.img pat/thin-flat.vmdk | xxd -s 0x1600 -l 16 -g 8
00001600: 820e000000000000 0000000000000000  ................
```

0x0000000000000E82: type 2, a sub-block. In sub-block addresses, bits 6–41 are the cluster and bits 56–63 the
resource: cluster 58, resource 0, in `.sbc.sf`.

```console
$ openvmfs cat fz.img .sbc.sf | xxd -l 0x14 -g 4
00000000: 00010000 08000000 00000100 00000100  ................
00000010: 00000108                             ....
```

256 resources per cluster, 8 clusters per group, resources of 0x10000 bytes, groups of 0x8010000 bytes. Cluster 58
is cluster 2 of group 7:

```
0x10000 + 7 × 0x8010000 + 8 × 8192 + 2 × 256 × 0x10000 + 0 = 0x3A090000
```

```console
$ openvmfs map fz.img .sbc.sf
     file offset           length  kind      volume offset    device offset
             0x0       0x14000000  data          0x8b00000        0x9d00000
      0x14000000       0x2c090000  data         0x2de00000       0x2f000000
```

File offset 0x3A090000 is in the second piece: volume offset 0x2DE00000 + 0x26090000 = 0x53E90000, device offset
0x55090000.

## 9. The pointer block

Entry *n* of the pointer block is the address of megabyte *n* of the file:

```console
$ xxd -s 0x55090000 -l 16 -g 8 fz.img                    # entries 0 and 1
55090000: 0180010000000000 0000000000000000  ................
$ xxd -s $((0x55090000 + 5 * 8)) -l 8 -g 8 fz.img        # entry 5
55090028: 0180010000000800                   ........
```

- Entry 0 is 0x0000000000018001: a small file block, cluster 3, resource 0. (3 × 512 + 0) MiB = volume offset
  **0x60000000**.
- Entry 1 is 0: megabyte 1 of the disk has no block. This is what "thin" means. A read there returns zeros.
- Entry 5 is 0x0008000000018001: cluster 3, resource 1, volume offset 0x60100000.

`openvmfs vmdk map fz.img pat/thin.vmdk` prints the same map:

```
     file offset           length  kind      volume offset    device offset
             0x0         0x100000  data         0x60000000       0x61200000
        0x100000         0x400000  hole                  -                -
        0x500000         0x100000  data         0x60100000       0x61300000
```

## 10. The data

Volume offset 0x60000000 is device offset 0x61200000:

```console
$ xxd -s 0x61200000 -l 32 -g 1 fz.img
61200000: 4f 56 4d 46 53 50 41 54 01 00 00 00 01 00 00 00  OVMFSPAT........
61200010: 00 00 00 00 00 00 00 00 88 3d f2 c8 01 01 01 01  .........=......
```

This is the first sector of the virtual disk. The test data is self-describing: `OVMFSPAT`, the number of the disk
(1), a generation (1), the number of the sector (0), a checksum. Whatever a reader returns for this disk, the bytes
themselves say whether they are the right ones.

## And the descriptor?

`pat/thin.vmdk`, the file a user thinks of as "the disk", is 468 bytes of text:

```console
$ openvmfs cat fz.img pat/thin.vmdk
# Disk DescriptorFile
version=1
encoding="UTF-8"
CID=fffffffe
parentCID=ffffffff
createType="vmfs"

# Extent description
RW 2097152 VMFS "thin-flat.vmdk"

# The Disk Data Base 
#DDB

ddb.adapterType = "lsilogic"
ddb.geometry.cylinders = "512"
ddb.geometry.heads = "128"
ddb.geometry.sectors = "32"
ddb.longContentID = "125c269ac4d904b1b056e6c4fffffffe"
ddb.thinProvisioned = "1"
ddb.uuid = "60 00 C2 94 3f 4f a5 49-2a 13 53 aa 27 e9 9f 56"
ddb.virtualHWVersion = "14"
```

It names the extent file and the size in sectors. A file this small needs no blocks at all: its descriptor has
addressing mode 0x10D1, *resident*, and the text sits inside the descriptor itself, at offset 0x1200.

## What the tour left out

| Topic | Where |
|---|---|
| Thick disks: blocks that are allocated but must read as zeros ("to be zeroed") | [`../format/40-file-descriptor.md`](../format/40-file-descriptor.md) |
| Large file blocks of 512 MiB | [`../format/30-addresses-resources.md`](../format/30-addresses-resources.md) |
| Files over 2.5 TiB: pointer blocks that point to pointer blocks | [`../format/40-file-descriptor.md`](../format/40-file-descriptor.md) |
| Small files in sub-blocks | [`../format/40-file-descriptor.md`](../format/40-file-descriptor.md) |
| How a directory finds its blocks and free slots | [`../format/50-directories.md`](../format/50-directories.md) |
| Snapshots: SESparse deltas and chains | [`../format/80-sesparse.md`](../format/80-sesparse.md) |
| Datastores over 16 TiB or on several devices | [`../format/10-partition-lvm.md`](../format/10-partition-lvm.md) |
| Locks, heartbeats and the journal | [`../format/60-heartbeat-journal.md`](../format/60-heartbeat-journal.md) |
