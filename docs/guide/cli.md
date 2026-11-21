# The `openvmfs` command

```
openvmfs <command> <device> [arguments]
```

`<device>` is an image file or a block device: a whole disk with a partition table, or the VMFS partition itself.
For a datastore that spans several devices, give all of them separated by commas, in any order. Paths inside the
datastore are written with `/`, relative to its root; a leading `/` is optional.

Ready-made programs for Linux x64 (statically linked) and Windows x64 are attached to each release on GitHub
(`openvmfs-<version>-linux-x64.tar.gz`, `openvmfs-<version>-windows-x64.zip`, with `SHA256SUMS`);
`tools/pack-cli.sh` builds the same archives.

On Windows a raw disk is `\\.\PhysicalDriveN` (the number is the one `Get-Disk` shows), from an elevated prompt:

```
openvmfs.exe info \\.\PhysicalDrive3
```

The disk is opened read-only and read unbuffered in whole sectors. It may be offline and read-only, which is how
Windows should keep a snapshot LUN; `openvmfs` does not bring it online and changes nothing.

Examples below use `fixtures/vmfs6/fixture-zero-v1.img.zst`, unpacked to `fz.img`.

## Commands for the volume

### `info`

```console
$ openvmfs info fz.img
label:              fz
uuid:               6afdfbfe-33b90712-daa4-0050568b5583
version:            VMFS 6.82
capacity:           4026531840 bytes (3.75 GiB)
file block size:    1.00 MiB
partition offset:   0x100000
lvm volume:         6afdfbfe-13f86ecc-ad3b-0050568b5583 (LVM 6.21)
lvm device:         6afdfbfe-31dc6002-fef6-0050568b5583
created on:          
created at:         1795030014 (seconds since 1970)
active heartbeats:  0 (no host was holding locks)
journal:            nothing pending in 0 journal(s)
```

| Line | Meaning |
|---|---|
| `uuid` | The datastore UUID, as in `/vmfs/volumes/<uuid>` on ESXi |
| `partition offset` | Where the VMFS partition starts on the device; 0 when the device is the partition |
| `lvm device` | One line per device for a datastore on several devices, with each device's share of the volume and the name ESXi knew it by |
| `created on` | Vendor and model of the device the datastore was created on, when ESXi recorded them |
| `active heartbeats` | Hosts that were holding locks (running VMs, open files) when the image was taken. 0 does not mean "unmounted" |
| `journal` | Transactions that hosts had written to their journal but not yet to their final place. More than 0 means: files that were open for writing lack their newest changes |

### `ls`, `stat`, `cat`, `hash`

```console
$ openvmfs ls fz.img pat
-       1073741824 1795030140 thin-flat.vmdk
-              468 1795030121 thin.vmdk
```

Columns: type (`d` directory, `-` file, `l` symbolic link, `s` system file, `r` raw device mapping), size in bytes,
modification time in seconds since 1970, name.

```console
$ openvmfs stat fz.img pat/thin-flat.vmdk
address:     <4, 60, 0> (0x00000f04)
descriptor:  volume offset 0x8190000, device offset 0x9390000
kind:        Regular
length:      1073741824 bytes (1.00 GiB)
blocks:      5 allocated, 0 never written
addressing:  Indirect
mode:        600  uid 0  gid 0  links 1
times:       mtime 1795030140  ctime 1795030140  atime 1795030163
generation:  1
lock:        free
```

| Line | Meaning |
|---|---|
| `address` | The file descriptor's address, printed the way `vmkfstools -D` prints it |
| `descriptor` | Where the file descriptor is, for looking at it with a hex editor |
| `blocks` | Allocated blocks, and how many of them were never written (they read as zeros) |
| `addressing` | How the blocks are found: directly, through pointer blocks, in sub-blocks, or inside the descriptor |
| `lock` | `free`, or how a host had the file open when the image was taken. `Exclusive` on a virtual disk means: do not trust its content, see [backup.md](backup.md) |

`cat` writes a file to standard output, `hash` prints its SHA-256. Both work on every file, including the system
files (`.fdc.sf`, `.sbc.sf`, ...) and files that are locked.

### `map`

The block map of a file: where each part of it lives.

