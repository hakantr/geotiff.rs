//! Image-level API and raster helpers corresponding to `geotiffimage.js`.

use crate::error::GeotiffError;
use crate::typed_array::TypedArray;
use half::f16;

/// `sum(array, start, end)`
pub fn sum(array: &TypedArray, start: usize, end: usize) -> f64 {
    let mut s = 0.0;
    for i in start..end {
        s += array.get_f64(i);
    }
    s
}

/// `sizeOrData` union parameter of `arrayForType`: either allocate a fresh
/// zeroed array of a given length, or reinterpret existing bytes as one.
pub enum SizeOrData<'a> {
    Size(usize),
    Data(&'a [u8]),
}

/// Native callable form of `getReaderForSample()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleReader {
    Uint8,
    Uint16,
    Uint32,
    Int8,
    Int16,
    Int32,
    Float16,
    Float32,
    Float64,
}

impl SampleReader {
    pub fn read(self, bytes: &[u8], offset: usize, little_endian: bool) -> AsyncTiffResult<f64> {
        let width = match self {
            Self::Uint8 | Self::Int8 => 1,
            Self::Uint16 | Self::Int16 | Self::Float16 => 2,
            Self::Uint32 | Self::Int32 | Self::Float32 => 4,
            Self::Float64 => 8,
        };
        let end = offset
            .checked_add(width)
            .ok_or_else(|| AsyncTiffError::General("sample reader offset overflow".to_string()))?;
        let value = bytes.get(offset..end).ok_or_else(|| {
            AsyncTiffError::General(format!(
                "sample reader needs bytes {offset}..{end}, buffer length is {}",
                bytes.len()
            ))
        })?;
        let number = match self {
            Self::Uint8 => f64::from(value[0]),
            Self::Int8 => f64::from(value[0] as i8),
            Self::Uint16 => f64::from(if little_endian {
                u16::from_le_bytes([value[0], value[1]])
            } else {
                u16::from_be_bytes([value[0], value[1]])
            }),
            Self::Int16 => f64::from(if little_endian {
                i16::from_le_bytes([value[0], value[1]])
            } else {
                i16::from_be_bytes([value[0], value[1]])
            }),
            Self::Uint32 => {
                let value: [u8; 4] = value.try_into().map_err(|_| {
                    AsyncTiffError::General("invalid uint32 sample width".to_string())
                })?;
                f64::from(if little_endian {
                    u32::from_le_bytes(value)
                } else {
                    u32::from_be_bytes(value)
                })
            }
            Self::Int32 => {
                let value: [u8; 4] = value.try_into().map_err(|_| {
                    AsyncTiffError::General("invalid int32 sample width".to_string())
                })?;
                f64::from(if little_endian {
                    i32::from_le_bytes(value)
                } else {
                    i32::from_be_bytes(value)
                })
            }
            Self::Float16 => {
                let value = if little_endian {
                    u16::from_le_bytes([value[0], value[1]])
                } else {
                    u16::from_be_bytes([value[0], value[1]])
                };
                f16::from_bits(value).to_f64()
            }
            Self::Float32 => {
                let value: [u8; 4] = value.try_into().map_err(|_| {
                    AsyncTiffError::General("invalid float32 sample width".to_string())
                })?;
                f64::from(if little_endian {
                    f32::from_le_bytes(value)
                } else {
                    f32::from_be_bytes(value)
                })
            }
            Self::Float64 => {
                let value: [u8; 8] = value.try_into().map_err(|_| {
                    AsyncTiffError::General("invalid float64 sample width".to_string())
                })?;
                if little_endian {
                    f64::from_le_bytes(value)
                } else {
                    f64::from_be_bytes(value)
                }
            }
        };
        Ok(number)
    }
}

fn try_zeroed<T: Default + Clone>(len: usize) -> Result<Vec<T>, GeotiffError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(len)
        .map_err(|error| GeotiffError::RasterAllocationFailed(error.to_string()))?;
    values.resize(len, T::default());
    Ok(values)
}

fn try_copy_bytes(d: &[u8]) -> Result<Vec<u8>, GeotiffError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(d.len())
        .map_err(|error| GeotiffError::RasterAllocationFailed(error.to_string()))?;
    values.extend_from_slice(d);
    Ok(values)
}

fn bytes_as<T>(
    d: &[u8],
    element_size: usize,
    mut decode: impl FnMut(&[u8]) -> T,
) -> Result<Vec<T>, GeotiffError> {
    if !d.len().is_multiple_of(element_size) {
        return Err(GeotiffError::InvalidTypedArrayByteLength {
            length: d.len(),
            element_size,
        });
    }
    let mut values = Vec::new();
    values
        .try_reserve_exact(d.len() / element_size)
        .map_err(|error| GeotiffError::RasterAllocationFailed(error.to_string()))?;
    values.extend(d.chunks_exact(element_size).map(&mut decode));
    Ok(values)
}

