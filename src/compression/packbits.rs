//! Port of `compression/packbits.js`. Pure RLE algorithm, no external
//! crate; `compression::registry::PackBitsDecoder` wires it into every
//! public raster path.

use crate::error::GeotiffError;

/// `PackbitsDecoder.decodeBlock(buffer)`. Truncated runs surface as a normal
/// error instead of indexing past an untrusted compressed buffer.
pub fn decode_block(buffer: &[u8]) -> Result<Vec<u8>, GeotiffError> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < buffer.len() {
        let header = buffer[i] as i8;
        if header < 0 {
            let next = *buffer.get(i + 1).ok_or(GeotiffError::OutOfBoundsByteRead {
                offset: (i + 1) as u64,
                length: 1,
                available: buffer.len(),
            })?;
            let count = (-(header as i16) + 1) as usize;
            out.try_reserve_exact(count)
                .map_err(|error| GeotiffError::RasterAllocationFailed(error.to_string()))?;
            out.extend(std::iter::repeat_n(next, count));
            i += 1;
        } else {
            let count = header as usize + 1;
            let start = i + 1;
            let end = start.checked_add(count).ok_or_else(|| {
                GeotiffError::InvalidRasterDimensions(
                    "PackBits literal run offset overflow".to_string(),
                )
            })?;
            let literal = buffer
                .get(start..end)
                .ok_or(GeotiffError::OutOfBoundsByteRead {
                    offset: start as u64,
                    length: count,
                    available: buffer.len(),
                })?;
            out.try_reserve_exact(count)
                .map_err(|error| GeotiffError::RasterAllocationFailed(error.to_string()))?;
            out.extend_from_slice(literal);
            i += count;
        }
        i += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_run_is_copied_verbatim() {
        // header 2 (positive) -> copy the next 3 literal bytes
        let input = [2, 10, 20, 30];
        assert_eq!(decode_block(&input).unwrap(), vec![10, 20, 30]);
    }

    #[test]
    fn negative_header_repeats_the_next_byte() {
        // header -3 (as i8, stored as 0xFD) -> repeat next byte 4 times
        let input = [0xFDu8, 99];
        assert_eq!(decode_block(&input).unwrap(), vec![99, 99, 99, 99]);
    }

    #[test]
    fn truncated_runs_are_errors_instead_of_panics() {
        assert!(decode_block(&[2, 10]).is_err());
        assert!(decode_block(&[0xFF]).is_err());
    }
}
