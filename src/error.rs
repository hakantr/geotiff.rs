use std::fmt;

/// Typed counterparts of the plain `Error`/`RangeError` cases raised by
/// geotiff.js, plus allocation/overflow failures required for safe native
/// processing of untrusted TIFF metadata.
#[derive(Debug, Clone, PartialEq)]
pub enum GeotiffError {
    /// globals.js `getFieldTypeSize`: `throw new RangeError('Invalid field type: ...')`
    InvalidFieldType(u16),
    /// compression/lzw.js `decompress`: `throw new Error('corrupted code at scanline ...')`
    CorruptedLzwCode(u32),
    /// compression/lzw.js `decompress`: `throw new Error('Invalid LZW code: ... with no previous code')`
    InvalidLzwCode(u32),
    /// geotiffimage.js `arrayForType`: `throw Error('Unsupported data format/bitsPerSample')`
    UnsupportedDataFormat(u8, u32),
    /// resample.js `resample`/`resampleInterleaved`: `throw new Error('Unsupported resampling method: ...')`
    UnsupportedResampleMethod(String),
    /// A raster size cannot be represented safely or does not match its data.
    InvalidRasterDimensions(String),
    /// A typed raster buffer could not be allocated.
    RasterAllocationFailed(String),
    /// A byte buffer cannot back the requested typed-array element width.
    InvalidTypedArrayByteLength { length: usize, element_size: usize },
    /// DataView/DataSlice read would extend outside the available bytes.
    OutOfBoundsByteRead {
        offset: u64,
        length: usize,
        available: usize,
    },
    /// geotiffimage.js `getOrigin`/`getResolution`: `throw new Error('The image does not have an affine transformation.')`
    NoAffineTransformation,
    /// A present affine tag does not contain enough numeric elements.
    InvalidAffineTransformation(String),
    /// geotiff.js `GeoTIFFBase.readRasters`: `throw new Error('Both "bbox" and "window" passed.')`
    BothBboxAndWindowPassed,
    /// geotiff.js `GeoTIFFBase.readRasters`: `throw new Error('Both width and resX passed')`
    BothWidthAndResXPassed,
    /// geotiff.js `GeoTIFFBase.readRasters`: `throw new Error('Both width and resY passed')` (sic -
    /// the original's own message says "width" here too, not "height"; kept verbatim for fidelity)
    BothWidthAndResYPassed,
}

impl fmt::Display for GeotiffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GeotiffError::InvalidFieldType(t) => write!(f, "Invalid field type: {t}"),
            GeotiffError::CorruptedLzwCode(c) => write!(f, "corrupted code at scanline {c}"),
            GeotiffError::InvalidLzwCode(c) => {
                write!(f, "Invalid LZW code: {c} with no previous code")
            }
            // Keep the public message byte-for-byte compatible with
            // geotiff.js. The values remain available in the typed variant
            // for native diagnostics without leaking into the JS contract.
            GeotiffError::UnsupportedDataFormat(_, _) => {
                write!(f, "Unsupported data format/bitsPerSample")
            }
            GeotiffError::UnsupportedResampleMethod(method) => {
                write!(f, "Unsupported resampling method: '{method}'")
            }
            GeotiffError::InvalidRasterDimensions(reason) => {
                write!(f, "Invalid raster dimensions: {reason}")
            }
            GeotiffError::RasterAllocationFailed(reason) => {
                write!(f, "Could not allocate raster output: {reason}")
            }
            GeotiffError::InvalidTypedArrayByteLength {
                length,
                element_size,
            } => write!(
                f,
                "Byte length {length} is not a multiple of typed-array element size {element_size}"
            ),
            GeotiffError::OutOfBoundsByteRead {
                offset,
                length,
                available,
            } => write!(
                f,
                "Byte read at offset {offset} with length {length} exceeds {available} available bytes"
            ),
            GeotiffError::NoAffineTransformation => {
                write!(f, "The image does not have an affine transformation.")
            }
            GeotiffError::InvalidAffineTransformation(reason) => {
                write!(f, "Invalid affine transformation metadata: {reason}")
            }
            GeotiffError::BothBboxAndWindowPassed => {
                write!(f, "Both \"bbox\" and \"window\" passed.")
            }
            GeotiffError::BothWidthAndResXPassed => write!(f, "Both width and resX passed"),
            GeotiffError::BothWidthAndResYPassed => write!(f, "Both width and resY passed"),
        }
    }
}

impl std::error::Error for GeotiffError {}
