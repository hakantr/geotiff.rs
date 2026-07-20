//! Port of `GeoTIFFImage.readRGB` (`geotiffimage.js:763-856`): converts any
//! supported photometric interpretation to RGB using the already-ported
//! `rgb.rs` conversion functions, or passes an already-RGB image through
//! directly (with a resize step if requested).
//!
use crate::decode_pool::CancellationToken;
use crate::error::GeotiffError;
use crate::raster::{ImageWindow, PackedSampleMode, Raster, resize_raster};
use crate::rgb;
use crate::typed_array::TypedArray;
use async_tiff::ImageFileDirectory;
use async_tiff::decoder::DecoderRegistry;
use async_tiff::error::{AsyncTiffError, AsyncTiffResult};
use async_tiff::reader::{AsyncFileReader, Endianness};
use async_tiff::tags::{ExtraSamples, PhotometricInterpretation};
use std::sync::Arc;

fn to_async_tiff_err(e: GeotiffError) -> AsyncTiffError {
    AsyncTiffError::General(e.to_string())
}

/// `GeoTIFFImage.readRGB({ window, width, height, resampleMethod, enableAlpha })`.
/// `window` of `None` means the full image (`ImageWindow::full(ifd)`),
/// matching JS's optional `window` option.
#[allow(clippy::too_many_arguments)]
pub async fn read_rgb(
    ifd: &ImageFileDirectory,
    reader: &dyn AsyncFileReader,
    registry: Arc<DecoderRegistry>,
    window: Option<ImageWindow>,
    out_width: Option<usize>,
    out_height: Option<usize>,
    resample_method: &str,
    enable_alpha: bool,
    endianness: Endianness,
    cancellation: Option<&CancellationToken>,
) -> AsyncTiffResult<Raster> {
    read_rgb_with_cache(
        ifd,
        reader,
        registry,
        window,
        out_width,
        out_height,
        resample_method,
        enable_alpha,
        PackedSampleMode::Lossless,
        endianness,
        cancellation,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn read_rgb_with_cache(
    ifd: &ImageFileDirectory,
    reader: &dyn AsyncFileReader,
    registry: Arc<DecoderRegistry>,
    window: Option<ImageWindow>,
    out_width: Option<usize>,
    out_height: Option<usize>,
    resample_method: &str,
    enable_alpha: bool,
    packed_sample_mode: PackedSampleMode,
    endianness: Endianness,
    cancellation: Option<&CancellationToken>,
    cache: Option<&crate::block::DecodedBlockCache>,
) -> AsyncTiffResult<Raster> {
    let window = window.unwrap_or_else(|| ImageWindow::full(ifd));
    let pi = ifd.photometric_interpretation();

    // Already RGB: samples [0, 1, 2] pass through unchanged (no rgb.rs
    // conversion needed) - unless `enableAlpha` and the image genuinely has
    // extra (non-unspecified) samples, in which case every band is kept
    // (`geotiffimage.js:776-796`).
    if pi == PhotometricInterpretation::RGB {
        let has_real_extra_samples = ifd
            .extra_samples()
            .is_some_and(|extra| extra.first() != Some(&ExtraSamples::Unspecified));
        let samples: Vec<usize> = if enable_alpha && has_real_extra_samples {
            (0..ifd.bits_per_sample().len()).collect()
        } else {
            vec![0, 1, 2]
        };
        let raster = crate::raster::read_rasters_interleaved_window_with_fill_and_cache(
            ifd,
            reader,
            registry,
            &samples,
            window,
            endianness,
            cancellation,
            None,
            cache,
            packed_sample_mode,
        )
        .await?;
        return resize_raster(raster, out_width, out_height, resample_method)
            .map_err(to_async_tiff_err);
    }

    let samples: Vec<usize> = match pi {
        PhotometricInterpretation::WhiteIsZero
        | PhotometricInterpretation::BlackIsZero
        | PhotometricInterpretation::RGBPalette => vec![0],
        PhotometricInterpretation::CMYK => vec![0, 1, 2, 3],
        PhotometricInterpretation::YCbCr | PhotometricInterpretation::CIELab => vec![0, 1, 2],
        _ => {
            return Err(AsyncTiffError::General(
                "Invalid or unsupported photometric interpretation.".to_string(),
            ));
        }
    };

    let raster = crate::raster::read_rasters_interleaved_window_with_fill_and_cache(
        ifd,
        reader,
        registry,
        &samples,
        window,
        endianness,
        cancellation,
        None,
        cache,
        packed_sample_mode,
    )
    .await?;
    let raster =
        resize_raster(raster, out_width, out_height, resample_method).map_err(to_async_tiff_err)?;

    let max = 2f64.powi(*ifd.bits_per_sample().first().unwrap_or(&8) as i32);

    let rgb_bytes = match pi {
        PhotometricInterpretation::WhiteIsZero => rgb::from_white_is_zero(&raster.data, max),
        PhotometricInterpretation::BlackIsZero => rgb::from_black_is_zero(&raster.data, max),
        PhotometricInterpretation::RGBPalette => {
            let colormap = ifd.colormap().ok_or_else(|| {
                AsyncTiffError::General(
                    "Palette photometric interpretation without a ColorMap tag".to_string(),
                )
            })?;
            rgb::from_palette(&raster.data, colormap)
        }
        PhotometricInterpretation::CMYK => rgb::from_cmyk(&raster.data),
        PhotometricInterpretation::YCbCr => rgb::from_y_cb_cr(&raster.data),
        PhotometricInterpretation::CIELab => rgb::from_cie_lab(&raster.data),
        // Every other case already returned via the `samples` match above.
        _ => unreachable!("photometric interpretation was already validated above"),
    };

    let data = if pi == PhotometricInterpretation::YCbCr {
        // geotiff.js `fromYCbCr()` returns a Uint8ClampedArray. Preserve the
        // observable typed-array kind as well as its clamped byte values.
        TypedArray::Uint8Clamped(rgb_bytes)
    } else {
        TypedArray::Uint8(rgb_bytes)
    };

    Ok(Raster {
        data,
        width: raster.width,
        height: raster.height,
        samples_per_pixel: 3,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compression::registry::build_decoder_registry;
    use crate::pipeline::open_tiff;
    use bytes::Bytes;
    use std::ops::Range;

    #[derive(Debug)]
    struct BytesReader(Bytes);

    #[async_trait::async_trait]
    impl AsyncFileReader for BytesReader {
        async fn get_bytes(&self, range: Range<u64>) -> AsyncTiffResult<Bytes> {
            let end = (range.end as usize).min(self.0.len());
            Ok(self.0.slice(range.start as usize..end))
        }
    }

    /// `tests/fixtures/tiled-gray-i1.tif` is `BlackIsZero`, 1 bit per
    /// sample - exercises the grayscale-to-RGB conversion path end to end
    /// against a real file.
    #[tokio::test]
    async fn converts_a_blackiszero_image_to_rgb() {
        let data = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/tiled-gray-i1.tif"
        ))
        .unwrap();
        let reader: Arc<dyn AsyncFileReader> = Arc::new(BytesReader(Bytes::from(data)));
        let tiff = open_tiff(reader.clone()).await.unwrap();
        let ifd = &tiff.ifds()[0];
        assert_eq!(
            ifd.photometric_interpretation(),
            PhotometricInterpretation::BlackIsZero
        );
        let registry = Arc::new(build_decoder_registry());

        let raster = read_rgb(
            ifd,
            reader.as_ref(),
            registry,
            None,
            None,
            None,
            "nearest",
            false,
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();

        assert_eq!(raster.width, 37);
        assert_eq!(raster.height, 51);
        assert_eq!(raster.samples_per_pixel, 3);
        match &raster.data {
            TypedArray::Uint8(pixels) => {
                assert_eq!(pixels.len(), 37 * 51 * 3);
                // BlackIsZero + 1-bit samples: max = 2^1 = 2, so a `true` (1) source
                // pixel maps to (1/2)*256 = 128 in every RGB channel (fromBlackIsZero
                // writes the same value into R, G and B), and `false` (0) maps to 0.
                assert_eq!(&pixels[0..3], &[128, 128, 128]);
            }
            other => panic!("expected Uint8, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resizes_the_rgb_output_when_requested() {
        let data = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/tiled-gray-i1.tif"
        ))
        .unwrap();
        let reader: Arc<dyn AsyncFileReader> = Arc::new(BytesReader(Bytes::from(data)));
        let tiff = open_tiff(reader.clone()).await.unwrap();
        let ifd = &tiff.ifds()[0];
        let registry = Arc::new(build_decoder_registry());

        let raster = read_rgb(
            ifd,
            reader.as_ref(),
            registry,
            None,
            Some(5),
            Some(5),
            "nearest",
            false,
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();

        assert_eq!(raster.width, 5);
        assert_eq!(raster.height, 5);
        assert_eq!(raster.samples_per_pixel, 3);
        match &raster.data {
            TypedArray::Uint8(pixels) => assert_eq!(pixels.len(), 5 * 5 * 3),
            other => panic!("expected Uint8, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_window_produces_rgb_only_for_that_sub_rectangle() {
        let data = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/tiled-gray-i1.tif"
        ))
        .unwrap();
        let reader: Arc<dyn AsyncFileReader> = Arc::new(BytesReader(Bytes::from(data)));
        let tiff = open_tiff(reader.clone()).await.unwrap();
        let ifd = &tiff.ifds()[0];
        let registry = Arc::new(build_decoder_registry());

        let window = crate::raster::ImageWindow {
            x0: 0,
            y0: 0,
            x1: 8,
            y1: 8,
        };
        let raster = read_rgb(
            ifd,
            reader.as_ref(),
            registry,
            Some(window),
            None,
            None,
            "nearest",
            false,
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            (raster.width, raster.height, raster.samples_per_pixel),
            (8, 8, 3)
        );
        // top-left corner of the fixture is solid white (BlackIsZero, true source
        // pixel -> 128 in every channel, per converts_a_blackiszero_image_to_rgb).
        match &raster.data {
            TypedArray::Uint8(pixels) => assert_eq!(&pixels[0..3], &[128, 128, 128]),
            other => panic!("expected Uint8, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ycbcr_conversion_preserves_the_js_clamped_array_kind() {
        let data = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/tiled-jpeg-ycbcr.tif"
        ))
        .unwrap();
        let reader: Arc<dyn AsyncFileReader> = Arc::new(BytesReader(Bytes::from(data)));
        let tiff = open_tiff(reader.clone()).await.unwrap();
        let ifd = &tiff.ifds()[0];
        let raster = read_rgb(
            ifd,
            reader.as_ref(),
            Arc::new(build_decoder_registry()),
            None,
            None,
            None,
            "nearest",
            false,
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();

        assert!(matches!(raster.data, TypedArray::Uint8Clamped(_)));
    }
}
