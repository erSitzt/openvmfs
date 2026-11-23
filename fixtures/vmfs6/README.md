# Fixtures written by ESXi

Each fixture is the image of a small disk that carries a VMFS6 datastore created by ESXi, plus what ESXi itself
reported about it. Images are stored compressed (`*.img.zst`) and unpacked as sparse files into
`target/fixture-cache` by the test kit.

Reference data is written by [`../recipes/pattern.py`](../recipes/pattern.py): every 512-byte sector names the disk
it belongs to and its own sector number. Junk sectors, written before the test files exist, name themselves too.

## fixture-zero-v1

| | |
|---|---|
| Written by | ESXi 7.0.3 (build 23307199), running nested, 2026-11-18 |
| Disk | 4 GiB, GUID partition table, VMFS partition from sector 2048 |
| Datastore | `fz`, VMFS 6.82, UUID `6afdfbfe-33b90712-daa4-0050568b5583`, automatic unmap switched off |
| State | cleanly unmounted before imaging; `voma` found 0 errors (`truth/voma.txt`) |
| Image | `fixture-zero-v1.img.zst`, 4 GiB unpacked, about 2.2 GiB of it non-zero |
| Changed after capture | 11 bytes: the host's IP address in its heartbeat slot (disk offset 0x1588036) was overwritten with zeros |

### How it was made

1. `partedUtil mklabel` / `setptbl` with the VMFS partition type, then `vmkfstools -C vmfs6 -S fz`.
2. `esxcli storage vmfs reclaim config set --volume-label=fz --reclaim-priority=none`.
3. **Junk:** `pattern.py junk junk.bin 2300` filled the free space with junk sectors; the file was then deleted.
   Blocks allocated afterwards still hold that junk on disk until they are written.
4. Test disks with `vmkfstools -c`, then `pattern.py write` into the `-flat.vmdk` files from the ESXi shell:

| Disk | Size | Type | Disk id | Sectors written (first, count) |
|---|---|---|---|---|
| `pat/thin.vmdk` | 1 GiB | thin | 1 | (0, 2048) (10248, 16) (614400, 4096) (2095104, 2048) |
| `pat/lazy.vmdk` | 300 MiB | zeroedthick | 2 | (2049, 3) (204800, 2048) (614392, 8) |
| `pat/eager.vmdk` | 64 MiB | eagerzeroedthick | 3 | (20480, 2048) |
| `pat/big.vmdk` | 1100 MiB | zeroedthick | 4 | (0, 16) (1046528, 4096) (2150400, 2048) (2252799, 1) |

5. Files: `files/resident.txt` (14 bytes), `files/sub.bin` (20 KiB, disk id 10), `files/blocks.bin` (3 MiB + 512
   bytes, disk id 11), `files/many/f0001` to `f0300`, and the symbolic link `files/link`.
6. Ground truth collected on the ESXi shell into `fixture-zero-v1/truth/`:

| File | Content |
|---|---|
| `sha256.txt` | `sha256sum` of every disk and file, read by ESXi through VMFS |
| `<disk>.D.txt`, `<disk>.before.D.txt` | `vmkfstools -D` of the flat file, after and before the writes |
| `<disk>.t0.txt`, `<disk>.before.t0.txt` | `vmkfstools -t0` of the descriptor: ESXi's block map |
| `volume.txt` | `vmkfstools -Ph -v10` |
| `voma.txt` | `voma -m vmfs -f check` on the unmounted volume |
| `ls.txt` | directory listing |

7. Unmounted, checked with `voma`, imaged with `dd` from the disk device.

### What it showed

- The disk is found through its partition table; all four disks and all files read identical to ESXi's hashes and
  to the write plan, sector by sector.
- A never-written block of the lazy-zeroed disks has bit 7 set in its address and holds junk on disk; ESXi and
  OpenVmfs both read zeros. A write of three sectors into such a block made ESXi zero the rest of the block.
- OpenVmfs's block map of `lazy` equals `truth/lazy.t0.txt`. The offsets `vmkfstools -t0` prints are offsets on
  the whole disk.
