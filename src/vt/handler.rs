use std::sync::Arc;

use crate::grid::cell::{CellFlags, Color, NamedColor};
use crate::grid::storage::Grid;
use crate::primitives::DirtyRows;

use super::state::TerminalState;

/// VT handler implementing `vte::Perform`. Writes to the grid based on escape sequences.
pub struct VtHandler<'a> {
    grid: &'a mut Grid,
    state: &'a mut TerminalState,
    dirty: &'a Arc<DirtyRows>,
}

impl<'a> VtHandler<'a> {
    pub fn new(
        grid: &'a mut Grid,
        state: &'a mut TerminalState,
        dirty: &'a Arc<DirtyRows>,
    ) -> Self {
        Self { grid, state, dirty }
    }

    fn mark_dirty(&self, row: u16) {
        self.dirty.mark(row);
    }

    fn cols(&self) -> u16 {
        self.grid.cols() as u16
    }

    fn rows(&self) -> u16 {
        self.grid.visible_rows() as u16
    }

    /// Write a character at the current cursor position and advance.
    /// Uses delayed wrap: when printing at the last column, we set pending_wrap
    /// instead of wrapping immediately. The wrap happens on the next printable char.
    fn put_char(&mut self, c: char) {
        // If a wrap is pending from a previous char at end-of-line, execute it now
        if self.state.pending_wrap {
            self.state.pending_wrap = false;
            self.carriage_return();
            self.linefeed();
        }

        let row = self.state.cursor.row as usize;
        let col = self.state.cursor.col as usize;
        let (fg, bg, flags) = self.state.attrs;

        if self.state.insert_mode {
            if let Some(grid_row) = self.grid.row_mut(row) {
                grid_row.insert_cells(col, 1);
            }
        }

        if let Some(cell) = self.grid.cell_mut(row, col) {
            cell.ch = c;
            cell.fg = fg;
            cell.bg = bg;
            cell.flags = flags;
        }

        self.mark_dirty(self.state.cursor.row);

        // Advance cursor — delayed wrap at last column
        let cols = self.cols();
        if self.state.cursor.col + 1 < cols {
            self.state.cursor.col += 1;
        } else if self.state.autowrap {
            self.state.pending_wrap = true;
        }
    }

    fn carriage_return(&mut self) {
        self.state.pending_wrap = false;
        self.state.cursor.col = 0;
    }

