//! Compatibility fixes applied at the byte-reader boundary before metadata
//! reaches `async-tiff`.
//!
//! geotiff.js's writer separates ASCII GeoKey values with NUL bytes and
//! includes each terminator in the key's `Count`. async-tiff 0.3 truncates
//! `GeoAsciiParams` at the *first* NUL and then slices that shortened string
//! using the original offsets/counts, which either panics or loses all later
//! keys. GeoTIFF also permits `|` separators, which async-tiff handles
//! correctly. This adapter therefore exposes a virtual `NUL -> |` patch at
//! each referenced value terminator. The source itself is never mutated,
//! and all other bytes pass through unchanged.

use crate::decode_pool::{CancellationToken, cancellable};
use crate::geokeys::{GeoKeys, ParsedGeoKeyValue};
use crate::globals::{FieldType, get_tag};
use crate::imagefiledirectory::{FileDirectory, IfdEntry, IfdValue};
use async_tiff::error::{AsyncTiffError, AsyncTiffResult};
use async_tiff::reader::{AsyncFileReader, Endianness};
use async_tiff::tags::Tag;
use async_tiff::{ImageFileDirectory, TIFF, TagValue};
use async_trait::async_trait;
use bytes::Bytes;
use std::collections::{BTreeMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

const PREFIX_SIZE: u64 = 64 * 1024;
const MAX_IFDS: usize = 65_536;
const MAX_IFD_ENTRIES: u64 = 1_000_000;
const GEO_KEY_DIRECTORY: u16 = 34735;
const GEO_ASCII_PARAMS: u16 = 34737;

#[derive(Debug, Clone, Copy)]
enum ByteOrder {
    Little,
    Big,
}

impl ByteOrder {
    fn u16(self, bytes: &[u8]) -> u16 {
        match self {
            ByteOrder::Little => u16::from_le_bytes([bytes[0], bytes[1]]),
            ByteOrder::Big => u16::from_be_bytes([bytes[0], bytes[1]]),
        }
    }

    fn u32(self, bytes: &[u8]) -> u32 {
        let mut array = [0; 4];
        array.copy_from_slice(bytes);
        match self {
            ByteOrder::Little => u32::from_le_bytes(array),
            ByteOrder::Big => u32::from_be_bytes(array),
        }
    }

    fn u64(self, bytes: &[u8]) -> u64 {
        let mut array = [0; 8];
        array.copy_from_slice(bytes);
        match self {
            ByteOrder::Little => u64::from_le_bytes(array),
            ByteOrder::Big => u64::from_be_bytes(array),
        }
    }

    fn i16(self, bytes: &[u8]) -> i16 {
        match self {
            ByteOrder::Little => i16::from_le_bytes([bytes[0], bytes[1]]),
            ByteOrder::Big => i16::from_be_bytes([bytes[0], bytes[1]]),
        }
    }

    fn i32(self, bytes: &[u8]) -> i32 {
        let mut array = [0; 4];
        array.copy_from_slice(bytes);
        match self {
            ByteOrder::Little => i32::from_le_bytes(array),
            ByteOrder::Big => i32::from_be_bytes(array),
        }
    }

    fn i64(self, bytes: &[u8]) -> i64 {
        let mut array = [0; 8];
        array.copy_from_slice(bytes);
        match self {
            ByteOrder::Little => i64::from_le_bytes(array),
            ByteOrder::Big => i64::from_be_bytes(array),
        }
    }

    fn f32(self, bytes: &[u8]) -> f32 {
        f32::from_bits(self.u32(bytes))
    }

    fn f64(self, bytes: &[u8]) -> f64 {
        f64::from_bits(self.u64(bytes))
    }
}

#[derive(Debug, Clone, Copy)]
struct TagLocation {
    absolute_offset: u64,
    byte_len: u64,
    field_type: u16,
    count: u64,
}

#[derive(Debug)]
struct MetadataCompatibilityReader {
    inner: Arc<dyn AsyncFileReader>,
    prefix: Bytes,
    patches: BTreeMap<u64, u8>,
    cancellation: Option<CancellationToken>,
}

impl MetadataCompatibilityReader {
    async fn read_unpatched(&self, range: Range<u64>) -> AsyncTiffResult<Bytes> {
        if range.start >= range.end {
            return Ok(Bytes::new());
        }
        if let (Ok(start), Ok(end)) = (usize::try_from(range.start), usize::try_from(range.end))
            && end <= self.prefix.len()
        {
            return Ok(self.prefix.slice(start..end));
        }
        cancellable(self.inner.get_bytes(range), self.cancellation.as_ref()).await
    }

    async fn read_exact(&self, offset: u64, len: usize) -> AsyncTiffResult<Bytes> {
        let end = offset
            .checked_add(len as u64)
            .ok_or_else(|| AsyncTiffError::General("metadata byte range overflow".to_string()))?;
        let bytes = self.read_unpatched(offset..end).await?;
        if bytes.len() != len {
            return Err(AsyncTiffError::General(format!(
                "Unexpected end of TIFF metadata at byte {offset}: expected {len}, got {}",
                bytes.len()
            )));
        }
        Ok(bytes)
    }

    async fn tag_bytes(&self, location: TagLocation) -> AsyncTiffResult<Bytes> {
        let len = usize::try_from(location.byte_len)
            .map_err(|_| AsyncTiffError::General("metadata tag is too large".to_string()))?;
        self.read_exact(location.absolute_offset, len).await
    }

    fn apply_patches(&self, range: &Range<u64>, bytes: Bytes) -> Bytes {
        if self.patches.is_empty() || bytes.is_empty() {
            return bytes;
        }
        let returned_end = range.start.saturating_add(bytes.len() as u64);
        if self
            .patches
            .range(range.start..returned_end)
            .next()
            .is_none()
        {
            return bytes;
        }
        let mut output = bytes.to_vec();
        for (&absolute, &value) in self.patches.range(range.start..returned_end) {
            output[(absolute - range.start) as usize] = value;
        }
        Bytes::from(output)
    }
}

#[async_trait]
impl AsyncFileReader for MetadataCompatibilityReader {
    async fn get_bytes(&self, range: Range<u64>) -> AsyncTiffResult<Bytes> {
        let bytes = self.read_unpatched(range.clone()).await?;
        Ok(self.apply_patches(&range, bytes))
    }
}

fn field_type_size(field_type: u16) -> Option<u64> {
    match field_type {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 | 13 => Some(4),
        5 | 10 | 12 | 16 | 17 | 18 => Some(8),
        _ => None,
    }
}

fn parse_tag_location(
    entry: &[u8],
    entry_absolute: u64,
    order: ByteOrder,
    big_tiff: bool,
) -> AsyncTiffResult<(u16, TagLocation)> {
    let tag = order.u16(&entry[0..2]);
    let field_type = order.u16(&entry[2..4]);
    let type_size = field_type_size(field_type).ok_or_else(|| {
        AsyncTiffError::General(format!(
            "Invalid field type {field_type} in metadata compatibility scan"
        ))
    })?;
    let (count, value_field_offset, inline_size) = if big_tiff {
        (order.u64(&entry[4..12]), 12usize, 8u64)
    } else {
        (u64::from(order.u32(&entry[4..8])), 8usize, 4u64)
    };
    let byte_len = count
        .checked_mul(type_size)
        .ok_or_else(|| AsyncTiffError::General("metadata tag length overflow".to_string()))?;
    let absolute_offset = if byte_len <= inline_size {
        entry_absolute + value_field_offset as u64
    } else if big_tiff {
        order.u64(&entry[value_field_offset..value_field_offset + 8])
    } else {
        u64::from(order.u32(&entry[value_field_offset..value_field_offset + 4]))
    };
    Ok((
        tag,
        TagLocation {
            absolute_offset,
            byte_len,
            field_type,
            count,
        },
    ))
}

fn invalid_metadata(message: impl Into<String>) -> AsyncTiffError {
    AsyncTiffError::General(message.into())
}

fn checked_value_range(
    location: TagLocation,
    offset: u16,
    count: u16,
    element_size: usize,
    bytes_len: usize,
) -> AsyncTiffResult<Range<usize>> {
    let start_element = u64::from(offset);
    let end_element = start_element
        .checked_add(u64::from(count))
        .ok_or_else(|| invalid_metadata("GeoKey value range overflow"))?;
    if end_element > location.count {
        return Err(invalid_metadata(format!(
            "GeoKey references values {start_element}..{end_element} in tag with {} values",
            location.count
        )));
    }
    let start = usize::try_from(start_element)
        .ok()
        .and_then(|value| value.checked_mul(element_size))
        .ok_or_else(|| invalid_metadata("GeoKey byte offset overflow"))?;
    let end = usize::try_from(end_element)
        .ok()
        .and_then(|value| value.checked_mul(element_size))
        .ok_or_else(|| invalid_metadata("GeoKey byte range overflow"))?;
    if end > bytes_len {
        return Err(invalid_metadata("GeoKey value range exceeds its TIFF tag"));
    }
    Ok(start..end)
}

fn scalar_or_unsigned(values: Vec<u64>) -> ParsedGeoKeyValue {
    match values.as_slice() {
        [value] => ParsedGeoKeyValue::Unsigned(*value),
        _ => ParsedGeoKeyValue::UnsignedArray(values),
    }
}

fn scalar_or_signed(values: Vec<i64>) -> ParsedGeoKeyValue {
    match values.as_slice() {
        [value] => ParsedGeoKeyValue::Signed(*value),
        _ => ParsedGeoKeyValue::SignedArray(values),
    }
}

fn scalar_or_float(values: Vec<f64>) -> ParsedGeoKeyValue {
    match values.as_slice() {
        [value] => ParsedGeoKeyValue::Float(*value),
        _ => ParsedGeoKeyValue::FloatArray(values),
    }
}

fn public_unsigned(values: Vec<u64>, force_array: bool) -> IfdValue {
    match values.as_slice() {
        [value] if !force_array => IfdValue::Unsigned(*value),
        _ => IfdValue::UnsignedArray(values),
    }
}

fn public_signed(values: Vec<i64>, force_array: bool) -> IfdValue {
    match values.as_slice() {
        [value] if !force_array => IfdValue::Signed(*value),
        _ => IfdValue::SignedArray(values),
    }
}

fn public_float(values: Vec<f64>, force_array: bool) -> IfdValue {
    match values.as_slice() {
        [value] if !force_array => IfdValue::Float(*value),
        _ => IfdValue::FloatArray(values),
    }
}

fn decode_ifd_value(
    tag: u16,
    location: TagLocation,
    bytes: &[u8],
    order: ByteOrder,
) -> AsyncTiffResult<IfdValue> {
    let force_array = get_tag(tag).is_some_and(|definition| definition.is_array);
    let count = usize::try_from(location.count)
        .map_err(|_| invalid_metadata(format!("TIFF tag {tag} has too many values")))?;
    match location.field_type {
        2 => Ok(IfdValue::Ascii(String::from_utf8_lossy(bytes).into_owned())),
        1 | 7 => Ok(public_unsigned(
            bytes.iter().map(|&value| u64::from(value)).collect(),
            force_array,
        )),
        6 => Ok(public_signed(
            bytes.iter().map(|&value| i64::from(value as i8)).collect(),
            force_array,
        )),
        3 => Ok(public_unsigned(
            bytes
                .chunks_exact(2)
                .map(|chunk| u64::from(order.u16(chunk)))
                .collect(),
            force_array,
        )),
        8 => Ok(public_signed(
            bytes
                .chunks_exact(2)
                .map(|chunk| i64::from(order.i16(chunk)))
                .collect(),
            force_array,
        )),
        4 | 13 => Ok(public_unsigned(
            bytes
                .chunks_exact(4)
                .map(|chunk| u64::from(order.u32(chunk)))
                .collect(),
            force_array,
        )),
        9 => Ok(public_signed(
            bytes
                .chunks_exact(4)
                .map(|chunk| i64::from(order.i32(chunk)))
                .collect(),
            force_array,
        )),
        11 => Ok(public_float(
            bytes
                .chunks_exact(4)
                .map(|chunk| f64::from(order.f32(chunk)))
                .collect(),
            force_array,
        )),
        12 => Ok(public_float(
            bytes
                .chunks_exact(8)
                .map(|chunk| order.f64(chunk))
                .collect(),
            force_array,
        )),
        16 | 18 => Ok(public_unsigned(
            bytes
                .chunks_exact(8)
                .map(|chunk| order.u64(chunk))
                .collect(),
            force_array,
        )),
        17 => Ok(public_signed(
            bytes
                .chunks_exact(8)
                .map(|chunk| order.i64(chunk))
                .collect(),
            force_array,
        )),
        5 => {
            let values = bytes
                .chunks_exact(8)
                .map(|chunk| {
                    (
                        u64::from(order.u32(&chunk[..4])),
                        u64::from(order.u32(&chunk[4..])),
                    )
                })
                .collect::<Vec<_>>();
            if values.len() != count {
                return Err(invalid_metadata(format!("TIFF tag {tag} is truncated")));
            }
            // geotiff.js keeps rational numerator/denominator values in an
            // array even when Count is one.
            Ok(IfdValue::UnsignedRationalArray(values))
        }
        10 => {
            let values = bytes
                .chunks_exact(8)
                .map(|chunk| {
                    (
                        i64::from(order.i32(&chunk[..4])),
                        i64::from(order.i32(&chunk[4..])),
                    )
                })
                .collect::<Vec<_>>();
            if values.len() != count {
                return Err(invalid_metadata(format!("TIFF tag {tag} is truncated")));
            }
            Ok(IfdValue::SignedRationalArray(values))
        }
        field_type => Err(invalid_metadata(format!(
            "Invalid field type {field_type} in TIFF tag {tag}"
        ))),
    }
}

fn list_or_one<T>(mut values: Vec<T>, wrap: impl Fn(T) -> TagValue) -> TagValue {
    if values.len() == 1 {
        wrap(values.remove(0))
    } else {
        TagValue::List(values.into_iter().map(wrap).collect())
    }
}

fn async_tag_value(entry: &IfdEntry) -> AsyncTiffResult<TagValue> {
    let mismatch = || {
        invalid_metadata(format!(
            "TIFF tag {} value does not match field type {}",
            entry.tag, entry.field_type as u16
        ))
    };
    match (entry.field_type, &entry.value) {
        (FieldType::Ascii, IfdValue::Ascii(value)) => Ok(TagValue::Ascii(value.clone())),
        (FieldType::Byte | FieldType::Undefined, IfdValue::Unsigned(value)) => Ok(TagValue::Byte(
            u8::try_from(*value).map_err(|_| mismatch())?,
        )),
        (FieldType::Byte | FieldType::Undefined, IfdValue::UnsignedArray(values)) => {
            Ok(list_or_one(
                values
                    .iter()
                    .map(|&value| u8::try_from(value).map_err(|_| mismatch()))
                    .collect::<Result<Vec<_>, _>>()?,
                TagValue::Byte,
            ))
        }
        (FieldType::SByte, IfdValue::Signed(value)) => Ok(TagValue::SignedByte(
            i8::try_from(*value).map_err(|_| mismatch())?,
        )),
        (FieldType::SByte, IfdValue::SignedArray(values)) => Ok(list_or_one(
            values
                .iter()
                .map(|&value| i8::try_from(value).map_err(|_| mismatch()))
                .collect::<Result<Vec<_>, _>>()?,
            TagValue::SignedByte,
        )),
        (FieldType::Short, IfdValue::Unsigned(value)) => Ok(TagValue::Short(
            u16::try_from(*value).map_err(|_| mismatch())?,
        )),
        (FieldType::Short, IfdValue::UnsignedArray(values)) => Ok(list_or_one(
            values
                .iter()
                .map(|&value| u16::try_from(value).map_err(|_| mismatch()))
                .collect::<Result<Vec<_>, _>>()?,
            TagValue::Short,
        )),
        (FieldType::SShort, IfdValue::Signed(value)) => Ok(TagValue::SignedShort(
            i16::try_from(*value).map_err(|_| mismatch())?,
        )),
        (FieldType::SShort, IfdValue::SignedArray(values)) => Ok(list_or_one(
            values
                .iter()
                .map(|&value| i16::try_from(value).map_err(|_| mismatch()))
                .collect::<Result<Vec<_>, _>>()?,
            TagValue::SignedShort,
        )),
        (FieldType::Long, IfdValue::Unsigned(value)) => Ok(TagValue::Unsigned(
            u32::try_from(*value).map_err(|_| mismatch())?,
        )),
        (FieldType::Long, IfdValue::UnsignedArray(values)) => Ok(list_or_one(
            values
                .iter()
                .map(|&value| u32::try_from(value).map_err(|_| mismatch()))
                .collect::<Result<Vec<_>, _>>()?,
            TagValue::Unsigned,
        )),
        (FieldType::Ifd, IfdValue::Unsigned(value)) => Ok(TagValue::Ifd(
            u32::try_from(*value).map_err(|_| mismatch())?,
        )),
        (FieldType::Ifd, IfdValue::UnsignedArray(values)) => Ok(list_or_one(
            values
                .iter()
                .map(|&value| u32::try_from(value).map_err(|_| mismatch()))
                .collect::<Result<Vec<_>, _>>()?,
            TagValue::Ifd,
        )),
        (FieldType::SLong, IfdValue::Signed(value)) => Ok(TagValue::Signed(
            i32::try_from(*value).map_err(|_| mismatch())?,
        )),
        (FieldType::SLong, IfdValue::SignedArray(values)) => Ok(list_or_one(
            values
                .iter()
                .map(|&value| i32::try_from(value).map_err(|_| mismatch()))
                .collect::<Result<Vec<_>, _>>()?,
            TagValue::Signed,
        )),
        (FieldType::Long8, IfdValue::Unsigned(value)) => Ok(TagValue::UnsignedBig(*value)),
        (FieldType::Long8, IfdValue::UnsignedArray(values)) => {
            Ok(list_or_one(values.clone(), TagValue::UnsignedBig))
        }
        (FieldType::Ifd8, IfdValue::Unsigned(value)) => Ok(TagValue::IfdBig(*value)),
        (FieldType::Ifd8, IfdValue::UnsignedArray(values)) => {
            Ok(list_or_one(values.clone(), TagValue::IfdBig))
        }
        (FieldType::SLong8, IfdValue::Signed(value)) => Ok(TagValue::SignedBig(*value)),
        (FieldType::SLong8, IfdValue::SignedArray(values)) => {
            Ok(list_or_one(values.clone(), TagValue::SignedBig))
        }
        (FieldType::Float, IfdValue::Float(value)) => Ok(TagValue::Float(*value as f32)),
        (FieldType::Float, IfdValue::FloatArray(values)) => Ok(list_or_one(
            values.iter().map(|&value| value as f32).collect(),
            TagValue::Float,
        )),
        (FieldType::Double, IfdValue::Float(value)) => Ok(TagValue::Double(*value)),
        (FieldType::Double, IfdValue::FloatArray(values)) => {
            Ok(list_or_one(values.clone(), TagValue::Double))
        }
        (FieldType::Rational, IfdValue::UnsignedRationalArray(values)) => Ok(list_or_one(
            values
                .iter()
                .map(|&(n, d)| {
                    Ok((
                        u32::try_from(n).map_err(|_| mismatch())?,
                        u32::try_from(d).map_err(|_| mismatch())?,
                    ))
                })
                .collect::<AsyncTiffResult<Vec<_>>>()?,
            |(n, d)| TagValue::Rational(n, d),
        )),
        (FieldType::SRational, IfdValue::SignedRationalArray(values)) => Ok(list_or_one(
            values
                .iter()
                .map(|&(n, d)| {
                    Ok((
                        i32::try_from(n).map_err(|_| mismatch())?,
                        i32::try_from(d).map_err(|_| mismatch())?,
                    ))
                })
                .collect::<AsyncTiffResult<Vec<_>>>()?,
            |(n, d)| TagValue::SRational(n, d),
        )),
        _ => Err(mismatch()),
    }
}

fn build_async_ifd(
    entries: &BTreeMap<u16, IfdEntry>,
    order: ByteOrder,
) -> AsyncTiffResult<ImageFileDirectory> {
    let mut tags = std::collections::HashMap::with_capacity(entries.len() + 5);
    for (&tag, entry) in entries {
        // The lossless GeoKey parser owns this special directory. Passing it
        // to async-tiff would both discard entries and exercise panic paths.
        if tag == GEO_KEY_DIRECTORY {
            continue;
        }
        let value = async_tag_value(entry)?;
        tags.insert(Tag::from_u16_exhaustive(tag), value);
    }

    // async-tiff's internal IFD requires these fields even though the JS API
    // permits opening a sparse IFD and reports zero/defaults through getters.
    tags.entry(Tag::ImageWidth).or_insert(TagValue::Unsigned(0));
    tags.entry(Tag::ImageLength)
        .or_insert(TagValue::Unsigned(0));
    tags.entry(Tag::BitsPerSample).or_insert(TagValue::Short(0));
    tags.entry(Tag::SamplesPerPixel)
        .or_insert(TagValue::Short(1));

    let valid_photometric = tags
        .get(&Tag::PhotometricInterpretation)
        .and_then(|value| match value {
            TagValue::Byte(value) => Some(u16::from(*value)),
            TagValue::Short(value) => Some(*value),
            _ => None,
        })
        .is_some_and(|value| {
            async_tiff::tags::PhotometricInterpretation::from_u16(value).is_some()
        });
    if !valid_photometric {
        tags.insert(Tag::PhotometricInterpretation, TagValue::Short(0));
    }

    let endianness = match order {
        ByteOrder::Little => Endianness::LittleEndian,
        ByteOrder::Big => Endianness::BigEndian,
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ImageFileDirectory::from_tags(tags, endianness)
    }))
    .map_err(|_| invalid_metadata("TIFF IFD conversion panicked on malformed metadata"))?
}

