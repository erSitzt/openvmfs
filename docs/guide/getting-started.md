# Getting started

## What OpenVmfs is

A VMware datastore on block storage is a VMFS volume: a filesystem that only ESXi can mount. OpenVmfs reads it
without ESXi, read-only, from anything that delivers the bytes of the LUN: a block device, a snapshot of it, or an
image file. It gives you the files of the datastore and, one level up, the content of virtual disks.

It comes as:

- a command line tool, `openvmfs`;
- a Rust library, `openvmfs`;
- a C library, `openvmfs_c`, for every other language;
- a .NET binding, `OpenVmfs`.

It runs on Linux and Windows. Nothing is ever written to the device.

## Build

You need the Rust toolchain (the version is pinned in `rust-toolchain.toml`; `rustup` installs it on first use).

```bash
cargo build --release
```

This produces `target/release/openvmfs` (the tool) and `target/release/libopenvmfs_c.so` (the C library; `.dll` on
Windows). The .NET binding is built with `dotnet build bindings/dotnet/OpenVmfs.slnx` and needs the .NET 10 SDK.

## Try it on a test image

The repository carries small images of real datastores. Unpack one:

```bash
zstd -d fixtures/vmfs6/fixture-zero-v1.img.zst -o /tmp/fz.img     # 4 GiB, sparse
```

What is it?

```console
$ openvmfs info /tmp/fz.img
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

What is on it?

```console
$ openvmfs ls /tmp/fz.img pat
-       1153433600 1795030140 big-flat.vmdk
-              441 1795030121 big.vmdk
-       1073741824 1795030140 thin-flat.vmdk
-              468 1795030121 thin.vmdk
...
```

A virtual disk is the small text file (`thin.vmdk`, the *descriptor*) plus the file it names. Always pass the
descriptor:

```console
$ openvmfs vmdk info /tmp/fz.img pat/thin.vmdk
capacity:     1073741824 bytes (1.00 GiB)
create type:  vmfs
content id:   fffffffe
thin flag:    yes
version:      1
link:         pat/thin.vmdk (vmfs, content id fffffffe): Vmfs "thin-flat.vmdk"
data:         5242880 bytes (5.00 MiB) in 4 ranges

$ openvmfs vmdk export /tmp/fz.img pat/thin.vmdk /tmp/thin.raw
```

`/tmp/thin.raw` is now the disk as a guest would see it: 1 GiB, of which 5 MiB hold data.

## Read a real datastore

Give `openvmfs` the device that holds the datastore: the whole LUN (it finds the VMFS partition itself) or the
partition.

```bash
openvmfs info /dev/mapper/3600a0980...        # a multipath device
openvmfs info /dev/sdc                        # a plain SCSI disk
openvmfs info /dev/sdc,/dev/sdd               # a datastore that was extended onto a second LUN
```

Three things matter when the LUN is real:

1. **Read a snapshot, not the live LUN.** A datastore in use changes under the reader. Take a snapshot on the
   storage array, present *that* to the machine that runs OpenVmfs, and read it. Never present a copy of a datastore
   to an ESXi host that has the original mounted, unless you mean to resignature it.
2. **Take a VM snapshot first if you want the disk of a running VM.** Otherwise the disk on the storage snapshot
   lacks its newest blocks, and OpenVmfs refuses it. [backup.md](backup.md) explains the order;
   [../consistency.md](../consistency.md) shows the measurements.
3. **Keep it read-only at every level you can.** OpenVmfs opens the device read-only and sends nothing but reads.
   Belt and braces: present the snapshot read-only on the array if it can, and `blockdev --setro /dev/sdX` on
   Linux. Do not let the operating system mount or "repair" anything on the device.

For a backup, add `OPENVMFS_DIRECT=1` (or the matching option in the libraries): the data is then read past the
page cache, which is faster for one sequential pass and cannot serve stale blocks of an earlier snapshot that had
the same device name.

On Windows, image files work and are tested. Raw disks (`\\.\PhysicalDrive3`, from an elevated prompt) are opened
by the command line tool, which asks the driver for their size; reading a datastore that way has not been tested
yet. The library takes the size from the caller (`FileSource::with_len`).

## What it will not read

OpenVmfs refuses whatever it has not verified against a volume written by ESXi, and says why. The list is in the
[README](../../README.md#what-is-refused); what to do about each case is in [errors.md](errors.md). In short: VMFS5,
VMFS-L, raw device mappings, encrypted disks, and disks that were open for writing when the image was taken.

## Next

- [cli.md](cli.md): every command.
- [rust.md](rust.md), [c.md](c.md), [dotnet.md](dotnet.md): the libraries.
- [backup.md](backup.md): reading virtual disks for a backup.
- [../internals/walkthrough.md](../internals/walkthrough.md): how the bytes of that 1 GiB disk are found, step by
  step.
