using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;

namespace OpenVmfs;

/// <summary>
/// A device that OpenVmfs reads through the caller's code instead of a path: a LUN reached through a tunnel, an
/// image in memory, a stream.
/// </summary>
/// <remarks>
/// <see cref="Read"/> is called from whatever thread uses the volume or one of its disks, from several at once. It
/// must fill the whole destination or throw.
/// </remarks>
public interface IVmfsDevice
{
    /// <summary>Size of the device in bytes.</summary>
    long Length { get; }

    /// <summary>Fills <paramref name="destination"/> completely with the bytes at <paramref name="offset"/>.</summary>
    void Read(long offset, Span<byte> destination);
}

/// <summary>
/// Connects an <see cref="IVmfsDevice"/> to the native library. It stays alive, through a GC handle, until the
/// native side releases the device: when the volume and all disks opened from it are closed.
/// </summary>
internal sealed unsafe class DeviceBridge
{
    private readonly IVmfsDevice _device;
    private Exception? _lastError;

    private DeviceBridge(IVmfsDevice device) => _device = device;

    /// <summary>Creates a bridge and the context pointer that the native callbacks get.</summary>
    internal static (DeviceBridge Bridge, nint Context) Create(IVmfsDevice device)
    {
        var bridge = new DeviceBridge(device);
        return (bridge, GCHandle.ToIntPtr(GCHandle.Alloc(bridge)));
    }

    internal static delegate* unmanaged[Cdecl]<nint, ulong, byte*, nuint, int> ReadFunction => &OnRead;

    internal static delegate* unmanaged[Cdecl]<nint, void> ReleaseFunction => &OnRelease;

    /// <summary>The exception the device threw most recently, once.</summary>
    internal Exception? TakeLastError() => Interlocked.Exchange(ref _lastError, null);

    [UnmanagedCallersOnly(CallConvs = [typeof(CallConvCdecl)])]
    private static int OnRead(nint context, ulong offset, byte* buffer, nuint length)
    {
        DeviceBridge? bridge = null;
        try
        {
            bridge = (DeviceBridge)GCHandle.FromIntPtr(context).Target!;
            bridge._device.Read(checked((long)offset), new Span<byte>(buffer, checked((int)length)));
            return 0;
        }
        catch (Exception error)
        {
            // An exception must not cross into native code. The binding attaches it to the VmfsException it throws.
            if (bridge is not null)
            {
                Volatile.Write(ref bridge._lastError, error);
            }

            return 1;
        }
    }

    [UnmanagedCallersOnly(CallConvs = [typeof(CallConvCdecl)])]
    private static void OnRelease(nint context) => GCHandle.FromIntPtr(context).Free();
}
