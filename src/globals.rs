//! Complete native counterpart of geotiff.js `globals.js`: TIFF field and
//! tag registries, writer field-type lookup, photometric/extra-sample/LERC
//! constants, and GeoKey name lookup.

use crate::error::GeotiffError;
use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

pub use crate::geokeys::{GEO_KEY_NAMES, geo_key_id, geo_key_name};

/// `fieldTypes`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldType {
    Byte = 1,
    Ascii = 2,
    Short = 3,
    Long = 4,
    Rational = 5,
    SByte = 6,
    Undefined = 7,
    SShort = 8,
    SLong = 9,
    SRational = 10,
    Float = 11,
    Double = 12,
    Ifd = 13,
    Long8 = 16,
    SLong8 = 17,
    Ifd8 = 18,
}

impl FieldType {
    pub const fn from_u16(v: u16) -> Option<FieldType> {
        match v {
            1 => Some(FieldType::Byte),
            2 => Some(FieldType::Ascii),
            3 => Some(FieldType::Short),
            4 => Some(FieldType::Long),
            5 => Some(FieldType::Rational),
            6 => Some(FieldType::SByte),
            7 => Some(FieldType::Undefined),
            8 => Some(FieldType::SShort),
            9 => Some(FieldType::SLong),
            10 => Some(FieldType::SRational),
            11 => Some(FieldType::Float),
            12 => Some(FieldType::Double),
            13 => Some(FieldType::Ifd),
            16 => Some(FieldType::Long8),
            17 => Some(FieldType::SLong8),
            18 => Some(FieldType::Ifd8),
            _ => None,
        }
    }
}

/// `fieldTypeSizes`, indexed through [`get_field_type_size`].
pub const FIELD_TYPE_SIZES: &[(FieldType, u8)] = &[
    (FieldType::Byte, 1),
    (FieldType::Ascii, 1),
    (FieldType::Short, 2),
    (FieldType::Long, 4),
    (FieldType::Rational, 8),
    (FieldType::SByte, 1),
    (FieldType::Undefined, 1),
    (FieldType::SShort, 2),
    (FieldType::SLong, 4),
    (FieldType::SRational, 8),
    (FieldType::Float, 4),
    (FieldType::Double, 8),
    (FieldType::Ifd, 4),
    (FieldType::Long8, 8),
    (FieldType::SLong8, 8),
    (FieldType::Ifd8, 8),
];

/// Numeric values of `photometricInterpretations`.
pub mod photometric_interpretations {
    pub const WHITE_IS_ZERO: u16 = 0;
    pub const BLACK_IS_ZERO: u16 = 1;
    pub const RGB: u16 = 2;
    pub const PALETTE: u16 = 3;
    pub const TRANSPARENCY_MASK: u16 = 4;
    pub const CMYK: u16 = 5;
    pub const Y_CB_CR: u16 = 6;
    pub const CIE_LAB: u16 = 8;
    pub const ICC_LAB: u16 = 9;
}

/// Numeric values of `ExtraSamplesValues`.
pub mod extra_samples_values {
    pub const UNSPECIFIED: u16 = 0;
    pub const ASSOCIATED_ALPHA: u16 = 1;
    pub const UNASSOCIATED_ALPHA: u16 = 2;
}

/// Indices in the TIFF `LercParameters` array.
pub mod lerc_parameters {
    pub const VERSION: usize = 0;
    pub const ADD_COMPRESSION: usize = 1;
}

/// Values stored at `LercParameters[ADD_COMPRESSION]`.
pub mod lerc_add_compression {
    pub const NONE: u32 = 0;
    pub const DEFLATE: u32 = 1;
    pub const ZSTANDARD: u32 = 2;
}

