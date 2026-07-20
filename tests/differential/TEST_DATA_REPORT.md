# GeoTIFF/test-data differential report

This report records the live comparison against the upstream
[`GeoTIFF/test-data`](https://github.com/GeoTIFF/test-data) repository. The
fixtures are not vendored here: both implementations read the same external
checkout during the test.

## Reference and result

- Corpus commit: `8506204783ff26a6c49ed1f721e7e1635b2e43ee`
- geotiff.js version: `3.1.0`
- geotiff.js commit: `8594d1b4bde4072326916185c848e73a9e704850`
- Verification date: 2026-07-20
- Result: 22/22 TIFF files and 32/32 images matched; no open, raster, block, or
  RGB operation failed in either implementation
- Compared input: 79,668,618 bytes (21 direct TIFF files plus the TIFF extracted
  from the repository's ZIP)

The matrix contains two BigTIFF files, 21 little-endian files and one big-endian
file. Across all IFDs it exercises 17 tiled and 15 stripped images; unsigned,
signed, and floating-point samples at 8, 16, and 32 bits; uncompressed, LZW,
Deflate, and PackBits compression; predictors 1 and 3; and BlackIsZero, RGB, and
palette photometric interpretations. This particular corpus contains only
chunky planar configuration; separate-planar coverage remains in the committed
synthetic/fixture differential matrices.

## Operations compared

For every file, the JavaScript oracle and Rust test independently perform and
compare:

- source byte length/hash, open result, byte order, Classic TIFF/BigTIFF mode,
  image count, GDAL COG ghost metadata, and dataset best-fit reading;
- every IFD and every materialized tag value, including typed-array identity,
  length, edge values, and SHA-256 over canonical little-endian element bytes;
- dimensions, tiling, planar configuration, bits/sample, sample format,
  GeoKeys, tie points, GDAL metadata/NoData, origin, resolution,
  PixelIsArea, and normal/tile-grid bounding boxes;
- band-separated and interleaved raster reads, first/last sample selection,
  top-left/center/bottom-right windows, out-of-bounds fill, and nearest/bilinear
  resampling;
- first, middle, and last tile/strip blocks for the first and last sample;
- interleaved/band RGB conversion and alpha-enabled conversion when an
  `ExtraSamples` tag exists.

Typed raster and block results must have exactly the same concrete array type,
shape, dimensions, byte length, edge values, and byte hash. Float32 and Float64
edges are represented by their raw bits, so JSON decimal formatting cannot hide
or invent a one-ULP difference.

## Large-image policy

Twenty-nine images are read completely in both runtimes. Three images exceed
the explicit 4,000,000-sample full-read threshold:

- `abetow-ERD2018-EBIRD_SCIENCE-20191109-a5cf4cb2_hr_2018_abundance_median.tiff`
  (7,074 x 5,630 x 52; 2,070,984,240 samples)
- `lcv_landuse.cropland_hyde_p_10km_s0..0cm_2016_v3.2.tif`
  (4,320 x 1,792; 7,741,440 samples)
- `spam2005v3r2_harvested-area_wheat_total.tiff`
  (4,320 x 1,853; 8,004,960 samples)

Allocating the first image in full would require several gigabytes per runtime.
These three are therefore explicitly classified as `sampledLargeImage`; they
still receive complete metadata/IFD comparison plus all window, resampling,
block, RGB, and best-fit operations above. The threshold and classification are
part of the compared oracle output, so coverage cannot silently shrink.

## Finding closed by the corpus

The initial run exposed one genuine compatibility defect: the corpus stores
GDAL NoData as `-inf`. geotiff.js applies ECMAScript `Number("-inf")` and returns
`NaN`, while Rust's native float parser accepts `-inf` as negative infinity.
`parse_js_number` now rejects Rust-only infinity aliases, with a unit regression
test, and the full corpus passes after that correction.

## Provenance and licensing review

The upstream README attributes files to NSIDC, OSGEO, USDA GADAS, the Australian
Antarctic Program, OpenLandMap, Umbra Space, MapSPAM, Global Fishing Watch,
Digital Earth Australia, TIM-Online, NASA, and other linked sources. It
specifically identifies the clipped Umbra image as CC BY 4.0. The repository
root at the pinned commit has a README but no single corpus-wide license file;
therefore this project references an external checkout and does not redistribute
the data. Anyone redistributing individual fixtures must review the provenance
and terms of the corresponding upstream source.
