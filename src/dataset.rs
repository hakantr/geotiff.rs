//! Top-level API surface: `GeoTiffDataset`, the native replacement for
//! `GeoTIFFBase`/`GeoTIFF`/`MultiGeoTIFF`'s inheritance hierarchy. A closed
//! enum represents the same two concrete dataset shapes without dynamic
//! async dispatch.

use crate::compression::registry::build_decoder_registry;
use crate::dataslice::DataSlice;
use crate::decode_pool::{CancellationToken, check_cancelled};
use crate::error::GeotiffError;
use crate::geo::{get_bounding_box, get_origin, get_resolution};
use crate::geotiff::GeoTiffImageIndexError;
use crate::geotiffimage::{
    FillValue, GeoTiffImage, ReadRasterResult, ReadRastersOptions, ReadRgbOptions,
};
use crate::imagefiledirectory::{FileDirectory, IfdValue};
use crate::raster::{ImageWindow, PackedSampleMode, Raster, RasterBands};
use crate::source::block_cache::{BlockCache, CachedReader, DEFAULT_BLOCK_CACHE_CAPACITY_BYTES};
use crate::source::metadata_compat::prepare_metadata_with_cancellation;
use async_tiff::decoder::DecoderRegistry;
use async_tiff::error::{AsyncTiffError, AsyncTiffResult};
use async_tiff::reader::{AsyncFileReader, Endianness};
use async_tiff::tags::Tag;
use async_tiff::{ImageFileDirectory, TIFF};
use std::collections::BTreeMap;
use std::sync::Arc;

fn image_index_error(index: usize) -> AsyncTiffError {
    AsyncTiffError::External(Box::new(GeoTiffImageIndexError::new(index as u32)))
}

/// Options applied to the TIFF object itself (as opposed to transport
/// options such as HTTP headers).  `cache` has the same meaning as
/// geotiff.js's `{ cache: true }`: decoded tile/strip results are retained
/// per image and concurrent requests for a block are coalesced.
#[derive(Clone)]
pub struct GeoTiffOptions {
    pub cache: bool,
    pub compressed_cache_capacity_bytes: u64,
    pub decoder_registry: Arc<DecoderRegistry>,
    pub cancellation: Option<CancellationToken>,
}

impl Default for GeoTiffOptions {
    fn default() -> Self {
        Self {
            cache: false,
            compressed_cache_capacity_bytes: DEFAULT_BLOCK_CACHE_CAPACITY_BYTES,
            decoder_registry: Arc::new(build_decoder_registry()),
            cancellation: None,
        }
    }
}

/// `class GeoTIFF extends GeoTIFFBase`. Wraps one TIFF file, which may
/// itself contain several images (IFDs) - e.g. full-resolution + overviews.
pub struct SingleGeoTiff {
    reader: Arc<dyn AsyncFileReader>,
    tiff: TIFF,
    registry: Arc<DecoderRegistry>,
    big_tiff: bool,
    file_directories: Vec<FileDirectory>,
    decoded_caches: Vec<Option<Arc<crate::block::DecodedBlockCache>>>,
}

impl SingleGeoTiff {
    /// `GeoTIFF.fromSource`/`fromUrl`/`fromFile`/`fromArrayBuffer`: opens
    /// the file's metadata (IFDs) up front, the same way `GeoTIFF`'s
    /// constructor and initial parse do together in the original. The
    /// reader used for pixel data is wrapped in a byte-weighted `BlockCache`.
    /// Every downstream tile/strip fetch goes through `self.reader`, so this
    /// one boundary covers both tiled and striped paths.
    pub async fn open(reader: Arc<dyn AsyncFileReader>) -> AsyncTiffResult<Self> {
        Self::open_with_options(reader, GeoTiffOptions::default()).await
    }

    /// Opens a TIFF with a caller-supplied decoder registry, the native
    /// equivalent of geotiff.js `addDecoder`/custom decoder usage.
    pub async fn open_with_registry(
        reader: Arc<dyn AsyncFileReader>,
        registry: Arc<DecoderRegistry>,
    ) -> AsyncTiffResult<Self> {
        Self::open_with_options(
            reader,
            GeoTiffOptions {
                decoder_registry: registry,
                ..GeoTiffOptions::default()
            },
        )
        .await
    }

    pub async fn open_with_options(
        reader: Arc<dyn AsyncFileReader>,
        options: GeoTiffOptions,
    ) -> AsyncTiffResult<Self> {
        let prepared =
            prepare_metadata_with_cancellation(reader, options.cancellation.as_ref()).await?;
        let reader = prepared.reader;
        let big_tiff = prepared.big_tiff;
        let tiff = prepared.tiff;
        let file_directories = prepared.file_directories;
        let cached_reader: Arc<dyn AsyncFileReader> =
            if options.compressed_cache_capacity_bytes == 0 {
                reader
            } else {
                let cache = Arc::new(BlockCache::new(options.compressed_cache_capacity_bytes));
                Arc::new(CachedReader::new(reader, cache, "single-geotiff"))
            };
        let decoded_caches = (0..tiff.ifds().len())
            .map(|_| {
                options
                    .cache
                    .then(|| Arc::new(crate::block::DecodedBlockCache::new()))
            })
            .collect();
        Ok(SingleGeoTiff {
            reader: cached_reader,
            tiff,
            registry: options.decoder_registry,
            big_tiff,
            file_directories,
            decoded_caches,
        })
    }

    /// `GeoTIFF.getImageCount()`
    pub fn image_count(&self) -> usize {
        self.tiff.ifds().len()
    }

    pub fn is_big_tiff(&self) -> bool {
        self.big_tiff
    }

    /// `GeoTIFF.getSlice(offset, size)`. The JavaScript implementation uses
    /// a 1024-byte default for classic TIFF and 4048 bytes for BigTIFF.
    pub async fn get_slice(&self, offset: u64, size: Option<u64>) -> AsyncTiffResult<DataSlice> {
        let length = size.unwrap_or(if self.big_tiff { 4048 } else { 1024 });
        let end = offset.checked_add(length).ok_or_else(|| {
            AsyncTiffError::General("GeoTIFF slice byte range overflow".to_string())
        })?;
        let bytes = self.reader.get_bytes(offset..end).await?;
        Ok(DataSlice::from_bytes(
            bytes,
            offset,
            self.tiff.endianness() == Endianness::LittleEndian,
            self.big_tiff,
        ))
    }

