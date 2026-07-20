//! HTTP header and multipart byte-range parsing used by the remote source.
//!
//! This is the native counterpart of `source/httputils.js`.  Parsing is a
//! little stricter than the JavaScript implementation: malformed range
//! responses are rejected instead of being allowed to shift TIFF offsets
//! silently.

use bytes::Bytes;
use std::collections::HashMap;

/// `itemsToObject(items)`.
pub fn items_to_object<T>(items: Vec<(String, T)>) -> HashMap<String, T> {
    items.into_iter().collect()
}

/// Parsed `Content-Type` value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContentType {
    pub media_type: Option<String>,
    pub parameters: HashMap<String, String>,
}

/// `parseContentType(rawContentType)`.
pub fn parse_content_type(raw: Option<&str>) -> ContentType {
    let Some(raw) = raw else {
        return ContentType::default();
    };
    let mut pieces = raw.split(';').map(str::trim);
    let media_type = pieces
        .next()
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());
    let parameters = pieces
        .filter_map(|piece| {
            let (name, value) = piece.split_once('=')?;
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .unwrap_or(value);
            Some((name.trim().to_ascii_lowercase(), value.to_string()))
        })
        .collect();
    ContentType {
        media_type,
        parameters,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContentRange {
    pub start: Option<u64>,
    pub end: Option<u64>,
    pub total: Option<u64>,
}

/// `parseContentRange(rawContentRange)`. Invalid and unsatisfied ranges
/// retain the JavaScript helper's all-empty result.
pub fn parse_content_range(raw: Option<&str>) -> ContentRange {
    raw.and_then(parse_content_range_strict).unwrap_or_default()
}

/// Strict parser for `bytes START-END/TOTAL`.
pub fn parse_content_range_strict(raw: &str) -> Option<ContentRange> {
    let raw = raw.trim();
    let rest = raw.strip_prefix("bytes ")?;
    let (bounds, total) = rest.split_once('/')?;
    let (start, end) = bounds.split_once('-')?;
    let start = start.parse::<u64>().ok()?;
    let end = end.parse::<u64>().ok()?;
    let total = total.parse::<u64>().ok()?;
    (start <= end && end < total).then_some(ContentRange {
        start: Some(start),
        end: Some(end),
        total: Some(total),
    })
}

/// Parses `bytes */TOTAL`, used by HTTP 416 responses.
pub fn parse_unsatisfied_content_range(raw: Option<&str>) -> Option<u64> {
    raw?.trim().strip_prefix("bytes */")?.parse().ok()
}

/// One part of a `multipart/byteranges` response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteRangePart {
    pub headers: HashMap<String, String>,
    pub data: Bytes,
    pub offset: u64,
    pub length: u64,
    pub file_size: u64,
}

fn find_bytes(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|position| from + position)
}

fn parse_headers(raw: &[u8]) -> Result<HashMap<String, String>, String> {
    let raw = std::str::from_utf8(raw)
        .map_err(|_| "multipart byte-range headers are not valid ASCII".to_string())?;
    let mut headers = HashMap::new();
    for line in raw.split("\r\n").filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| format!("invalid multipart header: {line}"))?;
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }
    Ok(headers)
}