fn bytes_u16(d: &[u8]) -> Result<Vec<u16>, GeotiffError> {
    bytes_as(d, 2, |c| u16::from_le_bytes([c[0], c[1]]))
}
fn bytes_i16(d: &[u8]) -> Result<Vec<i16>, GeotiffError> {
    bytes_as(d, 2, |c| i16::from_le_bytes([c[0], c[1]]))
}
fn bytes_u32(d: &[u8]) -> Result<Vec<u32>, GeotiffError> {
    bytes_as(d, 4, |c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
}
fn bytes_i32(d: &[u8]) -> Result<Vec<i32>, GeotiffError> {
    bytes_as(d, 4, |c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
}
fn bytes_f32(d: &[u8]) -> Result<Vec<f32>, GeotiffError> {
    bytes_as(d, 4, |c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
}
fn bytes_f16_as_f32(d: &[u8]) -> Result<Vec<f32>, GeotiffError> {
    bytes_as(d, 2, |c| f16::from_le_bytes([c[0], c[1]]).to_f32())
}
fn bytes_f64(d: &[u8]) -> Result<Vec<f64>, GeotiffError> {
    bytes_as(d, 8, |c| {
        f64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]])
    })
}

/// `arrayForType(format, bitsPerSample, sizeOrData)`. `format`: 1 =
/// unsigned integer, 2 = two's-complement signed integer, 3 = floating
/// point (matches `SampleFormat` TIFF tag values). Byte-reinterpretation of
/// the `Data` case assumes little-endian element layout, matching how a JS
/// typed array view over an `ArrayBuffer` reads multi-byte elements on
/// every real-world engine/platform.
pub fn array_for_type(
    format: u8,
    bits_per_sample: u32,
    size_or_data: SizeOrData,
) -> Result<TypedArray, GeotiffError> {
    match (format, bits_per_sample) {
        (1, b) if b <= 8 => Ok(TypedArray::Uint8(match size_or_data {
            SizeOrData::Size(n) => try_zeroed(n)?,
            SizeOrData::Data(d) => try_copy_bytes(d)?,
        })),
        (1, b) if b <= 16 => Ok(TypedArray::Uint16(match size_or_data {
            SizeOrData::Size(n) => try_zeroed(n)?,
            SizeOrData::Data(d) => bytes_u16(d)?,
        })),
        (1, b) if b <= 32 => Ok(TypedArray::Uint32(match size_or_data {
            SizeOrData::Size(n) => try_zeroed(n)?,
            SizeOrData::Data(d) => bytes_u32(d)?,
        })),
        (2, 8) => Ok(TypedArray::Int8(match size_or_data {
            SizeOrData::Size(n) => try_zeroed(n)?,
            SizeOrData::Data(d) => bytes_as(d, 1, |c| c[0] as i8)?,
        })),
        (2, 16) => Ok(TypedArray::Int16(match size_or_data {
            SizeOrData::Size(n) => try_zeroed(n)?,
            SizeOrData::Data(d) => bytes_i16(d)?,
        })),
        (2, 32) => Ok(TypedArray::Int32(match size_or_data {
            SizeOrData::Size(n) => try_zeroed(n)?,
            SizeOrData::Data(d) => bytes_i32(d)?,
        })),
        // (3, 16): geotiff.js's arrayForType picks Float32Array for 16-bit
        // float samples too (widening, not a 2-byte-per-element container) -
        // the `Data` case must chunk source bytes by 2 (half-precision) and
        // widen each to f32, not reuse the 32-bit-float 4-byte chunking
        // (`bytes_f32`) as if the bytes were already f32-sized; doing so
        // would silently misinterpret every sample.
        (3, 16) => Ok(TypedArray::Float32(match size_or_data {
            SizeOrData::Size(n) => try_zeroed(n)?,
            SizeOrData::Data(d) => bytes_f16_as_f32(d)?,
        })),
        (3, 32) => Ok(TypedArray::Float32(match size_or_data {
            SizeOrData::Size(n) => try_zeroed(n)?,
            SizeOrData::Data(d) => bytes_f32(d)?,
        })),
        (3, 64) => Ok(TypedArray::Float64(match size_or_data {
            SizeOrData::Size(n) => try_zeroed(n)?,
            SizeOrData::Data(d) => bytes_f64(d)?,
        })),
        _ => Err(GeotiffError::UnsupportedDataFormat(format, bits_per_sample)),
    }
}

/// `needsNormalization(format, bitsPerSample)`
pub fn needs_normalization(format: u8, bits_per_sample: u32) -> bool {
    if (format == 1 || format == 2) && bits_per_sample <= 32 && bits_per_sample.is_multiple_of(8) {
        return false;
    }
    if format == 3 && (bits_per_sample == 16 || bits_per_sample == 32 || bits_per_sample == 64) {
        return false;
    }
    true
}