    fn linefeed(&mut self) {
        let bottom = self.state.scroll_bottom();
        if self.state.cursor.row + 1 >= bottom {
            // At the bottom of scroll region: scroll up
            let top = self.state.scroll_top() as usize;
            let bot = bottom as usize;
            self.grid.scroll_up(top, bot, 1);
            // Mark the whole scroll region dirty
            for r in top..bot {
                self.dirty.mark(r as u16);
            }
        } else {
            self.state.cursor.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        let top = self.state.scroll_top();
        if self.state.cursor.row <= top {
            // At the top of scroll region: scroll down
            let bot = self.state.scroll_bottom() as usize;
            self.grid.scroll_down(top as usize, bot, 1);
            for r in (top as usize)..bot {
                self.dirty.mark(r as u16);
            }
        } else {
            self.state.cursor.row -= 1;
        }
    }

    fn tab(&mut self) {
        let cols = self.cols();
        let cur_col = self.state.cursor.col;
        // Find next tab stop
        let next = self.state.tab_stops.iter().find(|&&t| t > cur_col);
        self.state.cursor.col = match next {
            Some(&t) => t.min(cols.saturating_sub(1)),
            None => cols.saturating_sub(1),
        };
    }

    fn backspace(&mut self) {
        self.state.pending_wrap = false;
        if self.state.cursor.col > 0 {
            self.state.cursor.col -= 1;
        }
    }

    fn erase_display(&mut self, mode: u16) {
        let rows = self.rows();
        let cols = self.cols() as usize;
        match mode {
            0 => {
                // Erase from cursor to end of display
                let row = self.state.cursor.row as usize;
                let col = self.state.cursor.col as usize;
                // Current row from cursor
                if let Some(r) = self.grid.row_mut(row) {
                    r.clear_from(col);
                }
                self.mark_dirty(self.state.cursor.row);
                // Remaining rows
                for r in (row + 1)..(rows as usize) {
                    if let Some(grid_row) = self.grid.row_mut(r) {
                        grid_row.clear();
                    }
                    self.dirty.mark(r as u16);
                }
            }
            1 => {
                // Erase from start to cursor
                let row = self.state.cursor.row as usize;
                let col = self.state.cursor.col as usize;
                for r in 0..row {
                    if let Some(grid_row) = self.grid.row_mut(r) {
                        grid_row.clear();
                    }
                    self.dirty.mark(r as u16);
                }
                if let Some(grid_row) = self.grid.row_mut(row) {
                    grid_row.clear_to(col + 1);
                }
                self.mark_dirty(self.state.cursor.row);
            }
            2 | 3 => {
                // Erase entire display
                self.grid.clear_visible();
                self.dirty.mark_all();
                let _ = cols; // suppress unused
            }
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: u16) {
        let row = self.state.cursor.row as usize;
        let col = self.state.cursor.col as usize;
        match mode {
            0 => {
                // Erase from cursor to end of line
                if let Some(grid_row) = self.grid.row_mut(row) {
                    grid_row.clear_from(col);
                }
            }
            1 => {
                // Erase from start to cursor
                if let Some(grid_row) = self.grid.row_mut(row) {
                    grid_row.clear_to(col + 1);
                }
            }
            2 => {
                // Erase entire line
                if let Some(grid_row) = self.grid.row_mut(row) {
                    grid_row.clear();
                }
            }
            _ => {}
        }
        self.mark_dirty(self.state.cursor.row);
    }

    fn sgr(&mut self, params: &[&[u16]]) {
        let mut i = 0;
        while i < params.len() {
            let p = params.get(i).and_then(|sub| sub.first().copied()).unwrap_or(0);
            match p {
                0 => {
                    // Reset
                    self.state.attrs = (Color::Default, Color::Default, CellFlags::empty());
                }
                1 => self.state.attrs.2.insert(CellFlags::BOLD),
                2 => self.state.attrs.2.insert(CellFlags::DIM),
                3 => self.state.attrs.2.insert(CellFlags::ITALIC),
                4 => self.state.attrs.2.insert(CellFlags::UNDERLINE),
                7 => self.state.attrs.2.insert(CellFlags::INVERSE),
                8 => self.state.attrs.2.insert(CellFlags::HIDDEN),
                9 => self.state.attrs.2.insert(CellFlags::STRIKETHROUGH),
                21 => self.state.attrs.2.remove(CellFlags::BOLD),
                22 => {
                    self.state.attrs.2.remove(CellFlags::BOLD);
                    self.state.attrs.2.remove(CellFlags::DIM);
                }
                23 => self.state.attrs.2.remove(CellFlags::ITALIC),
                24 => self.state.attrs.2.remove(CellFlags::UNDERLINE),
                27 => self.state.attrs.2.remove(CellFlags::INVERSE),
                28 => self.state.attrs.2.remove(CellFlags::HIDDEN),
                29 => self.state.attrs.2.remove(CellFlags::STRIKETHROUGH),
                // Foreground colors
                30 => self.state.attrs.0 = Color::Named(NamedColor::Black),
                31 => self.state.attrs.0 = Color::Named(NamedColor::Red),
                32 => self.state.attrs.0 = Color::Named(NamedColor::Green),
                33 => self.state.attrs.0 = Color::Named(NamedColor::Yellow),
                34 => self.state.attrs.0 = Color::Named(NamedColor::Blue),
                35 => self.state.attrs.0 = Color::Named(NamedColor::Magenta),
                36 => self.state.attrs.0 = Color::Named(NamedColor::Cyan),
                37 => self.state.attrs.0 = Color::Named(NamedColor::White),
                38 => {
                    // Extended foreground
                    if let Some(color) = self.parse_extended_color(params, &mut i) {
                        self.state.attrs.0 = color;
                    }
                }
                39 => self.state.attrs.0 = Color::Default,
                // Background colors
                40 => self.state.attrs.1 = Color::Named(NamedColor::Black),
                41 => self.state.attrs.1 = Color::Named(NamedColor::Red),
                42 => self.state.attrs.1 = Color::Named(NamedColor::Green),
                43 => self.state.attrs.1 = Color::Named(NamedColor::Yellow),
                44 => self.state.attrs.1 = Color::Named(NamedColor::Blue),
                45 => self.state.attrs.1 = Color::Named(NamedColor::Magenta),
                46 => self.state.attrs.1 = Color::Named(NamedColor::Cyan),
                47 => self.state.attrs.1 = Color::Named(NamedColor::White),
                48 => {
                    // Extended background
                    if let Some(color) = self.parse_extended_color(params, &mut i) {
                        self.state.attrs.1 = color;
                    }
                }
                49 => self.state.attrs.1 = Color::Default,
                // Bright foreground
                90 => self.state.attrs.0 = Color::Named(NamedColor::BrightBlack),
                91 => self.state.attrs.0 = Color::Named(NamedColor::BrightRed),
                92 => self.state.attrs.0 = Color::Named(NamedColor::BrightGreen),
                93 => self.state.attrs.0 = Color::Named(NamedColor::BrightYellow),
                94 => self.state.attrs.0 = Color::Named(NamedColor::BrightBlue),
                95 => self.state.attrs.0 = Color::Named(NamedColor::BrightMagenta),
                96 => self.state.attrs.0 = Color::Named(NamedColor::BrightCyan),
                97 => self.state.attrs.0 = Color::Named(NamedColor::BrightWhite),
                // Bright background
                100 => self.state.attrs.1 = Color::Named(NamedColor::BrightBlack),
                101 => self.state.attrs.1 = Color::Named(NamedColor::BrightRed),
                102 => self.state.attrs.1 = Color::Named(NamedColor::BrightGreen),
                103 => self.state.attrs.1 = Color::Named(NamedColor::BrightYellow),
                104 => self.state.attrs.1 = Color::Named(NamedColor::BrightBlue),
                105 => self.state.attrs.1 = Color::Named(NamedColor::BrightMagenta),
                106 => self.state.attrs.1 = Color::Named(NamedColor::BrightCyan),
                107 => self.state.attrs.1 = Color::Named(NamedColor::BrightWhite),
                _ => {}
            }
            i += 1;
        }
    }

    fn parse_extended_color(&self, params: &[&[u16]], i: &mut usize) -> Option<Color> {
        // Check for colon-separated subparams first (e.g., 38:2:r:g:b or 38:5:idx)
        let current = params.get(*i)?;
        if current.len() > 1 {
            // Subparameters via colons
            let mode = current.get(1).copied()?;
            match mode {
                2 => {
                    // RGB via subparams: 38:2:colorspace:r:g:b or 38:2:r:g:b
                    if current.len() >= 5 {
                        // 38:2:cs:r:g:b
                        let r = current.get(3).copied()? as u8;
                        let g = current.get(4).copied()? as u8;
                        let b = current.get(5).copied().unwrap_or(0) as u8;
                        return Some(Color::Rgb(r, g, b));
                    } else if current.len() >= 4 {
                        // 38:2:r:g:b (no colorspace)
                        let r = current.get(2).copied()? as u8;
                        let g = current.get(3).copied()? as u8;
                        let b = current.get(4).copied().unwrap_or(0) as u8;
                        return Some(Color::Rgb(r, g, b));
                    }
                }
                5 => {
                    // Indexed via subparams: 38:5:idx
                    let idx = current.get(2).copied()? as u8;
                    return Some(Color::Indexed(idx));
                }
                _ => {}
            }
            return None;
        }

        // Semicolon-separated: 38;2;r;g;b or 38;5;idx
        let mode_param = params.get(*i + 1).and_then(|sub| sub.first().copied())?;
        match mode_param {
            2 => {
                // 38;2;r;g;b
                let r = params.get(*i + 2).and_then(|sub| sub.first().copied()).unwrap_or(0) as u8;
                let g = params.get(*i + 3).and_then(|sub| sub.first().copied()).unwrap_or(0) as u8;
                let b = params.get(*i + 4).and_then(|sub| sub.first().copied()).unwrap_or(0) as u8;
                *i += 4;
                Some(Color::Rgb(r, g, b))
            }
            5 => {
                // 38;5;idx
                let idx = params.get(*i + 2).and_then(|sub| sub.first().copied()).unwrap_or(0) as u8;
                *i += 2;
                Some(Color::Indexed(idx))
            }
            _ => None,
        }
    }

    fn set_mode(&mut self, params: &[&[u16]], intermediates: &[u8], enable: bool) {
        let is_private = intermediates.first().copied() == Some(b'?');

        for param_sub in params {
            let param = param_sub.first().copied().unwrap_or(0);
            if is_private {
                match param {
                    1 => self.state.application_cursor_keys = enable,
                    6 => {
                        self.state.origin_mode = enable;
                        if enable {
                            self.state.cursor.row = self.state.scroll_top();
                            self.state.cursor.col = 0;
                        }
                    }
                    7 => self.state.autowrap = enable,
                    25 => self.state.cursor.visible = enable,
                    1000 => {
                        self.state.mouse_mode = if enable {
                            super::state::MouseMode::Press
                        } else {
                            super::state::MouseMode::None
                        };
                    }
                    1002 => {
                        self.state.mouse_mode = if enable {
                            super::state::MouseMode::PressRelease
                        } else {
                            super::state::MouseMode::None
                        };
                    }
                    1006 => {
                        self.state.mouse_mode = if enable {
                            super::state::MouseMode::Sgr
                        } else {
                            super::state::MouseMode::None
                        };
                    }
                    1049 => {
                        // Alternate screen buffer
                        if enable && !self.state.alternate_screen {
                            self.state.saved_cursor = Some(self.state.cursor.clone());
                            self.grid.clear_visible();
                            self.state.alternate_screen = true;
                            self.dirty.mark_all();
                        } else if !enable && self.state.alternate_screen {
                            if let Some(saved) = self.state.saved_cursor.take() {
                                self.state.cursor = saved;
                            }
                            self.state.alternate_screen = false;
                            self.dirty.mark_all();
                        }
                    }
                    2004 => self.state.bracketed_paste = enable,
                    _ => {}
                }
            } else {
                if param == 4 {
                    self.state.insert_mode = enable;
                }
            }
        }
    }
}

impl<'a> vte::Perform for VtHandler<'a> {
    fn print(&mut self, c: char) {
        self.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => {
                // BEL - ignore (could trigger system bell)
            }
            0x08 => self.backspace(),
            0x09 => self.tab(),
            0x0A..=0x0C => self.linefeed(),
            0x0D => self.carriage_return(),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], _ignore: bool, action: char) {
        // Collect params into a Vec<&[u16]> for easier access
        let param_list: Vec<&[u16]> = params.iter().collect();

        // Helper: get param value with default
        let param = |idx: usize, default: u16| -> u16 {
            param_list
                .get(idx)
                .and_then(|sub| sub.first().copied())
                .map(|v| if v == 0 { default } else { v })
                .unwrap_or(default)
        };

        match action {
            'A' => {
                // Cursor Up
                self.state.pending_wrap = false;
                let n = param(0, 1);
                self.state.cursor.row = self.state.cursor.row.saturating_sub(n);
            }
            'B' => {
                // Cursor Down
                self.state.pending_wrap = false;
                let n = param(0, 1);
                let max_row = self.rows().saturating_sub(1);
                self.state.cursor.row = (self.state.cursor.row + n).min(max_row);
            }
            'C' => {
                // Cursor Forward
                self.state.pending_wrap = false;
                let n = param(0, 1);
                let max_col = self.cols().saturating_sub(1);
                self.state.cursor.col = (self.state.cursor.col + n).min(max_col);
            }
            'D' => {
                // Cursor Back
                self.state.pending_wrap = false;
                let n = param(0, 1);
                self.state.cursor.col = self.state.cursor.col.saturating_sub(n);
            }
            'G' => {
                // Cursor Horizontal Absolute (CHA)
                self.state.pending_wrap = false;
                let col = param(0, 1).saturating_sub(1); // 1-based to 0-based
                let max_col = self.cols().saturating_sub(1);
                self.state.cursor.col = col.min(max_col);
            }
            'H' | 'f' => {
                // Cursor Position (CUP)
                self.state.pending_wrap = false;
                let row = param(0, 1).saturating_sub(1);
                let col = param(1, 1).saturating_sub(1);
                let max_row = self.rows().saturating_sub(1);
                let max_col = self.cols().saturating_sub(1);

                if self.state.origin_mode {
                    let top = self.state.scroll_top();
                    let bottom = self.state.scroll_bottom();
                    self.state.cursor.row = (top + row).min(bottom.saturating_sub(1));
                } else {
                    self.state.cursor.row = row.min(max_row);
                }
                self.state.cursor.col = col.min(max_col);
            }
            'J' => {
                // Erase Display (ED)
                let mode = param(0, 0);
                self.erase_display(mode);
            }
            'K' => {
                // Erase Line (EL)
                let mode = param(0, 0);
                self.erase_line(mode);
            }
            'L' => {
                // Insert Lines (IL)
                let n = param(0, 1) as usize;
                let row = self.state.cursor.row as usize;
                let top = self.state.scroll_top() as usize;
                let bottom = self.state.scroll_bottom() as usize;
                self.grid.insert_lines(row, n, top, bottom);
                for r in top..bottom {
                    self.dirty.mark(r as u16);
                }
            }
            'M' => {
                // Delete Lines (DL)
                let n = param(0, 1) as usize;
                let row = self.state.cursor.row as usize;
                let top = self.state.scroll_top() as usize;
                let bottom = self.state.scroll_bottom() as usize;
                self.grid.delete_lines(row, n, top, bottom);
                for r in top..bottom {
                    self.dirty.mark(r as u16);
                }
            }
            'S' => {
                // Scroll Up (SU)
                let n = param(0, 1) as usize;
                let top = self.state.scroll_top() as usize;
                let bottom = self.state.scroll_bottom() as usize;
                self.grid.scroll_up(top, bottom, n);
                for r in top..bottom {
                    self.dirty.mark(r as u16);
                }
            }
            'T' => {
                // Scroll Down (SD)
                let n = param(0, 1) as usize;
                let top = self.state.scroll_top() as usize;
                let bottom = self.state.scroll_bottom() as usize;
                self.grid.scroll_down(top, bottom, n);
                for r in top..bottom {
                    self.dirty.mark(r as u16);
                }
            }
            '@' => {
                // Insert Characters (ICH)
                let n = param(0, 1) as usize;
                let row = self.state.cursor.row as usize;
                let col = self.state.cursor.col as usize;
                if let Some(grid_row) = self.grid.row_mut(row) {
                    grid_row.insert_cells(col, n);
                }
                self.mark_dirty(self.state.cursor.row);
            }
            'P' => {
                // Delete Characters (DCH)
                let n = param(0, 1) as usize;
                let row = self.state.cursor.row as usize;
                let col = self.state.cursor.col as usize;
                if let Some(grid_row) = self.grid.row_mut(row) {
                    grid_row.delete_cells(col, n);
                }
                self.mark_dirty(self.state.cursor.row);
            }
            'X' => {
                // Erase Characters (ECH)
                let n = param(0, 1) as usize;
                let row = self.state.cursor.row as usize;
                let col = self.state.cursor.col as usize;
                if let Some(grid_row) = self.grid.row_mut(row) {
                    grid_row.clear_range(col, col + n);
                }
                self.mark_dirty(self.state.cursor.row);
            }
            'd' => {
                // Vertical Position Absolute (VPA)
                self.state.pending_wrap = false;
                let row = param(0, 1).saturating_sub(1);
                let max_row = self.rows().saturating_sub(1);
                self.state.cursor.row = row.min(max_row);
            }
            'm' => {
                // SGR
                if param_list.is_empty() {
                    // No params = reset
                    self.state.attrs = (Color::Default, Color::Default, CellFlags::empty());
                } else {
                    self.sgr(&param_list);
                }
            }
            'r' => {
                // Set Scroll Region (DECSTBM)
                let top = param(0, 1).saturating_sub(1);
                let bottom_param = param_list
                    .get(1)
                    .and_then(|sub| sub.first().copied())
                    .unwrap_or(0);
                let bottom = if bottom_param == 0 {
                    self.rows()
                } else {
                    bottom_param.min(self.rows())
                };

                if top < bottom {
                    self.state.scroll_region = (top, bottom);
                    // Move cursor to home
                    if self.state.origin_mode {
                        self.state.cursor.row = top;
                    } else {
                        self.state.cursor.row = 0;
                    }
                    self.state.cursor.col = 0;
                }
            }
            'h' => {
                // Set Mode
                self.set_mode(&param_list, intermediates, true);
            }
            'l' => {
                // Reset Mode
                self.set_mode(&param_list, intermediates, false);
            }
            _ => {
                // Unhandled CSI sequence
            }
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        match (intermediates.first(), byte) {
            (None, b'7') => {
                // Save cursor (DECSC)
                self.state.saved_cursor = Some(self.state.cursor.clone());
            }
            (None, b'8') => {
                // Restore cursor (DECRC)
                if let Some(saved) = self.state.saved_cursor.clone() {
                    self.state.cursor = saved;
                }
            }
            (None, b'D') => {
                // Index (IND) - move cursor down, scroll if at bottom
                self.linefeed();
            }
            (None, b'M') => {
                // Reverse Index (RI) - move cursor up, scroll if at top
                self.reverse_index();
            }
            (None, b'c') => {
                // Full reset (RIS)
                let rows = self.rows();
                let cols = self.cols();
                *self.state = TerminalState::new(rows, cols);
                self.grid.clear_visible();
                self.dirty.mark_all();
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.is_empty() {
            return;
        }

        let command = params.first().copied().unwrap_or(&[]);
        match command {
            b"0" | b"2" => {
                // Set window title
                if let Some(title_bytes) = params.get(1) {
                    if let Ok(title) = std::str::from_utf8(title_bytes) {
                        self.state.window_title = title.to_string();
                    }
                }
            }
            _ => {}
        }
    }

    fn hook(&mut self, _params: &vte::Params, _intermediates: &[u8], _ignore: bool, _action: char) {
        // DCS sequences - not needed for basic operation
    }

    fn unhook(&mut self) {}

    fn put(&mut self, _byte: u8) {
        // DCS data - not needed for basic operation
    }
}

/// Public convenience: write a character at cursor pos, used by the SIMD fast path.
/// Uses delayed wrap matching the VtHandler behavior.
pub fn bulk_print_char(
    grid: &mut Grid,
    state: &mut TerminalState,
    dirty: &Arc<DirtyRows>,
    c: char,
) {
    // Execute pending wrap before printing
    if state.pending_wrap {
        state.pending_wrap = false;
        state.cursor.col = 0;
        let bottom = state.scroll_bottom();
        if state.cursor.row + 1 >= bottom {
            let top = state.scroll_top() as usize;
            let bot = bottom as usize;
            grid.scroll_up(top, bot, 1);
            for r in top..bot {
                dirty.mark(r as u16);
            }
        } else {
            state.cursor.row += 1;
        }
    }

    let row = state.cursor.row as usize;
    let col = state.cursor.col as usize;
    let (fg, bg, flags) = state.attrs;

    if let Some(cell) = grid.cell_mut(row, col) {
        cell.ch = c;
        cell.fg = fg;
        cell.bg = bg;
        cell.flags = flags;
    }

    dirty.mark(state.cursor.row);

    // Delayed wrap at last column
    let cols = grid.cols() as u16;
    if state.cursor.col + 1 < cols {
        state.cursor.col += 1;
    } else if state.autowrap {
        state.pending_wrap = true;
    }
}
