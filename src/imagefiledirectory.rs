//! Lossless image-file-directory model corresponding to
//! `imagefiledirectory.js`.
//!
//! Raster decoding still uses async-tiff's compact typed IFD internally, but
//! callers receive this model from `GeoTiffImage::file_directory()`. It keeps
//! every known or vendor tag, its original field type/count, scalar-vs-array
//! shape, and the next-IFD pointer. This avoids the metadata loss inherent in
//! exposing only a dependency's fixed set of getters.

use crate::error::GeotiffError;
use crate::geokeys::GeoKeys;
use crate::globals::{FieldType, TagIdentifier, get_tag, resolve_tag};
use crate::typed_array::TypedArray;
use std::collections::BTreeMap;

/// Exact value of one TIFF tag after byte-order decoding.
#[derive(Debug, Clone, PartialEq)]
pub enum IfdValue {
    Unsigned(u64),
    Signed(i64),
    Float(f64),
    Ascii(String),
    UnsignedArray(Vec<u64>),
    SignedArray(Vec<i64>),
    FloatArray(Vec<f64>),
    UnsignedRational(u64, u64),
    SignedRational(i64, i64),
    UnsignedRationalArray(Vec<(u64, u64)>),
    SignedRationalArray(Vec<(i64, i64)>),
}

/// Scalar returned by the indexed-value API.
#[derive(Debug, Clone, PartialEq)]
pub enum IfdScalar {
    Unsigned(u64),
    Signed(i64),
    Float(f64),
    Ascii(String),
    UnsignedRational(u64, u64),
    SignedRational(i64, i64),
}

