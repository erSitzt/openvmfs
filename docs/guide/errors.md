# Errors and refusals

Every error has a **kind** and a message that names the structure, the field, the value and the offset involved.
The kinds are the same in all interfaces:

| Kind | Rust | C | .NET | CLI exit code |
|---|---|---|---|---|
| The device could not be read | `Io` | `OVMFS_ERR_IO` | `Io` | 1 |
| The data contradicts the format | `Corrupt` | `OVMFS_ERR_CORRUPT` | `Corrupt` | 1 |
| Not verified, refused | `Unsupported` | `OVMFS_ERR_UNSUPPORTED` | `Unsupported` | 3 |
| A host had the disk open for writing | `InUse` | `OVMFS_ERR_IN_USE` | `InUse` | 4 |
| No such file, directory or volume | `NotFound` | `OVMFS_ERR_NOT_FOUND` | `NotFound` | 1 |
| A wrong argument | `InvalidInput` | `OVMFS_ERR_INVALID_INPUT` | `InvalidInput` | 1 |

## Why OpenVmfs refuses things

VMFS has no public specification. What is known comes from reading other implementations and from experiments, and
the older open tools show what happens when a reader guesses: they return wrong data without an error (stale bytes
from never-written blocks, data from the wrong place on large volumes). OpenVmfs therefore reads only layouts and
values that were seen on volumes written by ESXi, and answers everything else with `Unsupported`. That error does
not say the data is damaged. It says: this reader will not vouch for it.

## `InUse`

> "vm1/vm1-flat.vmdk" was open for writing on host 6afdfb51-... when this image was taken (lock Exclusive); its
> newest writes may be missing while later ones are present.

The image of the datastore was taken while a VM was running on this disk. **Fix:** take a VM snapshot before the
storage snapshot and open the descriptor below it ([backup.md](backup.md)). A lock can also be stale: the host
crashed while it had the file open. The file's newest metadata is then in that host's journal, where ESXi finds it
when it takes the datastore over.

Overrides: `replay_journal` applies what the host's journal holds and then opens the disk (experimental).
`accept_in_use` opens it as it is, which gives you a disk with missing blocks.

## `Unsupported`: the common cases

| Message contains | Cause | What you can do |
|---|---|---|
| `raw device mapping` | The disk's data lives on another LUN | Read that LUN directly |
| `I/O filter or encryption` | The disk is encrypted or passes through a filter | Nothing, without the keys and the filter |
| `VMFSSPARSE` / `COWD` | A snapshot delta in the format older VMFS versions used | Consolidate the snapshot on ESXi |
| `not closed cleanly` | A snapshot delta that a running VM was writing to | Open the level below it |
| `parent ... is on another datastore` | A snapshot chain that crosses datastores | Open both datastores and read the levels yourself, or consolidate |
| `zero grain (type 2) with further bits set` | A delta entry in a form that was never seen on ESXi (plain zero grains are read) | Tell us: we would like the sample. `accept_unverified` reads it as zeros |
| `to be zeroed` bitmap | A marker value that was never seen | Tell us. `accept_unverified` treats the block as never written |
| `LVM major version`, `VMFS-L`, `major version` other than 24 | VMFS5, a boot-device volume, or something newer than VMFS6 | Not supported |
| `minor version` | VMFS 6.81 or another version without a verified sample | Tell us |
| `extended extent metadata` | An LVM layout that was not seen up to 17 TiB | Tell us |
| `copy-on-write` | A file with copy-on-write blocks | Not supported |
| `journal ...` (only with `replay_journal`) | The journal holds something other than the transactions seen so far | Open without replay; disks that were in use stay refused |

Programs should not match on the message. Every `Unsupported` error carries a reason for counting and reporting:

| Reason | Rust `UnsupportedReason` | C `OVMFS_REASON_` | .NET `VmfsUnsupportedReason` |
|---|---|---|---|
| Not verified against data written by ESXi (every refusal without a reason of its own) | `Unverified` | `UNVERIFIED` | `Unverified` |
| Raw device mapping | `RawDeviceMapping` | `RDM` | `RawDeviceMapping` |
| Encrypted, or another I/O filter | `Encrypted` | `ENCRYPTED` | `Encrypted` |
| VMFS3 or VMFS5 | `OtherVmfsVersion` | `OTHER_VMFS_VERSION` | `OtherVmfsVersion` |
| VMFS-L | `VmfsL` | `VMFS_L` | `VmfsL` |
| Snapshot delta in the COWD format | `LegacySparse` | `LEGACY_SPARSE` | `LegacySparse` |
| A chain or extent that points outside the datastore | `ChainLeavesDatastore` | `CHAIN_LEAVES_DATASTORE` | `ChainLeavesDatastore` |

Read it with `Error::unsupported_reason()`, `ovmfs_error_reason(error)` or `VmfsException.UnsupportedReason`. More
reasons may be added; treat an unknown one like "not verified".

"Tell us" means: such a volume is exactly what is missing to verify the case. A description of how it came to be
(ESXi version, how the datastore was created) is valuable even without the image.

## `InvalidInput`: the common cases

| Message | Cause |
|---|---|
| `the datastore spans 2 devices (naa..., naa...), but 1 was given` | Pass all devices of the datastore. The message names them as ESXi knew them |
| `the device has 2 VMFS partitions` | Choose one with the partition option |
| `a descriptor is a small text file` | You passed the `-flat.vmdk` or `-sesparse.vmdk` file. Pass the descriptor |
| `goes beyond the end of the disk` | A read past the capacity |
| `has size 0` | A raw device whose size cannot be detected (Windows). Supply it with `FileSource::with_len` |

## `NotFound`

`no VMFS volume found` means the device has neither a VMFS logical volume at its start nor a partition table with
a VMFS partition. Check that you opened the right device, and the whole LUN rather than some other partition.

## `Corrupt`

The metadata contradicts itself: a signature is wrong, an offset points outside the volume, a snapshot's parent has
a different content id than the snapshot expects (`the parent was changed after the snapshot was taken, or the
chain is broken`). OpenVmfs stops at the first contradiction. If the volume mounts fine on ESXi, it is either a
bug in OpenVmfs or a layout it does not know and should have refused: please report it with the message.

## Reporting

A useful report has: the full message, the output of `openvmfs info`, the ESXi version that wrote the datastore,
and, when the problem is with one file, `openvmfs stat` of it. Images of datastores contain your data and host
names; do not send one without thinking about that.
