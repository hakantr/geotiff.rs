//! Port of `utils.ts`. Most of these exist in the original only because JS
//! lacks the array/object helpers Rust's std + iterator ecosystem already
//! provides; per the port's translator-function rule they keep their
//! original names/signatures (callers reference them the same way) but
//! delegate to idiomatic Rust internally where a std equivalent exists.

use crate::decode_pool::CancellationToken;
use std::collections::HashMap;
use std::hash::Hash;

pub use crate::typed_array::{is_typed_float_array, is_typed_int_array, is_typed_uint_array};

/// `assign<T, S>(target: T, source: S): T & S` - JS `Object.assign`-style
/// merge. The only real caller (geotiffwriter.js) uses it on plain
/// string-keyed maps, so `Record<string, number>` becomes `HashMap<K, V>`
/// rather than a generic struct merge (which Rust's type system can't do
/// generically the way JS object spread can). Mutates `target` in place and
/// also returns it, matching the original (JS objects are references, so
/// the call site can ignore the return value and rely on the mutation).
pub fn assign<K: Eq + Hash, V>(
    target: &mut HashMap<K, V>,
    source: HashMap<K, V>,
) -> &mut HashMap<K, V> {
    target.extend(source);
    target
}

/// `chunk<T>(iterable, length): T[][]`. No caller anywhere in src/ (verified
/// via full-repo grep during porting) - kept for API completeness since it's
/// a public export. JS pads the final chunk with `undefined` for indices
/// past the end rather than shortening it; `Option<T>` is the faithful
/// translation of that padding (there is no Rust value standing in for
/// `undefined`).
pub fn chunk<T: Clone>(iterable: &[T], length: usize) -> Vec<Vec<Option<T>>> {
    // The JavaScript loop never advances for a zero length. A public native
    // helper must not hang the process, so treat it as producing no chunks.
    if length == 0 {
        return Vec::new();
    }
    let mut results = Vec::new();
    let mut i = 0;
    while i < iterable.len() {
        let mut chunked = Vec::with_capacity(length);
        for ci in i..i + length {
            chunked.push(iterable.get(ci).cloned());
        }
        results.push(chunked);
        i += length;
    }
    results
}

/// `endsWith(string, expectedEnding): boolean` - JS reimplements this by
/// hand; Rust's `str::ends_with` is the exact behavioral equivalent.
pub fn ends_with(string: &str, expected_ending: &str) -> bool {
    string.ends_with(expected_ending)
}

/// `forEach<T>(iterable, func): void`
pub fn for_each<T>(iterable: &[T], mut func: impl FnMut(&T, usize)) {
    for (i, item) in iterable.iter().enumerate() {
        func(item, i);
    }
}

/// `invert<K, V>(oldObj): Record<V, K>`
pub fn invert<K: Clone, V: Eq + Hash + Clone>(old: &HashMap<K, V>) -> HashMap<V, K> {
    old.iter().map(|(k, v)| (v.clone(), k.clone())).collect()
}

/// `range(n): number[]`
pub fn range(n: usize) -> Vec<usize> {
    (0..n).collect()
}

/// `times<T>(numTimes, func): T[]`
pub fn times<T>(num_times: usize, mut func: impl FnMut(usize) -> T) -> Vec<T> {
    (0..num_times).map(&mut func).collect()
}

/// `toArray<T>(iterable): T[]`
pub fn to_array<T: Clone>(iterable: &[T]) -> Vec<T> {
    iterable.to_vec()
}

/// A minimal stand-in for the `unknown` JS `toArrayRecursively`/`isArrayLike`
/// operate over. Neither function has any caller in src/ (verified via
/// full-repo grep) and JS's dynamic "is this array-like?" runtime check has
/// no faithful generic Rust equivalent without a concrete dynamic-value
/// type - this is the minimal one that lets the recursive behavior exist at
/// all. Extend it if/when a real caller needs a richer value shape.
///
/// `isArrayLike` itself has no separate Rust function: it existed only to
/// answer "should I recurse into this?", which `NestedValue`'s two variants
/// already answer at the type level via pattern matching in
/// `to_array_recursively` below - a private runtime predicate replaced by a
/// static match, not a dropped behavior.
#[derive(Debug, Clone, PartialEq)]
pub enum NestedValue {
    Array(Vec<NestedValue>),
    Leaf(f64),
}

