# Changes

Each release lists what a program that embeds OpenVmfs has to know: changes to the API or the C interface, changes
in what is read and what is refused, and corrected read errors (data that an earlier version returned wrongly).

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
