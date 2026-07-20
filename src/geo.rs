//! Port of geotiff.js's affine-georeferencing helpers
//! (`GeoTIFFImage.getOrigin`/`getResolution`/`getBoundingBox`,
//! `geotiffimage.js:953-1075`). Pure arithmetic over three already-parsed
//! TIFF tags (`ModelTiepoint`/`ModelPixelScale`/`ModelTransformation`) -
//! `async_tiff::ImageFileDirectory` already exposes all three
//! (`model_tiepoint()`/`model_pixel_scale()`/`model_transformation()`), so
//! this needed no new tag parsing, just the same math the original does.
//! Deliberately **not** a coordinate-reference-system/reprojection layer -
//! that's the separate, much larger GeoKeys/CRS subsystem this port
//! doesn't touch; these three functions only convert between pixel space
//! and this image's own affine-transformed world space.

use crate::error::GeotiffError;
use async_tiff::ImageFileDirectory;

/// `GeoTIFFImage.getOrigin()`: the image's world-space origin as an XYZ
/// vector, from `ModelTiepoint` or `ModelTransformation`.
pub fn get_origin(ifd: &ImageFileDirectory) -> Result<[f64; 3], GeotiffError> {
    if let Some(tie_points) = ifd.model_tiepoint()
        && tie_points.len() == 6
    {
        return Ok([tie_points[3], tie_points[4], tie_points[5]]);
    }
    if let Some(t) = ifd.model_transformation() {
        if t.len() < 12 {
            return Err(GeotiffError::InvalidAffineTransformation(format!(
                "ModelTransformation has {} values; at least 12 are required",
                t.len()
            )));
        }
        return Ok([t[3], t[7], t[11]]);
    }
    Err(GeotiffError::NoAffineTransformation)
}

/// `GeoTIFFImage.getResolution(referenceImage)`: the image's pixel size in
/// world units, from `ModelPixelScale` or `ModelTransformation`. `reference`
/// mirrors the original's fallback for images (typically overviews) that
/// carry neither tag themselves: derive resolution from `reference`'s own
/// resolution, scaled by the width/height ratio between the two images.
/// `reference`'s own resolution is computed with no further fallback
/// (`getResolution(referenceImage)` in the original calls
/// `referenceImage.getResolution()` with no argument), matching the
/// original exactly - a reference image that itself lacks affine tags still
/// errors.
pub fn get_resolution(
    ifd: &ImageFileDirectory,
    reference: Option<&ImageFileDirectory>,
) -> Result<[f64; 3], GeotiffError> {
    if let Some(scale) = ifd.model_pixel_scale() {
        if scale.len() < 3 {
            return Err(GeotiffError::InvalidAffineTransformation(format!(
                "ModelPixelScale has {} values; at least 3 are required",
                scale.len()
            )));
        }
        return Ok([scale[0], -scale[1], scale[2]]);
    }
    if let Some(t) = ifd.model_transformation() {
        if t.len() < 11 {
            return Err(GeotiffError::InvalidAffineTransformation(format!(
                "ModelTransformation has {} values; at least 11 are required",
                t.len()
            )));
        }
        if t[1] == 0.0 && t[4] == 0.0 {
            return Ok([t[0], -t[5], t[10]]);
        }
        return Ok([
            ((t[0] * t[0]) + (t[4] * t[4])).sqrt(),
            -((t[1] * t[1]) + (t[5] * t[5])).sqrt(),
            t[10],
        ]);
    }
    if let Some(reference) = reference {
        let [ref_res_x, ref_res_y, ref_res_z] = get_resolution(reference, None)?;
        let ref_width = reference.image_width() as f64;
        let ref_height = reference.image_height() as f64;
        let width = ifd.image_width() as f64;
        let height = ifd.image_height() as f64;
        return Ok([
            ref_res_x * ref_width / width,
            ref_res_y * ref_height / height,
            ref_res_z * ref_width / width,
        ]);
    }
    Err(GeotiffError::NoAffineTransformation)
}

