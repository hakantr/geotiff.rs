//! Port of `predictor.js`: `decodeRowAcc`, `decodeRowFloatingPoint`, and the
//! exported `applyPredictor` entry point.
//! Used by `src/block.rs` for both striped and tiled TIFFs, including valid
//! layouts that `async_tiff::Tile::decode` cannot normalize losslessly.

use crate::error::GeotiffError;
use crate::typed_array::TypedArray;
use async_tiff::tags::{PlanarConfiguration, Predictor};

/// `decodeRowAcc(row, stride)` - horizontal-differencing predictor decode
/// (TIFF Predictor 2), in place. JS typed-array element assignment
/// auto-truncates to the array's bit width (e.g. `Uint8Array` wraps mod
/// 256), which is exactly the modular arithmetic this predictor relies on -
/// `wrapping_add` is required here, not incidental, to reproduce that.
pub fn decode_row_acc(row: &mut TypedArray, stride: usize) -> Result<(), GeotiffError> {
    if stride == 0 || row.len() < stride {
        return Err(GeotiffError::InvalidRasterDimensions(format!(
            "predictor row length {} is smaller than stride {stride}",
            row.len()
        )));
    }
    match row {
        TypedArray::Uint8(v) => decode_row_acc_impl(v, stride, u8::wrapping_add),
        TypedArray::Uint16(v) => decode_row_acc_impl(v, stride, u16::wrapping_add),
        TypedArray::Uint32(v) => decode_row_acc_impl(v, stride, u32::wrapping_add),
        _ => {
            return Err(GeotiffError::InvalidRasterDimensions(
                "decodeRowAcc requires Uint8, Uint16, or Uint32 input".to_string(),
            ));
        }
    }
    Ok(())
}

fn decode_row_acc_impl<T: Copy>(row: &mut [T], stride: usize, add: impl Fn(T, T) -> T) {
    // Real callers always pass a row whose length is an exact multiple of
    // `stride` (a full scanline); this mirrors the original's assumption
    // (JS typed arrays silently no-op/produce `undefined` on an
    // out-of-bounds index instead of throwing, so the original never
    // surfaced a mismatch either).
    let mut length = row.len() as i64 - stride as i64;
    let mut offset = 0usize;
    loop {
        for _ in 0..stride {
            row[offset + stride] = add(row[offset + stride], row[offset]);
            offset += 1;
        }
        length -= stride as i64;
        if length <= 0 {
            break;
        }
    }
}

/// `decodeRowFloatingPoint(row, stride, bytesPerSample)` - horizontal
/// floating-point predictor decode (TIFF Predictor 3), in place, always on
/// raw bytes regardless of the eventual float width (matches the original,
/// which always passes a `Uint8Array` here even for the 32/64-bit float
/// cases - the byte-level differencing and de-interleaving is the same
/// either way).
pub fn decode_row_floating_point(
    row: &mut [u8],
    stride: usize,
    bytes_per_sample: usize,
) -> Result<(), GeotiffError> {
    if stride == 0 || bytes_per_sample == 0 || !row.len().is_multiple_of(bytes_per_sample) {
        return Err(GeotiffError::InvalidRasterDimensions(format!(
            "invalid floating-point predictor row: length={}, stride={stride}, bytes/sample={bytes_per_sample}",
            row.len()
        )));
    }
    let mut index = 0usize;
    let mut count = row.len();
    let wc = count / bytes_per_sample;

    while count > stride {
        for _ in 0..stride {
            row[index + stride] = row[index + stride].wrapping_add(row[index]);
            index += 1;
        }
        count -= stride;
    }

    let copy = row.to_vec();
    for i in 0..wc {
        for b in 0..bytes_per_sample {
            row[(bytes_per_sample * i) + b] = copy[((bytes_per_sample - b - 1) * wc) + i];
        }
    }
    Ok(())
}

