use async_tiff::error::AsyncTiffError;
use async_tiff::tags::Compression;
use geotiff::compression::registry::{build_decoder_registry, get_decoder};
use geotiff::dataset::{BestFitOptions, GeoTiffDataset};
use geotiff::geotiff::GeoTiffImageIndexError;
use geotiff::geotiffimage::{FillValue, ReadRastersOptions, ReadRgbOptions};
use geotiff::raster::ImageWindow;
use geotiff::writer::{self, WriterMetadata, write_array_buffer};
use geotiff::{from_array_buffer, from_file};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fmt::Display;
use std::path::PathBuf;
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    root().join("tests/fixtures").join(name)
}

fn run_js_oracle() -> Value {
    let js_root = std::env::var_os("GEOTIFF_JS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join("../geotiff.js"));
    let output = Command::new("node")
        .args([
            root()
                .join("tests/differential/js_errors_oracle.mjs")
                .as_os_str(),
            js_root.as_os_str(),
            root().join("tests/fixtures").as_os_str(),
        ])
        .output()
        .expect("run geotiff.js error oracle");
    assert!(
        output.status.success(),
        "geotiff.js error oracle failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse error oracle JSON")
}

fn reference_error<'a>(
    js: &'a Value,
    section: &str,
    case: &str,
    name: &str,
    message: &str,
) -> &'a str {
    let error = &js[section][case]["error"];
    assert_eq!(
        error["name"], name,
        "unexpected reference error class for {section}.{case}"
    );
    assert_eq!(
        error["message"], message,
        "unexpected reference error message for {section}.{case}"
    );
    error["message"].as_str().unwrap()
}

fn result_error<T, E: Display>(result: Result<T, E>) -> String {
    match result {
        Ok(_) => panic!("operation unexpectedly succeeded"),
        Err(error) => error.to_string(),
    }
}

fn async_error<T>(result: Result<T, AsyncTiffError>) -> AsyncTiffError {
    match result {
        Ok(_) => panic!("operation unexpectedly succeeded"),
        Err(error) => error,
    }
}

fn async_error_message(error: AsyncTiffError) -> String {
    match error {
        AsyncTiffError::General(message) => message,
        other => other.to_string(),
    }
}

fn async_result_error<T>(result: Result<T, AsyncTiffError>) -> String {
    async_error_message(async_error(result))
}

fn assert_async_message<T>(result: Result<T, AsyncTiffError>, expected: &str) {
    assert_eq!(async_result_error(result), expected);
}

fn read_u16(bytes: &[u8], offset: usize, little: bool) -> u16 {
    let value = [bytes[offset], bytes[offset + 1]];
    if little {
        u16::from_le_bytes(value)
    } else {
        u16::from_be_bytes(value)
    }
}

fn read_u32(bytes: &[u8], offset: usize, little: bool) -> u32 {
    let value = [
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ];
    if little {
        u32::from_le_bytes(value)
    } else {
        u32::from_be_bytes(value)
    }
}

fn with_short_tag(mut bytes: Vec<u8>, tag: u16, value: u16) -> Vec<u8> {
    let little = &bytes[..2] == b"II";
    let ifd_offset = read_u32(&bytes, 4, little) as usize;
    let count = usize::from(read_u16(&bytes, ifd_offset, little));
    for index in 0..count {
        let entry = ifd_offset + 2 + index * 12;
        if read_u16(&bytes, entry, little) == tag {
            let encoded = if little {
                value.to_le_bytes()
            } else {
                value.to_be_bytes()
            };
            bytes[entry + 8..entry + 10].copy_from_slice(&encoded);
            return bytes;
        }
    }
    panic!("fixture does not contain TIFF tag {tag}");
}

fn header(byte_order: [u8; 2], magic: u16, offset_size: u16, reserved: u16) -> Vec<u8> {
    let mut bytes = vec![0u8; 1024];
    bytes[..2].copy_from_slice(&byte_order);
    let little = byte_order == *b"II";
    let encode = |value: u16| {
        if little {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        }
    };
    bytes[2..4].copy_from_slice(&encode(magic));
    if magic == 43 {
        bytes[4..6].copy_from_slice(&encode(offset_size));
        bytes[6..8].copy_from_slice(&encode(reserved));
    }
    bytes
}

