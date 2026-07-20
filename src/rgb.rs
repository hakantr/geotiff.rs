//! Port of `rgb.ts`. The original reads `{ width, height }` off a
//! `TypedArrayWithDimensions` (JS array with extra properties glued on)
//! purely to size the output buffer; that size is always exactly derivable
//! from the input length and the known channel count of each function
//! (`raster.length` for single-channel inputs, `/4` for CMYK, `/3` for
//! YCbCr/CIELab - each matches `width * height` given how the original
//! always calls these), so this port takes just the raster and computes
//! sizes directly rather than threading width/height through - same
//! result, no JS-specific property-bag type to invent in Rust.
//!
//! JS assignment into a typed array truncates the stored value to that
//! array's element type using ECMAScript's `ToUint8`/`ToUint8Clamp`, which
//! differ (`Uint8Array` truncates-toward-zero then wraps mod 256;
//! `Uint8ClampedArray` rounds-half-to-even and clamps to \[0, 255\]) -
//! `to_uint8`/`to_uint8_clamp` below reproduce each precisely rather than
//! using a single `as u8` cast for both, which would only match one of them.

use crate::typed_array::TypedArray;

fn to_uint8(v: f64) -> u8 {
    if !v.is_finite() {
        return 0;
    }
    let truncated = v.trunc() as i64;
    truncated.rem_euclid(256) as u8
}

fn to_uint8_clamp(v: f64) -> u8 {
    if v.is_nan() {
        return 0;
    }
    if v <= 0.0 {
        return 0;
    }
    if v >= 255.0 {
        return 255;
    }
    let floor = v.floor();
    let diff = v - floor;
    let rounded = if diff < 0.5 {
        floor
    } else if diff > 0.5 {
        floor + 1.0
    } else if (floor as i64) % 2 == 0 {
        floor
    } else {
        floor + 1.0
    };
    rounded as u8
}

/// `fromWhiteIsZero(raster, max): Uint8Array`
pub fn from_white_is_zero(raster: &TypedArray, max: f64) -> Vec<u8> {
    let mut rgb = vec![0u8; raster.len() * 3];
    for i in 0..raster.len() {
        let value = 256.0 - (raster.get_f64(i) / max) * 256.0;
        let v = to_uint8(value);
        rgb[i * 3] = v;
        rgb[i * 3 + 1] = v;
        rgb[i * 3 + 2] = v;
    }
    rgb
}

/// `fromBlackIsZero(raster, max): Uint8Array`
pub fn from_black_is_zero(raster: &TypedArray, max: f64) -> Vec<u8> {
    let mut rgb = vec![0u8; raster.len() * 3];
    for i in 0..raster.len() {
        let value = (raster.get_f64(i) / max) * 256.0;
        let v = to_uint8(value);
        rgb[i * 3] = v;
        rgb[i * 3 + 1] = v;
        rgb[i * 3 + 2] = v;
    }
    rgb
}

/// `fromPalette(raster, colorMap): Uint8Array`
pub fn from_palette(raster: &TypedArray, color_map: &[u16]) -> Vec<u8> {
    let mut rgb = vec![0u8; raster.len() * 3];
    let green_offset = color_map.len() / 3;
    let blue_offset = (color_map.len() / 3) * 2;
    for i in 0..raster.len() {
        let index = raster.get_f64(i);
        let map_index = (index.is_finite()
            && index >= 0.0
            && index.fract() == 0.0
            && index <= usize::MAX as f64)
            .then_some(index as usize);
        let lookup = |channel_offset: usize| {
            map_index
                .and_then(|value| value.checked_add(channel_offset))
                .and_then(|value| color_map.get(value))
                .copied()
                .map(f64::from)
                .unwrap_or(f64::NAN)
        };
        // Out-of-range JavaScript TypedArray indexing yields `undefined`;
        // arithmetic turns that into NaN and Uint8 assignment stores zero.
        // Optional lookup reproduces that result without a Rust panic.
        rgb[i * 3] = to_uint8((lookup(0) / 65536.0) * 256.0);
        rgb[i * 3 + 1] = to_uint8((lookup(green_offset) / 65536.0) * 256.0);
        rgb[i * 3 + 2] = to_uint8((lookup(blue_offset) / 65536.0) * 256.0);
    }
    rgb
}

