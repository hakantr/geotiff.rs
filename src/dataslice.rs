//! Port of `dataslice.js`. Structurally independent from `DataView64`
//! (`dataview64.rs`) - the original JS duplicates the same byte-read logic
//! across both classes rather than sharing it, and this port preserves that
//! same duplication rather than merging them, since deduplicating would be a
//! design change beyond either function's original scope.
//!
//! `readUint64`/`readInt64` drop the JS `MAX_SAFE_INTEGER` workaround for
//! the same reason as `DataView64::get_uint64`/`get_int64` - see
//! `dataview64.rs` module docs.

use crate::error::GeotiffError;
use bytes::Bytes;

#[derive(Debug, Clone)]
pub struct DataSlice {
    data: Bytes,
    slice_offset: u64,
    little_endian: bool,
    big_tiff: bool,
}

impl DataSlice {
    /// `constructor(arrayBuffer, sliceOffset, littleEndian, bigTiff)`
    pub fn new(
        array_buffer: &[u8],
        slice_offset: u64,
        little_endian: bool,
        big_tiff: bool,
    ) -> Self {
        DataSlice {
            data: Bytes::copy_from_slice(array_buffer),
            slice_offset,
            little_endian,
            big_tiff,
        }
    }

    /// Zero-copy constructor used by source-backed `GeoTIFF.getSlice()`.
    pub fn from_bytes(
        array_buffer: Bytes,
        slice_offset: u64,
        little_endian: bool,
        big_tiff: bool,
    ) -> Self {
        DataSlice {
            data: array_buffer,
            slice_offset,
            little_endian,
            big_tiff,
        }
    }

    /// `get sliceOffset()`
    pub fn slice_offset(&self) -> u64 {
        self.slice_offset
    }

    /// `get sliceTop()`
    pub fn slice_top(&self) -> u64 {
        self.slice_offset.saturating_add(self.data.len() as u64)
    }

    /// `get littleEndian()`
    pub fn little_endian(&self) -> bool {
        self.little_endian
    }

    /// `get bigTiff()`
    pub fn big_tiff(&self) -> bool {
        self.big_tiff
    }

    /// `get buffer()`
    pub fn buffer(&self) -> &[u8] {
        &self.data
    }

    /// `covers(offset, length)`
    pub fn covers(&self, offset: u64, length: u64) -> bool {
        self.slice_offset() <= offset
            && offset
                .checked_add(length)
                .is_some_and(|end| self.slice_top() >= end)
    }

    fn local(&self, offset: u64, length: usize) -> Result<usize, GeotiffError> {
        let local = offset
            .checked_sub(self.slice_offset)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(GeotiffError::OutOfBoundsByteRead {
                offset,
                length,
                available: self.data.len(),
            })?;
        local
            .checked_add(length)
            .filter(|end| *end <= self.data.len())
            .map(|_| local)
            .ok_or(GeotiffError::OutOfBoundsByteRead {
                offset,
                length,
                available: self.data.len(),
            })
    }

    fn bytes<const N: usize>(&self, offset: u64) -> Result<[u8; N], GeotiffError> {
        let start = self.local(offset, N)?;
        self.data[start..start + N]
            .try_into()
            .map_err(|_| GeotiffError::OutOfBoundsByteRead {
                offset,
                length: N,
                available: self.data.len(),
            })
    }

    /// `readUint8(offset)`
    pub fn read_uint8(&self, offset: u64) -> Result<u8, GeotiffError> {
        Ok(self.data[self.local(offset, 1)?])
    }

    /// `readInt8(offset)`
    pub fn read_int8(&self, offset: u64) -> Result<i8, GeotiffError> {
        self.read_uint8(offset).map(|value| value as i8)
    }

