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
}

/// <summary>
/// A VMFS6 volume on a block device or in an image file, opened read-only. Safe to use from many threads at once.
/// </summary>
public sealed class VmfsVolume : IDisposable
{
    private readonly VolumeHandle _handle;

    private VmfsVolume(VolumeHandle handle, Native.VolumeInfo info, string label, string uuid)
    {
        _handle = handle;
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
    public static unsafe VmfsVolume Open(string devicePath, VmfsOpenOptions? options = null)
    {
        ArgumentException.ThrowIfNullOrEmpty(devicePath);
        options ??= new VmfsOpenOptions();
        ArgumentOutOfRangeException.ThrowIfNegative(options.Partition);

        var abi = Native.ovmfs_abi_version();
        if (abi != Native.AbiVersion)
        {
            throw new VmfsException(
                VmfsErrorKind.Internal,
                $"The native OpenVmfs library has interface version {abi}, this binding needs {Native.AbiVersion}.");
        }

        var flags = options.AcceptUnverified ? Native.OpenAcceptUnverified : 0;
        var status = Native.ovmfs_volume_open(devicePath, (uint)options.Partition, flags, out var raw, out var error);
        Native.ThrowIfFailed(status, error);
        var handle = new VolumeHandle(raw);
        try
        {
            status = Native.ovmfs_volume_get_info(handle, out var info, out error);
            Native.ThrowIfFailed(status, error);
            var label = ReadText(new ReadOnlySpan<byte>(info.Label, 129));
            return new VmfsVolume(handle, info, label, FormatUuid(new ReadOnlySpan<byte>(info.Uuid, 16)));
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
    /// something that is refused (a snapshot chain, a raw device mapping, encryption, an unverified layout).
    /// </exception>
    public VmdkDisk OpenDisk(string descriptorPath)
    {
        ArgumentException.ThrowIfNullOrEmpty(descriptorPath);
        var status = Native.ovmfs_disk_open(_handle, descriptorPath, out var raw, out var error);
        Native.ThrowIfFailed(status, error);
        return new VmdkDisk(new DiskHandle(raw));
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
