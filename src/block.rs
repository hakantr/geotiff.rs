//! Lossless block decoding shared by tiled and stripped TIFF reads.
//!
//! `async-tiff::Tile::decode` is useful for the common uniform, byte-aligned
//! case, but it deliberately rejects TIFF sample widths such as 2/4/10/12/
//! 14 bits and applies predictors as though planar data were chunky.  Those
//! are valid inputs supported by geotiff.js, so the public raster path uses
//! this adapter instead: codecs still come from `DecoderRegistry`, while
//! predictor reversal, row-padding-aware bit unpacking, per-sample typing,
//! empty-block handling, and planar layout are controlled here.

use crate::decode_pool::{CancellationToken, cancellable, check_cancelled, spawn_decode};
use crate::predictor::apply_predictor;
use crate::typed_array::TypedArray;
use async_tiff::decoder::DecoderRegistry;
use async_tiff::error::{AsyncTiffError, AsyncTiffResult};
use async_tiff::reader::{AsyncFileReader, Endianness};
use async_tiff::tags::{PlanarConfiguration, Predictor, SampleFormat};
use async_tiff::{CompressedBytes, ImageFileDirectory};
use bytes::Bytes;
use half::f16;
use moka::future::Cache;
use std::sync::Arc;

/// A decoded TIFF tile/strip represented as one correctly typed array per
/// sample. Keeping bands separate internally makes mixed sample types and
/// planar storage unambiguous; callers can interleave them into the output
/// type selected by geotiff.js semantics.
#[derive(Debug, Clone)]
pub(crate) struct DecodedBlock {
    pub(crate) bands: Vec<TypedArray>,
    pub(crate) width: usize,
    pub(crate) height: usize,
    javascript_compatibility: Option<JavaScriptCompatibilityBlock>,
}

/// geotiff.js normalizes packed samples into a native typed-array buffer and
/// then reads that buffer again using the original (possibly fractional)
/// sample byte offsets.  That second step is observably lossy for chunky
/// multi-sample data such as 12/12/12-bit RGB.  Keep the exact intermediate
/// buffer only for layouts where it can differ, so callers may explicitly
/// request legacy JavaScript behavior without weakening the lossless path.
#[derive(Debug, Clone)]
struct JavaScriptCompatibilityBlock {
    data: JavaScriptBlockData,
    formats: Vec<SampleFormat>,
    bits: Vec<u16>,
}

#[derive(Debug, Clone)]
enum JavaScriptBlockData {
    Chunky(Vec<u8>),
    ChunkyBySample(Vec<Vec<u8>>),
    Planar(Vec<Vec<u8>>),
    Error(String),
}

/// Per-image decoded block cache corresponding to geotiff.js's
/// `new GeoTIFF(..., { cache: true })` tile-promise cache.  Values are
/// shared so cache hits do not clone potentially large sample planes, and
/// `try_get_with` coalesces concurrent reads of the same block.
#[derive(Debug)]
pub(crate) struct DecodedBlockCache {
    blocks: Cache<(usize, usize), Arc<DecodedBlock>>,
}

impl DecodedBlockCache {
    pub(crate) fn new() -> Self {
        // A TIFF has a finite IFD-declared block set.  The JavaScript cache
        // retains that complete set for the lifetime of GeoTIFFImage, so an
        // entry-count eviction limit here would be a behavioral mismatch.
        Self {
            blocks: Cache::new(u64::MAX),
        }
    }
}

impl DecodedBlock {
    pub(crate) fn value(&self, sample: usize, x: usize, y: usize) -> f64 {
        debug_assert!(x < self.width && y < self.height);
        self.bands[sample].get_f64(y * self.width + x)
    }

    pub(crate) fn javascript_compatible_value(
        &self,
        sample: usize,
        x: usize,
        y: usize,
        endianness: Endianness,
        planar_bytes_per_pixel: Option<usize>,
    ) -> AsyncTiffResult<f64> {
        debug_assert!(x < self.width && y < self.height);
        let Some(compatibility) = &self.javascript_compatibility else {
            return Ok(self.value(sample, x, y));
        };
        let bits = *compatibility
            .bits
            .get(sample)
            .ok_or_else(|| AsyncTiffError::General(format!("Invalid sample index '{sample}'.")))?;
        let format = *compatibility
            .formats
            .get(sample)
            .ok_or_else(|| AsyncTiffError::General(format!("Invalid sample index '{sample}'.")))?;
        let pixel = y
            .checked_mul(self.width)
            .and_then(|value| value.checked_add(x))
            .ok_or_else(|| {
                AsyncTiffError::General("JavaScript-compatible pixel offset overflow".to_string())
            })?;

        let (data, bytes_per_pixel, sample_offset) = match &compatibility.data {
            JavaScriptBlockData::Chunky(data) => {
                let bytes_per_pixel = compatibility
                    .bits
                    .iter()
                    .try_fold(0usize, |total, value| {
                        total.checked_add(usize::from(*value).div_ceil(8))
                    })
                    .ok_or_else(|| {
                        AsyncTiffError::General(
                            "JavaScript-compatible pixel byte count overflow".to_string(),
                        )
                    })?;
                // DataView's ToIndex conversion truncates this positive
                // fractional offset. This integer division reproduces
                // `sum(bitsPerSample, 0, sample) / 8` at the call boundary.
                let sample_offset = compatibility.bits[..sample]
                    .iter()
                    .try_fold(0usize, |total, value| {
                        total.checked_add(usize::from(*value))
                    })
                    .ok_or_else(|| {
                        AsyncTiffError::General(
                            "JavaScript-compatible sample bit offset overflow".to_string(),
                        )
                    })?
                    / 8;
                (data.as_slice(), bytes_per_pixel, sample_offset)
            }
            JavaScriptBlockData::ChunkyBySample(data) => {
                let data = data.get(sample).ok_or_else(|| {
                    AsyncTiffError::General(format!("Invalid sample index '{sample}'."))
                })?;
                let bytes_per_pixel = compatibility
                    .bits
                    .iter()
                    .try_fold(0usize, |total, value| {
                        total.checked_add(usize::from(*value).div_ceil(8))
                    })
                    .ok_or_else(|| {
                        AsyncTiffError::General(
                            "JavaScript-compatible pixel byte count overflow".to_string(),
                        )
                    })?;
                let sample_offset = compatibility.bits[..sample]
                    .iter()
                    .try_fold(0usize, |total, value| {
                        total.checked_add(usize::from(*value))
                    })
                    .ok_or_else(|| {
                        AsyncTiffError::General(
                            "JavaScript-compatible sample bit offset overflow".to_string(),
                        )
                    })?
                    / 8;
                (data.as_slice(), bytes_per_pixel, sample_offset)
            }
            JavaScriptBlockData::Planar(data) => (
                data.get(sample)
                    .ok_or_else(|| {
                        AsyncTiffError::General(format!("Invalid sample index '{sample}'."))
                    })?
                    .as_slice(),
                planar_bytes_per_pixel.unwrap_or_else(|| usize::from(bits).div_ceil(8)),
                0,
            ),
            JavaScriptBlockData::Error(error) => {
                return Err(AsyncTiffError::General(error.clone()));
            }
        };
        let offset = pixel
            .checked_mul(bytes_per_pixel)
            .and_then(|value| value.checked_add(sample_offset))
            .ok_or_else(|| {
                AsyncTiffError::General("JavaScript-compatible byte offset overflow".to_string())
            })?;
        read_javascript_sample(data, offset, format, bits, endianness)
    }

