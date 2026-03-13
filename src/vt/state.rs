use crate::grid::cell::{CellAttributes, CellFlags, Color};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseMode {
    None,
    Press,
    PressRelease,
    Sgr,
}

impl Default for MouseMode {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Debug, Clone)]
pub struct CursorState {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
}

impl Default for CursorState {
    fn default() -> Self {
        Self {
            row: 0,
            col: 0,
            visible: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TerminalState {
    pub cursor: CursorState,
    pub saved_cursor: Option<CursorState>,
    pub attrs: CellAttributes,
    pub scroll_region: (u16, u16), // (top, bottom), 0-indexed, bottom is exclusive
    pub alternate_screen: bool,
    pub autowrap: bool,
    pub origin_mode: bool,
    pub bracketed_paste: bool,
    pub mouse_mode: MouseMode,
    pub application_cursor_keys: bool,
    pub tab_stops: Vec<u16>,
    pub insert_mode: bool,
    pub pending_wrap: bool,
    pub window_title: String,
}

impl TerminalState {
    pub fn new(rows: u16, cols: u16) -> Self {
        // Default tab stops every 8 columns
        let tab_stops: Vec<u16> = (0..cols).filter(|c| c % 8 == 0).collect();

        Self {
            cursor: CursorState::default(),
            saved_cursor: None,
            attrs: (Color::Default, Color::Default, CellFlags::empty()),
            scroll_region: (0, rows),
            alternate_screen: false,
            autowrap: true,
            origin_mode: false,
            bracketed_paste: false,
            mouse_mode: MouseMode::default(),
            application_cursor_keys: false,
            tab_stops,
            insert_mode: false,
            pending_wrap: false,
            window_title: String::new(),
        }
    }

    /// Clamp cursor to within visible bounds.
    pub fn clamp_cursor(&mut self, rows: u16, cols: u16) {
        if cols > 0 {
            self.cursor.col = self.cursor.col.min(cols - 1);
        }
        if rows > 0 {
            self.cursor.row = self.cursor.row.min(rows - 1);
        }
    }

    /// Get the effective scroll region top/bottom.
    pub fn scroll_top(&self) -> u16 {
        self.scroll_region.0
    }

    pub fn scroll_bottom(&self) -> u16 {
        self.scroll_region.1
    }
}
