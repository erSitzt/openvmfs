# Directories

A VMFS6 directory is a file with a block structure. (VMFS5 used a flat array of 140-byte entries; that is not
implemented.)

## Layout

- The first **0x10000 bytes** are the header block.
- After it come blocks of one metadata alignment unit (4096 bytes). Block `n` is at file offset
  `0x10000 + n × 4096`.

An empty directory is 0x11000 bytes long; the samples' directory with 100 files is 0x19000 bytes **[V]**.

## Header block

| Offset | Size | Field | Notes |
|---|---|---|---|
| 0x000 | 4 | version | 0x00F50001 **[V]**; 0x00FDC001 on debug builds **[1]** |
| 0x004 | 4 | number of entries, including "." and ".." | **[V]** |
| 0x008 | 4 | number of allocated blocks | **[1]** |
| 0x00C | 4 | number of allocation map blocks | 1 **[V]** |
| 0x010 | 4 × 128 | block numbers of the allocation map blocks | **[V]** |
| 0x3A8 | 16 | checksum and its generation | **[1]**, algorithm unknown |
| 0x3B8 | 288 | entry for "." | **[V]** |
| 0x4D8 | 288 | entry for ".." | **[V]** |
| 0x5F8 | 4 × 16001 | hash table | **[1]** |

## Blocks

Every block starts with a 64-byte header:

| Offset | Size | Field |
|---|---|---|
| 0x00 | 2 | version |
| 0x02 | 2 | type: 1 entries, 2 links, 3 allocation map |
| 0x04 | 2 | total slots |
| 0x06 | 2 | free slots |
| 0x08 | 56 | slot bitmap |

### Allocation map block (type 3)

After the header, four bits describe each block of the directory, **the high half of each byte first** **[V]**:
bits 0–1 are the block's type, bit 2 says it still has free slots (an entry block with live entries and room has
code 5 **[V]**), bit 3 that it was never written. One map block describes
`2 × (4096 − 64)` blocks; the block number is `map index × that + position`.

### Entry block (type 1)

14 slots of 288 bytes follow the header **[V]**. A slot is in use when its bit in the slot bitmap is **0** **[V]**.

| Offset | Size | Field |
|---|---|---|
| 0x00 | 4 | type, as in the file descriptor |
| 0x04 | 4 | address of the file descriptor |
| 0x08 | 4 | generation |
| 0x0C | 2 | index into the hash table |
| 0x0E | 2 | second hash, used in link groups |
| 0x10 | 8 | offset |
| 0x18 | 256 | name, NUL-padded UTF-8. ESXi limits names to 127 bytes. |

## Listing a directory

1. Read the header; "." and ".." count towards the number of entries.
2. For each allocation map block, find the blocks of type 1.
3. In each of them, read the slots whose bitmap bit is 0, until the number of entries is reached.

OpenVmfs is strict here: the listed block must have type 1, the map block type 3, the slot count must fit the
block, names must be valid UTF-8 without `/`, addresses must be file descriptor addresses, and the number of
entries found must equal the count in the header. Directory blocks must be backed by written data.

The other bits of the map are not needed for listing. Bit 2 marks blocks with free slots, not free blocks: after a
file was deleted, the block that held it still had live entries and code 5 **[V]**. ESXi clears a freed slot and
sets its bit in the block's bitmap **[V]**; a slot marked in use that holds no entry is treated as damage.

## Lookup by name

OpenVmfs lists the directory and compares names; it does not use the hash table. For reference **[1]**: the table
has 16001 entries (15973 usable in the root directory, where system files have fixed positions), each naming a
block and a slot. The hash is Bob Jenkins' 64-bit lookup8 with seed 42 over the name in a 256-byte buffer padded
with a fixed salt. Colliding names are chained through link blocks (type 2) in groups of 12.

## Symbolic links

A symbolic link is a file of type 4 whose content is the target path **[V]**. OpenVmfs returns the target and does
not follow links.
