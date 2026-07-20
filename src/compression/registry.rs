//! Adapter layer wiring our own codecs into `async-tiff`'s `DecoderRegistry`
//! while retaining geotiff.js decoder-registration semantics.
//! `async_tiff::decoder::DecoderRegistry::default()` covers
//! Raw/Deflate/LZW/JPEG/Zstd plus feature-enabled LERC/WebP/JPEG2k;
//! **PackBits is the one codec it does
//! not ship** (verified directly against async-tiff 0.3.0 source: no
//! `PackBitsDecoder` in its `decoder.rs`, only the `Compression::PackBits`
//! tag value is recognized). This module registers our own
//! `compression::packbits::decode_block` port for that gap. It also replaces
//! panic-prone/default JPEG and LZW paths with compatibility-safe decoders.

use async_tiff::decoder::{Decoder, DecoderRegistry};
use async_tiff::error::{AsyncTiffError, AsyncTiffResult};
use async_tiff::tags::{Compression, PhotometricInterpretation};
use bytes::Bytes;
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder as NativeJpegDecoder;

/// Native counterpart of geotiff.js `addDecoder`. A fresh decoder is
/// created for every registered compression identifier, allowing callers
/// to override built-ins or add private TIFF compression codes.
pub fn add_decoder(
    registry: &mut DecoderRegistry,
    cases: impl IntoIterator<Item = Compression>,
    mut decoder_factory: impl FnMut() -> Box<dyn Decoder>,
) {
    for compression in cases {
        registry.as_mut().insert(compression, decoder_factory());
    }
}

/// Optional native decoder lookup for callers that want to probe registry
/// support without creating an error.
pub fn find_decoder(registry: &DecoderRegistry, compression: Compression) -> Option<&dyn Decoder> {
    registry
        .as_ref()
        .get(&compression)
        .map(std::boxed::Box::as_ref)
}

/// Native lookup counterpart of geotiff.js `getDecoder`.
///
/// Decoder parameters are supplied by the TIFF block pipeline when
/// `decode_tile` is called. Unlike [`find_decoder`], this public compatibility
/// form retains geotiff.js's exact errors for unknown compression identifiers
/// and the built-in old-style JPEG placeholder. A caller-registered decoder
/// for ID 6 still overrides that placeholder, just as `addDecoder(6, ...)`
/// does in JavaScript.
pub fn get_decoder(
    registry: &DecoderRegistry,
    compression: Compression,
) -> AsyncTiffResult<&dyn Decoder> {
    if let Some(decoder) = find_decoder(registry, compression) {
        return Ok(decoder);
    }
    if compression == Compression::JPEG {
        return Err(AsyncTiffError::General(
            "old style JPEG compression is not supported.".to_string(),
        ));
    }
    Err(AsyncTiffError::General(format!(
        "Unknown compression method identifier: {}",
        compression.to_u16()
    )))
}

#[derive(Debug, Clone)]
pub struct PackBitsDecoder;

impl Decoder for PackBitsDecoder {
    fn decode_tile(
        &self,
        buffer: Bytes,
        _photometric_interpretation: PhotometricInterpretation,
        _jpeg_tables: Option<&[u8]>,
        _samples_per_pixel: u16,
        _bits_per_sample: u16,
        _lerc_parameters: Option<&[u32]>,
    ) -> AsyncTiffResult<Vec<u8>> {
        super::packbits::decode_block(&buffer)
            .map_err(|error| async_tiff::error::AsyncTiffError::General(error.to_string()))
    }
}

/// The default async-tiff LZW adapter calls `expect` on a fallible weezl
/// decode, so malformed or incompatible input can panic inside a decode
/// worker. geotiff.js's TIFF-LZW algorithm is ported in this crate; use it
/// directly and surface the same invalid stream as a normal error.
#[derive(Debug, Clone)]
pub struct LzwDecoder;

impl Decoder for LzwDecoder {
    fn decode_tile(
        &self,
        buffer: Bytes,
        _photometric_interpretation: PhotometricInterpretation,
        _jpeg_tables: Option<&[u8]>,
        _samples_per_pixel: u16,
        _bits_per_sample: u16,
        _lerc_parameters: Option<&[u32]>,
    ) -> AsyncTiffResult<Vec<u8>> {
        super::lzw::decompress(&buffer)
            .map_err(|error| async_tiff::error::AsyncTiffError::General(error.to_string()))
    }
}

/// TIFF/JPEG decoder that emits pixels in the TIFF-declared component space.
/// A conventional JPEG decode turns YCbCr into RGB, which would make
/// `GeoTIFFImage::read_rgb` convert the bytes a second time. geotiff.js instead
/// returns upsampled Y/Cb/Cr components from `readRasters`; selecting the
/// output colour space from `PhotometricInterpretation` preserves that
/// contract while still supporting subsampled JPEG tiles.
#[derive(Debug, Clone)]
pub struct JpegDecoder;

