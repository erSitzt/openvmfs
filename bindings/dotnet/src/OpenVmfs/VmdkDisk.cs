namespace OpenVmfs;

/// <summary>A range of a disk that holds data.</summary>
/// <param name="Offset">First byte of the range.</param>
/// <param name="Length">Length of the range in bytes.</param>
public readonly record struct DataExtent(long Offset, long Length)
{
    /// <summary>First byte after the range.</summary>
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