impl IfdValue {
    pub fn len(&self) -> usize {
        match self {
            Self::Unsigned(_) | Self::Signed(_) | Self::Float(_) => 1,
            // geotiff.js exposes TIFF RATIONAL values as a flat typed array
            // containing numerator/denominator components. Preserve the
            // lossless pair representation internally, but report the public
            // array length that callers migrating from JavaScript observe.
            Self::UnsignedRational(_, _) | Self::SignedRational(_, _) => 2,
            Self::Ascii(value) => value.chars().count(),
            Self::UnsignedArray(values) => values.len(),
            Self::SignedArray(values) => values.len(),
            Self::FloatArray(values) => values.len(),
            Self::UnsignedRationalArray(values) => values.len().saturating_mul(2),
            Self::SignedRationalArray(values) => values.len().saturating_mul(2),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn indexed(&self, index: usize) -> Option<IfdScalar> {
        match self {
            Self::Ascii(value) => value
                .chars()
                .nth(index)
                .map(|character| IfdScalar::Ascii(character.to_string())),
            Self::UnsignedArray(values) => values.get(index).copied().map(IfdScalar::Unsigned),
            Self::SignedArray(values) => values.get(index).copied().map(IfdScalar::Signed),
            Self::FloatArray(values) => values.get(index).copied().map(IfdScalar::Float),
            Self::UnsignedRational(numerator, denominator) => match index {
                0 => Some(IfdScalar::Unsigned(*numerator)),
                1 => Some(IfdScalar::Unsigned(*denominator)),
                _ => None,
            },
            Self::SignedRational(numerator, denominator) => match index {
                0 => Some(IfdScalar::Signed(*numerator)),
                1 => Some(IfdScalar::Signed(*denominator)),
                _ => None,
            },
            Self::UnsignedRationalArray(values) => values
                .get(index / 2)
                .map(|pair| {
                    if index.is_multiple_of(2) {
                        pair.0
                    } else {
                        pair.1
                    }
                })
                .map(IfdScalar::Unsigned),
            Self::SignedRationalArray(values) => values
                .get(index / 2)
                .map(|pair| {
                    if index.is_multiple_of(2) {
                        pair.0
                    } else {
                        pair.1
                    }
                })
                .map(IfdScalar::Signed),
            // JavaScript numeric scalars are not indexable.
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Unsigned(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_u64_slice(&self) -> Option<&[u64]> {
        match self {
            Self::UnsignedArray(values) => Some(values),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Unsigned(value) => Some(*value as f64),
            Self::Signed(value) => Some(*value as f64),
            Self::Float(value) => Some(*value),
            Self::UnsignedRational(n, d) if *d != 0 => Some(*n as f64 / *d as f64),
            Self::SignedRational(n, d) if *d != 0 => Some(*n as f64 / *d as f64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Ascii(value) => Some(value),
            _ => None,
        }
    }
}

/// One IFD entry including wire-level type/count metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct IfdEntry {
    pub tag: u16,
    pub field_type: FieldType,
    pub count: u64,
    pub value: IfdValue,
}

/// Fully actualized equivalent of geotiff.js `ImageFileDirectory`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileDirectory {
    entries: BTreeMap<u16, IfdEntry>,
    next_ifd_byte_offset: u64,
    geo_keys: Option<GeoKeys>,
}

impl FileDirectory {
    pub(crate) fn new(
        entries: BTreeMap<u16, IfdEntry>,
        next_ifd_byte_offset: u64,
        geo_keys: Option<GeoKeys>,
    ) -> Self {
        Self {
            entries,
            next_ifd_byte_offset,
            geo_keys,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn next_ifd_byte_offset(&self) -> u64 {
        self.next_ifd_byte_offset
    }

    pub fn has_tag<'a>(&self, identifier: impl Into<TagIdentifier<'a>>) -> bool {
        resolve_tag(identifier).is_some_and(|tag| self.entries.contains_key(&tag))
    }

    pub fn entry<'a>(&self, identifier: impl Into<TagIdentifier<'a>>) -> Option<&IfdEntry> {
        resolve_tag(identifier).and_then(|tag| self.entries.get(&tag))
    }

    /// `getValue()`. All entries are already actualized by the native parser,
    /// so the JavaScript deferred-field error state cannot occur.
    pub fn get_value<'a>(&self, identifier: impl Into<TagIdentifier<'a>>) -> Option<&IfdValue> {
        self.entry(identifier).map(|entry| &entry.value)
    }

    /// `loadValue()` compatibility spelling. Native metadata parsing is
    /// eager, therefore this resolves immediately without extra I/O.
    pub async fn load_value<'a>(
        &self,
        identifier: impl Into<TagIdentifier<'a>>,
    ) -> Option<&IfdValue> {
        self.get_value(identifier)
    }

    pub async fn load_value_indexed<'a>(
        &self,
        identifier: impl Into<TagIdentifier<'a>>,
        index: usize,
    ) -> Option<IfdScalar> {
        self.get_value(identifier)
            .and_then(|value| value.indexed(index))
    }

    pub fn parse_geo_key_directory(&self) -> Option<&GeoKeys> {
        self.geo_keys.as_ref()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&u16, &IfdEntry)> {
        self.entries.iter()
    }

    /// `toObject()`, with unknown/vendor tags named `Tag{number}`.
    pub fn to_object(&self) -> BTreeMap<String, IfdValue> {
        self.entries
            .iter()
            .map(|(&tag, entry)| {
                let name = get_tag(tag)
                    .map(|definition| definition.name)
                    .unwrap_or_else(|| format!("Tag{tag}"));
                (name, entry.value.clone())
            })
            .collect()
    }
}

/// `getArrayForSamples(fieldType, count)`.
pub fn get_array_for_samples(field_type: u16, count: usize) -> Result<TypedArray, GeotiffError> {
    let Some(ft) = FieldType::from_u16(field_type) else {
        return Err(GeotiffError::InvalidFieldType(field_type));
    };
    let output_count = if matches!(ft, FieldType::Rational | FieldType::SRational) {
        count.checked_mul(2).ok_or_else(|| {
            GeotiffError::InvalidRasterDimensions(
                "TIFF rational metadata element count overflow".to_string(),
            )
        })?
    } else {
        count
    };
    let prototype = match ft {
        FieldType::Byte | FieldType::Ascii | FieldType::Undefined => TypedArray::Uint8(Vec::new()),
        FieldType::SByte => TypedArray::Int8(Vec::new()),
        FieldType::Short => TypedArray::Uint16(Vec::new()),
        FieldType::SShort => TypedArray::Int16(Vec::new()),
        FieldType::Long | FieldType::Ifd => TypedArray::Uint32(Vec::new()),
        FieldType::SLong => TypedArray::Int32(Vec::new()),
        FieldType::Long8 | FieldType::Ifd8 => TypedArray::Uint64(Vec::new()),
        FieldType::SLong8 => TypedArray::Int64(Vec::new()),
        FieldType::Rational => TypedArray::Uint32(Vec::new()),
        FieldType::SRational => TypedArray::Int32(Vec::new()),
        FieldType::Float => TypedArray::Float32(Vec::new()),
        FieldType::Double => TypedArray::Float64(Vec::new()),
    };
    prototype.try_new_zeroed(output_count).map_err(|error| {
        GeotiffError::RasterAllocationFailed(format!(
            "could not allocate {output_count} metadata values: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocates_the_right_variant_and_length() {
        assert!(
            matches!(get_array_for_samples(3, 5).unwrap(), TypedArray::Uint16(v) if v.len() == 5)
        );
        assert!(
            matches!(get_array_for_samples(16, 2).unwrap(), TypedArray::Uint64(v) if v.len() == 2)
        );
        assert!(
            matches!(get_array_for_samples(17, 2).unwrap(), TypedArray::Int64(v) if v.len() == 2)
        );
    }

    #[test]
    fn rational_types_allocate_double_length_for_pairs() {
        assert!(
            matches!(get_array_for_samples(5, 3).unwrap(), TypedArray::Uint32(v) if v.len() == 6)
        );
        assert!(
            matches!(get_array_for_samples(10, 3).unwrap(), TypedArray::Int32(v) if v.len() == 6)
        );
    }

    #[test]
    fn rejects_invalid_field_type() {
        assert_eq!(
            get_array_for_samples(999, 1),
            Err(GeotiffError::InvalidFieldType(999))
        );
    }

    #[tokio::test]
    async fn directory_lookup_supports_names_numbers_and_vendor_tags() {
        let directory = FileDirectory::new(
            BTreeMap::from([
                (
                    256,
                    IfdEntry {
                        tag: 256,
                        field_type: FieldType::Long,
                        count: 1,
                        value: IfdValue::Unsigned(7),
                    },
                ),
                (
                    65_000,
                    IfdEntry {
                        tag: 65_000,
                        field_type: FieldType::Ascii,
                        count: 2,
                        value: IfdValue::Ascii("x\0".to_string()),
                    },
                ),
            ]),
            0,
            None,
        );
        assert!(directory.has_tag("ImageWidth"));
        assert_eq!(
            directory.get_value(256u16).and_then(IfdValue::as_u64),
            Some(7)
        );
        assert_eq!(
            directory
                .load_value(65_000u16)
                .await
                .and_then(IfdValue::as_str),
            Some("x\0")
        );
        assert_eq!(
            directory.to_object().get("Tag65000"),
            directory.get_value(65_000u16)
        );
    }

    #[test]
    fn rational_values_have_geotiff_js_flat_index_semantics() {
        let scalar = IfdValue::UnsignedRational(72, 1);
        assert_eq!(scalar.len(), 2);
        assert_eq!(scalar.indexed(0), Some(IfdScalar::Unsigned(72)));
        assert_eq!(scalar.indexed(1), Some(IfdScalar::Unsigned(1)));
        assert_eq!(scalar.indexed(2), None);

        let array = IfdValue::SignedRationalArray(vec![(-3, 2), (7, -4)]);
        assert_eq!(array.len(), 4);
        assert_eq!(array.indexed(0), Some(IfdScalar::Signed(-3)));
        assert_eq!(array.indexed(1), Some(IfdScalar::Signed(2)));
        assert_eq!(array.indexed(2), Some(IfdScalar::Signed(7)));
        assert_eq!(array.indexed(3), Some(IfdScalar::Signed(-4)));
        assert_eq!(array.indexed(4), None);
    }
}
