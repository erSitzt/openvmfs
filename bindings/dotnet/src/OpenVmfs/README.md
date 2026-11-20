# OpenVmfs

Reads VMware VMFS6 volumes and the virtual disks (VMDK) stored on them, without ESXi: from a storage-array snapshot
presented to a Linux or Windows machine, from a disk image, or from any other block device. Read-only.

- VMFS 6 as formatted by ESXi 7.0 and 8.0, on one or several devices.
- Virtual disks: thin, lazy-zeroed and eager-zeroed base disks, and snapshot chains (SESparse deltas).
- Refuses instead of guessing: a structure that has not been verified against ESXi, or a disk that a host had open
  for writing, is an error with a kind you can act on.

The package carries the native library for Linux x64 and Windows x64. It targets .NET 10.

```csharp
using OpenVmfs;

using var volume = VmfsVolume.Open("/dev/mapper/snapshot-lun", new VmfsOpenOptions { DirectIo = true });
using var disk = volume.OpenDisk("vm1/vm1.vmdk");        // the descriptor, not the -flat file

var buffer = new byte[4 << 20];
foreach (var extent in disk.DataExtents)                  // everything outside the extents reads as zeros
{
    for (var offset = extent.Offset; offset < extent.End; offset += buffer.Length)
    {
        var length = (int)Math.Min(buffer.Length, extent.End - offset);
        disk.Read(offset, buffer.AsSpan(0, length));
    }
}
```

`disk.Map(offset, length)` tells where each part of a disk is stored on the device, for callers that read the
device themselves.

A virtual disk of a running VM can only be read reliably below a VM snapshot; the project's documentation explains
the order of snapshots a backup needs.

OpenVmfs is an independent project under the Apache-2.0 licence. It is not affiliated with VMware or Broadcom.