/// `fieldTagTypes` used by the JavaScript writer. GeoKey types not present
/// in that table intentionally return `None`, matching the source object.
pub fn field_tag_type(tag: u16) -> Option<FieldType> {
    Some(match tag {
        256 | 257 | 258 | 259 | 262 | 274 | 277 | 284 | 286 | 296 | 297 | 322 | 323 | 338 | 339
        | 1024 | 1025 | 2048 | 2052 | 2054 | 2060 | 3072 | 3076 | 4096 | 4099 | 34735 => {
            FieldType::Short
        }
        273 | 278 | 279 | 324 | 325 | 513 | 514 | 34665 => FieldType::Long,
        282 | 283 | 287 => FieldType::Rational,
        270 | 271 | 272 | 305 | 306 | 315 | 1026 | 2049 | 3073 | 4097 | 33432 | 34737 | 42113 => {
            FieldType::Ascii
        }
        2057 | 2059 | 33550 | 33922 | 34264 | 34736 => FieldType::Double,
        _ => return None,
    })
}

/// `getFieldTypeSize(fieldType)`. Takes a raw `u16` (not `FieldType`)
/// because the original validates values read straight out of untrusted
/// TIFF bytes - typing the parameter as the enum would silently relocate
/// that validation instead of preserving it.
pub fn get_field_type_size(field_type: u16) -> Result<u8, GeotiffError> {
    match field_type {
        1 | 2 | 6 | 7 => Ok(1),
        3 | 8 => Ok(2),
        4 | 9 | 11 | 13 => Ok(4),
        5 | 10 | 12 | 16 | 17 | 18 => Ok(8),
        _ => Err(GeotiffError::InvalidFieldType(field_type)),
    }
}

struct RawEntry {
    key: &'static str,
    tag: u16,
    name: Option<&'static str>,
    field_type: Option<FieldType>,
    is_array: bool,
    eager: bool,
}

