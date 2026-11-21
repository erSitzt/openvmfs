//! Raw disks on Windows (`\\.\PhysicalDrive3`).
//!
//! Windows does not tell the size of a raw disk through the file API, and reads from one must be whole sectors.
//! This module asks the disk driver for the size and opens the disk for aligned, unbuffered reading. Nothing is
//! written and the disk is not brought online: Windows lets an offline disk be read this way.
//!
//! This is the only place in the command line tool with unsafe code: one call into the operating system.

use std::sync::Arc;

use openvmfs::BlockSource;

/// Whether `path` names a Windows device (`\\.\...` or `\\?\...`) rather than a file.
pub(crate) fn is_raw_device(path: &str) -> bool {
    cfg!(windows) && (path.starts_with(r"\\.\") || path.starts_with(r"\\?\"))
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) fn open(path: &str) -> Result<Arc<dyn BlockSource>, openvmfs::Error> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;

    use openvmfs::{Error, FileSource};

    const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
    const IOCTL_DISK_GET_LENGTH_INFO: u32 = 0x0007_405C;

    #[link(name = "kernel32")]
    extern "system" {
        fn DeviceIoControl(
            device: *mut core::ffi::c_void,
            control_code: u32,
            in_buffer: *mut core::ffi::c_void,
            in_size: u32,
            out_buffer: *mut core::ffi::c_void,
            out_size: u32,
            bytes_returned: *mut u32,
            overlapped: *mut core::ffi::c_void,
        ) -> i32;
    }

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_NO_BUFFERING)
        .open(path)
        .map_err(|e| Error::io(format!("cannot open {path} (raw disks need an elevated prompt)"), e))?;
    let mut length: i64 = 0;
    let mut returned: u32 = 0;
    // SAFETY: the handle belongs to `file`, which outlives the call. The output buffer is one i64, which is what
    // IOCTL_DISK_GET_LENGTH_INFO writes (GET_LENGTH_INFORMATION), and its size is passed. No input buffer and no
    // overlapped structure are used; the handle was not opened for overlapped I/O.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle().cast(),
            IOCTL_DISK_GET_LENGTH_INFO,
            core::ptr::null_mut(),
            0,
            (&raw mut length).cast(),
            8,
            &raw mut returned,
            core::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(Error::io(
            format!("cannot determine the size of {path}"),
            std::io::Error::last_os_error(),
        ));
    }
    let length = u64::try_from(length)
        .ok()
        .filter(|&length| length > 0 && returned == 8)
        .ok_or_else(|| Error::invalid_input(format!("{path} reports no usable size")))?;
    // 4096 covers disks with 512-byte and with 4096-byte sectors.
    Ok(Arc::new(FileSource::with_len(file, length, path).with_alignment(4096)))
}

#[cfg(not(windows))]
pub(crate) fn open(path: &str) -> Result<Arc<dyn BlockSource>, openvmfs::Error> {
    Err(openvmfs::Error::invalid_input(format!(
        "{path}: raw device paths of this form exist on Windows only"
    )))
}