    /// `GeoTIFF.getGhostValues()`: parses GDAL's COG structural metadata
    /// header immediately following the TIFF header.
    pub async fn ghost_values(&self) -> AsyncTiffResult<Option<BTreeMap<String, String>>> {
        let offset = if self.big_tiff { 16u64 } else { 8u64 };
        const DETECTION: &str = "GDAL_STRUCTURAL_METADATA_SIZE=";
        let heuristic_size = (DETECTION.len() + 100) as u64;
        let mut bytes = self
            .reader
            .get_bytes(offset..offset + heuristic_size)
            .await?;
        if !bytes.starts_with(DETECTION.as_bytes()) {
            return Ok(None);
        }

        let heuristic = String::from_utf8_lossy(&bytes);
        let first_line = heuristic.lines().next().unwrap_or_default();
        let declared_size = first_line
            .split_once('=')
            .and_then(|(_, value)| value.split_whitespace().next())
            .and_then(|value| value.parse::<usize>().ok())
            .ok_or_else(|| {
                AsyncTiffError::General("Invalid GDAL_STRUCTURAL_METADATA_SIZE header".to_string())
            })?;
        let metadata_size = declared_size.checked_add(first_line.len()).ok_or_else(|| {
            AsyncTiffError::General("GDAL structural metadata size overflow".to_string())
        })?;
        if metadata_size > bytes.len() {
            bytes = self
                .reader
                .get_bytes(
                    offset..offset.checked_add(metadata_size as u64).ok_or_else(|| {
                        AsyncTiffError::General(
                            "GDAL structural metadata byte range overflow".to_string(),
                        )
                    })?,
                )
                .await?;
        }

        if bytes.len() < metadata_size {
            return Err(AsyncTiffError::EndOfFile(
                metadata_size as u64,
                bytes.len() as u64,
            ));
        }

        let full = String::from_utf8_lossy(&bytes[..metadata_size]);
        let mut values = BTreeMap::new();
        for line in full.lines().filter(|line| !line.is_empty()) {
            if let Some((key, value)) = line.split_once('=') {
                values.insert(key.to_string(), value.to_string());
            }
        }
        Ok(Some(values))
    }

    /// Explicitly closes the source by consuming the dataset. Rust's RAII
    /// drops file/network/object-store handles when this returns.
    pub fn close(self) {}

    /// Access to the underlying parsed IFD, for callers that need
    /// lower-level access than `read_rasters`/`read_rgb` provide.
    pub fn ifd(&self, index: usize) -> AsyncTiffResult<&ImageFileDirectory> {
        self.tiff
            .ifds()
            .get(index)
            .ok_or_else(|| image_index_error(index))
    }

    /// Eager native counterpart of `GeoTIFF.requestIFD(index)`. JavaScript
    /// returns a promise because it parses IFDs lazily; Rust has already
    /// actualized them during `open`, so the same directory is immediately
    /// available.
    pub fn request_ifd(&self, index: usize) -> AsyncTiffResult<&FileDirectory> {
        self.file_directories
            .get(index)
            .ok_or_else(|| image_index_error(index))
    }

    /// `GeoTIFF.getImage(index)`: returns the complete image-level API view.
    pub fn image(&self, index: usize) -> AsyncTiffResult<GeoTiffImage<'_>> {
        let file_directory = self
            .file_directories
            .get(index)
            .ok_or_else(|| image_index_error(index))?;
        if let Some(value) = file_directory.get_value(284u16) {
            let configuration = match value {
                IfdValue::Unsigned(value) => *value,
                _ => {
                    return Err(AsyncTiffError::General(
                        "Invalid planar configuration.".to_string(),
                    ));
                }
            };
            if configuration != 1 && configuration != 2 {
                return Err(AsyncTiffError::General(
                    "Invalid planar configuration.".to_string(),
                ));
            }
        }
        Ok(GeoTiffImage::new(
            self.ifd(index)?,
            self.reader.as_ref(),
            self.registry.clone(),
            self.tiff.endianness(),
            file_directory,
            self.decoded_caches.get(index).and_then(Option::as_deref),
        ))
    }

    /// `GeoTIFFImage.readRasters({ window, width, height, resampleMethod })`
    /// for the image at `index` (`GeoTIFF.getImage(index)` +
    /// `.readRasters(...)` combined). Callers needing the complete
    /// image-level object can use [`Self::image`]. `window` of `None` reads
    /// the full image.
    #[allow(clippy::too_many_arguments)]
    pub async fn read_rasters(
        &self,
        index: usize,
        window: Option<ImageWindow>,
        out_width: Option<usize>,
        out_height: Option<usize>,
        resample_method: &str,
        cancellation: Option<&CancellationToken>,
    ) -> AsyncTiffResult<Raster> {
        match self
            .image(index)?
            .read_rasters(ReadRastersOptions {
                window,
                interleave: true,
                width: out_width,
                height: out_height,
                resample_method: resample_method.to_string(),
                cancellation: cancellation.cloned(),
                ..ReadRastersOptions::default()
            })
            .await?
        {
            ReadRasterResult::Interleaved(raster) => Ok(raster),
            ReadRasterResult::Bands(_) => Err(AsyncTiffError::General(
                "internal error: interleaved read returned bands".to_string(),
            )),
        }
    }

    /// `GeoTIFFImage.readRasters({ interleave: false, window, width, height, resampleMethod })`
    /// for the image at `index` - one array per band instead of one
    /// interleaved array.
    #[allow(clippy::too_many_arguments)]
    pub async fn read_rasters_bands(
        &self,
        index: usize,
        window: Option<ImageWindow>,
        out_width: Option<usize>,
        out_height: Option<usize>,
        resample_method: &str,
        cancellation: Option<&CancellationToken>,
    ) -> AsyncTiffResult<RasterBands> {
        match self
            .image(index)?
            .read_rasters(ReadRastersOptions {
                window,
                width: out_width,
                height: out_height,
                resample_method: resample_method.to_string(),
                cancellation: cancellation.cloned(),
                ..ReadRastersOptions::default()
            })
            .await?
        {
            ReadRasterResult::Bands(raster) => Ok(raster),
            ReadRasterResult::Interleaved(_) => Err(AsyncTiffError::General(
                "internal error: band read returned interleaved data".to_string(),
            )),
        }
    }

    /// `GeoTIFFImage.readRGB({ window, width, height, resampleMethod, enableAlpha })`
    /// for the image at `index`.
    #[allow(clippy::too_many_arguments)]
    pub async fn read_rgb(
        &self,
        index: usize,
        window: Option<ImageWindow>,
        out_width: Option<usize>,
        out_height: Option<usize>,
        resample_method: &str,
        enable_alpha: bool,
        cancellation: Option<&CancellationToken>,
    ) -> AsyncTiffResult<Raster> {
        match self
            .image(index)?
            .read_rgb(ReadRgbOptions {
                window,
                interleave: true,
                width: out_width,
                height: out_height,
                resample_method: resample_method.to_string(),
                enable_alpha,
                packed_sample_mode: PackedSampleMode::Lossless,
                decoder_registry: None,
                cancellation: cancellation.cloned(),
            })
            .await?
        {
            ReadRasterResult::Interleaved(raster) => Ok(raster),
            ReadRasterResult::Bands(_) => Err(AsyncTiffError::General(
                "internal error: RGB read returned bands".to_string(),
            )),
        }
    }
}

/// `class MultiGeoTIFF extends GeoTIFFBase`: coordinates a main file plus
/// zero or more overview files behind one unified image index -
/// `getImage(index)`'s file-then-relative-index walk (`geotiff.js:645-668`).
/// The original's `getImageCount`/`getImage` are `async` because geotiff.js
/// loads each file's IFDs lazily; every `SingleGeoTiff` here has already
/// eagerly parsed its full IFD chain in `SingleGeoTiff::open`, so image
/// counts are known up front and indexing stays synchronous.
///
/// `GeoTIFFBase.readRasters`'s automatic best-fit-resolution overview
/// selection lives on `GeoTiffDataset::read_rasters_best_fit` below, not
/// here - it's on the shared base class in the original, applying equally
/// to `GeoTIFF`/`SingleGeoTiff`.
pub struct MultiGeoTiff {
    files: Vec<SingleGeoTiff>,
}