/// `tagDictionary`, transcribed verbatim from globals.js (98 entries).
static TAG_DICTIONARY: &[RawEntry] = &[
    RawEntry {
        key: "NewSubfileType",
        tag: 254,
        name: None,
        field_type: Some(FieldType::Long),
        is_array: false,
        eager: true,
    },
    RawEntry {
        key: "SubfileType",
        tag: 255,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: true,
    },
    RawEntry {
        key: "ImageWidth",
        tag: 256,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: true,
    },
    RawEntry {
        key: "ImageLength",
        tag: 257,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: true,
    },
    RawEntry {
        key: "BitsPerSample",
        tag: 258,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: true,
        eager: true,
    },
    RawEntry {
        key: "Compression",
        tag: 259,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: true,
    },
    RawEntry {
        key: "PhotometricInterpretation",
        tag: 262,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: true,
    },
    RawEntry {
        key: "Threshholding",
        tag: 263,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "CellWidth",
        tag: 264,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "CellLength",
        tag: 265,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "FillOrder",
        tag: 266,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "DocumentName",
        tag: 269,
        name: None,
        field_type: Some(FieldType::Ascii),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "ImageDescription",
        tag: 270,
        name: None,
        field_type: Some(FieldType::Ascii),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "Make",
        tag: 271,
        name: None,
        field_type: Some(FieldType::Ascii),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "Model",
        tag: 272,
        name: None,
        field_type: Some(FieldType::Ascii),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "StripOffsets",
        tag: 273,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "Orientation",
        tag: 274,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "SamplesPerPixel",
        tag: 277,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: true,
    },
    RawEntry {
        key: "RowsPerStrip",
        tag: 278,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: true,
    },
    RawEntry {
        key: "StripByteCounts",
        tag: 279,
        name: None,
        field_type: Some(FieldType::Long),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "MinSampleValue",
        tag: 280,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "MaxSampleValue",
        tag: 281,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "XResolution",
        tag: 282,
        name: None,
        field_type: Some(FieldType::Rational),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "YResolution",
        tag: 283,
        name: None,
        field_type: Some(FieldType::Rational),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "PlanarConfiguration",
        tag: 284,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: true,
    },
    RawEntry {
        key: "PageName",
        tag: 285,
        name: None,
        field_type: Some(FieldType::Ascii),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "XPosition",
        tag: 286,
        name: None,
        field_type: Some(FieldType::Rational),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "YPosition",
        tag: 287,
        name: None,
        field_type: Some(FieldType::Rational),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "FreeOffsets",
        tag: 288,
        name: None,
        field_type: Some(FieldType::Long),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "FreeByteCounts",
        tag: 289,
        name: None,
        field_type: Some(FieldType::Long),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "GrayResponseUnit",
        tag: 290,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "GrayResponseCurve",
        tag: 291,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "T4Options",
        tag: 292,
        name: None,
        field_type: Some(FieldType::Long),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "T6Options",
        tag: 293,
        name: None,
        field_type: Some(FieldType::Long),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "ResolutionUnit",
        tag: 296,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "PageNumber",
        tag: 297,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "TransferFunction",
        tag: 301,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "Software",
        tag: 305,
        name: None,
        field_type: Some(FieldType::Ascii),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "DateTime",
        tag: 306,
        name: None,
        field_type: Some(FieldType::Ascii),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "Artist",
        tag: 315,
        name: None,
        field_type: Some(FieldType::Ascii),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "HostComputer",
        tag: 316,
        name: None,
        field_type: Some(FieldType::Ascii),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "Predictor",
        tag: 317,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "WhitePoint",
        tag: 318,
        name: None,
        field_type: Some(FieldType::Rational),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "PrimaryChromaticities",
        tag: 319,
        name: None,
        field_type: Some(FieldType::Rational),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "ColorMap",
        tag: 320,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "HalftoneHints",
        tag: 321,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "TileWidth",
        tag: 322,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: true,
    },
    RawEntry {
        key: "TileLength",
        tag: 323,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: true,
    },
    RawEntry {
        key: "TileOffsets",
        tag: 324,
        name: None,
        field_type: Some(FieldType::Long),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "TileByteCounts",
        tag: 325,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "InkSet",
        tag: 332,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "InkNames",
        tag: 333,
        name: None,
        field_type: Some(FieldType::Ascii),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "NumberOfInks",
        tag: 334,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "DotRange",
        tag: 336,
        name: None,
        field_type: Some(FieldType::Byte),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "TargetPrinter",
        tag: 337,
        name: None,
        field_type: Some(FieldType::Ascii),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "ExtraSamples",
        tag: 338,
        name: None,
        field_type: Some(FieldType::Byte),
        is_array: true,
        eager: true,
    },
    RawEntry {
        key: "SampleFormat",
        tag: 339,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: true,
        eager: true,
    },
    RawEntry {
        key: "SMinSampleValue",
        tag: 340,
        name: None,
        field_type: None,
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "SMaxSampleValue",
        tag: 341,
        name: None,
        field_type: None,
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "TransferRange",
        tag: 342,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "JPEGProc",
        tag: 512,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "JPEGInterchangeFormat",
        tag: 513,
        name: None,
        field_type: Some(FieldType::Long),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "JPEGInterchangeFormatLngth",
        tag: 514,
        name: None,
        field_type: Some(FieldType::Long),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "JPEGRestartInterval",
        tag: 515,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "JPEGLosslessPredictors",
        tag: 517,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "JPEGPointTransforms",
        tag: 518,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "JPEGQTables",
        tag: 519,
        name: None,
        field_type: Some(FieldType::Long),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "JPEGDCTables",
        tag: 520,
        name: None,
        field_type: Some(FieldType::Long),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "JPEGACTables",
        tag: 521,
        name: None,
        field_type: Some(FieldType::Long),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "YCbCrCoefficients",
        tag: 529,
        name: None,
        field_type: Some(FieldType::Rational),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "YCbCrSubSampling",
        tag: 530,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "YCbCrPositioning",
        tag: 531,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "ReferenceBlackWhite",
        tag: 532,
        name: None,
        field_type: Some(FieldType::Long),
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "Copyright",
        tag: 33432,
        name: None,
        field_type: Some(FieldType::Ascii),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "BadFaxLines",
        tag: 326,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "CleanFaxData",
        tag: 327,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "ClipPath",
        tag: 343,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "ConsecutiveBadFaxLines",
        tag: 328,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "Decode",
        tag: 433,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "DefaultImageColor",
        tag: 434,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "Indexed",
        tag: 346,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "JPEGTables",
        tag: 347,
        name: None,
        field_type: None,
        is_array: true,
        eager: true,
    },
    RawEntry {
        key: "StripRowCounts",
        tag: 559,
        name: None,
        field_type: None,
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "SubIFDs",
        tag: 330,
        name: None,
        field_type: None,
        is_array: true,
        eager: false,
    },
    RawEntry {
        key: "XClipPathUnits",
        tag: 344,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "YClipPathUnits",
        tag: 345,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "ApertureValue",
        tag: 37378,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "ColorSpace",
        tag: 40961,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "DateTimeDigitized",
        tag: 36868,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "DateTimeOriginal",
        tag: 36867,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "ExifIFD",
        tag: 34665,
        name: Some("Exif IFD"),
        field_type: Some(FieldType::Long),
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "ExifVersion",
        tag: 36864,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "ExposureTime",
        tag: 33434,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "FileSource",
        tag: 41728,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "Flash",
        tag: 37385,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "FlashpixVersion",
        tag: 40960,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "FNumber",
        tag: 33437,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "ImageUniqueID",
        tag: 42016,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "LightSource",
        tag: 37384,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "MakerNote",
        tag: 37500,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "ShutterSpeedValue",
        tag: 37377,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "UserComment",
        tag: 37510,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "IPTC",
        tag: 33723,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "CZ_LSMINFO",
        tag: 34412,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "ICCProfile",
        tag: 34675,
        name: Some("ICC Profile"),
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "XMP",
        tag: 700,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "GDAL_METADATA",
        tag: 42112,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "GDAL_NODATA",
        tag: 42113,
        name: None,
        field_type: Some(FieldType::Ascii),
        is_array: false,
        eager: true,
    },
    RawEntry {
        key: "Photoshop",
        tag: 34377,
        name: None,
        field_type: None,
        is_array: false,
        eager: false,
    },
    RawEntry {
        key: "ModelPixelScale",
        tag: 33550,
        name: None,
        field_type: Some(FieldType::Double),
        is_array: true,
        eager: true,
    },
    RawEntry {
        key: "ModelTiepoint",
        tag: 33922,
        name: None,
        field_type: Some(FieldType::Double),
        is_array: true,
        eager: true,
    },
    RawEntry {
        key: "ModelTransformation",
        tag: 34264,
        name: None,
        field_type: Some(FieldType::Double),
        is_array: true,
        eager: true,
    },
    RawEntry {
        key: "GeoKeyDirectory",
        tag: 34735,
        name: None,
        field_type: Some(FieldType::Short),
        is_array: true,
        eager: true,
    },
    RawEntry {
        key: "GeoDoubleParams",
        tag: 34736,
        name: None,
        field_type: Some(FieldType::Double),
        is_array: true,
        eager: true,
    },
    RawEntry {
        key: "GeoAsciiParams",
        tag: 34737,
        name: None,
        field_type: Some(FieldType::Ascii),
        is_array: false,
        eager: true,
    },
    RawEntry {
        key: "LercParameters",
        tag: 50674,
        name: None,
        field_type: None,
        is_array: false,
        eager: true,
    },
];

