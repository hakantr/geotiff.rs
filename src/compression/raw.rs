//! Port of `compression/raw.js` - the no-op "decoder" for uncompressed
//! TIFF strips/tiles.

/// `RawDecoder.decodeBlock(buffer)`
pub fn decode_block(buffer: &[u8]) -> Vec<u8> {
    buffer.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_the_buffer_through_unchanged() {
        assert_eq!(decode_block(&[1, 2, 3]), vec![1, 2, 3]);
    }
}
