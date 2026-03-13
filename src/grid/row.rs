use super::cell::Cell;

#[derive(Debug, Clone)]
pub struct Row {
    cells: Vec<Cell>,
}

impl Row {
    pub fn new(cols: usize) -> Self {
        Self {
            cells: vec![Cell::blank(); cols],
        }
    }

    #[inline]
    pub fn get(&self, col: usize) -> Option<&Cell> {
        self.cells.get(col)
    }

    #[inline]
    pub fn get_mut(&mut self, col: usize) -> Option<&mut Cell> {
        self.cells.get_mut(col)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn clear(&mut self) {
        for cell in &mut self.cells {
            cell.reset();
        }
    }

    pub fn resize(&mut self, cols: usize) {
        self.cells.resize(cols, Cell::blank());
    }

    /// Clear cells from `start` to end of row.
    pub fn clear_from(&mut self, start: usize) {
        for cell in self.cells.get_mut(start..).unwrap_or(&mut []) {
            cell.reset();
        }
    }

    /// Clear cells from start of row to `end` (exclusive).
    pub fn clear_to(&mut self, end: usize) {
        let end = end.min(self.cells.len());
        for cell in self.cells.get_mut(..end).unwrap_or(&mut []) {
            cell.reset();
        }
    }

    /// Clear cells from `start` to `end` (exclusive).
    pub fn clear_range(&mut self, start: usize, end: usize) {
        let end = end.min(self.cells.len());
        if start < end {
            for cell in self.cells.get_mut(start..end).unwrap_or(&mut []) {
                cell.reset();
            }
        }
    }

    /// Insert `count` blank cells at `col`, shifting cells right. Excess cells are dropped.
    pub fn insert_cells(&mut self, col: usize, count: usize) {
        if col >= self.cells.len() {
            return;
        }
        for _ in 0..count {
            if col < self.cells.len() {
                self.cells.pop();
                self.cells.insert(col, Cell::blank());
            }
        }
    }

    /// Delete `count` cells at `col`, shifting cells left. Blank cells fill from the right.
    pub fn delete_cells(&mut self, col: usize, count: usize) {
        if col >= self.cells.len() {
            return;
        }
        let removable = count.min(self.cells.len() - col);
        for _ in 0..removable {
            if col < self.cells.len() {
                self.cells.remove(col);
            }
        }
        self.cells.resize(self.cells.len() + removable, Cell::blank());
    }
}