fn divergence_ids() -> BTreeSet<String> {
    let cases: Value = serde_json::from_slice(
        &std::fs::read(root().join("tests/differential/cases.json")).unwrap(),
    )
    .unwrap();
    cases["errorDivergences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn invalid_public_tile_coordinates_return_an_error_instead_of_panicking() {
    let dataset = from_file(fixture("tiled-gray-i1.tif")).await.unwrap();
    let error = async_result_error(
        dataset
            .image(0)
            .unwrap()
            .get_tile_or_strip(999, 999, 0, None)
            .await,
    );
    assert_eq!(error, "Offset is outside the bounds of the DataView");
}

#[tokio::test]
#[ignore = "requires the sibling geotiff.js 3.1.0 repository and Node.js"]
async fn live_two_repository_error_and_validation_differential() {
    let js = run_js_oracle();
    assert_eq!(js["reference"]["version"], "3.1.0");
    assert_eq!(
        divergence_ids(),
        [
            "geotiffjs-recursive-image-index",
            "native-abstract-source-trait",
            "native-bigtiff-reserved-field-validation",
            "native-empty-writer-validation",
            "native-invalid-chunky-block-sample-rejection",
            "native-typed-writer-metadata",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );

    // Container-header failures are part of the factory contract, not merely
    // parser implementation details.
    let expected = reference_error(
        &js,
        "headerErrors",
        "invalidByteOrder",
        "TypeError",
        "Invalid byte order value.",
    );
    assert_async_message(from_array_buffer(header(*b"ZZ", 42, 8, 0)).await, expected);
    let expected = reference_error(
        &js,
        "headerErrors",
        "invalidMagic",
        "TypeError",
        "Invalid magic number.",
    );
    assert_async_message(from_array_buffer(header(*b"II", 99, 8, 0)).await, expected);
    let expected = reference_error(
        &js,
        "headerErrors",
        "invalidBigTiffOffsetSize",
        "Error",
        "Unsupported offset byte-size.",
    );
    assert_async_message(from_array_buffer(header(*b"II", 43, 4, 0)).await, expected);
    assert_eq!(js["headerErrors"]["nonZeroBigTiffReserved"]["ok"], true);
    assert_eq!(
        async_result_error(from_array_buffer(header(*b"II", 43, 8, 1)).await),
        "Invalid BigTIFF reserved header field"
    );
    let missing_js = &js["headerErrors"]["missingFile"]["error"];
    assert_eq!(missing_js["name"], "Error");
    assert!(missing_js["message"].as_str().unwrap().contains("ENOENT"));
    let missing_rust = result_error(from_file(fixture("__missing__.tif")).await);
    assert!(
        missing_rust.contains("No such file") || missing_rust.contains("not found"),
        "unexpected missing-file error: {missing_rust}"
    );

    let fixture_bytes = std::fs::read(fixture("tiled-gray-i1.tif")).unwrap();
    let dataset = from_array_buffer(fixture_bytes.clone()).await.unwrap();

    // The reference recursively leaks index 1 for a request of index 5. Rust
    // retains the requested index in its typed error.
    for case in ["imageIndex", "requestIfdIndex"] {
        reference_error(
            &js,
            "datasetErrors",
            case,
            "GeoTIFFImageIndexError",
            "No image at index 1",
        );
    }
    for error in [
        async_error(dataset.image(5)),
        async_error(dataset.request_ifd(5)),
    ] {
        let AsyncTiffError::External(error) = error else {
            panic!("image index did not return an External typed error")
        };
        let error = error
            .downcast_ref::<GeoTiffImageIndexError>()
            .expect("GeoTiffImageIndexError");
        assert_eq!(error.index, 5);
        assert_eq!(error.to_string(), "No image at index 5");
    }

    let image = dataset.image(0).unwrap();
    let expected = reference_error(
        &js,
        "imageErrors",
        "reversedWindow",
        "Error",
        "Invalid subsets",
    );
    assert_async_message(
        image
            .read_rasters(ReadRastersOptions {
                window: Some(ImageWindow {
                    x0: 5,
                    y0: 5,
                    x1: 2,
                    y1: 2,
                }),
                ..ReadRastersOptions::default()
            })
            .await,
        expected,
    );
    let expected = reference_error(
        &js,
        "imageErrors",
        "invalidSample",
        "RangeError",
        "Invalid sample index '99'.",
    );
    assert_async_message(
        image
            .read_rasters(ReadRastersOptions {
                samples: vec![99],
                ..ReadRastersOptions::default()
            })
            .await,
        expected,
    );
    let expected = reference_error(
        &js,
        "imageErrors",
        "interleavedFillArray",
        "Error",
        "When reading interleaved data, fillValue must be a single number.",
    );
    assert_async_message(
        image
            .read_rasters(ReadRastersOptions {
                window: Some(ImageWindow {
                    x0: -1,
                    y0: -1,
                    x1: 2,
                    y1: 2,
                }),
                interleave: true,
                fill_value: Some(FillValue::PerSample(vec![1.0])),
                ..ReadRastersOptions::default()
            })
            .await,
        expected,
    );
    let expected = reference_error(
        &js,
        "imageErrors",
        "unknownResample",
        "Error",
        "Unsupported resampling method: 'oracle-unknown'",
    );
    assert_async_message(
        image
            .read_rasters(ReadRastersOptions {
                interleave: true,
                width: Some(5),
                height: Some(5),
                resample_method: "oracle-unknown".to_string(),
                ..ReadRastersOptions::default()
            })
            .await,
        expected,
    );
    let unsupported = "Unsupported data format/bitsPerSample";
    reference_error(
        &js,
        "imageErrors",
        "invalidReaderSample",
        "Error",
        unsupported,
    );
    reference_error(
        &js,
        "imageErrors",
        "invalidArraySample",
        "Error",
        unsupported,
    );
    assert_async_message(image.reader_for_sample(99), unsupported);
    assert_async_message(image.array_for_sample(99, 1), unsupported);

    assert_eq!(
        js["imageErrors"]["invalidTileSample"]["ok"],
        serde_json::json!({ "dataLength": 256, "sample": 99, "x": 0, "y": 0 })
    );
    assert_async_message(
        image.get_tile_or_strip(0, 0, 99, None).await,
        "Invalid sample index '99'.",
    );
    reference_error(
        &js,
        "imageErrors",
        "invalidTileCoordinates",
        "RangeError",
        "Offset is outside the bounds of the DataView",
    );
    assert!(image.get_tile_or_strip(999, 999, 0, None).await.is_err());

    let planar = from_array_buffer(with_short_tag(fixture_bytes.clone(), 284, 3))
        .await
        .unwrap();
    let expected = reference_error(
        &js,
        "imageErrors",
        "invalidPlanarConfiguration",
        "Error",
        "Invalid planar configuration.",
    );
    assert_async_message(planar.image(0), expected);

    let unsupported_rgb = from_array_buffer(with_short_tag(fixture_bytes.clone(), 262, 4))
        .await
        .unwrap();
    let expected = reference_error(
        &js,
        "imageErrors",
        "unsupportedPhotometric",
        "Error",
        "Invalid or unsupported photometric interpretation.",
    );
    assert_async_message(
        unsupported_rgb
            .image(0)
            .unwrap()
            .read_rgb(ReadRgbOptions::default())
            .await,
        expected,
    );

    let unsupported_format = from_array_buffer(with_short_tag(fixture_bytes, 339, 4))
        .await
        .unwrap();
    let expected = reference_error(
        &js,
        "imageErrors",
        "unsupportedSampleFormat",
        "Error",
        "Unsupported sample format for interleaved data. Must be 1, 2, or 3.",
    );
    assert_async_message(
        unsupported_format
            .image(0)
            .unwrap()
            .read_rasters(ReadRastersOptions {
                interleave: true,
                ..ReadRastersOptions::default()
            })
            .await,
        expected,
    );

    let geo_bytes = write_array_buffer(vec![0u8; 16], WriterMetadata::new(4, 4)).unwrap();
    let geo = GeoTiffDataset::Single(from_array_buffer(geo_bytes).await.unwrap());
    let expected = reference_error(
        &js,
        "optionErrors",
        "bboxAndWindow",
        "Error",
        "Both \"bbox\" and \"window\" passed.",
    );
    assert_async_message(
        geo.read_rasters_best_fit(BestFitOptions {
            window: Some(ImageWindow {
                x0: 0,
                y0: 0,
                x1: 2,
                y1: 2,
            }),
            bbox: Some([-180.0, 0.0, 0.0, 90.0]),
            ..BestFitOptions::default()
        })
        .await,
        expected,
    );
    let expected = reference_error(
        &js,
        "optionErrors",
        "widthAndResX",
        "Error",
        "Both width and resX passed",
    );
    assert_async_message(
        geo.read_rasters_best_fit(BestFitOptions {
            out_width: Some(2),
            res_x: Some(1.0),
            ..BestFitOptions::default()
        })
        .await,
        expected,
    );
    let expected = reference_error(
        &js,
        "optionErrors",
        "heightAndResY",
        "Error",
        "Both width and resY passed",
    );
    assert_async_message(
        geo.read_rasters_best_fit(BestFitOptions {
            out_height: Some(2),
            res_y: Some(1.0),
            ..BestFitOptions::default()
        })
        .await,
        expected,
    );

    let registry = build_decoder_registry();
    let expected = reference_error(
        &js,
        "codecErrors",
        "unknownCompression",
        "Error",
        "Unknown compression method identifier: 64000",
    );
    assert_async_message(
        get_decoder(&registry, Compression::Unknown(64_000)),
        expected,
    );
    let expected = reference_error(
        &js,
        "codecErrors",
        "oldJpeg",
        "Error",
        "old style JPEG compression is not supported.",
    );
    assert_async_message(get_decoder(&registry, Compression::JPEG), expected);

    // BaseSource and dynamically-invalid writer metadata states are rejected
    // by Rust's trait/type system before execution; keep their live JS
    // contracts pinned so a future untyped adapter cannot silently drift.
    for case in ["fetchSlice", "fetch"] {
        reference_error(
            &js,
            "sourceErrors",
            case,
            "Error",
            "fetching of slice [object Object] not possible, not implemented",
        );
    }
    reference_error(
        &js,
        "writerErrors",
        "geoAsciiType",
        "Error",
        "GeoAsciiParams must be a string if provided",
    );
    reference_error(
        &js,
        "writerErrors",
        "geoDoubleType",
        "Error",
        "GeoDoubleParams must be an array if provided",
    );
    reference_error(
        &js,
        "writerErrors",
        "geoKeyType",
        "Error",
        "GeoKey GeographicTypeGeoKey with type SHORT must have a number value",
    );

    reference_error(
        &js,
        "writerErrors",
        "empty",
        "TypeError",
        "Cannot read properties of undefined (reading 'length')",
    );
    assert_eq!(
        result_error(write_array_buffer(
            Vec::<u8>::new(),
            WriterMetadata::default()
        )),
        "image data must not be empty"
    );
    let expected = reference_error(
        &js,
        "writerErrors",
        "missingHeight",
        "Error",
        "height is required to be a number in metadata if data is a flat array",
    );
    assert_eq!(
        result_error(write_array_buffer(
            vec![1u8, 2],
            WriterMetadata {
                width: Some(2),
                ..WriterMetadata::default()
            },
        )),
        expected
    );
    let expected = reference_error(
        &js,
        "writerErrors",
        "missingWidth",
        "Error",
        "width is required to be a number in metadata if data is a flat array",
    );
    assert_eq!(
        result_error(write_array_buffer(
            vec![1u8, 2],
            WriterMetadata {
                height: Some(1),
                ..WriterMetadata::default()
            },
        )),
        expected
    );
    let expected = reference_error(
        &js,
        "writerErrors",
        "tiledSamples",
        "Error",
        "SamplesPerPixel must be specified when writing tiled images",
    );
    assert_eq!(
        result_error(write_array_buffer(
            vec![1u8, 2, 3, 4],
            WriterMetadata::new(2, 2)
                .with_tag(writer::tag::TILE_WIDTH, 2u16)
                .with_tag(writer::tag::TILE_LENGTH, 2u16)
                .with_tag(writer::tag::TILE_BYTE_COUNTS, vec![4u32]),
        )),
        expected
    );
    let expected = reference_error(
        &js,
        "writerErrors",
        "tiledDimensions",
        "Error",
        "Both TileWidth and TileLength must be specified when writing tiled images",
    );
    assert_eq!(
        result_error(write_array_buffer(
            vec![1u8, 2, 3, 4],
            WriterMetadata::new(2, 2)
                .with_tag(writer::tag::SAMPLES_PER_PIXEL, 1u16)
                .with_tag(writer::tag::TILE_BYTE_COUNTS, vec![4u32]),
        )),
        expected
    );
    let expected = reference_error(
        &js,
        "writerErrors",
        "ifdTooLarge",
        "Error",
        "Writing of IFDs with more than 1000 bytes is not supported",
    );
    assert_eq!(
        result_error(write_array_buffer(
            vec![1u8],
            WriterMetadata::new(1, 1)
                .with_tag(writer::tag::SAMPLES_PER_PIXEL, 1u16)
                .with_tag(writer::tag::TILE_WIDTH, 1u16)
                .with_tag(writer::tag::TILE_LENGTH, 1u16)
                .with_tag(writer::tag::TILE_BYTE_COUNTS, vec![1u32; 1000]),
        )),
        expected
    );
}
