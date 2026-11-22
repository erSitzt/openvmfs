# Changes

Each release lists what a program that embeds OpenVmfs has to know: changes to the API or the C interface, changes
in what is read and what is refused, and corrected read errors (data that an earlier version returned wrongly).

## 0.4.1

- **API and C interface:** unchanged (interface version 5).
- **Read or refused:** unchanged.
- **Corrected read errors:** none.
- Verified on a 40 TiB datastore made by ESXi 8.0 U3 (`fixture-big-esxi8-v1`): disks behind the first and the
  second 16 TiB boundary read as on ESXi. No change to the library was needed.
- Opening a disk whose snapshot delta was written in random order needs far fewer reads: the grain tables are
  fetched in the order of the file, up to 4 MiB at a time, whatever order the directory names them in. A 64 GiB
  disk with a million random 4 KiB writes took 4134 reads before and takes 73 now. The data read is the same,
  and the disk has the SHA-256 of ESXi's own consolidation.

## 0.4.0

- **API and C interface:** every `Unsupported` error now says why, for counting and reporting:
  `Error::unsupported_reason()` (Rust, new enum `UnsupportedReason`), `ovmfs_error_reason()` with `OVMFS_REASON_*`
  (C), `VmfsException.UnsupportedReason` (.NET). The reasons are: not verified, raw device mapping, encrypted or
  I/O filter, other VMFS version, VMFS-L, COWD delta, chain leaves the datastore. C interface version 5: the
  .NET binding and the native library must both be 0.4.0. Existing calls are unchanged.
- **Read or refused:** unchanged.
- **Corrected read errors:** none.
- `openvmfs` no longer reports an error when its output is closed early (`| head`).

## 0.3.1

- **API and C interface:** unchanged (interface version 4).
- **Read or refused:** unchanged.
- **Corrected read errors:** none.
- Opening a snapshot delta fetches grain tables that lie next to each other in one read of up to 4 MiB instead of
  one read per table. On a device behind a network this shortens opening disks with large deltas.
- `openvmfs` on Windows reads raw disks (`\\.\PhysicalDriveN`): it asks the driver for the size and reads
  unbuffered in whole sectors. The disk may be offline.
- Each release now carries the command line tool for Linux x64 (static) and Windows x64.
- `docs/guide/backup.md` has measured costs of opening volumes and disks.

## 0.3.0

- **API and C interface:** new `Disk::map` / `ovmfs_disk_map` / `VmdkDisk.Map`: where each part of a virtual disk
  is stored on the device, through all snapshot deltas. C interface version 4.
- **Read or refused:** snapshot deltas written by ESXi 8.0 (a header field that ESXi 7.0 leaves at 0 is 0x200
  there) are now read; 0.2.0 refused every disk with a snapshot on ESXi 8.0. Files whose sub-blocks lie in
  clusters above 255 are now read.
- **Corrected read errors:** none. Both changes turn refusals into reads.
- The NuGet package no longer contains paths of the build machine and carries licence notices.
