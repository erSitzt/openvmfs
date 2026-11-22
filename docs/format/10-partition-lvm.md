# Partition and logical volume manager

## Partition

A datastore lives in a partition of a disk with a GUID partition table. Its type GUID is
`AA31E02A-400F-11DB-9590-000C2911D1B8` **[2]**, stored on disk as
`2A E0 31 AA 0F 40 DB 11 95 90 00 0C 29 11 D1 B8` **[V]**. ESXi starts the partition at sector 2048 **[V]**.

OpenVmfs also accepts a bare partition (an image of the partition, or a device node such as `/dev/sdb1`): if the LVM
magic is found at offset 0x100000 of what was opened, it is treated as the partition **[V]**.

The GPT is parsed from the UEFI specification, with both checksums verified, trying 512-byte and then 4096-byte
sectors. A whole disk partitioned and formatted by ESXi 7.0.3 reads this way **[V]**; 4096-byte sectors have only
been tested on constructed images.

## LVM device header

At partition offset **0x100000**, magic **0xC001D00D** **[V]**.

| Offset | Size | Field | Seen |
|---|---|---|---|
| 0x00 | 4 | magic | 0xC001D00D **[V]** |
| 0x04 | 4 | major version | 6 **[V]** (5 on VMFS5 **[2]**) |
| 0x08 | 4 | minor version | 21 **[V]** |
| 0x0C | 78 | SCSI disk id: type 1, len 1, lun 2, device type 1, SCSI level 1, vendor 8, model 16, revision 4, id 44 | **[1]** |
| 0x5A | 4 | disk block size | 512 **[V]** |
| 0x5E | 8 | total bytes of the device | **[V]** |
| 0x66 | 4 | number of volumes | 1 **[V]** |
| 0x6A | 4 | number of entries in the physical-extent table | 1 **[V]**; 2 on a 17 TiB volume, 3 on a 40 TiB volume **[V]** |
| 0x6E | 4 | last physical-extent index | **[1]** |
| 0x72 | 8 | generation | **[1]** |
| 0x7A | 8 | data offset | 0x1100000 **[V]** |
| 0x82 | 16 | device UUID | **[V]** |
| 0xBE | 4 | number of extent maps | 0 **[V]**, also at 3 TiB and 17 TiB (0 means 1 **[1]**) |
| 0xC2 | 8 | offset of the next metadata region | 0 **[V]**, also at 3 TiB and 17 TiB |
| 0xCA | 4 | metadata alignment | 4096 **[V]** |
| 0xCE | 4 | number of 256 MiB extents | 8 for 2 GiB **[V]** |
| 0xD2 | 4 | flags | 0 **[V]**; 1 on VMFS-L **[V]** |

ESXi recognises a snapshot LUN by comparing the SCSI disk id stored here with the id of the device it sees. For a
reader it is information only.

## Header area

Everything is relative to the header. With LVM 6 and an alignment of 4096 **[V]**:

| Offset | Size | What |
|---|---|---|
| 0x0 | 4096 (one alignment unit) | device header |
| 0x1000 | 512 | the single volume descriptor |
| 0x1200 | 1023 × 512 − 8192 | reserved |
| 0x7F000 | 32 × 256 | device name table: on the first device, the names of all devices of the volume as ESXi knew them (`mpx.vmhba0:C0:T8:L0:1`), in volume order **[V]** |
| 0x81000 | max(alignment, 1024) | physical-extent bitmap: bit *n* of the first bytes is set when table entry *n* is in use **[V]** |
| 0x82000 | 8192 × 128 | physical-extent table |

The general rule **[1]**: device metadata (512 bytes on LVM 5, one alignment unit on LVM 6), then the volume table
(512 entries on LVM 5, one on LVM 6, 512 bytes each), then padding so that volume table, padding and device name
table together fill 1024 sectors, then pairs of bitmap and table. A metadata region holds up to 8 such pairs; more
are chained through the "next metadata region" offset, where a small header (magic, number of maps, data offset,
next offset) starts another region **[1]**.

## Volume descriptor