/// `fromCMYK(cmykRaster): Uint8Array`
pub fn from_cmyk(cmyk_raster: &TypedArray) -> Vec<u8> {
    let pixels = cmyk_raster.len() / 4;
    let mut rgb = vec![0u8; pixels * 3];
    for p in 0..pixels {
        let i = p * 4;
        let j = p * 3;
        let c = cmyk_raster.get_f64(i);
        let m = cmyk_raster.get_f64(i + 1);
        let y = cmyk_raster.get_f64(i + 2);
        let k = cmyk_raster.get_f64(i + 3);

        rgb[j] = to_uint8(255.0 * ((255.0 - c) / 256.0) * ((255.0 - k) / 256.0));
        rgb[j + 1] = to_uint8(255.0 * ((255.0 - m) / 256.0) * ((255.0 - k) / 256.0));
        rgb[j + 2] = to_uint8(255.0 * ((255.0 - y) / 256.0) * ((255.0 - k) / 256.0));
    }
    rgb
}

/// `fromYCbCr(yCbCrRaster): Uint8ClampedArray`
pub fn from_y_cb_cr(y_cb_cr_raster: &TypedArray) -> Vec<u8> {
    let pixels = y_cb_cr_raster.len() / 3;
    let mut rgb = vec![0u8; pixels * 3];
    for p in 0..pixels {
        let i = p * 3;
        let j = p * 3;
        let y = y_cb_cr_raster.get_f64(i);
        let cb = y_cb_cr_raster.get_f64(i + 1);
        let cr = y_cb_cr_raster.get_f64(i + 2);

        rgb[j] = to_uint8_clamp(y + 1.402 * (cr - 0x80 as f64));
        rgb[j + 1] =
            to_uint8_clamp(y - 0.34414 * (cb - 0x80 as f64) - 0.71414 * (cr - 0x80 as f64));
        rgb[j + 2] = to_uint8_clamp(y + 1.772 * (cb - 0x80 as f64));
    }
    rgb
}

const XN: f64 = 0.95047;
const YN: f64 = 1.0;
const ZN: f64 = 1.08883;