impl Decoder for JpegDecoder {
    fn decode_tile(
        &self,
        buffer: Bytes,
        photometric_interpretation: PhotometricInterpretation,
        jpeg_tables: Option<&[u8]>,
        samples_per_pixel: u16,
        _bits_per_sample: u16,
        _lerc_parameters: Option<&[u32]>,
    ) -> AsyncTiffResult<Vec<u8>> {
        let data = if let Some(tables) = jpeg_tables {
            if tables.len() < 2 || buffer.len() < 2 {
                return Err(async_tiff::error::AsyncTiffError::General(
                    "Truncated TIFF JPEG tables or tile stream".to_string(),
                ));
            }
            let capacity = tables
                .len()
                .checked_add(buffer.len())
                .and_then(|value| value.checked_sub(4))
                .ok_or_else(|| {
                    async_tiff::error::AsyncTiffError::General(
                        "TIFF JPEG stream size overflow".to_string(),
                    )
                })?;
            let mut combined = Vec::new();
            combined.try_reserve_exact(capacity).map_err(|error| {
                async_tiff::error::AsyncTiffError::General(format!(
                    "Could not allocate TIFF JPEG stream: {error}"
                ))
            })?;
            combined.extend_from_slice(&tables[..tables.len() - 2]);
            combined.extend_from_slice(&buffer[2..]);
            combined
        } else {
            buffer.to_vec()
        };
        let color_space = match photometric_interpretation {
            PhotometricInterpretation::RGB => ColorSpace::RGB,
            PhotometricInterpretation::YCbCr => ColorSpace::YCbCr,
            PhotometricInterpretation::CMYK => ColorSpace::CMYK,
            PhotometricInterpretation::WhiteIsZero
            | PhotometricInterpretation::BlackIsZero
            | PhotometricInterpretation::TransparencyMask => ColorSpace::Luma,
            other => {
                return Err(AsyncTiffError::General(format!(
                    "JPEG compression is unsupported for {other:?} photometric interpretation"
                )));
            }
        };
        if color_space.num_components() != usize::from(samples_per_pixel) {
            return Err(AsyncTiffError::General(format!(
                "JPEG component count {} does not match SamplesPerPixel {samples_per_pixel}",
                color_space.num_components()
            )));
        }
        let options = DecoderOptions::default().jpeg_set_out_colorspace(color_space);
        let mut decoder = NativeJpegDecoder::new_with_options(ZCursor::new(data), options);
        decoder
            .decode()
            .map_err(|error| AsyncTiffError::General(error.to_string()))
    }
}

/// Builds the codec registry this port actually uses: async-tiff's default
/// set (Raw/Deflate/LZW/JPEG/Zstd/LERC/WebP/JPEG2k), with native replacements
/// for PackBits, LZW and JPEG. WebP replaces geotiff.js's browser-only
/// `createImageBitmap` route with platform-independent native decoding.
pub fn build_decoder_registry() -> DecoderRegistry {
    let mut registry = DecoderRegistry::default();
    registry
        .as_mut()
        .insert(Compression::PackBits, Box::new(PackBitsDecoder));
    registry
        .as_mut()
        .insert(Compression::LZW, Box::new(LzwDecoder));
    registry
        .as_mut()
        .insert(Compression::ModernJPEG, Box::new(JpegDecoder));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_includes_packbits_and_the_async_tiff_defaults() {
        let registry = build_decoder_registry();
        let map = registry.as_ref();
        assert!(map.contains_key(&Compression::PackBits));
        assert!(map.contains_key(&Compression::None));
        assert!(map.contains_key(&Compression::Deflate));
        assert!(map.contains_key(&Compression::LZW));
        assert!(map.contains_key(&Compression::ModernJPEG));
        assert!(map.contains_key(&Compression::ZSTD));
        assert!(map.contains_key(&Compression::LERC));
        assert!(map.contains_key(&Compression::WebP));
        assert!(map.contains_key(&Compression::JPEG2k));
    }

    #[test]
    fn public_registration_helpers_support_multiple_custom_cases() {
        let mut registry = DecoderRegistry::empty();
        add_decoder(
            &mut registry,
            [Compression::None, Compression::PackBits],
            || Box::new(PackBitsDecoder),
        );
        assert!(get_decoder(&registry, Compression::None).is_ok());
        assert!(get_decoder(&registry, Compression::PackBits).is_ok());
        assert!(find_decoder(&registry, Compression::None).is_some());
        assert!(find_decoder(&registry, Compression::PackBits).is_some());
    }

    #[test]
    fn public_lookup_retains_reference_error_contract() {
        let registry = build_decoder_registry();
        let message = |error| match error {
            AsyncTiffError::General(message) => message,
            other => other.to_string(),
        };
        assert_eq!(
            message(get_decoder(&registry, Compression::Unknown(64_000)).unwrap_err()),
            "Unknown compression method identifier: 64000"
        );
        assert_eq!(
            message(get_decoder(&registry, Compression::JPEG).unwrap_err()),
            "old style JPEG compression is not supported."
        );
    }

    #[test]
    fn packbits_adapter_matches_the_pure_algorithm() {
        let decoder = PackBitsDecoder;
        let input = Bytes::from_static(&[2, 10, 20, 30]);
        let out = decoder
            .decode_tile(
                input,
                PhotometricInterpretation::BlackIsZero,
                None,
                1,
                8,
                None,
            )
            .unwrap();
        assert_eq!(out, vec![10, 20, 30]);
    }

    #[test]
    fn lzw_adapter_matches_the_pure_algorithm() {
        let decoder = LzwDecoder;
        let input = Bytes::from_static(&[32, 144, 96, 68, 34, 20, 22, 2]);
        let out = decoder
            .decode_tile(
                input,
                PhotometricInterpretation::BlackIsZero,
                None,
                1,
                8,
                None,
            )
            .unwrap();
        assert_eq!(out, b"AAAABBBB");
    }
}
