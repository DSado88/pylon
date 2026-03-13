use bitflags::bitflags;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct CellFlags: u8 {
        const BOLD          = 0b0000_0001;
        const ITALIC        = 0b0000_0010;
        const UNDERLINE     = 0b0000_0100;
        const STRIKETHROUGH = 0b0000_1000;
        const INVERSE       = 0b0001_0000;
        const DIM           = 0b0010_0000;
        const HIDDEN        = 0b0100_0000;
        const WIDE_CHAR     = 0b1000_0000;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedColor {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Named(NamedColor),
    Indexed(u8),
    Rgb(u8, u8, u8),
    Default,
}

impl Default for Color {
    fn default() -> Self {
        Self::Default
    }
}

/// Bundled foreground, background, and flags for a cell.
pub type CellAttributes = (Color, Color, CellFlags);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: CellFlags,
}

impl Cell {
    pub fn blank() -> Self {
        Self {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
        }
    }

    pub fn with_char(ch: char) -> Self {
        Self {
            ch,
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
        }
    }

    pub fn reset(&mut self) {
        self.ch = ' ';
        self.fg = Color::Default;
        self.bg = Color::Default;
        self.flags = CellFlags::empty();
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self::blank()
    }
}
