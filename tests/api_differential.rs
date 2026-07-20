use async_tiff::decoder::{Decoder, DecoderRegistry};
use async_tiff::error::AsyncTiffResult;
use async_tiff::reader::AsyncFileReader;
use async_tiff::tags::{Compression, PhotometricInterpretation};
use async_trait::async_trait;
use bytes::Bytes;
use geotiff::compression::registry::{add_decoder, build_decoder_registry, get_decoder};
use geotiff::dataset::{GeoTiffOptions, SingleGeoTiff};
use geotiff::decode_pool::CancellationToken;
use geotiff::geotiffimage::{ReadRasterResult, ReadRastersOptions, ReadRgbOptions};
use geotiff::imagefiledirectory::IfdValue;
use geotiff::source::reader::{BlockedReader, HttpSourceOptions};
use geotiff::typed_array::TypedArray;
use geotiff::writer::{
    self, WriterCompatibility, WriterData, WriterMetadata, write_array_buffer,
    write_array_buffer_with_mode, write_geotiff_with_mode,
};
use geotiff::{
    from_array_buffer, from_blob, from_custom_client, from_file, from_reader_with_options,
    from_url_with_options, from_urls_with_options,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::ops::Range;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    root().join("tests/fixtures").join(name)
}

fn run_js_oracle() -> Value {
    let js_root = std::env::var_os("GEOTIFF_JS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join("../geotiff.js"));
    let output = Command::new("node")
        .args([
            root()
                .join("tests/differential/js_api_oracle.mjs")
                .as_os_str(),
            js_root.as_os_str(),
            root().join("tests/fixtures").as_os_str(),
        ])
        .output()
        .expect("run geotiff.js API oracle");
    assert!(
        output.status.success(),
        "geotiff.js API oracle failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse API oracle JSON")
}

fn assert_section(name: &str, js: &Value, rust: &Value) {
    assert_eq!(
        js,
        rust,
        "API differential mismatch in {name}\nJS:\n{}\nRust:\n{}",
        serde_json::to_string_pretty(js).unwrap(),
        serde_json::to_string_pretty(rust).unwrap(),
    );
}

fn assert_api_divergence_ledger() {
    let cases: Value = serde_json::from_slice(
        &std::fs::read(root().join("tests/differential/cases.json")).unwrap(),
    )
    .unwrap();
    let ids = cases["apiDivergences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert!(ids.contains("geotiffjs-signed-writer-payload"));
}

fn typed_name(array: &TypedArray) -> &'static str {
    match array {
        TypedArray::Int8(_) => "Int8Array",
        TypedArray::Uint8(_) => "Uint8Array",
        TypedArray::Uint8Clamped(_) => "Uint8ClampedArray",
        TypedArray::Int16(_) => "Int16Array",
        TypedArray::Uint16(_) => "Uint16Array",
        TypedArray::Int32(_) => "Int32Array",
        TypedArray::Uint32(_) => "Uint32Array",
        TypedArray::Int64(_) => "BigInt64Array",
        TypedArray::Uint64(_) => "BigUint64Array",
        TypedArray::Float32(_) => "Float32Array",
        TypedArray::Float64(_) => "Float64Array",
    }
}

fn typed_bytes(array: &TypedArray) -> Vec<u8> {
    let mut bytes = Vec::new();
    macro_rules! extend {
        ($values:expr) => {
            for value in $values {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        };
    }
    match array {
        TypedArray::Int8(values) => bytes.extend(values.iter().map(|value| *value as u8)),
        TypedArray::Uint8(values) | TypedArray::Uint8Clamped(values) => {
            bytes.extend_from_slice(values)
        }
        TypedArray::Int16(values) => extend!(values),
        TypedArray::Uint16(values) => extend!(values),
        TypedArray::Int32(values) => extend!(values),
        TypedArray::Uint32(values) => extend!(values),
        TypedArray::Int64(values) => extend!(values),
        TypedArray::Uint64(values) => extend!(values),
        TypedArray::Float32(values) => extend!(values),
        TypedArray::Float64(values) => extend!(values),
    }
    bytes
}

fn bytes_summary(bytes: &[u8]) -> Value {
    json!({
        "length": bytes.len(),
        "sha256": format!("{:x}", Sha256::digest(bytes)),
    })
}

fn raster_summary(result: ReadRasterResult) -> Value {
    let ReadRasterResult::Interleaved(raster) = result else {
        panic!("expected interleaved raster")
    };
    let bytes = typed_bytes(&raster.data);
    json!({
        "type": typed_name(&raster.data),
        "width": raster.width,
        "height": raster.height,
        "length": bytes.len(),
        "sha256": format!("{:x}", Sha256::digest(&bytes)),
    })
}

async fn image_raster_summary(dataset: &SingleGeoTiff) -> Value {
    raster_summary(
        dataset
            .image(0)
            .unwrap()
            .read_rasters(ReadRastersOptions {
                interleave: true,
                ..ReadRastersOptions::default()
            })
            .await
            .unwrap(),
    )
}

async fn summarize_single(dataset: &SingleGeoTiff) -> Value {
    let image = dataset.image(0).unwrap();
    let raster = image_raster_summary(dataset).await;
    let slice = dataset.get_slice(0, Some(8)).await.unwrap();
    let directory = dataset.request_ifd(0).unwrap();
    let width = directory
        .get_value("ImageWidth")
        .and_then(IfdValue::as_u64)
        .unwrap();
    json!({
        "imageCount": dataset.image_count(),
        "width": image.width(),
        "height": image.height(),
        "samplesPerPixel": image.samples_per_pixel(),
        "raster": raster,
        "slice": slice.buffer(),
        "sliceOffset": slice.slice_offset(),
        "requestIfdWidth": width,
        "ghostValues": dataset.ghost_values().await.unwrap(),
    })
}

#[derive(Debug)]
struct RecordingReader {
    bytes: Bytes,
    ranges: Arc<Mutex<Vec<Range<u64>>>>,
    delay: Option<Duration>,
}

impl RecordingReader {
    fn immediate(bytes: Bytes, ranges: Arc<Mutex<Vec<Range<u64>>>>) -> Self {
        Self {
            bytes,
            ranges,
            delay: None,
        }
    }
}

#[async_trait]
impl AsyncFileReader for RecordingReader {
    async fn get_bytes(&self, range: Range<u64>) -> AsyncTiffResult<Bytes> {
        self.ranges.lock().unwrap().push(range.clone());
        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }
        let start = usize::try_from(range.start)
            .unwrap_or(usize::MAX)
            .min(self.bytes.len());
        let end = usize::try_from(range.end)
            .unwrap_or(usize::MAX)
            .min(self.bytes.len());
        Ok(if start < end {
            self.bytes.slice(start..end)
        } else {
            Bytes::new()
        })
    }
}

#[derive(Debug, Clone)]
struct RequestFact {
    path: String,
    range: Option<String>,
    oracle_header: Option<String>,
}

struct TestHttpServer {
    base_url: String,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<RequestFact>>>,
    thread: Option<thread::JoinHandle<()>>,
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<String> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut request = Vec::new();
    let mut buffer = [0u8; 2048];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.len() > 64 * 1024 {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&request).into_owned())
}

