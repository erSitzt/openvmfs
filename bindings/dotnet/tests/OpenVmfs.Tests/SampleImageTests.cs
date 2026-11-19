using System.Security.Cryptography;

namespace OpenVmfs.Tests;

/// <summary>
/// Reads a public VMFS 6.82 sample image through the binding. The expectations are the same facts the Rust tests
/// check (crates/openvmfs-testkit/tests/sample_images.rs). The tests skip when the image has not been unpacked:
/// run <c>fixtures/fetch-external.sh</c> and <c>cargo test -p openvmfs-testkit</c> first.
/// </summary>
public sealed class SampleImageTests
{
    private const long MiB = 1L << 20;
    private const long GiB = 1L << 30;

    private static string? FindImage()
    {
        for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir is not null; dir = dir.Parent)
        {
            var candidate = Path.Combine(dir.FullName, "target", "fixture-cache", "dissect", "vmfs6.bin");
            if (File.Exists(candidate))
            {
                return candidate;
            }
        }

        return null;
    }

    private static VmfsVolume OpenSample()
    {
        var image = FindImage();
        Assert.SkipWhen(image is null, "The sample image target/fixture-cache/dissect/vmfs6.bin is missing.");
        return VmfsVolume.Open(image!);
    }

    private static string? FindCacheFile(params string[] parts)
    {
        for (var dir = new DirectoryInfo(AppContext.BaseDirectory); dir is not null; dir = dir.Parent)
        {
            var candidate = Path.Combine([dir.FullName, "target", "fixture-cache", .. parts]);
            if (File.Exists(candidate))
            {
                return candidate;
            }
        }

        return null;
    }

    [Fact]
    public void DatastoreOnTwoDevices()
    {
        // Unpacked by `cargo test -p openvmfs-testkit --test fixture_span`.
        var first = FindCacheFile("ours", "fixture-span-v1-dev1.fixture.img");
        var second = FindCacheFile("ours", "fixture-span-v1-dev2.fixture.img");
        Assert.SkipWhen(first is null || second is null, "The spanned fixture has not been unpacked.");

        // In any order.
        using var volume = VmfsVolume.Open([second!, first!]);
        Assert.Equal("sp", volume.Label);
        Assert.Equal(4608 * MiB, volume.Capacity);
        using var disk = volume.OpenDisk("d/lazy.vmdk");
        Assert.Equal(2600 * MiB, disk.Capacity);
        using var sha = IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
        var buffer = new byte[4 * MiB];
        for (long offset = 0; offset < disk.Capacity; offset += buffer.Length)
        {
            var length = (int)Math.Min(buffer.Length, disk.Capacity - offset);
            disk.Read(offset, buffer.AsSpan(0, length));
            sha.AppendData(buffer, 0, length);
        }

        // What ESXi computed for d/lazy-flat.vmdk (fixtures/vmfs6/fixture-span-v1/truth/sha256.txt).
        Assert.Equal(
            "9dc96335f5b1e08e3725f7613d607b5377eb9c4d36127ef69d77adeb9d066f22",
            Convert.ToHexStringLower(sha.GetHashAndReset()));

        // One device alone is refused, and the message names both.
        var error = Assert.Throws<VmfsException>(() => VmfsVolume.Open(first!));
        Assert.Equal(VmfsErrorKind.InvalidInput, error.Kind);
        Assert.Contains("mpx.vmhba0:C0:T9:L0:1", error.Message);
    }

    [Fact]
    public void UnbufferedReadsReturnTheSameBytes()
    {
        var image = FindImage();
        Assert.SkipWhen(image is null, "The sample image target/fixture-cache/dissect/vmfs6.bin is missing.");
        using var buffered = VmfsVolume.Open(image!);
        VmfsVolume direct;
        try
        {
            direct = VmfsVolume.Open(image!, new VmfsOpenOptions { DirectIo = true });
        }
        catch (VmfsException error) when (error.Kind == VmfsErrorKind.Io)
        {
            // Some filesystems (tmpfs, some network mounts) refuse unbuffered opens.
            Assert.Skip("The filesystem holding the sample image does not support unbuffered reading.");
            return;
        }

        using (direct)
        {
            using var a = buffered.OpenDisk("normal.vmdk");
            using var b = direct.OpenDisk("normal.vmdk");
            // Odd offsets and lengths: the library aligns on its own.
            foreach (var (offset, length) in new[] { (42 * GiB, 4096), (42 * GiB + 1, 513), (42 * GiB + 4095, 70_001) })
            {
                var expected = new byte[length];
                var actual = new byte[length];
                a.Read(offset, expected);
                b.Read(offset, actual);
                Assert.Equal(expected, actual);
            }
        }
    }

    /// <summary>A file read through managed code, with a switch that makes it fail.</summary>
    private sealed class FileDevice(string path) : IVmfsDevice, IDisposable
    {
        private readonly Microsoft.Win32.SafeHandles.SafeFileHandle _file = File.OpenHandle(path);

        public int Reads;
        public bool Fail;

        public long Length => RandomAccess.GetLength(_file);

        public void Read(long offset, Span<byte> destination)
        {
            Interlocked.Increment(ref Reads);
            if (Fail)
            {
                throw new IOException("the tunnel is gone");
            }

            while (!destination.IsEmpty)
            {
                var read = RandomAccess.Read(_file, destination, offset);
                if (read == 0)
                {
                    throw new EndOfStreamException();
                }

                destination = destination[read..];
                offset += read;
            }
        }

        public void Dispose() => _file.Dispose();
    }

    [Fact]
    public async Task AVolumeCanBeReadThroughTheCallersOwnDevice()
    {
        var image = FindImage();
        Assert.SkipWhen(image is null, "The sample image target/fixture-cache/dissect/vmfs6.bin is missing.");
        using var device = new FileDevice(image!);

        VmdkDisk disk;
        using (var volume = VmfsVolume.Open(device))
        {
            Assert.Equal("vmfs6", volume.Label);
            disk = volume.OpenDisk("normal.vmdk");
        }

        // The disk outlives the volume and keeps using the device, from several threads at once.
        using (disk)
        {
            var before = device.Reads;
            var hashes = await Task.WhenAll(Enumerable.Range(0, 8).Select(_ => Task.Run(() =>
            {
                var buffer = new byte[MiB];
                disk.Read(42 * GiB, buffer);
                return Convert.ToHexStringLower(SHA256.HashData(buffer));
            })));
            Assert.All(hashes, hash => Assert.Equal("ce6475b1fc22ad8543d2aee58ef77aa19eb5d208c372f4ee69885683e0213a08", hash));
            Assert.True(device.Reads > before);

            // What the device throws arrives as the inner exception of an I/O error.
            device.Fail = true;
            var error = Assert.Throws<VmfsException>(() => disk.Read(42 * GiB, new byte[512]));
            Assert.Equal(VmfsErrorKind.Io, error.Kind);
            Assert.IsType<IOException>(error.InnerException);
            Assert.Equal("the tunnel is gone", error.InnerException!.Message);
        }

        // A device that fails during the open: the same.
        var failed = Assert.Throws<VmfsException>(() => VmfsVolume.Open(device));
        Assert.Equal(VmfsErrorKind.Io, failed.Kind);
        Assert.IsType<IOException>(failed.InnerException);
    }

    [Fact]
    public void MapNamesDeviceAndOffsetOfEveryRange()
    {
        // The same datastore on two devices as above; the caller reads the data itself.
        var first = FindCacheFile("ours", "fixture-span-v1-dev1.fixture.img");
        var second = FindCacheFile("ours", "fixture-span-v1-dev2.fixture.img");
        Assert.SkipWhen(first is null || second is null, "The spanned fixture has not been unpacked.");
        string[] paths = [first!, second!];
        using var volume = VmfsVolume.Open(paths);
        using var disk = volume.OpenDisk("d/lazy.vmdk");
        using var device0 = File.OpenHandle(paths[0]);
        using var device1 = File.OpenHandle(paths[1]);
        Microsoft.Win32.SafeHandles.SafeFileHandle[] devices = [device0, device1];

        var ours = new byte[MiB];
        var theirs = new byte[MiB];
        var devicesUsed = new HashSet<int>();
        long dataBytes = 0;
        long position = 0;
        while (position < disk.Capacity)
        {
            // A small page size, to go through the paging.
            var page = disk.Map(position, disk.Capacity - position, maxRanges: 2);
            Assert.InRange(page.Count, 1, 2);
            foreach (var range in page)
            {
                Assert.Equal(position, range.Offset);
                position = range.End;
                if (range.IsZero)
                {
                    disk.Read(range.Offset, ours.AsSpan(0, 512));
                    Assert.All(ours.AsSpan(0, 512).ToArray(), b => Assert.Equal(0, b));
                    continue;
                }

                devicesUsed.Add(range.Device);
                dataBytes += range.Length;
                for (long done = 0; done < range.Length; done += MiB)
                {
                    var take = (int)Math.Min(MiB, range.Length - done);
                    disk.Read(range.Offset + done, ours.AsSpan(0, take));
                    RandomAccess.Read(devices[range.Device], theirs.AsSpan(0, take), range.DeviceOffset + done);
                    Assert.True(ours.AsSpan(0, take).SequenceEqual(theirs.AsSpan(0, take)), $"disk offset {range.Offset + done}");
                }
            }
        }

        Assert.Equal(disk.Capacity, position);
        Assert.Equal(disk.DataExtents.Sum(extent => extent.Length), dataBytes);
        Assert.Equal([0, 1], devicesUsed.Order());

        var outside = Assert.Throws<VmfsException>(() => disk.Map(disk.Capacity, 1));
        Assert.Equal(VmfsErrorKind.InvalidInput, outside.Kind);
    }

    [Fact]
    public void VolumeFacts()
    {
        using var volume = OpenSample();
        Assert.Equal("vmfs6", volume.Label);
        Assert.Equal("6.82", volume.Version);
        Assert.Equal("68626f2d-90e23158-8a2e-bc2411b17e28", volume.Uuid);
        Assert.Equal(2 * GiB, volume.Capacity);
        Assert.Equal(MiB, volume.FileBlockSize);
        Assert.Equal(1, volume.ActiveHeartbeats);
    }

    [Fact]
    public void ThinDiskReadsDataAndZeros()
    {
        using var volume = OpenSample();
        using var disk = volume.OpenDisk("normal.vmdk");
        Assert.Equal(69 * GiB, disk.Capacity);
        Assert.Equal([new DataExtent(42 * GiB, MiB)], disk.DataExtents);

        var buffer = new byte[MiB];
        disk.Read(42 * GiB, buffer);
        Assert.Equal(
            "ce6475b1fc22ad8543d2aee58ef77aa19eb5d208c372f4ee69885683e0213a08",
            Convert.ToHexStringLower(SHA256.HashData(buffer)));

        Array.Fill(buffer, (byte)0xAA);
        disk.Read(0, buffer);
        Assert.All(buffer, b => Assert.Equal(0, b));

        disk.Read(0, Span<byte>.Empty);
    }

    [Fact]
    public async Task DiskOutlivesItsVolumeAndReadsInParallel()
    {
        VmdkDisk disk;
        using (var volume = OpenSample())
        {
            disk = volume.OpenDisk("large.vmdk");
        }

        await using (disk)
        {
            Assert.Equal(42 * 1024 * GiB, disk.Capacity);
            var written = Assert.Single(disk.DataExtents);
            var token = TestContext.Current.CancellationToken;
            var reads = Enumerable.Range(0, 16).Select(async _ =>
            {
                var buffer = new byte[MiB];
                await disk.ReadAsync(written.Offset, buffer, token);
                return Convert.ToHexStringLower(SHA256.HashData(buffer));
            });
            var hashes = await Task.WhenAll(reads);
            Assert.All(hashes, hash => Assert.Equal("ce6475b1fc22ad8543d2aee58ef77aa19eb5d208c372f4ee69885683e0213a08", hash));
        }
    }

    [Fact]
    public async Task CancelledReadDoesNotTouchTheBuffer()
    {
        using var volume = OpenSample();
        using var disk = volume.OpenDisk("normal.vmdk");
        var buffer = new byte[4096];
        Array.Fill(buffer, (byte)0x55);
        using var cancelled = new CancellationTokenSource();
        await cancelled.CancelAsync();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(async () => await disk.ReadAsync(42 * GiB, buffer, cancelled.Token));
        Assert.All(buffer, b => Assert.Equal(0x55, b));
    }

    [Fact]
    public void ErrorsCarryTheirKind()
    {
        using var volume = OpenSample();
        Assert.Equal(VmfsErrorKind.NotFound, Assert.Throws<VmfsException>(() => volume.OpenDisk("missing.vmdk")).Kind);
        // A raw device mapping: the data is not on this volume.
        var unsupported = Assert.Throws<VmfsException>(() => volume.OpenDisk("rdm.vmdk"));
        Assert.Equal(VmfsErrorKind.Unsupported, unsupported.Kind);
        Assert.Contains("raw device mapping", unsupported.Message);

        using var disk = volume.OpenDisk("normal.vmdk");
        var beyond = Assert.Throws<VmfsException>(() => disk.Read(disk.Capacity, new byte[1]));
        Assert.Equal(VmfsErrorKind.InvalidInput, beyond.Kind);

        Assert.Equal(VmfsErrorKind.Io, Assert.Throws<VmfsException>(() => VmfsVolume.Open("/nonexistent/device")).Kind);
    }
}
