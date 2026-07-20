//! Assembles decoded blocks into the image/window raster corresponding to
//! geotiff.js `GeoTIFFImage._readRaster`. Tiled and striped images share the
//! lossless decoder in `block`; the loops here only select intersecting
//! blocks, copy samples, apply fill values and dispatch resampling.

use crate::block;
use crate::decode_pool::{CancellationToken, check_cancelled};
use crate::error::GeotiffError;
use crate::resample::{resample as resample_bands, resample_interleaved};
use crate::typed_array::TypedArray as OurTypedArray;
use async_tiff::ImageFileDirectory;
use async_tiff::decoder::DecoderRegistry;
use async_tiff::error::{AsyncTiffError, AsyncTiffResult};
use async_tiff::reader::{AsyncFileReader, Endianness};
use async_tiff::tags::{PlanarConfiguration, SampleFormat};
use std::sync::Arc;

/// geotiff.js `ReadRasterResult` (interleaved case) /
/// `TypedArrayWithDimensions`: one flat, band-interleaved raster plus the
/// dimensions it was read at.
#[derive(Debug)]
pub struct Raster {
    pub data: OurTypedArray,
    pub width: usize,
    pub height: usize,
    pub samples_per_pixel: usize,
}

/// geotiff.js `ReadRasterResult` (`interleave: false` case) /
/// `TypedArrayArrayWithDimensions`: one separate array per band, plus the
/// dimensions they were read at.
#[derive(Debug)]
pub struct RasterBands {
    pub bands: Vec<OurTypedArray>,
    pub width: usize,
    pub height: usize,
}

/// Selects how non-byte-aligned samples are exposed after decoding.
///
/// `Lossless` is the safe default and preserves every TIFF sample. The
/// JavaScript implementation has a historical second-read offset bug for
/// chunky packed samples (for example 12-bit RGB); `GeotiffJs` reproduces
/// that observable output when byte-for-byte migration compatibility is
/// more important than retaining those affected values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PackedSampleMode {
    #[default]
    Lossless,
    GeotiffJs,
}

#[allow(clippy::too_many_arguments)]
fn write_decoded_value(
    output: &mut OurTypedArray,
    output_index: usize,
    block: &block::DecodedBlock,
    sample: usize,
    x: usize,
    y: usize,
    endianness: Endianness,
    mode: PackedSampleMode,
    planar_bytes_per_pixel: Option<usize>,
) -> AsyncTiffResult<()> {
    match mode {
        PackedSampleMode::Lossless => {
            let source_index = y
                .checked_mul(block.width)
                .and_then(|value| value.checked_add(x))
                .ok_or_else(|| {
                    AsyncTiffError::General("Decoded sample index overflow".to_string())
                })?;
            output.copy_value_from(output_index, &block.bands[sample], source_index);
        }
        PackedSampleMode::GeotiffJs => {
            let value = block.javascript_compatible_value(
                sample,
                x,
                y,
                endianness,
                planar_bytes_per_pixel,
            )?;
            output.set_f64(output_index, value);
        }
    }
    Ok(())
}

fn javascript_planar_bytes_per_pixel(
    ifd: &ImageFileDirectory,
    samples: &[usize],
    mode: PackedSampleMode,
) -> Option<usize> {
    if mode != PackedSampleMode::GeotiffJs
        || ifd.planar_configuration() != PlanarConfiguration::Planar
    {
        return None;
    }
    // `_readRaster` mutates one outer `bytesPerPixel` variable while it
    // queues every planar sample promise. Promise callbacks run after that
    // loop and therefore all observe the last requested sample's size.
    let bits = block::sample_bits(ifd);
    samples
        .last()
        .and_then(|sample| bits.get(*sample))
        .map(|bits| usize::from(*bits).div_ceil(8))
}

fn sample_format_rank(format: SampleFormat) -> Option<u8> {
    match format {
        SampleFormat::Uint => Some(1),
        SampleFormat::Int => Some(2),
        SampleFormat::Float => Some(3),
        _ => None,
    }
}

fn interleaved_output_array(
    ifd: &ImageFileDirectory,
    len: usize,
    fill: Option<f64>,
) -> AsyncTiffResult<OurTypedArray> {
    let formats = block::sample_formats(ifd);
    let bits = block::sample_bits(ifd);
    let format = formats
        .iter()
        .copied()
        .max_by_key(|format| sample_format_rank(*format).unwrap_or(u8::MAX))
        .ok_or_else(|| AsyncTiffError::General("SampleFormat is empty".to_string()))?;
    if !matches!(
        format,
        SampleFormat::Uint | SampleFormat::Int | SampleFormat::Float
    ) {
        return Err(AsyncTiffError::General(
            "Unsupported sample format for interleaved data. Must be 1, 2, or 3.".to_string(),
        ));
    }
    let bits = bits
        .into_iter()
        .max()
        .ok_or_else(|| AsyncTiffError::General("BitsPerSample is empty".to_string()))?;
    let mut output = block::typed_array_for(format, bits, len)?;
    if let Some(fill) = fill {
        for index in 0..len {
            output.set_f64(index, fill);
        }
    }
    Ok(output)
}

fn band_output_arrays(
    ifd: &ImageFileDirectory,
    samples: &[usize],
    len: usize,
    fill: Option<&[f64]>,
) -> AsyncTiffResult<Vec<OurTypedArray>> {
    let formats = block::sample_formats(ifd);
    let bits = block::sample_bits(ifd);
    samples
        .iter()
        .enumerate()
        .map(|(output_sample, &sample)| {
            let format = formats.get(sample).copied().ok_or_else(|| {
                AsyncTiffError::General(format!("Invalid sample index '{sample}'."))
            })?;
            let bits = bits.get(sample).copied().ok_or_else(|| {
                AsyncTiffError::General(format!("Invalid sample index '{sample}'."))
            })?;
            let mut output = block::typed_array_for(format, bits, len)?;
            if let Some(fill) = fill.and_then(|values| values.get(output_sample)).copied() {
                for index in 0..len {
                    output.set_f64(index, fill);
                }
            }
            Ok(output)
        })
        .collect()
}

