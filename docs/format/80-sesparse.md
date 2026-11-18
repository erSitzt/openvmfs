# SESparse snapshot deltas

Not implemented yet. This is what is known, for the next milestone.

Test data is ready: `fixtures/vmfs6/fixture-chain-v1` holds four disks with one SESparse delta each, written by
ESXi 7.0.3 while a guest was writing, together with ESXi's own consolidated reading of the chains
(`vmkfstools -i`): a reader of `guest/grow-000001.vmdk` must produce the SHA-256 recorded for
`clone/grow-now-flat.vmdk`.

VMware publishes no description of SESparse. Everything here comes from the read-only support in QEMU, which was
worked out from files and from the log lines ESXi writes when it creates a snapshot **[1]**.

A delta file holds the blocks ("grains", 4 KiB) written since the snapshot. A read of a grain goes to the newest
delta first and falls through to its parent, down to the base disk.

## Constant header

At byte 0, 512 bytes, all fields 64-bit; offsets and sizes are in sectors.

| Offset | Field | Required value |
|---|---|---|
| 0 | magic | 0x00000000CAFEBABE |
| 8 | version | 0x0000000200000001 |
| 16 | capacity | |
| 24 | grain size | 8 (4 KiB) |
| 32 | grain table size | 64 (4096 entries) |
| 40 | flags | 0 |
| 80, 88 | volatile header offset, size | |
| 96, 104 | journal header offset, size | |
| 112, 120 | journal offset, size | |
| 128, 136 | grain directory offset, size | |
| 144, 152 | grain tables offset, size | |
| 160, 168 | free bitmap offset, size | |
| 176, 184 | back map offset, size | |
| 192, 200 | grains offset, size | |

## Volatile header

At its offset, 512 bytes: magic 0x00000000CAFECAFE, free grain table number, next transaction number, and a
"replay journal" flag. A non-zero flag means the file was not closed cleanly; a reader that cannot replay the
journal must refuse it.

## Grain directory and grain tables

- A directory entry is 0 (no table) or has 0x10000000 in its upper half and a table index in its lower half.
- A table has 4096 entries and covers 16 MiB.
- A table entry carries its type in the top four bits:

| Type | Meaning |
|---|---|
| 0 | unallocated: read the parent |
| 1 | unmapped by the guest: **[?]** zeros according to QEMU, parent according to dissect |
| 2 | zero grain |
| 3 | allocated: grain index = bits 48–59 as the low 12 bits, bits 0–47 above them |

Data of grain `i` is at `grains offset + i × grain size`.

## Chains

A link names its parent with `parentFileNameHint`, usually a bare file name in the same directory, sometimes an
absolute path. The child's `parentCID` must equal the parent's `CID`; a mismatch means the chain is broken and must
be refused.

## To be settled with ESXi

1. Type 1 entries: write data in the base, take a snapshot, discard the range in the guest, and compare with what
   ESXi itself reads.
2. Headers of disks larger than 2 TiB.
3. A delta whose volatile header says "replay journal" in a snapshot of a running VM: expected for the active
   delta, which a backup does not read.
