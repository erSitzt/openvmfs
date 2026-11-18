using System.Runtime.InteropServices;
using System.Text;

namespace OpenVmfs;

/// <summary>How to open a volume.</summary>
public sealed record VmfsOpenOptions
{
    /// <summary>Partition number counted from 1, or 0 to detect the VMFS partition.</summary>
    public int Partition { get; init; }

    /// <summary>
    /// Reads structures that have not been verified against ESXi instead of refusing them. Meant for verification
    /// work: data read this way may be wrong.
    /// </summary>
    public bool AcceptUnverified { get; init; }

    /// <summary>
    /// Reads the devices without the page cache of the operating system (<c>O_DIRECT</c> on Linux,
    /// <c>FILE_FLAG_NO_BUFFERING</c> on Windows). Recommended when reading a LUN for a backup: the data is read once,
    /// and nothing stale from an earlier state of the LUN can be served from the cache. Callers need not align
    /// their buffers.
    /// </summary>
    public bool DirectIo { get; init; }

    /// <summary>
    /// Opens disks although an ESXi host had them open for writing when the image was taken, instead of failing
    /// with <see cref="VmfsErrorKind.InUse"/>. Such a disk can lack its newest writes while later ones are present.
    /// Only for looking at data that is known to be damaged anyway; never for a backup.
    /// </summary>
    public bool AcceptInUse { get; init; }

    /// <summary>
    /// Experimental: reads the volume as if the transactions waiting in its journals had reached their final
    /// place, as an ESXi host does after a crash. Disks that were open for writing can then be opened. Anything in
    /// a journal that is not understood makes the open fail with <see cref="VmfsErrorKind.Unsupported"/>.
    /// </summary>
    public bool ReplayJournal { get; init; }
}

/// <summary>
/// A VMFS6 volume on a block device or in an image file, opened read-only. Safe to use from many threads at once.
/// </summary>
public sealed class VmfsVolume : IDisposable
{
    private readonly VolumeHandle _handle;
    private readonly DeviceBridge? _bridge;

    private VmfsVolume(VolumeHandle handle, Native.VolumeInfo info, string label, string uuid, DeviceBridge? bridge)
    {
        _handle = handle;
        _bridge = bridge;
        Label = label;
        Uuid = uuid;
        Version = $"6.{info.MinorVersion}";
        Capacity = checked((long)info.Capacity);
        FileBlockSize = checked((long)info.FileBlockSize);
        PartitionOffset = checked((long)info.PartitionOffset);
        ActiveHeartbeats = checked((int)info.ActiveHeartbeats);
    }

    /// <summary>Datastore label.</summary>
    public string Label { get; }

    /// <summary>Datastore UUID as ESXi prints it, as in <c>/vmfs/volumes/&lt;uuid&gt;</c>.</summary>
    public string Uuid { get; }

    /// <summary>Filesystem version, for example <c>6.82</c>.</summary>
    public string Version { get; }

    /// <summary>Size of the logical volume in bytes.</summary>
    public long Capacity { get; }

    /// <summary>Size of a file block in bytes.</summary>
    public long FileBlockSize { get; }

    /// <summary>Offset of the VMFS partition on the opened device.</summary>
    public long PartitionOffset { get; }

    /// <summary>
    /// Heartbeat slots in use: hosts that were holding locks (running VMs, open files) when the image was taken,
    /// so metadata may have been in the middle of an update. 0 does not mean the datastore was unmounted: a mounted
    /// but idle datastore shows 0 as well.
    /// </summary>
    public int ActiveHeartbeats { get; }

    /// <summary>
    /// Opens the VMFS6 volume on <paramref name="devicePath"/>: a whole disk with a partition table (for example a
    /// LUN) or the VMFS partition itself.
    /// </summary>
    /// <exception cref="VmfsException">The volume could not be opened.</exception>
    public static VmfsVolume Open(string devicePath, VmfsOpenOptions? options = null)
    {
        ArgumentException.ThrowIfNullOrEmpty(devicePath);
        return Open([devicePath], options);
    }

    /// <summary>
    /// Opens a VMFS6 volume that is stored on several devices: a datastore that was extended onto further LUNs.
    /// All devices of the datastore are needed, in any order.
    /// </summary>
    /// <exception cref="VmfsException">
    /// The volume could not be opened. When a device is missing, the kind is
    /// <see cref="VmfsErrorKind.InvalidInput"/> and the message names the devices as ESXi recorded them.
    /// </exception>
    public static unsafe VmfsVolume Open(IReadOnlyList<string> devicePaths, VmfsOpenOptions? options = null)
    {
        ArgumentNullException.ThrowIfNull(devicePaths);
        ArgumentOutOfRangeException.ThrowIfZero(devicePaths.Count);
        foreach (var devicePath in devicePaths)
        {
            ArgumentException.ThrowIfNullOrEmpty(devicePath);
        }

        options ??= new VmfsOpenOptions();
        ArgumentOutOfRangeException.ThrowIfNegative(options.Partition);

        CheckInterfaceVersion();
        var flags = Flags(options);
        var paths = new nint[devicePaths.Count];
        int status;
        nint raw;
        nint error;
        try
        {
            for (var i = 0; i < paths.Length; i++)
            {
                paths[i] = Marshal.StringToCoTaskMemUTF8(devicePaths[i]);
            }

            fixed (nint* first = paths)
            {
                status = Native.ovmfs_volume_open_devices(
                    first, (nuint)paths.Length, (uint)options.Partition, flags, out raw, out error);
            }
        }
        finally
        {
            foreach (var path in paths)
            {
                Marshal.FreeCoTaskMem(path);
            }
        }

        Native.ThrowIfFailed(status, error);
        return Wrap(raw, null);
    }