- The 1100 MiB thick disk uses small file blocks only (`numLFB 0`); `vmkfstools -Ph` reports
  `Large File Blocks (total/used/file block clusters): 8/0/8` for this volume. Large file blocks need a bigger
  datastore.
- A second image taken while the datastore was mounted and idle has no heartbeat slot in use either.

### Not covered

Large file blocks, snapshot deltas, data written by a guest, a datastore in use by running VMs.

## fixture-lfb-v1

| | |
|---|---|
| Written by | ESXi 7.0.3 (build 23307199), running nested, 2026-11-18 |
| Disk | 256 GiB (thin), GUID partition table, VMFS partition from sector 2048 |
| Datastore | `f2`, VMFS 6.82, UUID `6afe0153-138d6a42-11e3-0050568b5583`, automatic unmap switched off |
| State | cleanly unmounted before imaging; `voma` found 0 errors (`truth/voma.txt`) |
| Image | **partial**: `fixture-lfb-v1.parts.zst` holds the captured ranges, `fixture-lfb-v1.ranges` lists them |
| Changed after capture | 11 bytes: the host's IP address in its heartbeat slot (disk offset 0x1588036) was overwritten with zeros |

Why this one exists: ESXi uses large file blocks only on datastores above 128 GiB, and a device that size cannot be
imaged in full. Captured were the first 2 GiB, every data extent ESXi listed for the test disks, the first two and
the last MiB of every "to be zeroed" extent, and two further MiB that a read trace showed to be needed
(`recipes/capture_partial.py`, `recipes/fetch_missing.py`, packed with `recipes/pack_partial.py`). Those two were
fetched after the datastore had been mounted again. In tests, a read outside the captured ranges is an error.

### How it was made

1. Datastore as for `fixture-zero-v1`.
2. **Junk:** a 5 GiB lazy-zeroed disk (10 large file blocks) was filled with junk sectors (`pattern.py junkfill`)
   and deleted.
3. Test disks, written from the ESXi shell:

| Disk | Size | Type | Large blocks | Disk id | Sectors written (first, count) |
|---|---|---|---|---|---|
| `pat/lfb-lazy.vmdk` | 2 GiB | zeroedthick | 4 | 21 | (0, 8) (1048560, 32) (1433600, 2048) (3147777, 3) (4194303, 1) |
| `pat/lfb-eager.vmdk` | 1 GiB | eagerzeroedthick | 2 | 22 | (2048, 2048) (1052672, 16) |
| `pat/lfb-mixed.vmdk` | 1300 MiB | zeroedthick | 2, plus 276 small blocks | 23 | (0, 16) (2097152, 2048) (2662399, 1) |
| `pat/thin2.vmdk` | 2 GiB | thin | 0 | 24 | (4096, 8) (2097152, 2048) (4194296, 8) |

4. Ground truth in `fixture-lfb-v1/truth/`, as for `fixture-zero-v1`.

### What it showed

- All four disks read identical to ESXi's hashes and to the write plan.
- The address of a large file block is repeated in each of the 512 per-MiB entries it backs, and each entry has
  its own "to be zeroed" bit. The second write range of `lfb-lazy` crosses the boundary between two large blocks.
- `lfb-mixed` got the two large blocks that had held junk: its never-written MiBs hold junk on the device and read
  as zeros.
- OpenVmfs's block map of `lfb-mixed` equals `truth/lfb-mixed.t0.txt`.

## fixture-guest-v1

The datastore of `fixture-zero-v1` a little later, imaged **while a VM was running on it**.

| | |
|---|---|
| Written by | a guest (Alpine Linux 3.24, paravirtual SCSI) inside ESXi 7.0.3, 2026-11-18 |
| State | datastore mounted, VM powered on with all three disks open, guest idle after `sync`; one heartbeat slot in use |
| Image | **partial**: only the ranges the tests read, plus one MiB of each never-written extent (`recipes/subset_image.py`, `recipes/pack_partial.py`) |
| Changed after capture | the host's IP address in its heartbeat slot was overwritten with zeros |

