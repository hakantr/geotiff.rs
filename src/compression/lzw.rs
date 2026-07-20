//! Port of `compression/lzw.js` - the classic TIFF-variant LZW decompressor
//! (variable-width codes 9-12 bits, dictionary reset via `CLEAR_CODE`). Pure
//! algorithm, no external crate. `compression::registry::LzwDecoder` wires
//! this function into the active decoder registry and converts corrupt input
//! into an error instead of allowing a dependency panic.

use crate::error::GeotiffError;

const MIN_BITS: u32 = 9;
const CLEAR_CODE: u32 = 256;
const EOI_CODE: u32 = 257;
const MAX_BYTELENGTH: u32 = 12;

/// `getByte(array, position, length)`. The `length - de` / `length - dg`
/// shift amounts are always non-negative in practice: `de = 8 - (position %
/// 8)` is in [1, 8] and `length` (the LZW code's bit width) never leaves
/// [9, 12] (`MIN_BITS`..=`MAX_BYTELENGTH`), so `length - de` >= 1 always -
/// unlike JS's `<<`, Rust's shift has no implicit mod-32 wraparound for a
/// hypothetical negative amount, but that case cannot occur here given
/// those invariants.
pub fn get_byte(array: &[u8], position: u32, length: u32) -> u32 {
    let d = position % 8;
    let a = (position / 8) as usize;
    let de = 8 - d;
    let ef = (position + length) as i64 - ((a as i64 + 1) * 8);
    let fg = ((8 * (a as i64 + 2)) - (position + length) as i64).max(0) as u32;
    let dg = (a as i64 + 2) * 8 - position as i64;

    if a >= array.len() {
        crate::logging::warn(
            "ran off the end of the buffer before finding EOI_CODE (end on input code)",
        );
        return EOI_CODE;
    }

    let mut chunks: u32 = (array[a] as u32 & ((1u32 << (8 - d)) - 1)) << (length - de);
    if a + 1 < array.len() {
        let shift2 = (length as i64 - dg).max(0) as u32;
        chunks += (array[a + 1] as u32 >> fg) << shift2;
    }
    if ef > 8 && a + 2 < array.len() {
        let hi = (a as i64 + 3) * 8 - (position + length) as i64;
        chunks += array[a + 2] as u32 >> hi;
    }
    chunks
}

/// `appendReversed(dest, source)`
pub fn append_reversed<T: Copy>(dest: &mut Vec<T>, source: &[T]) {
    for i in (0..source.len()).rev() {
        dest.push(source[i]);
    }
}

struct LzwState {
    dictionary_index: Vec<u16>,
    dictionary_char: Vec<u8>,
    dictionary_length: u32,
    byte_length: u32,
    position: u32,
}

impl LzwState {
    /// `initDictionary()`
    fn init_dictionary(&mut self) {
        self.dictionary_length = 258;
        self.byte_length = MIN_BITS;
    }

    /// `getNext(array)`
    fn get_next(&mut self, array: &[u8]) -> u32 {
        let byte = get_byte(array, self.position, self.byte_length);
        self.position += self.byte_length;
        byte
    }

    /// `addToDictionary(i, c)`
    fn add_to_dictionary(&mut self, i: u32, c: u8) -> u32 {
        // The JS implementation allocates 4093 entries and silently drops
        // out-of-bounds TypedArray writes. Native code must never turn a
        // hostile or merely dictionary-filling stream into a panic, so the
        // full 12-bit code space is allocated here.
        self.dictionary_char[self.dictionary_length as usize] = c;
        self.dictionary_index[self.dictionary_length as usize] = i as u16;
        self.dictionary_length += 1;
        self.dictionary_length - 1
    }

    /// `getDictionaryReversed(n)`, using caller-owned scratch space so a
    /// large tile does not allocate once per LZW code.
    fn get_dictionary_reversed(&self, n: u32, rev: &mut Vec<u8>) -> Result<(), GeotiffError> {
        rev.clear();
        let mut i = n;
        while i != 4096 {
            if i as usize >= self.dictionary_char.len() {
                return Err(GeotiffError::InvalidLzwCode(n));
            }
            rev.push(self.dictionary_char[i as usize]);
            i = self.dictionary_index[i as usize] as u32;
            if rev.len() > 4096 {
                return Err(GeotiffError::InvalidLzwCode(n));
            }
        }
        Ok(())
    }
}

