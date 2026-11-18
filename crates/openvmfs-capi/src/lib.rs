//! C interface to OpenVmfs. See `include/openvmfs.h` for the documentation of every function.
//!
//! Rules of this boundary:
//!
//! - Handles are opaque, owned by the caller and released with the matching `_close` / `_free` function. A disk
//!   stays valid after its volume has been closed.
//! - Every fallible function returns a status and, on failure, stores an error object in `out_error` (if that is
//!   not null). There is no global or thread-local error state.
//! - Read functions may be called from many threads at once on the same handle.
//! - No panic crosses the boundary: each entry point catches panics and reports them as `OVMFS_ERR_INTERNAL`.

use std::ffi::{c_char, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use openvmfs::{BlockSource, Disk, Error, ErrorKind, OpenOptions, Volume};

/// Version of this interface. Changes when a function or structure changes incompatibly.
pub const OVMFS_ABI_VERSION: u32 = 3;

/// Status codes. Zero is success.
pub const OVMFS_OK: i32 = 0;
/// The device could not be read.
pub const OVMFS_ERR_IO: i32 = 1;
/// The on-disk data contradicts the format.
pub const OVMFS_ERR_CORRUPT: i32 = 2;
/// The data may be valid but uses something that has not been verified; fall back to another way of reading.
pub const OVMFS_ERR_UNSUPPORTED: i32 = 3;
/// A file, directory or volume does not exist.
pub const OVMFS_ERR_NOT_FOUND: i32 = 4;
/// An argument was invalid.
pub const OVMFS_ERR_INVALID_INPUT: i32 = 5;
/// A bug in the library.
pub const OVMFS_ERR_INTERNAL: i32 = 6;
/// A host had the file open for writing when the image was taken; what the image holds of it is not reliable.
pub const OVMFS_ERR_IN_USE: i32 = 7;

/// Flag for `ovmfs_volume_open`: read structures that have not been verified against ESXi instead of refusing.
pub const OVMFS_OPEN_ACCEPT_UNVERIFIED: u32 = 1;

/// Flag for `ovmfs_volume_open`: read the devices without the page cache of the operating system (`O_DIRECT`).
pub const OVMFS_OPEN_DIRECT: u32 = 2;
/// Flag for `ovmfs_volume_open`: open disks although a host was writing to them when the image was taken.
pub const OVMFS_OPEN_ACCEPT_IN_USE: u32 = 4;
/// Flag for `ovmfs_volume_open`: read the volume with its journals replayed (experimental).
pub const OVMFS_OPEN_REPLAY_JOURNAL: u32 = 8;
const OPEN_FLAGS: u32 =
    OVMFS_OPEN_ACCEPT_UNVERIFIED | OVMFS_OPEN_DIRECT | OVMFS_OPEN_ACCEPT_IN_USE | OVMFS_OPEN_REPLAY_JOURNAL;

/// Most devices `ovmfs_volume_open_devices` accepts.
const MAX_DEVICES: usize = 32;

/// An opened volume.
#[allow(non_camel_case_types)]
#[derive(Debug)]
pub struct ovmfs_volume(Volume);

/// An opened virtual disk.
#[allow(non_camel_case_types)]
#[derive(Debug)]
pub struct ovmfs_disk(Disk);

/// An error with its status code and message.
#[allow(non_camel_case_types)]
#[derive(Debug)]
pub struct ovmfs_error {
    status: i32,
    message: CString,
}

/// A range of a disk, in bytes.
#[allow(non_camel_case_types)]
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ovmfs_range {
    /// First byte of the range.
    pub offset: u64,
    /// Length of the range.
    pub length: u64,
}

/// Facts about a volume.
#[allow(non_camel_case_types)]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ovmfs_volume_info {
    /// Size of the logical volume in bytes.
    pub capacity: u64,
    /// Size of a file block in bytes.
    pub file_block_size: u64,
    /// Offset of the VMFS partition on the opened device.
    pub partition_offset: u64,
    /// Heartbeat slots in use: hosts that held locks when the image was taken. 0 does not prove an unmount.
    pub active_heartbeats: u32,
    /// Filesystem minor version, for example 82 for VMFS 6.82.
    pub minor_version: u32,
    /// Datastore UUID as stored on disk.
    pub uuid: [u8; 16],
    /// Datastore label, NUL-terminated UTF-8.
    pub label: [u8; 129],
}

