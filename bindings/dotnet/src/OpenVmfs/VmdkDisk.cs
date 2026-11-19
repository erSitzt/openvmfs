namespace OpenVmfs;

/// <summary>A range of a disk that holds data.</summary>
/// <param name="Offset">First byte of the range.</param>
/// <param name="Length">Length of the range in bytes.</param>
public readonly record struct DataExtent(long Offset, long Length)
{
    /// <summary>First byte after the range.</summary>
    public long End => Offset + Length;
}

/// <summary>A range of a virtual disk and where its bytes are stored. See <see cref="VmdkDisk.Map"/>.</summary>
/// <param name="Offset">Offset on the virtual disk.</param>
/// <param name="Length">Length of the range in bytes.</param>
/// <param name="IsZero">True if the range reads as zeros: unallocated, never written, or discarded.</param>
/// <param name="Device">
/// Position of the device in the list the volume was opened with; 0 for a datastore on one device and for zero
/// ranges.
/// </param>
/// <param name="DeviceOffset">Offset of the bytes from the start of that device; 0 for zero ranges.</param>
public readonly record struct MappedRange(long Offset, long Length, bool IsZero, int Device, long DeviceOffset)
{
    /// <summary>First offset after the range.</summary>
    public long End => Offset + Length;
}

/// <summary>
/// A virtual disk: the bytes a guest sees. Reads can run in parallel; the disk stays usable after its volume has
/// been disposed.
/// </summary>
public sealed class VmdkDisk : IDisposable, IAsyncDisposable
{
    private readonly DiskHandle _handle;
    private readonly DeviceBridge? _bridge;

    internal unsafe VmdkDisk(DiskHandle handle, DeviceBridge? bridge)
    {
        _handle = handle;
        _bridge = bridge;
        try
        {
            Capacity = checked((long)Native.ovmfs_disk_capacity(handle));
            var count = Native.ovmfs_disk_data_range_count(handle);
            var ranges = new Native.Range[checked((int)count)];
            fixed (Native.Range* first = ranges)
            {
                var copied = Native.ovmfs_disk_data_ranges(handle, first, count);
                Array.Resize(ref ranges, checked((int)copied));
            }

            DataExtents = Array.ConvertAll(ranges, range => new DataExtent(checked((long)range.Offset), checked((long)range.Length)));
        }
        catch
        {
            handle.Dispose();
            throw;
        }
    }

    /// <summary>Size of the disk in bytes.</summary>
    public long Capacity { get; }

    /// <summary>
    /// The ranges of the disk that hold written data, sorted by offset. Everything outside them reads as zeros, so
    /// a backup can skip it.
    /// </summary>
    public IReadOnlyList<DataExtent> DataExtents { get; }

    /// <summary>
    /// Fills <paramref name="destination"/> completely with the bytes at <paramref name="offset"/>. Unallocated and
    /// never-written ranges read as zeros.
    /// </summary>
    /// <exception cref="VmfsException">The read failed or goes beyond the end of the disk.</exception>
    public unsafe void Read(long offset, Span<byte> destination)
    {
        ArgumentOutOfRangeException.ThrowIfNegative(offset);
        fixed (byte* buffer = destination)
        {
            // An empty span pins as null; the native side accepts that for a length of 0.
            var status = Native.ovmfs_disk_read(_handle, (ulong)offset, buffer, (nuint)destination.Length, out var error);
            Native.ThrowIfFailed(status, error, _bridge);
        }
    }

    /// <summary>
    /// Says where the bytes of the disk are stored: for every part of the requested range, an offset on a device or
    /// "zeros". With it a caller can read the data from the devices itself (asynchronously, batched, over a
    /// network) and leave only the metadata to this library. Reading a range from its device gives exactly what
    /// <see cref="Read"/> returns for it.
    /// </summary>
    /// <param name="offset">Where on the disk to start.</param>
    /// <param name="length">How many bytes to describe at most.</param>
    /// <param name="maxRanges">Upper limit for the number of ranges returned.</param>
    /// <returns>
    /// Contiguous ranges starting at <paramref name="offset"/>, neighbours merged. If they end before
    /// <paramref name="offset"/> + <paramref name="length"/>, call again from <see cref="MappedRange.End"/> of the
    /// last one. A disk with snapshots can need one range per 4 KiB where a delta is fragmented.
    /// </returns>
    /// <exception cref="VmfsException">
    /// The range is not inside the disk, or the volume was opened with <see cref="VmfsOpenOptions.ReplayJournal"/>.
    /// </exception>
    public unsafe IReadOnlyList<MappedRange> Map(long offset, long length, int maxRanges = 4096)
    {
        ArgumentOutOfRangeException.ThrowIfNegative(offset);
        ArgumentOutOfRangeException.ThrowIfNegative(length);
        ArgumentOutOfRangeException.ThrowIfNegativeOrZero(maxRanges);
        var ranges = new Native.MappedRange[maxRanges];
        nuint count;
        fixed (Native.MappedRange* first = ranges)
        {
            var status = Native.ovmfs_disk_map(_handle, (ulong)offset, (ulong)length, first, (nuint)maxRanges, out count, out var error);
            Native.ThrowIfFailed(status, error, _bridge);
        }

        var result = new MappedRange[checked((int)count)];
        for (var i = 0; i < result.Length; i++)
        {
            var range = ranges[i];
            result[i] = new MappedRange(
                checked((long)range.Offset),
                checked((long)range.Length),
                range.Backing == 0,
                checked((int)range.Device),
                checked((long)range.DeviceOffset));
        }

        return result;
    }

    /// <summary>
    /// Like <see cref="Read"/>, on a thread-pool thread.
    /// </summary>
    /// <remarks>
    /// Cancellation is honoured only before the read starts. Once the native read is running, the returned task
    /// completes only after it has finished, so the caller may reuse or return the buffer as soon as the task is
    /// done, whatever its outcome.
    /// </remarks>
    public ValueTask ReadAsync(long offset, Memory<byte> destination, CancellationToken cancellationToken = default)
    {
        ArgumentOutOfRangeException.ThrowIfNegative(offset);
        if (cancellationToken.IsCancellationRequested)
        {
            return ValueTask.FromCanceled(cancellationToken);
        }

        return new ValueTask(Task.Run(() => Read(offset, destination.Span), cancellationToken));
    }

    /// <summary>Closes the disk. A read that is still running finishes first.</summary>
    public void Dispose() => _handle.Dispose();

    /// <inheritdoc />
    public ValueTask DisposeAsync()
    {
        Dispose();
        return ValueTask.CompletedTask;
    }
}