/// `decompress(input)`. JS's `if (!oldVal)` dead-code guard (an array,
/// empty or not, is always truthy in JS, so that branch never actually
/// fires) is intentionally not ported - see the original for reference.
pub fn decompress(input: &[u8]) -> Result<Vec<u8>, GeotiffError> {
    let mut state = LzwState {
        dictionary_index: vec![0u16; 4096],
        dictionary_char: vec![0u8; 4096],
        dictionary_length: 0,
        byte_length: 0,
        position: 0,
    };
    for i in 0..=257usize {
        state.dictionary_index[i] = 4096;
        state.dictionary_char[i] = i as u8;
    }

    let mut result = Vec::new();
    let mut scratch = Vec::with_capacity(4096);
    state.init_dictionary();
    let array = input;
    let mut code = state.get_next(array);
    let mut old_code: Option<u32> = None;

    while code != EOI_CODE {
        if code == CLEAR_CODE {
            state.init_dictionary();
            code = state.get_next(array);
            while code == CLEAR_CODE {
                code = state.get_next(array);
            }

            if code == EOI_CODE {
                break;
            } else if code > CLEAR_CODE {
                return Err(GeotiffError::CorruptedLzwCode(code));
            } else {
                state.get_dictionary_reversed(code, &mut scratch)?;
                append_reversed(&mut result, &scratch);
                old_code = Some(code);
            }
        } else if code < state.dictionary_length {
            state.get_dictionary_reversed(code, &mut scratch)?;
            let first = *scratch.last().ok_or(GeotiffError::InvalidLzwCode(code))?;
            append_reversed(&mut result, &scratch);
            if let Some(oc) = old_code {
                state.add_to_dictionary(oc, first);
            }
            old_code = Some(code);
        } else {
            let oc = old_code.ok_or(GeotiffError::InvalidLzwCode(code))?;
            state.get_dictionary_reversed(oc, &mut scratch)?;
            let last = *scratch.last().ok_or(GeotiffError::InvalidLzwCode(code))?;
            append_reversed(&mut result, &scratch);
            result.push(last);
            state.add_to_dictionary(oc, last);
            old_code = Some(code);
        }

        if state.dictionary_length + 1 >= (1u32 << state.byte_length) {
            if state.byte_length == MAX_BYTELENGTH {
                old_code = None;
            } else {
                state.byte_length += 1;
            }
        }
        code = state.get_next(array);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_reversed_pushes_source_back_to_front() {
        let mut dest = vec![1, 2];
        append_reversed(&mut dest, &[3, 4, 5]);
        assert_eq!(dest, vec![1, 2, 5, 4, 3]);
    }

    #[test]
    fn decompress_matches_a_real_nodejs_run_of_the_original_algorithm() {
        // Bytes captured by literally running geotiff.js's own getByte/
        // decompress in Node on the code stream
        // [(65,9),(65,9),(258,9),(66,9),(66,9),(261,9),(257,9)] (AAAA then
        // BBBB, with dictionary hits) - a real cross-check against the
        // original, not just a self-consistent Rust-only test.
        let input: [u8; 8] = [32, 144, 96, 68, 34, 20, 22, 2];
        let out = decompress(&input).unwrap();
        assert_eq!(out, vec![65, 65, 65, 65, 66, 66, 66, 66]);
    }

    #[test]
    fn decompress_round_trips_a_hand_built_stream() {
        // Encode "AAAA" with 9-bit codes by hand: literal 'A' (65), 'A'
        // (65) again -> dictionary hit for "AA" (258), then EOI. Codes:
        // 65, 65, 257, packed MSB-first into 9-bit groups per the TIFF LZW
        // convention this decoder's getByte expects.
        fn pack_bits(codes: &[(u32, u32)]) -> Vec<u8> {
            let mut bitbuf: u64 = 0;
            let mut nbits = 0u32;
            let mut out = Vec::new();
            for &(code, width) in codes {
                bitbuf = (bitbuf << width) | code as u64;
                nbits += width;
                while nbits >= 8 {
                    let shift = nbits - 8;
                    out.push(((bitbuf >> shift) & 0xff) as u8);
                    nbits -= 8;
                }
            }
            if nbits > 0 {
                out.push(((bitbuf << (8 - nbits)) & 0xff) as u8);
            }
            out
        }

        let input = pack_bits(&[(65, 9), (65, 9), (EOI_CODE, 9)]);
        let out = decompress(&input).unwrap();
        assert_eq!(out, vec![65, 65]);
    }

    #[test]
    fn decompress_rejects_invalid_code_with_no_previous() {
        fn pack_bits(codes: &[(u32, u32)]) -> Vec<u8> {
            let mut bitbuf: u64 = 0;
            let mut nbits = 0u32;
            let mut out = Vec::new();
            for &(code, width) in codes {
                bitbuf = (bitbuf << width) | code as u64;
                nbits += width;
                while nbits >= 8 {
                    let shift = nbits - 8;
                    out.push(((bitbuf >> shift) & 0xff) as u8);
                    nbits -= 8;
                }
            }
            if nbits > 0 {
                out.push(((bitbuf << (8 - nbits)) & 0xff) as u8);
            }
            out
        }
        // first code is already a dictionary-reference code (>= 258) with no prior code
        let input = pack_bits(&[(300, 9), (EOI_CODE, 9)]);
        assert_eq!(decompress(&input), Err(GeotiffError::InvalidLzwCode(300)));
    }
}