    pub(crate) fn javascript_data(&self, sample: usize) -> AsyncTiffResult<Vec<u8>> {
        let compatibility = self.javascript_compatibility.as_ref().ok_or_else(|| {
            AsyncTiffError::General(
                "Decoded block is missing its geotiff.js-compatible buffer".to_string(),
            )
        })?;
        match &compatibility.data {
            JavaScriptBlockData::Chunky(data) => Ok(data.clone()),
            JavaScriptBlockData::ChunkyBySample(data) | JavaScriptBlockData::Planar(data) => {
                data.get(sample).cloned().ok_or_else(|| {
                    AsyncTiffError::General(format!("Invalid sample index '{sample}'."))
                })
            }
            JavaScriptBlockData::Error(error) => Err(AsyncTiffError::General(error.clone())),
        }
    }
}

#[derive(Clone, Copy)]
enum ValueByteOrder {
    File(Endianness),
    Native,
}

pub(crate) fn sample_bits(ifd: &ImageFileDirectory) -> Vec<u16> {
    let samples = ifd.samples_per_pixel() as usize;
    let source = ifd.bits_per_sample();
    let default = source.first().copied().unwrap_or(8);
    (0..samples)
        .map(|index| source.get(index).copied().unwrap_or(default))
        .collect()
}

pub(crate) fn sample_formats(ifd: &ImageFileDirectory) -> Vec<SampleFormat> {
    let samples = ifd.samples_per_pixel() as usize;
    let source = ifd.sample_format();
    let default = source.first().copied().unwrap_or(SampleFormat::Uint);
    (0..samples)
        .map(|index| source.get(index).copied().unwrap_or(default))
        .collect()
}

pub(crate) fn typed_array_for(
    format: SampleFormat,
    bits: u16,
    len: usize,
) -> AsyncTiffResult<TypedArray> {
    fn zeroed<T: Default + Clone>(len: usize) -> AsyncTiffResult<Vec<T>> {
        let mut values = Vec::new();
        values.try_reserve_exact(len).map_err(|error| {
            AsyncTiffError::General(format!("Could not allocate {len} raster samples: {error}"))
        })?;
        values.resize(len, T::default());
        Ok(values)
    }

    let array = match format {
        SampleFormat::Uint if bits <= 8 => TypedArray::Uint8(zeroed(len)?),
        SampleFormat::Uint if bits <= 16 => TypedArray::Uint16(zeroed(len)?),
        SampleFormat::Uint if bits <= 32 => TypedArray::Uint32(zeroed(len)?),
        SampleFormat::Uint if bits <= 64 => TypedArray::Uint64(zeroed(len)?),
        SampleFormat::Int if bits <= 8 => TypedArray::Int8(zeroed(len)?),
        SampleFormat::Int if bits <= 16 => TypedArray::Int16(zeroed(len)?),
        SampleFormat::Int if bits <= 32 => TypedArray::Int32(zeroed(len)?),
        SampleFormat::Int if bits <= 64 => TypedArray::Int64(zeroed(len)?),
        SampleFormat::Float if bits == 16 || bits == 32 => TypedArray::Float32(zeroed(len)?),
        SampleFormat::Float if bits == 64 => TypedArray::Float64(zeroed(len)?),
        _ => {
            return Err(AsyncTiffError::General(format!(
                "Unsupported data format/bitsPerSample: {format:?}/{bits}"
            )));
        }
    };
    Ok(array)
}

fn set_unsigned(array: &mut TypedArray, index: usize, value: u64) {
    match array {
        TypedArray::Uint8(values) | TypedArray::Uint8Clamped(values) => values[index] = value as u8,
        TypedArray::Uint16(values) => values[index] = value as u16,
        TypedArray::Uint32(values) => values[index] = value as u32,
        TypedArray::Uint64(values) => values[index] = value,
        _ => unreachable!("unsigned value assigned to non-unsigned array"),
    }
}

fn set_signed(array: &mut TypedArray, index: usize, value: i64) {
    match array {
        TypedArray::Int8(values) => values[index] = value as i8,
        TypedArray::Int16(values) => values[index] = value as i16,
        TypedArray::Int32(values) => values[index] = value as i32,
        TypedArray::Int64(values) => values[index] = value,
        _ => unreachable!("signed value assigned to non-signed array"),
    }
}

fn set_float(array: &mut TypedArray, index: usize, value: f64) {
    match array {
        TypedArray::Float32(values) => values[index] = value as f32,
        TypedArray::Float64(values) => values[index] = value,
        _ => unreachable!("float value assigned to non-float array"),
    }
}