/// `tagDefinitions[tag]`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagDefinition {
    pub tag: u16,
    pub name: String,
    pub field_type: Option<FieldType>,
    pub is_array: bool,
    pub eager: bool,
}

struct Registry {
    tags: HashMap<String, u16>,
    tag_definitions: HashMap<u16, TagDefinition>,
}

fn register_tag_in(
    reg: &mut Registry,
    tag: u16,
    name: &str,
    field_type: Option<FieldType>,
    is_array: bool,
    eager: bool,
) {
    reg.tags.insert(name.to_string(), tag);
    reg.tag_definitions.insert(
        tag,
        TagDefinition {
            tag,
            name: name.to_string(),
            field_type,
            is_array,
            eager,
        },
    );
}

static REGISTRY: LazyLock<RwLock<Registry>> = LazyLock::new(|| {
    let mut reg = Registry {
        tags: HashMap::new(),
        tag_definitions: HashMap::new(),
    };
    for entry in TAG_DICTIONARY {
        register_tag_in(
            &mut reg,
            entry.tag,
            entry.name.unwrap_or(entry.key),
            entry.field_type,
            entry.is_array,
            entry.eager,
        );
    }
    RwLock::new(reg)
});

/// `registerTag(tag, name, type, isArray, eager)` - public API for
/// registering custom/vendor TIFF tags at runtime, same as the original.
pub fn register_tag(
    tag: u16,
    name: &str,
    field_type: Option<FieldType>,
    is_array: bool,
    eager: bool,
) {
    let mut registry = REGISTRY
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    register_tag_in(&mut registry, tag, name, field_type, is_array, eager);
}

