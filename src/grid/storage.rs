use std::sync::{Arc, RwLock};

use super::cell::Cell;
use super::row::Row;

/// Ring buffer for scrollback history.
#[derive(Debug)]
pub struct ScrollbackBuffer {
    buffer: Vec<Row>,
    capacity: usize,
    head: usize,
    len: usize,
}

impl ScrollbackBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: Vec::new(),
            capacity,
            head: 0,
            len: 0,
        }
    }

    /// Push a row into the ring buffer. O(1) amortized.
    pub fn push(&mut self, row: Row) {
        if self.capacity == 0 {
            return;
        }
        if self.buffer.len() < self.capacity {
            // Still growing
            self.buffer.push(row);
            self.len = self.buffer.len();
            self.head = self.len;
        } else {
            // Ring is full, overwrite at head
            let idx = self.head % self.capacity;
            if let Some(slot) = self.buffer.get_mut(idx) {
                *slot = row;
            }
            self.head = self.head.wrapping_add(1);
            self.len = self.capacity;
        }
    }

    /// Get a row from the scrollback. Index 0 is the oldest visible row.
    pub fn get(&self, index: usize) -> Option<&Row> {
        if index >= self.len {
            return None;
        }
        if self.buffer.len() < self.capacity {
            // Not yet wrapped
            self.buffer.get(index)
        } else {
            let start = self.head % self.capacity;
            let real_idx = (start + index) % self.capacity;
            self.buffer.get(real_idx)
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

/// The terminal grid: visible rows + scrollback.
pub struct Grid {
    rows: Vec<Row>,
    cols: usize,
    visible_rows: usize,
    scrollback: ScrollbackBuffer,
    /// When true, scroll_up does not push lines to scrollback (alternate screen mode).
    pub suppress_scrollback: bool,
}

impl Grid {
    pub fn new(visible_rows: usize, cols: usize, scrollback_capacity: usize) -> Self {
        let rows = (0..visible_rows).map(|_| Row::new(cols)).collect();
        Self {
            rows,
            cols,
            visible_rows,
            scrollback: ScrollbackBuffer::new(scrollback_capacity),
            suppress_scrollback: false,
        }
    }

    #[inline]
    pub fn cols(&self) -> usize {
        self.cols
    }

    #[inline]
    pub fn visible_rows(&self) -> usize {
        self.visible_rows
    }

    /// Get a reference to a cell at (row, col) in the visible area.
    pub fn cell(&self, row: usize, col: usize) -> Option<&Cell> {
        self.rows.get(row)?.get(col)
    }

    /// Get a mutable reference to a cell at (row, col) in the visible area.
    pub fn cell_mut(&mut self, row: usize, col: usize) -> Option<&mut Cell> {
        self.rows.get_mut(row)?.get_mut(col)
    }

    /// Get a reference to a visible row.
    pub fn row(&self, row: usize) -> Option<&Row> {
        self.rows.get(row)
    }

    /// Get a mutable reference to a visible row.
    pub fn row_mut(&mut self, row: usize) -> Option<&mut Row> {
        self.rows.get_mut(row)
    }

    /// Scroll the region [top, bottom) up by `count` lines.
    /// Top lines go to scrollback (only if top == 0), bottom lines are filled blank.
    /// Uses rotate instead of clone to avoid O(N) heap allocations per scroll.
    pub fn scroll_up(&mut self, top: usize, bottom: usize, count: usize) {
        if top >= bottom || bottom > self.visible_rows || count == 0 {
            return;
        }
        let count = count.min(bottom - top);

        // Move scrolled-off rows to scrollback before they get rotated to the end
        if top == 0 && !self.suppress_scrollback {
            // Swap each row being scrolled off with a blank, push the original
            // to scrollback, then rotate the region. This way we take ownership
            // of the row content without cloning.
            for i in 0..count {
                let idx = top + i;
                if idx < self.rows.len() {
                    let mut scrollback_row = Row::new(self.cols);
                    if let Some(row) = self.rows.get_mut(idx) {
                        std::mem::swap(row, &mut scrollback_row);
                    }
                    self.scrollback.push(scrollback_row);
                }
            }
            // The first `count` rows in [top..bottom) are now blank.
            // Rotate them to the end of the region — this is the scroll.
            if let Some(region) = self.rows.get_mut(top..bottom) {
                region.rotate_left(count);
            }
            // The blank rows are now at the bottom of the region — already clear.
        } else {
            // Non-zero top: no scrollback, just rotate the sub-region
            if let Some(region) = self.rows.get_mut(top..bottom) {
                region.rotate_left(count);
            }
            // Clear the newly exposed lines at the bottom of the region
            for i in (bottom - count)..bottom {
                if let Some(row) = self.rows.get_mut(i) {
                    row.clear();
                }
            }
        }
    }

    /// Scroll the region [top, bottom) down by `count` lines.
    /// Bottom lines are lost, top lines are filled blank.
    /// Uses rotate instead of clone to avoid heap allocations.
    pub fn scroll_down(&mut self, top: usize, bottom: usize, count: usize) {
        if top >= bottom || bottom > self.visible_rows || count == 0 {
            return;
        }
        let count = count.min(bottom - top);

        // Rotate right moves top rows to the bottom, bottom rows to the top
        if let Some(region) = self.rows.get_mut(top..bottom) {
            region.rotate_right(count);
        }

        // Clear the newly exposed lines at the top of the region
        for i in top..(top + count) {
            if let Some(row) = self.rows.get_mut(i) {
                row.clear();
            }
        }
    }

    /// Resize the grid. Existing content is preserved where possible.
    pub fn resize(&mut self, new_rows: usize, new_cols: usize) {
        // Resize existing rows' column count
        for row in &mut self.rows {
            row.resize(new_cols);
        }

        // Add or remove rows
        if new_rows > self.visible_rows {
            for _ in 0..(new_rows - self.visible_rows) {
                self.rows.push(Row::new(new_cols));
            }
        } else if new_rows < self.visible_rows {
            // Push excess rows to scrollback before removing — drain takes ownership
            let excess = self.visible_rows - new_rows;
            for row in self.rows.drain(..excess) {
                self.scrollback.push(row);
            }
        }

        self.visible_rows = new_rows;
        self.cols = new_cols;
    }

    /// Get a row from scrollback by index (0 = oldest).
    pub fn scrollback_row(&self, index: usize) -> Option<&Row> {
        self.scrollback.get(index)
    }

    /// Number of rows in scrollback.
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    /// Get a cell at (row, col) with a scroll offset.
    /// offset=0 shows the live terminal. offset=N shifts the viewport N lines
    /// into scrollback history.
    pub fn cell_scrolled(&self, row: usize, col: usize, scroll_offset: usize) -> Option<&Cell> {
        if scroll_offset == 0 {
            return self.cell(row, col);
        }
        let sb_len = self.scrollback.len();
        // The viewport when scrolled: the top `scroll_offset` rows come from
        // scrollback (from the end), then the rest from visible rows.
        let sb_start = sb_len.saturating_sub(scroll_offset);
        let sb_row_idx = sb_start + row;
        if sb_row_idx < sb_len {
            // This row is in scrollback
            self.scrollback.get(sb_row_idx)?.get(col)
        } else {
            // This row is in visible area
            let visible_row = sb_row_idx - sb_len;
            self.cell(visible_row, col)
        }
    }

    /// Clear all visible rows.
    pub fn clear_visible(&mut self) {
        for row in &mut self.rows {
            row.clear();
        }
    }

    /// Insert `count` blank lines at row index within [top, bottom), shifting down.
    pub fn insert_lines(&mut self, at: usize, count: usize, top: usize, bottom: usize) {
        if at < top || at >= bottom || bottom > self.visible_rows {
            return;
        }
        // Shift rows from `at` downward, then blank `at..at+count`
        self.scroll_down(at, bottom, count);
    }

    /// Delete `count` lines at row index within [top, bottom), shifting up.
    pub fn delete_lines(&mut self, at: usize, count: usize, top: usize, bottom: usize) {
        if at < top || at >= bottom || bottom > self.visible_rows {
            return;
        }
        self.scroll_up(at, bottom, count);
    }
}

/// Thread-safe shared grid handle.
pub type SharedGrid = Arc<RwLock<Grid>>;