How it was made: `guest/src.vmdk` (64 MiB, thin) was filled with pattern 30 from the ESXi shell. The guest then
copied sector ranges from it to the same sector numbers of two empty disks with `dd`:

| Disk | Size | Type | Sectors written by the guest (first, count) |
|---|---|---|---|
| `guest/thin-g.vmdk` | 256 MiB | thin | (0, 16) (20480, 2048) (100000, 5) |
| `guest/lazy-g.vmdk` | 128 MiB | zeroedthick | (2049, 3) (40960, 2048) |

The hashes in `fixture-guest-v1/truth/sha256.txt` were computed by ESXi after the VM had been powered off (it cannot
read the files while the VM holds them).

What it showed: guest writes leave the same metadata as writes from the ESXi shell, and an image taken while the
VM runs reads identical to what ESXi reads afterwards. Under the never-written blocks of `lazy-g` the device holds
junk.

## fixture-chain-v1

The same datastore after the second consistency probe (`docs/consistency.md`): the guest VM is powered off and has
one VM snapshot, so every disk in `guest/` is a base (`x.vmdk`, `x-flat.vmdk`) plus a SESparse delta
(`x-000001.vmdk`, `x-000001-sesparse.vmdk`). Full image, cleanly unmounted, `voma` 0 errors, IP address blanked.

| Disk | Base | Delta |
|---|---|---|
| `guest/grow` | 400 MiB thin, 156 MiB written before the snapshot | 224 MiB written after it |
| `guest/thin-g`, `guest/lazy-g`, `guest/src` | as in `fixture-guest-v1` | empty |

`clone/grow-now.vmdk` and `clone/thin-g-now.vmdk` are ESXi's own consolidation of the chains (`vmkfstools -i`).
`fixture-chain-v1/truth/sha256.txt` has the hashes of bases, clones and delta files, `*.vmdk.txt` the descriptors,
`grow.chain.txt` the output of `vmkfstools -q -v1`. `pat/big.vmdk` of `fixture-zero-v1` was deleted to make room.

## probe-crash2

`probe-crash2/esxi.txt`: what ESXi reported about the base disk in the second consistency probe.

## fixture-chain-v2

The same datastore after a guest session with two more VM snapshots and discards (`blkdiscard`) in between.
Partial image: only what the tests read.

`guest/thin-g` now has three deltas. What the guest did (offsets in MiB; the base holds data at 0, 10 and 48):

| Level | Written | Discarded |
|---|---|---|
| `thin-g-000001` | 20, 21 | 10 (data in the base), 100–101 (never written), 21 (just written) |
| `thin-g-000002` | 8 sectors at sector 20544 (inside the discarded MiB 10), 30 | 20 (written one level below) |
| `thin-g-000003` | 8 sectors at 0 | |

`guest/grow` got two more, empty deltas.

`fixture-chain-v2/truth/`: `sha256.txt` with the base, the clones ESXi consolidated from each level
(`clone/thin-g-at1..3`) and the delta files; the descriptors; `thin-g-atN.t0.txt` with ESXi's block map of each
clone. `qemu-img convert` (7.2) produces the same content as ESXi at all three levels.

## fixture-big-v1

A 17 TiB datastore (thin device, ESXi 7.0.3). It was filled with four lazily allocated disks of 4100 GiB, so that
the two test disks `t/thin` (1 GiB, thin) and `t/lazy` (2 GiB, lazy-zeroed) lie above the 16 TiB mark, where the
logical volume has a second extent behind a 16 MiB gap. Partial image: 92 MiB of the device, 0.2 MiB packed.

`fixture-big-v1/truth/`: ESXi's hashes of both flat files, their block maps (`*.t0.txt`), `vmkfstools -D` and
`-Ph` output, the partition table.

## fixture-big-esxi8-v1

A 40 TiB datastore made by ESXi 8.0 U3 (thin device), so its logical volume has three extents: one more behind a
16 MiB gap at every 16 TiB. Filled with eight lazily allocated disks of 4100 GiB in two rounds (`fill/`), with a
thin disk of 1 GiB and a lazy-zeroed disk of 2 GiB created after each round (`t1/`, `t2/`). ESXi put `t1/lazy`
into the second extent and the other three into the third, near the end of the volume. Partial image: 105 MiB of
the device, 0.2 MiB packed.