use crate::block;
use crate::decode_pool::CancellationToken;
use crate::geo::{get_bounding_box, get_origin, get_resolution};
use crate::geokeys::GeoKeys;
use crate::imagefiledirectory::{FileDirectory, IfdValue};
use crate::raster::{ImageWindow, PackedSampleMode, Raster, RasterBands};
use crate::readrgb;
use async_tiff::ImageFileDirectory;
use async_tiff::decoder::DecoderRegistry;
use async_tiff::error::{AsyncTiffError, AsyncTiffResult};
use async_tiff::geo::GeoKeyDirectory as AsyncGeoKeyDirectory;
use async_tiff::reader::{AsyncFileReader, Endianness};
use async_tiff::tags::{ExtraSamples, PhotometricInterpretation, SampleFormat};
use std::collections::BTreeMap;
use std::sync::Arc;

/// geotiff.js `fillValue`: either one value for every selected sample or
/// one value per output band.
#[derive(Debug, Clone, PartialEq)]
pub enum FillValue {
    Scalar(f64),
    PerSample(Vec<f64>),
}

/// Complete option set of `GeoTIFFImage.readRasters` in Rust form.
#[derive(Debug, Clone)]
pub struct ReadRastersOptions {
    pub window: Option<ImageWindow>,
    pub samples: Vec<usize>,
    pub interleave: bool,
    pub width: Option<usize>,
    pub height: Option<usize>,
    pub resample_method: String,
    pub fill_value: Option<FillValue>,
    /// `Lossless` preserves packed samples. `GeotiffJs` is available for
    /// applications that must reproduce geotiff.js's historical packed
    /// chunky sample-offset behavior byte for byte.
    pub packed_sample_mode: PackedSampleMode,
    /// Native equivalent of geotiff.js's per-call `pool` option. When set,
    /// this registry overrides the one configured when the TIFF was opened.
    pub decoder_registry: Option<Arc<DecoderRegistry>>,
    pub cancellation: Option<CancellationToken>,
}

impl Default for ReadRastersOptions {
    fn default() -> Self {
        Self {
            window: None,
            samples: Vec::new(),
            // geotiff.js only enables interleaving when the option exists
            // and is truthy; omitted means separate arrays.
            interleave: false,
            width: None,
            height: None,
            resample_method: "nearest".to_string(),
            fill_value: None,
            packed_sample_mode: PackedSampleMode::Lossless,
            decoder_registry: None,
            cancellation: None,
        }
    }
}

/// The two result shapes selected by `ReadRastersOptions::interleave`.
#[derive(Debug)]
pub enum ReadRasterResult {
    Interleaved(Raster),
    Bands(RasterBands),
}

impl ReadRasterResult {
    pub fn width(&self) -> usize {
        match self {
            Self::Interleaved(raster) => raster.width,
            Self::Bands(raster) => raster.width,
        }
    }

    pub fn height(&self) -> usize {
        match self {
            Self::Interleaved(raster) => raster.height,
            Self::Bands(raster) => raster.height,
        }
    }
}

/// Complete option set of `GeoTIFFImage.readRGB` in Rust form.
#[derive(Debug, Clone)]
pub struct ReadRgbOptions {
    pub window: Option<ImageWindow>,
    pub interleave: bool,
    pub width: Option<usize>,
    pub height: Option<usize>,
    pub resample_method: String,
    pub enable_alpha: bool,
    /// Packed-sample policy forwarded to the internal raster read.
    pub packed_sample_mode: PackedSampleMode,
    /// Native equivalent of geotiff.js's per-call `pool` option.
    pub decoder_registry: Option<Arc<DecoderRegistry>>,
    pub cancellation: Option<CancellationToken>,
}

impl Default for ReadRgbOptions {
    fn default() -> Self {
        Self {
            window: None,
            interleave: false,
            width: None,
            height: None,
            resample_method: "nearest".to_string(),
            enable_alpha: false,
            packed_sample_mode: PackedSampleMode::Lossless,
            decoder_registry: None,
            cancellation: None,
        }
    }
}

/// One decoded block returned by `get_tile_or_strip`.
#[derive(Debug, Clone)]
pub struct RasterBlock {
    pub x: usize,
    pub y: usize,
    pub sample: usize,
    pub width: usize,
    pub height: usize,
    /// Predictor-reversed and, when required, normalized byte buffer matching
    /// geotiff.js `getTileOrStrip(...).data` exactly.
    pub data: Vec<u8>,
    /// Native typed view of the requested sample plane.
    pub sample_data: TypedArray,
    /// Every decoded sample plane. For chunky TIFFs this retains the samples
    /// that are present in geotiff.js's returned block buffer; for planar
    /// TIFFs it also avoids discarding already-decoded sibling planes.
    pub bands: Vec<TypedArray>,
}