impl MultiGeoTiff {
    /// `new MultiGeoTIFF(mainFile, overviewFiles)`.
    pub fn new(main_file: SingleGeoTiff, overview_files: Vec<SingleGeoTiff>) -> Self {
        let mut files = Vec::with_capacity(1 + overview_files.len());
        files.push(main_file);
        files.extend(overview_files);
        MultiGeoTiff { files }
    }

    /// `MultiGeoTIFF.getImageCount()`
    pub fn image_count(&self) -> usize {
        self.files.iter().map(SingleGeoTiff::image_count).sum()
    }

    /// Eager native counterpart of
    /// `MultiGeoTIFF.parseFileDirectoriesPerFile()`: the first directory of
    /// the main file followed by the first directory of every external
    /// overview file.
    pub fn parse_file_directories_per_file(&self) -> AsyncTiffResult<Vec<&FileDirectory>> {
        self.files.iter().map(|file| file.request_ifd(0)).collect()
    }

    /// Drops every underlying source/reader.
    pub fn close(self) {}

    /// `MultiGeoTIFF.getImage(index)`'s file-then-relative-index walk,
    /// generalized to hand back the owning file plus the index within it,
    /// so every `read_*` method below can delegate to `SingleGeoTiff`
    /// rather than duplicating its logic.
    fn locate(&self, index: usize) -> AsyncTiffResult<(&SingleGeoTiff, usize)> {
        let mut visited = 0;
        for file in &self.files {
            let count = file.image_count();
            if index < visited + count {
                return Ok((file, index - visited));
            }
            visited += count;
        }
        Err(image_index_error(index))
    }

    /// Access to the underlying parsed IFD for the image at the given
    /// global index - the `MultiGeoTiff` analogue of `SingleGeoTiff::ifd`.
    pub fn ifd(&self, index: usize) -> AsyncTiffResult<&ImageFileDirectory> {
        let (file, local_index) = self.locate(index)?;
        file.ifd(local_index)
    }

    /// `MultiGeoTIFF.getImage(index)`.
    pub fn image(&self, index: usize) -> AsyncTiffResult<GeoTiffImage<'_>> {
        let (file, local_index) = self.locate(index)?;
        file.image(local_index)
    }

    /// `GeoTIFFImage.readRasters(...)` for the image at the given global
    /// index, delegating to whichever underlying file owns it.
    #[allow(clippy::too_many_arguments)]
    pub async fn read_rasters(
        &self,
        index: usize,
        window: Option<ImageWindow>,
        out_width: Option<usize>,
        out_height: Option<usize>,
        resample_method: &str,
        cancellation: Option<&CancellationToken>,
    ) -> AsyncTiffResult<Raster> {
        let (file, local_index) = self.locate(index)?;
        file.read_rasters(
            local_index,
            window,
            out_width,
            out_height,
            resample_method,
            cancellation,
        )
        .await
    }

    /// `GeoTIFFImage.readRasters({ interleave: false, ... })` for the image
    /// at the given global index.
    #[allow(clippy::too_many_arguments)]
    pub async fn read_rasters_bands(
        &self,
        index: usize,
        window: Option<ImageWindow>,
        out_width: Option<usize>,
        out_height: Option<usize>,
        resample_method: &str,
        cancellation: Option<&CancellationToken>,
    ) -> AsyncTiffResult<RasterBands> {
        let (file, local_index) = self.locate(index)?;
        file.read_rasters_bands(
            local_index,
            window,
            out_width,
            out_height,
            resample_method,
            cancellation,
        )
        .await
    }

    /// `GeoTIFFImage.readRGB(...)` for the image at the given global index.
    #[allow(clippy::too_many_arguments)]
    pub async fn read_rgb(
        &self,
        index: usize,
        window: Option<ImageWindow>,
        out_width: Option<usize>,
        out_height: Option<usize>,
        resample_method: &str,
        enable_alpha: bool,
        cancellation: Option<&CancellationToken>,
    ) -> AsyncTiffResult<Raster> {
        let (file, local_index) = self.locate(index)?;
        file.read_rgb(
            local_index,
            window,
            out_width,
            out_height,
            resample_method,
            enable_alpha,
            cancellation,
        )
        .await
    }
}

/// `GeoTIFFBase`'s two concrete subclasses as a closed enum instead of a
/// ported class hierarchy.
pub enum GeoTiffDataset {
    Single(SingleGeoTiff),
    Multi(MultiGeoTiff),
}

impl GeoTiffDataset {
    /// `GeoTIFFBase.getImageCount()`
    pub fn image_count(&self) -> usize {
        match self {
            GeoTiffDataset::Single(g) => g.image_count(),
            GeoTiffDataset::Multi(m) => m.image_count(),
        }
    }

    /// Access to the underlying parsed IFD for the image at the given
    /// global index, regardless of which variant this is.
    pub fn ifd(&self, index: usize) -> AsyncTiffResult<&ImageFileDirectory> {
        match self {
            GeoTiffDataset::Single(g) => g.ifd(index),
            GeoTiffDataset::Multi(m) => m.ifd(index),
        }
    }