fn request_parts(request: &str) -> (String, Option<String>, Option<String>) {
    let mut lines = request.split("\r\n");
    let path = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();
    let mut range = None;
    let mut oracle = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "range" => range = Some(value.trim().to_string()),
            "x-oracle" => oracle = Some(value.trim().to_string()),
            _ => {}
        }
    }
    (path, range, oracle)
}

fn parse_single_range(raw: &str, size: usize) -> Option<(usize, usize)> {
    let value = raw.strip_prefix("bytes=")?;
    let (start, end) = value.split_once('-')?;
    let start = start.parse::<usize>().ok()?;
    let requested_end = end.parse::<usize>().ok()?;
    (start < size).then_some((start, requested_end.min(size - 1)))
}

fn write_response(
    stream: &mut TcpStream,
    status: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
    for (name, value) in headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    stream.shutdown(Shutdown::Both).ok();
    Ok(())
}

impl TestHttpServer {
    fn start(main: Vec<u8>, overview: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let thread_stop = stop.clone();
        let thread_requests = requests.clone();
        let handle = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(_) => break,
                };
                let Ok(request) = read_request(&mut stream) else {
                    continue;
                };
                let (path, range, oracle_header) = request_parts(&request);
                thread_requests.lock().unwrap().push(RequestFact {
                    path: path.clone(),
                    range: range.clone(),
                    oracle_header,
                });
                let bytes = if path.contains("overview") {
                    &overview
                } else {
                    &main
                };
                if path.contains("full") {
                    write_response(
                        &mut stream,
                        "200 OK",
                        &[("Content-Type", "application/octet-stream".to_string())],
                        bytes,
                    )
                    .ok();
                    continue;
                }
                let Some((start, end)) = range
                    .as_deref()
                    .and_then(|value| parse_single_range(value, bytes.len()))
                else {
                    write_response(
                        &mut stream,
                        "416 Range Not Satisfiable",
                        &[("Content-Range", format!("bytes */{}", bytes.len()))],
                        &[],
                    )
                    .ok();
                    continue;
                };
                write_response(
                    &mut stream,
                    "206 Partial Content",
                    &[
                        ("Content-Type", "application/octet-stream".to_string()),
                        (
                            "Content-Range",
                            format!("bytes {start}-{end}/{}", bytes.len()),
                        ),
                    ],
                    &bytes[start..=end],
                )
                .ok();
            }
        });
        Self {
            base_url: format!("http://{address}"),
            stop,
            requests,
            thread: Some(handle),
        }
    }
}

