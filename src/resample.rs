//! Port of `resample.js`: `copyNewSize`/`lerp` (wave 1) plus the resampling
//! functions that depend on them - `resampleNearest`/`resampleBilinear`/
//! `resample` (per-band arrays) and their `*Interleaved` counterparts
//! (single band-interleaved array). Wired into `raster.rs`'s output when
//! `readRasters`'s `width`/`height`/`resampleMethod` options are ported.

use crate::error::GeotiffError;
use crate::typed_array::TypedArray;

/// `copyNewSize(array, width, height, samplesPerPixel = 1)` - JS builds a
/// new empty array via `Object.getPrototypeOf(array).constructor(len)`
/// (same concrete typed-array class as the input); `TypedArray::new_zeroed`
/// is the direct Rust equivalent since `TypedArray` already models "which
/// concrete numeric class" the way that runtime reflection did.
pub fn copy_new_size(
    array: &TypedArray,
    width: usize,
    height: usize,
    samples_per_pixel: usize,
) -> Result<TypedArray, GeotiffError> {
    let len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(samples_per_pixel))
        .ok_or_else(|| {
            GeotiffError::InvalidRasterDimensions("output element count overflow".to_string())
        })?;
    array
        .try_new_zeroed(len)
        .map_err(|error| GeotiffError::RasterAllocationFailed(error.to_string()))
}

fn checked_pixels(width: usize, height: usize, context: &str) -> Result<usize, GeotiffError> {
    width.checked_mul(height).ok_or_else(|| {
        GeotiffError::InvalidRasterDimensions(format!("{context} pixel count overflow"))
    })
}

fn validate_band_input(
    value_arrays: &[TypedArray],
    in_width: usize,
    in_height: usize,
    out_width: usize,
    out_height: usize,
) -> Result<(), GeotiffError> {
    let input_pixels = checked_pixels(in_width, in_height, "input")?;
    let output_pixels = checked_pixels(out_width, out_height, "output")?;
    if output_pixels > 0 && input_pixels == 0 {
        return Err(GeotiffError::InvalidRasterDimensions(
            "cannot resample an empty input to a non-empty output".to_string(),
        ));
    }
    for (band, array) in value_arrays.iter().enumerate() {
        if array.len() < input_pixels {
            return Err(GeotiffError::InvalidRasterDimensions(format!(
                "band {band} contains {} values, expected at least {input_pixels}",
                array.len()
            )));
        }
    }
    Ok(())
}

/// `lerp(v0, v1, t)`
pub fn lerp(v0: f64, v1: f64, t: f64) -> f64 {
    ((1.0 - t) * v0) + (t * v1)
}

/// `resampleNearest(valueArrays, inWidth, inHeight, outWidth, outHeight)` -
/// one array per band.
pub fn resample_nearest(
    value_arrays: &[TypedArray],
    in_width: usize,
    in_height: usize,
    out_width: usize,
    out_height: usize,
) -> Result<Vec<TypedArray>, GeotiffError> {
    validate_band_input(value_arrays, in_width, in_height, out_width, out_height)?;
    let rel_x = in_width as f64 / out_width as f64;
    let rel_y = in_height as f64 / out_height as f64;
    value_arrays
        .iter()
        .map(|array| {
            let mut new_array = copy_new_size(array, out_width, out_height, 1)?;
            for y in 0..out_height {
                let cy = ((rel_y * y as f64).round() as usize).min(in_height - 1);
                for x in 0..out_width {
                    let cx = ((rel_x * x as f64).round() as usize).min(in_width - 1);
                    new_array.copy_value_from((y * out_width) + x, array, (cy * in_width) + cx);
                }
            }
            Ok(new_array)
        })
        .collect()
}