/// `imageWindow`: a pixel-coordinate sub-rectangle `[x0, y0, x1, y1)` of the
/// source image to read, in the source's *native* resolution (independent
/// of `width`/`height`, which resize the read-out window afterward - see
/// `resize_raster`). Signed, like the JS `number[]` it mirrors: geotiff.js
/// clamps an out-of-bounds window against the image/tile bounds rather than
/// erroring, so this allows out-of-range values through to the same effect
/// (only `x0 > x1 || y0 > y1` - `"Invalid subsets"` in the original - is
/// rejected).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageWindow {
    pub x0: i64,
    pub y0: i64,
    pub x1: i64,
    pub y1: i64,
}

impl ImageWindow {
    /// `[0, 0, this.getWidth(), this.getHeight()]` - `readRasters`/`readRGB`'s
    /// default when no `window` option is given.
    pub fn full(ifd: &ImageFileDirectory) -> Self {
        ImageWindow {
            x0: 0,
            y0: 0,
            x1: ifd.image_width() as i64,
            y1: ifd.image_height() as i64,
        }
    }

    pub fn width(&self) -> Option<usize> {
        self.x1
            .checked_sub(self.x0)
            .and_then(|value| usize::try_from(value).ok())
    }

    pub fn height(&self) -> Option<usize> {
        self.y1
            .checked_sub(self.y0)
            .and_then(|value| usize::try_from(value).ok())
    }
}

fn checked_window_dimensions(window: ImageWindow) -> AsyncTiffResult<(i64, i64, usize)> {
    if window.x0 > window.x1 || window.y0 > window.y1 {
        return Err(AsyncTiffError::General("Invalid subsets".to_string()));
    }
    let width = window
        .x1
        .checked_sub(window.x0)
        .ok_or_else(|| AsyncTiffError::General("Raster window width overflow".to_string()))?;
    let height = window
        .y1
        .checked_sub(window.y0)
        .ok_or_else(|| AsyncTiffError::General("Raster window height overflow".to_string()))?;
    let width_usize = usize::try_from(width)
        .map_err(|_| AsyncTiffError::General("Raster window width is too large".to_string()))?;
    let height_usize = usize::try_from(height)
        .map_err(|_| AsyncTiffError::General("Raster window height is too large".to_string()))?;
    let pixels = width_usize
        .checked_mul(height_usize)
        .ok_or_else(|| AsyncTiffError::General("Raster window pixel count overflow".to_string()))?;
    Ok((width, height, pixels))
}

fn checked_sample_len(pixels: usize, samples: usize) -> AsyncTiffResult<usize> {
    pixels
        .checked_mul(samples)
        .ok_or_else(|| AsyncTiffError::General("Raster output sample count overflow".to_string()))
}

/// `Math.floor(a / b)` for possibly-negative `a` (JS's `Math.floor` on a
/// division, not Rust's truncating `/`).
fn floor_div(a: i64, b: i64) -> i64 {
    a.div_euclid(b)
}

/// `Math.ceil(a / b)` without negating `i64::MIN`.
fn ceil_div(a: i64, b: i64) -> i64 {
    a.div_euclid(b) + i64::from(a.rem_euclid(b) != 0)
}

/// `GeoTIFFImage._readRaster`'s tile-selection + per-tile copy loop
/// (`geotiffimage.js:489-583`), generalized over an arbitrary `window` and
/// `samples` selection - `read_rasters_interleaved`/`read_rasters_interleaved_samples`
/// are both the full-image case of this with `window = ImageWindow::full(ifd)`.
/// Only the tiles that actually intersect `window` are fetched, matching
/// the original's `minXTile..maxXTile`/`minYTile..maxYTile` bounds (a real
/// optimization for small windows into large images, not just a correctness
/// detail).
#[allow(clippy::too_many_arguments)]
pub async fn read_rasters_interleaved_window(
    ifd: &ImageFileDirectory,
    reader: &dyn AsyncFileReader,
    registry: Arc<DecoderRegistry>,
    samples: &[usize],
    window: ImageWindow,
    endianness: Endianness,
    cancellation: Option<&CancellationToken>,
) -> AsyncTiffResult<Raster> {
    read_rasters_interleaved_window_with_fill(
        ifd,
        reader,
        registry,
        samples,
        window,
        endianness,
        cancellation,
        None,
    )
    .await
}

