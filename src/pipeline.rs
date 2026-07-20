//! Native read/decode pipeline: `AsyncFileReader` -> lossless metadata and
//! tile planning -> Rayon-offloaded synchronous decoder registry -> decoded
//! array. This composes the behavior of geotiff.js's `GeoTIFF.fromSource`,
//! `GeoTIFFImage.getTileOrStrip`, and `readRasters` around native traits.

use crate::decode_pool::CancellationToken;
use async_tiff::decoder::DecoderRegistry;
use async_tiff::error::AsyncTiffResult;
use async_tiff::reader::AsyncFileReader;
use async_tiff::{Array, ImageFileDirectory, TIFF};
use std::sync::Arc;

/// `GeoTIFF.fromSource`'s metadata-parsing half. The compatibility parser
/// retains a 64 KiB prefix while discovering the complete IFD chain, so
/// small header/tag reads do not become duplicate network round trips.
pub async fn open_tiff(reader: Arc<dyn AsyncFileReader>) -> AsyncTiffResult<TIFF> {
    Ok(crate::source::metadata_compat::prepare_metadata(reader)
        .await?
        .tiff)
}

/// Fetches one tile's compressed bytes (I/O, on the calling Tokio task) then
/// decodes it on `decode_pool`'s dedicated Rayon pool, keeping CPU work off
/// Tokio/GPUI threads. `cancellation`, when given, is checked by
/// `spawn_decode` before and after the CPU-bound step.
pub async fn fetch_and_decode_tile(
    ifd: &ImageFileDirectory,
    x: usize,
    y: usize,
    reader: &dyn AsyncFileReader,
    registry: Arc<DecoderRegistry>,
    cancellation: Option<&CancellationToken>,
) -> AsyncTiffResult<Array> {
    let tile = ifd.fetch_tile(x, y, reader).await?;
    crate::decode_pool::spawn_decode(move || tile.decode(&registry), cancellation).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compression::registry::build_decoder_registry;
    use async_tiff::TypedArray;
    use bytes::Bytes;
    use std::ops::Range;

    /// In-memory `AsyncFileReader` for tests - no real I/O backend needed to
    /// exercise the metadata + tile-decode wiring itself.
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

    /// End-to-end smoke test against a real (bundled, 614-byte) tiled TIFF
    /// fixture - the same file used by async-tiff's own test suite
    /// (`fixtures/image-tiff/tiled-gray-i1.tif`, originally from the
    /// `image-tiff` crate's test corpus). This validates our own
    /// wiring (`open_tiff`/`fetch_and_decode_tile`/`build_decoder_registry`),
    /// not async-tiff's internal decode correctness (that's covered by its
    /// own test suite).
    #[tokio::test]
    async fn opens_a_real_tiff_and_decodes_its_first_tile() {
        let reader = fixture_reader();
        let tiff = open_tiff(reader.clone()).await.unwrap();
        assert_eq!(tiff.ifds().len(), 1);

        let ifd = &tiff.ifds()[0];
        assert_eq!(ifd.image_width(), 37);
        assert_eq!(ifd.image_height(), 51);
        assert_eq!(ifd.tile_count(), Some((3, 4)));

        let registry = Arc::new(build_decoder_registry());
        let array = fetch_and_decode_tile(ifd, 0, 0, reader.as_ref(), registry, None)
            .await
            .unwrap();

        // 1-bit-per-sample -> async-tiff expands to a 16x16 Bool array (tile size, not cropped
        // to image bounds - cropping to the image's true 37x51 extent is a caller concern).
        assert_eq!(array.shape(), [16, 16, 1]);
        match array.data() {
            TypedArray::Bool(pixels) => {
                assert_eq!(pixels.len(), 256);
                // spot-checked against a real decode run (examples/inspect_fixture.rs) -
                // the fixture's top-left corner is solid white (BlackIsZero photometric, true=white)
                assert!(pixels[0..8].iter().all(|&p| p));
            }
            other => panic!("expected a Bool array for a 1-bit sample, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn decoding_every_tile_in_the_fixture_succeeds() {
        let reader = fixture_reader();
        let tiff = open_tiff(reader.clone()).await.unwrap();
        let ifd = &tiff.ifds()[0];
        let (cols, rows) = ifd.tile_count().unwrap();
        let registry = Arc::new(build_decoder_registry());

        for y in 0..rows {
            for x in 0..cols {
                let array =
                    fetch_and_decode_tile(ifd, x, y, reader.as_ref(), registry.clone(), None)
                        .await
                        .unwrap();
                assert_eq!(array.shape(), [16, 16, 1]);
            }
        }
    }
}
