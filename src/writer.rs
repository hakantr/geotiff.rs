//! Native port of `geotiffwriter.js`.
//!
//! The writer deliberately keeps the wire-level choices made by geotiff.js:
//! classic (non-BigTIFF), big-endian TIFF, a single IFD, and pixel data that
//! starts at byte 1000.  The Rust API replaces JavaScript's dynamic metadata
//! object with explicitly typed tag and GeoKey maps, while preserving the
//! same defaults and tiled/striped data ordering.

use crate::typed_array::TypedArray;
use std::collections::BTreeMap;
use std::fmt;

/// The fixed IFD reservation used by geotiff.js.
pub const IFD_RESERVE_BYTES: usize = 1000;

/// Pixel-payload policy for the writer.
///
/// geotiff.js 3.1.0 does not include signed typed arrays (or
/// `Uint8ClampedArray`) in its writer `typeMap`. It consequently allocates
/// eight bytes per value and stores only each value's low byte. The default
/// keeps the native writer lossless; `GeotiffJs` reproduces that historical
/// wire output when an application must compare or migrate existing files
/// byte-for-byte.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WriterCompatibility {
    #[default]
    Lossless,
    GeotiffJs,
}

/// Input accepted by [`write_geotiff`] and [`write_array_buffer`].
#[derive(Debug, Clone, PartialEq)]
pub enum WriterData {
    /// A flat, pixel-interleaved typed array (or already tile/strip ordered
    /// data when the corresponding byte-count tags are supplied).
    Typed(TypedArray),
    /// JavaScript plain-array equivalent. Values are written as 8-bit
    /// samples; the output retains geotiff.js's eight-byte-per-element file
    /// allocation for wire compatibility.
    Numbers(Vec<f64>),
    /// `[band][row][column]` input. It is flattened pixel-major/interleaved,
    /// exactly as `writeGeotiff` does.
    Nested(Vec<Vec<Vec<f64>>>),
}

impl WriterData {
    /// Construct nested writer data from any numeric type convertible to
    /// `f64`.
    pub fn from_bands<T>(bands: Vec<Vec<Vec<T>>>) -> Self
    where
        T: Copy + Into<f64>,
    {
        WriterData::Nested(
            bands
                .into_iter()
                .map(|band| {
                    band.into_iter()
                        .map(|row| row.into_iter().map(Into::into).collect())
                        .collect()
                })
                .collect(),
        )
    }
}

impl From<TypedArray> for WriterData {
    fn from(value: TypedArray) -> Self {
        WriterData::Typed(value)
    }
}

macro_rules! typed_data_from_vec {
    ($ty:ty, $variant:ident) => {
        impl From<Vec<$ty>> for WriterData {
            fn from(value: Vec<$ty>) -> Self {
                WriterData::Typed(TypedArray::$variant(value))
            }
        }
    };
}

typed_data_from_vec!(i8, Int8);
typed_data_from_vec!(u8, Uint8);
typed_data_from_vec!(i16, Int16);
typed_data_from_vec!(u16, Uint16);
typed_data_from_vec!(i32, Int32);
typed_data_from_vec!(u32, Uint32);
typed_data_from_vec!(i64, Int64);
typed_data_from_vec!(u64, Uint64);
typed_data_from_vec!(f32, Float32);

impl From<Vec<f64>> for WriterData {
    fn from(value: Vec<f64>) -> Self {
        // A Vec<f64> is the closest Rust spelling of a JavaScript number[],
        // while TypedArray::Float64 is available when Float64Array semantics
        // are wanted explicitly.
        WriterData::Numbers(value)
    }
}

/// Value of a TIFF metadata tag accepted by the JS writer.
#[derive(Debug, Clone, PartialEq)]
pub enum WriterValue {
    Numbers(Vec<f64>),
    Ascii(String),
}

impl WriterValue {
    pub fn numbers(values: impl IntoIterator<Item = impl Into<f64>>) -> Self {
        WriterValue::Numbers(values.into_iter().map(Into::into).collect())
    }
}

impl From<String> for WriterValue {
    fn from(value: String) -> Self {
        WriterValue::Ascii(value)
    }
}

impl From<&str> for WriterValue {
    fn from(value: &str) -> Self {
        WriterValue::Ascii(value.to_owned())
    }
}

macro_rules! writer_value_scalar {
    ($($ty:ty),+ $(,)?) => {$(
        impl From<$ty> for WriterValue {
            fn from(value: $ty) -> Self {
                WriterValue::Numbers(vec![value as f64])
            }
        }
    )+};
}

macro_rules! writer_value_vec {
    ($($ty:ty),+ $(,)?) => {$(
        impl From<Vec<$ty>> for WriterValue {
            fn from(value: Vec<$ty>) -> Self {
                WriterValue::Numbers(value.into_iter().map(|v| v as f64).collect())
            }
        }
    )+};
}

writer_value_scalar!(u8, u16, u32, usize, i8, i16, i32, f32, f64);
writer_value_vec!(u8, u16, u32, usize, i8, i16, i32, f32, f64);

/// A GeoKey directory value. This explicit representation also permits
/// GeoKeys beyond the small subset that geotiff.js's `fieldTagTypes` table
/// knows about.
#[derive(Debug, Clone, PartialEq)]
pub enum GeoKeyValue {
    Short(u16),
    Ascii(String),
    Double(Vec<f64>),
}

impl From<u16> for GeoKeyValue {
    fn from(value: u16) -> Self {
        GeoKeyValue::Short(value)
    }
}

impl From<String> for GeoKeyValue {
    fn from(value: String) -> Self {
        GeoKeyValue::Ascii(value)
    }
}

impl From<&str> for GeoKeyValue {
    fn from(value: &str) -> Self {
        GeoKeyValue::Ascii(value.to_owned())
    }
}

impl From<f64> for GeoKeyValue {
    fn from(value: f64) -> Self {
        GeoKeyValue::Double(vec![value])
    }
}

impl From<Vec<f64>> for GeoKeyValue {
    fn from(value: Vec<f64>) -> Self {
        GeoKeyValue::Double(value)
    }
}

/// Metadata for the native writer.
///
/// `tags` uses TIFF numeric tag IDs. The [`tag`] module exposes constants for
/// every tag used by the writer. `geo_keys` similarly uses GeoKey IDs and the
/// [`geo_key`] module provides their constants.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WriterMetadata {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub tags: BTreeMap<u16, WriterValue>,
    pub geo_keys: BTreeMap<u16, GeoKeyValue>,
}

