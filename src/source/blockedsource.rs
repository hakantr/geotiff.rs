//! Plain `Block`/`BlockGroup` compatibility types from
//! `source/blockedsource.js`. The operational equivalent is
//! `source::reader::BlockedReader`, backed by a concurrent moka cache.

pub struct Block {
    pub offset: u64,
    pub length: u64,
    pub data: Vec<u8>,
}

impl Block {
    /// `constructor(offset, length, data)`
    pub fn new(offset: u64, length: u64, data: Vec<u8>) -> Self {
        Block {
            offset,
            length,
            data,
        }
    }

    /// `get top()`
    pub fn top(&self) -> u64 {
        self.offset + self.length
    }
}

pub struct BlockGroup {
    pub offset: u64,
    pub length: u64,
    pub block_ids: Vec<u64>,
}

impl BlockGroup {
    /// `constructor(offset, length, blockIds)`
    pub fn new(offset: u64, length: u64, block_ids: Vec<u64>) -> Self {
        BlockGroup {
            offset,
            length,
            block_ids,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_top_is_offset_plus_length() {
        let b = Block::new(100, 50, vec![]);
        assert_eq!(b.top(), 150);
    }
}