fn native_endianness() -> Endianness {
    if cfg!(target_endian = "little") {
        Endianness::LittleEndian
    } else {
        Endianness::BigEndian
    }
}

fn effective_endianness(order: ValueByteOrder) -> Endianness {
    match order {
        ValueByteOrder::File(order) => order,
        ValueByteOrder::Native => native_endianness(),
    }
}

fn read_uint_bytes(bytes: &[u8], order: Endianness) -> u64 {
    match order {
        Endianness::LittleEndian => bytes.iter().enumerate().fold(0u64, |value, (index, byte)| {
            value | ((*byte as u64) << (index * 8))
        }),
        Endianness::BigEndian => bytes
            .iter()
            .fold(0u64, |value, byte| (value << 8) | *byte as u64),
    }
}

fn javascript_reader_bytes(format: SampleFormat, bits: u16) -> AsyncTiffResult<usize> {
    match format {
        SampleFormat::Uint | SampleFormat::Int if bits <= 8 => Ok(1),
        SampleFormat::Uint | SampleFormat::Int if bits <= 16 => Ok(2),
        SampleFormat::Uint | SampleFormat::Int if bits <= 32 => Ok(4),
        SampleFormat::Float if bits == 16 => Ok(2),
        SampleFormat::Float if bits == 32 => Ok(4),
        SampleFormat::Float if bits == 64 => Ok(8),
        _ => Err(AsyncTiffError::General(
            "Unsupported data format/bitsPerSample".to_string(),
        )),
    }
}

fn read_javascript_sample(
    data: &[u8],
    offset: usize,
    format: SampleFormat,
    bits: u16,
    endianness: Endianness,
) -> AsyncTiffResult<f64> {
    let byte_count = javascript_reader_bytes(format, bits)?;
    let end = offset.checked_add(byte_count).ok_or_else(|| {
        AsyncTiffError::General("JavaScript-compatible sample range overflow".to_string())
    })?;
    let bytes = data.get(offset..end).ok_or_else(|| {
        AsyncTiffError::General(format!(
            "JavaScript DataView read is outside the decoded block: need bytes {offset}..{end}, length is {}",
            data.len()
        ))
    })?;
    let raw = read_uint_bytes(bytes, endianness);
    let value = match format {
        SampleFormat::Uint => raw as f64,
        SampleFormat::Int => sign_extend(raw, (byte_count * 8) as u16) as f64,
        SampleFormat::Float => match bits {
            16 => f16::from_bits(raw as u16).to_f64(),
            32 => f32::from_bits(raw as u32) as f64,
            64 => f64::from_bits(raw),
            _ => unreachable!("floating-point width was validated above"),
        },
        _ => unreachable!("sample format was validated above"),
    };
    Ok(value)
}

fn javascript_array_element_bytes(format: SampleFormat, bits: u16) -> Result<usize, String> {
    match format {
        SampleFormat::Uint if bits <= 8 => Ok(1),
        SampleFormat::Uint if bits <= 16 => Ok(2),
        SampleFormat::Uint if bits <= 32 => Ok(4),
        SampleFormat::Int if bits == 8 => Ok(1),
        SampleFormat::Int if bits == 16 => Ok(2),
        SampleFormat::Int if bits == 32 => Ok(4),
        SampleFormat::Float if bits == 16 || bits == 32 => Ok(4),
        SampleFormat::Float if bits == 64 => Ok(8),
        _ => Err("Unsupported data format/bitsPerSample".to_string()),
    }
}

fn javascript_needs_normalization(format: SampleFormat, bits: u16) -> bool {
    match format {
        SampleFormat::Uint | SampleFormat::Int => bits > 32 || !bits.is_multiple_of(8),
        SampleFormat::Float => !matches!(bits, 16 | 32 | 64),
        _ => true,
    }
}

fn write_native_unsigned(bytes: &mut [u8], value: u64) {
    if cfg!(target_endian = "little") {
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = (value >> (index * 8)) as u8;
        }
    } else {
        let len = bytes.len();
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = (value >> ((len - index - 1) * 8)) as u8;
        }
    }
}

/// Exact native typed-array buffer produced by geotiff.js `normalizeArray`.
/// The original uses the first sample's type/width for every chunky sample;
/// retaining that quirk is essential to reproducing its later DataView reads.
fn javascript_normalize_array(
    decoded: &[u8],
    format: SampleFormat,
    planar: bool,
    samples_per_pixel: usize,
    bits: u16,
    width: usize,
    height: usize,
) -> Result<Vec<u8>, String> {
    let element_bytes = javascript_array_element_bytes(format, bits)?;
    let samples_to_transfer = if planar { 1 } else { samples_per_pixel };
    let output_len = width
        .checked_mul(height)
        .and_then(|value| value.checked_mul(samples_to_transfer))
        .and_then(|value| value.checked_mul(element_bytes))
        .ok_or_else(|| "JavaScript normalization buffer size overflow".to_string())?;
    let mut output = vec![0u8; output_len];

    // normalizeArray only implements unsigned packed integers. Its signed
    // and floating branches either reject the typed-array constructor or
    // leave the newly allocated array filled with zeros.
    if format != SampleFormat::Uint {
        return Ok(output);
    }
    let pixel_bit_skip = bits as usize * samples_to_transfer;
    let row_bits = width
        .checked_mul(pixel_bit_skip)
        .and_then(|value| value.checked_add(7))
        .map(|value| value / 8 * 8)
        .ok_or_else(|| "JavaScript normalization row size overflow".to_string())?;
    for y in 0..height {
        for x in 0..width {
            for sample in 0..samples_to_transfer {
                let bit_offset = y
                    .checked_mul(row_bits)
                    .and_then(|value| {
                        x.checked_mul(samples_to_transfer * bits as usize)
                            .and_then(|x_offset| value.checked_add(x_offset))
                    })
                    .and_then(|value| value.checked_add(sample * bits as usize))
                    .ok_or_else(|| "JavaScript normalization bit offset overflow".to_string())?;
                let value =
                    read_bits_msb(decoded, bit_offset, bits).map_err(|error| error.to_string())?;
                let output_index = (y * width + x) * samples_to_transfer + sample;
                let byte_offset = output_index * element_bytes;
                write_native_unsigned(&mut output[byte_offset..byte_offset + element_bytes], value);
            }
        }
    }
    Ok(output)
}