fn status_of(kind: ErrorKind) -> i32 {
    match kind {
        ErrorKind::Io => OVMFS_ERR_IO,
        ErrorKind::Corrupt => OVMFS_ERR_CORRUPT,
        ErrorKind::Unsupported => OVMFS_ERR_UNSUPPORTED,
        ErrorKind::NotFound => OVMFS_ERR_NOT_FOUND,
        ErrorKind::InvalidInput => OVMFS_ERR_INVALID_INPUT,
        ErrorKind::InUse => OVMFS_ERR_IN_USE,
        _ => OVMFS_ERR_INTERNAL,
    }
}

struct Failure {
    status: i32,
    message: String,
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Failure {
            status: status_of(error.kind()),
            message: error.to_string(),
        }
    }
}

fn invalid(message: &str) -> Failure {
    Failure {
        status: OVMFS_ERR_INVALID_INPUT,
        message: message.to_owned(),
    }
}

/// Runs `body`, converts its failure or panic into a status, and stores the error object if the caller wants it.
///
/// # Safety
/// `out_error` must be null or valid for writing one pointer.
unsafe fn guarded(out_error: *mut *mut ovmfs_error, body: impl FnOnce() -> Result<(), Failure>) -> i32 {
    let failure = match catch_unwind(AssertUnwindSafe(body)) {
        Ok(Ok(())) => return OVMFS_OK,
        Ok(Err(failure)) => failure,
        Err(_) => Failure {
            status: OVMFS_ERR_INTERNAL,
            message: "internal error (panic) in OpenVmfs".to_owned(),
        },
    };
    if !out_error.is_null() {
        // A message cannot contain a NUL byte in practice; replace any to keep the conversion infallible.
        let message = CString::new(failure.message.replace('\0', " ")).unwrap_or_default();
        let error = Box::new(ovmfs_error {
            status: failure.status,
            message,
        });
        // SAFETY: the caller guarantees that a non-null `out_error` is valid for writing one pointer.
        unsafe { out_error.write(Box::into_raw(error)) };
    }
    failure.status
}

/// # Safety
/// `text` must be null or point to a NUL-terminated string that stays valid for the duration of the call.
unsafe fn utf8<'a>(text: *const c_char, what: &str) -> Result<&'a str, Failure> {
    if text.is_null() {
        return Err(invalid(&format!("{what} is null")));
    }
    // SAFETY: the caller guarantees a valid NUL-terminated string.
    let text = unsafe { CStr::from_ptr(text) };
    text.to_str()
        .map_err(|_| invalid(&format!("{what} is not valid UTF-8")))
}

/// Version of this interface.
#[no_mangle]
pub extern "C" fn ovmfs_abi_version() -> u32 {
    OVMFS_ABI_VERSION
}