impl Drop for TestHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        TcpStream::connect(self.base_url.trim_start_matches("http://")).ok();
        if let Some(handle) = self.thread.take() {
            handle.join().ok();
        }
    }
}

async fn rust_factory_cases(main: Bytes, overview: Vec<u8>) -> Value {
    let array = from_array_buffer(main.clone()).await.unwrap();
    let array_summary = summarize_single(&array).await;
    array.close();

    let file = from_file(fixture("tiled-gray-i1.tif")).await.unwrap();
    let file_summary = summarize_single(&file).await;
    file.close();

    let blob = from_blob(main.clone()).await.unwrap();
    let blob_summary = summarize_single(&blob).await;
    blob.close();

    let custom_ranges = Arc::new(Mutex::new(Vec::new()));
    let custom = from_custom_client(Arc::new(RecordingReader::immediate(
        main.clone(),
        custom_ranges.clone(),
    )))
    .await
    .unwrap();
    let custom_summary = summarize_single(&custom).await;
    custom.close();

    let source = from_reader_with_options(
        Arc::new(RecordingReader::immediate(
            main.clone(),
            Arc::new(Mutex::new(Vec::new())),
        )),
        GeoTiffOptions {
            cache: true,
            ..GeoTiffOptions::default()
        },
    )
    .await
    .unwrap();
    let source_summary = summarize_single(&source).await;
    source.close();

    let server = TestHttpServer::start(main.to_vec(), overview);
    let mut unblocked = HttpSourceOptions {
        block_size: None,
        ..HttpSourceOptions::default()
    };
    unblocked
        .headers
        .insert("x-oracle", "present".parse().unwrap());
    let remote = from_url_with_options(format!("{}/main", server.base_url), unblocked)
        .await
        .unwrap();
    let url_summary = summarize_single(&remote).await;
    remote.close();

    let rejected = from_url_with_options(
        format!("{}/full", server.base_url),
        HttpSourceOptions::default(),
    )
    .await;
    assert!(
        rejected.is_err(),
        "full response must be rejected by default"
    );

    let allow_full = HttpSourceOptions {
        allow_full_file: true,
        ..HttpSourceOptions::default()
    };
    let allowed = from_url_with_options(format!("{}/full", server.base_url), allow_full)
        .await
        .unwrap();
    let full_allowed = summarize_single(&allowed).await;
    allowed.close();

    let multi_options = HttpSourceOptions {
        block_size: None,
        ..HttpSourceOptions::default()
    };
    let multi = from_urls_with_options(
        format!("{}/main-multi", server.base_url),
        [format!("{}/overview", server.base_url)],
        multi_options,
    )
    .await
    .unwrap();
    let directories = multi.parse_file_directories_per_file().unwrap();
    let directory_widths = directories
        .iter()
        .map(|directory| {
            directory
                .get_value("ImageWidth")
                .and_then(IfdValue::as_u64)
                .unwrap()
        })
        .collect::<Vec<_>>();
    let first = multi.image(0).unwrap();
    let second = multi.image(1).unwrap();
    let second_raster = raster_summary(
        second
            .read_rasters(ReadRastersOptions {
                interleave: true,
                ..ReadRastersOptions::default()
            })
            .await
            .unwrap(),
    );
    let multi_summary = json!({
        "imageCount": multi.image_count(),
        "directoryWidths": directory_widths,
        "imageWidths": [first.width(), second.width()],
        "imageHeights": [first.height(), second.height()],
        "secondRaster": second_raster,
    });
    multi.close();

    let requests = server.requests.lock().unwrap().clone();
    let request_facts = json!({
        "customHeaderSeen": requests.iter().any(|request| request.oracle_header.as_deref() == Some("present")),
        "allReadsRangedExceptFullEndpoint": requests.iter()
            .filter(|request| !request.path.contains("full"))
            .all(|request| request.range.is_some()),
    });

    assert!(!custom_ranges.lock().unwrap().is_empty());
    json!({
        "arrayBuffer": array_summary,
        "file": file_summary,
        "fileClose": { "$undefined": true },
        "blob": blob_summary,
        "customClient": custom_summary,
        "fromSource": source_summary,
        "url": url_summary,
        "fullAllowed": full_allowed,
        "multi": multi_summary,
        "httpRequestFacts": request_facts,
    })
}