| Offset | Size | Field | Seen |
|---|---|---|---|
| 0x00 | 8 | logical size in bytes | 2 GiB **[V]** |
| 0x08 | 8 | generation | **[1]** |
| 0x10 | 4 | state: 1 normal, 2 resignature pending, 3 resignature done | 1 **[V]** |
| 0x14 | 64 | name | the volume UUID as text **[V]** |
| 0x54 | 16 | volume UUID | **[V]**, equals the UUID stored in the filesystem descriptor |
| 0x64 | 4 | snapshot id | 1 **[V]** |
| 0x70 | 4 | volume id, referenced by extent entries | 0 **[V]** |
| 0x74 | 4 | number of 256 MiB extents | 8 **[V]** |
| 0x78 | 8 | first 256 MiB unit of the volume on this device | 0 **[V]**; 11 on the second device of a volume whose first device holds 2.75 GiB **[V]** |
| 0x80 | 8 | last 256 MiB unit on this device | 7 **[V]** |
| 0x90 | 4 | number of devices of the volume | 1 **[V]**, 2 **[V]** |

## Physical-extent table

Entries are 128 bytes apart; 37 bytes are used.

| Offset | Size | Field |
|---|---|---|
| 0x00 | 1 | in use |
| 0x01 | 4 | extent id |
| 0x05 | 4 | volume id |
| 0x09 | 8 | physical offset in bytes, **relative to the partition** **[V]** |
| 0x11 | 8 | logical offset in bytes |
| 0x19 | 8 | length in bytes |
| 0x21 | 4 | version |

One entry can describe a long run **[V]**:

| Volume | Entries (physical offset, logical offset, length) |
|---|---|
| 2 GiB | 0x1100000, 0, 2 GiB |
| 3 TiB | 0x1100000, 0, 3 TiB − 256 MiB |
| 17 TiB | 0x1100000, 0, 16 TiB and 0x100002100000, 16 TiB, 1 TiB − 256 MiB |

So an entry covers at most 16 TiB, and the next one starts 16 MiB further on the device than the previous one
ends. Above the 16 TiB mark a volume offset is therefore 33 MiB behind its partition offset instead of 17 MiB.

## Mapping a volume offset

1. Take the entries that are in use and belong to the volume.
2. Sort them by logical offset. They must cover the volume from 0 without gaps.
3. Merge entries that are adjacent on the device.
4. A volume offset maps to `physical + (offset − logical)` of the run that contains it.

For a small volume this equals `partition offset 0x1100000 + volume offset`. The older tools hard-code that, and
read data from 16 MiB too early on volumes above 16 TiB **[V]**. OpenVmfs always maps through the table; two disks
placed above the 16 TiB mark of a 17 TiB datastore read identical to ESXi (`fixture-big-v1`).

## Volumes on several devices

A datastore can be extended onto further devices ("extents", `vmkfstools -Z`). Verified on a datastore that ESXi
7.0.3 extended from a 3 GiB onto a 2 GiB device, and on the public two-device sample of dissect.vmfs **[V]**:

- every device has its own LVM header, with its own device UUID and the same volume UUID, snapshot id, logical
  size and device count;
- each device's volume descriptor says which 256 MiB units of the volume it holds (first and last); the first
  device starts at 0 and each further device starts where the volume ended when it was added;
- a device can hold **two separate parts** of the volume **[V]**: when a device is enlarged and the datastore
  grown in place (`vmkfstools --growfs`), the new space is appended at the end of the volume and the device gets
  a second extent entry. On a three-device datastore whose first device was grown from 4 to 8 GiB, the devices'
  descriptors say units 0–60, 15–29 and 30–44: the first device holds units 0–14 and 45–60
  (`fixture-span3-esxi8-v1`). So "first and last" of one device may enclose other devices, and only the extent
  entries say what lies where;
- the extent entries of a device carry logical offsets of the whole volume, and physical offsets inside that
  device's partition;
- the filesystem does not know about devices: a file's blocks can lie on any of them, and `vmkfstools -t0` prints
  offsets on whichever device holds the block.

OpenVmfs needs all devices, in any order. With one missing it refuses and names the devices from the device name
table.

## Refused for now

| Case | Reason |
|---|---|
| A non-zero "next metadata region" offset, or more than one extent map | Never seen: volumes of 3 TiB and 17 TiB have neither. Probably needs more than 8192 extent entries on one device |
| Volume state other than 1 | Resignature in progress |
| LVM 5, alignment other than 4096, flag 1 (VMFS-L) | Out of scope or no sample |