fn decode_geo_key_reference(
    location: TagLocation,
    bytes: &[u8],
    offset: u16,
    count: u16,
    order: ByteOrder,
) -> AsyncTiffResult<ParsedGeoKeyValue> {
    match location.field_type {
        2 => {
            let range = checked_value_range(location, offset, count, 1, bytes.len())?;
            // geotiff.js uses `substring(offset, offset + count - 1)` for
            // ASCII GeoKeys, unconditionally excluding the separator byte.
            let content_end = range.end.saturating_sub(usize::from(count > 0));
            Ok(ParsedGeoKeyValue::Ascii(
                String::from_utf8_lossy(&bytes[range.start..content_end]).into_owned(),
            ))
        }
        1 | 7 => {
            let range = checked_value_range(location, offset, count, 1, bytes.len())?;
            Ok(scalar_or_unsigned(
                bytes[range].iter().map(|&value| u64::from(value)).collect(),
            ))
        }
        6 => {
            let range = checked_value_range(location, offset, count, 1, bytes.len())?;
            Ok(scalar_or_signed(
                bytes[range]
                    .iter()
                    .map(|&value| i64::from(value as i8))
                    .collect(),
            ))
        }
        3 => {
            let range = checked_value_range(location, offset, count, 2, bytes.len())?;
            Ok(scalar_or_unsigned(
                bytes[range]
                    .chunks_exact(2)
                    .map(|chunk| u64::from(order.u16(chunk)))
                    .collect(),
            ))
        }
        8 => {
            let range = checked_value_range(location, offset, count, 2, bytes.len())?;
            Ok(scalar_or_signed(
                bytes[range]
                    .chunks_exact(2)
                    .map(|chunk| i64::from(order.i16(chunk)))
                    .collect(),
            ))
        }
        4 | 13 => {
            let range = checked_value_range(location, offset, count, 4, bytes.len())?;
            Ok(scalar_or_unsigned(
                bytes[range]
                    .chunks_exact(4)
                    .map(|chunk| u64::from(order.u32(chunk)))
                    .collect(),
            ))
        }
        9 => {
            let range = checked_value_range(location, offset, count, 4, bytes.len())?;
            Ok(scalar_or_signed(
                bytes[range]
                    .chunks_exact(4)
                    .map(|chunk| i64::from(order.i32(chunk)))
                    .collect(),
            ))
        }
        11 => {
            let range = checked_value_range(location, offset, count, 4, bytes.len())?;
            Ok(scalar_or_float(
                bytes[range]
                    .chunks_exact(4)
                    .map(|chunk| f64::from(order.f32(chunk)))
                    .collect(),
            ))
        }
        12 => {
            let range = checked_value_range(location, offset, count, 8, bytes.len())?;
            Ok(scalar_or_float(
                bytes[range]
                    .chunks_exact(8)
                    .map(|chunk| order.f64(chunk))
                    .collect(),
            ))
        }
        16 | 18 => {
            let range = checked_value_range(location, offset, count, 8, bytes.len())?;
            Ok(scalar_or_unsigned(
                bytes[range]
                    .chunks_exact(8)
                    .map(|chunk| order.u64(chunk))
                    .collect(),
            ))
        }
        17 => {
            let range = checked_value_range(location, offset, count, 8, bytes.len())?;
            Ok(scalar_or_signed(
                bytes[range]
                    .chunks_exact(8)
                    .map(|chunk| order.i64(chunk))
                    .collect(),
            ))
        }
        5 => {
            let range = checked_value_range(location, offset, count, 8, bytes.len())?;
            let values = bytes[range]
                .chunks_exact(8)
                .map(|chunk| {
                    (
                        u64::from(order.u32(&chunk[..4])),
                        u64::from(order.u32(&chunk[4..])),
                    )
                })
                .collect::<Vec<_>>();
            Ok(match values.as_slice() {
                [(numerator, denominator)] => {
                    ParsedGeoKeyValue::UnsignedRational(*numerator, *denominator)
                }
                _ => ParsedGeoKeyValue::UnsignedRationalArray(values),
            })
        }
        10 => {
            let range = checked_value_range(location, offset, count, 8, bytes.len())?;
            let values = bytes[range]
                .chunks_exact(8)
                .map(|chunk| {
                    (
                        i64::from(order.i32(&chunk[..4])),
                        i64::from(order.i32(&chunk[4..])),
                    )
                })
                .collect::<Vec<_>>();
            Ok(match values.as_slice() {
                [(numerator, denominator)] => {
                    ParsedGeoKeyValue::SignedRational(*numerator, *denominator)
                }
                _ => ParsedGeoKeyValue::SignedRationalArray(values),
            })
        }
        field_type => Err(invalid_metadata(format!(
            "GeoKey references unsupported TIFF field type {field_type}"
        ))),
    }
}