/// `toArrayRecursively(input): unknown`
pub fn to_array_recursively(input: NestedValue) -> NestedValue {
    match input {
        NestedValue::Array(items) => {
            NestedValue::Array(items.into_iter().map(to_array_recursively).collect())
        }
        leaf @ NestedValue::Leaf(_) => leaf,
    }
}

/// Result of the public `utils.parseContentRange` helper. This deliberately
/// remains separate from `source::httputils::ContentRange`: geotiff.js also
/// has two helpers with different shapes and permissiveness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedContentRange {
    pub unit: Option<String>,
    pub first: Option<u64>,
    pub last: Option<u64>,
    pub length: Option<u64>,
}

/// Public `utils.parseContentRange`, including its permissive unanchored
/// matching and `*` length behavior, implemented without a regex dependency.
pub fn parse_content_range(header_value: &str) -> Option<ParsedContentRange> {
    if header_value.is_empty() {
        return None;
    }
    let bytes = header_value.as_bytes();
    let unit = bytes
        .iter()
        .position(|byte| *byte == b' ')
        .and_then(|space| {
            bytes[..space]
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                .then(|| header_value[..space].to_string())
        });

    let digits_end = |from: usize| {
        let mut end = from;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        end
    };
    for start in 0..bytes.len() {
        if !bytes[start].is_ascii_digit() {
            continue;
        }
        let first_end = digits_end(start);
        if bytes.get(first_end) != Some(&b'-') {
            continue;
        }
        let last_start = first_end + 1;
        let last_end = digits_end(last_start);
        if last_end == last_start || bytes.get(last_end) != Some(&b'/') {
            continue;
        }
        let length_start = last_end + 1;
        let (length, matched) = if bytes.get(length_start) == Some(&b'*') {
            (None, true)
        } else {
            let length_end = digits_end(length_start);
            (
                header_value[length_start..length_end].parse::<u64>().ok(),
                length_end > length_start,
            )
        };
        if matched {
            return Some(ParsedContentRange {
                unit,
                first: header_value[start..first_end].parse().ok(),
                last: header_value[last_start..last_end].parse().ok(),
                length,
            });
        }
    }

    for start in 0..bytes.len() {
        if bytes[start] == b'*' {
            return Some(ParsedContentRange {
                unit,
                first: None,
                last: None,
                length: None,
            });
        }
        if bytes[start].is_ascii_digit() {
            let end = digits_end(start);
            return Some(ParsedContentRange {
                unit,
                first: None,
                last: None,
                length: header_value[start..end].parse().ok(),
            });
        }
    }
    None
}

/// `wait(milliseconds): Promise<void>`
pub async fn wait(milliseconds: Option<u64>) {
    tokio::time::sleep(std::time::Duration::from_millis(milliseconds.unwrap_or(0))).await;
}

/// `zip<T, U>(a, b): [T, U][]`. **Not** Rust `Iterator::zip` semantics: JS's
/// version drives the output length off `a` alone and reads `b[i]` (which is
/// `undefined` past `b`'s end) rather than stopping at the shorter of the
/// two - `std::iter::Iterator::zip` would silently truncate to the shorter
/// length instead, which is a real behavioral divergence if `b` is ever
/// shorter than `a`.
pub fn zip<T: Clone, U: Clone>(a: &[T], b: &[U]) -> Vec<(T, Option<U>)> {
    a.iter()
        .enumerate()
        .map(|(i, k)| (k.clone(), b.get(i).cloned()))
        .collect()
}

/// ECMAScript `Number(string)` for the metadata spellings encountered by
/// GeoTIFF/GDAL. Invalid input returns `NaN`, and an empty/whitespace-only
/// string returns zero, matching JavaScript rather than Rust `parse()`.
pub fn parse_js_number(input: &str) -> f64 {
    let value = input.trim();
    if value.is_empty() {
        return 0.0;
    }
    match value {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    // Rust accepts several non-ECMAScript infinity spellings (`inf`,
    // `-inf`, and case variants). `Number(...)` accepts only the exact
    // `Infinity` forms handled above; all of these aliases must become NaN.
    if matches!(
        value.to_ascii_lowercase().as_str(),
        "inf" | "+inf" | "-inf" | "infinity" | "+infinity" | "-infinity"
    ) {
        return f64::NAN;
    }
    let radix = |digits: &str, base| {
        u64::from_str_radix(digits, base)
            .map(|value| value as f64)
            .unwrap_or(f64::NAN)
    };
    if let Some(digits) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        return radix(digits, 16);
    }
    if let Some(digits) = value
        .strip_prefix("0b")
        .or_else(|| value.strip_prefix("0B"))
    {
        return radix(digits, 2);
    }
    if let Some(digits) = value
        .strip_prefix("0o")
        .or_else(|| value.strip_prefix("0O"))
    {
        return radix(digits, 8);
    }
    value.parse::<f64>().unwrap_or(f64::NAN)
}

