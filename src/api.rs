//! Root constructors mirroring geotiff.js's exported factory functions.

use crate::dataset::{GeoTiffOptions, MultiGeoTiff, SingleGeoTiff};
use crate::source::reader::{HttpSourceOptions, SourceSpec, open_reader};
use async_tiff::error::{AsyncTiffError, AsyncTiffResult};
use async_tiff::reader::AsyncFileReader;
use bytes::Bytes;
use object_store::ObjectStore;
use std::path::Path;
use std::sync::Arc;

/// Native `GeoTIFF.fromSource`/`fromCustomClient` entry point.
pub async fn from_reader(reader: Arc<dyn AsyncFileReader>) -> AsyncTiffResult<SingleGeoTiff> {
    SingleGeoTiff::open(reader).await
}

pub async fn from_reader_with_options(
    reader: Arc<dyn AsyncFileReader>,
    options: GeoTiffOptions,
) -> AsyncTiffResult<SingleGeoTiff> {
    SingleGeoTiff::open_with_options(reader, options).await
}

/// geotiff.js `fromArrayBuffer`.
pub async fn from_array_buffer(bytes: impl Into<Bytes>) -> AsyncTiffResult<SingleGeoTiff> {
    from_source(SourceSpec::Memory(bytes.into()), reqwest::Client::new()).await
}

pub async fn from_array_buffer_with_options(
    bytes: impl Into<Bytes>,
    options: GeoTiffOptions,
) -> AsyncTiffResult<SingleGeoTiff> {
    from_source_with_options(
        SourceSpec::Memory(bytes.into()),
        reqwest::Client::new(),
        options,
    )
    .await
}

/// Idiomatic alias for `from_array_buffer`.
pub async fn from_bytes(bytes: impl Into<Bytes>) -> AsyncTiffResult<SingleGeoTiff> {
    from_array_buffer(bytes).await
}

pub async fn from_bytes_with_options(
    bytes: impl Into<Bytes>,
    options: GeoTiffOptions,
) -> AsyncTiffResult<SingleGeoTiff> {
    from_array_buffer_with_options(bytes, options).await
}

/// Browser `fromBlob` has the same byte semantics once data reaches native
/// Rust, so its lossless counterpart accepts owned/shared bytes.
pub async fn from_blob(bytes: impl Into<Bytes>) -> AsyncTiffResult<SingleGeoTiff> {
    from_array_buffer(bytes).await
}

pub async fn from_blob_with_options(
    bytes: impl Into<Bytes>,
    options: GeoTiffOptions,
) -> AsyncTiffResult<SingleGeoTiff> {
    from_array_buffer_with_options(bytes, options).await
}

/// geotiff.js `fromFile`.
pub async fn from_file(path: impl AsRef<Path>) -> AsyncTiffResult<SingleGeoTiff> {
    from_source(
        SourceSpec::File(path.as_ref().to_path_buf()),
        reqwest::Client::new(),
    )
    .await
}

pub async fn from_file_with_options(
    path: impl AsRef<Path>,
    options: GeoTiffOptions,
) -> AsyncTiffResult<SingleGeoTiff> {
    from_source_with_options(
        SourceSpec::File(path.as_ref().to_path_buf()),
        reqwest::Client::new(),
        options,
    )
    .await
}

/// geotiff.js `fromUrl` with the default HTTP client.
pub async fn from_url(url: impl AsRef<str>) -> AsyncTiffResult<SingleGeoTiff> {
    from_url_with_options(url, HttpSourceOptions::default()).await
}

/// geotiff.js `fromUrl(url, options)`, including request headers,
/// multi-range requests, full-file fallback and optional block caching.
pub async fn from_url_with_options(
    url: impl AsRef<str>,
    options: HttpSourceOptions,
) -> AsyncTiffResult<SingleGeoTiff> {
    from_url_with_client_and_options(url, reqwest::Client::new(), options).await
}

/// `fromUrl` with a configured reqwest client (authentication, proxy,
/// certificates, timeout, and similar native equivalents of source options).
pub async fn from_url_with_client(
    url: impl AsRef<str>,
    client: reqwest::Client,
) -> AsyncTiffResult<SingleGeoTiff> {
    from_url_with_client_and_options(url, client, HttpSourceOptions::default()).await
}

/// Configured-client and configured-source form of `fromUrl`.
pub async fn from_url_with_client_and_options(
    url: impl AsRef<str>,
    client: reqwest::Client,
    options: HttpSourceOptions,
) -> AsyncTiffResult<SingleGeoTiff> {
    let url = url::Url::parse(url.as_ref())
        .map_err(|error| AsyncTiffError::General(format!("Invalid URL: {error}")))?;
    from_source(SourceSpec::UrlWithOptions { url, options }, client).await
}