/// `fromCIELab(cieLabRaster): Uint8Array`
pub fn from_cie_lab(cie_lab_raster: &TypedArray) -> Vec<u8> {
    let pixels = cie_lab_raster.len() / 3;
    let mut rgb = vec![0u8; pixels * 3];
    for p in 0..pixels {
        let i = p * 3;
        let j = p * 3;
        let l = cie_lab_raster.get_f64(i);
        // `(cieLabRaster[i+1] << 24) >> 24` in JS reinterprets a uint8 as an
        // int8 via sign-extending bit shifts; casting the byte to `i8` is
        // Rust's direct native equivalent of that same reinterpretation.
        let a_ = (cie_lab_raster.get_f64(i + 1) as u8) as i8 as f64;
        let b_ = (cie_lab_raster.get_f64(i + 2) as u8) as i8 as f64;

        let mut y = (l + 16.0) / 116.0;
        let mut x = a_ / 500.0 + y;
        let mut z = y - b_ / 200.0;

        x = XN
            * (if x * x * x > 0.008856 {
                x * x * x
            } else {
                (x - 16.0 / 116.0) / 7.787
            });
        y = YN
            * (if y * y * y > 0.008856 {
                y * y * y
            } else {
                (y - 16.0 / 116.0) / 7.787
            });
        z = ZN
            * (if z * z * z > 0.008856 {
                z * z * z
            } else {
                (z - 16.0 / 116.0) / 7.787
            });

        let mut r = x * 3.2406 + y * -1.5372 + z * -0.4986;
        let mut g = x * -0.9689 + y * 1.8758 + z * 0.0415;
        let mut b = x * 0.0557 + y * -0.204 + z * 1.057;

        r = if r > 0.0031308 {
            1.055 * r.powf(1.0 / 2.4) - 0.055
        } else {
            12.92 * r
        };
        g = if g > 0.0031308 {
            1.055 * g.powf(1.0 / 2.4) - 0.055
        } else {
            12.92 * g
        };
        b = if b > 0.0031308 {
            1.055 * b.powf(1.0 / 2.4) - 0.055
        } else {
            12.92 * b
        };

        rgb[j] = to_uint8(r.clamp(0.0, 1.0) * 255.0);
        rgb[j + 1] = to_uint8(g.clamp(0.0, 1.0) * 255.0);
        rgb[j + 2] = to_uint8(b.clamp(0.0, 1.0) * 255.0);
    }
    rgb
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_uint8_truncates_and_wraps_like_ecmascript_touint8() {
        assert_eq!(to_uint8(255.9), 255);
        assert_eq!(to_uint8(256.0), 0);
        assert_eq!(to_uint8(-1.0), 255);
    }

    #[test]
    fn to_uint8_clamp_clamps_instead_of_wrapping() {
        assert_eq!(to_uint8_clamp(-5.0), 0);
        assert_eq!(to_uint8_clamp(300.0), 255);
        assert_eq!(to_uint8_clamp(127.4), 127);
        assert_eq!(to_uint8_clamp(127.6), 128);
    }

    #[test]
    fn from_black_is_zero_maps_zero_to_black_and_scales_up() {
        let raster = TypedArray::Uint8(vec![0, 127]);
        let rgb = from_black_is_zero(&raster, 255.0);
        assert_eq!(&rgb[0..3], &[0, 0, 0]);
        assert_eq!(&rgb[3..6], &[127, 127, 127]);
    }

    #[test]
    fn from_black_is_zero_wraps_at_the_maximum_input_like_the_original() {
        // (raster[i] / max) * 256 hits exactly 256 when raster[i] == max,
        // and JS `Uint8Array` assignment truncates+wraps mod 256 (not
        // clamps) - so max input wraps to 0, matching the original exactly
        // even though it looks like an off-by-one at first glance.
        let raster = TypedArray::Uint8(vec![255]);
        let rgb = from_black_is_zero(&raster, 255.0);
        assert_eq!(&rgb[0..3], &[0, 0, 0]);
    }

    #[test]
    fn from_white_is_zero_ramps_down_as_input_increases() {
        // value = 256 - (x/max)*256; at x=1 this is just under 256 (truncates
        // to 254), well clear of the x=0/x=max wraparound edges below.
        let raster = TypedArray::Uint8(vec![1, 127]);
        let rgb = from_white_is_zero(&raster, 255.0);
        assert_eq!(&rgb[0..3], &[254, 254, 254]);
        assert_eq!(&rgb[3..6], &[128, 128, 128]);
    }

    #[test]
    fn from_white_is_zero_wraps_at_both_ends_like_the_original() {
        // 256 - (x/max)*256 hits exactly 256 at x=0 (wraps to 0, not 255 as
        // the "white is zero" name might suggest) and exactly 0 at x=max -
        // both ends wrap/land on 0 due to the same *256-not-*255 scaling
        // quirk as `from_black_is_zero`'s max-input case. Preserved exactly
        // rather than "corrected", per the port's fidelity rule.
        let raster = TypedArray::Uint8(vec![0, 255]);
        let rgb = from_white_is_zero(&raster, 255.0);
        assert_eq!(&rgb[0..3], &[0, 0, 0]);
        assert_eq!(&rgb[3..6], &[0, 0, 0]);
    }

    #[test]
    fn from_cmyk_full_black_gives_black_rgb() {
        let raster = TypedArray::Uint8(vec![0, 0, 0, 255]);
        let rgb = from_cmyk(&raster);
        assert_eq!(rgb, vec![0, 0, 0]);
    }

    #[test]
    fn palette_out_of_range_indices_match_javascript_zero_without_panicking() {
        let raster = TypedArray::Float64(vec![-1.0, 1.5, 99.0, f64::NAN]);
        let rgb = from_palette(&raster, &[0, 65_535, 0, 65_535, 0, 65_535]);
        assert_eq!(rgb, vec![0; 12]);
    }
}