impl WriterMetadata {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width: Some(width),
            height: Some(height),
            ..Self::default()
        }
    }

    pub fn set_tag(&mut self, tag: u16, value: impl Into<WriterValue>) -> &mut Self {
        self.tags.insert(tag, value.into());
        self
    }

    pub fn with_tag(mut self, tag: u16, value: impl Into<WriterValue>) -> Self {
        self.set_tag(tag, value);
        self
    }

    pub fn set_geo_key(&mut self, key: u16, value: impl Into<GeoKeyValue>) -> &mut Self {
        self.geo_keys.insert(key, value.into());
        self
    }

    pub fn with_geo_key(mut self, key: u16, value: impl Into<GeoKeyValue>) -> Self {
        self.set_geo_key(key, value);
        self
    }
}

/// TIFF tags used by `geotiffwriter.js`.
pub mod tag {
    pub const IMAGE_WIDTH: u16 = 256;
    pub const IMAGE_LENGTH: u16 = 257;
    pub const BITS_PER_SAMPLE: u16 = 258;
    pub const COMPRESSION: u16 = 259;
    pub const PHOTOMETRIC_INTERPRETATION: u16 = 262;
    pub const STRIP_OFFSETS: u16 = 273;
    pub const SAMPLES_PER_PIXEL: u16 = 277;
    pub const ROWS_PER_STRIP: u16 = 278;
    pub const STRIP_BYTE_COUNTS: u16 = 279;
    pub const PLANAR_CONFIGURATION: u16 = 284;
    pub const SOFTWARE: u16 = 305;
    pub const TILE_WIDTH: u16 = 322;
    pub const TILE_LENGTH: u16 = 323;
    pub const TILE_OFFSETS: u16 = 324;
    pub const TILE_BYTE_COUNTS: u16 = 325;
    pub const EXTRA_SAMPLES: u16 = 338;
    pub const SAMPLE_FORMAT: u16 = 339;
    pub const MODEL_PIXEL_SCALE: u16 = 33550;
    pub const MODEL_TIEPOINT: u16 = 33922;
    pub const MODEL_TRANSFORMATION: u16 = 34264;
    pub const GEO_KEY_DIRECTORY: u16 = 34735;
    pub const GEO_DOUBLE_PARAMS: u16 = 34736;
    pub const GEO_ASCII_PARAMS: u16 = 34737;
    pub const GDAL_NODATA: u16 = 42113;
}

/// GeoKey IDs from `globals.js`.
pub mod geo_key {
    pub const GT_MODEL_TYPE: u16 = 1024;
    pub const GT_RASTER_TYPE: u16 = 1025;
    pub const GT_CITATION: u16 = 1026;
    pub const GEOGRAPHIC_TYPE: u16 = 2048;
    pub const GEOG_CITATION: u16 = 2049;
    pub const GEOG_GEODETIC_DATUM: u16 = 2050;
    pub const GEOG_PRIME_MERIDIAN: u16 = 2051;
    pub const GEOG_LINEAR_UNITS: u16 = 2052;
    pub const GEOG_LINEAR_UNIT_SIZE: u16 = 2053;
    pub const GEOG_ANGULAR_UNITS: u16 = 2054;
    pub const GEOG_ANGULAR_UNIT_SIZE: u16 = 2055;
    pub const GEOG_ELLIPSOID: u16 = 2056;
    pub const GEOG_SEMI_MAJOR_AXIS: u16 = 2057;
    pub const GEOG_SEMI_MINOR_AXIS: u16 = 2058;
    pub const GEOG_INV_FLATTENING: u16 = 2059;
    pub const GEOG_AZIMUTH_UNITS: u16 = 2060;
    pub const GEOG_PRIME_MERIDIAN_LONG: u16 = 2061;
    pub const GEOG_TO_WGS84: u16 = 2062;
    pub const PROJECTED_CS_TYPE: u16 = 3072;
    pub const PCS_CITATION: u16 = 3073;
    pub const PROJECTION: u16 = 3074;
    pub const PROJ_COORD_TRANS: u16 = 3075;
    pub const PROJ_LINEAR_UNITS: u16 = 3076;
    pub const PROJ_LINEAR_UNIT_SIZE: u16 = 3077;
    pub const PROJ_STD_PARALLEL_1: u16 = 3078;
    pub const PROJ_STD_PARALLEL_2: u16 = 3079;
    pub const PROJ_NAT_ORIGIN_LONG: u16 = 3080;
    pub const PROJ_NAT_ORIGIN_LAT: u16 = 3081;
    pub const PROJ_FALSE_EASTING: u16 = 3082;
    pub const PROJ_FALSE_NORTHING: u16 = 3083;
    pub const PROJ_FALSE_ORIGIN_LONG: u16 = 3084;
    pub const PROJ_FALSE_ORIGIN_LAT: u16 = 3085;
    pub const PROJ_FALSE_ORIGIN_EASTING: u16 = 3086;
    pub const PROJ_FALSE_ORIGIN_NORTHING: u16 = 3087;
    pub const PROJ_CENTER_LONG: u16 = 3088;
    pub const PROJ_CENTER_LAT: u16 = 3089;
    pub const PROJ_CENTER_EASTING: u16 = 3090;
    pub const PROJ_CENTER_NORTHING: u16 = 3091;
    pub const PROJ_SCALE_AT_NAT_ORIGIN: u16 = 3092;
    pub const PROJ_SCALE_AT_CENTER: u16 = 3093;
    pub const PROJ_AZIMUTH_ANGLE: u16 = 3094;
    pub const PROJ_STRAIGHT_VERT_POLE_LONG: u16 = 3095;
    pub const PROJ_RECTIFIED_GRID_ANGLE: u16 = 3096;
    pub const VERTICAL_CS_TYPE: u16 = 4096;
    pub const VERTICAL_CITATION: u16 = 4097;
    pub const VERTICAL_DATUM: u16 = 4098;
    pub const VERTICAL_UNITS: u16 = 4099;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriterError {
    EmptyData,
    MissingWidth,
    MissingHeight,
    InvalidDimensions,
    InvalidNestedData(String),
    TiledSamplesPerPixelRequired,
    TiledDimensionsRequired,
    UnknownTag(u16),
    WrongTagValue { tag: u16, expected: &'static str },
    IfdTooLarge,
    NumericOverflow(&'static str),
    AllocationFailed(String),
}

impl fmt::Display for WriterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriterError::EmptyData => write!(f, "image data must not be empty"),
            WriterError::MissingWidth => {
                write!(
                    f,
                    "width is required to be a number in metadata if data is a flat array"
                )
            }
            WriterError::MissingHeight => {
                write!(
                    f,
                    "height is required to be a number in metadata if data is a flat array"
                )
            }
            WriterError::InvalidDimensions => {
                write!(f, "image dimensions and sample count are inconsistent")
            }
            WriterError::InvalidNestedData(message) => {
                write!(f, "invalid nested image data: {message}")
            }
            WriterError::TiledSamplesPerPixelRequired => {
                write!(
                    f,
                    "SamplesPerPixel must be specified when writing tiled images"
                )
            }
            WriterError::TiledDimensionsRequired => {
                write!(
                    f,
                    "Both TileWidth and TileLength must be specified when writing tiled images"
                )
            }
            WriterError::UnknownTag(tag) => write!(f, "unknown type of tag: {tag}"),
            WriterError::WrongTagValue { tag, expected } => {
                write!(f, "tag {tag} must have a {expected} value")
            }
            WriterError::IfdTooLarge => {
                write!(
                    f,
                    "Writing of IFDs with more than 1000 bytes is not supported"
                )
            }
            WriterError::NumericOverflow(what) => write!(f, "{what} exceeds classic TIFF limits"),
            WriterError::AllocationFailed(message) => {
                write!(f, "could not allocate GeoTIFF output: {message}")
            }
        }
    }
}