/// Complete `fromUrl` configuration: source/HTTP behavior plus TIFF-level
/// decoded-block caching and a caller supplied decoder registry.
pub async fn from_url_with_all_options(
    url: impl AsRef<str>,
    client: reqwest::Client,
    source_options: HttpSourceOptions,
    tiff_options: GeoTiffOptions,
) -> AsyncTiffResult<SingleGeoTiff> {
    let url = url::Url::parse(url.as_ref())
        .map_err(|error| AsyncTiffError::General(format!("Invalid URL: {error}")))?;
    from_source_with_options(
        SourceSpec::UrlWithOptions {
            url,
            options: source_options,
        },
        client,
        tiff_options,
    )
    .await
}

/// Native object-store source (S3/GCS/Azure/local object store).
pub async fn from_object(
    store: Arc<dyn ObjectStore>,
    path: object_store::path::Path,
) -> AsyncTiffResult<SingleGeoTiff> {
    from_source(SourceSpec::Object { store, path }, reqwest::Client::new()).await
}

pub async fn from_object_with_options(
    store: Arc<dyn ObjectStore>,
    path: object_store::path::Path,
    options: GeoTiffOptions,
) -> AsyncTiffResult<SingleGeoTiff> {
    from_source_with_options(
        SourceSpec::Object { store, path },
        reqwest::Client::new(),
        options,
    )
    .await
}

/// geotiff.js `fromCustomClient`, represented by the range-reader contract
/// the parser actually consumes.
pub async fn from_custom_client(
    reader: Arc<dyn AsyncFileReader>,
) -> AsyncTiffResult<SingleGeoTiff> {
    from_reader(reader).await
}

pub async fn from_custom_client_with_options(
    reader: Arc<dyn AsyncFileReader>,
    options: GeoTiffOptions,
) -> AsyncTiffResult<SingleGeoTiff> {
    from_reader_with_options(reader, options).await
}

/// geotiff.js `fromUrls` (main image plus external overview files).
pub async fn from_urls<I, S>(
    main_url: impl AsRef<str>,
    overview_urls: I,
) -> AsyncTiffResult<MultiGeoTiff>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    from_urls_with_options(main_url, overview_urls, HttpSourceOptions::default()).await
}

/// `fromUrls` with the same remote-source options applied to the main file
/// and all external overview files.
pub async fn from_urls_with_options<I, S>(
    main_url: impl AsRef<str>,
    overview_urls: I,
    options: HttpSourceOptions,
) -> AsyncTiffResult<MultiGeoTiff>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    from_urls_with_all_options(main_url, overview_urls, options, GeoTiffOptions::default()).await
}

pub async fn from_urls_with_all_options<I, S>(
    main_url: impl AsRef<str>,
    overview_urls: I,
    source_options: HttpSourceOptions,
    tiff_options: GeoTiffOptions,
) -> AsyncTiffResult<MultiGeoTiff>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let client = reqwest::Client::new();
    let main = from_url_with_all_options(
        main_url,
        client.clone(),
        source_options.clone(),
        tiff_options.clone(),
    )
    .await?;
    let mut tasks = tokio::task::JoinSet::new();
    for (index, overview) in overview_urls.into_iter().enumerate() {
        let overview = overview.as_ref().to_owned();
        let client = client.clone();
        let source_options = source_options.clone();
        let tiff_options = tiff_options.clone();
        tasks.spawn(async move {
            from_url_with_all_options(overview, client, source_options, tiff_options)
                .await
                .map(|file| (index, file))
        });
    }
    let mut indexed = Vec::new();
    while let Some(result) = tasks.join_next().await {
        let item = result.map_err(|error| {
            AsyncTiffError::General(format!("overview source task failed: {error}"))
        })??;
        indexed.push(item);
    }
    indexed.sort_unstable_by_key(|(index, _)| *index);
    let overviews = indexed.into_iter().map(|(_, file)| file).collect();
    Ok(MultiGeoTiff::new(main, overviews))
}

/// Opens any `SourceSpec`; shared by the root constructors.
pub async fn from_source(
    source: SourceSpec,
    http_client: reqwest::Client,
) -> AsyncTiffResult<SingleGeoTiff> {
    from_source_with_options(source, http_client, GeoTiffOptions::default()).await
}