async fn parse_geo_keys(
    reader: &mut MetadataCompatibilityReader,
    tags: &BTreeMap<u16, TagLocation>,
    order: ByteOrder,
) -> AsyncTiffResult<Option<GeoKeys>> {
    let Some(directory_location) = tags.get(&GEO_KEY_DIRECTORY).copied() else {
        return Ok(None);
    };
    if directory_location.field_type != 3 {
        return Err(invalid_metadata(
            "GeoKeyDirectory must have TIFF SHORT type",
        ));
    }
    let directory_bytes = reader.tag_bytes(directory_location).await?;
    if directory_bytes.len() < 8 || !directory_bytes.len().is_multiple_of(2) {
        return Err(invalid_metadata("GeoKeyDirectory is truncated"));
    }
    let version = order.u16(&directory_bytes[0..2]);
    let revision = order.u16(&directory_bytes[2..4]);
    if version != 1 || revision != 1 {
        return Err(invalid_metadata(format!(
            "Unsupported GeoKeyDirectory header version {version}.{revision}"
        )));
    }
    let key_count = usize::from(order.u16(&directory_bytes[6..8]));
    let required = key_count
        .checked_mul(8)
        .and_then(|length| length.checked_add(8))
        .ok_or_else(|| invalid_metadata("GeoKeyDirectory length overflow"))?;
    if required > directory_bytes.len() {
        return Err(invalid_metadata(format!(
            "GeoKeyDirectory declares {key_count} keys but is truncated"
        )));
    }

    let mut tag_data = BTreeMap::new();
    for index in 0..key_count {
        let start = 8 + index * 8;
        let entry = &directory_bytes[start..start + 8];
        let key_id = order.u16(&entry[0..2]);
        let tag_id = order.u16(&entry[2..4]);
        let count = order.u16(&entry[4..6]);
        let offset = order.u16(&entry[6..8]);
        let value = if tag_id == 0 {
            ParsedGeoKeyValue::Unsigned(u64::from(offset))
        } else {
            let location = tags.get(&tag_id).copied().ok_or_else(|| {
                invalid_metadata(format!(
                    "Could not get TIFF tag {tag_id} referenced by GeoKey {key_id}"
                ))
            })?;
            let bytes = reader.tag_bytes(location).await?;
            if tag_id == GEO_ASCII_PARAMS && count > 0 {
                let end = usize::from(offset)
                    .checked_add(usize::from(count))
                    .ok_or_else(|| invalid_metadata("GeoKey ASCII range overflow"))?;
                if end <= bytes.len() && bytes[end - 1] == 0 {
                    let absolute = location
                        .absolute_offset
                        .checked_add(end as u64 - 1)
                        .ok_or_else(|| invalid_metadata("GeoKey ASCII patch offset overflow"))?;
                    reader.patches.insert(absolute, b'|');
                }
            }
            decode_geo_key_reference(location, &bytes, offset, count, order)?
        };
        tag_data.insert(key_id, value);
    }
    Ok(Some(GeoKeys::new(tag_data)))
}