async fn rust_source_cases() -> Value {
    let ranges = Arc::new(Mutex::new(Vec::new()));
    let inner: Arc<dyn AsyncFileReader> = Arc::new(RecordingReader::immediate(
        Bytes::from_static(b"0123456789abcdef"),
        ranges.clone(),
    ));
    let blocked = BlockedReader::new(inner, 4, 8).unwrap();
    let first = blocked.get_bytes(3..10).await.unwrap();
    let request_count = ranges.lock().unwrap().len();
    let second = blocked.get_bytes(4..6).await.unwrap();
    assert_eq!(ranges.lock().unwrap().len(), request_count);
    assert!(
        ranges
            .lock()
            .unwrap()
            .iter()
            .all(|range| range.start.is_multiple_of(4) && range.end - range.start == 4)
    );
    json!({
        "first": first.to_vec(),
        "second": second.to_vec(),
    })
}

#[derive(Debug)]
struct CountingRawDecoder(Arc<AtomicUsize>);

impl Decoder for CountingRawDecoder {
    fn decode_tile(
        &self,
        buffer: Bytes,
        _photometric_interpretation: PhotometricInterpretation,
        _jpeg_tables: Option<&[u8]>,
        _samples_per_pixel: u16,
        _bits_per_sample: u16,
        _lerc_parameters: Option<&[u32]>,
    ) -> AsyncTiffResult<Vec<u8>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(buffer.to_vec())
    }
}

#[derive(Debug)]
struct ParameterDecoder {
    marker: u8,
}

impl Decoder for ParameterDecoder {
    fn decode_tile(
        &self,
        buffer: Bytes,
        _photometric_interpretation: PhotometricInterpretation,
        _jpeg_tables: Option<&[u8]>,
        _samples_per_pixel: u16,
        _bits_per_sample: u16,
        _lerc_parameters: Option<&[u32]>,
    ) -> AsyncTiffResult<Vec<u8>> {
        let mut output = buffer.to_vec();
        if let Some(first) = output.first_mut() {
            *first = first.wrapping_add(self.marker);
        }
        Ok(output)
    }
}

async fn cache_run(bytes: Bytes, cache: bool) -> Value {
    let count = Arc::new(AtomicUsize::new(0));
    let mut registry = build_decoder_registry();
    registry.as_mut().insert(
        Compression::None,
        Box::new(CountingRawDecoder(count.clone())),
    );
    let dataset = SingleGeoTiff::open_with_options(
        Arc::new(RecordingReader::immediate(
            bytes,
            Arc::new(Mutex::new(Vec::new())),
        )),
        GeoTiffOptions {
            cache,
            decoder_registry: Arc::new(registry),
            ..GeoTiffOptions::default()
        },
    )
    .await
    .unwrap();
    let first = image_raster_summary(&dataset).await;
    let first_count = count.load(Ordering::SeqCst);
    let second = image_raster_summary(&dataset).await;
    let second_count = count.load(Ordering::SeqCst);
    json!({
        "firstCount": first_count,
        "secondCount": second_count,
        "first": first,
        "second": second,
    })
}

fn with_short_tag(mut bytes: Vec<u8>, tag: u16, value: u16) -> Vec<u8> {
    let little = &bytes[0..2] == b"II";
    let read_u16 = |bytes: &[u8]| {
        if little {
            u16::from_le_bytes(bytes.try_into().unwrap())
        } else {
            u16::from_be_bytes(bytes.try_into().unwrap())
        }
    };
    let read_u32 = |bytes: &[u8]| {
        if little {
            u32::from_le_bytes(bytes.try_into().unwrap())
        } else {
            u32::from_be_bytes(bytes.try_into().unwrap())
        }
    };
    let ifd = read_u32(&bytes[4..8]) as usize;
    let count = read_u16(&bytes[ifd..ifd + 2]) as usize;
    for index in 0..count {
        let entry = ifd + 2 + index * 12;
        if read_u16(&bytes[entry..entry + 2]) == tag {
            let encoded = if little {
                value.to_le_bytes()
            } else {
                value.to_be_bytes()
            };
            bytes[entry + 8..entry + 10].copy_from_slice(&encoded);
            return bytes;
        }
    }
    panic!("SHORT tag {tag} not found")
}

fn with_compression(bytes: Vec<u8>, compression: u16) -> Vec<u8> {
    with_short_tag(bytes, writer::tag::COMPRESSION, compression)
}

