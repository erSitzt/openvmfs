namespace OpenVmfs;

/// <summary>What went wrong, in terms a caller can act on.</summary>
public enum VmfsErrorKind
{
    /// <summary>The device could not be read.</summary>
    Io = 1,

    /// <summary>The on-disk data contradicts the format.</summary>
    Corrupt = 2,

    /// <summary>
    /// The data may be valid, but uses something that has not been verified and is therefore refused. A caller
    /// that has another way to read the data should fall back to it.
    /// </summary>
    Unsupported = 3,

    /// <summary>A file, directory or volume does not exist.</summary>
    NotFound = 4,

    /// <summary>An argument was invalid.</summary>
    InvalidInput = 5,

    /// <summary>A bug in the library.</summary>
    Internal = 6,

    /// <summary>
    /// An ESXi host had the disk open for writing when the image of the datastore was taken. What the image holds
    /// of it may be out of date and out of order. Take a VM snapshot before the storage snapshot and read the disk
    /// below it.
    /// </summary>
    InUse = 7,
}

/// <summary>An error reported by OpenVmfs. The message names the structure, field and offset involved.</summary>
public sealed class VmfsException : Exception
{
    /// <summary>Creates the exception.</summary>
    public VmfsException(VmfsErrorKind kind, string message) : base(message) => Kind = kind;

    /// <summary>The kind of error.</summary>
    public VmfsErrorKind Kind { get; }
}
