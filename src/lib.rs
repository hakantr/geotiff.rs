#![doc = include_str!("../README.md")]

/// geotiff.js release whose observable behavior this crate ports.
pub const GEOTIFF_JS_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Exact geotiff.js source revision used by the live differential tests.
pub const GEOTIFF_JS_COMMIT: &str = "8594d1b4bde4072326916185c848e73a9e704850";

/// English version of the crate guide, included here so its Rust examples are
/// compiled as documentation tests alongside the Turkish crate-level guide.
#[doc = include_str!("../README_EN.md")]
pub mod english_readme {}

pub mod api;
mod block;
pub mod compression;
pub mod dataset;
pub mod dataslice;
pub mod dataview64;
pub mod decode_pool;
pub mod error;
pub mod geo;
pub mod geokeys;
pub mod geotiff;
pub mod geotiffimage;
pub mod globals;
pub mod imagefiledirectory;
pub mod logging;
pub mod pipeline;
pub mod predictor;
pub mod raster;
pub mod readrgb;
pub mod resample;
pub mod rgb;
pub mod source;
pub mod typed_array;
pub mod utils;
pub mod writer;

pub use api::{
    from_array_buffer, from_array_buffer_with_options, from_blob, from_blob_with_options,
    from_bytes, from_bytes_with_options, from_custom_client, from_custom_client_with_options,
    from_file, from_file_with_options, from_object, from_object_with_options, from_reader,
    from_reader_with_options, from_source, from_source_with_options, from_url,
    from_url_with_all_options, from_url_with_client, from_url_with_client_and_options,
    from_url_with_options, from_urls, from_urls_with_all_options, from_urls_with_options,
};
pub use async_tiff::decoder::{Decoder, DecoderRegistry};
pub use async_tiff::reader::{AsyncFileReader, Endianness};
pub use async_tiff::tags::{
    Compression, ExtraSamples, PhotometricInterpretation, PlanarConfiguration, Predictor,
    SampleFormat,
};
pub use compression::registry::{add_decoder, build_decoder_registry, find_decoder, get_decoder};
pub use dataset::{BestFitOptions, GeoTiffDataset, GeoTiffOptions, MultiGeoTiff, SingleGeoTiff};
pub use decode_pool::{CancellationToken, configure_decode_pool};
pub use geokeys::{GeoKeys, ParsedGeoKeyValue, geo_key_id, geo_key_name};
pub use geotiff::GeoTiffImageIndexError;
pub use geotiffimage::{
    FillValue, GeoTiffImage, RasterBlock, ReadRasterResult, ReadRastersOptions, ReadRgbOptions,
    SampleReader, SizeOrData, TiePoint,
};
pub use globals::{
    FieldType, TagDefinition, get_field_type_size, get_tag, register_tag, resolve_tag,
};
pub use imagefiledirectory::{FileDirectory, IfdEntry, IfdScalar, IfdValue};
pub use logging::{DummyLogger, Logger, set_logger};
pub use raster::{ImageWindow, PackedSampleMode, Raster, RasterBands};
pub use source::reader::{BlockedReader, HttpRangeReader, HttpSourceOptions, SourceSpec};
pub use typed_array::TypedArray;
pub use writer::{
    GeoKeyValue, WriterCompatibility, WriterData, WriterError, WriterMetadata, write_array_buffer,
    write_array_buffer_with_mode, write_geotiff, write_geotiff_with_mode,
};