async fn rust_decoder_and_cache_cases(main: Bytes) -> Value {
    let uncached = cache_run(main.clone(), false).await;
    let cached = cache_run(main.clone(), true).await;

    let mut registry = DecoderRegistry::empty();
    let compression = Compression::from_u16_exhaustive(65000);
    add_decoder(&mut registry, [compression], || {
        Box::new(ParameterDecoder { marker: 37 })
    });
    let decoder = get_decoder(&registry, compression).unwrap();
    let output = decoder
        .decode_tile(
            Bytes::from_static(&[1, 2, 3]),
            PhotometricInterpretation::BlackIsZero,
            None,
            1,
            8,
            None,
        )
        .unwrap();

    let custom_dataset = SingleGeoTiff::open_with_options(
        Arc::new(RecordingReader::immediate(
            Bytes::from(with_compression(main.to_vec(), 65000)),
            Arc::new(Mutex::new(Vec::new())),
        )),
        GeoTiffOptions {
            decoder_registry: Arc::new(registry),
            ..GeoTiffOptions::default()
        },
    )
    .await
    .unwrap();
    let custom_raster = image_raster_summary(&custom_dataset).await;

    // `Pool(0)` is geotiff.js's inline decoder path. The native equivalent
    // is a direct registry lookup/decode; worker-backed image reads exercise
    // the dedicated Rayon pool elsewhere in this same matrix.
    let inline_registry = build_decoder_registry();
    let inline_decoder = get_decoder(&inline_registry, Compression::None).unwrap();
    let inline_output = inline_decoder
        .decode_tile(
            Bytes::from_static(&[9, 8, 7, 6]),
            PhotometricInterpretation::BlackIsZero,
            None,
            1,
            8,
            None,
        )
        .unwrap();

    json!({
        "uncached": uncached,
        "cached": cached,
        "custom": {
            "parameters": { "marker": 37 },
            "output": output,
            "raster": custom_raster,
        },
        "inlinePool0": {
            "output": inline_output,
            "firstDestroy": { "$undefined": true },
            "secondDestroy": { "$undefined": true },
        },
    })
}

async fn assert_cancellation(js: &Value, main: Bytes) {
    assert_eq!(
        js["preCancelled"]["error"]["name"], "AbortError",
        "pre-aborted JS source must reject as AbortError"
    );
    assert!(js["inFlightCancelled"].get("error").is_some());
    assert_eq!(js["inFlightRequests"], 1);

    let pre_ranges = Arc::new(Mutex::new(Vec::new()));
    let pre_token = CancellationToken::new();
    pre_token.cancel();
    let pre_result = from_reader_with_options(
        Arc::new(RecordingReader::immediate(main.clone(), pre_ranges.clone())),
        GeoTiffOptions {
            cancellation: Some(pre_token),
            ..GeoTiffOptions::default()
        },
    )
    .await;
    assert!(pre_result.is_err());
    assert!(pre_ranges.lock().unwrap().is_empty());

    let ranges = Arc::new(Mutex::new(Vec::new()));
    let token = CancellationToken::new();
    let cancel = token.clone();
    let reader: Arc<dyn AsyncFileReader> = Arc::new(RecordingReader {
        bytes: main,
        ranges: ranges.clone(),
        delay: Some(Duration::from_millis(100)),
    });
    let pending = tokio::spawn(async move {
        from_reader_with_options(
            reader,
            GeoTiffOptions {
                cancellation: Some(token),
                ..GeoTiffOptions::default()
            },
        )
        .await
        .map(|_| ())
    });
    tokio::time::sleep(Duration::from_millis(5)).await;
    cancel.cancel();
    let result = pending.await.unwrap();
    assert!(result.is_err());
    assert_eq!(ranges.lock().unwrap().len(), 1);
}

fn writer_case(root: Vec<u8>, direct: Vec<u8>) -> Value {
    json!({
        "root": bytes_summary(&root),
        "direct": bytes_summary(&direct),
        "header": &root[..8],
    })
}

fn rich_metadata() -> WriterMetadata {
    WriterMetadata::new(2, 2)
        .with_tag(writer::tag::GDAL_NODATA, "-9999\0")
        .with_tag(274, 3u16)
        .with_geo_key(writer::geo_key::GEOGRAPHIC_TYPE, 4326u16)
        .with_geo_key(writer::geo_key::GEOG_CITATION, "X")
        .with_geo_key(writer::geo_key::GT_RASTER_TYPE, 1u16)
}

fn tiled_metadata() -> WriterMetadata {
    WriterMetadata::new(3, 3)
        .with_tag(writer::tag::SAMPLES_PER_PIXEL, 3u16)
        .with_tag(writer::tag::TILE_WIDTH, 3u16)
        .with_tag(writer::tag::TILE_LENGTH, 3u16)
        .with_tag(writer::tag::TILE_BYTE_COUNTS, vec![27u32])
}