fn javascript_chunky_compatibility(
    decoded: &[u8],
    formats: &[SampleFormat],
    bits: &[u16],
    width: usize,
    height: usize,
) -> Option<JavaScriptCompatibilityBlock> {
    let data = if javascript_needs_normalization(formats[0], bits[0]) {
        match javascript_normalize_array(
            decoded,
            formats[0],
            false,
            bits.len(),
            bits[0],
            width,
            height,
        ) {
            Ok(data) => JavaScriptBlockData::Chunky(data),
            Err(error) => JavaScriptBlockData::Error(error),
        }
    } else {
        JavaScriptBlockData::Chunky(decoded.to_vec())
    };
    Some(JavaScriptCompatibilityBlock {
        data,
        formats: formats.to_vec(),
        bits: bits.to_vec(),
    })
}

fn javascript_planar_band_data(
    decoded: Vec<u8>,
    first_format: SampleFormat,
    first_bits: u16,
    samples_per_pixel: usize,
    width: usize,
    height: usize,
) -> Result<Vec<u8>, String> {
    if javascript_needs_normalization(first_format, first_bits) {
        javascript_normalize_array(
            &decoded,
            first_format,
            true,
            samples_per_pixel,
            first_bits,
            width,
            height,
        )
    } else {
        Ok(decoded)
    }
}

fn javascript_integer_conversion(value: f64, byte_count: usize) -> u64 {
    if !value.is_finite() || value == 0.0 {
        return 0;
    }
    let modulus = 2f64.powi((byte_count * 8) as i32);
    value.trunc().rem_euclid(modulus) as u64
}

fn javascript_filled_array_buffer(
    byte_len: usize,
    format: SampleFormat,
    bits: u16,
    fill: f64,
) -> Result<Vec<u8>, String> {
    let element_bytes = javascript_array_element_bytes(format, bits)?;
    if !byte_len.is_multiple_of(element_bytes) {
        return Err("byte length of typed array should be a multiple of element size".to_string());
    }
    let mut output = vec![0u8; byte_len];
    for element in output.chunks_exact_mut(element_bytes) {
        let raw = match format {
            SampleFormat::Uint | SampleFormat::Int => {
                javascript_integer_conversion(fill, element_bytes)
            }
            SampleFormat::Float if element_bytes == 4 => u64::from((fill as f32).to_bits()),
            SampleFormat::Float => fill.to_bits(),
            _ => return Err("Unsupported data format/bitsPerSample".to_string()),
        };
        write_native_unsigned(element, raw);
    }
    Ok(output)
}

fn javascript_empty_compatibility(
    formats: &[SampleFormat],
    bits: &[u16],
    layout: PlanarConfiguration,
    pixels: usize,
    fill: f64,
) -> Option<JavaScriptCompatibilityBlock> {
    let data = match layout {
        PlanarConfiguration::Chunky => {
            let byte_len = bits
                .iter()
                .try_fold(0usize, |total, value| {
                    total.checked_add(usize::from(*value).div_ceil(8))
                })
                .and_then(|bytes_per_pixel| bytes_per_pixel.checked_mul(pixels));
            match byte_len {
                Some(len) => {
                    let mut buffers = Vec::with_capacity(bits.len());
                    for sample in 0..bits.len() {
                        match javascript_filled_array_buffer(
                            len,
                            formats[sample],
                            bits[sample],
                            fill,
                        ) {
                            Ok(data) => buffers.push(data),
                            Err(error) => {
                                return Some(JavaScriptCompatibilityBlock {
                                    data: JavaScriptBlockData::Error(error),
                                    formats: formats.to_vec(),
                                    bits: bits.to_vec(),
                                });
                            }
                        }
                    }
                    JavaScriptBlockData::ChunkyBySample(buffers)
                }
                None => JavaScriptBlockData::Error(
                    "Could not allocate JavaScript-compatible empty block".to_string(),
                ),
            }
        }
        PlanarConfiguration::Planar => {
            let mut decoded = Vec::with_capacity(bits.len());
            for sample in 0..bits.len() {
                let Some(byte_len) = usize::from(bits[sample]).div_ceil(8).checked_mul(pixels)
                else {
                    return Some(JavaScriptCompatibilityBlock {
                        data: JavaScriptBlockData::Error(
                            "JavaScript-compatible empty block size overflow".to_string(),
                        ),
                        formats: formats.to_vec(),
                        bits: bits.to_vec(),
                    });
                };
                match javascript_filled_array_buffer(byte_len, formats[sample], bits[sample], fill)
                {
                    Ok(data) => decoded.push(data),
                    Err(error) => {
                        return Some(JavaScriptCompatibilityBlock {
                            data: JavaScriptBlockData::Error(error),
                            formats: formats.to_vec(),
                            bits: bits.to_vec(),
                        });
                    }
                }
            }
            JavaScriptBlockData::Planar(decoded)
        }
        other => JavaScriptBlockData::Error(format!(
            "Invalid planar configuration for JavaScript compatibility: {other:?}"
        )),
    };
    Some(JavaScriptCompatibilityBlock {
        data,
        formats: formats.to_vec(),
        bits: bits.to_vec(),
    })
}

fn read_bits_msb(bytes: &[u8], bit_offset: usize, bit_count: u16) -> AsyncTiffResult<u64> {
    if bit_count == 0 || bit_count > 64 {
        return Err(AsyncTiffError::General(format!(
            "Unsupported packed sample width: {bit_count}"
        )));
    }
    let end = bit_offset
        .checked_add(bit_count as usize)
        .ok_or_else(|| AsyncTiffError::General("Packed sample bit offset overflow".to_string()))?;
    let available_bits = bytes.len().saturating_mul(8);
    if end > available_bits {
        return Err(AsyncTiffError::General(format!(
            "Decoded block is too short: need bit {end}, only {} bits available",
            available_bits
        )));
    }

    let mut value = 0u64;
    for bit in bit_offset..end {
        value = (value << 1) | u64::from((bytes[bit / 8] >> (7 - (bit % 8))) & 1);
    }
    Ok(value)
}