impl std::error::Error for WriterError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IfdType {
    Ascii = 2,
    Short = 3,
    Long = 4,
    Rational = 5,
    Double = 12,
}

impl IfdType {
    fn size(self) -> usize {
        match self {
            IfdType::Ascii => 1,
            IfdType::Short => 2,
            IfdType::Long => 4,
            IfdType::Rational | IfdType::Double => 8,
        }
    }
}

fn writer_field_type(tag: u16) -> Option<IfdType> {
    Some(match tag {
        256 | 257 | 258 | 259 | 262 | 274 | 277 | 284 | 286 | 296 | 297 | 322 | 323 | 338 | 339
        | 1024 | 1025 | 2052 | 2054 | 2060 | 3072 | 3076 | 4096 | 4099 | 34735 => IfdType::Short,
        273 | 278 | 279 | 324 | 325 | 513 | 514 | 34665 => IfdType::Long,
        282 | 283 | 287 => IfdType::Rational,
        270 | 271 | 272 | 305 | 306 | 315 | 33432 | 34737 | 42113 => IfdType::Ascii,
        33550 | 33922 | 34264 | 34736 => IfdType::Double,
        _ => return None,
    })
}

fn numeric_values(value: &WriterValue, tag: u16) -> Result<&[f64], WriterError> {
    match value {
        WriterValue::Numbers(values) => Ok(values),
        WriterValue::Ascii(_) => Err(WriterError::WrongTagValue {
            tag,
            expected: "numeric",
        }),
    }
}

fn first_u32(tags: &BTreeMap<u16, WriterValue>, tag: u16) -> Option<u32> {
    match tags.get(&tag) {
        Some(WriterValue::Numbers(values)) => values.first().copied().map(|v| v as u32),
        _ => None,
    }
}

fn type_info(data: &WriterData) -> (usize, u16, u16) {
    match data {
        WriterData::Numbers(_) | WriterData::Nested(_) => (8, 8, 1),
        WriterData::Typed(array) => match array {
            TypedArray::Int8(_) => (1, 8, 2),
            TypedArray::Uint8(_) | TypedArray::Uint8Clamped(_) => (1, 8, 1),
            TypedArray::Int16(_) => (2, 16, 2),
            TypedArray::Uint16(_) => (2, 16, 1),
            TypedArray::Int32(_) => (4, 32, 2),
            TypedArray::Uint32(_) => (4, 32, 1),
            TypedArray::Int64(_) => (8, 64, 2),
            TypedArray::Uint64(_) => (8, 64, 1),
            TypedArray::Float32(_) => (4, 32, 3),
            TypedArray::Float64(_) => (8, 64, 3),
        },
    }
}

fn data_len(data: &WriterData) -> usize {
    match data {
        WriterData::Typed(array) => array.len(),
        WriterData::Numbers(values) => values.len(),
        WriterData::Nested(bands) => bands
            .first()
            .map(|band| band.iter().map(Vec::len).sum())
            .unwrap_or(0),
    }
}

fn flatten_nested(bands: &[Vec<Vec<f64>>], samples: usize) -> Result<Vec<f64>, WriterError> {
    let first = bands.first().ok_or(WriterError::EmptyData)?;
    let height = first.len();
    let width = first.first().map(Vec::len).unwrap_or(0);
    if height == 0 || width == 0 {
        return Err(WriterError::EmptyData);
    }
    if bands.len() != samples {
        return Err(WriterError::InvalidNestedData(format!(
            "SamplesPerPixel is {samples}, but {} bands were supplied",
            bands.len()
        )));
    }
    for (band_index, band) in bands.iter().enumerate() {
        if band.len() != height {
            return Err(WriterError::InvalidNestedData(format!(
                "band {band_index} has a different height"
            )));
        }
        for (row_index, row) in band.iter().enumerate() {
            if row.len() != width {
                return Err(WriterError::InvalidNestedData(format!(
                    "band {band_index}, row {row_index} has a different width"
                )));
            }
        }
    }

    let flattened_len = width
        .checked_mul(height)
        .and_then(|value| value.checked_mul(samples))
        .ok_or(WriterError::NumericOverflow("nested sample count"))?;
    let mut flattened = Vec::new();
    flattened
        .try_reserve_exact(flattened_len)
        .map_err(|error| WriterError::AllocationFailed(error.to_string()))?;
    for row in 0..height {
        for column in 0..width {
            for band in bands {
                flattened.push(band[row][column]);
            }
        }
    }
    Ok(flattened)
}

fn js_u8(value: f64) -> u8 {
    if !value.is_finite() || value == 0.0 {
        return 0;
    }
    let truncated = value.trunc();
    let modulo = truncated.rem_euclid(256.0);
    modulo as u8
}

fn javascript_writer_unmapped_typed_array(data: &WriterData) -> bool {
    matches!(
        data,
        WriterData::Typed(
            TypedArray::Int8(_)
                | TypedArray::Int16(_)
                | TypedArray::Int32(_)
                | TypedArray::Uint8Clamped(_)
        )
    )
}

fn output_element_size(data: &WriterData, compatibility: WriterCompatibility) -> usize {
    if compatibility == WriterCompatibility::GeotiffJs
        && javascript_writer_unmapped_typed_array(data)
    {
        // `encodeImage` defaults unknown constructors to Float64's size.
        8
    } else {
        type_info(data).0
    }
}

