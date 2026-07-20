use geotiff::{
    ImageWindow, PackedSampleMode, ReadRasterResult, ReadRastersOptions, ReadRgbOptions,
    TypedArray, from_file,
};

fn fnv1a64(array: &TypedArray) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    let mut update = |byte: u8| {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    };
    match array {
        TypedArray::Int8(values) => values.iter().for_each(|value| update(*value as u8)),
        TypedArray::Uint8(values) | TypedArray::Uint8Clamped(values) => {
            values.iter().for_each(|value| update(*value));
        }
        TypedArray::Int16(values) => values
            .iter()
            .for_each(|value| value.to_ne_bytes().into_iter().for_each(&mut update)),
        TypedArray::Uint16(values) => values
            .iter()
            .for_each(|value| value.to_ne_bytes().into_iter().for_each(&mut update)),
        TypedArray::Int32(values) => values
            .iter()
            .for_each(|value| value.to_ne_bytes().into_iter().for_each(&mut update)),
        TypedArray::Uint32(values) => values
            .iter()
            .for_each(|value| value.to_ne_bytes().into_iter().for_each(&mut update)),
        TypedArray::Int64(values) => values
            .iter()
            .for_each(|value| value.to_ne_bytes().into_iter().for_each(&mut update)),
        TypedArray::Uint64(values) => values
            .iter()
            .for_each(|value| value.to_ne_bytes().into_iter().for_each(&mut update)),
        TypedArray::Float32(values) => values
            .iter()
            .for_each(|value| value.to_ne_bytes().into_iter().for_each(&mut update)),
        TypedArray::Float64(values) => values
            .iter()
            .for_each(|value| value.to_ne_bytes().into_iter().for_each(&mut update)),
    }
    hash
}

#[tokio::test]
async fn lossless_codec_layout_and_sample_fixtures_match_geotiff_js_v3_1_0() {
    let fixtures: &[(&str, &[u64])] = &[
        (
            "planar-rgb-u8.tif",
            &[0x62aee77d48cef8bd, 0x2d7d42f4bdf36a7f, 0x28caeb0f5e7dd793],
        ),
        ("palette-1c-4b.tiff", &[0x76e9caebbbaa50b4]),
        ("palette-1c-1b.tiff", &[0x175fe6c9e17a8e2a]),
        ("12bit.cropped.tiff", &[0xabb1d0b45561ad60]),
        (
            "no_rows_per_strip.tiff",
            &[0x36c38e198c9bd129, 0xbb2dbcab8467d108, 0x1dc60ce7b00ea2d4],
        ),
        ("predictor-3-gray-f32.tif", &[0x7d7fca3568c2e7f1]),
        (
            "predictor-3-rgb-f32.tif",
            &[0x204f7da1687341a5, 0xd7804dc027f9f7f5, 0xc31c67b2bfcd1c85],
        ),
        ("random-fp16-pred2.tiff", &[0xda13f48afce7dcca]),
        ("random-fp16-pred3.tiff", &[0xda13f48afce7dcca]),
        ("random-fp16.tiff", &[0xda13f48afce7dcca]),
        (
            "int8_rgb.tif",
            &[0x23e4caae7690f757, 0x5d505d3df9f36a82, 0xf35df3d0addb03f6],
        ),
        (
            "int16_rgb.tif",
            &[0x570f63a4f5c504ea, 0xf5b281cebb5ca226, 0x08f728051ad3b13e],
        ),
        ("int16_zstd.tif", &[0x22321f4e69928bc6]),
        ("issue_69_packbits.tiff", &[0x9593768b97e5c8c8]),
        ("float32_1band_lerc_block32.tif", &[0xe717d69dd8ea1215]),
        (
            "float32_1band_lerc_deflate_block32.tif",
            &[0xe717d69dd8ea1215],
        ),
        ("float32_1band_lerc_zstd_block32.tif", &[0xe717d69dd8ea1215]),
        (
            "uint8_rgb_webp_block64_cog.tif",
            &[0x41e6c7779b1ea4b3, 0x66f7dcb0f46be13a, 0xfac4a485097a4d6f],
        ),
        (
            "uint8_rgba_webp_block64_cog.tif",
            &[
                0x41e6c7779b1ea4b3,
                0x66f7dcb0f46be13a,
                0xfac4a485097a4d6f,
                0x0d65b7532d396325,
            ],
        ),
        (
            "minisblack-2c-8b-alpha.tiff",
            &[0xf6f0532030b93362, 0x1d2eb79bef8d9633],
        ),
    ];

    for (name, expected_hashes) in fixtures {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        let dataset = from_file(path).await.unwrap();
        let result = dataset
            .image(0)
            .unwrap()
            .read_rasters(ReadRastersOptions::default())
            .await
            .unwrap();
        let ReadRasterResult::Bands(raster) = result else {
            panic!("default readRasters must return bands for {name}")
        };
        let hashes = raster.bands.iter().map(fnv1a64).collect::<Vec<_>>();
        assert_eq!(
            hashes, *expected_hashes,
            "reference raster mismatch for {name}"
        );
    }
}