/// Opens the VMFS6 volume on a device or in an image file.
///
/// # Safety
/// `device_path` must be a NUL-terminated string. `out_volume` must be valid for writing one pointer. `out_error`
/// must be null or valid for writing one pointer.
#[no_mangle]
pub unsafe extern "C" fn ovmfs_volume_open(
    device_path: *const c_char,
    partition: u32,
    flags: u32,
    out_volume: *mut *mut ovmfs_volume,
    out_error: *mut *mut ovmfs_error,
) -> i32 {
    // SAFETY: forwarded caller guarantees; see the function's safety section.
    unsafe {
        guarded(out_error, || {
            if out_volume.is_null() {
                return Err(invalid("out_volume is null"));
            }
            out_volume.write(std::ptr::null_mut());
            if flags & !OPEN_FLAGS != 0 {
                return Err(invalid("unknown open flags"));
            }
            let path = utf8(device_path, "device_path")?;
            let options = OpenOptions {
                partition: (partition != 0).then_some(partition),
                accept_unverified: flags & OVMFS_OPEN_ACCEPT_UNVERIFIED != 0,
                direct: flags & OVMFS_OPEN_DIRECT != 0,
                accept_in_use: flags & OVMFS_OPEN_ACCEPT_IN_USE != 0,
                replay_journal: flags & OVMFS_OPEN_REPLAY_JOURNAL != 0,
            };
            let volume = Volume::open_paths(&[path], &options)?;
            out_volume.write(Box::into_raw(Box::new(ovmfs_volume(volume))));
            Ok(())
        })
    }
}

/// Opens a VMFS6 volume that is stored on several devices (a datastore with extents). All devices of the datastore
/// are needed, in any order. With one device this is the same as `ovmfs_volume_open`.
///
/// # Safety
/// `device_paths` must point to `device_count` pointers to NUL-terminated strings. `out_volume` must be valid for
/// writing one pointer. `out_error` must be null or valid for writing one pointer.
#[no_mangle]
pub unsafe extern "C" fn ovmfs_volume_open_devices(
    device_paths: *const *const c_char,
    device_count: usize,
    partition: u32,
    flags: u32,
    out_volume: *mut *mut ovmfs_volume,
    out_error: *mut *mut ovmfs_error,
) -> i32 {
    // SAFETY: forwarded caller guarantees; see the function's safety section.
    unsafe {
        guarded(out_error, || {
            if out_volume.is_null() {
                return Err(invalid("out_volume is null"));
            }
            out_volume.write(std::ptr::null_mut());
            if flags & !OPEN_FLAGS != 0 {
                return Err(invalid("unknown open flags"));
            }
            if device_paths.is_null() || device_count == 0 {
                return Err(invalid("no device given"));
            }
            if device_count > MAX_DEVICES {
                return Err(invalid("too many devices"));
            }
            let mut paths = Vec::with_capacity(device_count);
            for index in 0..device_count {
                paths.push(utf8(device_paths.add(index).read(), "a device path")?);
            }
            let options = OpenOptions {
                partition: (partition != 0).then_some(partition),
                accept_unverified: flags & OVMFS_OPEN_ACCEPT_UNVERIFIED != 0,
                direct: flags & OVMFS_OPEN_DIRECT != 0,
                accept_in_use: flags & OVMFS_OPEN_ACCEPT_IN_USE != 0,
                replay_journal: flags & OVMFS_OPEN_REPLAY_JOURNAL != 0,
            };
            let volume = Volume::open_paths(&paths, &options)?;
            out_volume.write(Box::into_raw(Box::new(ovmfs_volume(volume))));
            Ok(())
        })
    }
}

/// Reads `length` bytes at `offset` of the caller's device into `buffer`. Returns 0 on success.
#[allow(non_camel_case_types)]
pub type ovmfs_read_fn = unsafe extern "C" fn(context: *mut c_void, offset: u64, buffer: *mut u8, length: usize) -> i32;
/// Tells the caller that the library no longer uses the device.
#[allow(non_camel_case_types)]
pub type ovmfs_release_fn = unsafe extern "C" fn(context: *mut c_void);

/// A device that is read through a function of the caller.
struct CallbackSource {
    read: ovmfs_read_fn,
    release: Option<ovmfs_release_fn>,
    context: *mut c_void,
    len: u64,
}

// SAFETY: the caller of `ovmfs_volume_open_device` promises that `read` may be called from any thread, from several
// at once, and that `context` stays valid until `release` is called.
unsafe impl Send for CallbackSource {}
// SAFETY: see above.
unsafe impl Sync for CallbackSource {}

