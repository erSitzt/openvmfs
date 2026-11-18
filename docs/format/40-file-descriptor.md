# File descriptors and block maps

A file descriptor is the inode of VMFS. On VMFS6 it is 8192 bytes: a lock block, then a metadata block that also
holds resident data and the address array.

## Lock block (offset 0, 4096 bytes)

| Offset | Size | Field | Seen |
|---|---|---|---|
| 0x00 | 4 | lock type | 0x10C00001 for file descriptors **[V]** |
| 0x04 | 8 | volume offset of this descriptor | **[V]** |
| 0x0C | 8 | volume offset of the owner's heartbeat slot | **[1]** |
| 0x14 | 8 | heartbeat generation | **[1]** |
| 0x24 | 4 | lock mode: 0 free, 1 exclusive, 2 read-only, 3 multi-writer | 0 **[V]** |
| 0x28 | 16 | owner UUID | **[1]** |
| 0x38 | 8 | modification time of the lock | **[1]** |

An unused descriptor slot is all zeros **[V]**. OpenVmfs checks the lock type and that the recorded position equals
the position the descriptor was read from. It does not interpret locks.

## File metadata (offset 0x1000)

| Offset | Size | Field | Notes |
|---|---|---|---|
| 0x00 | 4 | own address | must equal the address used to find it **[V]** |
| 0x04 | 4 | generation | |
| 0x08 | 4 | link count | |
| 0x0C | 4 | type: 2 directory, 3 regular file, 4 symbolic link, 5 system file, 6 raw device mapping | **[V]** for all five |
| 0x10 | 4 | flags | 0x8 on `.sbc.sf`, 0xA on thick disks, 0x1 on a small file **[V]**; meaning unknown |
| 0x14 | 8 | length in bytes | **[V]** |
| 0x1C | 8 | block size of this file | 1 MiB, or 64 KiB for files made of sub-blocks **[V]** |
| 0x24 | 8 | number of allocated blocks | **[V]** |
| 0x2C | 12 | modification, change and access time, 4 bytes each, seconds since 1970 | |
| 0x38 | 8 | uid, gid | |
| 0x40 | 4 | mode | permission bits; directories also carry the directory bit **[V]** |
| 0x44 | 4 | addressing mode (below) | **[V]** |
| 0x48, 0x54 | 4 + 4 | number of "to be zeroed" blocks, low and high half | **[V]** |
| 0x4C, 0x58 | 4 + 4 | number of copy-on-write blocks, low and high half | **[1]** |
| 0x5C | 4 | number of pointer blocks | 1 on `.sbc.sf` **[V]** |
| 0x64 | 4 | signature `fdmd` = 0x66646D64 | **[V]** |
| 0x6C | 4 | "to be zeroed" granularity shift | 20 **[V]** |
| 0x70 | 4 | address of the parent directory's descriptor | **[1]** |
| 0x89 | 1 | block offset shift: log2 of the block size | 20, or 16 for sub-block files **[V]** |
| 0x8C | 4 | number of large file blocks | 0, or 4 for a 2 GiB thick disk on a large datastore **[V]** |

System files that are directories (`.sdd.sf`) have type 5 and the directory bit (0o040000) in their mode **[V]**.

OpenVmfs refuses descriptors without the signature and descriptors that count copy-on-write blocks.

## Addressing modes

| Value | Name | The address array holds |
|---|---|---|
| 0 | none | nothing; seen on raw device mappings **[V]** |
| 1 | file blocks | block addresses **[V]** |
| 2 | sub-blocks | sub-block addresses **[V]** |
| 3 | indirect | pointer blocks **[V]** |
| 5 | indirect, second type | pointer blocks **[V]** (on `.sbc.sf`) |
| 0x10D0 | double indirect | pointer blocks that hold pointer blocks **[V]** |
| 0x10D1 | resident | nothing: the data is in the descriptor **[V]** |

- **Resident:** the data is at offset 0x1200 of the descriptor, up to 3584 bytes **[V]**. A 5-byte file and a
  25-byte symbolic link were stored this way.
- **Address array:** 320 entries of 8 bytes at offset 0x1600 **[V]**.
- **Pointer block:** 8192 entries of 8 bytes (64 KiB) **[V]**. Where it lives follows from the type of its address:
  a sub-block in `.sbc.sf` **[V]** (every regular file seen so far), a resource of `.pb2.sf` **[V]** (the
  system file `.sbc.sf` itself), or a resource of `.pbc.sf` **[1]**. A pointer
  block stored in a file block **[1]** is refused.

