using System.Runtime.InteropServices;

namespace OpenVmfs;

/// <summary>The C interface of the native library (<c>include/openvmfs.h</c>).</summary>
internal static unsafe partial class Native
{
    private const string Library = "openvmfs_c";

    /// <summary>The interface version this binding was written for.</summary>
    internal const uint AbiVersion = 5;

    internal const uint OpenAcceptUnverified = 1;
    internal const uint OpenDirect = 2;
    internal const uint OpenAcceptInUse = 4;
    internal const uint OpenReplayJournal = 8;

    [StructLayout(LayoutKind.Sequential)]
    internal struct Range
    {
        public ulong Offset;
        public ulong Length;
    }

    [StructLayout(LayoutKind.Sequential)]
    internal struct VolumeInfo
    {
        public ulong Capacity;
        public ulong FileBlockSize;
        public ulong PartitionOffset;
        public uint ActiveHeartbeats;
        public uint MinorVersion;
        public fixed byte Uuid[16];
        public fixed byte Label[129];
    }

    [LibraryImport(Library)]
    internal static partial uint ovmfs_abi_version();

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial int ovmfs_volume_open(string devicePath, uint partition, uint flags, out nint volume, out nint error);

    [LibraryImport(Library)]
    internal static partial int ovmfs_volume_open_devices(nint* devicePaths, nuint deviceCount, uint partition, uint flags, out nint volume, out nint error);

    [LibraryImport(Library)]
    internal static partial int ovmfs_volume_open_device(
        delegate* unmanaged[Cdecl]<nint, ulong, byte*, nuint, int> read,
        delegate* unmanaged[Cdecl]<nint, void> release,
        nint context,
        ulong size,
        uint partition,
        uint flags,
        out nint volume,
        out nint error);

    [LibraryImport(Library)]
    internal static partial void ovmfs_volume_close(nint volume);

    [LibraryImport(Library)]
    internal static partial int ovmfs_volume_get_info(VolumeHandle volume, out VolumeInfo info, out nint error);

    [LibraryImport(Library, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial int ovmfs_disk_open(VolumeHandle volume, string descriptorPath, out nint disk, out nint error);

    [LibraryImport(Library)]
    internal static partial void ovmfs_disk_close(nint disk);

    [LibraryImport(Library)]
    internal static partial ulong ovmfs_disk_capacity(DiskHandle disk);

    [LibraryImport(Library)]
    internal static partial int ovmfs_disk_read(DiskHandle disk, ulong offset, byte* buffer, nuint length, out nint error);

    [LibraryImport(Library)]
    internal static partial nuint ovmfs_disk_data_range_count(DiskHandle disk);

    [LibraryImport(Library)]
    internal static partial nuint ovmfs_disk_data_ranges(DiskHandle disk, Range* ranges, nuint capacity);

    [StructLayout(LayoutKind.Sequential)]
    internal struct MappedRange
    {
        public ulong Offset;
        public ulong Length;
        public ulong DeviceOffset;
        public uint Device;
        public uint Backing;
    }

    [LibraryImport(Library)]
    internal static partial int ovmfs_disk_map(DiskHandle disk, ulong offset, ulong length, MappedRange* ranges, nuint capacity, out nuint count, out nint error);

    [LibraryImport(Library)]
    internal static partial nint ovmfs_error_message(nint error);

    [LibraryImport(Library)]
    internal static partial int ovmfs_error_reason(nint error);

    [LibraryImport(Library)]
    internal static partial void ovmfs_error_free(nint error);

    /// <summary>Turns a failed status into an exception and frees the native error object.</summary>
    /// <param name="status">The status the native call returned.</param>
    /// <param name="error">The native error object, or 0.</param>
    /// <param name="bridge">The caller's device, if the volume is read through one: its exception is attached.</param>
    internal static void ThrowIfFailed(int status, nint error, DeviceBridge? bridge = null)
    {
        if (status == 0)
        {
            return;
        }

        var message = "OpenVmfs failed without a message.";
        var reason = VmfsUnsupportedReason.None;
        if (error != 0)
        {
            message = Marshal.PtrToStringUTF8(ovmfs_error_message(error)) ?? message;
            reason = (VmfsUnsupportedReason)ovmfs_error_reason(error);
            ovmfs_error_free(error);
        }

        throw new VmfsException((VmfsErrorKind)status, message, bridge?.TakeLastError()) { UnsupportedReason = reason };
    }
}

/// <summary>Owns a native volume. The runtime keeps it alive while a call that uses it is running.</summary>
internal sealed class VolumeHandle : SafeHandle
{
    public VolumeHandle(nint handle) : base(0, ownsHandle: true) => SetHandle(handle);

    public override bool IsInvalid => handle == 0;

    protected override bool ReleaseHandle()
    {
        Native.ovmfs_volume_close(handle);
        return true;
    }
}

/// <summary>Owns a native disk. The runtime keeps it alive while a call that uses it is running.</summary>
internal sealed class DiskHandle : SafeHandle
{
    public DiskHandle(nint handle) : base(0, ownsHandle: true) => SetHandle(handle);

    public override bool IsInvalid => handle == 0;

    protected override bool ReleaseHandle()
    {
        Native.ovmfs_disk_close(handle);
        return true;
    }
}