    /// `readUint16(offset)`
    pub fn read_uint16(&self, offset: u64) -> Result<u16, GeotiffError> {
        let b = self.bytes::<2>(offset)?;
        Ok(if self.little_endian {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }

    /// `readInt16(offset)`
    pub fn read_int16(&self, offset: u64) -> Result<i16, GeotiffError> {
        let b = self.bytes::<2>(offset)?;
        Ok(if self.little_endian {
            i16::from_le_bytes(b)
        } else {
            i16::from_be_bytes(b)
        })
    }

    /// `readUint32(offset)`
    pub fn read_uint32(&self, offset: u64) -> Result<u32, GeotiffError> {
        let b = self.bytes::<4>(offset)?;
        Ok(if self.little_endian {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    /// `readInt32(offset)`
    pub fn read_int32(&self, offset: u64) -> Result<i32, GeotiffError> {
        let b = self.bytes::<4>(offset)?;
        Ok(if self.little_endian {
            i32::from_le_bytes(b)
        } else {
            i32::from_be_bytes(b)
        })
    }

    /// `readFloat32(offset)`
    pub fn read_float32(&self, offset: u64) -> Result<f32, GeotiffError> {
        let b = self.bytes::<4>(offset)?;
        Ok(if self.little_endian {
            f32::from_le_bytes(b)
        } else {
            f32::from_be_bytes(b)
        })
    }

    /// `readFloat64(offset)`
    pub fn read_float64(&self, offset: u64) -> Result<f64, GeotiffError> {
        let b = self.bytes::<8>(offset)?;
        Ok(if self.little_endian {
            f64::from_le_bytes(b)
        } else {
            f64::from_be_bytes(b)
        })
    }

    /// `readUint64(offset)` - see module docs re: dropped MAX_SAFE_INTEGER check.
    pub fn read_uint64(&self, offset: u64) -> Result<u64, GeotiffError> {
        let b = self.bytes::<8>(offset)?;
        Ok(if self.little_endian {
            u64::from_le_bytes(b)
        } else {
            u64::from_be_bytes(b)
        })
    }

    /// `readInt64(offset)`
    pub fn read_int64(&self, offset: u64) -> Result<i64, GeotiffError> {
        let b = self.bytes::<8>(offset)?;
        Ok(if self.little_endian {
            i64::from_le_bytes(b)
        } else {
            i64::from_be_bytes(b)
        })
    }

    /// `readOffset(offset)` - widens the 32-bit (classic TIFF) case to u64
    /// so the caller has one return type regardless of format.
    pub fn read_offset(&self, offset: u64) -> Result<u64, GeotiffError> {
        if self.big_tiff {
            self.read_uint64(offset)
        } else {
            self.read_uint32(offset).map(u64::from)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_matches_original_boundary_semantics() {
        let data = [0u8; 16];
        let slice = DataSlice::new(&data, 100, true, false);
        assert!(slice.covers(100, 16));
        assert!(slice.covers(104, 8));
        assert!(!slice.covers(100, 17));
        assert!(!slice.covers(99, 16));
    }

    #[test]
    fn reads_are_relative_to_slice_offset() {
        let data = [0xef, 0xbe, 0xad, 0xde]; // 0xdeadbeef little-endian
        let slice = DataSlice::new(&data, 1000, true, false);
        assert_eq!(slice.read_uint32(1000), Ok(0xdead_beef));
    }

    #[test]
    fn read_offset_dispatches_on_big_tiff() {
        let mut data = vec![0u8; 8];
        data[0..4].copy_from_slice(&42u32.to_le_bytes());
        let classic = DataSlice::new(&data, 0, true, false);
        assert_eq!(classic.read_offset(0), Ok(42));

        let mut big_data = vec![0u8; 8];
        big_data.copy_from_slice(&123456789012u64.to_le_bytes());
        let big = DataSlice::new(&big_data, 0, true, true);
        assert_eq!(big.read_offset(0), Ok(123456789012));
    }

    #[test]
    fn offsets_before_or_after_the_slice_are_normal_errors() {
        let slice = DataSlice::new(&[1, 2, 3, 4], 100, true, false);
        assert!(matches!(
            slice.read_uint16(99),
            Err(GeotiffError::OutOfBoundsByteRead { .. })
        ));
        assert!(matches!(
            slice.read_uint32(101),
            Err(GeotiffError::OutOfBoundsByteRead { .. })
        ));
    }
}