/// `parseByteRanges(responseArrayBuffer, boundary)` with bounds checking.
pub fn parse_byte_ranges(body: Bytes, boundary: &str) -> Result<Vec<ByteRangePart>, String> {
    if boundary.is_empty() {
        return Err("multipart byte-range boundary is empty".to_string());
    }
    let delimiter = format!("--{boundary}").into_bytes();
    let closing = format!("--{boundary}--").into_bytes();
    let bytes = body.as_ref();
    let mut cursor = find_bytes(bytes, &delimiter, 0)
        .ok_or_else(|| "could not find initial multipart boundary".to_string())?;
    let mut output = Vec::new();

    loop {
        if bytes.get(cursor..cursor + closing.len()) == Some(closing.as_slice()) {
            break;
        }
        if bytes.get(cursor..cursor + delimiter.len()) != Some(delimiter.as_slice()) {
            return Err("multipart part does not start with its boundary".to_string());
        }
        cursor += delimiter.len();
        if bytes.get(cursor..cursor + 2) != Some(b"\r\n") {
            return Err("multipart boundary is not followed by CRLF".to_string());
        }
        cursor += 2;

        let header_end = find_bytes(bytes, b"\r\n\r\n", cursor)
            .ok_or_else(|| "multipart part has no header terminator".to_string())?;
        let headers = parse_headers(&bytes[cursor..header_end])?;
        let range = headers
            .get("content-range")
            .and_then(|value| parse_content_range_strict(value))
            .ok_or_else(|| "multipart part has an invalid Content-Range".to_string())?;
        let start = range.start.expect("strict content range has a start");
        let end = range.end.expect("strict content range has an end");
        let total = range.total.expect("strict content range has a total");
        let length = end - start + 1;
        let data_start = header_end + 4;
        let data_end = data_start
            .checked_add(usize::try_from(length).map_err(|_| "multipart part is too large")?)
            .ok_or_else(|| "multipart part length overflow".to_string())?;
        if data_end > bytes.len() {
            return Err("multipart part is shorter than Content-Range".to_string());
        }
        output.push(ByteRangePart {
            headers,
            data: body.slice(data_start..data_end),
            offset: start,
            length,
            file_size: total,
        });

        cursor = data_end;
        if bytes.get(cursor..cursor + 2) != Some(b"\r\n") {
            return Err("multipart part data is not followed by CRLF".to_string());
        }
        cursor += 2;
        if bytes.get(cursor..cursor + closing.len()) == Some(closing.as_slice()) {
            break;
        }
    }

    if output.is_empty() {
        return Err("multipart response contains no byte ranges".to_string());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_to_object_builds_a_map_without_lowercasing() {
        let items = vec![("Content-Type".to_string(), "text/plain".to_string())];
        let obj = items_to_object(items);
        assert_eq!(obj.get("Content-Type"), Some(&"text/plain".to_string()));
        assert_eq!(obj.get("content-type"), None);
    }

    #[test]
    fn parses_content_type_and_quoted_boundary() {
        let parsed = parse_content_type(Some(
            "Multipart/ByteRanges; boundary=\"abc-123\"; charset=x",
        ));
        assert_eq!(parsed.media_type.as_deref(), Some("multipart/byteranges"));
        assert_eq!(
            parsed.parameters.get("boundary").map(String::as_str),
            Some("abc-123")
        );
    }

    #[test]
    fn parse_content_range_extracts_bytes_range() {
        let r = parse_content_range(Some("bytes 200-1000/67589"));
        assert_eq!(
            r,
            ContentRange {
                start: Some(200),
                end: Some(1000),
                total: Some(67589)
            }
        );
    }

    #[test]
    fn parse_content_range_returns_none_fields_when_absent_or_unmatched() {
        assert_eq!(parse_content_range(None), ContentRange::default());
        assert_eq!(
            parse_content_range(Some("garbage")),
            ContentRange::default()
        );
        assert_eq!(
            parse_content_range(Some("bytes 9-10/10")),
            ContentRange::default()
        );
        assert_eq!(
            parse_unsatisfied_content_range(Some("bytes */123")),
            Some(123)
        );
    }

    #[test]
    fn parses_multipart_byte_ranges_without_copying_payloads() {
        let body = Bytes::from_static(
            b"preamble\r\n--xyz\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes 2-4/10\r\n\r\n234\r\n--xyz\r\nContent-Range: bytes 8-9/10\r\n\r\n89\r\n--xyz--\r\n",
        );
        let parts = parse_byte_ranges(body, "xyz").unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].offset, 2);
        assert_eq!(parts[0].data.as_ref(), b"234");
        assert_eq!(parts[1].offset, 8);
        assert_eq!(parts[1].data.as_ref(), b"89");
        assert_eq!(parts[1].file_size, 10);
    }

    #[test]
    fn malformed_multipart_data_is_an_error() {
        let body =
            Bytes::from_static(b"--x\r\nContent-Range: bytes 0-4/10\r\n\r\nabc\r\n--x--\r\n");
        assert!(parse_byte_ranges(body, "x").is_err());
    }
}