/// `read_rasters_interleaved_window` with geotiff.js's scalar `fillValue`
/// applied before intersecting source pixels are copied into the output.
#[allow(clippy::too_many_arguments)]
pub async fn read_rasters_interleaved_window_with_fill(
    ifd: &ImageFileDirectory,
    reader: &dyn AsyncFileReader,
    registry: Arc<DecoderRegistry>,
    samples: &[usize],
    window: ImageWindow,
    endianness: Endianness,
    cancellation: Option<&CancellationToken>,
    fill: Option<f64>,
) -> AsyncTiffResult<Raster> {
    read_rasters_interleaved_window_with_fill_and_cache(
        ifd,
        reader,
        registry,
        samples,
        window,
        endianness,
        cancellation,
        fill,
        None,
        PackedSampleMode::Lossless,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn read_rasters_interleaved_window_with_fill_and_cache(
    ifd: &ImageFileDirectory,
    reader: &dyn AsyncFileReader,
    registry: Arc<DecoderRegistry>,
    samples: &[usize],
    window: ImageWindow,
    endianness: Endianness,
    cancellation: Option<&CancellationToken>,
    fill: Option<f64>,
    cache: Option<&block::DecodedBlockCache>,
    packed_sample_mode: PackedSampleMode,
) -> AsyncTiffResult<Raster> {
    let dimensions = checked_window_dimensions(window)?;
    let javascript_planar_bytes_per_pixel =
        javascript_planar_bytes_per_pixel(ifd, samples, packed_sample_mode);

    if ifd.tile_count().is_none() {
        return read_rasters_interleaved_window_striped(
            ifd,
            reader,
            registry,
            samples,
            window,
            endianness,
            cancellation,
            fill,
            cache,
            packed_sample_mode,
        )
        .await;
    }

    let image_width = ifd.image_width() as i64;
    let image_height = ifd.image_height() as i64;
    let total_samples = ifd.samples_per_pixel() as usize;
    let out_samples = samples.len();
    let tile_width = i64::from(
        ifd.tile_width()
            .filter(|value| *value != 0)
            .ok_or_else(|| {
                AsyncTiffError::General("Tiled TIFF has no positive TileWidth".to_string())
            })?,
    );
    let tile_height = i64::from(ifd.tile_height().filter(|value| *value != 0).ok_or_else(
        || AsyncTiffError::General("Tiled TIFF has no positive TileLength".to_string()),
    )?);
    let (tile_cols, tile_rows) = ifd.tile_count().ok_or_else(|| {
        AsyncTiffError::General("Tiled TIFF is missing tile dimensions".to_string())
    })?;
    for &sample in samples {
        if sample >= total_samples {
            return Err(AsyncTiffError::General(format!(
                "Invalid sample index '{sample}'."
            )));
        }
    }

    let min_x_tile = floor_div(window.x0, tile_width).max(0);
    let max_x_tile = ceil_div(window.x1, tile_width).min(tile_cols as i64);
    let min_y_tile = floor_div(window.y0, tile_height).max(0);
    let max_y_tile = ceil_div(window.y1, tile_height).min(tile_rows as i64);

    let (window_width, window_height, window_pixels) = dimensions;

    let mut out =
        interleaved_output_array(ifd, checked_sample_len(window_pixels, out_samples)?, fill)?;

    for y_tile in min_y_tile..max_y_tile {
        for x_tile in min_x_tile..max_x_tile {
            check_cancelled(cancellation)?;
            let tile = block::fetch_tile_cached(
                cache,
                ifd,
                (x_tile as usize, y_tile as usize),
                reader,
                endianness,
                registry.clone(),
                cancellation,
            )
            .await?;

            let first_line = y_tile * tile_height;
            let first_col = x_tile * tile_width;
            let last_line = first_line + tile_height;
            let last_col = first_col + tile_width;

            let ymax = tile_height
                .min(tile_height - (last_line - window.y1))
                .min(image_height - first_line);
            let xmax = tile_width
                .min(tile_width - (last_col - window.x1))
                .min(image_width - first_col);
            let y_start = (window.y0 - first_line).max(0);
            let x_start = (window.x0 - first_col).max(0);

            let mut ty = y_start;
            while ty < ymax {
                let mut tx = x_start;
                while tx < xmax {
                    for (out_s, &src_s) in samples.iter().enumerate() {
                        let out_y = ty + first_line - window.y0;
                        let out_x = tx + first_col - window.x0;
                        let out_index = ((out_y * window_width + out_x) * out_samples as i64
                            + out_s as i64) as usize;
                        write_decoded_value(
                            &mut out,
                            out_index,
                            &tile,
                            src_s,
                            tx as usize,
                            ty as usize,
                            endianness,
                            packed_sample_mode,
                            javascript_planar_bytes_per_pixel,
                        )?;
                    }
                    tx += 1;
                }
                ty += 1;
            }
        }
    }

    Ok(Raster {
        data: out,
        width: window_width as usize,
        height: window_height as usize,
        samples_per_pixel: out_samples,
    })
}

/// Striped fallback for `read_rasters_interleaved_window`, taken whenever
/// `ifd.tile_count()` is `None`. Each strip is treated as a single,
/// full-image-width block of nominal height `RowsPerStrip`.
#[allow(clippy::too_many_arguments)]
async fn read_rasters_interleaved_window_striped(
    ifd: &ImageFileDirectory,
    reader: &dyn AsyncFileReader,
    registry: Arc<DecoderRegistry>,
    samples: &[usize],
    window: ImageWindow,
    endianness: Endianness,
    cancellation: Option<&CancellationToken>,
    fill: Option<f64>,
    cache: Option<&block::DecodedBlockCache>,
    packed_sample_mode: PackedSampleMode,
) -> AsyncTiffResult<Raster> {
    let (window_width, window_height, window_pixels) = checked_window_dimensions(window)?;
    let javascript_planar_bytes_per_pixel =
        javascript_planar_bytes_per_pixel(ifd, samples, packed_sample_mode);
    let image_width = ifd.image_width() as i64;
    let image_height = ifd.image_height() as i64;
    let total_samples = ifd.samples_per_pixel() as usize;
    let out_samples = samples.len();
    for &sample in samples {
        if sample >= total_samples {
            return Err(AsyncTiffError::General(format!(
                "Invalid sample index '{sample}'."
            )));
        }
    }
    let rows_per_strip = ifd.rows_per_strip().map(i64::from).unwrap_or(image_height);
    if rows_per_strip <= 0 {
        return Err(AsyncTiffError::General(
            "RowsPerStrip must be greater than zero".to_string(),
        ));
    }
    let total_strips = (image_height + rows_per_strip - 1) / rows_per_strip;

    let min_strip = floor_div(window.y0, rows_per_strip).max(0);
    let max_strip = ceil_div(window.y1, rows_per_strip).min(total_strips);

    let mut out =
        interleaved_output_array(ifd, checked_sample_len(window_pixels, out_samples)?, fill)?;

    for strip_idx in min_strip..max_strip {
        check_cancelled(cancellation)?;
        let decoded = block::fetch_strip_cached(
            cache,
            ifd,
            strip_idx as usize,
            reader,
            endianness,
            registry.clone(),
            cancellation,
        )
        .await?;

        let first_line = strip_idx * rows_per_strip;
        let first_col = 0i64;
        let last_line = first_line + rows_per_strip;
        let last_col = image_width;

        let ymax = rows_per_strip
            .min(rows_per_strip - (last_line - window.y1))
            .min(image_height - first_line);
        let xmax = image_width
            .min(image_width - (last_col - window.x1))
            .min(image_width - first_col);
        let y_start = (window.y0 - first_line).max(0);
        let x_start = (window.x0 - first_col).max(0);

        let mut ty = y_start;
        while ty < ymax {
            let mut tx = x_start;
            while tx < xmax {
                for (out_s, &src_s) in samples.iter().enumerate() {
                    let out_y = ty + first_line - window.y0;
                    let out_x = tx + first_col - window.x0;
                    let out_index = ((out_y * window_width + out_x) * out_samples as i64
                        + out_s as i64) as usize;
                    write_decoded_value(
                        &mut out,
                        out_index,
                        &decoded,
                        src_s,
                        tx as usize,
                        ty as usize,
                        endianness,
                        packed_sample_mode,
                        javascript_planar_bytes_per_pixel,
                    )?;
                }
                tx += 1;
            }
            ty += 1;
        }
    }

    Ok(Raster {
        data: out,
        width: window_width as usize,
        height: window_height as usize,
        samples_per_pixel: out_samples,
    })
}

/// `GeoTIFFImage.readRasters({ interleave: false })`'s tile-selection +
/// per-tile copy loop - the same algorithm as `read_rasters_interleaved_window`
/// (`geotiffimage.js:489-583`'s single `interleave` boolean branch, taken
/// the other way), producing one separate array per band instead of one
/// interleaved array. Each output band keeps the concrete type selected by
/// that sample's own SampleFormat/BitsPerSample pair.
#[allow(clippy::too_many_arguments)]
pub async fn read_rasters_window(
    ifd: &ImageFileDirectory,
    reader: &dyn AsyncFileReader,
    registry: Arc<DecoderRegistry>,
    samples: &[usize],
    window: ImageWindow,
    endianness: Endianness,
    cancellation: Option<&CancellationToken>,
) -> AsyncTiffResult<RasterBands> {
    read_rasters_window_with_fill(
        ifd,
        reader,
        registry,
        samples,
        window,
        endianness,
        cancellation,
        None,
    )
    .await
}

/// `read_rasters_window` with one prefill value per requested output band.
#[allow(clippy::too_many_arguments)]
pub async fn read_rasters_window_with_fill(
    ifd: &ImageFileDirectory,
    reader: &dyn AsyncFileReader,
    registry: Arc<DecoderRegistry>,
    samples: &[usize],
    window: ImageWindow,
    endianness: Endianness,
    cancellation: Option<&CancellationToken>,
    fill: Option<&[f64]>,
) -> AsyncTiffResult<RasterBands> {
    read_rasters_window_with_fill_and_cache(
        ifd,
        reader,
        registry,
        samples,
        window,
        endianness,
        cancellation,
        fill,
        None,
        PackedSampleMode::Lossless,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn read_rasters_window_with_fill_and_cache(
    ifd: &ImageFileDirectory,
    reader: &dyn AsyncFileReader,
    registry: Arc<DecoderRegistry>,
    samples: &[usize],
    window: ImageWindow,
    endianness: Endianness,
    cancellation: Option<&CancellationToken>,
    fill: Option<&[f64]>,
    cache: Option<&block::DecodedBlockCache>,
    packed_sample_mode: PackedSampleMode,
) -> AsyncTiffResult<RasterBands> {
    let dimensions = checked_window_dimensions(window)?;
    let javascript_planar_bytes_per_pixel =
        javascript_planar_bytes_per_pixel(ifd, samples, packed_sample_mode);

    if ifd.tile_count().is_none() {
        return read_rasters_window_striped(
            ifd,
            reader,
            registry,
            samples,
            window,
            endianness,
            cancellation,
            fill,
            cache,
            packed_sample_mode,
        )
        .await;
    }

    let image_width = ifd.image_width() as i64;
    let image_height = ifd.image_height() as i64;
    let total_samples = ifd.samples_per_pixel() as usize;
    for &sample in samples {
        if sample >= total_samples {
            return Err(AsyncTiffError::General(format!(
                "Invalid sample index '{sample}'."
            )));
        }
    }
    let tile_width = i64::from(
        ifd.tile_width()
            .filter(|value| *value != 0)
            .ok_or_else(|| {
                AsyncTiffError::General("Tiled TIFF has no positive TileWidth".to_string())
            })?,
    );
    let tile_height = i64::from(ifd.tile_height().filter(|value| *value != 0).ok_or_else(
        || AsyncTiffError::General("Tiled TIFF has no positive TileLength".to_string()),
    )?);
    let (tile_cols, tile_rows) = ifd.tile_count().ok_or_else(|| {
        AsyncTiffError::General("Tiled TIFF is missing tile dimensions".to_string())
    })?;

    let min_x_tile = floor_div(window.x0, tile_width).max(0);
    let max_x_tile = ceil_div(window.x1, tile_width).min(tile_cols as i64);
    let min_y_tile = floor_div(window.y0, tile_height).max(0);
    let max_y_tile = ceil_div(window.y1, tile_height).min(tile_rows as i64);

    let (window_width, window_height, window_pixels) = dimensions;

    let mut bands = band_output_arrays(ifd, samples, window_pixels, fill)?;

    for y_tile in min_y_tile..max_y_tile {
        for x_tile in min_x_tile..max_x_tile {
            check_cancelled(cancellation)?;
            let tile = block::fetch_tile_cached(
                cache,
                ifd,
                (x_tile as usize, y_tile as usize),
                reader,
                endianness,
                registry.clone(),
                cancellation,
            )
            .await?;

            let first_line = y_tile * tile_height;
            let first_col = x_tile * tile_width;
            let last_line = first_line + tile_height;
            let last_col = first_col + tile_width;

            let ymax = tile_height
                .min(tile_height - (last_line - window.y1))
                .min(image_height - first_line);
            let xmax = tile_width
                .min(tile_width - (last_col - window.x1))
                .min(image_width - first_col);
            let y_start = (window.y0 - first_line).max(0);
            let x_start = (window.x0 - first_col).max(0);

            let mut ty = y_start;
            while ty < ymax {
                let mut tx = x_start;
                while tx < xmax {
                    let out_y = ty + first_line - window.y0;
                    let out_x = tx + first_col - window.x0;
                    let out_index = (out_y * window_width + out_x) as usize;
                    for (out_s, &src_s) in samples.iter().enumerate() {
                        write_decoded_value(
                            &mut bands[out_s],
                            out_index,
                            &tile,
                            src_s,
                            tx as usize,
                            ty as usize,
                            endianness,
                            packed_sample_mode,
                            javascript_planar_bytes_per_pixel,
                        )?;
                    }
                    tx += 1;
                }
                ty += 1;
            }
        }
    }

    Ok(RasterBands {
        bands,
        width: window_width as usize,
        height: window_height as usize,
    })
}

/// Striped fallback for `read_rasters_window`, taken whenever
/// `ifd.tile_count()` is `None` - see `read_rasters_interleaved_window_striped`'s
/// docs, this is the same idea for the per-band output shape.
#[allow(clippy::too_many_arguments)]
async fn read_rasters_window_striped(
    ifd: &ImageFileDirectory,
    reader: &dyn AsyncFileReader,
    registry: Arc<DecoderRegistry>,
    samples: &[usize],
    window: ImageWindow,
    endianness: Endianness,
    cancellation: Option<&CancellationToken>,
    fill: Option<&[f64]>,
    cache: Option<&block::DecodedBlockCache>,
    packed_sample_mode: PackedSampleMode,
) -> AsyncTiffResult<RasterBands> {
    let (window_width, window_height, window_pixels) = checked_window_dimensions(window)?;
    let javascript_planar_bytes_per_pixel =
        javascript_planar_bytes_per_pixel(ifd, samples, packed_sample_mode);
    let image_width = ifd.image_width() as i64;
    let image_height = ifd.image_height() as i64;
    let total_samples = ifd.samples_per_pixel() as usize;
    for &sample in samples {
        if sample >= total_samples {
            return Err(AsyncTiffError::General(format!(
                "Invalid sample index '{sample}'."
            )));
        }
    }
    let rows_per_strip = ifd.rows_per_strip().map(i64::from).unwrap_or(image_height);
    if rows_per_strip <= 0 {
        return Err(AsyncTiffError::General(
            "RowsPerStrip must be greater than zero".to_string(),
        ));
    }
    let total_strips = (image_height + rows_per_strip - 1) / rows_per_strip;

    let min_strip = floor_div(window.y0, rows_per_strip).max(0);
    let max_strip = ceil_div(window.y1, rows_per_strip).min(total_strips);

    let mut bands = band_output_arrays(ifd, samples, window_pixels, fill)?;

    for strip_idx in min_strip..max_strip {
        check_cancelled(cancellation)?;
        let decoded = block::fetch_strip_cached(
            cache,
            ifd,
            strip_idx as usize,
            reader,
            endianness,
            registry.clone(),
            cancellation,
        )
        .await?;

        let first_line = strip_idx * rows_per_strip;
        let first_col = 0i64;
        let last_line = first_line + rows_per_strip;
        let last_col = image_width;

        let ymax = rows_per_strip
            .min(rows_per_strip - (last_line - window.y1))
            .min(image_height - first_line);
        let xmax = image_width
            .min(image_width - (last_col - window.x1))
            .min(image_width - first_col);
        let y_start = (window.y0 - first_line).max(0);
        let x_start = (window.x0 - first_col).max(0);

        let mut ty = y_start;
        while ty < ymax {
            let mut tx = x_start;
            while tx < xmax {
                let out_y = ty + first_line - window.y0;
                let out_x = tx + first_col - window.x0;
                let out_index = (out_y * window_width + out_x) as usize;
                for (out_s, &src_s) in samples.iter().enumerate() {
                    write_decoded_value(
                        &mut bands[out_s],
                        out_index,
                        &decoded,
                        src_s,
                        tx as usize,
                        ty as usize,
                        endianness,
                        packed_sample_mode,
                        javascript_planar_bytes_per_pixel,
                    )?;
                }
                tx += 1;
            }
            ty += 1;
        }
    }

    Ok(RasterBands {
        bands,
        width: window_width as usize,
        height: window_height as usize,
    })
}

/// `GeoTIFFImage.readRasters({ interleave: false })`'s default: full
/// image, every band, one array per band.
pub async fn read_rasters(
    ifd: &ImageFileDirectory,
    reader: &dyn AsyncFileReader,
    registry: Arc<DecoderRegistry>,
    endianness: Endianness,
    cancellation: Option<&CancellationToken>,
) -> AsyncTiffResult<RasterBands> {
    let all_samples: Vec<usize> = (0..ifd.samples_per_pixel() as usize).collect();
    read_rasters_window(
        ifd,
        reader,
        registry,
        &all_samples,
        ImageWindow::full(ifd),
        endianness,
        cancellation,
    )
    .await
}

/// Resamples each band independently to a requested output size, mirroring
/// `resize_raster` but for the per-band (`interleave: false`) shape -
/// wires `resample.rs`'s `resample` (not `resample_interleaved`).
pub fn resize_raster_bands(
    raster: RasterBands,
    out_width: Option<usize>,
    out_height: Option<usize>,
    resample_method: &str,
) -> Result<RasterBands, GeotiffError> {
    let target_width = out_width
        .filter(|value| *value != 0)
        .unwrap_or(raster.width);
    let target_height = out_height
        .filter(|value| *value != 0)
        .unwrap_or(raster.height);

    if target_width == raster.width && target_height == raster.height {
        return Ok(raster);
    }

    let resampled = resample_bands(
        &raster.bands,
        raster.width,
        raster.height,
        target_width,
        target_height,
        resample_method,
    )?;
    Ok(RasterBands {
        bands: resampled,
        width: target_width,
        height: target_height,
    })
}

/// `GeoTIFFImage.readRasters({ interleave: false })` combined:
/// fetch+stitch every band over `window` (`None` = full image), then
/// resize.
#[allow(clippy::too_many_arguments)]
pub async fn read_rasters_resized(
    ifd: &ImageFileDirectory,
    reader: &dyn AsyncFileReader,
    registry: Arc<DecoderRegistry>,
    window: Option<ImageWindow>,
    out_width: Option<usize>,
    out_height: Option<usize>,
    resample_method: &str,
    endianness: Endianness,
    cancellation: Option<&CancellationToken>,
) -> AsyncTiffResult<RasterBands> {
    let window = window.unwrap_or_else(|| ImageWindow::full(ifd));
    let all_samples: Vec<usize> = (0..ifd.samples_per_pixel() as usize).collect();
    let raster = read_rasters_window(
        ifd,
        reader,
        registry,
        &all_samples,
        window,
        endianness,
        cancellation,
    )
    .await?;
    resize_raster_bands(raster, out_width, out_height, resample_method)
        .map_err(|e| AsyncTiffError::General(e.to_string()))
}

/// `GeoTIFFImage.readRasters({ samples })`: reads only the given sample
/// (band) indices, over the full image, densely packed into the output in
/// the order given - this is what lets `readrgb.rs` ask for e.g. just
/// `[0, 1, 2]` out of a 4-band image instead of every band.
pub async fn read_rasters_interleaved_samples(
    ifd: &ImageFileDirectory,
    reader: &dyn AsyncFileReader,
    registry: Arc<DecoderRegistry>,
    samples: &[usize],
    endianness: Endianness,
    cancellation: Option<&CancellationToken>,
) -> AsyncTiffResult<Raster> {
    read_rasters_interleaved_window(
        ifd,
        reader,
        registry,
        samples,
        ImageWindow::full(ifd),
        endianness,
        cancellation,
    )
    .await
}

/// `GeoTIFFImage.readRasters({})`'s default case: full image, native
/// resolution, interleaved, every band.
pub async fn read_rasters_interleaved(
    ifd: &ImageFileDirectory,
    reader: &dyn AsyncFileReader,
    registry: Arc<DecoderRegistry>,
    endianness: Endianness,
    cancellation: Option<&CancellationToken>,
) -> AsyncTiffResult<Raster> {
    let all_samples: Vec<usize> = (0..ifd.samples_per_pixel() as usize).collect();
    read_rasters_interleaved_samples(
        ifd,
        reader,
        registry,
        &all_samples,
        endianness,
        cancellation,
    )
    .await
}

/// `GeoTIFFImage.readRasters({ width, height, resampleMethod })`'s tail
/// logic (`geotiffimage.js:586-611`): resample a raster to a requested
/// output size if it differs from its current size. `out_width`/
/// `out_height` of `None` means "keep the current size on that axis"
/// (matches JS's `width`/`height` both being optional). Pure/no I/O, so
/// `readrgb.rs` can reuse it too.
pub fn resize_raster(
    raster: Raster,
    out_width: Option<usize>,
    out_height: Option<usize>,
    resample_method: &str,
) -> Result<Raster, GeotiffError> {
    let target_width = out_width
        .filter(|value| *value != 0)
        .unwrap_or(raster.width);
    let target_height = out_height
        .filter(|value| *value != 0)
        .unwrap_or(raster.height);

    if target_width == raster.width && target_height == raster.height {
        return Ok(raster);
    }

    let resampled = resample_interleaved(
        &raster.data,
        raster.width,
        raster.height,
        target_width,
        target_height,
        raster.samples_per_pixel,
        resample_method,
    )?;

    Ok(Raster {
        data: resampled,
        width: target_width,
        height: target_height,
        samples_per_pixel: raster.samples_per_pixel,
    })
}

/// `GeoTIFFImage.readRasters({ window, width, height, resampleMethod })`
/// combined: fetch+stitch every band over `window` (`None` = full image),
/// then resize. This is the full `readRasters` default-samples path.
#[allow(clippy::too_many_arguments)]
pub async fn read_rasters_interleaved_resized(
    ifd: &ImageFileDirectory,
    reader: &dyn AsyncFileReader,
    registry: Arc<DecoderRegistry>,
    window: Option<ImageWindow>,
    out_width: Option<usize>,
    out_height: Option<usize>,
    resample_method: &str,
    endianness: Endianness,
    cancellation: Option<&CancellationToken>,
) -> AsyncTiffResult<Raster> {
    let window = window.unwrap_or_else(|| ImageWindow::full(ifd));
    let all_samples: Vec<usize> = (0..ifd.samples_per_pixel() as usize).collect();
    let raster = read_rasters_interleaved_window(
        ifd,
        reader,
        registry,
        &all_samples,
        window,
        endianness,
        cancellation,
    )
    .await?;
    resize_raster(raster, out_width, out_height, resample_method)
        .map_err(|e| AsyncTiffError::General(e.to_string()))
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

    fn fixture_reader() -> Arc<dyn AsyncFileReader> {
        let data = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/tiled-gray-i1.tif"
        ))
        .unwrap();
        Arc::new(BytesReader(Bytes::from(data)))
    }

    #[tokio::test]
    async fn stitches_all_tiles_into_a_full_image_raster() {
        let reader = fixture_reader();
        let tiff = open_tiff(reader.clone()).await.unwrap();
        let ifd = &tiff.ifds()[0];
        let registry = Arc::new(build_decoder_registry());

        let raster = read_rasters_interleaved(
            ifd,
            reader.as_ref(),
            registry,
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();

        // image is 37x51, 1 band -> exactly that many samples, not the padded 48x64 (3x4 tiles of 16x16)
        assert_eq!(raster.width, 37);
        assert_eq!(raster.height, 51);
        assert_eq!(raster.samples_per_pixel, 1);
        match &raster.data {
            OurTypedArray::Uint8(pixels) => assert_eq!(pixels.len(), 37 * 51),
            other => panic!("expected Uint8 (from 1-bit Bool samples), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resized_read_matches_the_requested_output_dimensions() {
        let reader = fixture_reader();
        let tiff = open_tiff(reader.clone()).await.unwrap();
        let ifd = &tiff.ifds()[0];
        let registry = Arc::new(build_decoder_registry());

        let raster = read_rasters_interleaved_resized(
            ifd,
            reader.as_ref(),
            registry,
            None,
            Some(10),
            Some(20),
            "nearest",
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();

        assert_eq!(raster.width, 10);
        assert_eq!(raster.height, 20);
        match &raster.data {
            OurTypedArray::Uint8(pixels) => assert_eq!(pixels.len(), 10 * 20),
            other => panic!("expected Uint8, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resized_read_with_native_dimensions_skips_resampling() {
        let reader = fixture_reader();
        let tiff = open_tiff(reader.clone()).await.unwrap();
        let ifd = &tiff.ifds()[0];
        let registry = Arc::new(build_decoder_registry());

        // None/None (or explicitly the native size) should return the same
        // data as the unresized read, not run it through resampling.
        let native = read_rasters_interleaved(
            ifd,
            reader.as_ref(),
            registry.clone(),
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();
        let resized = read_rasters_interleaved_resized(
            ifd,
            reader.as_ref(),
            registry,
            None,
            None,
            None,
            "nearest",
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();

        assert_eq!(resized.width, native.width);
        assert_eq!(resized.height, native.height);
        assert_eq!(resized.data, native.data);
    }

    #[tokio::test]
    async fn zero_resize_options_match_javascript_truthiness() {
        let reader = fixture_reader();
        let tiff = open_tiff(reader.clone()).await.unwrap();
        let ifd = &tiff.ifds()[0];
        let registry = Arc::new(build_decoder_registry());

        let native = read_rasters_interleaved(
            ifd,
            reader.as_ref(),
            registry.clone(),
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();
        let resized = read_rasters_interleaved_resized(
            ifd,
            reader.as_ref(),
            registry,
            None,
            Some(0),
            Some(0),
            "nearest",
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();
        assert_eq!(resized.width, native.width);
        assert_eq!(resized.height, native.height);
        assert_eq!(resized.data, native.data);
    }

    #[tokio::test]
    async fn overflowing_window_is_a_normal_error_and_extreme_empty_window_is_safe() {
        let reader = fixture_reader();
        let tiff = open_tiff(reader.clone()).await.unwrap();
        let ifd = &tiff.ifds()[0];
        let registry = Arc::new(build_decoder_registry());

        let overflow = read_rasters_interleaved_window(
            ifd,
            reader.as_ref(),
            registry.clone(),
            &[0],
            ImageWindow {
                x0: i64::MIN,
                y0: 0,
                x1: i64::MAX,
                y1: 1,
            },
            Endianness::LittleEndian,
            None,
        )
        .await;
        assert!(overflow.is_err());

        let empty = read_rasters_interleaved_window(
            ifd,
            reader.as_ref(),
            registry,
            &[0],
            ImageWindow {
                x0: i64::MIN,
                y0: 0,
                x1: i64::MIN,
                y1: 0,
            },
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();
        assert_eq!((empty.width, empty.height, empty.data.len()), (0, 0, 0));
    }

    #[tokio::test]
    async fn full_window_matches_the_unwindowed_read() {
        let reader = fixture_reader();
        let tiff = open_tiff(reader.clone()).await.unwrap();
        let ifd = &tiff.ifds()[0];
        let registry = Arc::new(build_decoder_registry());

        let native = read_rasters_interleaved(
            ifd,
            reader.as_ref(),
            registry.clone(),
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();
        let windowed = read_rasters_interleaved_window(
            ifd,
            reader.as_ref(),
            registry,
            &[0],
            ImageWindow::full(ifd),
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();

        assert_eq!(windowed.width, native.width);
        assert_eq!(windowed.height, native.height);
        assert_eq!(windowed.data, native.data);
    }

    #[tokio::test]
    async fn a_window_spanning_a_tile_boundary_matches_the_corresponding_slice_of_the_full_image() {
        // Tiles are 16x16; this window (x:[10,25), y:[10,25)) straddles all
        // four tiles in the top-left 2x2 block of the 3x4 tile grid, so it
        // genuinely exercises cross-tile stitching within a window, not
        // just a crop of a single tile.
        let reader = fixture_reader();
        let tiff = open_tiff(reader.clone()).await.unwrap();
        let ifd = &tiff.ifds()[0];
        let registry = Arc::new(build_decoder_registry());

        let full = read_rasters_interleaved(
            ifd,
            reader.as_ref(),
            registry.clone(),
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();
        let OurTypedArray::Uint8(full_pixels) = &full.data else {
            panic!("expected Uint8")
        };

        let window = ImageWindow {
            x0: 10,
            y0: 10,
            x1: 25,
            y1: 25,
        };
        let windowed = read_rasters_interleaved_window(
            ifd,
            reader.as_ref(),
            registry,
            &[0],
            window,
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();
        assert_eq!(windowed.width, 15);
        assert_eq!(windowed.height, 15);
        let OurTypedArray::Uint8(windowed_pixels) = &windowed.data else {
            panic!("expected Uint8")
        };

        for y in 0..15usize {
            for x in 0..15usize {
                let expected = full_pixels[(y + 10) * full.width + (x + 10)];
                let actual = windowed_pixels[y * 15 + x];
                assert_eq!(actual, expected, "mismatch at window-relative ({x}, {y})");
            }
        }
    }

    #[tokio::test]
    async fn a_window_within_a_single_tile_matches_the_full_image_slice() {
        let reader = fixture_reader();
        let tiff = open_tiff(reader.clone()).await.unwrap();
        let ifd = &tiff.ifds()[0];
        let registry = Arc::new(build_decoder_registry());

        let full = read_rasters_interleaved(
            ifd,
            reader.as_ref(),
            registry.clone(),
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();
        let OurTypedArray::Uint8(full_pixels) = &full.data else {
            panic!("expected Uint8")
        };

        let window = ImageWindow {
            x0: 2,
            y0: 2,
            x1: 8,
            y1: 8,
        };
        let windowed = read_rasters_interleaved_window(
            ifd,
            reader.as_ref(),
            registry,
            &[0],
            window,
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();
        assert_eq!((windowed.width, windowed.height), (6, 6));
        let OurTypedArray::Uint8(windowed_pixels) = &windowed.data else {
            panic!("expected Uint8")
        };

        for y in 0..6usize {
            for x in 0..6usize {
                assert_eq!(
                    windowed_pixels[y * 6 + x],
                    full_pixels[(y + 2) * full.width + (x + 2)]
                );
            }
        }
    }

    #[tokio::test]
    async fn invalid_window_is_rejected() {
        let reader = fixture_reader();
        let tiff = open_tiff(reader.clone()).await.unwrap();
        let ifd = &tiff.ifds()[0];
        let registry = Arc::new(build_decoder_registry());

        let bad_window = ImageWindow {
            x0: 10,
            y0: 0,
            x1: 5,
            y1: 10,
        };
        let err = read_rasters_interleaved_window(
            ifd,
            reader.as_ref(),
            registry,
            &[0],
            bad_window,
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("Invalid subsets"));
    }

    #[tokio::test]
    async fn per_band_read_matches_the_interleaved_read_for_a_single_band_image() {
        let reader = fixture_reader();
        let tiff = open_tiff(reader.clone()).await.unwrap();
        let ifd = &tiff.ifds()[0];
        let registry = Arc::new(build_decoder_registry());

        let interleaved = read_rasters_interleaved(
            ifd,
            reader.as_ref(),
            registry.clone(),
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();
        let bands = read_rasters(
            ifd,
            reader.as_ref(),
            registry,
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();

        assert_eq!(bands.width, interleaved.width);
        assert_eq!(bands.height, interleaved.height);
        assert_eq!(bands.bands.len(), 1);
        // single band -> per-band output and interleaved output hold identical data
        assert_eq!(bands.bands[0], interleaved.data);
    }

    #[tokio::test]
    async fn per_band_window_matches_the_interleaved_window_across_a_tile_boundary() {
        let reader = fixture_reader();
        let tiff = open_tiff(reader.clone()).await.unwrap();
        let ifd = &tiff.ifds()[0];
        let registry = Arc::new(build_decoder_registry());

        let window = ImageWindow {
            x0: 10,
            y0: 10,
            x1: 25,
            y1: 25,
        };
        let interleaved = read_rasters_interleaved_window(
            ifd,
            reader.as_ref(),
            registry.clone(),
            &[0],
            window,
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();
        let bands = read_rasters_window(
            ifd,
            reader.as_ref(),
            registry,
            &[0],
            window,
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();

        assert_eq!((bands.width, bands.height), (15, 15));
        assert_eq!(bands.bands[0], interleaved.data);
    }

    #[tokio::test]
    async fn per_band_resize_matches_the_interleaved_resize() {
        let reader = fixture_reader();
        let tiff = open_tiff(reader.clone()).await.unwrap();
        let ifd = &tiff.ifds()[0];
        let registry = Arc::new(build_decoder_registry());

        let interleaved = read_rasters_interleaved_resized(
            ifd,
            reader.as_ref(),
            registry.clone(),
            None,
            Some(10),
            Some(20),
            "nearest",
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();
        let bands = read_rasters_resized(
            ifd,
            reader.as_ref(),
            registry,
            None,
            Some(10),
            Some(20),
            "nearest",
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();

        assert_eq!((bands.width, bands.height), (10, 20));
        assert_eq!(bands.bands[0], interleaved.data);
    }

    fn striped_fixture_reader(name: &str) -> Arc<dyn AsyncFileReader> {
        let data = std::fs::read(format!(
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/{}"),
            name
        ))
        .unwrap();
        Arc::new(BytesReader(Bytes::from(data)))
    }

    /// `tests/fixtures/minisblack-1c-8b.tiff` is striped (157x151,
    /// RowsPerStrip=52, 3 strips) - exercises the `read_rasters_interleaved_window`
    /// -> `read_rasters_interleaved_window_striped` fallback through the
    /// real public API and confirms
    /// the full stitched image matches the fixture's independently-shipped
    /// `.pgm` reference exactly.
    #[tokio::test]
    async fn full_read_of_a_striped_fixture_matches_its_pgm_reference() {
        let reader = striped_fixture_reader("minisblack-1c-8b.tiff");
        let tiff = open_tiff(reader.clone()).await.unwrap();
        let ifd = &tiff.ifds()[0];
        assert!(
            ifd.tile_count().is_none(),
            "fixture is expected to be striped, not tiled"
        );
        let registry = Arc::new(build_decoder_registry());

        let raster = read_rasters_interleaved(
            ifd,
            reader.as_ref(),
            registry,
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();
        assert_eq!(raster.width, 157);
        assert_eq!(raster.height, 151);

        let pgm = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/minisblack-1c-8b.pgm"
        ))
        .unwrap();
        let header_end = {
            let mut newlines = 0;
            let mut idx = 0;
            for (i, &b) in pgm.iter().enumerate() {
                if b == b'\n' {
                    newlines += 1;
                    if newlines == 3 {
                        idx = i + 1;
                        break;
                    }
                }
            }
            idx
        };
        let expected = &pgm[header_end..];

        let OurTypedArray::Uint8(pixels) = &raster.data else {
            panic!("expected Uint8")
        };
        assert_eq!(pixels.as_slice(), expected);
    }

    /// A window straddling the fixture's strip boundary at y=52 (rows
    /// 40..70) must match the corresponding slice of the full read -
    /// genuinely exercises cross-strip stitching, the striped analogue of
    /// `a_window_spanning_a_tile_boundary_matches_the_corresponding_slice_of_the_full_image`.
    #[tokio::test]
    async fn a_window_spanning_a_strip_boundary_matches_the_corresponding_slice_of_the_full_image()
    {
        let reader = striped_fixture_reader("minisblack-1c-8b.tiff");
        let tiff = open_tiff(reader.clone()).await.unwrap();
        let ifd = &tiff.ifds()[0];
        let registry = Arc::new(build_decoder_registry());

        let full = read_rasters_interleaved(
            ifd,
            reader.as_ref(),
            registry.clone(),
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();
        let OurTypedArray::Uint8(full_pixels) = &full.data else {
            panic!("expected Uint8")
        };

        let window = ImageWindow {
            x0: 20,
            y0: 40,
            x1: 60,
            y1: 70,
        };
        let windowed = read_rasters_interleaved_window(
            ifd,
            reader.as_ref(),
            registry,
            &[0],
            window,
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();
        assert_eq!((windowed.width, windowed.height), (40, 30));
        let OurTypedArray::Uint8(windowed_pixels) = &windowed.data else {
            panic!("expected Uint8")
        };

        for y in 0..30usize {
            for x in 0..40usize {
                let expected = full_pixels[(y + 40) * full.width + (x + 20)];
                let actual = windowed_pixels[y * 40 + x];
                assert_eq!(actual, expected, "mismatch at window-relative ({x}, {y})");
            }
        }
    }

    #[tokio::test]
    async fn per_band_read_matches_interleaved_for_a_striped_fixture() {
        let reader = striped_fixture_reader("minisblack-1c-8b.tiff");
        let tiff = open_tiff(reader.clone()).await.unwrap();
        let ifd = &tiff.ifds()[0];
        let registry = Arc::new(build_decoder_registry());

        let interleaved = read_rasters_interleaved(
            ifd,
            reader.as_ref(),
            registry.clone(),
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();
        let bands = read_rasters(
            ifd,
            reader.as_ref(),
            registry,
            Endianness::LittleEndian,
            None,
        )
        .await
        .unwrap();

        assert_eq!(bands.width, interleaved.width);
        assert_eq!(bands.height, interleaved.height);
        assert_eq!(bands.bands[0], interleaved.data);
    }

    #[derive(Debug)]
    struct CountingReader {
        inner: BytesReader,
        fetch_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl AsyncFileReader for CountingReader {
        async fn get_bytes(&self, range: Range<u64>) -> AsyncTiffResult<Bytes> {
            self.fetch_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.get_bytes(range).await
        }
    }

    /// A token cancelled *before* the read starts must stop the tile loop
    /// before it fetches a single tile - not just fail eventually. This is
    /// what actually justifies threading `cancellation` all the way down to
    /// `read_rasters_interleaved` instead of only checking it inside
    /// `decode_pool::spawn_decode` (which alone would still let every tile
    /// in a large image get fetched and queued for decode before any of
    /// them individually noticed the cancellation).
    #[tokio::test]
    async fn a_cancelled_token_stops_the_tile_loop_before_any_fetch() {
        let data = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/tiled-gray-i1.tif"
        ))
        .unwrap();
        let tiff = open_tiff(Arc::new(BytesReader(Bytes::from(data.clone()))))
            .await
            .unwrap();
        let ifd = &tiff.ifds()[0];
        let registry = Arc::new(build_decoder_registry());

        let fetch_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let reader = CountingReader {
            inner: BytesReader(Bytes::from(data)),
            fetch_count: fetch_count.clone(),
        };

        let token = crate::decode_pool::CancellationToken::new();
        token.cancel();

        let err = read_rasters_interleaved(
            ifd,
            &reader,
            registry,
            Endianness::LittleEndian,
            Some(&token),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("cancelled"));
        assert_eq!(
            fetch_count.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "no tile bytes should be fetched once the token is already cancelled"
        );
    }
}