`fixture-big-esxi8-v1/truth/`: ESXi's hashes of the four flat files, their block maps (`*.t0.txt`), `vmkfstools -D`
and `-Ph` output, the descriptors, the partition table.

## fixture-span-v1

A datastore that ESXi 7.0.3 created on a 3 GiB device and then extended onto a 2 GiB device (`vmkfstools -Z`).
Two whole-device images: `fixture-span-v1-dev1.img.zst` and `fixture-span-v1-dev2.img.zst`. `d/lazy` (2600 MiB,
lazy-zeroed) has blocks on both devices; `d/thin` lies on the second one.

`fixture-span-v1/truth/`: ESXi's hashes, block maps, extent list (`extents.txt`), `voma` result (0 errors).

## fixture-span-esxi8-v1

The same on ESXi 8.0 U3: a datastore created on a 4 GiB device and extended onto a second one. `pat/lazy`
(4500 MiB, lazy-zeroed) has blocks on both devices, `pat/thin` (1 GiB) on the first. Two whole-device images.
`truth/` holds ESXi's hashes, block maps, `vmkfstools -D` and `-Ph` output and the extent list. The host's address
in a heartbeat slot is blanked.

## fixture-span3-esxi8-v1

A datastore that ESXi 8.0 U3 created on a 4 GiB device and extended onto a second and a third (`vmkfstools -Z`),
with a lazy-zeroed disk created after each step (`pat/l1` to `l3`) and a thin one (`pat/t`). Then the first device
was enlarged to 8 GiB, its partition resized and the datastore grown in place (`vmkfstools --growfs`), and `pat/l4`
(4000 MiB) created, most of which lies in the new space.

What it showed: the first device's new space is **appended at the end of the volume**, behind devices two and
three. The first device then holds two separate parts of the volume (0 to 3.75 GiB and 11.25 to 15.25 GiB).
OpenVmfs up to 0.4.1 assumed one part per device and failed to open this datastore.

Three whole-device images (0.9 MiB packed). `truth/`: ESXi's hashes, block maps, `vmkfstools -D` and `-Ph` output,
the extent list. The host's address in a heartbeat slot is blanked.

## fixture-grow-esxi8-v1

The single-device case: a datastore on a 4 GiB device, the device enlarged to 8 GiB, the partition resized and the
datastore grown (`vmkfstools --growfs`), ESXi 8.0 U3. `pat/l1` (2000 MiB, lazy-zeroed) was created before the
growth, `pat/l2` (3500 MiB) and `pat/t` (1 GiB thin) after it; `l2` lies in the new space. One whole-device image
(0.4 MiB packed), `truth/` as above. OpenVmfs 0.4.1 read this one correctly already.

Also measured on the 256 GiB datastore of this host, without a fixture:

- A chain of **31 deltas**, the deepest ESXi allows (the 32nd snapshot fails with "Snapshot hierarchy is too
  deep"), with a guest write and a discard before each snapshot: top and middle level have the SHA-256 of ESXi's
  consolidation. Opening the top takes 989 reads.
- A disk attached with **multi-writer sharing**: read with ESXi's hash while the VM is off. While the VM runs its
  lock is mode 3 with a zero owner and the host in a list behind it (`docs/format/40-file-descriptor.md`), and
  OpenVmfs refuses the disk as in use.

## fixture-span3-esxi7-v1, fixture-grow-esxi7-v1

The same two recipes as `fixture-span3-esxi8-v1` and `fixture-grow-esxi8-v1`, on ESXi 7.0.3: a datastore on three
4 GiB devices whose first device was enlarged to 8 GiB and grown in place, and a single-device datastore grown the
same way. Same disks, same sizes. ESXi 7.0.3 lays the volume out exactly as 8.0 U3 does: the first device's new
space is at the end of the volume. Whole-device images (0.9 MiB and 0.4 MiB packed), `truth/` as above.

Also measured on this host without a fixture: a chain of 31 deltas (the 32nd snapshot fails here too; top and
middle level have the SHA-256 of ESXi's consolidation) and a multi-writer disk (lock mode 3 with one owner in the
list while the VM runs, refused as in use; read with ESXi's hash after power-off).

## probe-series1

No image, only the record of seven consistency runs (`results.json`), made with
`fixtures/recipes/consistency_probe.py`; see `docs/consistency.md`.

## fixture-inuse-v1

A storage snapshot of the probe datastore, taken about a second after a VM snapshot, while the guest ran the
marker workload of `fixtures/recipes/consistency_marker.py`. `probe/m.vmdk` is the disk below the VM snapshot
(locked read-only, complete up to marker 1401); `probe/m-000001.vmdk` is the level the VM was writing to (delta
locked exclusively, "replay journal" flag set). Partial image.

`fixture-inuse-v1/truth/marker-runs.jsonl`: the record of four runs of the same workload **without** a VM
snapshot, all of them inconsistent.

## fixture-journal-v1

A storage snapshot of the probe datastore, taken four seconds after the guest started a burst of the marker
workload, with no VM snapshot. Two block allocations of `probe/m-flat.vmdk` wait in the host's journal: read as it
is, the disk lacks chunks 1696 to 1700 although its marker says 1700; with the journal replayed it is complete.
Partial image.

`fixture-journal-v1/truth/`: `sha256.txt` with the hash of the same disk on an image taken seven minutes later,
when nothing was waiting; `marker-runs-replay.jsonl` with four more runs, each read with and without replay.

## fixture-basic-esxi8-v1 and fixture-chain-esxi8-v1

The first fixtures from **ESXi 8.0 U3** (build 24022510); it formats VMFS 6.82 like ESXi 7.0.3.

- `fixture-basic-esxi8-v1`: the datastore that `fixtures/recipes/make_basic.sh` builds: thin, lazy-zeroed (over
  junk), eager-zeroed and far-apart thin disks, small files, a symlink, a directory with 300 files. `truth/` holds
  what the script recorded on ESXi; `fixtures/recipes/compare_truth.py` compares an image with such a record.
- `fixture-chain-esxi8-v1`: a disk with three VM snapshots, with the same guest writes and discards as
  `fixture-chain-v2`. Partial image. `truth/sha256.txt` has the base and ESXi's consolidated clone of each level.
  ESXi 8 writes 0x200 into a SESparse header field that ESXi 7 leaves at 0.

## fixture-double-v1

Two thin disks larger than 2.5 TiB (4 TiB and 2600 GiB) on a 256 GiB datastore, ESXi 7.0.3. Files of that size use
**double indirection** (`zla 4304` in `truth/*-flat.D.txt`). Written at the start, on both sides of the 2.5 TiB line
and at the end. Partial image. The disks are too large to hash on ESXi, so the reference is the written pattern,
sector by sector, and ESXi's block map (`truth/*.t0.txt`); `fixtures/recipes/check_chunks.py` does the same check on
any image. The same disks made by ESXi 8.0 U3 read identical too (not kept as a fixture).

## fixture-many-esxi8-v1

A 48 GiB datastore on which ESXi 8.0 U3 created 100,697 files of 8 KiB (`recipes/make_many.py`), to see what ESXi
does when its pools run out. What it did (`truth/esxi.txt`):

- It grew the descriptor pool from its initial size to 102,848 descriptors and the sub-block pool to 66,816
  sub-blocks (261 clusters), a few clusters at a time and far slower than the files arrived.
- A file created while no sub-block was free got a whole 1 MiB file block instead (`zla 1`). About a third of the
  files ended up that way; the datastore used 38 GiB for 0.8 GiB of content.
- Sub-block clusters 256 to 260 are in use. That is where the older tools read sub-block addresses in two
  different ways.

Every file holds a hash of its own number, so no list of hashes is needed. On the full image all 100,697 files read
as written. Partial image (39 MiB of the device, 0.1 MiB packed): the two directories and seven files the tests read,
among them `d098/f0255` and `d098/f0256`, the first sub-blocks of clusters 255 and 256.

## fixture-pb2-esxi8-v1

The same datastore later, after `recipes/make_pb2.py` made ESXi grow the sub-block pool from 67,328 to 131,840
sub-blocks (`truth/esxi.txt`). What it showed:

- ESXi enlarges the pool only for something that must have a sub-block: a pointer block or a directory. Each time
  it added 512 sub-blocks. The recipe alternates one sparse file that needs a pointer block with about 500 small
  files that use the new sub-blocks up; 254 rounds took 40 minutes.
- At 131,072 sub-blocks `.sbc.sf` passes 8 GiB, which is all one pointer block covers, and gets a second one in
  `.pb2.sf` (`numPointerBlocks 2`, "Secondary Ptr Blocks" 256/254). Its address is cluster 0, resource 1.
- `.pbc.sf` stayed empty throughout.

OpenVmfs up to 0.4.1 refused that second address, and with it the whole datastore. Partial image (62 MiB of the
device, 0.1 MiB packed): three directories and the files the tests read, on both sides of the 8 GiB mark; four of
them were also hashed on ESXi.

Two earlier runs on 16 GiB datastores filled them before reaching cluster 256 and are not kept: 56,848 files on ESXi
7.0.3 and 41,775 on ESXi 8.0 U3, all read as written.

## fixture-pb2-v1

The same on ESXi 7.0.3, on a fresh 256 GiB datastore: `recipes/make_pb2.py` from round 0 to 131,840 sub-blocks in
449 rounds and 70 minutes, about 132,000 files of 8 KiB (`truth/esxi.txt`). ESXi 7.0.3 enlarged the pool from
16,384 to 32,000 sub-blocks in one step and by 512 from then on; the descriptor pool went from 16,384 to 134,096.
`.sbc.sf` has the same length and the same second pointer block as on ESXi 8.0 U3. Partial image (38 MiB of the
device, 0.1 MiB packed): three directories and nine files, eight of 8 KiB and `r447/big`, all with ESXi's hashes.

The datastore also held the two guest tests that the README lists as measured for ESXi 7.0.3: a 24 GiB disk with a
delta of 1,048,576 random 4 KiB writes (818,488 runs; SHA-256 equal to ESXi's consolidation; opened with 51 reads)
and three 16 GiB thin disks filled by a guest, two front to back at the same time (pieces of 512 MiB) and one in
random megabytes (3,451 extents), with block maps identical to `vmkfstools -t0`.

## fixture-merge-esxi8-v1

Five 2 GiB thin disks of one VM on ESXi 8.0 U3, one each on a paravirtual SCSI controller (`pv`, and `cbt` with
changed-block tracking), LSI Logic SAS (`lsi`), SATA (`sata`) and NVMe (`nvme`). A Linux guest wrote 3 MiB of random
data and 40 scattered 4 KiB blocks per round and discarded parts of them (`blkdiscard`), with a VM snapshot before
each of twelve rounds. Then, with the VM running:

1. Snapshots 6 and 7 were deleted: ESXi merged their deltas into `-000004`. The discards of those two rounds are
   **zero grains** (type 2) there; the discards `-000004` received itself are still type 1.
2. After a snapshot, the guest discarded 1 MiB of data that lies in the base (98 MiB) and 1 MiB that lies in an
   older delta (292 MiB); after another snapshot and a write, the first of the two was deleted. The discards are
   zero grains in `-000012`, over data of the levels below.

The chain is base, `-000001` to `-000004`, `-000007` to `-000012`, `-000006` (top). On the full image, 25 reads
(five disks, levels `-000004`, `-000006`, `-000011`, `-000012` and the earlier top) had the SHA-256 of ESXi's
consolidation of that level (`truth/sha256.txt` for the last three). The SATA disk has no discards: the guest
reported them as not supported.

Partial image (91 MiB of the device, 4.8 MiB packed): the tables of the `pv` and `nvme` chains and nine single
megabytes, whose hashes from ESXi's consolidations are in `truth/mib.sha256.txt`.

The same procedure on ESXi 7.0.3 gave zero grains in the same places, and 20 reads (five disks, four levels) had
the hashes of ESXi's consolidations. That image is not kept.
