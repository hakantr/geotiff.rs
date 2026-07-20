# Test fixtures

`tiled-gray-i1.tif` — a tiny (614-byte) tiled, 1-bit-per-sample grayscale
TIFF (37x51 px, 3x4 tiles of 16x16), copied from the
[`async-tiff`](https://github.com/developmentseed/async-tiff) crate's own
bundled test fixtures (`fixtures/image-tiff/tiled-gray-i1.tif`, MIT
licensed), itself reused from the `image-tiff` Rust crate's long-standing
test corpus. Used by `src/pipeline.rs`'s integration tests to exercise the
real `SourceSpec`/`open_tiff`/`fetch_and_decode_tile` path end-to-end
against an actual TIFF file, not a mock.

The following fixtures come from the same MIT-licensed `async-tiff`
`fixtures/image-tiff` corpus (originally the `image-tiff`/`tiff` crate test
corpus). They are checked against geotiff.js v3.1.0 byte-for-byte, using a
64-bit FNV-1a digest of each returned typed array:

- `planar-rgb-u8.tif`: planar LZW RGB; band hashes
  `62aee77d48cef8bd`, `2d7d42f4bdf36a7f`, `28caeb0f5e7dd793`.
- `palette-1c-4b.tiff`: packed 4-bit rows; hash `76e9caebbbaa50b4`.
- `palette-1c-1b.tiff`: packed 1-bit rows whose width requires per-row
  padding; hash `175fe6c9e17a8e2a`.
- `12bit.cropped.tiff`: packed 12-bit unsigned samples widened to `u16`;
  hash `abb1d0b45561ad60`.
- `no_rows_per_strip.tiff`: valid stripped RGB with the TIFF-default
  `RowsPerStrip`; band hashes `36c38e198c9bd129`, `bb2dbcab8467d108`,
  `1dc60ce7b00ea2d4`.
- `predictor-3-gray-f32.tif` and `predictor-3-rgb-f32.tif`: LZW-compressed
  floating-point Predictor=3 fixtures; hashes `7d7fca3568c2e7f1` and
  `204f7da1687341a5`, `d7804dc027f9f7f5`, `c31c67b2bfcd1c85`.
- `random-fp16{,-pred2,-pred3}.tiff`: raw, horizontal-predictor, and
  floating-predictor Float16 variants; all widen to the same f32 hash
  `da13f48afce7dcca`.
- `int8_rgb.tif`, `int16_rgb.tif`: signed sample coverage; hashes are
  recorded in `tests/js_parity.rs`.
- `int16_zstd.tif`: signed Zstandard coverage, hash `22321f4e69928bc6`.
- `issue_69_packbits.tiff`: PackBits compatibility coverage, hash
  `9593768b97e5c8c8`.
- `minisblack-2c-8b-alpha.tiff`: planar PackBits gray+alpha coverage;
  hashes `f6f0532030b93362`, `1d2eb79bef8d9633`.
- `12bit.cropped.rgb.tiff`: chunky 12-bit RGB regression from async-tiff's
  image-tiff corpus. The three source planes are identical and remain exact
  in the default lossless mode (`abb1d0b45561ad60` each). Explicit
  `PackedSampleMode::GeotiffJs` reproduces v3.1.0's fractional-byte-offset
  quirk (`abb1d0b45561ad60`, `b01d966cc9be977e`, `b01d966cc9be977e`).
- `float32_1band_lerc_{block32,deflate_block32,zstd_block32}.tif`: the same
  Float32 gradient encoded as raw LERC, LERC+Deflate and LERC+Zstd. All
  three match geotiff.js v3.1.0 exactly (`e717d69dd8ea1215`).
- `uint8_{rgb,rgba}_webp_block64_cog.tif`: lossless WebP COG fixtures from
  the MIT/Apache-2.0 `developmentseed/geotiff-test-data` corpus. They cover
  RGB and alpha expansion in native Rust; geotiff.js uses its browser
  `createImageBitmap` path for the same compression and cannot execute this
  decoder under Node.
- `tiled-jpeg-rgb-u8.tif`: native JPEG decoder coverage, compared against
  the lossless `planar-rgb-u8.tif` source with a bounded per-channel error
  (independent JPEG IDCT implementations need not round every pixel in the
  same direction).
- `tiled-jpeg-ycbcr.tif`: striped JPEG/YCbCr with 4:2:0 chroma subsampling;
  verifies raw-component upsampling and `readRGB` without a native decoder
  panic. Its RGB result is compared with the same lossless source and the
  tolerance is calibrated against a live geotiff.js v3.1.0 run.