fn sign_extend(value: u64, bits: u16) -> i64 {
    if bits == 64 {
        value as i64
    } else {
        let shift = 64 - bits;
        ((value << shift) as i64) >> shift
    }
}

fn set_integer_value(
    array: &mut TypedArray,
    index: usize,
    format: SampleFormat,
    value: u64,
    bits: u16,
) {
    match format {
        SampleFormat::Uint => set_unsigned(array, index, value),
        SampleFormat::Int => set_signed(array, index, sign_extend(value, bits)),
        _ => unreachable!("integer reader called for non-integer sample"),
    }
}

fn set_byte_aligned_value(
    array: &mut TypedArray,
    index: usize,
    format: SampleFormat,
    bits: u16,
    bytes: &[u8],
    order: ValueByteOrder,
) -> AsyncTiffResult<()> {
    let raw = read_uint_bytes(bytes, effective_endianness(order));
    match format {
        SampleFormat::Uint | SampleFormat::Int => {
            set_integer_value(array, index, format, raw, bits)
        }
        SampleFormat::Float => {
            let value = match bits {
                16 => f16::from_bits(raw as u16).to_f64(),
                32 => f32::from_bits(raw as u32) as f64,
                64 => f64::from_bits(raw),
                _ => {
                    return Err(AsyncTiffError::General(format!(
                        "Unsupported floating-point sample width: {bits}"
                    )));
                }
            };
            set_float(array, index, value);
        }
        _ => {
            return Err(AsyncTiffError::General(format!(
                "Unsupported sample format: {format:?}"
            )));
        }
    }
    Ok(())
}

fn decode_chunky_bands(
    bytes: &[u8],
    formats: &[SampleFormat],
    bits: &[u16],
    width: usize,
    height: usize,
    order: ValueByteOrder,
) -> AsyncTiffResult<Vec<TypedArray>> {
    let samples = bits.len();
    let len = width
        .checked_mul(height)
        .ok_or_else(|| AsyncTiffError::General("Decoded block pixel count overflow".to_string()))?;
    let mut bands = formats
        .iter()
        .zip(bits)
        .map(|(&format, &width)| typed_array_for(format, width, len))
        .collect::<AsyncTiffResult<Vec<_>>>()?;
    let pixel_bits = bits.iter().try_fold(0usize, |sum, value| {
        sum.checked_add(*value as usize)
            .ok_or_else(|| AsyncTiffError::General("BitsPerSample sum overflow".to_string()))
    })?;
    let row_bits = width
        .checked_mul(pixel_bits)
        .ok_or_else(|| AsyncTiffError::General("Decoded row bit count overflow".to_string()))?;
    let row_stride_bits = row_bits
        .checked_add(7)
        .and_then(|value| value.checked_div(8))
        .and_then(|value| value.checked_mul(8))
        .ok_or_else(|| AsyncTiffError::General("Decoded row stride overflow".to_string()))?;
    let byte_aligned = bits.iter().all(|value| value.is_multiple_of(8));

    for y in 0..height {
        for x in 0..width {
            let output_index = y * width + x;
            let mut sample_bit_offset = 0usize;
            for sample in 0..samples {
                let sample_bits = bits[sample];
                let bit_offset = y
                    .checked_mul(row_stride_bits)
                    .and_then(|value| x.checked_mul(pixel_bits).and_then(|x| value.checked_add(x)))
                    .and_then(|value| value.checked_add(sample_bit_offset))
                    .ok_or_else(|| {
                        AsyncTiffError::General("Decoded sample bit offset overflow".to_string())
                    })?;
                if byte_aligned {
                    let byte_offset = bit_offset / 8;
                    let byte_count = (sample_bits / 8) as usize;
                    let end = byte_offset.checked_add(byte_count).ok_or_else(|| {
                        AsyncTiffError::General("Decoded byte offset overflow".to_string())
                    })?;
                    let value_bytes = bytes.get(byte_offset..end).ok_or_else(|| {
                        AsyncTiffError::General(format!(
                            "Decoded chunky block is too short: need bytes {byte_offset}..{end}, length is {}",
                            bytes.len()
                        ))
                    })?;
                    set_byte_aligned_value(
                        &mut bands[sample],
                        output_index,
                        formats[sample],
                        sample_bits,
                        value_bytes,
                        order,
                    )?;
                } else {
                    if !matches!(formats[sample], SampleFormat::Uint | SampleFormat::Int) {
                        return Err(AsyncTiffError::General(format!(
                            "Packed non-integer sample is unsupported: {:?}/{sample_bits}",
                            formats[sample]
                        )));
                    }
                    let value = read_bits_msb(bytes, bit_offset, sample_bits)?;
                    set_integer_value(
                        &mut bands[sample],
                        output_index,
                        formats[sample],
                        value,
                        sample_bits,
                    );
                }
                sample_bit_offset = sample_bit_offset
                    .checked_add(sample_bits as usize)
                    .ok_or_else(|| {
                        AsyncTiffError::General("Sample bit offset overflow".to_string())
                    })?;
            }
        }
    }
    Ok(bands)
}