fn pixel_bytes(
    data: &WriterData,
    compatibility: WriterCompatibility,
) -> Result<Vec<u8>, WriterError> {
    if compatibility == WriterCompatibility::GeotiffJs
        && javascript_writer_unmapped_typed_array(data)
    {
        let values = match data {
            WriterData::Typed(values) => values,
            _ => unreachable!("unmapped JavaScript writer input is a typed array"),
        };
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(values.len())
            .map_err(|error| WriterError::AllocationFailed(error.to_string()))?;
        for index in 0..values.len() {
            bytes.push(js_u8(values.get_f64(index)));
        }
        return Ok(bytes);
    }

    let (element_size, _, _) = type_info(data);
    let byte_len = data_len(data)
        .checked_mul(element_size)
        .ok_or(WriterError::NumericOverflow("pixel byte count"))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(byte_len)
        .map_err(|error| WriterError::AllocationFailed(error.to_string()))?;
    match data {
        WriterData::Numbers(values) => bytes.extend(values.iter().map(|&value| js_u8(value))),
        WriterData::Nested(_) => unreachable!("nested data is flattened before encoding"),
        WriterData::Typed(array) => match array {
            TypedArray::Int8(values) => bytes.extend(values.iter().map(|&v| v as u8)),
            TypedArray::Uint8(values) | TypedArray::Uint8Clamped(values) => {
                bytes.extend_from_slice(values)
            }
            TypedArray::Int16(values) => {
                for value in values {
                    bytes.extend_from_slice(&value.to_be_bytes());
                }
            }
            TypedArray::Uint16(values) => {
                for value in values {
                    bytes.extend_from_slice(&value.to_be_bytes());
                }
            }
            TypedArray::Int32(values) => {
                for value in values {
                    bytes.extend_from_slice(&value.to_be_bytes());
                }
            }
            TypedArray::Uint32(values) => {
                for value in values {
                    bytes.extend_from_slice(&value.to_be_bytes());
                }
            }
            TypedArray::Int64(values) => {
                for value in values {
                    bytes.extend_from_slice(&value.to_be_bytes());
                }
            }
            TypedArray::Uint64(values) => {
                for value in values {
                    bytes.extend_from_slice(&value.to_be_bytes());
                }
            }
            TypedArray::Float32(values) => {
                for value in values {
                    bytes.extend_from_slice(&value.to_be_bytes());
                }
            }
            TypedArray::Float64(values) => {
                for value in values {
                    bytes.extend_from_slice(&value.to_be_bytes());
                }
            }
        },
    }
    Ok(bytes)
}

fn counts(tags: &BTreeMap<u16, WriterValue>, tag: u16) -> Result<Vec<u32>, WriterError> {
    numeric_values(
        tags.get(&tag).ok_or(WriterError::WrongTagValue {
            tag,
            expected: "numeric",
        })?,
        tag,
    )
    .map(|values| values.iter().map(|&value| value as u32).collect())
}

fn counts_to_offsets(values: &[u32]) -> Result<Vec<f64>, WriterError> {
    let mut total = IFD_RESERVE_BYTES as u64;
    let mut result = Vec::with_capacity(values.len());
    for &count in values {
        let offset =
            u32::try_from(total).map_err(|_| WriterError::NumericOverflow("data offset"))?;
        result.push(offset as f64);
        total = total
            .checked_add(u64::from(count))
            .ok_or(WriterError::NumericOverflow("data offset"))?;
    }
    Ok(result)
}

fn build_geo_key_directory(metadata: &mut WriterMetadata) -> Result<(), WriterError> {
    if metadata.tags.contains_key(&tag::GEO_KEY_DIRECTORY) {
        return Ok(());
    }

    let ascii_was_provided = metadata.tags.contains_key(&tag::GEO_ASCII_PARAMS);
    let double_was_provided = metadata.tags.contains_key(&tag::GEO_DOUBLE_PARAMS);
    let mut ascii = match metadata.tags.get(&tag::GEO_ASCII_PARAMS) {
        Some(WriterValue::Ascii(value)) => value.clone(),
        _ => String::new(),
    };
    let mut doubles = match metadata.tags.get(&tag::GEO_DOUBLE_PARAMS) {
        Some(WriterValue::Numbers(values)) => values.clone(),
        _ => Vec::new(),
    };
    let mut directory = vec![1.0, 1.0, 0.0, 0.0];
    let mut valid_keys = 0u16;

    for (&key, value) in &metadata.geo_keys {
        match value {
            GeoKeyValue::Short(value) => {
                directory.extend([key as f64, 0.0, 1.0, *value as f64]);
            }
            GeoKeyValue::Ascii(value) if !ascii_was_provided => {
                let offset = ascii.len();
                ascii.push_str(value);
                ascii.push('\0');
                directory.extend([
                    key as f64,
                    tag::GEO_ASCII_PARAMS as f64,
                    // geotiff.js includes the terminating NUL in the GeoKey
                    // count. The read-side compatibility adapter preserves
                    // that wire format while shielding async-tiff 0.3 from
                    // its shortened-string slicing panic.
                    (value.len() + 1) as f64,
                    offset as f64,
                ]);
            }
            GeoKeyValue::Double(values) if !double_was_provided => {
                let offset = doubles.len();
                doubles.extend_from_slice(values);
                directory.extend([
                    key as f64,
                    tag::GEO_DOUBLE_PARAMS as f64,
                    values.len() as f64,
                    offset as f64,
                ]);
            }
            GeoKeyValue::Ascii(_) | GeoKeyValue::Double(_) => continue,
        }
        valid_keys = valid_keys
            .checked_add(1)
            .ok_or(WriterError::NumericOverflow("GeoKey count"))?;
    }

    directory[3] = valid_keys as f64;
    metadata
        .tags
        .insert(tag::GEO_KEY_DIRECTORY, WriterValue::Numbers(directory));
    if !ascii_was_provided && !ascii.is_empty() {
        metadata
            .tags
            .insert(tag::GEO_ASCII_PARAMS, WriterValue::Ascii(ascii));
    }
    if !double_was_provided && !doubles.is_empty() {
        metadata
            .tags
            .insert(tag::GEO_DOUBLE_PARAMS, WriterValue::Numbers(doubles));
    }
    Ok(())
}

fn put_u16(buffer: &mut [u8], offset: usize, value: u16) -> Result<(), WriterError> {
    let target = buffer
        .get_mut(offset..offset + 2)
        .ok_or(WriterError::IfdTooLarge)?;
    target.copy_from_slice(&value.to_be_bytes());
    Ok(())
}

fn put_u32(buffer: &mut [u8], offset: usize, value: u32) -> Result<(), WriterError> {
    let target = buffer
        .get_mut(offset..offset + 4)
        .ok_or(WriterError::IfdTooLarge)?;
    target.copy_from_slice(&value.to_be_bytes());
    Ok(())
}