struct DiscoveredMetadata {
    order: ByteOrder,
    big_tiff: bool,
    ifds: Vec<ImageFileDirectory>,
    file_directories: Vec<FileDirectory>,
}

async fn discover_metadata(
    reader: &mut MetadataCompatibilityReader,
) -> AsyncTiffResult<DiscoveredMetadata> {
    if reader.prefix.len() < 8 {
        return Err(invalid_metadata("TIFF header is truncated"));
    }
    let order = match &reader.prefix[0..2] {
        b"II" => ByteOrder::Little,
        b"MM" => ByteOrder::Big,
        _ => return Err(invalid_metadata("Invalid byte order value.")),
    };
    let magic = order.u16(&reader.prefix[2..4]);
    let (big_tiff, mut ifd_offset) = match magic {
        42 => (false, u64::from(order.u32(&reader.prefix[4..8]))),
        43 if reader.prefix.len() >= 16 => {
            if order.u16(&reader.prefix[4..6]) != 8 {
                return Err(invalid_metadata("Unsupported offset byte-size."));
            }
            if order.u16(&reader.prefix[6..8]) != 0 {
                return Err(invalid_metadata("Invalid BigTIFF reserved header field"));
            }
            (true, order.u64(&reader.prefix[8..16]))
        }
        _ => return Err(invalid_metadata("Invalid magic number.")),
    };

    let count_size = if big_tiff { 8usize } else { 2usize };
    let entry_size = if big_tiff { 20usize } else { 12usize };
    let next_size = if big_tiff { 8usize } else { 4usize };
    let mut visited = HashSet::new();
    let mut ifds = Vec::new();
    let mut file_directories = Vec::new();

    for _ in 0..MAX_IFDS {
        if ifd_offset == 0 {
            return Ok(DiscoveredMetadata {
                order,
                big_tiff,
                ifds,
                file_directories,
            });
        }
        if !visited.insert(ifd_offset) {
            return Err(invalid_metadata(format!(
                "TIFF IFD chain contains a cycle at byte {ifd_offset}"
            )));
        }
        let count_bytes = reader.read_exact(ifd_offset, count_size).await?;
        let entry_count = if big_tiff {
            order.u64(&count_bytes)
        } else {
            u64::from(order.u16(&count_bytes))
        };
        if entry_count > MAX_IFD_ENTRIES {
            return Err(AsyncTiffError::General(format!(
                "TIFF IFD has an unreasonable {entry_count} entries"
            )));
        }
        let entries_len = usize::try_from(entry_count)
            .ok()
            .and_then(|value| value.checked_mul(entry_size))
            .ok_or_else(|| AsyncTiffError::General("IFD entry length overflow".to_string()))?;
        let directory_len = entries_len
            .checked_add(next_size)
            .ok_or_else(|| AsyncTiffError::General("IFD directory length overflow".to_string()))?;
        let entries_offset = ifd_offset
            .checked_add(count_size as u64)
            .ok_or_else(|| invalid_metadata("IFD entry offset overflow"))?;
        let directory = reader.read_exact(entries_offset, directory_len).await?;

        let mut tags = BTreeMap::new();
        for index in 0..entry_count as usize {
            let start = index * entry_size;
            let entry = &directory[start..start + entry_size];
            let absolute = entries_offset
                .checked_add(start as u64)
                .ok_or_else(|| invalid_metadata("IFD tag offset overflow"))?;
            let (tag, location) = parse_tag_location(entry, absolute, order, big_tiff)?;
            tags.insert(tag, location);
        }
        let next_at = entries_len;
        let next_ifd_offset = if big_tiff {
            order.u64(&directory[next_at..next_at + 8])
        } else {
            u64::from(order.u32(&directory[next_at..next_at + 4]))
        };

        let geo_keys = parse_geo_keys(reader, &tags, order).await?;
        let mut entries = BTreeMap::new();
        for (&tag, &location) in &tags {
            let bytes = reader.tag_bytes(location).await?;
            let field_type = FieldType::from_u16(location.field_type).ok_or_else(|| {
                invalid_metadata(format!(
                    "Invalid field type {} in TIFF tag {tag}",
                    location.field_type
                ))
            })?;
            let value = decode_ifd_value(tag, location, &bytes, order)?;
            entries.insert(
                tag,
                IfdEntry {
                    tag,
                    field_type,
                    count: location.count,
                    value,
                },
            );
        }
        let async_ifd = build_async_ifd(&entries, order)?;
        file_directories.push(FileDirectory::new(entries, next_ifd_offset, geo_keys));
        ifds.push(async_ifd);
        ifd_offset = next_ifd_offset;
    }
    Err(invalid_metadata(format!(
        "TIFF contains more than {MAX_IFDS} linked IFDs"
    )))
}