    /// <summary>
    /// Opens the VMFS6 volume on a device that is read through <paramref name="device"/>: for devices that are not
    /// a path, such as a LUN reached through a tunnel, or an image in memory.
    /// </summary>
    /// <remarks>
    /// The device is used until the volume and every disk opened from it have been disposed.
    /// <see cref="VmfsOpenOptions.DirectIo"/> has no effect here. When the device throws, the
    /// <see cref="VmfsException"/> that results carries that exception as its inner exception.
    /// </remarks>
    /// <exception cref="VmfsException">The volume could not be opened.</exception>
    public static unsafe VmfsVolume Open(IVmfsDevice device, VmfsOpenOptions? options = null)
    {
        ArgumentNullException.ThrowIfNull(device);
        options ??= new VmfsOpenOptions();
        ArgumentOutOfRangeException.ThrowIfNegative(options.Partition);
        CheckInterfaceVersion();
        var length = device.Length;
        ArgumentOutOfRangeException.ThrowIfNegativeOrZero(length, nameof(device));

        var (bridge, context) = DeviceBridge.Create(device);
        // From here on the native side owns the context and releases it, also when the open fails.
        var status = Native.ovmfs_volume_open_device(
            DeviceBridge.ReadFunction, DeviceBridge.ReleaseFunction, context, (ulong)length, (uint)options.Partition,
            Flags(options), out var raw, out var error);
        Native.ThrowIfFailed(status, error, bridge);
        return Wrap(raw, bridge);
    }

    private static uint Flags(VmfsOpenOptions options) =>
        (options.AcceptUnverified ? Native.OpenAcceptUnverified : 0)
        | (options.DirectIo ? Native.OpenDirect : 0)
        | (options.AcceptInUse ? Native.OpenAcceptInUse : 0)
        | (options.ReplayJournal ? Native.OpenReplayJournal : 0);

    private static void CheckInterfaceVersion()
    {
        var abi = Native.ovmfs_abi_version();
        if (abi != Native.AbiVersion)
        {
            throw new VmfsException(
                VmfsErrorKind.Internal,
                $"The native OpenVmfs library has interface version {abi}, this binding needs {Native.AbiVersion}.");
        }
    }

    private static unsafe VmfsVolume Wrap(nint raw, DeviceBridge? bridge)
    {
        var handle = new VolumeHandle(raw);
        try
        {
            var status = Native.ovmfs_volume_get_info(handle, out var info, out var error);
            Native.ThrowIfFailed(status, error, bridge);
            var label = ReadText(new ReadOnlySpan<byte>(info.Label, 129));
            return new VmfsVolume(handle, info, label, FormatUuid(new ReadOnlySpan<byte>(info.Uuid, 16)), bridge);
        }
        catch
        {
            handle.Dispose();
            throw;
        }
    }

    /// <summary>
    /// Opens the virtual disk described by the descriptor file (the small <c>.vmdk</c> text file) at
    /// <paramref name="descriptorPath"/>, for example <c>vm1/vm1.vmdk</c>.
    /// </summary>
    /// <exception cref="VmfsException">
    /// The disk could not be opened. <see cref="VmfsErrorKind.Unsupported"/> means the disk may be fine but uses
    /// something that is refused (a raw device mapping, encryption, an unverified layout);
    /// <see cref="VmfsErrorKind.InUse"/> means a host was writing to it when the image was taken.
    /// </exception>
    public VmdkDisk OpenDisk(string descriptorPath)
    {
        ArgumentException.ThrowIfNullOrEmpty(descriptorPath);
        var status = Native.ovmfs_disk_open(_handle, descriptorPath, out var raw, out var error);
        Native.ThrowIfFailed(status, error, _bridge);
        return new VmdkDisk(new DiskHandle(raw), _bridge);
    }

    /// <summary>Closes the volume. Disks opened from it stay usable.</summary>
    public void Dispose() => _handle.Dispose();

    private static string ReadText(ReadOnlySpan<byte> bytes)
    {
        var end = bytes.IndexOf((byte)0);
        return Encoding.UTF8.GetString(end < 0 ? bytes : bytes[..end]);
    }

    /// <summary>Formats a UUID the way ESXi does: time (low, high), a random part, the creating host's MAC.</summary>
    private static string FormatUuid(ReadOnlySpan<byte> b)
    {
        var timeLow = BitConverter.ToUInt32(b[..4]);
        var timeHigh = BitConverter.ToUInt32(b[4..8]);
        var random = BitConverter.ToUInt16(b[8..10]);
        return $"{timeLow:x8}-{timeHigh:x8}-{random:x4}-{Convert.ToHexStringLower(b[10..16])}";
    }
}
