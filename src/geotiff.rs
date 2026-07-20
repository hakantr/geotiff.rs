//! `GeoTIFFImageIndexError` compatibility type. The operational
//! `GeoTIFFBase`/`GeoTIFF`/`MultiGeoTIFF` counterparts live in `dataset` as
//! `GeoTiffDataset`, `SingleGeoTiff` and `MultiGeoTiff`.

use std::fmt;

/// `class GeoTIFFImageIndexError extends Error`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeoTiffImageIndexError {
    pub index: u32,
}

impl GeoTiffImageIndexError {
    pub fn new(index: u32) -> Self {
        GeoTiffImageIndexError { index }
    }
}

impl fmt::Display for GeoTiffImageIndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "No image at index {}", self.index)
    }
}

impl std::error::Error for GeoTiffImageIndexError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_includes_the_index() {
        let err = GeoTiffImageIndexError::new(3);
        assert_eq!(err.to_string(), "No image at index 3");
    }
}
