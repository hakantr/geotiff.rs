# Live geotiff.js differential tests

This directory drives the same fixtures and operations through the sibling
`geotiff.js` 3.1.0 repository and the native Rust port. The JavaScript result is
computed live; no copied golden output can silently become stale.

Run the metadata/raster/RGB/direct-block matrix with:

```sh
cargo test --offline --test differential_parity -- --ignored --nocapture
```

Run the pure-helper/module matrix with:

```sh
cargo test --offline --test helpers_differential -- --ignored --nocapture
```

Run the factory/source/cache/decoder/cancellation/writer matrix with:

```sh
cargo test --offline --test api_differential -- --ignored --nocapture
```

Run the malformed-input and public error-contract matrix with:

```sh
cargo test --offline --test errors_differential -- --ignored --nocapture
```

Run the pinned upstream `GeoTIFF/test-data` corpus matrix with:

```sh
git clone https://github.com/GeoTIFF/test-data.git /tmp/geotiff-test-data
git -C /tmp/geotiff-test-data checkout 8506204783ff26a6c49ed1f721e7e1635b2e43ee
unzip -o /tmp/geotiff-test-data/files/spam2005v3r2_harvested-area_wheat_total.tiff.zip \
  -d /tmp/geotiff-test-data/files
GEOTIFF_TEST_DATA_DIR=/tmp/geotiff-test-data \
  cargo test --offline --test test_data_differential -- --ignored --nocapture
```

The corpus checkout must be at the pinned commit and must contain the extracted
TIFF from its ZIP. The test intentionally does not copy third-party imagery into
this repository. See [TEST_DATA_REPORT.md](TEST_DATA_REPORT.md) for the exact
coverage, resource policy, provenance review, and latest result.

The default JavaScript repository is `../geotiff.js`. Set `GEOTIFF_JS_DIR` to
override it. Its `dist-module` build must already exist.

`cases.json` is both the case matrix and the explicit divergence ledger. Exact
typed-array results are compared by concrete array type, dimensions, length and
SHA-256 over canonical little-endian element bytes. Numeric tolerance is used
only for independently implemented lossy JPEG decoding, with per-case bounds.
Direct `getTileOrStrip()` results are compared as exact predictor-reversed
`ArrayBuffer` bytes. The helper matrix compares `DataView64`, `DataSlice`, every
resampling and RGB conversion export, predictor handling, utility/global/HTTP
helpers, tag registration, root/module logger delegation, and raw/PackBits/LZW
decoder behavior. The metadata matrix calls every `ImageFileDirectory` method,
including flat numerator/denominator indexing through `loadValueIndexed()`.

The API matrix compares both writer entry points byte-for-byte for all numeric
input families plus nested, multi-strip, tiled chunky/planar, tiled Float64 and
zero-byte-count/nodata cases. Every generated TIFF is then opened again in its
own runtime and its raster is compared. Synthetic uncompressed files also drive
WhiteIsZero, CMYK, CIELab and RGBA alpha through the complete `readRGB()`
dispatcher, while `Pool(0)` exercises the reference's inline decoder path.

Known data-preserving divergences are asserted rather than hidden:

- geotiff.js 3.1.0 skips alternating multi-value TIFF `RATIONAL` pairs and
  leaves zeros; Rust retains every numerator/denominator pair.
- geotiff.js `ImageFileDirectory.toObject()` omits values that remain backed by
  `DeferredArray`; the native parser eagerly exposes the complete directory.
- geotiff.js 3.1.0's `DataSlice.readInt64()` fails to subtract a non-zero slice
  offset; Rust handles the absolute offset consistently with every other
  `DataSlice` reader.
- geotiff.js 3.1.0's multipart byte-range parser starts payloads at the boundary
  CRLF, returning two CRLF bytes and truncating the actual payload; Rust returns
  the exact `Content-Range` body.
- geotiff.js 3.1.0's writer omits signed typed arrays from its serialization
  type map, allocates eight bytes per value and writes only each value's low
  byte. Rust defaults to a lossless signed payload and offers
  `WriterCompatibility::GeotiffJs` for exact legacy wire output.
- geotiff.js recursively reports index 1 for any missing image requested
  beyond the first absent IFD; Rust's typed error retains the requested index.
- geotiff.js accepts an out-of-range sample label in chunky
  `getTileOrStrip`; Rust rejects the inconsistent block request and also
  validates tile coordinates before entering the decoder dependency.
- malformed empty writer input and a non-zero BigTIFF reserved header field
  are rejected with stable native validation errors instead of copying the
  reference's incidental TypeError/spec omission.
- abstract source operations and invalid dynamic writer metadata types are
  unrepresentable through Rust's required reader trait and typed writer maps.
- Node.js cannot execute geotiff.js's browser-only WebP `createImageBitmap`
  path; Rust must still decode the same WebP fixtures natively. Browser parity
  must be covered by the separate browser matrix.

Adding a divergence requires a named classification and rationale in
`cases.json`. Unexpected differences fail the test.
