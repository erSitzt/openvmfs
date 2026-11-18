//! Bounds-checked little-endian field access and checked arithmetic for on-disk structures.

use std::fmt;

use crate::error::{Error, Result};

/// A read-only view of one on-disk structure. Every access is bounds-checked and reports the structure by name.
#[derive(Clone, Copy)]
pub(crate) struct View<'a> {
    what: &'static str,
    at: u64,
    buf: &'a [u8],
}

impl fmt::Debug for View<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "View({} at {:#x}, {} bytes)", self.what, self.at, self.buf.len())
    }
}

impl<'a> View<'a> {
    /// `what` names the structure, `at` is where it was read from (for messages only).
    pub(crate) fn new(what: &'static str, at: u64, buf: &'a [u8]) -> Self {
        View { what, at, buf }
    }

    pub(crate) fn bytes(&self, offset: usize, len: usize) -> Result<&'a [u8]> {
        offset
            .checked_add(len)
            .and_then(|end| self.buf.get(offset..end))
            .ok_or_else(|| {
                Error::corrupt(format!(
                    "{} at {:#x}: field at {:#x} (+{} bytes) lies outside the {}-byte structure",
                    self.what,
                    self.at,
                    offset,
                    len,
                    self.buf.len()
                ))
            })
    }

    fn array<const N: usize>(&self, offset: usize) -> Result<[u8; N]> {
        let bytes = self.bytes(offset, N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(bytes);
        Ok(out)
    }

    pub(crate) fn u8(&self, offset: usize) -> Result<u8> {
        Ok(u8::from_le_bytes(self.array(offset)?))
    }

    pub(crate) fn u16(&self, offset: usize) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array(offset)?))
    }

    pub(crate) fn u32(&self, offset: usize) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array(offset)?))
    }

    pub(crate) fn u64(&self, offset: usize) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array(offset)?))
    }

    pub(crate) fn uuid(&self, offset: usize) -> Result<crate::Uuid> {
        Ok(crate::Uuid::from_bytes(self.array(offset)?))
    }

    /// A NUL-padded text field. Invalid UTF-8 is replaced, because names are informational here.
    pub(crate) fn text(&self, offset: usize, len: usize) -> Result<String> {
        let raw = self.bytes(offset, len)?;
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        Ok(String::from_utf8_lossy(raw.get(..end).unwrap_or(raw)).into_owned())
    }
}

/// `a + b`, or a corrupt-data error naming what was being computed.
pub(crate) fn add(a: u64, b: u64, what: &str) -> Result<u64> {
    a.checked_add(b)
        .ok_or_else(|| Error::corrupt(format!("{what}: {a:#x} + {b:#x} overflows")))
}

/// `a * b`, or a corrupt-data error naming what was being computed.
pub(crate) fn mul(a: u64, b: u64, what: &str) -> Result<u64> {
    a.checked_mul(b)
        .ok_or_else(|| Error::corrupt(format!("{what}: {a:#x} * {b:#x} overflows")))
}

/// Converts a length that was already bounded to `usize`.
pub(crate) fn to_usize(value: u64, what: &str) -> Result<usize> {
    usize::try_from(value).map_err(|_| Error::corrupt(format!("{what}: {value:#x} does not fit into memory")))
}

/// log2 of a power of two, or `None`.
pub(crate) fn log2_exact(value: u64) -> Option<u32> {
    if value.is_power_of_two() {
        Some(value.trailing_zeros())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    #[test]
    fn reads_little_endian_fields() {
        let buf = [0x0d, 0xd0, 0x01, 0xc0, 0x06, 0, 0, 0, 0xff];
        let view = View::new("test", 0x100, &buf);
        assert_eq!(view.u32(0).unwrap(), 0xC001_D00D);
        assert_eq!(view.u16(4).unwrap(), 6);
        assert_eq!(view.u8(8).unwrap(), 0xff);
    }

    #[test]
    fn out_of_bounds_is_an_error_not_a_panic() {
        let buf = [0u8; 4];
        let view = View::new("test", 0, &buf);
        assert_eq!(view.u64(0).unwrap_err().kind(), ErrorKind::Corrupt);
        assert_eq!(view.u32(usize::MAX).unwrap_err().kind(), ErrorKind::Corrupt);
    }

    #[test]
    fn text_stops_at_nul() {
        let view = View::new("test", 0, b"vmfs6\0\0\0junk");
        assert_eq!(view.text(0, 8).unwrap(), "vmfs6");
    }

    #[test]
    fn checked_math_reports_overflow() {
        assert_eq!(add(u64::MAX, 1, "x").unwrap_err().kind(), ErrorKind::Corrupt);
        assert_eq!(mul(1 << 40, 1 << 40, "x").unwrap_err().kind(), ErrorKind::Corrupt);
        assert_eq!(log2_exact(1 << 20), Some(20));
        assert_eq!(log2_exact(3), None);
    }
}