/// `resampleBilinear(valueArrays, inWidth, inHeight, outWidth, outHeight)`
pub fn resample_bilinear(
    value_arrays: &[TypedArray],
    in_width: usize,
    in_height: usize,
    out_width: usize,
    out_height: usize,
) -> Result<Vec<TypedArray>, GeotiffError> {
    validate_band_input(value_arrays, in_width, in_height, out_width, out_height)?;
    let rel_x = in_width as f64 / out_width as f64;
    let rel_y = in_height as f64 / out_height as f64;
    value_arrays
        .iter()
        .map(|array| {
            let mut new_array = copy_new_size(array, out_width, out_height, 1)?;
            for y in 0..out_height {
                let raw_y = rel_y * y as f64;
                let yl = raw_y.floor() as usize;
                let yh = (raw_y.ceil() as usize).min(in_height - 1);
                for x in 0..out_width {
                    let raw_x = rel_x * x as f64;
                    let tx = raw_x.fract();
                    let xl = raw_x.floor() as usize;
                    let xh = (raw_x.ceil() as usize).min(in_width - 1);

                    let ll = array.get_f64((yl * in_width) + xl);
                    let hl = array.get_f64((yl * in_width) + xh);
                    let lh = array.get_f64((yh * in_width) + xl);
                    let hh = array.get_f64((yh * in_width) + xh);

                    let value = lerp(lerp(ll, hl, tx), lerp(lh, hh, tx), raw_y.fract());
                    new_array.set_f64((y * out_width) + x, value);
                }
            }
            Ok(new_array)
        })
        .collect()
}

/// `resample(valueArrays, inWidth, inHeight, outWidth, outHeight, method)`.
/// JS defaults `method` to `'nearest'`; Rust has no default parameters, so
/// callers supply it explicitly (the future `readRasters` options layer
/// will default it the same way JS does).
pub fn resample(
    value_arrays: &[TypedArray],
    in_width: usize,
    in_height: usize,
    out_width: usize,
    out_height: usize,
    method: &str,
) -> Result<Vec<TypedArray>, GeotiffError> {
    match method.to_lowercase().as_str() {
        "nearest" => resample_nearest(value_arrays, in_width, in_height, out_width, out_height),
        "bilinear" | "linear" => {
            resample_bilinear(value_arrays, in_width, in_height, out_width, out_height)
        }
        _ => Err(GeotiffError::UnsupportedResampleMethod(method.to_string())),
    }
}

/// `resampleNearestInterleaved(valueArray, inWidth, inHeight, outWidth, outHeight, samples)`
pub fn resample_nearest_interleaved(
    value_array: &TypedArray,
    in_width: usize,
    in_height: usize,
    out_width: usize,
    out_height: usize,
    samples: usize,
) -> Result<TypedArray, GeotiffError> {
    validate_interleaved_input(
        value_array,
        in_width,
        in_height,
        out_width,
        out_height,
        samples,
    )?;
    let rel_x = in_width as f64 / out_width as f64;
    let rel_y = in_height as f64 / out_height as f64;
    let mut new_array = copy_new_size(value_array, out_width, out_height, samples)?;
    for y in 0..out_height {
        let cy = ((rel_y * y as f64).round() as usize).min(in_height - 1);
        for x in 0..out_width {
            let cx = ((rel_x * x as f64).round() as usize).min(in_width - 1);
            for i in 0..samples {
                new_array.copy_value_from(
                    (y * out_width * samples) + (x * samples) + i,
                    value_array,
                    (cy * in_width * samples) + (cx * samples) + i,
                );
            }
        }
    }
    Ok(new_array)
}

