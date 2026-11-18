# Virtual disks: the VMDK descriptor and flat files

A virtual disk on a datastore consists of a small text file, the descriptor (`disk.vmdk`), and the file it names.
The descriptor grammar is documented by VMware ("Virtual Disk Format 5.0" technical note); what ESXi really writes
differs in a few names.

## Descriptor

A descriptor written by ESXi for a 69 GiB thin disk **[V]**:

```
# Disk DescriptorFile
version=1
encoding="UTF-8"
CID=fffffffe
parentCID=ffffffff
createType="vmfs"

# Extent description
RW 144703488 VMFS "normal-flat.vmdk"

# The Disk Data Base 
#DDB

ddb.adapterType = "lsilogic"
ddb.geometry.cylinders = "9007"
ddb.geometry.heads = "255"
ddb.geometry.sectors = "63"
ddb.longContentID = "bd746094172e7bea0b3805d8fffffffe"
ddb.thinProvisioned = "1"
ddb.uuid = "60 00 C2 9d 7b b2 3a ab-42 e8 01 7f 14 20 40 f3"
ddb.virtualHWVersion = "14"
```

- Lines starting with `#` are comments. Keys are not case-sensitive.
- `version`: 1, or 3 when changed block tracking is enabled (then `changeTrackPath` names the tracking file).
- `CID` and `parentCID`: 32-bit content ids in hex. `parentCID=ffffffff` means the disk has no parent.
- `parentFileNameHint`: path of the parent's descriptor, present in snapshot links.
- **Extent line:** access (`RW`, `RDONLY`, `NOACCESS`), size in 512-byte sectors, type, file name in double quotes,
  and an optional offset. ESXi writes exactly one extent per descriptor.
- `ddb.*`: the disk database. `ddb.thinProvisioned = "1"` marks a thin disk.

The file name is everything between the first and the last double quote of the line; names may contain spaces and
non-ASCII characters. How ESXi writes a name that itself contains a double quote is **[?]**.

### Decide by extent type, not by `createType`

The technical note and real files use different `createType` names for the same thing. The extent type is
consistent:

| Extent type | Meaning | `createType` written by ESXi | Extent file |
|---|---|---|---|
| `VMFS` | flat file | `vmfs` **[V]** | `disk-flat.vmdk` |
| `SESPARSE` | snapshot delta, space-efficient sparse | `seSparse` **[V]** | `disk-000001-sesparse.vmdk` **[V]** |
| `VMFSSPARSE` | snapshot delta, older COWD format | `vmfsSparse` **[1]** | `disk-000001-delta.vmdk` |
| `VMFSRDM` | raw device mapping | `vmfsRawDeviceMap` **[V]**, `vmfsPassthroughRawDeviceMap` **[1]** | `disk-rdm.vmdk`, `disk-rdmp.vmdk` |
| `VMFSRAW` | raw device | | a device path |

VMFS6 creates only SESparse deltas; COWD deltas come from VMFS5 and NFS datastores **[1]**.

## Flat files

The flat file has the full size of the disk, and the byte at offset `n` of the disk is the byte at offset `n` of
the file **[V]**. Thin, lazy-zeroed thick and eager-zeroed thick disks differ only in the file's block map:

| Disk type | Block map |
|---|---|
| thin | blocks are allocated when first written; the rest are holes **[V]** |
| lazy-zeroed thick | all blocks allocated, marked "to be zeroed" until first written **[V]** |
| eager-zeroed thick | all blocks allocated and zeroed **[V]** |

So the list of ranges that hold data comes from the filesystem, not from the VMDK layer. `Disk::data_ranges`
returns the data extents of the flat file; holes and "to be zeroed" ranges read as zeros and are not listed.

## What OpenVmfs opens

A descriptor with `version` 1 or 3, UTF-8 encoding, no parent, exactly one extent of type `VMFS` with offset 0,
whose file is a regular file next to the descriptor and exactly `sectors × 512` bytes long.

Refused as unsupported, each with its own message:

- snapshot links (a parent content id or a parent hint), `SESPARSE` and `VMFSSPARSE` extents;
- raw device mappings (`VMFSRDM`, `VMFSRAW`); the mapping file itself is a file of type 6 without block addresses;
- `ddb.iofilters`, `ddb.sidecars` or keys naming encryption: the disk passes through an I/O filter, for example
  VM encryption (`vmwarevmcrypt`), and its content is not readable **[1]**;
- extent files given as absolute paths (`/vmfs/volumes/<uuid>/...`), which may point to another datastore;
- a flat file whose length differs from the size in the descriptor.

A snapshot link written by ESXi 7.0.3 for the disk `grow` **[V]** has `CID=dd4a9120`, `parentCID=7b555d49` (the
base's `CID`), `createType="seSparse"`, `parentFileNameHint="grow.vmdk"` and the extent
`RW 819200 SESPARSE "grow-000001-sesparse.vmdk"`. The hint is a bare file name in the same directory.

## Changed block tracking

The `-ctk.vmdk` file has no public format description. Incremental backups get their changed ranges from vSphere
(`QueryChangedDiskAreas`) while the VM snapshot exists, and then read those ranges from the storage snapshot.

## Which descriptor a backup reads

After "VM snapshot, then array snapshot", the array snapshot contains the base disk, any older deltas, and a new
active delta. The backup wants the disk as it was when the VM snapshot was taken: the chain **below** the active
delta. For a VM without earlier snapshots that is the base descriptor, which OpenVmfs can read today. For a VM with
earlier snapshots it is a chain of SESparse deltas ([80-sesparse.md](80-sesparse.md)).