fn write_ifd(tags: &BTreeMap<u16, WriterValue>, data: &mut [u8]) -> Result<usize, WriterError> {
    let count = u16::try_from(tags.len()).map_err(|_| WriterError::IfdTooLarge)?;
    let mut offset = 8usize;
    put_u16(data, offset, count)?;
    offset += 2;
    let mut extra_offset = offset
        .checked_add(
            12usize
                .checked_mul(tags.len())
                .ok_or(WriterError::IfdTooLarge)?,
        )
        .and_then(|value| value.checked_add(4))
        .ok_or(WriterError::IfdTooLarge)?;
    if extra_offset > data.len() {
        return Err(WriterError::IfdTooLarge);
    }

    for (&tag, value) in tags {
        let field_type = writer_field_type(tag).ok_or(WriterError::UnknownTag(tag))?;
        let (count, ascii): (usize, Option<Vec<u8>>) = match field_type {
            IfdType::Ascii => match value {
                WriterValue::Ascii(value) => {
                    let mut bytes = value.as_bytes().to_vec();
                    if !bytes.ends_with(&[0]) {
                        bytes.push(0);
                    }
                    (bytes.len(), Some(bytes))
                }
                WriterValue::Numbers(_) => {
                    return Err(WriterError::WrongTagValue {
                        tag,
                        expected: "string",
                    });
                }
            },
            _ => (numeric_values(value, tag)?.len(), None),
        };
        let data_len = field_type
            .size()
            .checked_mul(count)
            .ok_or(WriterError::IfdTooLarge)?;

        put_u16(data, offset, tag)?;
        put_u16(data, offset + 2, field_type as u16)?;
        put_u32(
            data,
            offset + 4,
            u32::try_from(count).map_err(|_| WriterError::NumericOverflow("tag value count"))?,
        )?;

        let value_offset = if data_len > 4 {
            put_u32(
                data,
                offset + 8,
                u32::try_from(extra_offset).map_err(|_| WriterError::IfdTooLarge)?,
            )?;
            extra_offset
        } else {
            offset + 8
        };

        if value_offset
            .checked_add(data_len)
            .is_none_or(|end| end > data.len())
        {
            return Err(WriterError::IfdTooLarge);
        }

        match field_type {
            IfdType::Ascii => {
                let bytes = ascii.expect("ASCII value prepared above");
                data[value_offset..value_offset + bytes.len()].copy_from_slice(&bytes);
            }
            IfdType::Short => {
                for (index, &value) in numeric_values(value, tag)?.iter().enumerate() {
                    put_u16(data, value_offset + index * 2, value as i64 as u16)?;
                }
            }
            IfdType::Long => {
                for (index, &value) in numeric_values(value, tag)?.iter().enumerate() {
                    put_u32(data, value_offset + index * 4, value as i64 as u32)?;
                }
            }
            IfdType::Rational => {
                for (index, &value) in numeric_values(value, tag)?.iter().enumerate() {
                    let numerator = (value * 10_000.0 + 0.5).floor() as i64 as u32;
                    put_u32(data, value_offset + index * 8, numerator)?;
                    put_u32(data, value_offset + index * 8 + 4, 10_000)?;
                }
            }
            IfdType::Double => {
                for (index, &value) in numeric_values(value, tag)?.iter().enumerate() {
                    let start = value_offset + index * 8;
                    data[start..start + 8].copy_from_slice(&value.to_be_bytes());
                }
            }
        }

        if data_len > 4 {
            extra_offset = extra_offset
                .checked_add(data_len)
                .and_then(|value| value.checked_add(data_len & 1))
                .ok_or(WriterError::IfdTooLarge)?;
            if extra_offset > data.len() {
                return Err(WriterError::IfdTooLarge);
            }
        }
        offset += 12;
    }

    // The next-IFD offset remains zero in the zero-filled buffer.
    Ok(extra_offset)
}

fn encode_ifd(tags: &BTreeMap<u16, WriterValue>) -> Result<Vec<u8>, WriterError> {
    let mut data = Vec::new();
    data.try_reserve_exact(IFD_RESERVE_BYTES)
        .map_err(|error| WriterError::AllocationFailed(error.to_string()))?;
    data.resize(IFD_RESERVE_BYTES, 0u8);
    data[0] = b'M';
    data[1] = b'M';
    data[3] = 42;
    put_u32(&mut data, 4, 8)?;
    let end = write_ifd(tags, &mut data)?;
    data.truncate(end);
    Ok(data)
}

/// Write a classic big-endian GeoTIFF, equivalent to geotiff.js
/// `writeGeotiff`.
pub fn write_geotiff(
    data: impl Into<WriterData>,
    metadata: WriterMetadata,
) -> Result<Vec<u8>, WriterError> {
    write_geotiff_with_mode(data, metadata, WriterCompatibility::Lossless)
}

