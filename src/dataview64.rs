//! Port of `dataview64.js`. Wraps a byte slice the way JS `DataView` wraps
//! an `ArrayBuffer`; endianness-aware reads use Rust's native
//! `from_le_bytes`/`from_be_bytes` instead of a `DataView` object.
//!
//! `getUint64`/`getInt64` in the original hand-roll 64-bit reads (splitting
//! into two 32-bit reads, or a manual two's-complement byte walk) purely
//! because JS numbers are f64 and can't hold a full 64-bit integer exactly -
//! `getUint64` even throws once the value exceeds `Number.MAX_SAFE_INTEGER`.
//! Rust's native `u64`/`i64` have no such precision ceiling, so this port
//! uses `u64::from_le_bytes`/`i64::from_le_bytes` directly and drops the
//! safe-integer check - the check was a workaround for a limitation this
//! target language doesn't have, not a semantic requirement of the format
//! (BigTIFF 64-bit offsets are always within u64 range).
//!
//! Out-of-bounds offsets panic via slice indexing, mirroring how the
//! original throws via the underlying `DataView` on an invalid offset.

use crate::error::GeotiffError;
use half::f16;

pub struct DataView64<'a> {
    data: &'a [u8],
}

impl<'a> DataView64<'a> {
    /// `constructor(arrayBuffer)`
    pub fn new(array_buffer: &'a [u8]) -> Self {
        DataView64 { data: array_buffer }
    }

    /// `get buffer()`
    pub fn buffer(&self) -> &'a [u8] {
        self.data
    }

    fn bytes<const N: usize>(&self, offset: usize) -> Result<[u8; N], GeotiffError> {
        self.data
            .get(offset..offset.saturating_add(N))
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(GeotiffError::OutOfBoundsByteRead {
                offset: offset as u64,
                length: N,
                available: self.data.len(),
            })
    }

    /// `getUint64(offset, littleEndian)` - see module docs re: dropped
    /// MAX_SAFE_INTEGER check.
    pub fn get_uint64(&self, offset: usize, little_endian: bool) -> Result<u64, GeotiffError> {
        let b = self.bytes::<8>(offset)?;
        Ok(if little_endian {
            u64::from_le_bytes(b)
        } else {
            u64::from_be_bytes(b)
        })
    }

    /// `getInt64(offset, littleEndian)`
    pub fn get_int64(&self, offset: usize, little_endian: bool) -> Result<i64, GeotiffError> {
        let b = self.bytes::<8>(offset)?;
        Ok(if little_endian {
            i64::from_le_bytes(b)
        } else {
            i64::from_be_bytes(b)
        })
    }

    /// `getUint8(offset)`
    pub fn get_uint8(&self, offset: usize) -> Result<u8, GeotiffError> {
        self.data
            .get(offset)
            .copied()
            .ok_or(GeotiffError::OutOfBoundsByteRead {
                offset: offset as u64,
                length: 1,
                available: self.data.len(),
            })
    }

    /// `getInt8(offset)`
    pub fn get_int8(&self, offset: usize) -> Result<i8, GeotiffError> {
        self.get_uint8(offset).map(|value| value as i8)
    }

    /// `getUint16(offset, littleEndian)`
    pub fn get_uint16(&self, offset: usize, little_endian: bool) -> Result<u16, GeotiffError> {
        let b = self.bytes::<2>(offset)?;
        Ok(if little_endian {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }

    /// `getInt16(offset, littleEndian)`
    pub fn get_int16(&self, offset: usize, little_endian: bool) -> Result<i16, GeotiffError> {
        let b = self.bytes::<2>(offset)?;
        Ok(if little_endian {
            i16::from_le_bytes(b)
        } else {
            i16::from_be_bytes(b)
        })
    }

    /// `getUint32(offset, littleEndian)`
    pub fn get_uint32(&self, offset: usize, little_endian: bool) -> Result<u32, GeotiffError> {
        let b = self.bytes::<4>(offset)?;
        Ok(if little_endian {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    /// `getInt32(offset, littleEndian)`
    pub fn get_int32(&self, offset: usize, little_endian: bool) -> Result<i32, GeotiffError> {
        let b = self.bytes::<4>(offset)?;
        Ok(if little_endian {
            i32::from_le_bytes(b)
        } else {
            i32::from_be_bytes(b)
        })
    }

    /// `getFloat16(offset, littleEndian)` - delegates to `half::f16`, reading
    /// with the TIFF's actual endianness rather than a blind byte cast.
    pub fn get_float16(&self, offset: usize, little_endian: bool) -> Result<f32, GeotiffError> {
        let b = self.bytes::<2>(offset)?;
        let h = if little_endian {
            f16::from_le_bytes(b)
        } else {
            f16::from_be_bytes(b)
        };
        Ok(h.to_f32())
    }

    /// `getFloat32(offset, littleEndian)`
    pub fn get_float32(&self, offset: usize, little_endian: bool) -> Result<f32, GeotiffError> {
        let b = self.bytes::<4>(offset)?;
        Ok(if little_endian {
            f32::from_le_bytes(b)
        } else {
            f32::from_be_bytes(b)
        })
    }

    /// `getFloat64(offset, littleEndian)`
    pub fn get_float64(&self, offset: usize, little_endian: bool) -> Result<f64, GeotiffError> {
        let b = self.bytes::<8>(offset)?;
        Ok(if little_endian {
            f64::from_le_bytes(b)
        } else {
            f64::from_be_bytes(b)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_respect_endianness() {
        let data = [0x01, 0x00, 0x00, 0x00];
        let dv = DataView64::new(&data);
        assert_eq!(dv.get_uint32(0, true), Ok(1));
        assert_eq!(dv.get_uint32(0, false), Ok(0x0100_0000));
    }

    #[test]
    fn get_uint64_exceeds_js_safe_integer_without_erroring() {
        // JS `getUint64` would throw here (> Number.MAX_SAFE_INTEGER);
        // native u64 represents it exactly, per the module-level rationale.
        let data = [0xff; 8];
        let dv = DataView64::new(&data);
        assert_eq!(dv.get_uint64(0, true), Ok(u64::MAX));
    }

    #[test]
    fn get_int64_handles_negative_values() {
        let data = (-12345i64).to_le_bytes();
        let dv = DataView64::new(&data);
        assert_eq!(dv.get_int64(0, true), Ok(-12345));
    }

    #[test]
    fn get_float16_matches_known_value() {
        // 1.5 in IEEE 754 half precision, little-endian bytes
        let bytes = half::f16::from_f32(1.5).to_le_bytes();
        let dv = DataView64::new(&bytes);
        assert_eq!(dv.get_float16(0, true), Ok(1.5));
    }

    #[test]
    fn out_of_bounds_reads_are_errors_not_panics() {
        let dv = DataView64::new(&[1, 2, 3]);
        assert!(matches!(
            dv.get_uint32(0, true),
            Err(GeotiffError::OutOfBoundsByteRead { .. })
        ));
    }
}