    /// `GeoTIFFBase.getImage(index)` for either single- or multi-file data.
    pub fn image(&self, index: usize) -> AsyncTiffResult<GeoTiffImage<'_>> {
        match self {
            GeoTiffDataset::Single(g) => g.image(index),
            GeoTiffDataset::Multi(m) => m.image(index),
        }
    }

    /// `GeoTIFFImage.readRasters(...)` for the image at the given global
    /// index, regardless of which variant this is - `read_rasters_best_fit`
    /// below delegates here once it has picked an image/window.
    #[allow(clippy::too_many_arguments)]
    pub async fn read_rasters(
        &self,
        index: usize,
        window: Option<ImageWindow>,
        out_width: Option<usize>,
        out_height: Option<usize>,
        resample_method: &str,
        cancellation: Option<&CancellationToken>,
    ) -> AsyncTiffResult<Raster> {
        match self {
            GeoTiffDataset::Single(g) => {
                g.read_rasters(
                    index,
                    window,
                    out_width,
                    out_height,
                    resample_method,
                    cancellation,
                )
                .await
            }
            GeoTiffDataset::Multi(m) => {
                m.read_rasters(
                    index,
                    window,
                    out_width,
                    out_height,
                    resample_method,
                    cancellation,
                )
                .await
            }
        }
    }

    /// Non-interleaved image read for either single- or multi-file data.
    #[allow(clippy::too_many_arguments)]
    pub async fn read_rasters_bands(
        &self,
        index: usize,
        window: Option<ImageWindow>,
        out_width: Option<usize>,
        out_height: Option<usize>,
        resample_method: &str,
        cancellation: Option<&CancellationToken>,
    ) -> AsyncTiffResult<RasterBands> {
        match self {
            GeoTiffDataset::Single(g) => {
                g.read_rasters_bands(
                    index,
                    window,
                    out_width,
                    out_height,
                    resample_method,
                    cancellation,
                )
                .await
            }
            GeoTiffDataset::Multi(m) => {
                m.read_rasters_bands(
                    index,
                    window,
                    out_width,
                    out_height,
                    resample_method,
                    cancellation,
                )
                .await
            }
        }
    }

    /// RGB image read for either single- or multi-file data.
    #[allow(clippy::too_many_arguments)]
    pub async fn read_rgb(
        &self,
        index: usize,
        window: Option<ImageWindow>,
        out_width: Option<usize>,
        out_height: Option<usize>,
        resample_method: &str,
        enable_alpha: bool,
        cancellation: Option<&CancellationToken>,
    ) -> AsyncTiffResult<Raster> {
        match self {
            GeoTiffDataset::Single(g) => {
                g.read_rgb(
                    index,
                    window,
                    out_width,
                    out_height,
                    resample_method,
                    enable_alpha,
                    cancellation,
                )
                .await
            }
            GeoTiffDataset::Multi(m) => {
                m.read_rgb(
                    index,
                    window,
                    out_width,
                    out_height,
                    resample_method,
                    enable_alpha,
                    cancellation,
                )
                .await
            }
        }
    }

    pub fn close(self) {
        match self {
            GeoTiffDataset::Single(dataset) => dataset.close(),
            GeoTiffDataset::Multi(dataset) => dataset.close(),
        }
    }

    /// `GeoTIFFBase.readRasters(options)` (`geotiff.js:280-378`): picks the
    /// lowest-resolution image that still meets a requested `res_x`/`res_y`
    /// (directly given, or derived from `out_width`/`out_height` against the
    /// first image's bounding box), converts a geographic `bbox` into a
    /// pixel `window` against the chosen image, then reads from it exactly
    /// like `read_rasters` would.
    ///
    /// An image is a candidate overview if it's image 0, or its
    /// `NewSubfileType` tag has bit 0 set, or its (legacy) `SubfileType` tag
    /// equals 2 - both flag "reduced-resolution version of another image"
    /// per the TIFF spec; candidates are tried smallest-width-first and the
    /// loop stops at the first one whose resolution is no coarser than
    /// requested (`geotiff.js:335-347`'s ascending-sort-then-break, not a
    /// full scan for the true best fit).
    ///
    /// Only `options.window`/`bbox` (not both), `res_x`/`res_y` (derived
    /// from `out_width`/`out_height` if unset), and `out_width`/`out_height`
    /// participate in image/window selection; `out_width`/`out_height`/
    /// `resample_method` are then forwarded unchanged to the final
    /// `read_rasters` call on the selected image, the same
    /// dual-purpose-options pattern the original uses.
    pub async fn read_rasters_best_fit(
        &self,
        options: BestFitOptions,
    ) -> AsyncTiffResult<ReadRasterResult> {
        check_cancelled(options.cancellation.as_ref())?;

        if options.window.is_some() && options.bbox.is_some() {
            return Err(to_async_tiff_err(GeotiffError::BothBboxAndWindowPassed));
        }

        let mut used_index = 0usize;
        let image_count = self.image_count();
        let img_bbox = get_bounding_box(self.ifd(0)?, false).map_err(to_async_tiff_err)?;

        let out_width = options.out_width.filter(|value| *value != 0);
        let out_height = options.out_height.filter(|value| *value != 0);
        let mut res_x = options
            .res_x
            .filter(|value| *value != 0.0 && !value.is_nan());
        let mut res_y = options
            .res_y
            .filter(|value| *value != 0.0 && !value.is_nan());
        let mut bbox = options.bbox;

        if out_width.is_some() || out_height.is_some() {
            if let Some(window) = options.window {
                let [o_x, o_y, ..] = get_origin(self.ifd(0)?).map_err(to_async_tiff_err)?;
                let [r_x, r_y, ..] =
                    get_resolution(self.ifd(0)?, None).map_err(to_async_tiff_err)?;
                bbox = Some([
                    o_x + (window.x0 as f64 * r_x),
                    o_y + (window.y0 as f64 * r_y),
                    o_x + (window.x1 as f64 * r_x),
                    o_y + (window.y1 as f64 * r_y),
                ]);
            }

            let used_bbox = bbox.unwrap_or(img_bbox);
            if let Some(width) = out_width {
                if res_x.is_some() {
                    return Err(to_async_tiff_err(GeotiffError::BothWidthAndResXPassed));
                }
                res_x = Some((used_bbox[2] - used_bbox[0]) / width as f64);
            }
            if let Some(height) = out_height {
                if res_y.is_some() {
                    return Err(to_async_tiff_err(GeotiffError::BothWidthAndResYPassed));
                }
                res_y = Some((used_bbox[3] - used_bbox[1]) / height as f64);
            }
        }

        if res_x.is_some() || res_y.is_some() {
            let mut candidates: Vec<(usize, f64, f64)> = Vec::new();
            for i in 0..image_count {
                let ifd = self.ifd(i)?;
                let is_candidate =
                    i == 0 || subfile_type(ifd) == Some(2) || new_subfile_type(ifd) & 1 == 1;
                if is_candidate {
                    candidates.push((i, ifd.image_width() as f64, ifd.image_height() as f64));
                }
            }
            candidates.sort_by(|a, b| a.1.total_cmp(&b.1));

            for (index, w, h) in candidates {
                let img_res_x = (img_bbox[2] - img_bbox[0]) / w;
                let img_res_y = (img_bbox[3] - img_bbox[1]) / h;
                used_index = index;
                if res_x.is_some_and(|r| r > img_res_x) || res_y.is_some_and(|r| r > img_res_y) {
                    break;
                }
            }
        }

        let mut window = options.window;
        if let Some(bbox) = bbox {
            if bbox.iter().any(|value| !value.is_finite()) {
                return Err(AsyncTiffError::General(
                    "Bounding box values must be finite".to_string(),
                ));
            }
            let [o_x, o_y, ..] = get_origin(self.ifd(0)?).map_err(to_async_tiff_err)?;
            let [image_res_x, image_res_y, ..] =
                get_resolution(self.ifd(used_index)?, Some(self.ifd(0)?))
                    .map_err(to_async_tiff_err)?;

            let raw = [
                js_round_to_i64((bbox[0] - o_x) / image_res_x)?,
                js_round_to_i64((bbox[1] - o_y) / image_res_y)?,
                js_round_to_i64((bbox[2] - o_x) / image_res_x)?,
                js_round_to_i64((bbox[3] - o_y) / image_res_y)?,
            ];
            window = Some(ImageWindow {
                x0: raw[0].min(raw[2]),
                y0: raw[1].min(raw[3]),
                x1: raw[0].max(raw[2]),
                y1: raw[1].max(raw[3]),
            });
        }

        self.image(used_index)?
            .read_rasters(ReadRastersOptions {
                window,
                samples: options.samples,
                interleave: options.interleave,
                width: out_width,
                height: out_height,
                resample_method: options.resample_method,
                fill_value: options.fill_value,
                packed_sample_mode: options.packed_sample_mode,
                decoder_registry: options.decoder_registry,
                cancellation: options.cancellation,
            })
            .await
    }

    /// Interleaved convenience form retained for native callers that used
    /// the earlier Rust-only API shape.
    pub async fn read_rasters_best_fit_interleaved(
        &self,
        mut options: BestFitOptions,
    ) -> AsyncTiffResult<Raster> {
        options.interleave = true;
        match self.read_rasters_best_fit(options).await? {
            ReadRasterResult::Interleaved(raster) => Ok(raster),
            ReadRasterResult::Bands(_) => Err(AsyncTiffError::General(
                "internal error: interleaved best-fit read returned bands".to_string(),
            )),
        }
    }
}