/// `GeoTIFFImage.getBoundingBox(tilegrid)`: `[minX, minY, maxX, maxY]` in
/// world space. When `ModelTransformation` is present and `tilegrid` is
/// `false`, all four pixel-space corners are projected through the full
/// affine matrix and min/max'd (handles rotation/shear); otherwise it's the
/// simpler origin + resolution * size box.
pub fn get_bounding_box(
    ifd: &ImageFileDirectory,
    tilegrid: bool,
) -> Result<[f64; 4], GeotiffError> {
    let height = ifd.image_height() as f64;
    let width = ifd.image_width() as f64;

    if !tilegrid && let Some(t) = ifd.model_transformation() {
        if t.len() < 8 {
            return Err(GeotiffError::InvalidAffineTransformation(format!(
                "ModelTransformation has {} values; at least 8 are required",
                t.len()
            )));
        }
        let (a, b, d, e, f, h) = (t[0], t[1], t[3], t[4], t[5], t[7]);
        let corners = [(0.0, 0.0), (0.0, height), (width, 0.0), (width, height)];
        let projected: Vec<(f64, f64)> = corners
            .iter()
            .map(|&(i, j)| (d + (a * i) + (b * j), h + (e * i) + (f * j)))
            .collect();
        let min_x = projected.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
        let max_x = projected
            .iter()
            .map(|p| p.0)
            .fold(f64::NEG_INFINITY, f64::max);
        let min_y = projected.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
        let max_y = projected
            .iter()
            .map(|p| p.1)
            .fold(f64::NEG_INFINITY, f64::max);
        return Ok([min_x, min_y, max_x, max_y]);
    }

    let origin = get_origin(ifd)?;
    let resolution = get_resolution(ifd, None)?;
    let x1 = origin[0];
    let y1 = origin[1];
    let x2 = x1 + (resolution[0] * width);
    let y2 = y1 + (resolution[1] * height);
    Ok([x1.min(x2), y1.min(y2), x1.max(x2), y1.max(y2)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::open_tiff;
    use async_tiff::error::AsyncTiffResult;
    use async_tiff::reader::AsyncFileReader;
    use bytes::Bytes;
    use std::ops::Range;
    use std::sync::Arc;

    #[derive(Debug)]
    struct BytesReader(Bytes);

    #[async_trait::async_trait]
    impl AsyncFileReader for BytesReader {
        async fn get_bytes(&self, range: Range<u64>) -> AsyncTiffResult<Bytes> {
            let end = (range.end as usize).min(self.0.len());
            Ok(self.0.slice(range.start as usize..end))
        }
    }

    /// `tests/fixtures/tiled-gray-i1.tif` has no geo tags at all - a real
    /// fixture, but one that exercises the "no affine transformation"
    /// error path rather than the happy path (no bundled fixture carries
    /// `ModelPixelScale`/`ModelTiepoint`/`ModelTransformation`).
    #[tokio::test]
    async fn a_non_georeferenced_fixture_reports_no_affine_transformation() {
        let data = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/tiled-gray-i1.tif"
        ))
        .unwrap();
        let reader: Arc<dyn AsyncFileReader> = Arc::new(BytesReader(Bytes::from(data)));
        let tiff = open_tiff(reader).await.unwrap();
        let ifd = &tiff.ifds()[0];

        assert_eq!(get_origin(ifd), Err(GeotiffError::NoAffineTransformation));
        assert_eq!(
            get_resolution(ifd, None),
            Err(GeotiffError::NoAffineTransformation)
        );
        assert_eq!(
            get_bounding_box(ifd, false),
            Err(GeotiffError::NoAffineTransformation)
        );
    }

    // The remaining tests build a synthetic `ImageFileDirectory` in memory
    // (async-tiff has no public IFD constructor, so these go through a
    // hand-built minimal TIFF byte stream) to exercise the geo-tag happy
    // paths a plain fixture can't, since none of the bundled fixtures carry
    // geo tags. Kept intentionally small - just enough bytes for a valid
    // 1x1 uncompressed TIFF plus whichever DOUBLE-typed geo tags (field
    // type 12: `ModelTiepoint`/`ModelPixelScale`/`ModelTransformation`) a
    // given test needs, with their values in an external data block after
    // the IFD (mirroring how real TIFF encoders place count>1 entries,
    // since 8*count bytes never fits in an entry's inline 4-byte slot).
    fn tiff_with_double_tags(tagged_values: &[(u16, Vec<f64>)]) -> Vec<u8> {
        let mut entries: Vec<(u16, u16, u32, [u8; 4])> = vec![
            (256, 3, 1, [1, 0, 0, 0]), // ImageWidth = 1
            (257, 3, 1, [1, 0, 0, 0]), // ImageLength = 1
            (258, 3, 1, [8, 0, 0, 0]), // BitsPerSample = 8
            (259, 3, 1, [1, 0, 0, 0]), // Compression = 1 (none)
            (262, 3, 1, [1, 0, 0, 0]), // PhotometricInterpretation = BlackIsZero
            (273, 4, 1, [0, 0, 0, 0]), // StripOffsets, patched below once the layout is known
            (277, 3, 1, [1, 0, 0, 0]), // SamplesPerPixel = 1
            (278, 3, 1, [1, 0, 0, 0]), // RowsPerStrip = 1
            (279, 4, 1, [1, 0, 0, 0]), // StripByteCounts = 1
        ];

        let ifd_entry_count = entries.len() + tagged_values.len();
        let ifd_header_size = 2 + ifd_entry_count * 12 + 4;
        let data_block_start = 8 + ifd_header_size as u32;

        let mut data_block = Vec::new();
        let mut geo_entries = Vec::new();
        for (tag, values) in tagged_values {
            let offset = data_block_start + data_block.len() as u32;
            for v in values {
                data_block.extend_from_slice(&v.to_le_bytes());
            }
            geo_entries.push((*tag, 12u16, values.len() as u32, offset.to_le_bytes()));
        }

        let strip_offset = data_block_start + data_block.len() as u32;
        entries[5] = (273, 4, 1, strip_offset.to_le_bytes());
        entries.extend(geo_entries);
        entries.sort_by_key(|e| e.0); // TIFF spec requires tags in ascending order

        let mut out = Vec::new();
        out.extend_from_slice(b"II"); // little-endian
        out.extend_from_slice(&42u16.to_le_bytes()); // TIFF magic
        out.extend_from_slice(&8u32.to_le_bytes()); // offset to first IFD
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for &(tag, field_type, count, ref value_bytes) in &entries {
            out.extend_from_slice(&tag.to_le_bytes());
            out.extend_from_slice(&field_type.to_le_bytes());
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(value_bytes);
        }
        out.extend_from_slice(&0u32.to_le_bytes()); // next IFD offset = 0 (none)
        out.extend_from_slice(&data_block);
        out.push(0xAB); // the 1x1 image's single pixel byte

        assert_eq!(out.len(), (strip_offset + 1) as usize);
        out
    }

    async fn open_bytes(bytes: Vec<u8>) -> async_tiff::TIFF {
        let reader: Arc<dyn AsyncFileReader> = Arc::new(BytesReader(Bytes::from(bytes)));
        open_tiff(reader).await.unwrap()
    }

    #[tokio::test]
    async fn get_origin_reads_model_tiepoint() {
        // ModelTiepointTag = 33922, 6 doubles: [i, j, k, x, y, z] - origin is (x, y, z) = last 3.
        let bytes = tiff_with_double_tags(&[(33922, vec![0.0, 0.0, 0.0, 100.0, 200.0, 0.0])]);
        let tiff = open_bytes(bytes).await;
        let ifd = &tiff.ifds()[0];
        assert_eq!(get_origin(ifd).unwrap(), [100.0, 200.0, 0.0]);
    }

    #[tokio::test]
    async fn get_resolution_reads_model_pixel_scale_and_negates_y() {
        // ModelPixelScaleTag = 33550, 3 doubles: [scaleX, scaleY, scaleZ].
        let bytes = tiff_with_double_tags(&[(33550, vec![2.0, 3.0, 0.0])]);
        let tiff = open_bytes(bytes).await;
        let ifd = &tiff.ifds()[0];
        // Y resolution is negated (world Y decreases as pixel row increases) - geotiffimage.js:989.
        assert_eq!(get_resolution(ifd, None).unwrap(), [2.0, -3.0, 0.0]);
    }

    #[tokio::test]
    async fn get_bounding_box_derives_from_origin_and_resolution_when_no_transformation_matrix() {
        let bytes = tiff_with_double_tags(&[
            (33922, vec![0.0, 0.0, 0.0, 10.0, 20.0, 0.0]),
            (33550, vec![2.0, 2.0, 0.0]),
        ]);
        let tiff = open_bytes(bytes).await;
        let ifd = &tiff.ifds()[0];
        // 1x1 image, origin (10, 20), resolution (2, -2) -> x2 = 12, y2 = 18.
        assert_eq!(
            get_bounding_box(ifd, false).unwrap(),
            [10.0, 18.0, 12.0, 20.0]
        );
    }

    #[tokio::test]
    async fn get_resolution_falls_back_to_a_reference_image_scaled_by_size_ratio() {
        let main_bytes = tiff_with_double_tags(&[(33550, vec![2.0, 2.0, 0.0])]);
        let main_tiff = open_bytes(main_bytes).await;
        let main_ifd = &main_tiff.ifds()[0];

        let overview_bytes = tiff_with_double_tags(&[]); // no geo tags of its own
        let overview_tiff = open_bytes(overview_bytes).await;
        let overview_ifd = &overview_tiff.ifds()[0];

        // Both fixtures are 1x1, so the ratio is 1:1 - resolution should pass through unchanged.
        assert_eq!(
            get_resolution(overview_ifd, Some(main_ifd)).unwrap(),
            [2.0, -2.0, 0.0]
        );
    }
}