/// Writes with an explicit payload compatibility policy. Metadata inference
/// remains identical in both modes; only geotiff.js's broken serialization
/// of typed-array constructors absent from its `typeMap` is reproduced.
pub fn write_geotiff_with_mode(
    data: impl Into<WriterData>,
    mut metadata: WriterMetadata,
    compatibility: WriterCompatibility,
) -> Result<Vec<u8>, WriterError> {
    let mut data = data.into();
    if data_len(&data) == 0 {
        return Err(WriterError::EmptyData);
    }

    let tiled = metadata.tags.contains_key(&tag::TILE_BYTE_COUNTS);
    let (width, height, samples) = match &data {
        WriterData::Nested(bands) => {
            let height = bands.first().map(Vec::len).unwrap_or(0);
            let width = bands
                .first()
                .and_then(|band| band.first())
                .map(Vec::len)
                .unwrap_or(0);
            let samples = first_u32(&metadata.tags, tag::SAMPLES_PER_PIXEL)
                .map(|value| value as usize)
                .unwrap_or(bands.len());
            if width == 0 || height == 0 || samples == 0 {
                return Err(WriterError::EmptyData);
            }
            let flattened = flatten_nested(bands, samples)?;
            data = WriterData::Numbers(flattened);
            (
                u32::try_from(width).map_err(|_| WriterError::InvalidDimensions)?,
                u32::try_from(height).map_err(|_| WriterError::InvalidDimensions)?,
                samples,
            )
        }
        WriterData::Typed(_) | WriterData::Numbers(_) => {
            let height = metadata
                .height
                .or_else(|| first_u32(&metadata.tags, tag::IMAGE_LENGTH))
                .ok_or(WriterError::MissingHeight)?;
            let width = metadata
                .width
                .or_else(|| first_u32(&metadata.tags, tag::IMAGE_WIDTH))
                .ok_or(WriterError::MissingWidth)?;
            if width == 0 || height == 0 {
                return Err(WriterError::InvalidDimensions);
            }
            let pixel_count = (width as usize)
                .checked_mul(height as usize)
                .ok_or(WriterError::InvalidDimensions)?;
            let samples = match first_u32(&metadata.tags, tag::SAMPLES_PER_PIXEL) {
                Some(value) => value as usize,
                None if data_len(&data).is_multiple_of(pixel_count) => {
                    data_len(&data) / pixel_count
                }
                None => return Err(WriterError::InvalidDimensions),
            };
            let required = pixel_count
                .checked_mul(samples)
                .ok_or(WriterError::InvalidDimensions)?;
            if samples == 0 || data_len(&data) < required {
                return Err(WriterError::InvalidDimensions);
            }
            (width, height, samples)
        }
    };

    if tiled {
        if !metadata.tags.contains_key(&tag::SAMPLES_PER_PIXEL) {
            return Err(WriterError::TiledSamplesPerPixelRequired);
        }
        if !metadata.tags.contains_key(&tag::TILE_WIDTH)
            || !metadata.tags.contains_key(&tag::TILE_LENGTH)
        {
            return Err(WriterError::TiledDimensionsRequired);
        }
    }

    let (element_size, inferred_bits, inferred_format) = type_info(&data);
    metadata.width = Some(width);
    metadata.height = Some(height);
    metadata
        .tags
        .insert(tag::IMAGE_WIDTH, WriterValue::Numbers(vec![width as f64]));
    metadata
        .tags
        .insert(tag::IMAGE_LENGTH, WriterValue::Numbers(vec![height as f64]));
    metadata
        .tags
        .entry(tag::BITS_PER_SAMPLE)
        .or_insert_with(|| WriterValue::Numbers(vec![inferred_bits as f64; samples]));
    metadata
        .tags
        .entry(tag::COMPRESSION)
        .or_insert_with(|| WriterValue::Numbers(vec![1.0]));
    metadata
        .tags
        .entry(tag::PLANAR_CONFIGURATION)
        .or_insert_with(|| WriterValue::Numbers(vec![1.0]));
    // geotiff.js writes ExtraSamples=0 as a zero-count field because the
    // scalar has no JS `.length`. Keep that observable wire behavior.
    metadata
        .tags
        .entry(tag::EXTRA_SAMPLES)
        .or_insert_with(|| WriterValue::Numbers(Vec::new()));

    let photometric_missing =
        first_u32(&metadata.tags, tag::PHOTOMETRIC_INTERPRETATION).is_none_or(|value| value == 0);
    if photometric_missing {
        let bit_count = numeric_values(
            metadata
                .tags
                .get(&tag::BITS_PER_SAMPLE)
                .expect("inserted above"),
            tag::BITS_PER_SAMPLE,
        )?
        .len();
        metadata.tags.insert(
            tag::PHOTOMETRIC_INTERPRETATION,
            WriterValue::Numbers(vec![if bit_count == 3 { 2.0 } else { 1.0 }]),
        );
    }
    metadata
        .tags
        .entry(tag::SAMPLES_PER_PIXEL)
        .or_insert_with(|| WriterValue::Numbers(vec![samples as f64]));

    if !tiled && !metadata.tags.contains_key(&tag::STRIP_BYTE_COUNTS) {
        let count = samples
            .checked_mul(element_size)
            .and_then(|value| value.checked_mul(height as usize))
            .and_then(|value| value.checked_mul(width as usize))
            .ok_or(WriterError::NumericOverflow("strip byte count"))?;
        metadata.tags.insert(
            tag::STRIP_BYTE_COUNTS,
            WriterValue::Numbers(vec![count as f64]),
        );
    }
    if !metadata.tags.contains_key(&tag::MODEL_PIXEL_SCALE)
        && !metadata.tags.contains_key(&tag::MODEL_TRANSFORMATION)
    {
        metadata.tags.insert(
            tag::MODEL_PIXEL_SCALE,
            WriterValue::Numbers(vec![360.0 / width as f64, 180.0 / height as f64, 0.0]),
        );
    }
    metadata
        .tags
        .entry(tag::SAMPLE_FORMAT)
        .or_insert_with(|| WriterValue::Numbers(vec![inferred_format as f64; samples]));

    if !metadata.geo_keys.contains_key(&geo_key::GEOGRAPHIC_TYPE)
        && !metadata.geo_keys.contains_key(&geo_key::PROJECTED_CS_TYPE)
    {
        metadata
            .geo_keys
            .insert(geo_key::GEOGRAPHIC_TYPE, GeoKeyValue::Short(4326));
        if !metadata.tags.contains_key(&tag::MODEL_TRANSFORMATION) {
            metadata.tags.insert(
                tag::MODEL_TIEPOINT,
                WriterValue::Numbers(vec![0.0, 0.0, 0.0, -180.0, 90.0, 0.0]),
            );
        }
        metadata.geo_keys.insert(
            geo_key::GEOG_CITATION,
            GeoKeyValue::Ascii("WGS 84".to_owned()),
        );
        metadata
            .geo_keys
            .insert(geo_key::GT_MODEL_TYPE, GeoKeyValue::Short(2));
    }
    build_geo_key_directory(&mut metadata)?;

    let byte_counts = if tiled {
        counts(&metadata.tags, tag::TILE_BYTE_COUNTS)?
    } else {
        counts(&metadata.tags, tag::STRIP_BYTE_COUNTS)?
    };
    let offsets = counts_to_offsets(&byte_counts)?;

    let mut ifd = BTreeMap::new();
    ifd.insert(tag::IMAGE_WIDTH, WriterValue::Numbers(vec![width as f64]));
    ifd.insert(tag::IMAGE_LENGTH, WriterValue::Numbers(vec![height as f64]));
    ifd.insert(tag::SOFTWARE, WriterValue::Ascii("geotiff.js".to_owned()));
    if tiled {
        ifd.insert(tag::TILE_OFFSETS, WriterValue::Numbers(offsets));
    } else {
        ifd.insert(tag::STRIP_OFFSETS, WriterValue::Numbers(offsets));
        ifd.insert(
            tag::ROWS_PER_STRIP,
            WriterValue::Numbers(vec![height as f64]),
        );
    }
    metadata.tags.remove(&tag::STRIP_OFFSETS);
    ifd.extend(metadata.tags);

    let prefix = encode_ifd(&ifd)?;
    let payload = pixel_bytes(&data, compatibility)?;
    let output_element_size = output_element_size(&data, compatibility);
    let allocation = data_len(&data)
        .checked_mul(output_element_size)
        .and_then(|value| value.checked_add(IFD_RESERVE_BYTES))
        .ok_or(WriterError::NumericOverflow("output size"))?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(allocation)
        .map_err(|error| WriterError::AllocationFailed(error.to_string()))?;
    output.resize(allocation, 0u8);
    output[..prefix.len()].copy_from_slice(&prefix);
    let payload_end = IFD_RESERVE_BYTES
        .checked_add(payload.len())
        .ok_or(WriterError::NumericOverflow("output size"))?;
    if payload_end > output.len() {
        return Err(WriterError::NumericOverflow("output size"));
    }
    output[IFD_RESERVE_BYTES..payload_end].copy_from_slice(&payload);
    Ok(output)
}

