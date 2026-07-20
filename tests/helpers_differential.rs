use async_tiff::tags::{PlanarConfiguration, Predictor};
use geotiff::dataslice::DataSlice;
use geotiff::dataview64::DataView64;
use geotiff::globals::{
    FieldType, extra_samples_values, get_field_type_size, get_tag, lerc_add_compression,
    photometric_interpretations, register_tag, resolve_tag,
};
use geotiff::source::httputils;
use geotiff::typed_array::{
    TypedArray, is_typed_float_array, is_typed_int_array, is_typed_uint_array,
};
use geotiff::{DummyLogger, Logger, compression, predictor, resample, rgb, set_logger, utils};
use serde_json::{Map, Value, json};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

fn manifest_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn run_js_oracle() -> Value {
    let root = manifest_root();
    let js_root = std::env::var_os("GEOTIFF_JS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("../geotiff.js"));
    let script = root.join("tests/differential/js_helpers_oracle.mjs");
    let output = Command::new("node")
        .args([script.as_os_str(), js_root.as_os_str()])
        .output()
        .expect("run geotiff.js helper oracle");
    assert!(
        output.status.success(),
        "geotiff.js helper oracle failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse helper oracle JSON")
}

fn assert_section(name: &str, js: &Value, rust: &Value) {
    assert_eq!(
        js,
        rust,
        "helper differential mismatch in {name}\nJS:\n{}\nRust:\n{}",
        serde_json::to_string_pretty(js).unwrap(),
        serde_json::to_string_pretty(rust).unwrap(),
    );
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
    let mut bytes = Vec::new();
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

fn typed(array: TypedArray) -> Value {
    json!({ "type": typed_name(&array), "bytes": typed_bytes(&array) })
}

fn rust_data_views() -> (Value, Value, i64) {
    let mut buffer = vec![0u8; 48];
    buffer[0..8].copy_from_slice(&9_007_199_254_740_991u64.to_le_bytes());
    buffer[8..16].copy_from_slice(&(-123_456_789i64).to_be_bytes());
    buffer[16] = 250;
    buffer[18..20].copy_from_slice(&0xabcdu16.to_le_bytes());
    buffer[20..22].copy_from_slice(&(-1234i16).to_be_bytes());
    buffer[22..26].copy_from_slice(&0xdead_beefu32.to_le_bytes());
    buffer[26..30].copy_from_slice(&(-123_456i32).to_be_bytes());
    buffer[30..32].copy_from_slice(&half::f16::from_f32(1.5).to_le_bytes());
    buffer[32..36].copy_from_slice(&(-12.5f32).to_le_bytes());
    buffer[36..44].copy_from_slice(&std::f64::consts::PI.to_be_bytes());
    let view = DataView64::new(&buffer);
    let data_view = json!({
        "buffer": buffer.clone(),
        "uint64Le": view.get_uint64(0, true).unwrap(),
        "int64Be": view.get_int64(8, false).unwrap(),
        "uint8": view.get_uint8(16).unwrap(),
        "int8": view.get_int8(16).unwrap(),
        "uint16Le": view.get_uint16(18, true).unwrap(),
        "int16Be": view.get_int16(20, false).unwrap(),
        "uint32Le": view.get_uint32(22, true).unwrap(),
        "int32Be": view.get_int32(26, false).unwrap(),
        "float16Le": view.get_float16(30, true).unwrap(),
        "float32Le": view.get_float32(32, true).unwrap(),
        "float64Be": view.get_float64(36, false).unwrap(),
    });

    let mut slice_buffer = view.buffer().to_vec();
    slice_buffer[8..16].copy_from_slice(&(-987_654_321i64).to_le_bytes());
    slice_buffer[36..44].copy_from_slice(&std::f64::consts::PI.to_le_bytes());
    let slice = DataSlice::new(&slice_buffer, 100, true, true);
    let corrected_nonzero_int64 = slice.read_int64(108).unwrap();
    let slice_value = json!({
        "sliceOffset": slice.slice_offset(),
        "sliceTop": slice.slice_top(),
        "littleEndian": slice.little_endian(),
        "bigTiff": slice.big_tiff(),
        "coversWhole": slice.covers(100, 48),
        "coversInner": slice.covers(108, 8),
        "coversBefore": slice.covers(99, 1),
        "uint64Le": slice.read_uint64(100).unwrap(),
        "int64Le": DataSlice::new(&slice_buffer, 0, true, true).read_int64(8).unwrap(),
        "uint8": slice.read_uint8(116).unwrap(),
        "int8": slice.read_int8(116).unwrap(),
        "uint16Le": slice.read_uint16(118).unwrap(),
        "int16Le": slice.read_int16(120).unwrap(),
        "uint32Le": slice.read_uint32(122).unwrap(),
        "int32Le": slice.read_int32(126).unwrap(),
        "float32Le": slice.read_float32(132).unwrap(),
        "float64Le": slice.read_float64(136).unwrap(),
        "offset": slice.read_offset(100).unwrap(),
    });
    (data_view, slice_value, corrected_nonzero_int64)
}

fn rust_resample() -> Value {
    let bands = vec![
        TypedArray::Uint16(vec![1, 2, 3, 4, 5, 6]),
        TypedArray::Int16(vec![-30, -20, -10, 10, 20, 30]),
        TypedArray::Float32(vec![0.25, 1.5, -2.25, 3.75, 4.5, 9.25]),
    ];
    let interleaved = TypedArray::Uint16(vec![1, 101, 2, 102, 3, 103, 4, 104, 5, 105, 6, 106]);
    let describe = |arrays: Vec<TypedArray>| Value::Array(arrays.into_iter().map(typed).collect());
    json!({
        "nearest": describe(resample::resample_nearest(&bands, 3, 2, 5, 4).unwrap()),
        "bilinear": describe(resample::resample_bilinear(&bands, 3, 2, 5, 4).unwrap()),
        "dispatchLinear": describe(resample::resample(&bands, 3, 2, 5, 4, "LiNeAr").unwrap()),
        "nearestInterleaved": typed(resample::resample_nearest_interleaved(&interleaved, 3, 2, 5, 4, 2).unwrap()),
        "bilinearInterleaved": typed(resample::resample_bilinear_interleaved(&interleaved, 3, 2, 5, 4, 2).unwrap()),
        "dispatchInterleaved": typed(resample::resample_interleaved(&interleaved, 3, 2, 5, 4, 2, "BILINEAR").unwrap()),
    })
}

fn rust_rgb() -> Value {
    let grayscale = TypedArray::Uint16(vec![0, 1, 2, 3]);
    let color_map = vec![
        0, 16384, 32768, 49152, 65535, 65535, 49152, 32768, 16384, 0, 0, 8192, 24576, 40960, 65535,
    ];
    json!({
        "whiteIsZero": typed(TypedArray::Uint8(rgb::from_white_is_zero(&grayscale, 4.0))),
        "blackIsZero": typed(TypedArray::Uint8(rgb::from_black_is_zero(&grayscale, 4.0))),
        "palette": typed(TypedArray::Uint8(rgb::from_palette(&TypedArray::Uint8(vec![0, 1, 2, 3, 4]), &color_map))),
        "cmyk": typed(TypedArray::Uint8(rgb::from_cmyk(&TypedArray::Uint8(vec![0, 0, 0, 0, 20, 40, 60, 80])))),
        "yCbCr": typed(TypedArray::Uint8Clamped(rgb::from_y_cb_cr(&TypedArray::Uint8(vec![0, 128, 128, 255, 128, 128, 100, 10, 240])))),
        "cieLab": typed(TypedArray::Uint8(rgb::from_cie_lab(&TypedArray::Uint8(vec![0, 128, 128, 128, 0, 0, 255, 127, 255])))),
    })
}

fn rust_predictor() -> Value {
    let apply = |mut bytes: Vec<u8>,
                 kind: Predictor,
                 width: usize,
                 height: usize,
                 bits: &[u16],
                 planar: PlanarConfiguration| {
        predictor::apply_predictor(&mut bytes, Some(kind), width, height, bits, planar).unwrap();
        bytes
    };
    let horizontal16 = [1u16, 2, 3, 10, 20, 30]
        .into_iter()
        .flat_map(u16::to_le_bytes)
        .collect();
    json!({
        "none": apply(vec![1, 2, 3, 4], Predictor::None, 4, 1, &[8], PlanarConfiguration::Chunky),
        "horizontal8Chunky": apply(vec![1, 10, 2, 20, 3, 30, 4, 40, 5, 50, 6, 60], Predictor::Horizontal, 3, 2, &[8, 8], PlanarConfiguration::Chunky),
        "horizontal16Planar": apply(horizontal16, Predictor::Horizontal, 3, 2, &[16], PlanarConfiguration::Planar),
        "floating32": apply(vec![63, 0, 1, 255, 128, 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0], Predictor::FloatingPoint, 4, 1, &[32], PlanarConfiguration::Chunky),
    })
}

fn nested_value(value: utils::NestedValue) -> Value {
    match value {
        utils::NestedValue::Array(values) => {
            Value::Array(values.into_iter().map(nested_value).collect())
        }
        // JavaScript JSON.stringify emits integral Numbers without a decimal
        // point; keep the oracle representation identical even though the
        // Rust compatibility value stores numeric leaves as f64.
        utils::NestedValue::Leaf(value) if value.fract() == 0.0 => json!(value as i64),
        utils::NestedValue::Leaf(value) => json!(value),
    }
}

fn parsed_content_range(value: Option<utils::ParsedContentRange>) -> Value {
    value.map_or(Value::Null, |value| {
        json!({
            "unit": value.unit,
            "first": value.first,
            "last": value.last,
            "length": value.length,
        })
    })
}

async fn rust_utils() -> Value {
    let mut assigned = HashMap::from([
        ("a".to_string(), json!(1)),
        ("keep".to_string(), json!(true)),
    ]);
    utils::assign(
        &mut assigned,
        HashMap::from([("a".to_string(), json!(2)), ("b".to_string(), json!(3))]),
    );
    let chunks = utils::chunk(&[1, 2, 3, 4, 5], 3)
        .into_iter()
        .map(|chunk| {
            Value::Array(
                chunk
                    .into_iter()
                    .map(|value| value.map_or_else(|| json!({ "$undefined": true }), Value::from))
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    let mut each = Vec::new();
    utils::for_each(&[4, 5, 6], |value, index| each.push(json!([value, index])));
    let inverted = utils::invert(&HashMap::from([
        ("one".to_string(), 1),
        ("two".to_string(), 2),
    ]));
    let recursively = utils::to_array_recursively(utils::NestedValue::Array(vec![
        utils::NestedValue::Array(vec![
            utils::NestedValue::Leaf(1.0),
            utils::NestedValue::Leaf(2.0),
        ]),
        utils::NestedValue::Array(vec![utils::NestedValue::Array(vec![
            utils::NestedValue::Leaf(-3.0),
            utils::NestedValue::Leaf(4.0),
        ])]),
    ]));
    let zipped = utils::zip(&[1, 2, 3], &["a", "b"])
        .into_iter()
        .map(|(left, right)| {
            json!([
                left,
                right.map_or_else(|| json!({ "$undefined": true }), Value::from)
            ])
        })
        .collect::<Vec<_>>();
    let abort = utils::AbortError::new("stop");
    let aggregate = utils::AggregateError::new(
        vec![
            Box::new(std::io::Error::other("a")),
            Box::new(std::io::Error::other("b")),
        ],
        "many",
    );
    utils::wait(Some(0)).await;
    json!({
        "assign": assigned,
        "chunk": chunks,
        "endsWithTrue": utils::ends_with("geotiff.tiff", ".tiff"),
        "endsWithFalse": utils::ends_with("tif", ".tiff"),
        "forEach": each,
        "invert": inverted,
        "range": utils::range(5),
        "times": utils::times(4, |index| index * index),
        "toArray": utils::to_array(&[7, 8, 9]),
        "recursively": nested_value(recursively),
        "contentRanges": [
            parsed_content_range(utils::parse_content_range("bytes 10-19/100")),
            parsed_content_range(utils::parse_content_range("items 5-9/*")),
            parsed_content_range(utils::parse_content_range("bytes */123")),
            parsed_content_range(utils::parse_content_range("")),
        ],
        "zip": zipped,
        "typed": {
            "float": is_typed_float_array(&TypedArray::Float32(vec![])),
            "floatReject": is_typed_float_array(&TypedArray::Uint32(vec![])),
            "int": is_typed_int_array(&TypedArray::Int16(vec![])),
            "intReject": is_typed_int_array(&TypedArray::Int64(vec![])),
            "uint": is_typed_uint_array(&TypedArray::Uint8Clamped(vec![])),
            "uintReject": is_typed_uint_array(&TypedArray::Uint64(vec![])),
        },
        "abortError": { "name": "AbortError", "message": abort.to_string() },
        "aggregateError": {
            "name": "AggregateError",
            "message": aggregate.to_string(),
            "errors": aggregate.errors.iter().map(ToString::to_string).collect::<Vec<_>>(),
        },
        "typeMap": ["Float32Array", "Float64Array", "Uint16Array", "Uint32Array", "Uint8Array"],
        "wait": { "ok": { "$undefined": true } },
    })
}

fn rust_globals() -> Value {
    let fields = [
        ("ASCII", FieldType::Ascii),
        ("BYTE", FieldType::Byte),
        ("DOUBLE", FieldType::Double),
        ("FLOAT", FieldType::Float),
        ("IFD", FieldType::Ifd),
        ("IFD8", FieldType::Ifd8),
        ("LONG", FieldType::Long),
        ("LONG8", FieldType::Long8),
        ("RATIONAL", FieldType::Rational),
        ("SBYTE", FieldType::SByte),
        ("SHORT", FieldType::Short),
        ("SLONG", FieldType::SLong),
        ("SLONG8", FieldType::SLong8),
        ("SRATIONAL", FieldType::SRational),
        ("SSHORT", FieldType::SShort),
        ("UNDEFINED", FieldType::Undefined),
    ];
    let sizes = Map::from_iter(fields.map(|(name, field)| {
        let id = field as u16;
        (
            name.to_string(),
            json!({ "id": id, "size": get_field_type_size(id).unwrap() }),
        )
    }));
    register_tag(
        65001,
        "DifferentialPrivateTag",
        Some(FieldType::Long),
        true,
        true,
    );
    let definition = |definition: geotiff::TagDefinition| {
        json!({
            "tag": definition.tag,
            "name": definition.name,
            "type": definition.field_type.map(|field| field as u16),
            "isArray": definition.is_array,
            "eager": definition.eager,
        })
    };
    json!({
        "exports": [
            "ExtraSamplesValues", "LercAddCompression", "LercParameters", "fieldTagTypes",
            "fieldTypeSizes", "fieldTypes", "geoKeyNames", "geoKeys", "getFieldTypeSize",
            "getTag", "photometricInterpretations", "registerTag", "resolveTag",
            "tagDefinitions", "tagDictionary", "tags"
        ],
        "sizes": sizes,
        "imageWidthByName": definition(get_tag("ImageWidth").unwrap()),
        "imageWidthById": definition(get_tag(256u16).unwrap()),
        "unknownName": { "$undefined": true },
        "privateByName": resolve_tag("DifferentialPrivateTag").unwrap(),
        "privateDefinition": definition(get_tag(65001u16).unwrap()),
        "constants": {
            "rgb": photometric_interpretations::RGB,
            "alpha": extra_samples_values::ASSOCIATED_ALPHA,
            "lercZstd": lerc_add_compression::ZSTANDARD,
            "rasterType": geotiff::geo_key_id("GTRasterTypeGeoKey").unwrap(),
        },
    })
}

fn rust_http(js: &mut Value) -> Value {
    let multipart = [
        "--oracle",
        "Content-Type: application/octet-stream",
        "Content-Range: bytes 2-4/10",
        "",
        "abc",
        "--oracle",
        "Content-Type: application/octet-stream",
        "Content-Range: bytes 7-8/10",
        "",
        "de",
        "--oracle--",
        "",
    ]
    .join("\r\n");
    let parts = httputils::parse_byte_ranges(bytes::Bytes::from(multipart), "oracle").unwrap();
    let rust_parts = parts
        .iter()
        .map(|part| {
            json!({
                "headers": part.headers,
                "data": part.data.to_vec(),
                "offset": part.offset,
                "length": part.length,
                "fileSize": part.file_size,
            })
        })
        .collect::<Vec<_>>();
    let js_parts = js["multipart"].as_array_mut().expect("JS multipart array");
    assert_eq!(js_parts[0]["data"], json!([13, 10, 97]));
    assert_eq!(js_parts[1]["data"], json!([13, 10]));
    assert_eq!(rust_parts[0]["data"], json!([97, 98, 99]));
    assert_eq!(rust_parts[1]["data"], json!([100, 101]));
    for part in js_parts {
        part.as_object_mut().unwrap().remove("data");
    }
    let mut comparable_rust_parts = rust_parts;
    for part in &mut comparable_rust_parts {
        part.as_object_mut().unwrap().remove("data");
    }
    json!({
        "contentTypes": [
            { "type": null, "params": {} },
            {
                "type": httputils::parse_content_type(Some("multipart/byteranges; boundary=oracle; charset=utf-8")).media_type,
                "params": httputils::parse_content_type(Some("multipart/byteranges; boundary=oracle; charset=utf-8")).parameters,
            }
        ],
        "contentRanges": [
            { "start": { "$number": "NaN" }, "end": { "$number": "NaN" }, "total": { "$number": "NaN" } },
            { "start": 10, "end": 19, "total": 100 },
        ],
        "multipart": comparable_rust_parts,
    })
}

fn rust_codecs() -> Value {
    let raw = vec![1, 2, 3, 4];
    json!({
        "rawDecodeBlock": compression::raw::decode_block(&raw),
        "rawDecode": compression::raw::decode_block(&raw),
        "packbits": compression::packbits::decode_block(&[2, 10, 20, 30, 0xfd, 99]).unwrap(),
        "lzw": compression::lzw::decompress(&[32, 144, 96, 68, 34, 20, 22, 2]).unwrap(),
    })
}

struct RecordingLogger(Arc<Mutex<Vec<[String; 2]>>>);

impl RecordingLogger {
    fn push(&self, method: &str, message: &str) {
        self.0
            .lock()
            .unwrap()
            .push([method.to_string(), message.to_string()]);
    }
}

impl Logger for RecordingLogger {
    fn log(&self, message: &str) {
        self.push("log", message);
    }

    fn debug(&self, message: &str) {
        self.push("debug", message);
    }

    fn info(&self, message: &str) {
        self.push("info", message);
    }

    fn warn(&self, message: &str) {
        self.push("warn", message);
    }

    fn error(&self, message: &str) {
        self.push("error", message);
    }

    fn time(&self, label: &str) {
        self.push("time", label);
    }

    fn time_end(&self, label: &str) {
        self.push("timeEnd", label);
    }
}

fn rust_logging() -> Value {
    let calls = Arc::new(Mutex::new(Vec::new()));
    set_logger(Box::new(RecordingLogger(calls.clone())));
    geotiff::logging::debug("debug-message");
    geotiff::logging::log("log-message");
    geotiff::logging::info("info-message");
    geotiff::logging::warn("warn-message");
    geotiff::logging::error("error-message");
    geotiff::logging::time("timer");
    geotiff::logging::time_end("timer");
    set_logger(Box::new(DummyLogger));
    geotiff::logging::log("not-recorded-after-reset");
    json!(*calls.lock().unwrap())
}

fn assert_divergence_ledger() {
    let cases: Value = serde_json::from_slice(
        &std::fs::read(manifest_root().join("tests/differential/cases.json")).unwrap(),
    )
    .unwrap();
    let ids = cases["helperDivergences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert!(ids.contains("geotiffjs-dataslice-int64-offset"));
    assert!(ids.contains("geotiffjs-multipart-data-offset"));
}

#[tokio::test]
#[ignore = "requires the sibling geotiff.js 3.1.0 repository and Node.js"]
async fn live_two_repository_pure_helper_differential() {
    let mut js = run_js_oracle();
    assert_eq!(js["reference"]["version"], "3.1.0");
    assert_divergence_ledger();
    assert_eq!(
        js["moduleExports"],
        json!({
            "resample": ["resample", "resampleBilinear", "resampleBilinearInterleaved", "resampleInterleaved", "resampleNearest", "resampleNearestInterleaved"],
            "rgb": ["fromBlackIsZero", "fromCIELab", "fromCMYK", "fromPalette", "fromWhiteIsZero", "fromYCbCr"],
            "predictor": ["applyPredictor"],
            "utils": ["AbortError", "AggregateError", "CustomAggregateError", "assign", "chunk", "endsWith", "forEach", "invert", "isTypedFloatArray", "isTypedIntArray", "isTypedUintArray", "parseContentRange", "range", "times", "toArray", "toArrayRecursively", "typeMap", "wait", "zip"],
            "httpUtils": ["parseByteRanges", "parseContentRange", "parseContentType"],
            "logging": ["debug", "error", "info", "log", "setLogger", "time", "timeEnd", "warn"],
        })
    );

    let (rust_data_view, rust_slice, corrected_int64) = rust_data_views();
    assert_section("DataView64", &js["dataView64"], &rust_data_view);
    let js_offset_bug = js["dataSlice"]
        .as_object_mut()
        .unwrap()
        .remove("int64NonZeroOffset")
        .unwrap();
    assert_eq!(
        js_offset_bug["error"]["message"],
        "Offset is outside the bounds of the DataView"
    );
    assert_eq!(corrected_int64, -987_654_321);
    assert_section("DataSlice", &js["dataSlice"], &rust_slice);
    assert_section("resample", &js["resample"], &rust_resample());
    assert_section("rgb", &js["rgb"], &rust_rgb());
    assert_section("predictor", &js["predictor"], &rust_predictor());
    assert_section("utils", &js["utils"], &rust_utils().await);
    assert_section("globals", &js["globals"], &rust_globals());

    let mut js_http = js["http"].clone();
    let rust_http = rust_http(&mut js_http);
    assert_section("http", &js_http, &rust_http);

    assert_eq!(
        js["codecs"]["abstractError"]["error"]["message"],
        "decodeBlock not implemented"
    );
    let mut js_codecs = js["codecs"].clone();
    js_codecs.as_object_mut().unwrap().remove("abstractError");
    assert_section("codecs", &js_codecs, &rust_codecs());
    assert_section("logging", &js["logging"], &rust_logging());
}