/// `number|string` union `tagIdentifier` parameter shared by `resolveTag`/`getTag`.
pub enum TagIdentifier<'a> {
    Number(u16),
    Name(&'a str),
}

impl From<u16> for TagIdentifier<'static> {
    fn from(n: u16) -> Self {
        TagIdentifier::Number(n)
    }
}

impl<'a> From<&'a str> for TagIdentifier<'a> {
    fn from(s: &'a str) -> Self {
        TagIdentifier::Name(s)
    }
}

/// `resolveTag(tagIdentifier)`
pub fn resolve_tag<'a>(identifier: impl Into<TagIdentifier<'a>>) -> Option<u16> {
    match identifier.into() {
        TagIdentifier::Number(n) => Some(n),
        TagIdentifier::Name(name) => REGISTRY
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .tags
            .get(name)
            .copied(),
    }
}

/// `getTag(tagIdentifier)`
pub fn get_tag<'a>(identifier: impl Into<TagIdentifier<'a>>) -> Option<TagDefinition> {
    let tag = resolve_tag(identifier)?;
    REGISTRY
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .tag_definitions
        .get(&tag)
        .cloned()
}

/// Snapshot of the exported `tags` name-to-ID object, including runtime
/// registrations.
pub fn tags() -> HashMap<String, u16> {
    REGISTRY
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .tags
        .clone()
}

/// Snapshot of the exported `tagDefinitions` ID-to-definition object.
pub fn tag_definitions() -> HashMap<u16, TagDefinition> {
    REGISTRY
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .tag_definitions
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_field_type_size_matches_js_table() {
        assert_eq!(get_field_type_size(1).unwrap(), 1); // BYTE
        assert_eq!(get_field_type_size(3).unwrap(), 2); // SHORT
        assert_eq!(get_field_type_size(4).unwrap(), 4); // LONG
        assert_eq!(get_field_type_size(12).unwrap(), 8); // DOUBLE
        assert!(get_field_type_size(999).is_err());
    }

    #[test]
    fn built_in_tags_are_registered_at_startup() {
        assert_eq!(resolve_tag("ImageWidth"), Some(256));
        assert_eq!(resolve_tag(256u16), Some(256)); // numbers pass through unchecked, like the original
        assert_eq!(resolve_tag("NotARealTag"), None);
    }

    #[test]
    fn name_field_overrides_dictionary_key() {
        let def = get_tag("Exif IFD").unwrap();
        assert_eq!(def.tag, 34665);
        assert!(get_tag("ExifIFD").is_none()); // the dictionary key itself is not registered when `name` overrides it
    }

    #[test]
    fn register_tag_adds_custom_entries_at_runtime() {
        register_tag(60000, "MyCustomTag", Some(FieldType::Short), false, false);
        assert_eq!(resolve_tag("MyCustomTag"), Some(60000));
        assert_eq!(get_tag("MyCustomTag").unwrap().tag, 60000);
    }
}
