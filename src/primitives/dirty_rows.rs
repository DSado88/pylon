use std::sync::atomic::{AtomicU64, Ordering};

/// Atomic bitmask tracking which rows (0..511) need re-rendering.
/// Uses 8 x AtomicU64 = 512 bits, fitting in 4 cache lines.
pub struct DirtyRows {
    bits: [AtomicU64; 8],
}

impl DirtyRows {
    pub const fn new() -> Self {
        Self {
            bits: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
        }
    }

    /// Mark a single row as dirty (0..511). Rows >= 512 are ignored.
    #[inline]
    pub fn mark(&self, row: u16) {
        if row < 512 {
            let bucket = (row / 64) as usize;
            let bit = row % 64;
            if let Some(word) = self.bits.get(bucket) {
                word.fetch_or(1u64 << bit, Ordering::Release);
            }
        }
    }

    /// Mark all rows as dirty.
    #[inline]
    pub fn mark_all(&self) {
        for word in &self.bits {
            word.store(u64::MAX, Ordering::Release);
        }
    }

    /// Atomically drain all dirty bits, returning the bitmasks.
    #[inline]
    pub fn drain(&self) -> [u64; 8] {
        [
            self.bits.first().map_or(0, |w| w.swap(0, Ordering::AcqRel)),
            self.bits.get(1).map_or(0, |w| w.swap(0, Ordering::AcqRel)),
            self.bits.get(2).map_or(0, |w| w.swap(0, Ordering::AcqRel)),
            self.bits.get(3).map_or(0, |w| w.swap(0, Ordering::AcqRel)),
            self.bits.get(4).map_or(0, |w| w.swap(0, Ordering::AcqRel)),
            self.bits.get(5).map_or(0, |w| w.swap(0, Ordering::AcqRel)),
            self.bits.get(6).map_or(0, |w| w.swap(0, Ordering::AcqRel)),
            self.bits.get(7).map_or(0, |w| w.swap(0, Ordering::AcqRel)),
        ]
    }

    /// Check if any row is dirty.
    #[inline]
    pub fn any_dirty(&self) -> bool {
        self.bits.iter().any(|w| w.load(Ordering::Acquire) != 0)
    }
}

impl Default for DirtyRows {
    fn default() -> Self {
        Self::new()
    }
}