impl From<SingleGeoTiff> for GeoTiffDataset {
    fn from(value: SingleGeoTiff) -> Self {
        Self::Single(value)
    }
}

impl From<MultiGeoTiff> for GeoTiffDataset {
    fn from(value: MultiGeoTiff) -> Self {
        Self::Multi(value)
    }
}

fn js_round_to_i64(value: f64) -> AsyncTiffResult<i64> {
    if !value.is_finite() {
        return Err(AsyncTiffError::General(
            "Bounding box cannot be converted to a finite pixel window".to_string(),
        ));
    }
    // JavaScript Math.round chooses the value toward +infinity on a tie;
    // Rust's `round` chooses away from zero for negative ties.
    let rounded = (value + 0.5).floor();
    if rounded < i64::MIN as f64 || rounded > i64::MAX as f64 {
        return Err(AsyncTiffError::General(
            "Bounding box pixel window is outside the supported integer range".to_string(),
        ));
    }
    Ok(rounded as i64)
}

fn to_async_tiff_err(e: GeotiffError) -> AsyncTiffError {
    AsyncTiffError::General(e.to_string())
}

/// `image.fileDirectory.getValue('NewSubfileType')` - `async_tiff` exposes
/// this tag with a dedicated accessor already; the original treats an
/// absent tag as `0` (`(newSubfileType || 0) & 1`).
fn new_subfile_type(ifd: &ImageFileDirectory) -> u32 {
    ifd.new_subfile_type().unwrap_or(0)
}

/// `image.fileDirectory.getValue('SubfileType')` - the legacy (pre-TIFF 6.0)
/// equivalent of `NewSubfileType`. `async_tiff` has no dedicated accessor
/// for this rarely-used tag (real-world files use `NewSubfileType`), so it's
/// read from the catch-all `other_tags()` map instead.
fn subfile_type(ifd: &ImageFileDirectory) -> Option<u32> {
    ifd.other_tags()
        .get(&Tag::SubfileType)
        .cloned()
        .and_then(|v| v.into_u32().ok())
}

/// `GeoTIFFBase.readRasters(options)`'s options object, restricted to the
/// fields that participate in best-fit image/window selection plus the
/// ones forwarded unchanged to the selected image's own `read_rasters`
/// (`window`/`bbox` are mutually exclusive, like the original).
#[derive(Debug, Clone)]
pub struct BestFitOptions {
    pub window: Option<ImageWindow>,
    pub out_width: Option<usize>,
    pub out_height: Option<usize>,
    pub resample_method: String,
    pub res_x: Option<f64>,
    pub res_y: Option<f64>,
    pub bbox: Option<[f64; 4]>,
    pub samples: Vec<usize>,
    pub interleave: bool,
    pub fill_value: Option<FillValue>,
    /// Packed-sample policy forwarded to the selected main/overview image.
    pub packed_sample_mode: PackedSampleMode,
    /// Native equivalent of the `pool` option forwarded to the selected
    /// overview image's `readRasters` call.
    pub decoder_registry: Option<Arc<DecoderRegistry>>,
    pub cancellation: Option<CancellationToken>,
}

