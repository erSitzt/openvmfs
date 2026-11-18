# Using the .NET binding

`OpenVmfs` (in [`bindings/dotnet`](../../bindings/dotnet)) is a thin layer over the C library for .NET 10. It adds
no logic of its own: handles become `IDisposable` objects, status codes become exceptions.

## Build

```bash
cargo build --release -p openvmfs-capi                 # the native library
dotnet build bindings/dotnet/OpenVmfs.slnx -c Release
dotnet test --solution bindings/dotnet/OpenVmfs.slnx   # needs the sample images, see docs/internals/testing.md
```

The project copies `libopenvmfs_c.so` (and `openvmfs_c.dll`, when it was built with `tools/build-windows.sh`) next
to the managed assembly.

For your own application, build the NuGet package. It carries the native library for Linux x64 and Windows x64
under `runtimes/<rid>/native/`, where the .NET runtime finds it on its own:

```bash
tools/pack-nuget.sh                      # target/nuget/OpenVmfs.<version>.nupkg
```

Reference it from a local package source until the package is published:

```xml
<!-- nuget.config -->
<packageSources>
  <add key="openvmfs-local" value="/path/to/openvmfs/target/nuget" />
</packageSources>
```

## Reading a disk

```csharp
using OpenVmfs;

using var volume = VmfsVolume.Open("/dev/mapper/snapshot-lun", new VmfsOpenOptions { DirectIo = true });
Console.WriteLine($"{volume.Label} ({volume.Uuid}), VMFS {volume.Version}, {volume.Capacity} bytes");

using var disk = volume.OpenDisk("vm1/vm1.vmdk");       // the descriptor, not the -flat file
var buffer = new byte[4 << 20];
foreach (var extent in disk.DataExtents)                 // everything outside the extents reads as zeros
{
    for (var offset = extent.Offset; offset < extent.End; offset += buffer.Length)
    {
        var length = (int)Math.Min(buffer.Length, extent.End - offset);
        disk.Read(offset, buffer.AsSpan(0, length));
        // ... use buffer[..length]
    }
}
```

A datastore on several devices:

```csharp
using var volume = VmfsVolume.Open(["/dev/sdc", "/dev/sdd"]);
```

## The types

| Type | Members |
|---|---|
| `VmfsVolume` | `Open(path, options)`, `Open(paths, options)`, `OpenDisk(descriptorPath)`, `Label`, `Uuid`, `Version`, `Capacity`, `FileBlockSize`, `PartitionOffset`, `ActiveHeartbeats` |
| `VmfsOpenOptions` | `Partition`, `DirectIo`, `ReplayJournal`, `AcceptInUse`, `AcceptUnverified` |
| `VmdkDisk` | `Capacity`, `DataExtents`, `Read(offset, Span<byte>)`, `ReadAsync(offset, Memory<byte>, cancellationToken)` |
| `DataExtent` | `Offset`, `Length`, `End` |
| `VmfsException` | `Kind` (`VmfsErrorKind`), `Message` |

## Behaviour worth knowing

- **Reads fill the buffer or throw.** There are no short reads. A zero-length read is allowed and does nothing.
- **Threads.** One `VmdkDisk` can be read from many threads at once. The handles are reference counted while a
  call is in progress, so disposing a disk during a read on another thread is safe: the read finishes first.
- **`ReadAsync`** checks the token and then runs the read on the thread pool. It completes only when the native
  call has returned, so a pooled buffer can be handed back as soon as the task is done. A read that is under way is
  not interrupted by cancellation.
- **A disk outlives its volume.** You can dispose the `VmfsVolume` and keep reading the disks you opened.
- **Errors.** Every failure is a `VmfsException` with a `Kind`:

| `VmfsErrorKind` | Meaning | What to do |
|---|---|---|
| `Io` | The device could not be read | |
| `Corrupt` | The data contradicts the format | Do not trust this object |
| `Unsupported` | Not verified, refused | Read the disk another way |
| `InUse` | A host was writing to the disk when the image was taken | Take a VM snapshot before the storage snapshot |
| `NotFound` | No such file, directory or volume | |
| `InvalidInput` | A wrong argument, or a device missing from a datastore on several devices | |
| `Internal` | A bug in the native library, or an interface version mismatch | |

```csharp
try
{
    using var disk = volume.OpenDisk(path);
    // read
}
catch (VmfsException e) when (e.Kind is VmfsErrorKind.Unsupported or VmfsErrorKind.InUse)
{
    // This disk cannot be read from the storage snapshot; use another transport for it.
}
```

The binding checks at the first open that the native library has the interface version it was written for and
throws `VmfsException` (`Internal`) otherwise.

Like the C interface, the binding covers volumes and virtual disks; directory listings and plain files are not in
it yet.