/// `applyPredictor(block, predictor, width, height, bitsPerSample, planarConfiguration)`.
/// `predictor` of `None` matches JS's `!predictor` (tag absent - no-op,
/// same as `Predictor::None`). `bits_per_sample[i] % 8 != 0` or samples of
/// differing width panic instead of throwing `Error` - both are hard
/// failures for malformed input the original never actually receives in
/// practice (real TIFF encoders always emit byte-aligned, uniform-width
/// samples), consistent with how the rest of this port treats invariant
/// violations as bugs, not recoverable conditions.
pub fn apply_predictor(
    block: &mut [u8],
    predictor: Option<Predictor>,
    width: usize,
    height: usize,
    bits_per_sample: &[u16],
    planar_configuration: PlanarConfiguration,
) -> Result<(), GeotiffError> {
    let predictor = match predictor {
        None | Some(Predictor::None) => return Ok(()),
        Some(p) => p,
    };

    for &b in bits_per_sample {
        if !b.is_multiple_of(8) {
            return Err(GeotiffError::InvalidRasterDimensions(format!(
                "applyPredictor only supports multiples of 8 bits, got {b}"
            )));
        }
        if bits_per_sample.first() != Some(&b) {
            return Err(GeotiffError::InvalidRasterDimensions(
                "applyPredictor requires all samples to have the same size".to_string(),
            ));
        }
    }

    let Some(&first_bits) = bits_per_sample.first() else {
        return Err(GeotiffError::InvalidRasterDimensions(
            "applyPredictor requires at least one sample".to_string(),
        ));
    };

    let bytes_per_sample = (first_bits / 8) as usize;
    let stride = if planar_configuration == PlanarConfiguration::Planar {
        1
    } else {
        bits_per_sample.len()
    };

    for i in 0..height {
        let row_start = i
            .checked_mul(stride)
            .and_then(|value| value.checked_mul(width))
            .and_then(|value| value.checked_mul(bytes_per_sample))
            .ok_or_else(|| {
                GeotiffError::InvalidRasterDimensions("predictor row offset overflow".to_string())
            })?;
        if row_start >= block.len() {
            break;
        }
        let row_len = stride
            .checked_mul(width)
            .and_then(|value| value.checked_mul(bytes_per_sample))
            .ok_or_else(|| {
                GeotiffError::InvalidRasterDimensions("predictor row length overflow".to_string())
            })?;
        let row_end = row_start.checked_add(row_len).ok_or_else(|| {
            GeotiffError::InvalidRasterDimensions("predictor row range overflow".to_string())
        })?;
        let row = block.get_mut(row_start..row_end).ok_or_else(|| {
            GeotiffError::InvalidRasterDimensions(format!(
                "decoded predictor block is truncated at row {i}"
            ))
        })?;

        match predictor {
            Predictor::Horizontal => match first_bits {
                8 => {
                    let mut typed = TypedArray::Uint8(row.to_vec());
                    decode_row_acc(&mut typed, stride)?;
                    if let TypedArray::Uint8(v) = typed {
                        row.copy_from_slice(&v);
                    }
                }
                16 => {
                    let mut vals: Vec<u16> = row
                        .chunks_exact(2)
                        .map(|c| u16::from_ne_bytes([c[0], c[1]]))
                        .collect();
                    decode_row_acc_impl(&mut vals, stride, u16::wrapping_add);
                    for (i, v) in vals.iter().enumerate() {
                        row[i * 2..i * 2 + 2].copy_from_slice(&v.to_ne_bytes());
                    }
                }
                32 => {
                    let mut vals: Vec<u32> = row
                        .chunks_exact(4)
                        .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                        .collect();
                    decode_row_acc_impl(&mut vals, stride, u32::wrapping_add);
                    for (i, v) in vals.iter().enumerate() {
                        row[i * 4..i * 4 + 4].copy_from_slice(&v.to_ne_bytes());
                    }
                }
                other => {
                    return Err(GeotiffError::InvalidRasterDimensions(format!(
                        "Predictor 2 not allowed with {other} bits per sample."
                    )));
                }
            },
            Predictor::FloatingPoint => {
                decode_row_floating_point(row, stride, bytes_per_sample)?;
            }
            // Any other/unknown predictor tag value: JS leaves `row` unassigned and does
            // nothing for this row (falls through both `if`/`else if` branches) - same here.
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_row_acc_undoes_horizontal_differencing() {
        // 3 pixels, stride 1: encoded deltas [10, 5, 5] -> decoded [10, 15, 20]
        let mut row = TypedArray::Uint8(vec![10, 5, 5]);
        decode_row_acc(&mut row, 1).unwrap();
        assert_eq!(row, TypedArray::Uint8(vec![10, 15, 20]));
    }

    #[test]
    fn decode_row_acc_wraps_like_a_js_typed_array() {
        let mut row = TypedArray::Uint8(vec![200, 100]);
        decode_row_acc(&mut row, 1).unwrap();
        // 200 + 100 = 300, wraps mod 256 = 44
        assert_eq!(row, TypedArray::Uint8(vec![200, 44]));
    }

    #[test]
    fn decode_row_acc_respects_stride_for_multi_sample_pixels() {
        // 2 samples per pixel (stride 2), 2 pixels: deltas per-channel
        let mut row = TypedArray::Uint16(vec![10, 20, 5, 5]);
        decode_row_acc(&mut row, 2).unwrap();
        assert_eq!(row, TypedArray::Uint16(vec![10, 20, 15, 25]));
    }

    // Both expected outputs below were captured by literally running
    // predictor.js's own applyPredictor/decodeRowAcc/decodeRowFloatingPoint
    // in Node on this exact input, not derived from this Rust port's logic.

    #[test]
    fn apply_predictor_horizontal_matches_a_real_js_run() {
        // 2 rows, width=3, 2 samples/pixel, 8-bit
        let mut block = vec![10, 20, 5, 5, 5, 5, 1, 2, 1, 1, 1, 1];
        apply_predictor(
            &mut block,
            Some(Predictor::Horizontal),
            3,
            2,
            &[8, 8],
            PlanarConfiguration::Chunky,
        )
        .unwrap();
        assert_eq!(block, vec![10, 20, 15, 25, 20, 30, 1, 2, 2, 3, 3, 4]);
    }

    #[test]
    fn apply_predictor_floating_point_matches_a_real_js_run() {
        // 1 row, width=4, 1 sample/pixel, 32-bit float
        let mut block = vec![10, 5, 5, 5, 1, 1, 1, 1, 0, 0, 0, 0, 2, 3, 1, 0];
        apply_predictor(
            &mut block,
            Some(Predictor::FloatingPoint),
            4,
            1,
            &[32],
            PlanarConfiguration::Chunky,
        )
        .unwrap();
        assert_eq!(
            block,
            vec![
                31, 29, 26, 10, 34, 29, 27, 15, 35, 29, 28, 20, 35, 29, 29, 25
            ]
        );
    }

    #[test]
    fn apply_predictor_none_is_a_no_op() {
        let mut block = vec![1, 2, 3, 4];
        let original = block.clone();
        apply_predictor(&mut block, None, 2, 2, &[8], PlanarConfiguration::Chunky).unwrap();
        assert_eq!(block, original);
        apply_predictor(
            &mut block,
            Some(Predictor::None),
            2,
            2,
            &[8],
            PlanarConfiguration::Chunky,
        )
        .unwrap();
        assert_eq!(block, original);
    }
}