impl Default for BestFitOptions {
    fn default() -> Self {
        Self {
            window: None,
            out_width: None,
            out_height: None,
            resample_method: "nearest".to_string(),
            res_x: None,
            res_y: None,
            bbox: None,
            samples: Vec::new(),
            interleave: false,
            fill_value: None,
            packed_sample_mode: PackedSampleMode::Lossless,
            decoder_registry: None,
            cancellation: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_tiff::decoder::Decoder;
    use async_tiff::tags::{Compression, PhotometricInterpretation};
    use bytes::Bytes;
    use std::ops::Range;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    #[derive(Debug)]
    struct CountingReader {
        inner: BytesReader,
        fetch_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[derive(Debug)]
    struct CountingRawDecoder(Arc<AtomicUsize>);

    impl Decoder for CountingRawDecoder {
        fn decode_tile(
            &self,
            buffer: Bytes,
            _photometric_interpretation: PhotometricInterpretation,
            _jpeg_tables: Option<&[u8]>,
            _samples_per_pixel: u16,
            _bits_per_sample: u16,
            _lerc_parameters: Option<&[u32]>,
        ) -> AsyncTiffResult<Vec<u8>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(buffer.to_vec())
        }
    }

    #[async_trait::async_trait]
    impl AsyncFileReader for CountingReader {
        async fn get_bytes(&self, range: Range<u64>) -> AsyncTiffResult<Bytes> {
            self.fetch_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.get_bytes(range).await
        }
    }

    /// End-to-end confirmation (not just `CachedReader`'s own unit tests)
    /// that `SingleGeoTiff::open` actually wires the block cache into the
    /// real read path: reading the same tiles twice should not double the
    /// number of underlying byte fetches.
    #[tokio::test]
    async fn repeated_read_rasters_calls_reuse_cached_tile_bytes() {
        let data = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/tiled-gray-i1.tif"
        ))
        .unwrap();
        let fetch_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let reader: Arc<dyn AsyncFileReader> = Arc::new(CountingReader {
            inner: BytesReader(Bytes::from(data)),
            fetch_count: fetch_count.clone(),
        });

        let gt = SingleGeoTiff::open(reader).await.unwrap();
        let after_open = fetch_count.load(std::sync::atomic::Ordering::SeqCst);

        gt.read_rasters(0, None, None, None, "nearest", None)
            .await
            .unwrap();
        let after_first_read = fetch_count.load(std::sync::atomic::Ordering::SeqCst);
        // This tiny fixture fits in the metadata compatibility reader's
        // retained 64 KiB prefix, so the first pixel read may need no new
        // underlying fetch at all. It must never reduce the count, and the
        // repeated read below must remain a cache hit.
        assert!(after_first_read >= after_open);

        gt.read_rasters(0, None, None, None, "nearest", None)
            .await
            .unwrap();
        let after_second_read = fetch_count.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            after_second_read, after_first_read,
            "the second identical read should be served entirely from the block cache"
        );
    }

    #[tokio::test]
    async fn decoded_cache_reuses_blocks_and_custom_decoder_registry() {
        let decode_count = Arc::new(AtomicUsize::new(0));
        let mut registry = build_decoder_registry();
        registry.as_mut().insert(
            Compression::None,
            Box::new(CountingRawDecoder(decode_count.clone())),
        );
        let dataset = SingleGeoTiff::open_with_options(
            fixture_reader(),
            GeoTiffOptions {
                cache: true,
                decoder_registry: Arc::new(registry),
                ..GeoTiffOptions::default()
            },
        )
        .await
        .unwrap();

        dataset
            .read_rasters(0, None, None, None, "nearest", None)
            .await
            .unwrap();
        let first = decode_count.load(Ordering::SeqCst);
        assert_eq!(first, 12, "fixture contains twelve tiles");
        dataset
            .read_rasters(0, None, None, None, "nearest", None)
            .await
            .unwrap();
        assert_eq!(decode_count.load(Ordering::SeqCst), first);
    }

    #[tokio::test]
    async fn image_read_can_override_the_decoder_registry_per_call() {
        let dataset = SingleGeoTiff::open(fixture_reader()).await.unwrap();
        let decode_count = Arc::new(AtomicUsize::new(0));
        let mut registry = build_decoder_registry();
        registry.as_mut().insert(
            Compression::None,
            Box::new(CountingRawDecoder(decode_count.clone())),
        );

        dataset
            .image(0)
            .unwrap()
            .read_rasters(ReadRastersOptions {
                decoder_registry: Some(Arc::new(registry)),
                ..ReadRastersOptions::default()
            })
            .await
            .unwrap();

        assert_eq!(decode_count.load(Ordering::SeqCst), 12);
    }

    #[tokio::test]
    async fn direct_block_read_accepts_the_javascript_pool_or_decoder_equivalent() {
        let dataset = SingleGeoTiff::open(fixture_reader()).await.unwrap();
        let decode_count = Arc::new(AtomicUsize::new(0));
        let mut registry = build_decoder_registry();
        registry.as_mut().insert(
            Compression::None,
            Box::new(CountingRawDecoder(decode_count.clone())),
        );

        let block = dataset
            .image(0)
            .unwrap()
            .get_tile_or_strip_with_registry(0, 0, 0, Arc::new(registry), None)
            .await
            .unwrap();

        assert_eq!((block.x, block.y, block.sample), (0, 0, 0));
        assert_eq!(decode_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn opening_honors_an_already_cancelled_token_without_fetching() {
        let data = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/tiled-gray-i1.tif"
        ))
        .unwrap();
        let fetch_count = Arc::new(AtomicUsize::new(0));
        let reader: Arc<dyn AsyncFileReader> = Arc::new(CountingReader {
            inner: BytesReader(Bytes::from(data)),
            fetch_count: fetch_count.clone(),
        });
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let result = SingleGeoTiff::open_with_options(
            reader,
            GeoTiffOptions {
                cancellation: Some(cancellation),
                ..GeoTiffOptions::default()
            },
        )
        .await;
        assert!(result.is_err());
        assert_eq!(fetch_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn opens_a_file_and_reports_its_image_count() {
        let gt = SingleGeoTiff::open(fixture_reader()).await.unwrap();
        assert_eq!(gt.image_count(), 1);

        let dataset = GeoTiffDataset::Single(gt);
        assert_eq!(dataset.image_count(), 1);
    }

    #[tokio::test]
    async fn public_slice_and_request_ifd_match_the_eager_dataset() {
        let gt = SingleGeoTiff::open(fixture_reader()).await.unwrap();
        let slice = gt.get_slice(0, Some(8)).await.unwrap();
        assert_eq!(slice.buffer(), b"II*\0\x08\0\0\0");
        assert_eq!(slice.slice_offset(), 0);
        assert_eq!(slice.slice_top(), 8);
        assert!(slice.little_endian());
        assert!(!slice.big_tiff());

        let directory = gt.request_ifd(0).unwrap();
        assert_eq!(
            directory.get_value("ImageWidth").and_then(IfdValue::as_u64),
            Some(37)
        );
        assert!(gt.request_ifd(1).is_err());
    }

    #[tokio::test]
    async fn out_of_range_index_gives_a_geotiff_image_index_error() {
        let gt = SingleGeoTiff::open(fixture_reader()).await.unwrap();
        let err = gt
            .read_rasters(5, None, None, None, "nearest", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("No image at index 5"));
    }

    #[tokio::test]
    async fn read_rasters_and_read_rgb_work_through_the_dataset_api() {
        let gt = SingleGeoTiff::open(fixture_reader()).await.unwrap();

        let raster = gt
            .read_rasters(0, None, None, None, "nearest", None)
            .await
            .unwrap();
        assert_eq!(
            (raster.width, raster.height, raster.samples_per_pixel),
            (37, 51, 1)
        );

        let rgb = gt
            .read_rgb(0, None, None, None, "nearest", false, None)
            .await
            .unwrap();
        assert_eq!((rgb.width, rgb.height, rgb.samples_per_pixel), (37, 51, 3));
    }

    #[tokio::test]
    async fn read_rasters_respects_a_window() {
        let gt = SingleGeoTiff::open(fixture_reader()).await.unwrap();

        let window = ImageWindow {
            x0: 2,
            y0: 2,
            x1: 8,
            y1: 8,
        };
        let raster = gt
            .read_rasters(0, Some(window), None, None, "nearest", None)
            .await
            .unwrap();
        assert_eq!(
            (raster.width, raster.height, raster.samples_per_pixel),
            (6, 6, 1)
        );
    }

    #[tokio::test]
    async fn read_rasters_bands_works_through_the_dataset_api() {
        let gt = SingleGeoTiff::open(fixture_reader()).await.unwrap();
        let bands = gt
            .read_rasters_bands(0, None, None, None, "nearest", None)
            .await
            .unwrap();
        assert_eq!((bands.width, bands.height, bands.bands.len()), (37, 51, 1));
    }

    fn striped_fixture_reader() -> Arc<dyn AsyncFileReader> {
        let data = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/minisblack-1c-8b.tiff"
        ))
        .unwrap();
        Arc::new(BytesReader(Bytes::from(data)))
    }

    /// Two distinct real files (`tiled-gray-i1.tif` as "main", the striped
    /// `minisblack-1c-8b.tiff` as a single "overview"), each with exactly
    /// one image - genuinely exercises `locate`'s cross-file index walk
    /// (global index 0 -> main file's image 0, global index 1 -> the
    /// overview file's image 0), not just a single-file passthrough.
    #[tokio::test]
    async fn multi_geotiff_sums_image_counts_and_locates_across_files() {
        let main = SingleGeoTiff::open(fixture_reader()).await.unwrap();
        let overview = SingleGeoTiff::open(striped_fixture_reader()).await.unwrap();
        let multi = MultiGeoTiff::new(main, vec![overview]);

        assert_eq!(multi.image_count(), 2);

        let from_main = multi
            .read_rasters(0, None, None, None, "nearest", None)
            .await
            .unwrap();
        assert_eq!(
            (
                from_main.width,
                from_main.height,
                from_main.samples_per_pixel
            ),
            (37, 51, 1)
        );

        let from_overview = multi
            .read_rasters(1, None, None, None, "nearest", None)
            .await
            .unwrap();
        assert_eq!(
            (
                from_overview.width,
                from_overview.height,
                from_overview.samples_per_pixel
            ),
            (157, 151, 1)
        );

        let directories = multi.parse_file_directories_per_file().unwrap();
        let widths = directories
            .iter()
            .map(|directory| {
                directory
                    .get_value("ImageWidth")
                    .and_then(IfdValue::as_u64)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(widths, vec![37, 157]);
    }

    #[tokio::test]
    async fn multi_geotiff_out_of_range_index_gives_a_geotiff_image_index_error() {
        let main = SingleGeoTiff::open(fixture_reader()).await.unwrap();
        let overview = SingleGeoTiff::open(striped_fixture_reader()).await.unwrap();
        let multi = MultiGeoTiff::new(main, vec![overview]);

        let err = multi
            .read_rasters(2, None, None, None, "nearest", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("No image at index 2"));
    }

    #[tokio::test]
    async fn multi_geotiff_read_rasters_bands_and_read_rgb_delegate_to_the_owning_file() {
        let main = SingleGeoTiff::open(fixture_reader()).await.unwrap();
        let overview = SingleGeoTiff::open(striped_fixture_reader()).await.unwrap();
        let multi = MultiGeoTiff::new(main, vec![overview]);

        let bands = multi
            .read_rasters_bands(1, None, None, None, "nearest", None)
            .await
            .unwrap();
        assert_eq!(
            (bands.width, bands.height, bands.bands.len()),
            (157, 151, 1)
        );

        let rgb = multi
            .read_rgb(0, None, None, None, "nearest", false, None)
            .await
            .unwrap();
        assert_eq!((rgb.width, rgb.height, rgb.samples_per_pixel), (37, 51, 3));
    }

    #[tokio::test]
    async fn geotiff_dataset_multi_variant_reports_the_combined_image_count() {
        let main = SingleGeoTiff::open(fixture_reader()).await.unwrap();
        let overview = SingleGeoTiff::open(striped_fixture_reader()).await.unwrap();
        let dataset = GeoTiffDataset::Multi(MultiGeoTiff::new(main, vec![overview]));
        assert_eq!(dataset.image_count(), 2);
    }

    /// A minimal, hand-built, single-IFD, uncompressed 8-bit grayscale
    /// `width`x`height` TIFF, every pixel set to `pixel_value` (so main vs.
    /// overview reads are trivially distinguishable), carrying
    /// `ModelPixelScale`/`ModelTiepoint` (so `geo.rs`'s functions have real
    /// tags to read) and an optional `NewSubfileType` (so it can act as a
    /// `readRasters` best-fit overview candidate). Async-tiff has no public
    /// IFD constructor, hence building real bytes rather than an in-memory
    /// struct - the same approach `geo.rs`'s own tests use.
    fn single_image_tiff(
        width: u16,
        height: u16,
        pixel_value: u8,
        new_subfile_type: Option<u32>,
        pixel_scale: [f64; 3],
        tiepoint: [f64; 6],
    ) -> Vec<u8> {
        let mut entries: Vec<(u16, u16, u32, [u8; 4])> = vec![
            (
                256,
                3,
                1,
                [width.to_le_bytes()[0], width.to_le_bytes()[1], 0, 0],
            ), // ImageWidth
            (
                257,
                3,
                1,
                [height.to_le_bytes()[0], height.to_le_bytes()[1], 0, 0],
            ), // ImageLength
            (258, 3, 1, [8, 0, 0, 0]), // BitsPerSample
            (259, 3, 1, [1, 0, 0, 0]), // Compression = 1 (none)
            (262, 3, 1, [1, 0, 0, 0]), // PhotometricInterpretation = BlackIsZero
            (273, 4, 1, [0, 0, 0, 0]), // StripOffsets, patched below
            (277, 3, 1, [1, 0, 0, 0]), // SamplesPerPixel = 1
            (
                278,
                3,
                1,
                [height.to_le_bytes()[0], height.to_le_bytes()[1], 0, 0],
            ), // RowsPerStrip = height (one strip)
            (279, 4, 1, ((width as u32) * (height as u32)).to_le_bytes()), // StripByteCounts
        ];
        if let Some(nst) = new_subfile_type {
            entries.push((254, 4, 1, nst.to_le_bytes()));
        }

        let double_tags: [(u16, &[f64]); 2] = [(33550, &pixel_scale), (33922, &tiepoint)];
        let ifd_entry_count = entries.len() + double_tags.len();
        let ifd_header_size = 2 + ifd_entry_count * 12 + 4;
        let data_block_start = 8 + ifd_header_size as u32;

        let mut data_block = Vec::new();
        for (tag, values) in double_tags {
            let offset = data_block_start + data_block.len() as u32;
            for v in values {
                data_block.extend_from_slice(&v.to_le_bytes());
            }
            entries.push((tag, 12, values.len() as u32, offset.to_le_bytes()));
        }

        let strip_offset = data_block_start + data_block.len() as u32;
        entries[5] = (273, 4, 1, strip_offset.to_le_bytes());
        entries.sort_by_key(|e| e.0);

        let mut out = Vec::new();
        out.extend_from_slice(b"II");
        out.extend_from_slice(&42u16.to_le_bytes());
        out.extend_from_slice(&8u32.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for &(tag, field_type, count, ref value_bytes) in &entries {
            out.extend_from_slice(&tag.to_le_bytes());
            out.extend_from_slice(&field_type.to_le_bytes());
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(value_bytes);
        }
        out.extend_from_slice(&0u32.to_le_bytes()); // next IFD offset = 0 (none)
        out.extend_from_slice(&data_block);
        out.extend(std::iter::repeat_n(
            pixel_value,
            (width as usize) * (height as usize),
        ));

        assert_eq!(
            out.len(),
            (strip_offset as usize) + (width as usize) * (height as usize)
        );
        out
    }

    /// Main: 4x4, pixel value 0x11, resolution (1, 1) (fine). Overview:
    /// 2x2, pixel value 0x22, `NewSubfileType` bit 0 set (marks it a
    /// reduced-resolution candidate), resolution (2, 2) (coarse, half the
    /// main's - matches its half pixel count over the same world extent).
    /// Both share the same origin, so their bounding boxes coincide.
    async fn main_plus_overview_dataset() -> GeoTiffDataset {
        let main_bytes = single_image_tiff(
            4,
            4,
            0x11,
            None,
            [1.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        );
        let overview_bytes = single_image_tiff(
            2,
            2,
            0x22,
            Some(1),
            [2.0, 2.0, 0.0],
            [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        );
        let main = SingleGeoTiff::open(Arc::new(BytesReader(Bytes::from(main_bytes))))
            .await
            .unwrap();
        let overview = SingleGeoTiff::open(Arc::new(BytesReader(Bytes::from(overview_bytes))))
            .await
            .unwrap();
        GeoTiffDataset::Multi(MultiGeoTiff::new(main, vec![overview]))
    }

    fn default_best_fit_options() -> BestFitOptions {
        BestFitOptions::default()
    }

    #[tokio::test]
    async fn best_fit_picks_the_overview_when_a_coarse_resolution_is_requested() {
        let dataset = main_plus_overview_dataset().await;
        // Requested resX=3 is coarser than the overview's own resX=2, so the
        // overview (the first, coarsest, candidate) already satisfies it.
        let raster = dataset
            .read_rasters_best_fit_interleaved(BestFitOptions {
                res_x: Some(3.0),
                ..default_best_fit_options()
            })
            .await
            .unwrap();
        assert_eq!((raster.width, raster.height), (2, 2));
        match &raster.data {
            crate::typed_array::TypedArray::Uint8(pixels) => {
                assert!(pixels.iter().all(|&p| p == 0x22))
            }
            other => panic!("expected Uint8, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn best_fit_default_preserves_javascript_non_interleaved_shape() {
        let dataset = main_plus_overview_dataset().await;
        let result = dataset
            .read_rasters_best_fit(BestFitOptions::default())
            .await
            .unwrap();
        let ReadRasterResult::Bands(raster) = result else {
            panic!("omitted interleave must return separate bands")
        };
        assert_eq!((raster.width, raster.height, raster.bands.len()), (4, 4, 1));
    }

    #[test]
    fn bbox_rounding_uses_javascript_negative_tie_rule() {
        assert_eq!(js_round_to_i64(-1.5).unwrap(), -1);
        assert_eq!(js_round_to_i64(-1.6).unwrap(), -2);
        assert_eq!(js_round_to_i64(1.5).unwrap(), 2);
        assert!(js_round_to_i64(f64::NAN).is_err());
    }

    #[tokio::test]
    async fn best_fit_picks_the_main_image_when_a_fine_resolution_is_requested() {
        let dataset = main_plus_overview_dataset().await;
        // Requested resX=0.5 is finer than either candidate's own resX (2
        // and 1), so neither satisfies it and the loop falls through to the
        // finest (main) image.
        let raster = dataset
            .read_rasters_best_fit_interleaved(BestFitOptions {
                res_x: Some(0.5),
                ..default_best_fit_options()
            })
            .await
            .unwrap();
        assert_eq!((raster.width, raster.height), (4, 4));
        match &raster.data {
            crate::typed_array::TypedArray::Uint8(pixels) => {
                assert!(pixels.iter().all(|&p| p == 0x11))
            }
            other => panic!("expected Uint8, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn best_fit_derives_resolution_from_out_width_against_the_main_bounding_box() {
        let dataset = main_plus_overview_dataset().await;
        // Main's bbox is [0,0,4,4] (origin (0,0), resolution (1,1), size
        // 4x4) - out_width=1 -> resX = 4/1 = 4, clearly coarser than the
        // overview's own resX=2, so the overview gets selected. `out_width`
        // is then *also* forwarded as the actual resize target for the
        // final read (same dual-purpose-options behavior as the original),
        // so the returned width is the resized 1, not the overview's native
        // 2 - height stays at the overview's native 2 since out_height
        // wasn't given, which is what actually proves the overview (not
        // main) was the one selected and read from.
        let raster = dataset
            .read_rasters_best_fit_interleaved(BestFitOptions {
                out_width: Some(1),
                ..default_best_fit_options()
            })
            .await
            .unwrap();
        assert_eq!((raster.width, raster.height), (1, 2));
        match &raster.data {
            crate::typed_array::TypedArray::Uint8(pixels) => {
                assert!(pixels.iter().all(|&p| p == 0x22))
            }
            other => panic!("expected Uint8, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn best_fit_computes_a_window_from_a_bbox() {
        let dataset = main_plus_overview_dataset().await;
        // Main image: origin (0,0), resolution (1,-1) - Y is negated by
        // `get_resolution` (world Y increases upward, pixel row increases
        // downward), so pixel window [1,1,3,3) corresponds to world bbox
        // [1,-3,3,-1], not [1,1,3,3]. No resX/resY requested, so no
        // overview selection runs and the main image (index 0) is used.
        let raster = dataset
            .read_rasters_best_fit_interleaved(BestFitOptions {
                bbox: Some([1.0, -3.0, 3.0, -1.0]),
                ..default_best_fit_options()
            })
            .await
            .unwrap();
        assert_eq!((raster.width, raster.height), (2, 2));
        match &raster.data {
            crate::typed_array::TypedArray::Uint8(pixels) => {
                assert!(pixels.iter().all(|&p| p == 0x11))
            }
            other => panic!("expected Uint8, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn best_fit_rejects_both_window_and_bbox() {
        let dataset = main_plus_overview_dataset().await;
        let options = BestFitOptions {
            window: Some(ImageWindow {
                x0: 0,
                y0: 0,
                x1: 1,
                y1: 1,
            }),
            bbox: Some([0.0, 0.0, 1.0, 1.0]),
            ..default_best_fit_options()
        };
        let err = dataset.read_rasters_best_fit(options).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("Both \"bbox\" and \"window\" passed")
        );
    }

    #[tokio::test]
    async fn best_fit_rejects_both_out_width_and_res_x() {
        let dataset = main_plus_overview_dataset().await;
        let options = BestFitOptions {
            out_width: Some(2),
            res_x: Some(1.0),
            ..default_best_fit_options()
        };
        let err = dataset.read_rasters_best_fit(options).await.unwrap_err();
        assert!(err.to_string().contains("Both width and resX passed"));
    }

    #[tokio::test]
    async fn best_fit_respects_an_already_cancelled_token() {
        let dataset = main_plus_overview_dataset().await;
        let token = CancellationToken::new();
        token.cancel();
        let options = BestFitOptions {
            cancellation: Some(token),
            ..default_best_fit_options()
        };
        let err = dataset.read_rasters_best_fit(options).await.unwrap_err();
        assert!(err.to_string().contains("cancelled"));
    }

    #[tokio::test]
    async fn image_api_defaults_to_non_interleaved_output() {
        let dataset = SingleGeoTiff::open(fixture_reader()).await.unwrap();
        let result = dataset
            .image(0)
            .unwrap()
            .read_rasters(crate::geotiffimage::ReadRastersOptions::default())
            .await
            .unwrap();
        match result {
            crate::geotiffimage::ReadRasterResult::Bands(raster) => {
                assert_eq!((raster.width, raster.height), (37, 51));
                assert_eq!(raster.bands.len(), 1);
            }
            crate::geotiffimage::ReadRasterResult::Interleaved(_) => {
                panic!("omitted interleave must return separate bands")
            }
        }
    }

    #[tokio::test]
    async fn image_api_prefills_out_of_bounds_pixels() {
        let dataset = SingleGeoTiff::open(fixture_reader()).await.unwrap();
        let result = dataset
            .image(0)
            .unwrap()
            .read_rasters(crate::geotiffimage::ReadRastersOptions {
                window: Some(ImageWindow {
                    x0: -1,
                    y0: -1,
                    x1: 2,
                    y1: 2,
                }),
                fill_value: Some(crate::geotiffimage::FillValue::Scalar(9.0)),
                ..Default::default()
            })
            .await
            .unwrap();
        let crate::geotiffimage::ReadRasterResult::Bands(raster) = result else {
            panic!("expected bands")
        };
        assert_eq!((raster.width, raster.height), (3, 3));
        let crate::typed_array::TypedArray::Uint8(values) = &raster.bands[0] else {
            panic!("expected Uint8")
        };
        assert_eq!(&values[0..3], &[9, 9, 9]);
        assert_eq!(values[3], 9);
        assert_eq!(values[6], 9);
    }

    #[tokio::test]
    async fn interleaved_fill_rejects_an_array_like_js() {
        let dataset = SingleGeoTiff::open(fixture_reader()).await.unwrap();
        let error = dataset
            .image(0)
            .unwrap()
            .read_rasters(crate::geotiffimage::ReadRastersOptions {
                interleave: true,
                fill_value: Some(crate::geotiffimage::FillValue::PerSample(vec![1.0])),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("fillValue must be a single number")
        );
    }

    #[tokio::test]
    async fn scalar_nan_fill_is_falsy_like_javascript() {
        let dataset = SingleGeoTiff::open(fixture_reader()).await.unwrap();
        let result = dataset
            .image(0)
            .unwrap()
            .read_rasters(crate::geotiffimage::ReadRastersOptions {
                window: Some(ImageWindow {
                    x0: -1,
                    y0: -1,
                    x1: 1,
                    y1: 1,
                }),
                fill_value: Some(crate::geotiffimage::FillValue::Scalar(f64::NAN)),
                ..Default::default()
            })
            .await
            .unwrap();
        let crate::geotiffimage::ReadRasterResult::Bands(raster) = result else {
            panic!("expected bands")
        };
        assert_eq!(raster.bands[0].get_f64(0), 0.0);
    }
}