/// A single model tie point (`GeoTIFFImage.getTiePoints`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TiePoint {
    pub i: f64,
    pub j: f64,
    pub k: f64,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// Borrowed view corresponding to geotiff.js's exported `GeoTIFFImage`
/// class. It keeps the IFD, reader, decoder registry, and byte order
/// together so every image-level method is available from one object.
pub struct GeoTiffImage<'a> {
    ifd: &'a ImageFileDirectory,
    reader: &'a dyn AsyncFileReader,
    registry: Arc<DecoderRegistry>,
    endianness: Endianness,
    file_directory: &'a FileDirectory,
    decoded_cache: Option<&'a block::DecodedBlockCache>,
}

impl<'a> GeoTiffImage<'a> {
    pub(crate) fn new(
        ifd: &'a ImageFileDirectory,
        reader: &'a dyn AsyncFileReader,
        registry: Arc<DecoderRegistry>,
        endianness: Endianness,
        file_directory: &'a FileDirectory,
        decoded_cache: Option<&'a block::DecodedBlockCache>,
    ) -> Self {
        Self {
            ifd,
            reader,
            registry,
            endianness,
            file_directory,
            decoded_cache,
        }
    }

    /// `getFileDirectory()`: lossless, name/number-addressable metadata.
    pub fn file_directory(&self) -> &FileDirectory {
        self.file_directory
    }

    /// Internal async-tiff view used for tile planning and native codecs.
    pub fn async_tiff_file_directory(&self) -> &ImageFileDirectory {
        self.ifd
    }

    /// `getGeoKeys()`: the complete directory, including vendor keys and
    /// valid IDs not represented by async-tiff's typed structure.
    pub fn geo_keys(&self) -> Option<&GeoKeys> {
        self.file_directory.parse_geo_key_directory()
    }

    /// The dependency's recognized, strongly typed subset. Prefer
    /// [`Self::geo_keys`] when lossless metadata transfer matters.
    pub fn async_tiff_geo_keys(&self) -> Option<&AsyncGeoKeyDirectory> {
        self.ifd.geo_key_directory()
    }

    pub fn width(&self) -> usize {
        self.file_directory
            .get_value(256u16)
            .and_then(IfdValue::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0)
    }

    pub fn height(&self) -> usize {
        self.file_directory
            .get_value(257u16)
            .and_then(IfdValue::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0)
    }