/// Rust spelling of geotiff.js `writeArrayBuffer`.
pub fn write_array_buffer(
    data: impl Into<WriterData>,
    metadata: WriterMetadata,
) -> Result<Vec<u8>, WriterError> {
    write_geotiff(data, metadata)
}

/// Rust spelling of geotiff.js `writeArrayBuffer` with an explicit choice
/// between lossless native payloads and exact 3.1.0 signed-array wire quirks.
pub fn write_array_buffer_with_mode(
    data: impl Into<WriterData>,
    metadata: WriterMetadata,
    compatibility: WriterCompatibility,
) -> Result<Vec<u8>, WriterError> {
    write_geotiff_with_mode(data, metadata, compatibility)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::from_bytes;
    use crate::geotiffimage::{ReadRasterResult, ReadRastersOptions};

    fn fnv64(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        })
    }

    #[test]
    fn unsigned_and_float_wire_bytes_match_geotiff_js() {
        let unsigned = write_array_buffer(vec![1u8, 2, 3, 4], WriterMetadata::new(2, 2)).unwrap();
        assert_eq!(fnv64(&unsigned), 0x2f34d38204e0ad36);

        let floats = write_array_buffer(
            TypedArray::Float32(vec![-1.5, 0.0, 2.25, 8.5]),
            WriterMetadata::new(2, 2),
        )
        .unwrap();
        assert_eq!(fnv64(&floats), 0x6a6b33765f0d2dd2);
    }

    #[test]
    fn signed_writer_defaults_to_lossless_and_can_reproduce_the_js_wire_bug() {
        let values = TypedArray::Int16(vec![-32768, -2, 3, 32767]);
        let lossless = write_array_buffer(values.clone(), WriterMetadata::new(2, 2)).unwrap();
        assert_eq!(lossless.len(), 1008);
        assert_eq!(
            &lossless[1000..],
            &[0x80, 0x00, 0xff, 0xfe, 0x00, 0x03, 0x7f, 0xff]
        );

        let compatible = write_array_buffer_with_mode(
            values,
            WriterMetadata::new(2, 2),
            WriterCompatibility::GeotiffJs,
        )
        .unwrap();
        assert_eq!(compatible.len(), 1032);
        assert_eq!(&compatible[1000..1004], &[0, 254, 3, 255]);
        assert!(compatible[1004..].iter().all(|value| *value == 0));
    }

    #[test]
    fn nested_metadata_tiled_and_multistrip_wire_bytes_match_live_js_oracle() {
        let nested = WriterData::Nested(vec![
            vec![vec![1.0, 2.0], vec![3.0, 4.0]],
            vec![vec![5.0, 6.0], vec![7.0, 8.0]],
            vec![vec![9.0, 10.0], vec![11.0, 12.0]],
        ]);
        let bytes = write_array_buffer(nested, WriterMetadata::default()).unwrap();
        assert_eq!((bytes.len(), fnv64(&bytes)), (1096, 0x7f343fc530d0a350));

        let metadata = WriterMetadata::new(2, 2)
            .with_tag(tag::GDAL_NODATA, "-9999\0")
            .with_tag(274, 3u16)
            .with_geo_key(geo_key::GEOGRAPHIC_TYPE, 4326u16)
            .with_geo_key(geo_key::GEOG_CITATION, "X")
            .with_geo_key(geo_key::GT_RASTER_TYPE, 1u16);
        let bytes = write_array_buffer(vec![1u16, 2, 3, 4], metadata).unwrap();
        assert_eq!((bytes.len(), fnv64(&bytes)), (1008, 0x109698d6ba270384));

        let tiled = WriterMetadata::new(3, 3)
            .with_tag(tag::SAMPLES_PER_PIXEL, 3u16)
            .with_tag(tag::TILE_WIDTH, 3u16)
            .with_tag(tag::TILE_LENGTH, 3u16)
            .with_tag(tag::TILE_BYTE_COUNTS, vec![27u32]);
        let bytes = write_array_buffer((0..27u8).collect::<Vec<_>>(), tiled).unwrap();
        assert_eq!((bytes.len(), fnv64(&bytes)), (1027, 0xcf210268e18dd9a3));

        let multistrip = WriterMetadata::new(3, 2)
            .with_tag(tag::ROWS_PER_STRIP, 1u16)
            .with_tag(tag::STRIP_BYTE_COUNTS, vec![3u32, 3]);
        let bytes = write_array_buffer(vec![1u8, 2, 3, 4, 5, 6], multistrip).unwrap();
        assert_eq!((bytes.len(), fnv64(&bytes)), (1006, 0x1106474286e16038));
    }

    #[tokio::test]
    async fn uint8_rgb_roundtrips_through_public_reader() {
        let values = vec![
            255u8, 0, 0, 255, 0, 0, 255, 0, 0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0, 0, 255, 0, 0,
            255, 0, 0, 255,
        ];
        let bytes = write_array_buffer(values.clone(), WriterMetadata::new(3, 3)).unwrap();
        assert_eq!(bytes.len(), IFD_RESERVE_BYTES + values.len());
        let dataset = from_bytes(bytes).await.unwrap();
        let image = dataset.image(0).unwrap();
        let result = image
            .read_rasters(ReadRastersOptions::default())
            .await
            .unwrap();
        let ReadRasterResult::Bands(raster) = result else {
            panic!("expected separate bands");
        };
        assert_eq!((raster.width, raster.height), (3, 3));
        assert_eq!(raster.bands.len(), 3);
    }

    #[tokio::test]
    async fn float32_and_signed_samples_roundtrip_without_js_writer_data_loss() {
        for values in [
            TypedArray::Float32(vec![-1.5, 0.0, 2.25, 8.5]),
            TypedArray::Int16(vec![-32768, -2, 3, 32767]),
        ] {
            let bytes = write_array_buffer(values.clone(), WriterMetadata::new(2, 2)).unwrap();
            let dataset = from_bytes(bytes).await.unwrap();
            let image = dataset.image(0).unwrap();
            let result = image
                .read_rasters(ReadRastersOptions::default())
                .await
                .unwrap();
            let ReadRasterResult::Bands(raster) = result else {
                panic!("expected separate bands");
            };
            assert_eq!(raster.bands, vec![values]);
        }
    }

    fn uint8_bands(result: ReadRasterResult) -> Vec<Vec<u8>> {
        let ReadRasterResult::Bands(raster) = result else {
            panic!("expected separate bands");
        };
        raster
            .bands
            .into_iter()
            .map(|band| match band {
                TypedArray::Uint8(values) => values,
                other => panic!("expected Uint8 band, got {other:?}"),
            })
            .collect()
    }

    #[tokio::test]
    async fn multistrip_rgb_roundtrips() {
        let values = vec![
            255u8, 0, 0, 255, 0, 0, 255, 0, 0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0, 0, 255, 0, 0,
            255, 0, 0, 255,
        ];
        let metadata = WriterMetadata::new(3, 3)
            .with_tag(tag::ROWS_PER_STRIP, 1u32)
            .with_tag(tag::STRIP_BYTE_COUNTS, vec![9u32, 9, 9]);
        let dataset = from_bytes(write_array_buffer(values, metadata).unwrap())
            .await
            .unwrap();
        let bands = uint8_bands(
            dataset
                .image(0)
                .unwrap()
                .read_rasters(ReadRastersOptions::default())
                .await
                .unwrap(),
        );
        assert_eq!(bands[0], vec![255, 255, 255, 0, 0, 0, 0, 0, 0]);
        assert_eq!(bands[1], vec![0, 0, 0, 255, 255, 255, 0, 0, 0]);
        assert_eq!(bands[2], vec![0, 0, 0, 0, 0, 0, 255, 255, 255]);
    }

    fn tiled_interleaved_rgb() -> Vec<u8> {
        let pixels = [
            [255, 2, 3],
            [255, 2, 3],
            [255, 2, 3],
            [1, 255, 3],
            [1, 255, 3],
            [1, 255, 3],
            [1, 2, 255],
            [1, 2, 255],
            [1, 2, 255],
        ];
        let mut output = Vec::new();
        for tile_y in 0..2 {
            for tile_x in 0..2 {
                for y in 0..2 {
                    for x in 0..2 {
                        let source_x = tile_x * 2 + x;
                        let source_y = tile_y * 2 + y;
                        if source_x < 3 && source_y < 3 {
                            output.extend(pixels[source_y * 3 + source_x]);
                        } else {
                            output.extend([0, 0, 0]);
                        }
                    }
                }
            }
        }
        output
    }

    fn tiled_metadata(planar: bool, bytes_per_tile: u32) -> WriterMetadata {
        let count = if planar { 12 } else { 4 };
        WriterMetadata::new(3, 3)
            .with_tag(tag::TILE_BYTE_COUNTS, vec![bytes_per_tile; count])
            .with_tag(tag::TILE_WIDTH, 2u16)
            .with_tag(tag::TILE_LENGTH, 2u16)
            .with_tag(tag::SAMPLES_PER_PIXEL, 3u16)
            .with_tag(tag::PLANAR_CONFIGURATION, if planar { 2u16 } else { 1u16 })
    }

    #[tokio::test]
    async fn tiled_interleaved_and_planar_rgb_roundtrip() {
        let interleaved = tiled_interleaved_rgb();
        let dataset =
            from_bytes(write_array_buffer(interleaved, tiled_metadata(false, 12)).unwrap())
                .await
                .unwrap();
        let bands = uint8_bands(
            dataset
                .image(0)
                .unwrap()
                .read_rasters(ReadRastersOptions::default())
                .await
                .unwrap(),
        );
        assert_eq!(bands[0], vec![255, 255, 255, 1, 1, 1, 1, 1, 1]);
        assert_eq!(bands[1], vec![2, 2, 2, 255, 255, 255, 2, 2, 2]);
        assert_eq!(bands[2], vec![3, 3, 3, 3, 3, 3, 255, 255, 255]);

        let source_bands = [&bands[0], &bands[1], &bands[2]];
        let mut planar = Vec::new();
        for band in source_bands {
            for tile_y in 0..2 {
                for tile_x in 0..2 {
                    for y in 0..2 {
                        for x in 0..2 {
                            let source_x = tile_x * 2 + x;
                            let source_y = tile_y * 2 + y;
                            planar.push(if source_x < 3 && source_y < 3 {
                                band[source_y * 3 + source_x]
                            } else {
                                0
                            });
                        }
                    }
                }
            }
        }
        let dataset = from_bytes(write_array_buffer(planar, tiled_metadata(true, 4)).unwrap())
            .await
            .unwrap();
        let decoded = uint8_bands(
            dataset
                .image(0)
                .unwrap()
                .read_rasters(ReadRastersOptions::default())
                .await
                .unwrap(),
        );
        assert_eq!(decoded, bands);
    }

    #[tokio::test]
    async fn generated_ascii_and_double_geokeys_roundtrip() {
        let metadata = WriterMetadata::new(2, 2)
            .with_geo_key(geo_key::GEOGRAPHIC_TYPE, 4326u16)
            .with_geo_key(geo_key::GT_MODEL_TYPE, 2u16)
            .with_geo_key(geo_key::GEOG_SEMI_MAJOR_AXIS, 6_378_137.0)
            .with_geo_key(geo_key::GEOG_INV_FLATTENING, 298.257_223_563)
            .with_geo_key(geo_key::GEOG_CITATION, "WGS 84")
            .with_geo_key(geo_key::PCS_CITATION, "test-ascii");
        let dataset =
            from_bytes(write_array_buffer(TypedArray::Float32(vec![1.0; 4]), metadata).unwrap())
                .await
                .unwrap();
        let image = dataset.image(0).unwrap();
        let keys = image.geo_keys().unwrap();
        assert_eq!(
            keys.get_named("GeogSemiMajorAxisGeoKey")
                .and_then(|value| value.as_f64()),
            Some(6_378_137.0)
        );
        assert_eq!(
            keys.get_named("GeogInvFlatteningGeoKey")
                .and_then(|value| value.as_f64()),
            Some(298.257_223_563)
        );
        assert_eq!(
            keys.get_named("GeogCitationGeoKey")
                .and_then(|value| value.as_str()),
            Some("WGS 84")
        );
        assert_eq!(
            keys.get_named("PCSCitationGeoKey")
                .and_then(|value| value.as_str()),
            Some("test-ascii")
        );
    }

    #[test]
    fn rejects_ifd_larger_than_the_js_reservation() {
        let mut metadata = WriterMetadata::new(1, 1);
        metadata.set_tag(tag::TILE_BYTE_COUNTS, vec![1u32; 1000]);
        metadata.set_tag(tag::TILE_WIDTH, 1u16);
        metadata.set_tag(tag::TILE_LENGTH, 1u16);
        metadata.set_tag(tag::SAMPLES_PER_PIXEL, 1u16);
        let error = write_array_buffer(vec![1u8], metadata).unwrap_err();
        assert_eq!(error, WriterError::IfdTooLarge);
    }
}