impl std::fmt::Debug for CallbackSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CallbackSource({} bytes)", self.len)
    }
}

impl Drop for CallbackSource {
    fn drop(&mut self) {
        if let Some(release) = self.release {
            // SAFETY: the caller's function with the caller's context, called once.
            unsafe { release(self.context) };
        }
    }
}

impl BlockSource for CallbackSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), Error> {
        match offset.checked_add(buf.len() as u64) {
            Some(end) if end <= self.len => {}
            _ => {
                return Err(Error::corrupt(format!(
                    "read of {} bytes at {offset:#x} goes beyond the end of the device ({:#x} bytes)",
                    buf.len(),
                    self.len
                )));
            }
        }
        if buf.is_empty() {
            return Ok(());
        }
        // SAFETY: `buf` is valid for writing `buf.len()` bytes; the caller's function and context are valid until
        // `release` is called, which happens only when this object is dropped.
        let status = unsafe { (self.read)(self.context, offset, buf.as_mut_ptr(), buf.len()) };
        if status == 0 {
            Ok(())
        } else {
            Err(Error::new(
                ErrorKind::Io,
                format!(
                    "the caller's read function failed with status {status} for {} bytes at {offset:#x}",
                    buf.len()
                ),
            ))
        }
    }
}

/// Opens the VMFS6 volume on a device that is read through a function of the caller: for devices that are not a
/// path, such as a LUN reached through a tunnel, or an image in memory.
///
/// `release`, if not null, is called exactly once when the library no longer uses the device: when this call
/// fails, or when the volume and every disk opened from it have been closed.
///
/// # Safety
/// `read` must be callable from any thread and from several threads at once until `release` has been called.
/// `out_volume` must be valid for writing one pointer. `out_error` must be null or valid for writing one pointer.
#[no_mangle]
pub unsafe extern "C" fn ovmfs_volume_open_device(
    read: Option<ovmfs_read_fn>,
    release: Option<ovmfs_release_fn>,
    context: *mut c_void,
    size: u64,
    partition: u32,
    flags: u32,
    out_volume: *mut *mut ovmfs_volume,
    out_error: *mut *mut ovmfs_error,
) -> i32 {
    // SAFETY: forwarded caller guarantees; see the function's safety section.
    unsafe {
        guarded(out_error, || {
            let Some(read) = read else {
                return Err(invalid("read is null"));
            };
            // From here on the device is owned: dropping it tells the caller.
            let device = Arc::new(CallbackSource {
                read,
                release,
                context,
                len: size,
            });
            if out_volume.is_null() {
                return Err(invalid("out_volume is null"));
            }
            out_volume.write(std::ptr::null_mut());
            if flags & !OPEN_FLAGS != 0 {
                return Err(invalid("unknown open flags"));
            }
            if size == 0 {
                return Err(invalid("the device has size 0"));
            }
            let options = OpenOptions {
                partition: (partition != 0).then_some(partition),
                accept_unverified: flags & OVMFS_OPEN_ACCEPT_UNVERIFIED != 0,
                // Unbuffered reading is the caller's business here.
                direct: false,
                accept_in_use: flags & OVMFS_OPEN_ACCEPT_IN_USE != 0,
                replay_journal: flags & OVMFS_OPEN_REPLAY_JOURNAL != 0,
            };
            let volume = Volume::open_with(device, &options)?;
            out_volume.write(Box::into_raw(Box::new(ovmfs_volume(volume))));
            Ok(())
        })
    }
}

/// Closes a volume. Disks opened from it stay valid. Null is ignored.
///
/// # Safety
/// `volume` must be null or a handle returned by `ovmfs_volume_open` that has not been closed, and no other thread
/// may be using it.
#[no_mangle]
pub unsafe extern "C" fn ovmfs_volume_close(volume: *mut ovmfs_volume) {
    if !volume.is_null() {
        // SAFETY: the caller guarantees a live handle that nobody else uses; it was created by `Box::into_raw`.
        drop(unsafe { Box::from_raw(volume) });
    }
}