fn decode_planar_band(
    bytes: &[u8],
    format: SampleFormat,
    bits: u16,
    width: usize,
    height: usize,
    order: ValueByteOrder,
) -> AsyncTiffResult<TypedArray> {
    let len = width
        .checked_mul(height)
        .ok_or_else(|| AsyncTiffError::General("Decoded block pixel count overflow".to_string()))?;
    let mut band = typed_array_for(format, bits, len)?;
    let row_bits = width
        .checked_mul(bits as usize)
        .ok_or_else(|| AsyncTiffError::General("Decoded row bit count overflow".to_string()))?;
    let row_stride_bits = row_bits
        .checked_add(7)
        .and_then(|value| value.checked_div(8))
        .and_then(|value| value.checked_mul(8))
        .ok_or_else(|| AsyncTiffError::General("Decoded row stride overflow".to_string()))?;

    for y in 0..height {
        for x in 0..width {
            let output_index = y * width + x;
            let bit_offset = y
                .checked_mul(row_stride_bits)
                .and_then(|value| {
                    x.checked_mul(bits as usize)
                        .and_then(|x| value.checked_add(x))
                })
                .ok_or_else(|| {
                    AsyncTiffError::General("Decoded sample bit offset overflow".to_string())
                })?;
            if bits.is_multiple_of(8) {
                let byte_offset = bit_offset / 8;
                let byte_count = (bits / 8) as usize;
                let end = byte_offset.checked_add(byte_count).ok_or_else(|| {
                    AsyncTiffError::General("Decoded byte offset overflow".to_string())
                })?;
                let value_bytes = bytes.get(byte_offset..end).ok_or_else(|| {
                    AsyncTiffError::General(format!(
                        "Decoded planar block is too short: need bytes {byte_offset}..{end}, length is {}",
                        bytes.len()
                    ))
                })?;
                set_byte_aligned_value(&mut band, output_index, format, bits, value_bytes, order)?;
            } else {
                if !matches!(format, SampleFormat::Uint | SampleFormat::Int) {
                    return Err(AsyncTiffError::General(format!(
                        "Packed non-integer sample is unsupported: {format:?}/{bits}"
                    )));
                }
                let value = read_bits_msb(bytes, bit_offset, bits)?;
                set_integer_value(&mut band, output_index, format, value, bits);
            }
        }
    }
    Ok(band)
}

fn swap_to_native(bytes: &mut [u8], endianness: Endianness, bits: u16) {
    if endianness.is_native() || bits <= 8 {
        return;
    }
    let byte_count = (bits as usize).div_ceil(8);
    for value in bytes.chunks_exact_mut(byte_count) {
        value.reverse();
    }
}

fn reverse_predictor(
    bytes: &mut [u8],
    predictor: Option<Predictor>,
    width: usize,
    height: usize,
    bits: &[u16],
    layout: PlanarConfiguration,
    endianness: Endianness,
) -> AsyncTiffResult<ValueByteOrder> {
    match predictor.unwrap_or(Predictor::None) {
        Predictor::None => Ok(ValueByteOrder::File(endianness)),
        Predictor::Horizontal => {
            if bits.is_empty()
                || bits.iter().any(|value| !value.is_multiple_of(8))
                || bits.iter().any(|value| *value != bits[0])
                || !matches!(bits[0], 8 | 16 | 32)
            {
                return Err(AsyncTiffError::General(
                    "When decoding predictor 2, samples must have the same 8, 16, or 32-bit width."
                        .to_string(),
                ));
            }
            swap_to_native(bytes, endianness, bits[0]);
            apply_predictor(
                bytes,
                Some(Predictor::Horizontal),
                width,
                height,
                bits,
                layout,
            )
            .map_err(|error| AsyncTiffError::General(error.to_string()))?;
            Ok(ValueByteOrder::Native)
        }
        Predictor::FloatingPoint => {
            if bits.is_empty()
                || bits.iter().any(|value| !value.is_multiple_of(8))
                || bits.iter().any(|value| *value != bits[0])
                || !matches!(bits[0], 16 | 32 | 64)
            {
                return Err(AsyncTiffError::General(
                    "When decoding predictor 3, samples must have the same 16, 32, or 64-bit width."
                        .to_string(),
                ));
            }
            apply_predictor(
                bytes,
                Some(Predictor::FloatingPoint),
                width,
                height,
                bits,
                layout,
            )
            .map_err(|error| AsyncTiffError::General(error.to_string()))?;
            // `decode_row_floating_point` already reverses the big-endian
            // byte planes into native little-endian sample byte order (the
            // ordering produced by geotiff.js on its supported hosts).
            // Swapping again here corrupts every Predictor=3 float.
            Ok(ValueByteOrder::Native)
        }
        _ => Ok(ValueByteOrder::File(endianness)),
    }
}

fn nodata_value(ifd: &ImageFileDirectory) -> f64 {
    ifd.gdal_nodata()
        .map(|value| {
            let end = value
                .char_indices()
                .next_back()
                .map(|(index, _)| index)
                .unwrap_or(0);
            crate::utils::parse_js_number(&value[..end])
        })
        .filter(|value| *value != 0.0 && !value.is_nan())
        .unwrap_or(0.0)
}

fn empty_block(
    ifd: &ImageFileDirectory,
    width: usize,
    height: usize,
) -> AsyncTiffResult<DecodedBlock> {
    let bits = sample_bits(ifd);
    let formats = sample_formats(ifd);
    let fill = nodata_value(ifd);
    let len = width
        .checked_mul(height)
        .ok_or_else(|| AsyncTiffError::General("Empty block pixel count overflow".to_string()))?;
    let mut bands = formats
        .iter()
        .zip(&bits)
        .map(|(&format, &bits)| typed_array_for(format, bits, len))
        .collect::<AsyncTiffResult<Vec<_>>>()?;
    if fill != 0.0 {
        for band in &mut bands {
            for index in 0..len {
                band.set_f64(index, fill);
            }
        }
    }
    let javascript_compatibility =
        javascript_empty_compatibility(&formats, &bits, ifd.planar_configuration(), len, fill);
    Ok(DecodedBlock {
        bands,
        width,
        height,
        javascript_compatibility,
    })
}

