//! Compression support for the complete geotiff.js decoder matrix. The
//! registry uses native Deflate, Zstd, LERC and WebP implementations; the
//! faithful LZW and PackBits ports are wired as real decoders, and the JPEG
//! adapter preserves the TIFF-declared component space. JPEG2000 is an
//! additional native capability supplied by async-tiff.

pub mod lzw;
pub mod packbits;
pub mod raw;
pub mod registry;