#[tokio::test]
async fn packed_rgb_is_lossless_by_default_and_can_reproduce_geotiff_js_offsets() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/12bit.cropped.rgb.tiff");
    let dataset = from_file(path).await.unwrap();
    let image = dataset.image(0).unwrap();

    let ReadRasterResult::Bands(lossless) = image
        .read_rasters(ReadRastersOptions::default())
        .await
        .unwrap()
    else {
        panic!("expected band output")
    };
    let lossless_hashes = lossless.bands.iter().map(fnv1a64).collect::<Vec<_>>();
    assert_eq!(
        lossless_hashes,
        [0xabb1d0b45561ad60, 0xabb1d0b45561ad60, 0xabb1d0b45561ad60],
        "the source contains three identical 12-bit planes"
    );

    let ReadRasterResult::Bands(javascript) = image
        .read_rasters(ReadRastersOptions {
            packed_sample_mode: PackedSampleMode::GeotiffJs,
            ..ReadRastersOptions::default()
        })
        .await
        .unwrap()
    else {
        panic!("expected band output")
    };
    let javascript_hashes = javascript.bands.iter().map(fnv1a64).collect::<Vec<_>>();
    assert_eq!(
        javascript_hashes,
        [0xabb1d0b45561ad60, 0xb01d966cc9be977e, 0xb01d966cc9be977e],
        "live geotiff.js v3.1.0 oracle output"
    );
}

#[tokio::test]
async fn native_jpeg_decode_stays_close_to_lossless_and_subsampled_references() {
    async fn bands(name: &str) -> Vec<TypedArray> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        let dataset = from_file(path).await.unwrap();
        let ReadRasterResult::Bands(raster) = dataset
            .image(0)
            .unwrap()
            .read_rasters(ReadRastersOptions::default())
            .await
            .unwrap()
        else {
            panic!("expected separate bands")
        };
        raster.bands
    }

    let lossless = bands("planar-rgb-u8.tif").await;
    let jpeg = bands("tiled-jpeg-rgb-u8.tif").await;
    let differences = lossless
        .iter()
        .zip(&jpeg)
        .flat_map(|(left, right)| {
            (0..left.len()).map(move |index| (left.get_f64(index) - right.get_f64(index)).abs())
        })
        .collect::<Vec<_>>();
    let max_difference = differences.iter().copied().fold(0.0f64, f64::max);
    let mean_difference = differences.iter().sum::<f64>() / differences.len() as f64;
    assert!(
        max_difference <= 42.0 && mean_difference <= 3.0,
        "native JPEG decoder divergence: max={max_difference}, mean={mean_difference}"
    );

    async fn rgb(name: &str) -> TypedArray {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        let dataset = from_file(path).await.unwrap();
        let ReadRasterResult::Interleaved(raster) = dataset
            .image(0)
            .unwrap()
            .read_rgb(ReadRgbOptions {
                interleave: true,
                ..ReadRgbOptions::default()
            })
            .await
            .unwrap()
        else {
            panic!("expected interleaved RGB")
        };
        raster.data
    }

    // The YCbCr fixture uses 4:2:0 subsampling. The old native path panicked
    // while upsampling it. geotiff.js and zune-jpeg use independent IDCT and
    // chroma interpolation implementations, so individual edge pixels need
    // not be byte-identical; both stay equally close to the lossless source
    // (live JS mean error 3.31, native mean error 3.26).
    let lossless = rgb("planar-rgb-u8.tif").await;
    let subsampled = rgb("tiled-jpeg-ycbcr.tif").await;
    let differences = (0..lossless.len())
        .map(|index| (lossless.get_f64(index) - subsampled.get_f64(index)).abs())
        .collect::<Vec<_>>();
    let max_difference = differences.iter().copied().fold(0.0f64, f64::max);
    let mean_difference = differences.iter().sum::<f64>() / differences.len() as f64;
    assert!(
        max_difference <= 60.0 && mean_difference <= 3.5,
        "subsampled JPEG divergence: max={max_difference}, mean={mean_difference}"
    );
}