/// Fills `out_info` with facts about the volume.
///
/// # Safety
/// `volume` must be a live handle. `out_info` must be valid for writing one `ovmfs_volume_info`. `out_error` must
/// be null or valid for writing one pointer.
#[no_mangle]
pub unsafe extern "C" fn ovmfs_volume_get_info(
    volume: *const ovmfs_volume,
    out_info: *mut ovmfs_volume_info,
    out_error: *mut *mut ovmfs_error,
) -> i32 {
    // SAFETY: forwarded caller guarantees; see the function's safety section.
    unsafe {
        guarded(out_error, || {
            let volume = volume.as_ref().ok_or_else(|| invalid("volume is null"))?;
            if out_info.is_null() {
                return Err(invalid("out_info is null"));
            }
            let info = volume.0.info();
            let mut label = [0u8; 129];
            let bytes = info.label.as_bytes();
            let len = bytes.len().min(label.len() - 1);
            label[..len].copy_from_slice(&bytes[..len]);
            out_info.write(ovmfs_volume_info {
                capacity: info.capacity,
                file_block_size: info.file_block_size,
                partition_offset: info.partition_offset,
                active_heartbeats: info.active_heartbeats,
                minor_version: u32::from(info.descriptor.minor_version),
                uuid: *info.uuid.as_bytes(),
                label,
            });
            Ok(())
        })
    }
}

/// Opens the virtual disk described by the descriptor file at `descriptor_path` inside the datastore.
///
/// # Safety
/// `volume` must be a live handle. `descriptor_path` must be a NUL-terminated string. `out_disk` must be valid for
/// writing one pointer. `out_error` must be null or valid for writing one pointer.
#[no_mangle]
pub unsafe extern "C" fn ovmfs_disk_open(
    volume: *const ovmfs_volume,
    descriptor_path: *const c_char,
    out_disk: *mut *mut ovmfs_disk,
    out_error: *mut *mut ovmfs_error,
) -> i32 {
    // SAFETY: forwarded caller guarantees; see the function's safety section.
    unsafe {
        guarded(out_error, || {
            if out_disk.is_null() {
                return Err(invalid("out_disk is null"));
            }
            out_disk.write(std::ptr::null_mut());
            let volume = volume.as_ref().ok_or_else(|| invalid("volume is null"))?;
            let path = utf8(descriptor_path, "descriptor_path")?;
            let disk = volume.0.open_disk(path)?;
            out_disk.write(Box::into_raw(Box::new(ovmfs_disk(disk))));
            Ok(())
        })
    }
}

/// Closes a disk. Null is ignored.
///
/// # Safety
/// `disk` must be null or a handle returned by `ovmfs_disk_open` that has not been closed, and no other thread may
/// be using it.
#[no_mangle]
pub unsafe extern "C" fn ovmfs_disk_close(disk: *mut ovmfs_disk) {
    if !disk.is_null() {
        // SAFETY: the caller guarantees a live handle that nobody else uses; it was created by `Box::into_raw`.
        drop(unsafe { Box::from_raw(disk) });
    }
}

/// Size of the disk in bytes; 0 for a null handle.
///
/// # Safety
/// `disk` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn ovmfs_disk_capacity(disk: *const ovmfs_disk) -> u64 {
    // SAFETY: the caller guarantees null or a live handle.
    unsafe { disk.as_ref() }.map_or(0, |disk| disk.0.capacity())
}