/// `class AbortError extends Error`
#[derive(Debug, Clone)]
pub struct AbortError {
    pub message: String,
    pub signal: Option<CancellationToken>,
}

impl AbortError {
    pub fn new(message: impl Into<String>) -> Self {
        AbortError {
            message: message.into(),
            signal: None,
        }
    }

    pub fn with_signal(message: impl Into<String>, signal: CancellationToken) -> Self {
        AbortError {
            message: message.into(),
            signal: Some(signal),
        }
    }
}

impl std::fmt::Display for AbortError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AbortError {}

/// `class CustomAggregateError extends Error` (exported as `AggregateError`
/// in JS to avoid colliding with the global built-in `AggregateError` - that
/// name collision concern doesn't exist in Rust, so the port uses the plain
/// intended name directly instead of keeping both names as aliases).
#[derive(Debug)]
pub struct AggregateError {
    pub errors: Vec<Box<dyn std::error::Error + Send + Sync>>,
    pub message: String,
}

impl AggregateError {
    pub fn new(
        errors: Vec<Box<dyn std::error::Error + Send + Sync>>,
        message: impl Into<String>,
    ) -> Self {
        AggregateError {
            errors,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for AggregateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AggregateError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_pads_final_chunk_with_none() {
        let result = chunk(&[1, 2, 3, 4, 5], 2);
        assert_eq!(
            result,
            vec![
                vec![Some(1), Some(2)],
                vec![Some(3), Some(4)],
                vec![Some(5), None]
            ]
        );
    }

    #[test]
    fn zero_length_chunk_does_not_loop_forever() {
        assert!(chunk(&[1, 2, 3], 0).is_empty());
    }

    #[test]
    fn ends_with_matches_str_ends_with() {
        assert!(ends_with("hello.tif", ".tif"));
        assert!(!ends_with("hi", "hello"));
    }

    #[test]
    fn zip_drives_length_off_first_arg_not_shorter() {
        let a = [1, 2, 3];
        let b = [10, 20];
        let z = zip(&a, &b);
        assert_eq!(z, vec![(1, Some(10)), (2, Some(20)), (3, None)]);
    }

    #[test]
    fn invert_swaps_keys_and_values() {
        let mut m = HashMap::new();
        m.insert("a".to_string(), 1);
        m.insert("b".to_string(), 2);
        let inv = invert(&m);
        assert_eq!(inv.get(&1), Some(&"a".to_string()));
        assert_eq!(inv.get(&2), Some(&"b".to_string()));
    }

    #[test]
    fn times_and_range() {
        assert_eq!(range(4), vec![0, 1, 2, 3]);
        assert_eq!(times(4, |i| i * 2), vec![0, 2, 4, 6]);
    }

    #[test]
    fn javascript_number_metadata_semantics_are_preserved() {
        assert_eq!(parse_js_number(""), 0.0);
        assert_eq!(parse_js_number(" 0x10 "), 16.0);
        assert_eq!(parse_js_number("Infinity"), f64::INFINITY);
        assert_eq!(parse_js_number("+Infinity"), f64::INFINITY);
        assert_eq!(parse_js_number("-Infinity"), f64::NEG_INFINITY);
        assert!(parse_js_number("inf").is_nan());
        assert!(parse_js_number("-inf").is_nan());
        assert!(parse_js_number("INFINITY").is_nan());
        assert!(parse_js_number("not-a-number").is_nan());
    }

    #[test]
    fn public_content_range_helper_matches_the_js_shapes() {
        assert_eq!(
            parse_content_range("bytes 10-19/100"),
            Some(ParsedContentRange {
                unit: Some("bytes".to_string()),
                first: Some(10),
                last: Some(19),
                length: Some(100),
            })
        );
        assert_eq!(
            parse_content_range("items */42"),
            Some(ParsedContentRange {
                unit: Some("items".to_string()),
                first: None,
                last: None,
                length: None,
            })
        );
        assert_eq!(parse_content_range(""), None);
    }
}