```console
$ openvmfs map fz.img pat/thin-flat.vmdk
     file offset           length  kind      volume offset    device offset
             0x0         0x100000  data         0x60000000       0x61200000
        0x100000         0x400000  hole                  -                -
        0x500000         0x100000  data         0x60100000       0x61300000
```

`data` is written data, `tbz` is allocated but never written (reads as zeros, although the device holds stale bytes
there), `hole` is unallocated (reads as zeros). The device offsets equal what `vmkfstools -t0` prints on ESXi. On a
datastore with several devices, a last column names the device.

### `fd`

Writes the raw file descriptor of a file (8192 bytes) to standard output, for studying the format:

```console
$ openvmfs fd fz.img pat/thin-flat.vmdk | xxd -s 0x1600 -l 16 -g 8
00001600: 820e000000000000 0000000000000000  ................
```

[../internals/walkthrough.md](../internals/walkthrough.md) explains what these bytes mean.

### `manifest`

One line per entry of the whole datastore: type, size, SHA-256 (for files up to a size limit, default 256 MiB) and
path, separated by tabs. Made for comparing two readers, or two images.

```console
$ openvmfs manifest fz.img 1048576
l	12	c47311ca98dba21a633a282ebd6f23ef49af6e9935049051a6df89964ed4ff2c	/files/link
-	7	5f5d584c5857d85af911ade1b2ae7cb593c17654282091f3ace31efd9e951360	/files/many/f0001
```

## Commands for virtual disks

All of them take the path of the **descriptor**: the small `.vmdk` text file, not the `-flat.vmdk` or
`-sesparse.vmdk` file next to it.

### `vmdk info`

```console
$ openvmfs vmdk info chain.img guest/grow-000001.vmdk
capacity:     419430400 bytes (400.00 MiB)
create type:  seSparse
content id:   dd4a9120
thin flag:    no
version:      1
link:         guest/grow-000001.vmdk (seSparse, content id dd4a9120): SeSparse "grow-000001-sesparse.vmdk"
link:         guest/grow.vmdk (vmfs, content id 7b555d49): Vmfs "grow-flat.vmdk"
data:         398458880 bytes (380.00 MiB) in 1 ranges
```

`link` lines are the snapshot chain, newest first. A disk without snapshots has one. To read a disk as it was when
a snapshot was taken, name the descriptor that was current at that time: here `guest/grow.vmdk` is the disk at the
moment of the snapshot, and `guest/grow-000001.vmdk` is everything written since on top of it. `version: 3` means
that changed-block tracking is switched on for the disk.

### `vmdk map`

The block map of the base disk's flat file, as `map` prints it. Snapshot deltas are not part of it.

### `vmdk hash`

The SHA-256 of the whole disk, zeros included: the same value as `sha256sum` of a raw export. With `--chunks`, one
line per MiB that holds data, with its offset; two disks can then be compared without reading their holes.

### `vmdk export`

Writes the disk to a raw image file (sparse), or to standard output with `-`.

```bash
openvmfs vmdk export /dev/mapper/snapshot-lun vm1/vm1.vmdk vm1.raw
openvmfs vmdk export /dev/mapper/snapshot-lun vm1/vm1.vmdk - | qemu-img convert -f raw -O qcow2 /dev/stdin vm1.qcow2
```

## Environment variables

| Variable | Effect |
|---|---|
| `OPENVMFS_DIRECT=1` | Read without the operating system's page cache (`O_DIRECT`). Use it when reading a LUN |
| `OPENVMFS_REPLAY_JOURNAL=1` | Experimental: read the volume with its journals replayed, as ESXi would see it after a crash. Disks that were in use can then be opened. See [../consistency.md](../consistency.md) |
| `OPENVMFS_ACCEPT_IN_USE=1` | Open disks although a host was writing to them. Their content is then unreliable; for looking at damaged data only |
| `OPENVMFS_ACCEPT_UNVERIFIED=1` | Read structures whose meaning is known from other implementations but was never verified against ESXi. For verification work; the data may be wrong |

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Error: the device could not be read, the data is damaged, or something does not exist |
| 2 | Wrong usage |
| 3 | Unsupported: the data may be fine, but reading it would be a guess |
| 4 | In use: a host was writing to the disk when the image was taken |

A script that falls back to another way of reading should do so on 3 and 4, and stop on 1.
