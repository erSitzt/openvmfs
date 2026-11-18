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
