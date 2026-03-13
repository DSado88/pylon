use crate::grid::cell::Cell;
use crate::grid::row::Row;

/// Converts Claude text output to grid rows for sidebar rendering.
pub struct SessionRenderer;

impl SessionRenderer {
    /// Render a text string into rows of cells, wrapping at column boundaries.
    pub fn render_text(text: &str, cols: usize) -> Vec<Row> {
        if cols == 0 {
            return Vec::new();
        }

        let mut rows = Vec::new();
        let mut current_row = Row::new(cols);
        let mut col = 0;

        for ch in text.chars() {
            if ch == '\n' {
                rows.push(current_row);
                current_row = Row::new(cols);
                col = 0;
                continue;
            }

            if col >= cols {
                rows.push(current_row);
                current_row = Row::new(cols);
                col = 0;
            }

            if let Some(cell) = current_row.get_mut(col) {
                *cell = Cell::with_char(ch);
            }
            col += 1;
        }

        // Push final row if it has content
        if col > 0 {
            rows.push(current_row);
        }

        rows
    }
}
