use bitflags::bitflags;
use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

bitflags! {
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct AttrsFlags: u16 {
        const BOLD = 1 << 0;
        const ITALIC = 1 << 1;
        const UNDERLINE = 1 << 2;
        const INVERSE = 1 << 3;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attrs {
    pub fg: Color,
    pub bg: Color,
    pub flags: AttrsFlags,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub attrs: Attrs,
}

impl Cell {
    #[must_use]
    pub fn blank(attrs: Attrs) -> Self {
        Self { ch: ' ', attrs }
    }

    #[must_use]
    pub fn continuation(attrs: Attrs) -> Self {
        Self { ch: '\0', attrs }
    }

    #[must_use]
    pub fn is_continuation(&self) -> bool {
        self.ch == '\0'
    }

    #[must_use]
    pub fn display_width(&self) -> usize {
        if self.is_continuation() {
            0
        } else {
            UnicodeWidthChar::width(self.ch).unwrap_or(1).max(1)
        }
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self::blank(Attrs::default())
    }
}