/// Metadata discovered without relying on async-tiff's lossy GeoKey model.
pub struct PreparedMetadata {
    pub reader: Arc<dyn AsyncFileReader>,
    pub tiff: TIFF,
    pub file_directories: Vec<FileDirectory>,
    pub big_tiff: bool,
}

/// Wrap a source in the metadata compatibility adapter and retain the full
/// GeoKey map for every linked IFD. A 64 KiB prefix is reused by the parser,
/// so discovery does not add a duplicate initial HTTP request.
pub async fn prepare_metadata(
    inner: Arc<dyn AsyncFileReader>,
) -> AsyncTiffResult<PreparedMetadata> {
    prepare_metadata_with_cancellation(inner, None).await
}

pub async fn prepare_metadata_with_cancellation(
    inner: Arc<dyn AsyncFileReader>,
    cancellation: Option<&CancellationToken>,
) -> AsyncTiffResult<PreparedMetadata> {
    let prefix = cancellable(inner.get_bytes(0..PREFIX_SIZE), cancellation).await?;
    let mut reader = MetadataCompatibilityReader {
        inner,
        prefix,
        patches: BTreeMap::new(),
        cancellation: cancellation.cloned(),
    };
    let discovered = discover_metadata(&mut reader).await?;
    // The open signal only governs metadata discovery, exactly like the
    // signal passed to `GeoTIFF.fromSource`. Later raster calls carry their
    // own token and must not remain coupled to this one.
    reader.cancellation = None;
    let endianness = match discovered.order {
        ByteOrder::Little => Endianness::LittleEndian,
        ByteOrder::Big => Endianness::BigEndian,
    };
    Ok(PreparedMetadata {
        reader: Arc::new(reader),
        tiff: TIFF::new(discovered.ifds, endianness),
        file_directories: discovered.file_directories,
        big_tiff: discovered.big_tiff,
    })
}