fn multi_strip_metadata() -> WriterMetadata {
    WriterMetadata::new(3, 2)
        .with_tag(writer::tag::ROWS_PER_STRIP, 1u16)
        .with_tag(writer::tag::STRIP_BYTE_COUNTS, vec![3u32, 3])
}

fn tiled_edge_metadata(planar: bool, bytes_per_tile: u32) -> WriterMetadata {
    WriterMetadata::new(3, 3)
        .with_tag(writer::tag::SAMPLES_PER_PIXEL, 3u16)
        .with_tag(
            writer::tag::PLANAR_CONFIGURATION,
            if planar { 2u16 } else { 1u16 },
        )
        .with_tag(writer::tag::TILE_WIDTH, 2u16)
        .with_tag(writer::tag::TILE_LENGTH, 2u16)
        .with_tag(
            writer::tag::TILE_BYTE_COUNTS,
            vec![bytes_per_tile; if planar { 12 } else { 4 }],
        )
}

fn tiled_edge_inputs() -> (Vec<u8>, Vec<u8>, Vec<f64>) {
    let pixels = [
        [255, 2, 3],
        [255, 2, 3],
        [255, 2, 3],
        [1, 255, 3],
        [1, 255, 3],
        [1, 255, 3],
        [1, 2, 255],
        [1, 2, 255],
        [1, 2, 255],
    ];
    let interleaved_source = pixels.into_iter().flatten().collect::<Vec<_>>();
    let mut interleaved = Vec::with_capacity(48);
    for tile_y in 0..2 {
        for tile_x in 0..2 {
            for y in 0..2 {
                for x in 0..2 {
                    let source_x = tile_x * 2 + x;
                    let source_y = tile_y * 2 + y;
                    if source_x < 3 && source_y < 3 {
                        interleaved.extend_from_slice(&pixels[source_y * 3 + source_x]);
                    } else {
                        interleaved.extend_from_slice(&[0, 0, 0]);
                    }
                }
            }
        }
    }

    let bands = [
        [[255, 255, 255], [1, 1, 1], [1, 1, 1]],
        [[2, 2, 2], [255, 255, 255], [2, 2, 2]],
        [[3, 3, 3], [3, 3, 3], [255, 255, 255]],
    ];
    let mut planar = Vec::with_capacity(48);
    for band in bands {
        for tile_y in 0..2 {
            for tile_x in 0..2 {
                for y in 0..2 {
                    for x in 0..2 {
                        let source_x = tile_x * 2 + x;
                        let source_y = tile_y * 2 + y;
                        planar.push(if source_x < 3 && source_y < 3 {
                            band[source_y][source_x]
                        } else {
                            0
                        });
                    }
                }
            }
        }
    }

    let float_source = interleaved_source
        .into_iter()
        .enumerate()
        .map(|(index, value)| f64::from(value) + f64::from((index % 3 + 1) as u8) / 10.0)
        .collect::<Vec<_>>();
    let mut tiled_float = Vec::with_capacity(48);
    for tile_y in 0..2 {
        for tile_x in 0..2 {
            for y in 0..2 {
                for x in 0..2 {
                    let source_x = tile_x * 2 + x;
                    let source_y = tile_y * 2 + y;
                    for sample in 0..3 {
                        tiled_float.push(if source_x < 3 && source_y < 3 {
                            float_source[((source_y * 3 + source_x) * 3) + sample]
                        } else {
                            0.0
                        });
                    }
                }
            }
        }
    }
    (interleaved, planar, tiled_float)
}

struct WriterPair {
    name: &'static str,
    root: Vec<u8>,
    direct: Vec<u8>,
}