fn validate_interleaved_input(
    value_array: &TypedArray,
    in_width: usize,
    in_height: usize,
    out_width: usize,
    out_height: usize,
    samples: usize,
) -> Result<(), GeotiffError> {
    let input_pixels = checked_pixels(in_width, in_height, "input")?;
    let output_pixels = checked_pixels(out_width, out_height, "output")?;
    let expected = input_pixels.checked_mul(samples).ok_or_else(|| {
        GeotiffError::InvalidRasterDimensions("input sample count overflow".to_string())
    })?;
    output_pixels.checked_mul(samples).ok_or_else(|| {
        GeotiffError::InvalidRasterDimensions("output sample count overflow".to_string())
    })?;
    if output_pixels > 0 && input_pixels == 0 {
        return Err(GeotiffError::InvalidRasterDimensions(
            "cannot resample an empty input to a non-empty output".to_string(),
        ));
    }
    if value_array.len() < expected {
        return Err(GeotiffError::InvalidRasterDimensions(format!(
            "interleaved input contains {} values, expected at least {expected}",
            value_array.len()
        )));
    }
    Ok(())
}

/// `resampleBilinearInterleaved(valueArray, inWidth, inHeight, outWidth, outHeight, samples)`
pub fn resample_bilinear_interleaved(
    value_array: &TypedArray,
    in_width: usize,
    in_height: usize,
    out_width: usize,
    out_height: usize,
    samples: usize,
) -> Result<TypedArray, GeotiffError> {
    validate_interleaved_input(
        value_array,
        in_width,
        in_height,
        out_width,
        out_height,
        samples,
    )?;
    let rel_x = in_width as f64 / out_width as f64;
    let rel_y = in_height as f64 / out_height as f64;
    let mut new_array = copy_new_size(value_array, out_width, out_height, samples)?;
    for y in 0..out_height {
        let raw_y = rel_y * y as f64;
        let yl = raw_y.floor() as usize;
        let yh = (raw_y.ceil() as usize).min(in_height - 1);
        for x in 0..out_width {
            let raw_x = rel_x * x as f64;
            let tx = raw_x.fract();
            let xl = raw_x.floor() as usize;
            let xh = (raw_x.ceil() as usize).min(in_width - 1);
            for i in 0..samples {
                let ll = value_array.get_f64((yl * in_width * samples) + (xl * samples) + i);
                let hl = value_array.get_f64((yl * in_width * samples) + (xh * samples) + i);
                let lh = value_array.get_f64((yh * in_width * samples) + (xl * samples) + i);
                let hh = value_array.get_f64((yh * in_width * samples) + (xh * samples) + i);

                let value = lerp(lerp(ll, hl, tx), lerp(lh, hh, tx), raw_y.fract());
                new_array.set_f64((y * out_width * samples) + (x * samples) + i, value);
            }
        }
    }
    Ok(new_array)
}