/// Fills `buffer` with the `length` bytes of the disk at `offset`. Unallocated and never-written ranges read as
/// zeros. May be called from many threads at once.
///
/// # Safety
/// `disk` must be a live handle. `buffer` must be valid for writing `length` bytes and must not be read or written
/// by anyone else until the call returns; it may be null when `length` is 0. `out_error` must be null or valid for
/// writing one pointer.
#[no_mangle]
pub unsafe extern "C" fn ovmfs_disk_read(
    disk: *const ovmfs_disk,
    offset: u64,
    buffer: *mut u8,
    length: usize,
    out_error: *mut *mut ovmfs_error,
) -> i32 {
    // SAFETY: forwarded caller guarantees; see the function's safety section.
    unsafe {
        guarded(out_error, || {
            let disk = disk.as_ref().ok_or_else(|| invalid("disk is null"))?;
            if length == 0 {
                // Nothing to do, and a null buffer must not be turned into a slice.
                return Ok(());
            }
            if buffer.is_null() {
                return Err(invalid("buffer is null"));
            }
            let buffer = std::slice::from_raw_parts_mut(buffer, length);
            disk.0.read_exact_at(offset, buffer)?;
            Ok(())
        })
    }
}

/// Number of ranges of the disk that hold data. Everything outside them reads as zeros.
///
/// # Safety
/// `disk` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn ovmfs_disk_data_range_count(disk: *const ovmfs_disk) -> usize {
    // SAFETY: the caller guarantees null or a live handle.
    unsafe { disk.as_ref() }.map_or(0, |disk| disk.0.data_ranges().len())
}

/// Copies up to `capacity` data ranges, sorted by offset, into `out_ranges` and returns how many were copied.
///
/// # Safety
/// `disk` must be null or a live handle. `out_ranges` must be valid for writing `capacity` elements; it may be null
/// when `capacity` is 0.
#[no_mangle]
pub unsafe extern "C" fn ovmfs_disk_data_ranges(
    disk: *const ovmfs_disk,
    out_ranges: *mut ovmfs_range,
    capacity: usize,
) -> usize {
    // SAFETY: the caller guarantees null or a live handle.
    let Some(disk) = (unsafe { disk.as_ref() }) else {
        return 0;
    };
    if out_ranges.is_null() || capacity == 0 {
        return 0;
    }
    let ranges = disk.0.data_ranges();
    let count = ranges.len().min(capacity);
    // SAFETY: the caller guarantees room for `capacity` elements, and `count <= capacity`.
    let out = unsafe { std::slice::from_raw_parts_mut(out_ranges, count) };
    for (slot, range) in out.iter_mut().zip(ranges) {
        *slot = ovmfs_range {
            offset: range.start,
            length: range.end.saturating_sub(range.start),
        };
    }
    count
}

/// Status code of an error; `OVMFS_OK` for null.
///
/// # Safety
/// `error` must be null or a live error object.
#[no_mangle]
pub unsafe extern "C" fn ovmfs_error_status(error: *const ovmfs_error) -> i32 {
    // SAFETY: the caller guarantees null or a live error object.
    unsafe { error.as_ref() }.map_or(OVMFS_OK, |error| error.status)
}

/// Message of an error as NUL-terminated UTF-8, valid until the error is freed. An empty string for null.
///
/// # Safety
/// `error` must be null or a live error object.
#[no_mangle]
pub unsafe extern "C" fn ovmfs_error_message(error: *const ovmfs_error) -> *const c_char {
    // SAFETY: the caller guarantees null or a live error object.
    match unsafe { error.as_ref() } {
        Some(error) => error.message.as_ptr(),
        None => c"".as_ptr(),
    }
}

/// Frees an error object. Null is ignored.
///
/// # Safety
/// `error` must be null or an error object returned by this library that has not been freed.
#[no_mangle]
pub unsafe extern "C" fn ovmfs_error_free(error: *mut ovmfs_error) {
    if !error.is_null() {
        // SAFETY: the caller guarantees a live error object; it was created by `Box::into_raw`.
        drop(unsafe { Box::from_raw(error) });
    }
}