fn rust_writer_outputs() -> Vec<WriterPair> {
    let nested = WriterData::Nested(vec![
        vec![vec![1.0, 2.0], vec![3.0, 4.0]],
        vec![vec![5.0, 6.0], vec![7.0, 8.0]],
        vec![vec![9.0, 10.0], vec![11.0, 12.0]],
    ]);
    let int16 = TypedArray::Int16(vec![-32768, -2, 3, 32767]);
    let (tiled_interleaved, tiled_planar, tiled_float) = tiled_edge_inputs();
    let zero_tile = || {
        WriterMetadata::new(2, 2)
            .with_tag(writer::tag::SAMPLES_PER_PIXEL, 1u16)
            .with_tag(writer::tag::TILE_WIDTH, 2u16)
            .with_tag(writer::tag::TILE_LENGTH, 2u16)
            .with_tag(writer::tag::TILE_BYTE_COUNTS, vec![0u32])
    };

    let inputs = vec![
        (
            "uint8",
            WriterData::from(vec![1u8, 2, 3, 4]),
            WriterMetadata::new(2, 2),
            WriterCompatibility::Lossless,
        ),
        (
            "int8",
            WriterData::from(TypedArray::Int8(vec![-128, -2, 3, 127])),
            WriterMetadata::new(2, 2),
            WriterCompatibility::GeotiffJs,
        ),
        (
            "uint16",
            WriterData::from(vec![0u16, 2, 65534, 65535]),
            WriterMetadata::new(2, 2),
            WriterCompatibility::Lossless,
        ),
        (
            "int16",
            WriterData::from(int16.clone()),
            WriterMetadata::new(2, 2),
            WriterCompatibility::GeotiffJs,
        ),
        (
            "uint32",
            WriterData::from(vec![0u32, 2, 4_294_967_294, 4_294_967_295]),
            WriterMetadata::new(2, 2),
            WriterCompatibility::Lossless,
        ),
        (
            "int32",
            WriterData::from(TypedArray::Int32(vec![i32::MIN, -2, 3, i32::MAX])),
            WriterMetadata::new(2, 2),
            WriterCompatibility::GeotiffJs,
        ),
        (
            "float32",
            WriterData::from(TypedArray::Float32(vec![-1.5, 0.0, 2.25, 8.5])),
            WriterMetadata::new(2, 2),
            WriterCompatibility::Lossless,
        ),
        (
            "float64",
            WriterData::from(TypedArray::Float64(vec![
                -1.5,
                0.0,
                std::f64::consts::PI,
                8.5,
            ])),
            WriterMetadata::new(2, 2),
            WriterCompatibility::Lossless,
        ),
        (
            "flatNumbers",
            WriterData::Numbers(vec![1.0, 2.0, 3.0, 4.0]),
            WriterMetadata::new(2, 2),
            WriterCompatibility::Lossless,
        ),
        (
            "nested",
            nested,
            WriterMetadata::default(),
            WriterCompatibility::Lossless,
        ),
        (
            "richMetadata",
            WriterData::from(vec![1u16, 2, 3, 4]),
            rich_metadata(),
            WriterCompatibility::Lossless,
        ),
        (
            "tiled",
            WriterData::from((0..27u8).collect::<Vec<_>>()),
            tiled_metadata(),
            WriterCompatibility::Lossless,
        ),
        (
            "tiledInterleaved",
            WriterData::from(tiled_interleaved),
            tiled_edge_metadata(false, 12),
            WriterCompatibility::Lossless,
        ),
        (
            "tiledPlanar",
            WriterData::from(tiled_planar),
            tiled_edge_metadata(true, 4),
            WriterCompatibility::Lossless,
        ),
        (
            "tiledFloat64",
            WriterData::from(TypedArray::Float64(tiled_float)),
            tiled_edge_metadata(false, 96),
            WriterCompatibility::Lossless,
        ),
        (
            "zeroTile",
            WriterData::from(vec![9u8, 8, 7, 6]),
            zero_tile(),
            WriterCompatibility::Lossless,
        ),
        (
            "zeroTileNoData",
            WriterData::from(vec![9u8, 8, 7, 6]),
            zero_tile().with_tag(writer::tag::GDAL_NODATA, "7\0"),
            WriterCompatibility::Lossless,
        ),
        (
            "multiStrip",
            WriterData::from(vec![1u8, 2, 3, 4, 5, 6]),
            multi_strip_metadata(),
            WriterCompatibility::Lossless,
        ),
    ];

    let outputs = inputs
        .into_iter()
        .map(|(name, data, metadata, compatibility)| WriterPair {
            name,
            root: write_array_buffer_with_mode(data.clone(), metadata.clone(), compatibility)
                .unwrap(),
            direct: write_geotiff_with_mode(data, metadata, compatibility).unwrap(),
        })
        .collect::<Vec<_>>();

    // The default remains lossless, and is deliberately not compared to the
    // reference's corrupt signed payload.
    let int16_lossless = write_geotiff_with_mode(
        int16,
        WriterMetadata::new(2, 2),
        WriterCompatibility::Lossless,
    )
    .unwrap();
    assert_eq!(&int16_lossless[1000..], &[128, 0, 255, 254, 0, 3, 127, 255]);

    outputs
}

fn rust_writer_cases() -> Value {
    Value::Object(Map::from_iter(rust_writer_outputs().into_iter().map(
        |pair| (pair.name.to_string(), writer_case(pair.root, pair.direct)),
    )))
}

async fn rust_writer_readbacks() -> Value {
    let mut output = Map::new();
    for pair in rust_writer_outputs() {
        let dataset = from_array_buffer(Bytes::from(pair.root)).await.unwrap();
        output.insert(pair.name.to_string(), image_raster_summary(&dataset).await);
    }
    Value::Object(output)
}