#[cfg(test)]
mod tests {
    use crate::api::from_bytes;
    use crate::geokeys::ParsedGeoKeyValue;
    use crate::writer::{WriterMetadata, geo_key, write_array_buffer};

    #[tokio::test]
    async fn opens_the_nul_count_emitted_by_the_javascript_writer_without_panicking() {
        let bytes = write_array_buffer(vec![1u8, 2, 3, 4], WriterMetadata::new(2, 2)).unwrap();

        // The generated directory is byte-compatible with geotiff.js:
        // [GeogCitation, GeoAsciiParams, 7 including NUL, offset 0].
        let needle = [0x08, 0x01, 0x87, 0xb1, 0x00, 0x07, 0x00, 0x00];
        assert!(bytes.windows(needle.len()).any(|window| window == needle));

        let dataset = from_bytes(bytes).await.unwrap();
        let image = dataset.image(0).unwrap();
        let keys = image.geo_keys().unwrap();
        assert_eq!(
            keys.get_named("GeogCitationGeoKey")
                .and_then(|value| value.as_str()),
            Some("WGS 84")
        );
        assert_eq!(
            keys.get_named("GeographicTypeGeoKey")
                .and_then(|value| value.as_u16()),
            Some(4326)
        );
        assert_eq!(
            keys.get_named("GTModelTypeGeoKey")
                .and_then(|value| value.as_u16()),
            Some(2)
        );
        let _ = geo_key::GEOG_CITATION; // public constant remains reachable
    }

    #[tokio::test]
    async fn retains_every_named_and_vendor_geokey_without_dependency_filtering() {
        let metadata = WriterMetadata::new(1, 1)
            .with_geo_key(geo_key::GEOG_TO_WGS84, vec![1.0, 2.0, 3.0])
            .with_geo_key(geo_key::PROJ_RECTIFIED_GRID_ANGLE, 12.5)
            .with_geo_key(65_000, 77u16);
        let dataset = from_bytes(write_array_buffer(vec![1u8], metadata).unwrap())
            .await
            .unwrap();
        let image = dataset.image(0).unwrap();
        let keys = image.geo_keys().unwrap();
        assert_eq!(
            keys.get(geo_key::GEOG_TO_WGS84),
            Some(&ParsedGeoKeyValue::FloatArray(vec![1.0, 2.0, 3.0]))
        );
        assert_eq!(
            keys.get(geo_key::PROJ_RECTIFIED_GRID_ANGLE),
            Some(&ParsedGeoKeyValue::Float(12.5))
        );
        assert_eq!(keys.get(65_000), Some(&ParsedGeoKeyValue::Unsigned(77)));
    }
}