/// `resampleInterleaved(valueArray, inWidth, inHeight, outWidth, outHeight, samples, method)`
pub fn resample_interleaved(
    value_array: &TypedArray,
    in_width: usize,
    in_height: usize,
    out_width: usize,
    out_height: usize,
    samples: usize,
    method: &str,
) -> Result<TypedArray, GeotiffError> {
    match method.to_lowercase().as_str() {
        "nearest" => resample_nearest_interleaved(
            value_array,
            in_width,
            in_height,
            out_width,
            out_height,
            samples,
        ),
        "bilinear" | "linear" => resample_bilinear_interleaved(
            value_array,
            in_width,
            in_height,
            out_width,
            out_height,
            samples,
        ),
        _ => Err(GeotiffError::UnsupportedResampleMethod(method.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_new_size_preserves_variant_and_sizes_by_pixels() {
        let src = TypedArray::Uint8(vec![1, 2, 3, 4]);
        let out = copy_new_size(&src, 3, 2, 2).unwrap();
        assert_eq!(out, TypedArray::Uint8(vec![0; 12]));
    }

    #[test]
    fn lerp_interpolates_linearly() {
        assert_eq!(lerp(0.0, 10.0, 0.0), 0.0);
        assert_eq!(lerp(0.0, 10.0, 1.0), 10.0);
        assert_eq!(lerp(0.0, 10.0, 0.5), 5.0);
    }

    // Expected outputs cross-checked against a literal run of resample.js's
    // own resampleNearest/resampleBilinear in Node on this exact input.
    fn src_2x2() -> TypedArray {
        TypedArray::Float64(vec![1.0, 2.0, 3.0, 4.0])
    }

    #[test]
    fn resample_nearest_matches_a_real_js_run() {
        let out = resample_nearest(&[src_2x2()], 2, 2, 4, 4).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0],
            TypedArray::Float64(vec![
                1.0, 2.0, 2.0, 2.0, //
                3.0, 4.0, 4.0, 4.0, //
                3.0, 4.0, 4.0, 4.0, //
                3.0, 4.0, 4.0, 4.0,
            ])
        );
    }

    #[test]
    fn resample_bilinear_matches_a_real_js_run() {
        let out = resample_bilinear(&[src_2x2()], 2, 2, 4, 4).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0],
            TypedArray::Float64(vec![
                1.0, 1.5, 2.0, 2.0, //
                2.0, 2.5, 3.0, 3.0, //
                3.0, 3.5, 4.0, 4.0, //
                3.0, 3.5, 4.0, 4.0,
            ])
        );
    }

    #[test]
    fn resample_dispatches_by_method_name_case_insensitively() {
        let a = resample(&[src_2x2()], 2, 2, 4, 4, "Nearest").unwrap();
        let b = resample_nearest(&[src_2x2()], 2, 2, 4, 4).unwrap();
        assert_eq!(a, b);

        let c = resample(&[src_2x2()], 2, 2, 4, 4, "linear").unwrap();
        let d = resample_bilinear(&[src_2x2()], 2, 2, 4, 4).unwrap();
        assert_eq!(c, d);
    }

    #[test]
    fn resample_rejects_unknown_methods() {
        let err = resample(&[src_2x2()], 2, 2, 4, 4, "cubic").unwrap_err();
        assert_eq!(
            err,
            GeotiffError::UnsupportedResampleMethod("cubic".to_string())
        );
    }

    #[test]
    fn interleaved_nearest_matches_the_per_band_result_for_a_single_band() {
        let interleaved = resample_nearest_interleaved(&src_2x2(), 2, 2, 4, 4, 1).unwrap();
        let per_band = resample_nearest(&[src_2x2()], 2, 2, 4, 4).unwrap();
        assert_eq!(interleaved, per_band[0]);
    }

    #[test]
    fn interleaved_resample_keeps_samples_together_per_pixel() {
        // 1x1 image, 2 samples per pixel -> upsampled to 2x1: both output pixels
        // must read the same (only) source pixel's two samples, in order.
        let src = TypedArray::Uint8(vec![10, 20]);
        let out = resample_nearest_interleaved(&src, 1, 1, 2, 1, 2).unwrap();
        assert_eq!(out, TypedArray::Uint8(vec![10, 20, 10, 20]));
    }

    #[test]
    fn nearest_resampling_preserves_64_bit_integer_values_exactly() {
        let values = TypedArray::Uint64(vec![9_007_199_254_740_993]);
        let bands = resample_nearest(std::slice::from_ref(&values), 1, 1, 2, 2).unwrap();
        assert_eq!(
            bands,
            vec![TypedArray::Uint64(vec![9_007_199_254_740_993; 4])]
        );

        let interleaved = resample_nearest_interleaved(&values, 1, 1, 2, 2, 1).unwrap();
        assert_eq!(
            interleaved,
            TypedArray::Uint64(vec![9_007_199_254_740_993; 4])
        );
    }

    #[test]
    fn invalid_and_overflowing_dimensions_return_errors_instead_of_panicking() {
        assert!(resample_nearest(&[TypedArray::Uint8(vec![])], 0, 0, 1, 1).is_err());
        assert!(copy_new_size(&TypedArray::Uint8(vec![]), usize::MAX, 2, 1).is_err());
        assert!(
            resample_nearest_interleaved(&TypedArray::Uint8(vec![1]), usize::MAX, 2, 1, 1, 1,)
                .is_err()
        );
    }
}
