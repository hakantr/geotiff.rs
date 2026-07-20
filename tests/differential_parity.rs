use geotiff::geokeys::ParsedGeoKeyValue;
use geotiff::imagefiledirectory::{IfdScalar, IfdValue};
use geotiff::{
    FillValue, GeoTiffImage, ImageWindow, PackedSampleMode, ReadRasterResult, ReadRastersOptions,
    ReadRgbOptions, SampleFormat, SampleReader, TypedArray, from_file,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DifferentialCases {
    metadata_fixtures: Vec<String>,
    metadata_divergences: Vec<MetadataDivergence>,
    directory_object_divergence: DirectoryObjectDivergence,
    default_raster_fixtures: Vec<String>,
    raster_cases: Vec<DifferentialCase>,
    block_cases: Vec<DifferentialCase>,
    rgb_cases: Vec<DifferentialCase>,
}

#[derive(Debug, Deserialize)]
struct DirectoryObjectDivergence {
    id: String,
    classification: String,
    rationale: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataDivergence {
    id: String,
    classification: String,
    rationale: String,
    tags: Vec<MetadataTagDivergence>,
}

#[derive(Debug, Deserialize)]
struct MetadataTagDivergence {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DifferentialCase {
    id: String,
    fixture: String,
    #[serde(default)]
    comparison: Option<String>,
    #[serde(default)]
    max_absolute_difference: Option<f64>,
    #[serde(default)]
    max_mean_absolute_difference: Option<f64>,
    #[serde(default)]
    options: DifferentialOptions,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DifferentialOptions {
    x: Option<usize>,
    y: Option<usize>,
    sample: Option<usize>,
    window: Option<[i64; 4]>,
    #[serde(default)]
    samples: Vec<usize>,
    interleave: Option<bool>,
    width: Option<usize>,
    height: Option<usize>,
    resample_method: Option<String>,
    fill_value: Option<Value>,
    enable_alpha: Option<bool>,
    packed_sample_mode: Option<String>,
}

fn manifest_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn load_cases() -> DifferentialCases {
    let path = manifest_root().join("tests/differential/cases.json");
    serde_json::from_slice(&std::fs::read(path).expect("read differential cases"))
        .expect("parse differential cases")
}

fn number(value: f64) -> Value {
    if value.is_nan() {
        return json!({ "$number": "NaN" });
    }
    if value == f64::INFINITY {
        return json!({ "$number": "Infinity" });
    }
    if value == f64::NEG_INFINITY {
        return json!({ "$number": "-Infinity" });
    }
    if value == 0.0 && value.is_sign_negative() {
        return json!({ "$number": "-0" });
    }
    if value.fract() == 0.0
        && value >= i64::MIN as f64
        && value <= i64::MAX as f64
        && value.abs() <= 9_007_199_254_740_991.0
    {
        return json!(value as i64);
    }
    Value::Number(serde_json::Number::from_f64(value).expect("finite JSON number"))
}

fn typed_value(array: &TypedArray, index: usize) -> Value {
    match array {
        TypedArray::Int8(values) => json!(values[index]),
        TypedArray::Uint8(values) | TypedArray::Uint8Clamped(values) => json!(values[index]),
        TypedArray::Int16(values) => json!(values[index]),
        TypedArray::Uint16(values) => json!(values[index]),
        TypedArray::Int32(values) => json!(values[index]),
        TypedArray::Uint32(values) => json!(values[index]),
        TypedArray::Int64(values) => json!(values[index]),
        TypedArray::Uint64(values) => json!(values[index]),
        TypedArray::Float32(values) => number(f64::from(values[index])),
        TypedArray::Float64(values) => number(values[index]),
    }
}

fn typed_diagnostic_value(array: &TypedArray, index: usize) -> Value {
    match array {
        TypedArray::Float32(values) => {
            json!({ "$float32Bits": format!("{:08x}", values[index].to_bits()) })
        }
        TypedArray::Float64(values) => {
            json!({ "$float64Bits": format!("{:016x}", values[index].to_bits()) })
        }
        _ => typed_value(array, index),
    }
}

fn typed_name(array: &TypedArray) -> &'static str {
    match array {
        TypedArray::Int8(_) => "Int8Array",
        TypedArray::Uint8(_) => "Uint8Array",
        TypedArray::Uint8Clamped(_) => "Uint8ClampedArray",
        TypedArray::Int16(_) => "Int16Array",
        TypedArray::Uint16(_) => "Uint16Array",
        TypedArray::Int32(_) => "Int32Array",
        TypedArray::Uint32(_) => "Uint32Array",
        TypedArray::Int64(_) => "BigInt64Array",
        TypedArray::Uint64(_) => "BigUint64Array",
        TypedArray::Float32(_) => "Float32Array",
        TypedArray::Float64(_) => "Float64Array",
    }
}

fn typed_bytes(array: &TypedArray) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(array.len().saturating_mul(8));
    macro_rules! extend {
        ($values:expr) => {
            for value in $values {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        };
    }
    match array {
        TypedArray::Int8(values) => bytes.extend(values.iter().map(|value| *value as u8)),
        TypedArray::Uint8(values) | TypedArray::Uint8Clamped(values) => {
            bytes.extend_from_slice(values)
        }
        TypedArray::Int16(values) => extend!(values),
        TypedArray::Uint16(values) => extend!(values),
        TypedArray::Int32(values) => extend!(values),
        TypedArray::Uint32(values) => extend!(values),
        TypedArray::Int64(values) => extend!(values),
        TypedArray::Uint64(values) => extend!(values),
        TypedArray::Float32(values) => extend!(values),
        TypedArray::Float64(values) => extend!(values),
    }
    bytes
}

fn typed_summary(array: &TypedArray, include_values: bool) -> Value {
    let digest = Sha256::digest(typed_bytes(array));
    let first = (0..array.len().min(8))
        .map(|index| typed_diagnostic_value(array, index))
        .collect::<Vec<_>>();
    let last_start = array.len().saturating_sub(8);
    let last = (last_start..array.len())
        .map(|index| typed_diagnostic_value(array, index))
        .collect::<Vec<_>>();
    let mut summary = Map::from_iter([
        ("type".to_string(), json!(typed_name(array))),
        ("length".to_string(), json!(array.len())),
        ("sha256".to_string(), json!(format!("{digest:x}"))),
        ("first".to_string(), Value::Array(first)),
        ("last".to_string(), Value::Array(last)),
    ]);
    if include_values {
        summary.insert(
            "values".to_string(),
            Value::Array(
                (0..array.len())
                    .map(|index| typed_value(array, index))
                    .collect(),
            ),
        );
    }
    Value::Object(summary)
}

fn raster_summary(result: &ReadRasterResult, include_values: bool) -> Value {
    match result {
        ReadRasterResult::Interleaved(raster) => json!({
            "shape": "interleaved",
            "width": raster.width,
            "height": raster.height,
            "data": typed_summary(&raster.data, include_values),
        }),
        ReadRasterResult::Bands(raster) => json!({
            "shape": "bands",
            "width": raster.width,
            "height": raster.height,
            "bands": raster.bands.iter()
                .map(|band| typed_summary(band, include_values))
                .collect::<Vec<_>>(),
        }),
    }
}

fn normalize_ifd_value(value: &IfdValue) -> Value {
    match value {
        IfdValue::Unsigned(value) => json!(value),
        IfdValue::Signed(value) => json!(value),
        IfdValue::Float(value) => number(*value),
        IfdValue::Ascii(value) => json!(value),
        IfdValue::UnsignedArray(values) => json!(values),
        IfdValue::SignedArray(values) => json!(values),
        IfdValue::FloatArray(values) => {
            Value::Array(values.iter().map(|value| number(*value)).collect())
        }
        IfdValue::UnsignedRational(numerator, denominator) => json!([numerator, denominator]),
        IfdValue::SignedRational(numerator, denominator) => json!([numerator, denominator]),
        IfdValue::UnsignedRationalArray(values) => Value::Array(
            values
                .iter()
                .flat_map(|(numerator, denominator)| [json!(numerator), json!(denominator)])
                .collect(),
        ),
        IfdValue::SignedRationalArray(values) => Value::Array(
            values
                .iter()
                .flat_map(|(numerator, denominator)| [json!(numerator), json!(denominator)])
                .collect(),
        ),
    }
}

fn normalize_ifd_scalar(value: Option<IfdScalar>) -> Value {
    match value {
        Some(IfdScalar::Unsigned(value)) => json!(value),
        Some(IfdScalar::Signed(value)) => json!(value),
        Some(IfdScalar::Float(value)) => number(value),
        Some(IfdScalar::Ascii(value)) => json!(value),
        Some(IfdScalar::UnsignedRational(numerator, denominator)) => {
            json!([numerator, denominator])
        }
        Some(IfdScalar::SignedRational(numerator, denominator)) => {
            json!([numerator, denominator])
        }
        None => json!({ "$undefined": true }),
    }
}

fn normalize_geokey_value(value: &ParsedGeoKeyValue) -> Value {
    match value {
        ParsedGeoKeyValue::Unsigned(value) => json!(value),
        ParsedGeoKeyValue::Signed(value) => json!(value),
        ParsedGeoKeyValue::Float(value) => number(*value),
        ParsedGeoKeyValue::Ascii(value) => json!(value),
        ParsedGeoKeyValue::UnsignedArray(values) => json!(values),
        ParsedGeoKeyValue::SignedArray(values) => json!(values),
        ParsedGeoKeyValue::FloatArray(values) => {
            Value::Array(values.iter().map(|value| number(*value)).collect())
        }
        ParsedGeoKeyValue::UnsignedRational(numerator, denominator) => {
            json!([numerator, denominator])
        }
        ParsedGeoKeyValue::SignedRational(numerator, denominator) => {
            json!([numerator, denominator])
        }
        ParsedGeoKeyValue::UnsignedRationalArray(values) => Value::Array(
            values
                .iter()
                .flat_map(|(numerator, denominator)| [json!(numerator), json!(denominator)])
                .collect(),
        ),
        ParsedGeoKeyValue::SignedRationalArray(values) => Value::Array(
            values
                .iter()
                .flat_map(|(numerator, denominator)| [json!(numerator), json!(denominator)])
                .collect(),
        ),
    }
}

fn normalize_geokeys(image: &GeoTiffImage<'_>) -> Value {
    let Some(keys) = image.geo_keys() else {
        return Value::Null;
    };
    let mut output = Map::new();
    for (id, value) in keys.iter() {
        let name = geotiff::geo_key_name(id)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "undefined".to_string());
        output.insert(name, normalize_geokey_value(value));
    }
    Value::Object(output)
}

fn ok(value: Value) -> Value {
    json!({ "ok": value })
}

fn error(message: impl ToString) -> Value {
    json!({
        "error": {
            "name": "Error",
            "message": message.to_string(),
        }
    })
}

fn option_map(map: Option<BTreeMap<String, String>>) -> Value {
    match map {
        Some(map) => json!(map),
        None => Value::Null,
    }
}

fn sample_format_number(format: SampleFormat) -> u16 {
    format.to_u16()
}

fn sample_reader_name(reader: SampleReader) -> &'static str {
    match reader {
        SampleReader::Uint8 => "uint8",
        SampleReader::Uint16 => "uint16",
        SampleReader::Uint32 => "uint32",
        SampleReader::Int8 => "int8",
        SampleReader::Int16 => "int16",
        SampleReader::Int32 => "int32",
        SampleReader::Float16 => "float16",
        SampleReader::Float32 => "float32",
        SampleReader::Float64 => "float64",
    }
}

async fn rust_metadata(path: &Path) -> Value {
    let dataset = from_file(path).await.expect("Rust opens metadata fixture");
    let ghost = match dataset.ghost_values().await {
        Ok(value) => ok(option_map(value)),
        Err(value) => error(value),
    };
    let image_count = dataset.image_count();
    let big_tiff = dataset.is_big_tiff();
    let image = dataset.image(0).expect("Rust gets first image");
    let directory = image.file_directory();
    let samples = image.samples_per_pixel();
    let tile_height = image.tile_height();
    let block_rows = if tile_height > 0 {
        image.height().div_ceil(tile_height)
    } else {
        0
    };

    let sample_info = (0..samples)
        .map(|sample| {
            let bits = image.bits_per_sample(sample).expect("sample bits");
            let format = image.sample_format(sample).expect("sample format");
            let reader = image.reader_for_sample(sample).expect("sample reader");
            let array = image.array_for_sample(sample, 3).expect("sample array");
            json!({
                "bits": bits,
                "format": sample_format_number(format),
                "byteSize": image.sample_byte_size(sample).expect("sample byte size"),
                "reader": sample_reader_name(reader),
                "arrayType": typed_name(&array),
            })
        })
        .collect::<Vec<_>>();

    let gdal_by_sample = (0..samples)
        .map(|sample| match image.gdal_metadata(Some(sample)) {
            Ok(value) => ok(option_map(value)),
            Err(value) => error(value),
        })
        .collect::<Vec<_>>();

    let mut values = Map::new();
    for (tag, entry) in directory.iter() {
        values.insert(tag.to_string(), normalize_ifd_value(&entry.value));
    }
    let mut object = Map::new();
    for (name, value) in directory.to_object() {
        object.insert(name, normalize_ifd_value(&value));
    }

    let indexed = json!({
        "bitsPerSample0": ok(normalize_ifd_scalar(directory.load_value_indexed("BitsPerSample", 0).await)),
        "bitsPerSampleOutOfBounds": ok(normalize_ifd_scalar(directory.load_value_indexed("BitsPerSample", 999).await)),
        "imageWidthScalar": ok(normalize_ifd_scalar(directory.load_value_indexed("ImageWidth", 0).await)),
        "software0": ok(normalize_ifd_scalar(directory.load_value_indexed("Software", 0).await)),
        "softwareOutOfBounds": ok(normalize_ifd_scalar(directory.load_value_indexed("Software", 999).await)),
        "xResolutionNumerator": ok(normalize_ifd_scalar(directory.load_value_indexed("XResolution", 0).await)),
        "xResolutionDenominator": ok(normalize_ifd_scalar(directory.load_value_indexed("XResolution", 1).await)),
        "xResolutionOutOfBounds": ok(normalize_ifd_scalar(directory.load_value_indexed("XResolution", 2).await)),
        "missingTag": ok(normalize_ifd_scalar(directory.load_value_indexed(65_000u16, 0).await)),
    });

    let geo_keys = normalize_geokeys(&image);
    let tie_points = match image.tie_points() {
        Ok(points) => ok(Value::Array(
            points
                .iter()
                .map(|point| {
                    json!({
                        "i": number(point.i),
                        "j": number(point.j),
                        "k": number(point.k),
                        "x": number(point.x),
                        "y": number(point.y),
                        "z": number(point.z),
                    })
                })
                .collect(),
        )),
        Err(value) => error(value),
    };
    let gdal_metadata = match image.gdal_metadata(None) {
        Ok(value) => ok(option_map(value)),
        Err(value) => error(value),
    };
    let origin = match image.origin() {
        Ok(value) => ok(Value::Array(value.into_iter().map(number).collect())),
        Err(value) => error(value),
    };
    let resolution = match image.resolution(None) {
        Ok(value) => ok(Value::Array(value.into_iter().map(number).collect())),
        Err(value) => error(value),
    };
    let bounding_box = match image.bounding_box(false) {
        Ok(value) => ok(Value::Array(value.into_iter().map(number).collect())),
        Err(value) => error(value),
    };
    let tilegrid_bounding_box = match image.bounding_box(true) {
        Ok(value) => ok(Value::Array(value.into_iter().map(number).collect())),
        Err(value) => error(value),
    };

    json!({
        "dataset": {
            "imageCount": image_count,
            "bigTiff": big_tiff,
            "littleEndian": image.little_endian(),
            "ghostValues": ghost,
        },
        "image": {
            "width": image.width(),
            "height": image.height(),
            "samplesPerPixel": samples,
            "tiled": image.is_tiled(),
            "planarConfiguration": image.planar_configuration(),
            "tileWidth": image.tile_width(),
            "tileHeight": tile_height,
            "blockWidth": image.block_width(),
            "firstBlockHeight": if block_rows > 0 { image.block_height(0) } else { 0 },
            "lastBlockHeight": if block_rows > 0 { image.block_height(block_rows - 1) } else { 0 },
            "bytesPerPixel": ok(json!(image.bytes_per_pixel())),
            "samples": sample_info,
            "geoKeys": ok(geo_keys.clone()),
            "tiePoints": tie_points,
            "gdalMetadata": gdal_metadata,
            "gdalMetadataBySample": gdal_by_sample,
            "gdalNoData": image.gdal_nodata().map(number).unwrap_or(Value::Null),
            "origin": origin,
            "resolution": resolution,
            "pixelIsArea": image.pixel_is_area(),
            "boundingBox": bounding_box,
            "tilegridBoundingBox": tilegrid_bounding_box,
        },
        "directory": {
            "nextIfdByteOffset": directory.next_ifd_byte_offset(),
            "values": values,
            "indexed": indexed,
            "geoKeys": ok(geo_keys),
            "object": object,
        },
    })
}

fn image_window(value: Option<[i64; 4]>) -> Option<ImageWindow> {
    value.map(|window| ImageWindow {
        x0: window[0],
        y0: window[1],
        x1: window[2],
        y1: window[3],
    })
}

fn fill_value(value: Option<&Value>) -> FillValueResult {
    match value {
        None => FillValueResult::None,
        Some(Value::Number(value)) => FillValueResult::Value(FillValue::Scalar(
            value.as_f64().expect("numeric fill value"),
        )),
        Some(Value::Array(values)) => FillValueResult::Value(FillValue::PerSample(
            values
                .iter()
                .map(|value| value.as_f64().expect("numeric per-sample fill value"))
                .collect(),
        )),
        Some(other) => panic!("unsupported fillValue in differential case: {other}"),
    }
}

enum FillValueResult {
    None,
    Value(FillValue),
}

impl FillValueResult {
    fn into_option(self) -> Option<FillValue> {
        match self {
            Self::None => None,
            Self::Value(value) => Some(value),
        }
    }
}

async fn rust_raster_case(fixture_root: &Path, test_case: &DifferentialCase, rgb: bool) -> Value {
    let dataset = match from_file(fixture_root.join(&test_case.fixture)).await {
        Ok(value) => value,
        Err(value) => return error(value),
    };
    let image = match dataset.image(0) {
        Ok(value) => value,
        Err(value) => return error(value),
    };
    let include_values = test_case.comparison.as_deref() == Some("numericTolerance");
    let mode = match test_case.options.packed_sample_mode.as_deref() {
        Some("geotiffJs") => PackedSampleMode::GeotiffJs,
        _ => PackedSampleMode::Lossless,
    };
    let result = if rgb {
        image
            .read_rgb(ReadRgbOptions {
                window: image_window(test_case.options.window),
                interleave: test_case.options.interleave.unwrap_or(false),
                width: test_case.options.width,
                height: test_case.options.height,
                resample_method: test_case
                    .options
                    .resample_method
                    .clone()
                    .unwrap_or_else(|| "nearest".to_string()),
                enable_alpha: test_case.options.enable_alpha.unwrap_or(false),
                packed_sample_mode: mode,
                ..ReadRgbOptions::default()
            })
            .await
    } else {
        image
            .read_rasters(ReadRastersOptions {
                window: image_window(test_case.options.window),
                samples: test_case.options.samples.clone(),
                interleave: test_case.options.interleave.unwrap_or(false),
                width: test_case.options.width,
                height: test_case.options.height,
                resample_method: test_case
                    .options
                    .resample_method
                    .clone()
                    .unwrap_or_else(|| "nearest".to_string()),
                fill_value: fill_value(test_case.options.fill_value.as_ref()).into_option(),
                packed_sample_mode: mode,
                ..ReadRastersOptions::default()
            })
            .await
    };
    match result {
        Ok(value) => ok(raster_summary(&value, include_values)),
        Err(value) => error(value),
    }
}

async fn rust_block_case(fixture_root: &Path, test_case: &DifferentialCase) -> Value {
    let dataset = match from_file(fixture_root.join(&test_case.fixture)).await {
        Ok(value) => value,
        Err(value) => return error(value),
    };
    let image = match dataset.image(0) {
        Ok(value) => value,
        Err(value) => return error(value),
    };
    let x = test_case.options.x.expect("block case x");
    let y = test_case.options.y.expect("block case y");
    let sample = test_case.options.sample.expect("block case sample");
    match image.get_tile_or_strip(x, y, sample, None).await {
        Ok(block) => ok(json!({
            "x": block.x,
            "y": block.y,
            "sample": block.sample,
            "data": typed_summary(&TypedArray::Uint8(block.data), false),
        })),
        Err(value) => error(value),
    }
}

fn run_js_oracle() -> Value {
    let root = manifest_root();
    let js_root = std::env::var_os("GEOTIFF_JS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("../geotiff.js"));
    let script = root.join("tests/differential/js_oracle.mjs");
    let fixtures = root.join("tests/fixtures");
    let cases = root.join("tests/differential/cases.json");
    let output = Command::new("node")
        .args([
            script.as_os_str(),
            js_root.as_os_str(),
            fixtures.as_os_str(),
            cases.as_os_str(),
        ])
        .output()
        .expect("run live geotiff.js oracle");
    assert!(
        output.status.success(),
        "live geotiff.js oracle failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse live geotiff.js oracle JSON")
}

fn assert_case_eq(section: &str, id: &str, js: &Value, rust: &Value) {
    assert_eq!(
        js,
        rust,
        "differential mismatch in {section}/{id}\nJS:\n{}\nRust:\n{}",
        serde_json::to_string_pretty(js).unwrap(),
        serde_json::to_string_pretty(rust).unwrap(),
    );
}

fn remove_object_field(value: &mut Value, pointer: &str, field: &str) -> Value {
    value
        .pointer_mut(pointer)
        .and_then(Value::as_object_mut)
        .unwrap_or_else(|| panic!("differential object missing at {pointer}"))
        .remove(field)
        .unwrap_or_else(|| panic!("differential field missing at {pointer}/{field}"))
}

fn geotiff_js_rational_value(rust_value: &Value, divergence_id: &str) -> Value {
    let rust_values = rust_value.as_array().expect("lossless RATIONAL array");
    assert_eq!(
        rust_values.len() % 2,
        0,
        "{divergence_id}: rational value must contain numerator/denominator pairs"
    );
    let pair_count = rust_values.len() / 2;
    let mut expected_js = Vec::with_capacity(rust_values.len());
    for pair in (0..pair_count).step_by(2) {
        expected_js.extend_from_slice(&rust_values[pair * 2..pair * 2 + 2]);
    }
    expected_js.resize(rust_values.len(), json!(0));
    Value::Array(expected_js)
}

fn assert_directory_object(
    fixture: &str,
    js: Value,
    rust: Value,
    rational_divergences: &[MetadataDivergence],
    object_divergence: &DirectoryObjectDivergence,
) -> usize {
    assert_eq!(
        object_divergence.classification, "nativeEagerMetadataSuperset",
        "{} must remain an explicit native metadata superset: {}",
        object_divergence.id, object_divergence.rationale
    );
    let js = js.as_object().expect("JS directory object");
    let mut rust = rust.as_object().expect("Rust directory object").clone();
    for (name, js_value) in js {
        let rust_value = rust.remove(name).unwrap_or_else(|| {
            panic!("{fixture}: Rust directory object is missing JS field {name}")
        });
        let rational_divergence = rational_divergences
            .iter()
            .find(|divergence| divergence.tags.iter().any(|tag| tag.name == *name));
        if let Some(divergence) = rational_divergence
            .filter(|_| rust_value.as_array().is_some_and(|values| values.len() > 2))
        {
            assert_eq!(
                *js_value,
                geotiff_js_rational_value(&rust_value, &divergence.id),
                "{}: named RATIONAL field {name}",
                divergence.id
            );
        } else {
            assert_eq!(
                *js_value, rust_value,
                "differential mismatch in metadata/{fixture}/directory/object/{name}"
            );
        }
    }
    rust.len()
}

fn assert_metadata_case(
    fixture: &str,
    js: &Value,
    rust: &Value,
    divergences: &[MetadataDivergence],
    object_divergence: &DirectoryObjectDivergence,
) -> (usize, usize) {
    let mut comparable_js = js.clone();
    let mut comparable_rust = rust.clone();
    let mut classified = 0;
    let js_object = remove_object_field(&mut comparable_js, "/directory", "object");
    let rust_object = remove_object_field(&mut comparable_rust, "/directory", "object");

    for divergence in divergences {
        assert_eq!(
            divergence.classification, "referenceBugDataLoss",
            "{} must remain an explicit data-preserving divergence: {}",
            divergence.id, divergence.rationale
        );
        for tag in &divergence.tags {
            let Some(rust_value) = comparable_rust["directory"]["values"].get(&tag.id) else {
                continue;
            };
            let Some(rust_values) = rust_value.as_array() else {
                continue;
            };
            if rust_values.len() <= 2 {
                continue;
            }

            let rust_value =
                remove_object_field(&mut comparable_rust, "/directory/values", &tag.id);
            let js_value = remove_object_field(&mut comparable_js, "/directory/values", &tag.id);
            assert_eq!(
                js_value,
                geotiff_js_rational_value(&rust_value, &divergence.id),
                "{}: geotiff.js RATIONAL bug changed for TIFF tag {}",
                divergence.id,
                tag.id
            );
            assert_ne!(
                js_value, rust_value,
                "{}: obsolete divergence entry for TIFF tag {}",
                divergence.id, tag.id
            );
            classified += 1;
        }
    }

    let eager_superset_fields = assert_directory_object(
        fixture,
        js_object,
        rust_object,
        divergences,
        object_divergence,
    );
    assert_case_eq("metadata", fixture, &comparable_js, &comparable_rust);
    (classified, eager_superset_fields)
}

fn raster_data_summaries(summary: &Value) -> Vec<&Value> {
    match summary["shape"].as_str() {
        Some("interleaved") => vec![&summary["data"]],
        Some("bands") => summary["bands"]
            .as_array()
            .expect("numeric raster bands")
            .iter()
            .collect(),
        other => panic!("unexpected raster shape: {other:?}"),
    }
}

fn assert_lossy_case(test_case: &DifferentialCase, js: &Value, rust: &Value) {
    let id = &test_case.id;
    let js_summary = &js["ok"];
    let rust_summary = &rust["ok"];
    for field in ["shape", "width", "height"] {
        assert_eq!(js_summary[field], rust_summary[field], "{id}: {field}");
    }
    let js_data = raster_data_summaries(js_summary);
    let rust_data = raster_data_summaries(rust_summary);
    assert_eq!(js_data.len(), rust_data.len(), "{id}: data array count");
    for (index, (js_data, rust_data)) in js_data.iter().zip(&rust_data).enumerate() {
        for field in ["type", "length"] {
            assert_eq!(
                js_data[field], rust_data[field],
                "{id}: data[{index}].{field}"
            );
        }
    }
    let numeric_values = |data: &[&Value]| {
        data.iter()
            .flat_map(|summary| {
                summary["values"]
                    .as_array()
                    .expect("numeric raster values")
                    .iter()
                    .map(|value| value.as_f64().expect("finite numeric raster value"))
            })
            .collect::<Vec<_>>()
    };
    let js_values = numeric_values(&js_data);
    let rust_values = numeric_values(&rust_data);
    let differences = js_values
        .iter()
        .zip(&rust_values)
        .map(|(left, right)| (left - right).abs())
        .collect::<Vec<_>>();
    let maximum = differences.iter().copied().fold(0.0f64, f64::max);
    let mean = differences.iter().sum::<f64>() / differences.len() as f64;
    let maximum_limit = test_case
        .max_absolute_difference
        .expect("numericTolerance case needs maxAbsoluteDifference");
    let mean_limit = test_case
        .max_mean_absolute_difference
        .expect("numericTolerance case needs maxMeanAbsoluteDifference");
    assert!(
        maximum <= maximum_limit && mean <= mean_limit,
        "{id}: lossy decoder divergence max={maximum}/{maximum_limit}, mean={mean}/{mean_limit}"
    );
}

fn assert_js_surface(surface: &Value) {
    assert_eq!(
        surface["exports"],
        json!([
            "BaseClient",
            "BaseDecoder",
            "BaseResponse",
            "GeoTIFF",
            "GeoTIFFImage",
            "ImageFileDirectory",
            "MultiGeoTIFF",
            "Pool",
            "addDecoder",
            "default",
            "fromArrayBuffer",
            "fromBlob",
            "fromCustomClient",
            "fromFile",
            "fromUrl",
            "fromUrls",
            "getDecoder",
            "globals",
            "registerTag",
            "rgb",
            "setLogger",
            "writeArrayBuffer"
        ])
    );
    assert_eq!(
        surface["prototypes"]["GeoTIFF"],
        json!([
            "close",
            "getGhostValues",
            "getImage",
            "getImageCount",
            "getSlice",
            "requestIFD"
        ])
    );
    assert_eq!(
        surface["prototypes"]["MultiGeoTIFF"],
        json!(["getImage", "getImageCount", "parseFileDirectoriesPerFile"])
    );
    assert_eq!(
        surface["prototypes"]["GeoTIFFImage"],
        json!([
            "_readRaster",
            "getArrayForSample",
            "getBitsPerSample",
            "getBlockHeight",
            "getBlockWidth",
            "getBoundingBox",
            "getBytesPerPixel",
            "getFileDirectory",
            "getGDALMetadata",
            "getGDALNoData",
            "getGeoKeys",
            "getHeight",
            "getOrigin",
            "getReaderForSample",
            "getResolution",
            "getSampleByteSize",
            "getSampleFormat",
            "getSamplesPerPixel",
            "getTiePoints",
            "getTileHeight",
            "getTileOrStrip",
            "getTileWidth",
            "getWidth",
            "pixelIsArea",
            "readRGB",
            "readRasters"
        ])
    );
    assert_eq!(
        surface["prototypes"]["ImageFileDirectory"],
        json!([
            "getValue",
            "hasTag",
            "loadValue",
            "loadValueIndexed",
            "parseGeoKeyDirectory",
            "toObject"
        ])
    );
    assert_eq!(
        surface["prototypes"]["Pool"],
        json!(["bindParameters", "destroy"])
    );
    assert_eq!(surface["prototypes"]["BaseClient"], json!(["request"]));
    assert_eq!(
        surface["prototypes"]["BaseResponse"],
        json!(["getData", "getHeader", "ok", "status"])
    );
    assert_eq!(
        surface["prototypes"]["BaseDecoder"],
        json!(["decode", "decodeBlock"])
    );
}

#[tokio::test]
#[ignore = "requires the sibling geotiff.js 3.1.0 repository and Node.js"]
async fn live_two_repository_metadata_raster_and_rgb_differential() {
    let cases = load_cases();
    let js = run_js_oracle();
    assert_eq!(js["reference"]["version"], "3.1.0");
    assert_js_surface(&js["surface"]);

    let fixture_root = manifest_root().join("tests/fixtures");
    let mut classified_metadata_divergences = 0;
    let mut eager_metadata_superset_fields = 0;
    for fixture in &cases.metadata_fixtures {
        let rust = rust_metadata(&fixture_root.join(fixture)).await;
        let (classified, eager_fields) = assert_metadata_case(
            fixture,
            &js["metadata"][fixture],
            &rust,
            &cases.metadata_divergences,
            &cases.directory_object_divergence,
        );
        classified_metadata_divergences += classified;
        eager_metadata_superset_fields += eager_fields;
    }
    assert!(
        classified_metadata_divergences > 0,
        "configured metadata divergence was not exercised"
    );
    assert!(
        eager_metadata_superset_fields > 0,
        "native eager metadata superset was not exercised"
    );

    for fixture in &cases.default_raster_fixtures {
        let test_case = DifferentialCase {
            id: fixture.clone(),
            fixture: fixture.clone(),
            comparison: None,
            max_absolute_difference: None,
            max_mean_absolute_difference: None,
            options: DifferentialOptions::default(),
        };
        let rust = rust_raster_case(&fixture_root, &test_case, false).await;
        assert_case_eq(
            "defaultRasters",
            fixture,
            &js["defaultRasters"][fixture],
            &rust,
        );
    }

    for test_case in &cases.raster_cases {
        let rust = rust_raster_case(&fixture_root, test_case, false).await;
        match test_case.comparison.as_deref() {
            Some("jsRuntimeUnsupported") => {
                assert!(
                    js["rasterCases"][&test_case.id]["error"]["message"]
                        .as_str()
                        .is_some_and(|message| message.contains("createImageBitmap"))
                );
                assert!(rust.get("ok").is_some(), "Rust must natively decode WebP");
            }
            Some("numericTolerance") => {
                assert_lossy_case(test_case, &js["rasterCases"][&test_case.id], &rust);
            }
            _ => assert_case_eq(
                "rasterCases",
                &test_case.id,
                &js["rasterCases"][&test_case.id],
                &rust,
            ),
        }
    }

    for test_case in &cases.block_cases {
        let rust = rust_block_case(&fixture_root, test_case).await;
        assert_case_eq(
            "blockCases",
            &test_case.id,
            &js["blockCases"][&test_case.id],
            &rust,
        );
    }

    for test_case in &cases.rgb_cases {
        let rust = rust_raster_case(&fixture_root, test_case, true).await;
        match test_case.comparison.as_deref() {
            Some("jsRuntimeUnsupported") => {
                assert!(
                    js["rgbCases"][&test_case.id]["error"]["message"]
                        .as_str()
                        .is_some_and(|message| message.contains("createImageBitmap"))
                );
                assert!(rust.get("ok").is_some(), "Rust must natively decode WebP");
            }
            Some("numericTolerance") => {
                assert_lossy_case(test_case, &js["rgbCases"][&test_case.id], &rust);
            }
            _ => assert_case_eq(
                "rgbCases",
                &test_case.id,
                &js["rgbCases"][&test_case.id],
                &rust,
            ),
        }
    }
}