pub async fn from_source_with_options(
    source: SourceSpec,
    http_client: reqwest::Client,
    options: GeoTiffOptions,
) -> AsyncTiffResult<SingleGeoTiff> {
    let reader = open_reader(source, http_client).await?;
    SingleGeoTiff::open_with_options(reader, options).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn little_endian_bigtiff_one_pixel(bits: u16, value: u64) -> Vec<u8> {
        let byte_count = u64::from(bits).div_ceil(8);
        let entries: [(u16, u16, u64, u64); 11] = [
            (256, 4, 1, 1),               // ImageWidth LONG
            (257, 4, 1, 1),               // ImageLength LONG
            (258, 3, 1, u64::from(bits)), // BitsPerSample SHORT
            (259, 3, 1, 1),               // Compression=None
            (262, 3, 1, 1),               // BlackIsZero
            (273, 16, 1, 252),            // StripOffsets LONG8
            (277, 3, 1, 1),               // SamplesPerPixel
            (278, 4, 1, 1),               // RowsPerStrip
            (279, 16, 1, byte_count),     // StripByteCounts LONG8
            (284, 3, 1, 1),               // PlanarConfiguration
            (339, 3, 1, 1),               // SampleFormat=uint
        ];
        let mut bytes = vec![0u8; 252 + byte_count as usize];
        bytes[0..2].copy_from_slice(b"II");
        bytes[2..4].copy_from_slice(&43u16.to_le_bytes());
        bytes[4..6].copy_from_slice(&8u16.to_le_bytes());
        bytes[8..16].copy_from_slice(&16u64.to_le_bytes());
        bytes[16..24].copy_from_slice(&(entries.len() as u64).to_le_bytes());
        for (index, (tag, field_type, count, value)) in entries.into_iter().enumerate() {
            let start = 24 + index * 20;
            bytes[start..start + 2].copy_from_slice(&tag.to_le_bytes());
            bytes[start + 2..start + 4].copy_from_slice(&field_type.to_le_bytes());
            bytes[start + 4..start + 12].copy_from_slice(&count.to_le_bytes());
            bytes[start + 12..start + 20].copy_from_slice(&value.to_le_bytes());
        }
        bytes[252..].copy_from_slice(&value.to_le_bytes()[..byte_count as usize]);
        bytes
    }

    #[tokio::test]
    async fn array_buffer_and_file_factories_open_the_same_fixture() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/tiled-gray-i1.tif"
        );
        let bytes = std::fs::read(path).unwrap();
        let memory = from_array_buffer(bytes).await.unwrap();
        let file = from_file(path).await.unwrap();
        assert_eq!(memory.image_count(), file.image_count());
        assert_eq!(
            memory.image(0).unwrap().width(),
            file.image(0).unwrap().width()
        );
    }

    #[tokio::test]
    async fn bigtiff_long8_offsets_open_and_read_without_narrowing() {
        let dataset = from_bytes(little_endian_bigtiff_one_pixel(8, 123))
            .await
            .unwrap();
        assert!(dataset.is_big_tiff());
        let image = dataset.image(0).unwrap();
        assert_eq!((image.width(), image.height()), (1, 1));
        let crate::geotiffimage::ReadRasterResult::Bands(raster) = image
            .read_rasters(crate::geotiffimage::ReadRastersOptions::default())
            .await
            .unwrap()
        else {
            panic!("expected bands")
        };
        assert_eq!(raster.bands[0].get_f64(0), 123.0);
    }

    #[tokio::test]
    async fn uint64_rasters_preserve_values_above_javascript_safe_integer_range() {
        let value = 9_007_199_254_740_993u64;
        let dataset = from_bytes(little_endian_bigtiff_one_pixel(64, value))
            .await
            .unwrap();
        let image = dataset.image(0).unwrap();

        let crate::geotiffimage::ReadRasterResult::Bands(raster) = image
            .read_rasters(crate::geotiffimage::ReadRastersOptions::default())
            .await
            .unwrap()
        else {
            panic!("expected bands")
        };
        assert_eq!(
            raster.bands[0],
            crate::typed_array::TypedArray::Uint64(vec![value])
        );

        let crate::geotiffimage::ReadRasterResult::Interleaved(raster) = image
            .read_rasters(crate::geotiffimage::ReadRastersOptions {
                interleave: true,
                width: Some(2),
                height: Some(2),
                ..Default::default()
            })
            .await
            .unwrap()
        else {
            panic!("expected interleaved raster")
        };
        assert_eq!(
            raster.data,
            crate::typed_array::TypedArray::Uint64(vec![value; 4])
        );
    }
}