Capacity with 1 MiB blocks: 320 MiB direct, 2.5 TiB with one level, far more than the 62 TiB file limit with two.
On the samples a 69 GiB thin disk used one level and a 42 TiB thin disk two **[V]**.

## From file offset to address

```
block  = file offset >> block offset shift
direct:           address = array[block]
indirect:         address = pointer_block(array[block / 8192])[block % 8192]
double indirect:  address = pointer_block(pointer_block(array[block / 8192²])[(block / 8192) % 8192])[block % 8192]
```

A zero in the array or in a pointer block means that nothing below it is allocated. It is a **hole**, not the end
of the file **[V]**: thin disks consist mostly of such zeros. The older tools stopped at the first zero and returned
truncated or wrong data.

## From address to bytes

The entry for a block names what backs `block size` bytes of the file starting at `block × block size`:

| Address | Bytes are at |
|---|---|
| null | nowhere: zeros |
| SFB | `SFB offset + (file offset mod 1 MiB)` **[V]** |
| SB | `sub-block offset + (file offset mod 64 KiB)` **[V]** |
| LFB | `LFB offset + (file offset mod 512 MiB)` **[V]** |

A file made of sub-blocks has a block size of 64 KiB and one entry per sub-block; a 100 KiB directory had two
**[V]**.

### Large file blocks **[V]**

A thick disk on a larger datastore is made of large file blocks. Its pointer block still has **one entry per
MiB**, and the address of a large file block is **repeated in all 512 entries it backs**; the position inside the
block is the file offset modulo 512 MiB. A 1 GiB eager-zeroed disk has entries 0–511 naming block 132 and entries
512–1023 naming block 133.

A file can mix both kinds: a 1300 MiB disk had 1024 entries naming two large file blocks, followed by 276 small
file block addresses in another part of the device.

When ESXi uses them: on the three datastores we formatted, `vmkfstools -Ph` reported
`Large File Blocks (total/used/file block clusters)` as `8/0/8` (4 GiB), `32/0/32` (16 GiB) and `512/0/256`
(256 GiB). So ESXi sets aside up to 256 large file blocks (128 GiB) for small-block clusters, and thick disks get
large file blocks only from what is left. On the two small datastores even a 2 GiB thick disk consisted of small
file blocks (`numLFB 0`); on the 256 GiB one it got four large ones. Where the limit lies exactly is an inference
from these three points.

## "To be zeroed"

Space that is allocated but has never been written still holds whatever was on the device before. Its address
carries a "to be zeroed" bitmap (bits 7 to 14). Such a block must read as zeros; returning what is on disk would
leak stale data into a backup.

Verified on datastores that were filled with recognisable junk before the test disks were created **[V]**:

- The mark is **bit 7** (bitmap value 1) of the entry for that MiB. On disk the MiB still holds the junk; ESXi
  reads it as zeros.
- It works per entry, and there is one entry per MiB, for small and large file blocks alike. A 2 GiB lazy-zeroed
  disk made of four large file blocks had 2048 marked entries after creation and 2042 after writing into six
  different MiBs; the entries of one large block then differ only in that bit.
- The first write into a MiB, even of three sectors, makes ESXi zero the rest of that MiB on disk and clear the
  bit. A MiB is therefore either entirely "to be zeroed" or entirely valid.
- The file descriptor counts these entries, and `vmkfstools -t0` shows them as `VMFS Z-`.
- An eager-zeroed thick disk has none; a thin disk has holes instead.

OpenVmfs reads an entry with bitmap value 1 as zeros and reports the range as `ExtentKind::ToBeZeroed` (with its
volume offset, separate from holes). Any other bitmap value has never been seen and is refused unless
`OpenOptions::accept_unverified` is set.

## The block map

OpenVmfs resolves the complete map when a file is opened: a sorted list of extents without gaps, each either data
at a volume offset, "to be zeroed" at a volume offset, or a hole. Neighbouring blocks that are adjacent on the
volume are merged. The last block is clipped to the file length. A missing pointer block becomes one hole for
everything below it, so a 42 TiB thin disk with one written MiB has three extents.

Checks while building: every address must decode cleanly, a block must lie inside the volume, an entry must not
back more bytes than its block has, and the file must fit its addressing mode.
