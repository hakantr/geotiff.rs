//! Native source layer corresponding to geotiff.js `BaseSource` and its
//! memory, file, remote, custom-client and blocked variants. All backends
//! converge on `AsyncFileReader`; HTTP range/multipart behavior and both
//! aligned-block and compressed-range caches remain source-transparent.

pub mod block_cache;
pub mod blockedsource;
pub mod httputils;
pub mod metadata_compat;
pub mod reader;