#[tokio::test]
async fn window_resampling_and_rgb_match_live_geotiff_js_3_1_0_oracle() {
    struct Case {
        name: &'static str,
        nearest_bands: &'static [u64],
        bilinear_bands: &'static [u64],
        nearest_interleaved: u64,
        bilinear_interleaved: u64,
        rgb_bands: &'static [u64],
        rgb_interleaved: u64,
    }
    let cases = [
        Case {
            name: "planar-rgb-u8.tif",
            nearest_bands: &[0x711dbf12a21753ba, 0x1b4b4f1604b8877a, 0x05f03c5a34c817d4],
            bilinear_bands: &[0x282c63b52dfcec21, 0x89d9ae1f95920f5c, 0x016650ebe091e804],
            nearest_interleaved: 0x612e23a2e6ef5e6e,
            bilinear_interleaved: 0xa36d634472a30227,
            rgb_bands: &[0x282c63b52dfcec21, 0x89d9ae1f95920f5c, 0x016650ebe091e804],
            rgb_interleaved: 0xa36d634472a30227,
        },
        Case {
            name: "palette-1c-4b.tiff",
            nearest_bands: &[0xcb146faceca36eca],
            bilinear_bands: &[0xe6a7f117d6d75473],
            nearest_interleaved: 0xcb146faceca36eca,
            bilinear_interleaved: 0xe6a7f117d6d75473,
            rgb_bands: &[0xeb959d2e5adbcc74, 0x91bd2506728f8f3d, 0xbe2daed5d4b08d6f],
            rgb_interleaved: 0xda8bd9b230a2f60a,
        },
        Case {
            name: "tiled-gray-i1.tif",
            nearest_bands: &[0xc2baeaf7b6afcf78],
            bilinear_bands: &[0xc2baeaf7b6afcf78],
            nearest_interleaved: 0xc2baeaf7b6afcf78,
            bilinear_interleaved: 0xc2baeaf7b6afcf78,
            rgb_bands: &[0x1ea323a0d23c5fb7, 0x1ea323a0d23c5fb7, 0x1ea323a0d23c5fb7],
            rgb_interleaved: 0xc0443dba934fe3bf,
        },
    ];
    let window = ImageWindow {
        x0: 2,
        y0: 3,
        x1: 19,
        y1: 21,
    };

    for case in cases {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(case.name);
        let dataset = from_file(path).await.unwrap();
        let image = dataset.image(0).unwrap();
        for (method, expected_bands, expected_interleaved) in [
            ("nearest", case.nearest_bands, case.nearest_interleaved),
            ("bilinear", case.bilinear_bands, case.bilinear_interleaved),
        ] {
            let ReadRasterResult::Bands(bands) = image
                .read_rasters(ReadRastersOptions {
                    window: Some(window),
                    width: Some(7),
                    height: Some(5),
                    resample_method: method.to_string(),
                    ..Default::default()
                })
                .await
                .unwrap()
            else {
                panic!("expected band result for {}", case.name)
            };
            assert_eq!(
                bands.bands.iter().map(fnv1a64).collect::<Vec<_>>(),
                expected_bands,
                "{method} bands differ for {}",
                case.name
            );

            let ReadRasterResult::Interleaved(raster) = image
                .read_rasters(ReadRastersOptions {
                    window: Some(window),
                    interleave: true,
                    width: Some(7),
                    height: Some(5),
                    resample_method: method.to_string(),
                    ..Default::default()
                })
                .await
                .unwrap()
            else {
                panic!("expected interleaved result for {}", case.name)
            };
            assert_eq!(fnv1a64(&raster.data), expected_interleaved);
        }

        let ReadRasterResult::Bands(rgb) = image
            .read_rgb(ReadRgbOptions {
                window: Some(window),
                width: Some(7),
                height: Some(5),
                resample_method: "bilinear".to_string(),
                enable_alpha: true,
                ..Default::default()
            })
            .await
            .unwrap()
        else {
            panic!("expected RGB bands for {}", case.name)
        };
        assert_eq!(
            rgb.bands.iter().map(fnv1a64).collect::<Vec<_>>(),
            case.rgb_bands
        );

        let ReadRasterResult::Interleaved(rgb) = image
            .read_rgb(ReadRgbOptions {
                window: Some(window),
                interleave: true,
                width: Some(7),
                height: Some(5),
                resample_method: "bilinear".to_string(),
                enable_alpha: true,
                ..Default::default()
            })
            .await
            .unwrap()
        else {
            panic!("expected interleaved RGB for {}", case.name)
        };
        assert_eq!(fnv1a64(&rgb.data), case.rgb_interleaved);
    }
}