    pub fn samples_per_pixel(&self) -> usize {
        self.file_directory
            .get_value(277u16)
            .and_then(IfdValue::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(1)
    }

    pub fn is_tiled(&self) -> bool {
        !self.file_directory.has_tag(273u16)
    }

    pub fn planar_configuration(&self) -> u16 {
        self.ifd.planar_configuration().to_u16()
    }

    pub fn little_endian(&self) -> bool {
        self.endianness == Endianness::LittleEndian
    }

    pub fn tile_width(&self) -> usize {
        if self.is_tiled() {
            self.file_directory
                .get_value(322u16)
                .and_then(|value| value.as_u64())
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(0)
        } else {
            self.width()
        }
    }

    pub fn tile_height(&self) -> usize {
        if self.is_tiled() {
            self.file_directory
                .get_value(323u16)
                .and_then(|value| value.as_u64())
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(0)
        } else {
            self.file_directory
                .get_value(278u16)
                .and_then(|value| value.as_u64())
                .and_then(|value| usize::try_from(value).ok())
                .filter(|value| *value != 0)
                .unwrap_or_else(|| self.height())
                .min(self.height())
        }
    }

    pub fn block_width(&self) -> usize {
        self.tile_width()
    }

    pub fn block_height(&self, y: usize) -> usize {
        let tile_height = self.tile_height();
        let block_end = y
            .checked_add(1)
            .and_then(|value| value.checked_mul(tile_height));
        if self.ifd.tile_count().is_some() || block_end.is_some_and(|end| end <= self.height()) {
            self.tile_height()
        } else {
            self.height().saturating_sub(y.saturating_mul(tile_height))
        }
    }

    pub fn sample_byte_size(&self, sample: usize) -> AsyncTiffResult<usize> {
        self.file_directory
            .get_value(258u16)
            .and_then(|value| match value {
                IfdValue::Unsigned(value) if sample == 0 => Some(*value),
                IfdValue::UnsignedArray(values) => values.get(sample).copied(),
                _ => None,
            })
            .and_then(|value| usize::try_from(value).ok())
            .map(|value| value.div_ceil(8))
            .ok_or_else(|| {
                AsyncTiffError::General(format!("Sample index {sample} is out of range."))
            })
    }

    pub fn bytes_per_pixel(&self) -> usize {
        let count = self
            .file_directory
            .get_value(258u16)
            .map(IfdValue::len)
            .unwrap_or(0);
        (0..count)
            .filter_map(|sample| self.sample_byte_size(sample).ok())
            .sum()
    }

    pub fn sample_format(&self, sample: usize) -> AsyncTiffResult<SampleFormat> {
        block::sample_formats(self.ifd)
            .get(sample)
            .copied()
            .ok_or_else(|| {
                AsyncTiffError::General(format!("Sample index {sample} is out of range."))
            })
    }

    pub fn reader_for_sample(&self, sample: usize) -> AsyncTiffResult<SampleReader> {
        let unsupported =
            || AsyncTiffError::General("Unsupported data format/bitsPerSample".to_string());
        // The exported JS helper intentionally reports the generic data
        // format error for an out-of-range sample (readRasters itself has a
        // more specific sample-index check). Preserve that method-level
        // distinction here.
        let format = self.sample_format(sample).map_err(|_| unsupported())?;
        let bits = self.bits_per_sample(sample).map_err(|_| unsupported())?;
        match (format, bits) {
            (SampleFormat::Uint, 0..=8) => Ok(SampleReader::Uint8),
            (SampleFormat::Uint, 9..=16) => Ok(SampleReader::Uint16),
            (SampleFormat::Uint, 17..=32) => Ok(SampleReader::Uint32),
            (SampleFormat::Int, 0..=8) => Ok(SampleReader::Int8),
            (SampleFormat::Int, 9..=16) => Ok(SampleReader::Int16),
            (SampleFormat::Int, 17..=32) => Ok(SampleReader::Int32),
            (SampleFormat::Float, 16) => Ok(SampleReader::Float16),
            (SampleFormat::Float, 32) => Ok(SampleReader::Float32),
            (SampleFormat::Float, 64) => Ok(SampleReader::Float64),
            _ => Err(AsyncTiffError::General(
                "Unsupported data format/bitsPerSample".to_string(),
            )),
        }
    }

    pub fn bits_per_sample(&self, sample: usize) -> AsyncTiffResult<u16> {
        block::sample_bits(self.ifd)
            .get(sample)
            .copied()
            .ok_or_else(|| {
                AsyncTiffError::General(format!("Sample index {sample} is out of range."))
            })
    }

    pub fn array_for_sample(&self, sample: usize, len: usize) -> AsyncTiffResult<TypedArray> {
        let unsupported =
            || AsyncTiffError::General("Unsupported data format/bitsPerSample".to_string());
        let format = self.sample_format(sample).map_err(|_| unsupported())?;
        let bits = self.bits_per_sample(sample).map_err(|_| unsupported())?;
        block::typed_array_for(format, bits, len).map_err(|error| match error {
            AsyncTiffError::General(message)
                if message.starts_with("Unsupported data format/bitsPerSample") =>
            {
                unsupported()
            }
            other => other,
        })
    }

    pub fn array_for_sample_from(
        &self,
        sample: usize,
        size_or_data: SizeOrData<'_>,
    ) -> AsyncTiffResult<TypedArray> {
        let unsupported =
            || AsyncTiffError::General("Unsupported data format/bitsPerSample".to_string());
        let format = match self.sample_format(sample).map_err(|_| unsupported())? {
            SampleFormat::Uint => 1,
            SampleFormat::Int => 2,
            SampleFormat::Float => 3,
            _ => return Err(unsupported()),
        };
        array_for_type(
            format,
            u32::from(self.bits_per_sample(sample).map_err(|_| unsupported())?),
            size_or_data,
        )
        .map_err(|error| AsyncTiffError::General(error.to_string()))
    }

    pub async fn get_tile_or_strip(
        &self,
        x: usize,
        y: usize,
        sample: usize,
        cancellation: Option<&CancellationToken>,
    ) -> AsyncTiffResult<RasterBlock> {
        self.get_tile_or_strip_with_registry(x, y, sample, self.registry.clone(), cancellation)
            .await
    }

    /// `getTileOrStrip(x, y, sample, poolOrDecoder, signal)`: reads one
    /// block with a call-specific decoder registry. The shorter
    /// [`Self::get_tile_or_strip`] form uses the registry configured when
    /// the dataset was opened.
    pub async fn get_tile_or_strip_with_registry(
        &self,
        x: usize,
        y: usize,
        sample: usize,
        registry: Arc<DecoderRegistry>,
        cancellation: Option<&CancellationToken>,
    ) -> AsyncTiffResult<RasterBlock> {
        if sample >= self.samples_per_pixel() {
            return Err(AsyncTiffError::General(format!(
                "Invalid sample index '{sample}'."
            )));
        }
        let decoded = if self.ifd.tile_count().is_some() {
            let (tiles_across, tiles_down) = self.ifd.tile_count().ok_or_else(|| {
                AsyncTiffError::General("Invalid tiled TIFF dimensions".to_string())
            })?;
            // async-tiff indexes TileOffsets/TileByteCounts before it turns an
            // invalid coordinate into a Result, which panics for hostile
            // public getTileOrStrip input. geotiff.js surfaces a DataView
            // bounds error for the same call; retain that observable message
            // while guaranteeing native memory safety.
            if x >= tiles_across || y >= tiles_down {
                return Err(AsyncTiffError::General(
                    "Offset is outside the bounds of the DataView".to_string(),
                ));
            }
            block::fetch_tile_cached(
                self.decoded_cache,
                self.ifd,
                (x, y),
                self.reader,
                self.endianness,
                registry.clone(),
                cancellation,
            )
            .await?
        } else {
            if x != 0 {
                return Err(AsyncTiffError::General(
                    "Striped TIFF blocks always have x = 0".to_string(),
                ));
            }
            let rows_per_strip = self.tile_height();
            if rows_per_strip == 0 || y >= self.height().div_ceil(rows_per_strip) {
                return Err(AsyncTiffError::General(
                    "Offset is outside the bounds of the DataView".to_string(),
                ));
            }
            block::fetch_strip_cached(
                self.decoded_cache,
                self.ifd,
                y,
                self.reader,
                self.endianness,
                registry,
                cancellation,
            )
            .await?
        };
        let data = decoded.javascript_data(sample)?;
        let sample_data = decoded.bands[sample].clone();
        Ok(RasterBlock {
            x,
            y,
            sample,
            width: decoded.width,
            height: decoded.height,
            data,
            sample_data,
            bands: decoded.bands.clone(),
        })
    }

    pub async fn read_rasters(
        &self,
        mut options: ReadRastersOptions,
    ) -> AsyncTiffResult<ReadRasterResult> {
        if options.samples.is_empty() {
            options.samples = (0..self.samples_per_pixel()).collect();
        }
        for &sample in &options.samples {
            if sample >= self.samples_per_pixel() {
                return Err(AsyncTiffError::General(format!(
                    "Invalid sample index '{sample}'."
                )));
            }
        }
        let window = options
            .window
            .unwrap_or_else(|| ImageWindow::full(self.ifd));
        let registry = options
            .decoder_registry
            .unwrap_or_else(|| self.registry.clone());
        let cancellation = options.cancellation.as_ref();

        if options.interleave {
            let fill = match options.fill_value {
                Some(FillValue::Scalar(value)) if value != 0.0 && !value.is_nan() => Some(value),
                Some(FillValue::Scalar(_)) => None,
                Some(FillValue::PerSample(_)) => {
                    return Err(AsyncTiffError::General(
                        "When reading interleaved data, fillValue must be a single number."
                            .to_string(),
                    ));
                }
                None => None,
            };
            let raster = crate::raster::read_rasters_interleaved_window_with_fill_and_cache(
                self.ifd,
                self.reader,
                registry,
                &options.samples,
                window,
                self.endianness,
                cancellation,
                fill,
                self.decoded_cache,
                options.packed_sample_mode,
            )
            .await?;
            let raster = crate::raster::resize_raster(
                raster,
                options.width,
                options.height,
                &options.resample_method,
            )
            .map_err(|error| AsyncTiffError::General(error.to_string()))?;
            Ok(ReadRasterResult::Interleaved(raster))
        } else {
            let fill_storage = match options.fill_value {
                Some(FillValue::Scalar(value)) if value != 0.0 && !value.is_nan() => {
                    Some(vec![value; options.samples.len()])
                }
                Some(FillValue::Scalar(_)) => None,
                Some(FillValue::PerSample(values)) => Some(values),
                None => None,
            };
            let raster = crate::raster::read_rasters_window_with_fill_and_cache(
                self.ifd,
                self.reader,
                registry,
                &options.samples,
                window,
                self.endianness,
                cancellation,
                fill_storage.as_deref(),
                self.decoded_cache,
                options.packed_sample_mode,
            )
            .await?;
            let raster = crate::raster::resize_raster_bands(
                raster,
                options.width,
                options.height,
                &options.resample_method,
            )
            .map_err(|error| AsyncTiffError::General(error.to_string()))?;
            Ok(ReadRasterResult::Bands(raster))
        }
    }

    pub async fn read_rgb(&self, options: ReadRgbOptions) -> AsyncTiffResult<ReadRasterResult> {
        let registry = options
            .decoder_registry
            .clone()
            .unwrap_or_else(|| self.registry.clone());
        let photometric = match self.file_directory.get_value(262u16) {
            Some(IfdValue::Unsigned(value)) => u16::try_from(*value).ok(),
            _ => None,
        };
        if !photometric.is_some_and(|value| matches!(value, 0 | 1 | 2 | 3 | 5 | 6 | 8)) {
            return Err(AsyncTiffError::General(
                "Invalid or unsupported photometric interpretation.".to_string(),
            ));
        }
        if !options.interleave
            && self.ifd.photometric_interpretation() == PhotometricInterpretation::RGB
        {
            let has_real_extra_samples = self
                .ifd
                .extra_samples()
                .is_some_and(|extra| extra.first() != Some(&ExtraSamples::Unspecified));
            let samples = if options.enable_alpha && has_real_extra_samples {
                (0..self.ifd.bits_per_sample().len()).collect()
            } else {
                vec![0, 1, 2]
            };
            return self
                .read_rasters(ReadRastersOptions {
                    window: options.window,
                    samples,
                    interleave: false,
                    width: options.width,
                    height: options.height,
                    resample_method: options.resample_method,
                    fill_value: None,
                    packed_sample_mode: options.packed_sample_mode,
                    decoder_registry: Some(registry),
                    cancellation: options.cancellation,
                })
                .await;
        }

        let raster = readrgb::read_rgb_with_cache(
            self.ifd,
            self.reader,
            registry,
            options.window,
            options.width,
            options.height,
            &options.resample_method,
            options.enable_alpha,
            options.packed_sample_mode,
            self.endianness,
            options.cancellation.as_ref(),
            self.decoded_cache,
        )
        .await?;
        if options.interleave {
            Ok(ReadRasterResult::Interleaved(raster))
        } else {
            Ok(ReadRasterResult::Bands(split_interleaved(raster)?))
        }
    }

    pub fn tie_points(&self) -> AsyncTiffResult<Vec<TiePoint>> {
        let values = self.ifd.model_tiepoint().unwrap_or_default();
        if !values.len().is_multiple_of(6) {
            return Err(AsyncTiffError::General(format!(
                "Expected ModelTiepoint to contain groups of 6 values, got {}",
                values.len()
            )));
        }
        Ok(values
            .chunks_exact(6)
            .map(|values| TiePoint {
                i: values[0],
                j: values[1],
                k: values[2],
                x: values[3],
                y: values[4],
                z: values[5],
            })
            .collect())
    }

    pub fn gdal_metadata(
        &self,
        sample: Option<usize>,
    ) -> AsyncTiffResult<Option<BTreeMap<String, String>>> {
        let Some(xml) = self.ifd.gdal_metadata() else {
            return Ok(None);
        };
        let xml = xml.trim_end_matches('\0');
        Ok(Some(parse_gdal_metadata(xml, sample)?))
    }

    pub fn gdal_nodata(&self) -> Option<f64> {
        let IfdValue::Ascii(value) = self.file_directory.get_value(42113u16)? else {
            return None;
        };
        if value.is_empty() {
            return None;
        }
        let end = value
            .char_indices()
            .next_back()
            .map(|(index, _)| index)
            .unwrap_or(0);
        Some(crate::utils::parse_js_number(&value[..end]))
    }

    pub fn origin(&self) -> Result<[f64; 3], GeotiffError> {
        get_origin(self.ifd)
    }

    pub fn resolution(
        &self,
        reference: Option<&GeoTiffImage<'_>>,
    ) -> Result<[f64; 3], GeotiffError> {
        get_resolution(self.ifd, reference.map(|image| image.ifd))
    }

    pub fn pixel_is_area(&self) -> bool {
        self.geo_keys()
            .and_then(|keys| keys.get(1025))
            .and_then(|value| value.as_u16())
            .or_else(|| {
                self.ifd
                    .geo_key_directory()
                    .and_then(|keys| keys.raster_type)
            })
            == Some(1)
    }

    pub fn bounding_box(&self, tilegrid: bool) -> Result<[f64; 4], GeotiffError> {
        get_bounding_box(self.ifd, tilegrid)
    }
}

fn parse_gdal_metadata(
    xml: &str,
    sample: Option<usize>,
) -> AsyncTiffResult<BTreeMap<String, String>> {
    let document = roxmltree::Document::parse(xml)
        .map_err(|error| AsyncTiffError::General(format!("Invalid GDAL_METADATA XML: {error}")))?;
    let mut metadata = BTreeMap::new();
    for item in document
        .descendants()
        .filter(|node| node.has_tag_name("Item"))
    {
        let matches_sample = match sample {
            // JS distinguishes an absent attribute from an invalid one for
            // dataset-level metadata (`undefined`, not `Number(value)`).
            None => item.attribute("sample").is_none(),
            Some(sample) => item
                .attribute("sample")
                .is_some_and(|value| crate::utils::parse_js_number(value) == sample as f64),
        };
        if !matches_sample {
            continue;
        }
        if let Some(name) = item.attribute("name") {
            metadata.insert(
                name.to_string(),
                item.text().unwrap_or_default().to_string(),
            );
        }
    }
    Ok(metadata)
}

fn split_interleaved(raster: Raster) -> AsyncTiffResult<RasterBands> {
    let samples = raster.samples_per_pixel;
    let pixels = raster
        .width
        .checked_mul(raster.height)
        .ok_or_else(|| AsyncTiffError::General("RGB output pixel count overflow".to_string()))?;
    let mut bands = (0..samples)
        .map(|_| {
            let allocated = match &raster.data {
                // geotiff.js splits a converted Uint8ClampedArray into three
                // ordinary Uint8Array bands when interleave is false.
                TypedArray::Uint8Clamped(_) => TypedArray::Uint8(Vec::new()).try_new_zeroed(pixels),
                _ => raster.data.try_new_zeroed(pixels),
            };
            allocated.map_err(|error| {
                AsyncTiffError::General(format!("Could not allocate RGB output band: {error}"))
            })
        })
        .collect::<AsyncTiffResult<Vec<_>>>()?;
    for pixel in 0..pixels {
        for (sample, band) in bands.iter_mut().enumerate() {
            band.set_f64(pixel, raster.data.get_f64(pixel * samples + sample));
        }
    }
    Ok(RasterBands {
        bands,
        width: raster.width,
        height: raster.height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sum_adds_over_the_given_range() {
        let a = TypedArray::Uint8(vec![1, 2, 3, 4, 5]);
        assert_eq!(sum(&a, 1, 4), 9.0); // 2+3+4
    }

    #[test]
    fn array_for_type_picks_the_right_variant_by_size() {
        assert!(
            matches!(array_for_type(1, 8, SizeOrData::Size(3)).unwrap(), TypedArray::Uint8(v) if v.len() == 3)
        );
        assert!(
            matches!(array_for_type(1, 16, SizeOrData::Size(3)).unwrap(), TypedArray::Uint16(v) if v.len() == 3)
        );
        assert!(
            matches!(array_for_type(2, 32, SizeOrData::Size(2)).unwrap(), TypedArray::Int32(v) if v.len() == 2)
        );
        assert!(
            matches!(array_for_type(3, 64, SizeOrData::Size(1)).unwrap(), TypedArray::Float64(v) if v.len() == 1)
        );
        assert!(array_for_type(9, 8, SizeOrData::Size(1)).is_err());
    }

    #[test]
    fn array_for_type_widens_16_bit_float_bytes_not_reads_them_as_32_bit() {
        // Two f16 values (2 bytes each, 4 bytes total) must produce two f32
        // outputs, not be misread as a single 4-byte f32.
        let bytes: Vec<u8> = [1.5f32, -2.25]
            .iter()
            .flat_map(|&v| half::f16::from_f32(v).to_le_bytes())
            .collect();
        let result = array_for_type(3, 16, SizeOrData::Data(&bytes)).unwrap();
        assert_eq!(result, TypedArray::Float32(vec![1.5, -2.25]));
    }

    #[test]
    fn array_for_type_reinterprets_bytes_little_endian() {
        let bytes = 0x1234u16.to_le_bytes();
        let result = array_for_type(1, 16, SizeOrData::Data(&bytes)).unwrap();
        assert_eq!(result, TypedArray::Uint16(vec![0x1234]));
    }

    #[test]
    fn array_for_type_rejects_misaligned_or_unallocatable_buffers() {
        let misaligned = array_for_type(1, 16, SizeOrData::Data(&[1, 2, 3])).unwrap_err();
        assert!(matches!(
            misaligned,
            GeotiffError::InvalidTypedArrayByteLength {
                length: 3,
                element_size: 2
            }
        ));
        assert!(array_for_type(3, 64, SizeOrData::Size(usize::MAX)).is_err());
    }

    #[test]
    fn needs_normalization_matches_native_byte_aligned_formats() {
        assert!(!needs_normalization(1, 8));
        assert!(!needs_normalization(1, 16));
        assert!(!needs_normalization(3, 32));
        assert!(needs_normalization(1, 12)); // not byte-aligned
        assert!(needs_normalization(3, 8)); // not a valid float width
    }

    #[test]
    fn gdal_metadata_distinguishes_missing_invalid_and_numeric_sample_attributes() {
        let xml = r#"<GDALMetadata>
            <Item name="dataset">root</Item>
            <Item name="invalid" sample="not-a-number">bad</Item>
            <Item name="empty" sample="">zero</Item>
            <Item name="whitespace" sample="  ">also zero</Item>
            <Item name="band" sample="2">value</Item>
            <Item name="hex-band" sample="0x2">hex value</Item>
        </GDALMetadata>"#;
        let dataset = parse_gdal_metadata(xml, None).unwrap();
        assert_eq!(dataset.len(), 1);
        assert_eq!(dataset.get("dataset").map(String::as_str), Some("root"));
        let zero = parse_gdal_metadata(xml, Some(0)).unwrap();
        assert_eq!(zero.len(), 2);
        assert_eq!(zero.get("empty").map(String::as_str), Some("zero"));
        assert_eq!(
            zero.get("whitespace").map(String::as_str),
            Some("also zero")
        );
        let band = parse_gdal_metadata(xml, Some(2)).unwrap();
        assert_eq!(band.len(), 2);
        assert_eq!(band.get("band").map(String::as_str), Some("value"));
        assert_eq!(band.get("hex-band").map(String::as_str), Some("hex value"));
    }
}