#[allow(clippy::too_many_arguments)]
async fn decode_one(
    bytes: Bytes,
    ifd: &ImageFileDirectory,
    registry: Arc<DecoderRegistry>,
    samples: u16,
    bits: u16,
    cancellation: Option<&CancellationToken>,
) -> AsyncTiffResult<Vec<u8>> {
    let compression = ifd.compression();
    let photometric = ifd.photometric_interpretation();
    let jpeg_tables = ifd.jpeg_tables().map(ToOwned::to_owned);
    let lerc_parameters = ifd.lerc_parameters().map(ToOwned::to_owned);
    spawn_decode(
        move || {
            let decoder = registry
                .as_ref()
                .as_ref()
                .get(&compression)
                .ok_or_else(|| {
                    AsyncTiffError::General(format!("No decoder registered for {compression:?}"))
                })?;
            decoder.decode_tile(
                bytes,
                photometric,
                jpeg_tables.as_deref(),
                samples,
                bits,
                lerc_parameters.as_deref(),
            )
        },
        cancellation,
    )
    .await
}

async fn decode_compressed(
    ifd: &ImageFileDirectory,
    compressed: CompressedBytes,
    width: usize,
    height: usize,
    endianness: Endianness,
    registry: Arc<DecoderRegistry>,
    cancellation: Option<&CancellationToken>,
) -> AsyncTiffResult<DecodedBlock> {
    let bits = sample_bits(ifd);
    let formats = sample_formats(ifd);
    let predictor = ifd.predictor();

    match compressed {
        CompressedBytes::Chunky(bytes) => {
            if bytes.is_empty() {
                return empty_block(ifd, width, height);
            }
            let mut decoded = decode_one(
                bytes,
                ifd,
                registry,
                ifd.samples_per_pixel(),
                bits[0],
                cancellation,
            )
            .await?;
            let order = reverse_predictor(
                &mut decoded,
                predictor,
                width,
                height,
                &bits,
                PlanarConfiguration::Chunky,
                endianness,
            )?;
            let javascript_compatibility =
                javascript_chunky_compatibility(&decoded, &formats, &bits, width, height);
            let bands = decode_chunky_bands(&decoded, &formats, &bits, width, height, order)?;
            Ok(DecodedBlock {
                bands,
                width,
                height,
                javascript_compatibility,
            })
        }
        CompressedBytes::Planar(compressed_bands) => {
            let mut bands = Vec::with_capacity(bits.len());
            let mut javascript_data_bands = Some(Vec::with_capacity(bits.len()));
            let mut javascript_error = None;
            for sample in 0..bits.len() {
                let bytes = compressed_bands.get(sample).cloned().unwrap_or_default();
                if bytes.is_empty() {
                    let mut empty = empty_block(ifd, width, height)?;
                    bands.push(empty.bands.remove(sample));
                    if let Some(data_bands) = &mut javascript_data_bands {
                        match empty.javascript_compatibility {
                            Some(JavaScriptCompatibilityBlock {
                                data: JavaScriptBlockData::Planar(mut data),
                                ..
                            }) => data_bands.push(data.remove(sample)),
                            Some(JavaScriptCompatibilityBlock {
                                data: JavaScriptBlockData::Error(error),
                                ..
                            }) => {
                                javascript_error = Some(error);
                                data_bands.push(Vec::new());
                            }
                            _ => data_bands.push(Vec::new()),
                        }
                    }
                    continue;
                }
                let mut decoded =
                    decode_one(bytes, ifd, registry.clone(), 1, bits[sample], cancellation).await?;
                let order = reverse_predictor(
                    &mut decoded,
                    predictor,
                    width,
                    height,
                    &[bits[sample]],
                    PlanarConfiguration::Planar,
                    endianness,
                )?;
                if let Some(data_bands) = &mut javascript_data_bands {
                    match javascript_planar_band_data(
                        decoded.clone(),
                        formats[0],
                        bits[0],
                        bits.len(),
                        width,
                        height,
                    ) {
                        Ok(data) => data_bands.push(data),
                        Err(error) => {
                            javascript_error = Some(error);
                            data_bands.push(Vec::new());
                        }
                    }
                }
                bands.push(decode_planar_band(
                    &decoded,
                    formats[sample],
                    bits[sample],
                    width,
                    height,
                    order,
                )?);
            }
            let javascript_compatibility = if let Some(error) = javascript_error {
                Some(JavaScriptCompatibilityBlock {
                    data: JavaScriptBlockData::Error(error),
                    formats: formats.clone(),
                    bits: bits.clone(),
                })
            } else {
                javascript_data_bands.map(|data| JavaScriptCompatibilityBlock {
                    data: JavaScriptBlockData::Planar(data),
                    formats: formats.clone(),
                    bits: bits.clone(),
                })
            };
            Ok(DecodedBlock {
                bands,
                width,
                height,
                javascript_compatibility,
            })
        }
    }
}

pub(crate) async fn fetch_tile(
    ifd: &ImageFileDirectory,
    x: usize,
    y: usize,
    reader: &dyn AsyncFileReader,
    endianness: Endianness,
    registry: Arc<DecoderRegistry>,
    cancellation: Option<&CancellationToken>,
) -> AsyncTiffResult<DecodedBlock> {
    let width = ifd
        .tile_width()
        .filter(|value| *value != 0)
        .ok_or_else(|| {
            AsyncTiffError::General("Tiled TIFF has no positive TileWidth".to_string())
        })? as usize;
    let height = ifd
        .tile_height()
        .filter(|value| *value != 0)
        .ok_or_else(|| {
            AsyncTiffError::General("Tiled TIFF has no positive TileLength".to_string())
        })? as usize;
    let tile = cancellable(ifd.fetch_tile(x, y, reader), cancellation).await?;
    let compressed = tile.compressed_bytes().clone();
    decode_compressed(
        ifd,
        compressed,
        width,
        height,
        endianness,
        registry,
        cancellation,
    )
    .await
}

pub(crate) async fn fetch_tile_cached(
    cache: Option<&DecodedBlockCache>,
    ifd: &ImageFileDirectory,
    coordinates: (usize, usize),
    reader: &dyn AsyncFileReader,
    endianness: Endianness,
    registry: Arc<DecoderRegistry>,
    cancellation: Option<&CancellationToken>,
) -> AsyncTiffResult<Arc<DecodedBlock>> {
    check_cancelled(cancellation)?;
    let (x, y) = coordinates;
    let Some(cache) = cache else {
        return fetch_tile(ifd, x, y, reader, endianness, registry, cancellation)
            .await
            .map(Arc::new);
    };
    cache
        .blocks
        .try_get_with((x, y), async move {
            fetch_tile(ifd, x, y, reader, endianness, registry, cancellation)
                .await
                .map(Arc::new)
        })
        .await
        .map_err(|error| AsyncTiffError::General(error.to_string()))
}

