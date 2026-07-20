use geotiff::geokeys::ParsedGeoKeyValue;
use geotiff::globals::FieldType;
use geotiff::imagefiledirectory::{IfdEntry, IfdValue};
use geotiff::{
    BestFitOptions, GeoTiffDataset, GeoTiffImage, ImageWindow, PackedSampleMode, ReadRasterResult,
    ReadRastersOptions, ReadRgbOptions, SampleFormat, TypedArray, from_file,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const TEST_DATA_COMMIT: &str = "8506204783ff26a6c49ed1f721e7e1635b2e43ee";
const GEOTIFF_JS_COMMIT: &str = "8594d1b4bde4072326916185c848e73a9e704850";
const FULL_SAMPLE_LIMIT: usize = 4_000_000;

fn manifest_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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
    json!({ "$float64Bits": format!("{:016x}", value.to_bits()) })
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

fn diagnostic_value(array: &TypedArray, index: usize) -> Value {
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

fn typed_summary(array: &TypedArray) -> Value {
    let edge = array.len().min(4);
    json!({
        "type": typed_name(array),
        "length": array.len(),
        "sha256": format!("{:x}", Sha256::digest(typed_bytes(array))),
        "first": (0..edge).map(|index| diagnostic_value(array, index)).collect::<Vec<_>>(),
        "last": (array.len().saturating_sub(edge)..array.len())
            .map(|index| diagnostic_value(array, index))
            .collect::<Vec<_>>(),
    })
}

fn raster_summary(result: &ReadRasterResult) -> Value {
    match result {
        ReadRasterResult::Interleaved(raster) => json!({
            "shape": "interleaved",
            "width": raster.width,
            "height": raster.height,
            "data": typed_summary(&raster.data),
        }),
        ReadRasterResult::Bands(raster) => json!({
            "shape": "bands",
            "width": raster.width,
            "height": raster.height,
            "bands": raster.bands.iter().map(typed_summary).collect::<Vec<_>>(),
        }),
    }
}

fn ok(value: Value) -> Value {
    json!({ "ok": value })
}

fn error() -> Value {
    json!({ "error": true })
}

fn plain_array_summary<T: ToString>(values: &[T]) -> Value {
    let strings = values.iter().map(ToString::to_string).collect::<Vec<_>>();
    let edge = values.len().min(4);
    json!({
        "type": "Array",
        "length": values.len(),
        "sha256": format!("{:x}", Sha256::digest(strings.join("\0").as_bytes())),
        "first": strings[..edge].iter().map(|value| json!(value.parse::<i64>().unwrap())).collect::<Vec<_>>(),
        "last": strings[values.len().saturating_sub(edge)..].iter()
            .map(|value| json!(value.parse::<i64>().unwrap()))
            .collect::<Vec<_>>(),
    })
}

fn js_buggy_unsigned_rationals(values: &[(u64, u64)]) -> Vec<u32> {
    let mut output = vec![0; values.len().saturating_mul(2)];
    for index in (0..values.len()).step_by(2) {
        output[index] = values[index].0 as u32;
        output[index + 1] = values[index].1 as u32;
    }
    output
}

fn js_buggy_signed_rationals(values: &[(i64, i64)]) -> Vec<i32> {
    let mut output = vec![0; values.len().saturating_mul(2)];
    for index in (0..values.len()).step_by(2) {
        output[index] = values[index].0 as i32;
        output[index + 1] = values[index].1 as i32;
    }
    output
}

fn directory_value_summary(entry: &IfdEntry) -> Value {
    match &entry.value {
        IfdValue::Unsigned(value) => json!(value),
        IfdValue::Signed(value) => json!(value),
        IfdValue::Float(value) => number(*value),
        IfdValue::Ascii(value) => json!(value),
        IfdValue::UnsignedArray(values) => match entry.field_type {
            FieldType::Byte | FieldType::Undefined => typed_summary(&TypedArray::Uint8(
                values.iter().map(|value| *value as u8).collect(),
            )),
            FieldType::Short => typed_summary(&TypedArray::Uint16(
                values.iter().map(|value| *value as u16).collect(),
            )),
            FieldType::Long | FieldType::Ifd => typed_summary(&TypedArray::Uint32(
                values.iter().map(|value| *value as u32).collect(),
            )),
            FieldType::Long8 | FieldType::Ifd8 => plain_array_summary(values),
            other => panic!("unexpected unsigned directory array type {other:?}"),
        },
        IfdValue::SignedArray(values) => match entry.field_type {
            FieldType::SByte => typed_summary(&TypedArray::Int8(
                values.iter().map(|value| *value as i8).collect(),
            )),
            FieldType::SShort => typed_summary(&TypedArray::Int16(
                values.iter().map(|value| *value as i16).collect(),
            )),
            FieldType::SLong => typed_summary(&TypedArray::Int32(
                values.iter().map(|value| *value as i32).collect(),
            )),
            FieldType::SLong8 => plain_array_summary(values),
            other => panic!("unexpected signed directory array type {other:?}"),
        },
        IfdValue::FloatArray(values) => match entry.field_type {
            FieldType::Float => typed_summary(&TypedArray::Float32(
                values.iter().map(|value| *value as f32).collect(),
            )),
            FieldType::Double => typed_summary(&TypedArray::Float64(values.clone())),
            other => panic!("unexpected float directory array type {other:?}"),
        },
        IfdValue::UnsignedRational(numerator, denominator) => {
            typed_summary(&TypedArray::Uint32(vec![
                *numerator as u32,
                *denominator as u32,
            ]))
        }
        IfdValue::SignedRational(numerator, denominator) => {
            typed_summary(&TypedArray::Int32(vec![
                *numerator as i32,
                *denominator as i32,
            ]))
        }
        IfdValue::UnsignedRationalArray(values) => {
            typed_summary(&TypedArray::Uint32(js_buggy_unsigned_rationals(values)))
        }
        IfdValue::SignedRationalArray(values) => {
            typed_summary(&TypedArray::Int32(js_buggy_signed_rationals(values)))
        }
    }
}

fn directory_summary(image: &GeoTiffImage<'_>) -> Value {
    Value::Object(Map::from_iter(image.file_directory().iter().map(
        |(tag, entry)| (tag.to_string(), directory_value_summary(entry)),
    )))
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
    Value::Object(Map::from_iter(keys.iter().map(|(id, value)| {
        (
            geotiff::geo_key_name(id)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| "undefined".to_string()),
            normalize_geokey_value(value),
        )
    })))
}

fn option_map(map: Option<BTreeMap<String, String>>) -> Value {
    map.map_or(Value::Null, |map| json!(map))
}

fn image_windows(width: usize, height: usize) -> (ImageWindow, ImageWindow, ImageWindow) {
    let window = |x0: usize, y0: usize| ImageWindow {
        x0: x0 as i64,
        y0: y0 as i64,
        x1: width.min(x0 + 64) as i64,
        y1: height.min(y0 + 64) as i64,
    };
    (
        window(0, 0),
        window(
            width.saturating_div(2).saturating_sub(32),
            height.saturating_div(2).saturating_sub(32),
        ),
        window(width.saturating_sub(64), height.saturating_sub(64)),
    )
}

async fn read_raster(image: &GeoTiffImage<'_>, options: ReadRastersOptions) -> Value {
    match image.read_rasters(options).await {
        Ok(result) => ok(raster_summary(&result)),
        Err(_) => error(),
    }
}

async fn rust_raster_cases(image: &GeoTiffImage<'_>) -> Value {
    let width = image.width();
    let height = image.height();
    let samples_per_pixel = image.samples_per_pixel();
    let (top_left, center, bottom_right) = image_windows(width, height);
    let samples = if samples_per_pixel == 1 {
        vec![0]
    } else {
        vec![samples_per_pixel - 1, 0]
    };
    let sample_count = width
        .checked_mul(height)
        .and_then(|value| value.checked_mul(samples_per_pixel))
        .unwrap_or(usize::MAX);
    let compatibility = PackedSampleMode::GeotiffJs;
    let full = if sample_count <= FULL_SAMPLE_LIMIT {
        json!({
            "bands": read_raster(image, ReadRastersOptions {
                packed_sample_mode: compatibility,
                ..ReadRastersOptions::default()
            }).await,
            "interleaved": read_raster(image, ReadRastersOptions {
                interleave: true,
                packed_sample_mode: compatibility,
                ..ReadRastersOptions::default()
            }).await,
        })
    } else {
        json!({
            "classification": "sampledLargeImage",
            "sampleCount": sample_count,
        })
    };
    json!({
        "full": full,
        "topLeftBands": read_raster(image, ReadRastersOptions {
            window: Some(top_left),
            packed_sample_mode: compatibility,
            ..ReadRastersOptions::default()
        }).await,
        "topLeftInterleaved": read_raster(image, ReadRastersOptions {
            window: Some(top_left),
            interleave: true,
            packed_sample_mode: compatibility,
            ..ReadRastersOptions::default()
        }).await,
        "selectedCenterBands": read_raster(image, ReadRastersOptions {
            window: Some(center),
            samples: samples.clone(),
            packed_sample_mode: compatibility,
            ..ReadRastersOptions::default()
        }).await,
        "selectedCenterInterleaved": read_raster(image, ReadRastersOptions {
            window: Some(center),
            samples: samples.clone(),
            interleave: true,
            packed_sample_mode: compatibility,
            ..ReadRastersOptions::default()
        }).await,
        "nearestBottomRight": read_raster(image, ReadRastersOptions {
            window: Some(bottom_right),
            samples: samples.clone(),
            interleave: true,
            width: Some(17),
            height: Some(13),
            resample_method: "nearest".to_string(),
            packed_sample_mode: compatibility,
            ..ReadRastersOptions::default()
        }).await,
        "bilinearBottomRight": read_raster(image, ReadRastersOptions {
            window: Some(bottom_right),
            samples,
            interleave: true,
            width: Some(17),
            height: Some(13),
            resample_method: "bilinear".to_string(),
            packed_sample_mode: compatibility,
            ..ReadRastersOptions::default()
        }).await,
        "outOfBoundsFill": read_raster(image, ReadRastersOptions {
            window: Some(ImageWindow {
                x0: -2,
                y0: -3,
                x1: width.min(6) as i64,
                y1: height.min(5) as i64,
            }),
            samples: vec![0],
            interleave: true,
            fill_value: Some(geotiff::FillValue::Scalar(17.0)),
            packed_sample_mode: compatibility,
            ..ReadRastersOptions::default()
        }).await,
    })
}

async fn rust_block_cases(image: &GeoTiffImage<'_>) -> Value {
    let columns = image.width().div_ceil(image.tile_width()).max(1);
    let rows = image.height().div_ceil(image.tile_height()).max(1);
    let coordinates = [
        (0, 0),
        ((columns - 1) / 2, (rows - 1) / 2),
        (columns - 1, rows - 1),
    ];
    let samples = if image.samples_per_pixel() == 1 {
        vec![0]
    } else {
        vec![0, image.samples_per_pixel() - 1]
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut output = Vec::new();
    for (x, y) in coordinates {
        for &sample in &samples {
            if !seen.insert((x, y, sample)) {
                continue;
            }
            output.push(match image.get_tile_or_strip(x, y, sample, None).await {
                Ok(block) => ok(json!({
                    "x": block.x,
                    "y": block.y,
                    "sample": block.sample,
                    "data": typed_summary(&TypedArray::Uint8(block.data)),
                })),
                Err(_) => error(),
            });
        }
    }
    Value::Array(output)
}

async fn read_rgb(
    image: &GeoTiffImage<'_>,
    window: ImageWindow,
    interleave: bool,
    alpha: bool,
) -> Value {
    match image
        .read_rgb(ReadRgbOptions {
            window: Some(window),
            interleave,
            enable_alpha: alpha,
            packed_sample_mode: PackedSampleMode::GeotiffJs,
            ..ReadRgbOptions::default()
        })
        .await
    {
        Ok(result) => ok(raster_summary(&result)),
        Err(_) => error(),
    }
}

async fn rust_rgb_cases(image: &GeoTiffImage<'_>) -> Value {
    let (window, _, _) = image_windows(image.width(), image.height());
    json!({
        "interleaved": read_rgb(image, window, true, false).await,
        "bands": read_rgb(image, window, false, false).await,
        "alpha": if image.file_directory().has_tag("ExtraSamples") {
            read_rgb(image, window, true, true).await
        } else {
            json!({ "classification": "notApplicableNoExtraSamples" })
        },
    })
}

fn sample_format_number(format: SampleFormat) -> u16 {
    format.to_u16()
}

async fn rust_image_summary(image: GeoTiffImage<'_>, index: usize) -> Value {
    let samples = image.samples_per_pixel();
    let bits = (0..samples)
        .map(|sample| image.bits_per_sample(sample).unwrap())
        .collect::<Vec<_>>();
    let formats = (0..samples)
        .map(|sample| sample_format_number(image.sample_format(sample).unwrap()))
        .collect::<Vec<_>>();
    let geo_keys = ok(normalize_geokeys(&image));
    let tie_points = match image.tie_points() {
        Ok(points) => ok(Value::Array(
            points
                .iter()
                .map(|point| {
                    json!({
                        "i": number(point.i), "j": number(point.j), "k": number(point.k),
                        "x": number(point.x), "y": number(point.y), "z": number(point.z),
                    })
                })
                .collect(),
        )),
        Err(_) => error(),
    };
    let gdal_metadata = match image.gdal_metadata(None) {
        Ok(value) => ok(option_map(value)),
        Err(_) => error(),
    };
    let origin = image
        .origin()
        .map(|values| ok(Value::Array(values.into_iter().map(number).collect())))
        .unwrap_or_else(|_| error());
    let resolution = image
        .resolution(None)
        .map(|values| ok(Value::Array(values.into_iter().map(number).collect())))
        .unwrap_or_else(|_| error());
    let bounding_box = image
        .bounding_box(false)
        .map(|values| ok(Value::Array(values.into_iter().map(number).collect())))
        .unwrap_or_else(|_| error());
    let tilegrid_bounding_box = image
        .bounding_box(true)
        .map(|values| ok(Value::Array(values.into_iter().map(number).collect())))
        .unwrap_or_else(|_| error());
    let metadata = json!({
        "width": image.width(),
        "height": image.height(),
        "samplesPerPixel": samples,
        "tiled": image.is_tiled(),
        "planarConfiguration": image.planar_configuration(),
        "tileWidth": image.tile_width(),
        "tileHeight": image.tile_height(),
        "bits": bits,
        "formats": formats,
        "geoKeys": geo_keys,
        "tiePoints": tie_points,
        "gdalMetadata": gdal_metadata,
        "gdalNoData": image.gdal_nodata().map(number).unwrap_or(Value::Null),
        "origin": origin,
        "resolution": resolution,
        "pixelIsArea": image.pixel_is_area(),
        "boundingBox": bounding_box,
        "tilegridBoundingBox": tilegrid_bounding_box,
        "directory": directory_summary(&image),
    });
    let rasters = rust_raster_cases(&image).await;
    let blocks = rust_block_cases(&image).await;
    let rgb = rust_rgb_cases(&image).await;
    json!({
        "index": index,
        "metadata": metadata,
        "rasters": rasters,
        "blocks": blocks,
        "rgb": rgb,
    })
}

async fn rust_file_summary(files_root: &Path, name: &str) -> Value {
    let path = files_root.join(name);
    let bytes = std::fs::read(&path).unwrap();
    let file_facts = json!({
        "byteLength": bytes.len(),
        "sha256": format!("{:x}", Sha256::digest(&bytes)),
    });
    let single = match from_file(&path).await {
        Ok(dataset) => dataset,
        Err(_) => return json!({ "file": file_facts, "open": { "error": true } }),
    };
    let big_tiff = single.is_big_tiff();
    let ghost_values = match single.ghost_values().await {
        Ok(value) => ok(option_map(value)),
        Err(_) => error(),
    };
    let dataset = GeoTiffDataset::from(single);
    let image_count = dataset.image_count();
    let first = dataset.image(0).unwrap();
    let little_endian = first.little_endian();
    let best_fit_window = ImageWindow {
        x0: 0,
        y0: 0,
        x1: first.width().min(64) as i64,
        y1: first.height().min(64) as i64,
    };
    let best_fit = match dataset
        .read_rasters_best_fit(BestFitOptions {
            window: Some(best_fit_window),
            out_width: Some(first.width().min(31)),
            out_height: Some(first.height().min(29)),
            interleave: true,
            packed_sample_mode: PackedSampleMode::GeotiffJs,
            ..BestFitOptions::default()
        })
        .await
    {
        Ok(result) => ok(raster_summary(&result)),
        Err(_) => error(),
    };
    let mut images = Vec::with_capacity(image_count);
    for index in 0..image_count {
        images.push(rust_image_summary(dataset.image(index).unwrap(), index).await);
    }
    json!({
        "file": file_facts,
        "open": {
            "ok": {
                "imageCount": image_count,
                "bigTiff": big_tiff,
                "littleEndian": little_endian,
                "ghostValues": ghost_values,
                "bestFit": best_fit,
                "images": images,
            }
        }
    })
}

fn corpus_root() -> PathBuf {
    std::env::var_os("GEOTIFF_TEST_DATA_DIR")
        .map(PathBuf::from)
        .expect("GEOTIFF_TEST_DATA_DIR must point to a GeoTIFF/test-data checkout")
}

fn corpus_names(files_root: &Path) -> Vec<String> {
    let mut names = std::fs::read_dir(files_root)
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let lowercase = name.to_ascii_lowercase();
            (lowercase.ends_with(".tif") || lowercase.ends_with(".tiff")).then_some(name)
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn assert_checkout_commit(root: &Path, expected: &str, label: &str) {
    let output = Command::new("git")
        .args(["-C", root.to_str().unwrap(), "rev-parse", "HEAD"])
        .output()
        .unwrap_or_else(|error| panic!("read {label} commit: {error}"));
    assert!(output.status.success(), "read {label} commit");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        expected,
        "unexpected {label} commit"
    );
}

fn run_js_oracle(root: &Path) -> Value {
    let manifest = manifest_root();
    let js_root = std::env::var_os("GEOTIFF_JS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../geotiff.js"));
    assert_checkout_commit(&js_root, GEOTIFF_JS_COMMIT, "geotiff.js");
    let output = Command::new("node")
        .args([
            manifest
                .join("tests/differential/js_test_data_oracle.mjs")
                .as_os_str(),
            js_root.as_os_str(),
            root.as_os_str(),
        ])
        .output()
        .expect("run GeoTIFF/test-data JavaScript oracle");
    assert!(
        output.status.success(),
        "GeoTIFF/test-data JS oracle failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse GeoTIFF/test-data JS oracle")
}

fn first_difference(left: &Value, right: &Value, path: &str) -> Option<String> {
    if left == right {
        return None;
    }

    match (left, right) {
        (Value::Object(left), Value::Object(right)) => {
            for key in left.keys() {
                if !right.contains_key(key) {
                    return Some(format!("{path}.{key}: missing from Rust result"));
                }
            }
            for key in right.keys() {
                if !left.contains_key(key) {
                    return Some(format!("{path}.{key}: missing from JavaScript result"));
                }
            }
            for (key, left_value) in left {
                if let Some(difference) =
                    first_difference(left_value, &right[key], &format!("{path}.{key}"))
                {
                    return Some(difference);
                }
            }
            None
        }
        (Value::Array(left), Value::Array(right)) => {
            if left.len() != right.len() {
                return Some(format!(
                    "{path}: array length differs (JavaScript={}, Rust={})",
                    left.len(),
                    right.len()
                ));
            }
            for (index, (left_value, right_value)) in left.iter().zip(right).enumerate() {
                if let Some(difference) =
                    first_difference(left_value, right_value, &format!("{path}[{index}]"))
                {
                    return Some(difference);
                }
            }
            None
        }
        _ => Some(format!(
            "{path}: JavaScript={} Rust={}",
            serde_json::to_string(left).unwrap(),
            serde_json::to_string(right).unwrap()
        )),
    }
}

#[tokio::test]
#[ignore = "requires a pinned GeoTIFF/test-data checkout, its extracted ZIP, geotiff.js 3.1.0, and Node.js"]
async fn live_geotiff_test_data_corpus_differential() {
    let root = corpus_root();
    assert_checkout_commit(&root, TEST_DATA_COMMIT, "GeoTIFF/test-data");
    let files_root = root.join("files");
    let names = corpus_names(&files_root);
    assert_eq!(names.len(), 22, "all 21 direct TIFFs plus the ZIP member");
    assert!(
        names.contains(&"spam2005v3r2_harvested-area_wheat_total.tiff".to_string()),
        "the repository's ZIP member must be extracted before running"
    );

    let js = run_js_oracle(&root);
    assert_eq!(js["reference"]["version"], "3.1.0");
    assert_eq!(js["policy"]["fullSampleLimit"], FULL_SAMPLE_LIMIT);
    assert_eq!(js["names"], json!(names));

    for name in &names {
        let rust = rust_file_summary(&files_root, name).await;
        if js["files"][name] != rust {
            panic!(
                "GeoTIFF/test-data differential mismatch in {name}: {}",
                first_difference(&js["files"][name], &rust, "$file")
                    .expect("unequal values must have a first difference")
            );
        }
    }
}