async fn rust_rgb_dispatch_cases() -> Value {
    let rgba = vec![10u8, 20, 30, 40, 200, 150, 100, 128];
    let white = with_short_tag(
        write_array_buffer(vec![0u8, 64, 255, 128], WriterMetadata::new(2, 2)).unwrap(),
        writer::tag::PHOTOMETRIC_INTERPRETATION,
        0,
    );
    let cases = vec![
        ("whiteIsZero", white, false),
        (
            "cmyk",
            write_array_buffer(
                vec![0u8, 0, 0, 0, 255, 0, 0, 0],
                WriterMetadata::new(2, 1)
                    .with_tag(writer::tag::SAMPLES_PER_PIXEL, 4u16)
                    .with_tag(writer::tag::PHOTOMETRIC_INTERPRETATION, 5u16),
            )
            .unwrap(),
            false,
        ),
        (
            "cielab",
            write_array_buffer(
                vec![100u8, 128, 128, 200, 100, 150],
                WriterMetadata::new(2, 1)
                    .with_tag(writer::tag::SAMPLES_PER_PIXEL, 3u16)
                    .with_tag(writer::tag::PHOTOMETRIC_INTERPRETATION, 8u16),
            )
            .unwrap(),
            false,
        ),
        (
            "rgbaWithoutAlpha",
            write_array_buffer(
                rgba.clone(),
                WriterMetadata::new(2, 1)
                    .with_tag(writer::tag::SAMPLES_PER_PIXEL, 4u16)
                    .with_tag(writer::tag::PHOTOMETRIC_INTERPRETATION, 2u16)
                    .with_tag(writer::tag::EXTRA_SAMPLES, vec![2u16]),
            )
            .unwrap(),
            false,
        ),
        (
            "rgbaWithAlpha",
            write_array_buffer(
                rgba,
                WriterMetadata::new(2, 1)
                    .with_tag(writer::tag::SAMPLES_PER_PIXEL, 4u16)
                    .with_tag(writer::tag::PHOTOMETRIC_INTERPRETATION, 2u16)
                    .with_tag(writer::tag::EXTRA_SAMPLES, vec![2u16]),
            )
            .unwrap(),
            true,
        ),
    ];
    let mut output = Map::new();
    for (name, bytes, enable_alpha) in cases {
        let dataset = from_array_buffer(Bytes::from(bytes)).await.unwrap();
        let raster = dataset
            .image(0)
            .unwrap()
            .read_rgb(ReadRgbOptions {
                interleave: true,
                enable_alpha,
                ..ReadRgbOptions::default()
            })
            .await
            .unwrap();
        output.insert(name.to_string(), raster_summary(raster));
    }
    Value::Object(output)
}

#[tokio::test]
#[ignore = "requires the sibling geotiff.js 3.1.0 repository and Node.js"]
async fn live_two_repository_factory_source_cache_decoder_cancellation_and_writer_differential() {
    let js = run_js_oracle();
    assert_eq!(js["reference"]["version"], "3.1.0");
    assert_api_divergence_ledger();
    let main = Bytes::from(std::fs::read(fixture("tiled-gray-i1.tif")).unwrap());
    let overview = std::fs::read(fixture("minisblack-1c-8b.tiff")).unwrap();

    let rust_factories = rust_factory_cases(main.clone(), overview).await;
    for name in [
        "arrayBuffer",
        "file",
        "fileClose",
        "blob",
        "customClient",
        "fromSource",
        "url",
        "fullAllowed",
        "multi",
        "httpRequestFacts",
    ] {
        assert_section(
            &format!("factories.{name}"),
            &js["factories"][name],
            &rust_factories[name],
        );
    }
    assert!(js["factories"]["fullRejected"].get("error").is_some());
    assert!(
        !js["factories"]["customClientRequests"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let rust_sources = rust_source_cases().await;
    assert_eq!(js["sources"]["fileSize"], 16);
    assert_eq!(js["sources"]["requests"], json!([[0, 12]]));
    assert_section(
        "sources.first",
        &js["sources"]["first"],
        &rust_sources["first"],
    );
    assert_section(
        "sources.second",
        &js["sources"]["second"],
        &rust_sources["second"],
    );

    assert_section(
        "decoderAndCache",
        &js["decoderAndCache"],
        &rust_decoder_and_cache_cases(main.clone()).await,
    );
    assert_cancellation(&js["cancellation"], main).await;
    assert_section("writer", &js["writer"], &rust_writer_cases());
    assert_section(
        "writerReadbacks",
        &js["writerReadbacks"],
        &rust_writer_readbacks().await,
    );
    assert_section(
        "rgbDispatch",
        &js["rgbDispatch"],
        &rust_rgb_dispatch_cases().await,
    );
}