pub(crate) async fn fetch_strip(
    ifd: &ImageFileDirectory,
    strip_row: usize,
    reader: &dyn AsyncFileReader,
    endianness: Endianness,
    registry: Arc<DecoderRegistry>,
    cancellation: Option<&CancellationToken>,
) -> AsyncTiffResult<DecodedBlock> {
    let width = ifd.image_width() as usize;
    let image_height = ifd.image_height() as usize;
    let rows_per_strip = ifd
        .rows_per_strip()
        .map(|value| value as usize)
        .unwrap_or(image_height);
    if rows_per_strip == 0 {
        return Err(AsyncTiffError::General("RowsPerStrip is zero".to_string()));
    }
    let strip_origin = strip_row
        .checked_mul(rows_per_strip)
        .ok_or_else(|| AsyncTiffError::General("Strip row offset overflow".to_string()))?;
    let height = rows_per_strip.min(image_height.saturating_sub(strip_origin));
    let row_strip_count = image_height.div_ceil(rows_per_strip);
    let offsets = ifd
        .strip_offsets()
        .ok_or_else(|| AsyncTiffError::General("StripOffsets is missing".to_string()))?;
    let byte_counts = ifd
        .strip_byte_counts()
        .ok_or_else(|| AsyncTiffError::General("StripByteCounts is missing".to_string()))?;

    let compressed = match ifd.planar_configuration() {
        PlanarConfiguration::Planar => {
            let mut bands = Vec::with_capacity(ifd.samples_per_pixel() as usize);
            for sample in 0..ifd.samples_per_pixel() as usize {
                let index = sample
                    .checked_mul(row_strip_count)
                    .and_then(|value| value.checked_add(strip_row))
                    .ok_or_else(|| {
                        AsyncTiffError::General("Planar strip index overflow".to_string())
                    })?;
                let offset = *offsets.get(index).ok_or_else(|| {
                    AsyncTiffError::General(format!("StripOffsets index {index} is out of bounds"))
                })?;
                let byte_count = *byte_counts.get(index).ok_or_else(|| {
                    AsyncTiffError::General(format!(
                        "StripByteCounts index {index} is out of bounds"
                    ))
                })?;
                let end = offset.checked_add(byte_count).ok_or_else(|| {
                    AsyncTiffError::General("Strip byte range overflow".to_string())
                })?;
                bands.push(cancellable(reader.get_bytes(offset..end), cancellation).await?);
            }
            CompressedBytes::Planar(bands)
        }
        _ => {
            let offset = *offsets.get(strip_row).ok_or_else(|| {
                AsyncTiffError::General(format!("StripOffsets index {strip_row} is out of bounds"))
            })?;
            let byte_count = *byte_counts.get(strip_row).ok_or_else(|| {
                AsyncTiffError::General(format!(
                    "StripByteCounts index {strip_row} is out of bounds"
                ))
            })?;
            let end = offset
                .checked_add(byte_count)
                .ok_or_else(|| AsyncTiffError::General("Strip byte range overflow".to_string()))?;
            CompressedBytes::Chunky(cancellable(reader.get_bytes(offset..end), cancellation).await?)
        }
    };

    decode_compressed(
        ifd,
        compressed,
        width,
        height,
        endianness,
        registry,
        cancellation,
    )
    .await
}

pub(crate) async fn fetch_strip_cached(
    cache: Option<&DecodedBlockCache>,
    ifd: &ImageFileDirectory,
    strip_row: usize,
    reader: &dyn AsyncFileReader,
    endianness: Endianness,
    registry: Arc<DecoderRegistry>,
    cancellation: Option<&CancellationToken>,
) -> AsyncTiffResult<Arc<DecodedBlock>> {
    check_cancelled(cancellation)?;
    let Some(cache) = cache else {
        return fetch_strip(ifd, strip_row, reader, endianness, registry, cancellation)
            .await
            .map(Arc::new);
    };
    cache
        .blocks
        .try_get_with((0, strip_row), async move {
            fetch_strip(ifd, strip_row, reader, endianness, registry, cancellation)
                .await
                .map(Arc::new)
        })
        .await
        .map_err(|error| AsyncTiffError::General(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpacking_respects_padding_at_the_end_of_each_row() {
        // width=5, height=2, 1-bit. Each row occupies one full byte; the
        // three padding bits must not become the beginning of the next row.
        let decoded = decode_planar_band(
            &[0b1010_1000, 0b0101_0000],
            SampleFormat::Uint,
            1,
            5,
            2,
            ValueByteOrder::File(Endianness::BigEndian),
        )
        .unwrap();
        assert_eq!(
            decoded,
            TypedArray::Uint8(vec![1, 0, 1, 0, 1, 0, 1, 0, 1, 0])
        );
    }

    #[test]
    fn unpacking_supports_twelve_bit_samples() {
        let decoded = decode_planar_band(
            &[0x12, 0x3a, 0xbc],
            SampleFormat::Uint,
            12,
            2,
            1,
            ValueByteOrder::File(Endianness::BigEndian),
        )
        .unwrap();
        assert_eq!(decoded, TypedArray::Uint16(vec![0x123, 0xabc]));
    }

    #[test]
    fn chunky_packed_samples_are_split_into_bands() {
        let bands = decode_chunky_bands(
            &[0b10_01_11_00],
            &[SampleFormat::Uint, SampleFormat::Uint],
            &[2, 2],
            2,
            1,
            ValueByteOrder::File(Endianness::BigEndian),
        )
        .unwrap();
        assert_eq!(bands[0], TypedArray::Uint8(vec![2, 3]));
        assert_eq!(bands[1], TypedArray::Uint8(vec![1, 0]));
    }
}
