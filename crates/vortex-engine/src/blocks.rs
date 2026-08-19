//! Blocks under leases (02 §3).
//!
//! Ground truth for a transfer is a roaring bitmap of completed fixed-size blocks. Blocks
//! make resume, stealing and out-of-order completion trivial and keep persisted state tiny;
//! contiguous leases on top of them keep each socket's read pattern sequential, which is
//! what TCP congestion control and the disk both want. You need both.

use roaring::RoaringBitmap;

/// 1 MiB default; 4 MiB above 4 GB so a huge file does not carry a huge bitmap.
pub const DEFAULT_BLOCK: u32 = 1 << 20;
pub const LARGE_BLOCK: u32 = 4 << 20;
pub const LARGE_FILE_THRESHOLD: u64 = 4 * 1024 * 1024 * 1024;

pub fn block_size_for(total: u64) -> u32 {
    if total >= LARGE_FILE_THRESHOLD {
        LARGE_BLOCK
    } else {
        DEFAULT_BLOCK
    }
}

/// A byte range, end-exclusive.
pub type Range = (u64, u64);

#[derive(Debug, Clone)]
pub struct BlockMap {
    total: u64,
    block_size: u32,
    blocks: u32,
    complete: RoaringBitmap,
}

impl BlockMap {
    pub fn new(total: u64, block_size: u32) -> Self {
        let blocks = total.div_ceil(block_size as u64) as u32;
        Self {
            total,
            block_size,
            blocks,
            complete: RoaringBitmap::new(),
        }
    }

    pub fn from_bitmap(total: u64, block_size: u32, complete: RoaringBitmap) -> Self {
        let mut map = Self::new(total, block_size);
        // A bitmap claiming blocks past the end of the file is corrupt metadata, not a
        // reason to write past the end. Drop them.
        map.complete = complete;
        map.complete.remove_range(map.blocks..u32::MAX);
        map
    }

    pub fn total(&self) -> u64 {
        self.total
    }
    pub fn block_size(&self) -> u32 {
        self.block_size
    }
    pub fn blocks(&self) -> u32 {
        self.blocks
    }
    pub fn bitmap(&self) -> &RoaringBitmap {
        &self.complete
    }

    /// Byte range covered by one block. The last block is short unless the file divides
    /// evenly — getting this wrong is how downloaders write past the end of a file.
    pub fn block_range(&self, index: u32) -> Range {
        let start = index as u64 * self.block_size as u64;
        let end = (start + self.block_size as u64).min(self.total);
        (start, end)
    }

    pub fn block_of(&self, offset: u64) -> u32 {
        (offset / self.block_size as u64) as u32
    }

    pub fn is_complete(&self, index: u32) -> bool {
        self.complete.contains(index)
    }

    pub fn mark(&mut self, index: u32) {
        if index < self.blocks {
            self.complete.insert(index);
        }
    }

    pub fn mark_range(&mut self, start: u32, end: u32) {
        self.complete.insert_range(start..end.min(self.blocks));
    }

    pub fn is_done(&self) -> bool {
        self.complete.len() == self.blocks as u64
    }

    /// Exact byte count, honouring a short final block.
    pub fn completed_bytes(&self) -> u64 {
        let mut bytes = 0u64;
        let last = self.blocks.saturating_sub(1);
        for index in self.complete.iter() {
            let (s, e) = self.block_range(index);
            bytes += e - s;
            debug_assert!(index <= last);
        }
        bytes
    }

    /// The byte ranges still missing, in file order. This is what the scheduler leases out,
    /// and on resume it is what makes a half-finished file cost nothing to pick up.
    pub fn missing_ranges(&self) -> Vec<Range> {
        let mut out: Vec<Range> = Vec::new();
        let mut index = 0u32;
        while index < self.blocks {
            if self.complete.contains(index) {
                index += 1;
                continue;
            }
            let start = index;
            while index < self.blocks && !self.complete.contains(index) {
                index += 1;
            }
            out.push((
                start as u64 * self.block_size as u64,
                self.block_range(index - 1).1,
            ));
        }
        out
    }

    /// Complete blocks as run-length encoded runs with owner 0, ready for the wire.
    pub fn complete_runs(&self) -> Vec<(u32, u32, u8)> {
        let mut out = Vec::new();
        let mut iter = self.complete.iter().peekable();
        while let Some(start) = iter.next() {
            let mut end = start + 1;
            while iter.peek() == Some(&end) {
                iter.next();
                end += 1;
            }
            out.push((start, end - start, 0));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_block_is_short_and_counted_short() {
        let mut map = BlockMap::new(2_500_000, 1_000_000);
        assert_eq!(map.blocks(), 3);
        assert_eq!(map.block_range(2), (2_000_000, 2_500_000));
        map.mark(2);
        assert_eq!(map.completed_bytes(), 500_000);
        map.mark(0);
        assert_eq!(map.completed_bytes(), 1_500_000);
    }

    #[test]
    fn missing_ranges_are_byte_exact_and_ordered() {
        let mut map = BlockMap::new(10_000, 1_000);
        map.mark_range(0, 3);
        map.mark(7);
        assert_eq!(map.missing_ranges(), vec![(3_000, 7_000), (8_000, 10_000)]);
        map.mark_range(0, 10);
        assert!(map.missing_ranges().is_empty());
        assert!(map.is_done());
    }

    #[test]
    fn runs_collapse_to_a_handful_even_when_the_file_is_huge() {
        let mut map = BlockMap::new(4 * 1024 * 1024 * 1024, LARGE_BLOCK);
        map.mark_range(0, 300);
        map.mark_range(400, 500);
        assert_eq!(map.complete_runs(), vec![(0, 300, 0), (400, 100, 0)]);
    }

    #[test]
    fn a_bitmap_claiming_blocks_past_the_end_is_trimmed() {
        let mut bits = RoaringBitmap::new();
        bits.insert_range(0..50);
        let map = BlockMap::from_bitmap(5_000, 1_000, bits);
        assert_eq!(map.bitmap().len(), 5);
        assert!(map.is_done());
    }

    #[test]
    fn block_size_scales_with_the_file() {
        assert_eq!(block_size_for(100 << 20), DEFAULT_BLOCK);
        assert_eq!(block_size_for(8 * 1024 * 1024 * 1024), LARGE_BLOCK);
    }
}
