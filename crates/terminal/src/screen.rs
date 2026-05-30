use std::collections::VecDeque;

use shell_core::TerminalSize;

use crate::{Attrs, AttrsFlags, Cell, Color};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
}

/// 光标位置和可见性（DECTCEM）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorState {
    pub pos: Cursor,
    pub visible: bool,
}

impl Default for CursorState {
    fn default() -> Self {
        Self {
            pos: Cursor::default(),
            visible: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtyRegion {
    lines: Vec<bool>,
}

impl DirtyRegion {
    #[must_use]
    pub fn new(height: usize) -> Self {
        Self {
            lines: vec![true; height],
        }
    }

    pub fn resize(&mut self, height: usize) {
        self.lines.resize(height, true);
    }

    pub fn mark_line(&mut self, row: usize) {
        if let Some(line) = self.lines.get_mut(row) {
            *line = true;
        }
    }

    pub fn mark_all(&mut self) {
        self.lines.fill(true);
    }

    #[must_use]
    pub fn dirty_lines(&self) -> Vec<usize> {
        self.lines
            .iter()
            .enumerate()
            .filter_map(|(idx, dirty)| dirty.then_some(idx))
            .collect()
    }

    pub fn clear(&mut self) {
        self.lines.fill(false);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSnapshot {
    pub size: TerminalSize,
    pub cursor: CursorState,
    pub lines: Vec<Vec<Cell>>,
    /// 回滚历史行，最旧的在前。
    pub scrollback: Vec<Vec<Cell>>,
}

#[derive(Debug, Clone)]
pub struct TerminalBuffer {
    width: usize,
    height: usize,
    lines: VecDeque<Vec<Cell>>,
    /// 进入备用屏幕时保存的主屏幕行。
    alt_lines: Option<VecDeque<Vec<Cell>>>,
    scrollback: VecDeque<Vec<Cell>>,
    max_scrollback: usize,
    cursor: CursorState,
    /// 保存的光标位置（DECSC/DECRC）。
    saved_cursor: CursorState,
    attrs: Attrs,
    dirty: DirtyRegion,
    /// 滚动区域顶部行，0 基且包含该行。
    scroll_top: usize,
    /// 滚动区域底部行，0 基且包含该行。
    scroll_bottom: usize,
    /// 最后一个打印字符落在末列时，终端会把光标保持在屏幕内，直到下一个可打印字符
    /// 到来时才真正换行。
    wrap_pending: bool,
}

impl TerminalBuffer {
    #[must_use]
    pub fn new(size: TerminalSize, max_scrollback: usize) -> Self {
        let width = usize::from(size.cols.max(1));
        let height = usize::from(size.rows.max(1));
        Self {
            width,
            height,
            lines: (0..height)
                .map(|_| vec![Cell::blank(Attrs::default()); width])
                .collect(),
            alt_lines: None,
            scrollback: VecDeque::with_capacity(max_scrollback.min(1024)),
            max_scrollback,
            cursor: CursorState::default(),
            saved_cursor: CursorState::default(),
            attrs: Attrs::default(),
            dirty: DirtyRegion::new(height),
            scroll_top: 0,
            scroll_bottom: height.saturating_sub(1),
            wrap_pending: false,
        }
    }

    #[must_use]
    pub fn size(&self) -> TerminalSize {
        TerminalSize::new(self.width as u16, self.height as u16)
    }

    #[must_use]
    pub fn cursor(&self) -> Cursor {
        self.cursor.pos
    }

    #[must_use]
    pub fn cursor_state(&self) -> CursorState {
        self.cursor
    }

    #[must_use]
    pub fn current_attrs(&self) -> Attrs {
        self.attrs
    }

    #[must_use]
    pub fn snapshot(&self) -> TerminalSnapshot {
        TerminalSnapshot {
            size: self.size(),
            cursor: self.cursor,
            lines: self.lines.iter().cloned().collect(),
            scrollback: self.scrollback.iter().cloned().collect(),
        }
    }

    #[must_use]
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    #[must_use]
    pub fn total_line_count(&self) -> usize {
        self.scrollback.len() + self.lines.len()
    }

    #[must_use]
    pub fn line_at(&self, index: usize) -> Option<&[Cell]> {
        if index < self.scrollback.len() {
            self.scrollback.get(index).map(Vec::as_slice)
        } else {
            self.lines
                .get(index.saturating_sub(self.scrollback.len()))
                .map(Vec::as_slice)
        }
    }

    #[must_use]
    pub fn line_text(&self, row: usize) -> Option<String> {
        self.lines.get(row).map(|line| {
            line.iter()
                .filter(|cell| !cell.is_continuation())
                .map(|cell| cell.ch)
                .collect::<String>()
        })
    }

    #[must_use]
    pub fn dirty_lines(&self) -> Vec<usize> {
        self.dirty.dirty_lines()
    }

    pub fn clear_dirty(&mut self) {
        self.dirty.clear();
    }

    pub fn resize(&mut self, size: TerminalSize) {
        let new_width = usize::from(size.cols.max(1));
        let new_height = usize::from(size.rows.max(1));

        for line in &mut self.lines {
            line.resize(new_width, Cell::blank(self.attrs));
        }

        if new_height > self.height {
            while self.lines.len() < new_height {
                self.lines
                    .push_back(vec![Cell::blank(self.attrs); new_width]);
            }
        } else {
            self.lines.truncate(new_height);
        }

        self.width = new_width;
        self.height = new_height;
        self.cursor.pos.row = self.cursor.pos.row.min(self.height.saturating_sub(1));
        self.cursor.pos.col = self.cursor.pos.col.min(self.width.saturating_sub(1));
        self.dirty.resize(self.height);
        self.dirty.mark_all();
        self.scroll_top = 0;
        self.scroll_bottom = self.height.saturating_sub(1);
        self.wrap_pending = false;
    }

    // ── Character output ──────────────────────────────────────────────────

    pub fn put_char(&mut self, ch: char) {
        match ch {
            '\n' => {
                self.new_line();
                return;
            }
            '\r' => {
                self.carriage_return();
                return;
            }
            '\u{8}' => {
                self.backspace();
                return;
            }
            _ => {}
        }

        let char_width = Cell {
            ch,
            attrs: self.attrs,
        }
        .display_width();
        if char_width == 0 {
            return;
        }

        if self.wrap_pending {
            self.new_line();
            self.wrap_pending = false;
        }

        if char_width > self.width.saturating_sub(self.cursor.pos.col) {
            self.new_line();
        }

        if self.cursor.pos.col >= self.width {
            self.cursor.pos.col = self.width.saturating_sub(1);
        }

        if let Some(line) = self.lines.get_mut(self.cursor.pos.row) {
            if let Some(cell) = line.get_mut(self.cursor.pos.col) {
                *cell = Cell {
                    ch,
                    attrs: self.attrs,
                };
            }
            for offset in 1..char_width {
                if let Some(cell) = line.get_mut(self.cursor.pos.col + offset) {
                    *cell = Cell::continuation(self.attrs);
                }
            }
            self.dirty.mark_line(self.cursor.pos.row);
        }

        if self.cursor.pos.col + char_width >= self.width {
            self.wrap_pending = true;
        } else {
            self.cursor.pos.col += char_width;
        }
    }

    pub fn tab(&mut self) {
        let next_tab = ((self.cursor.pos.col / 8) + 1) * 8;
        while self.cursor.pos.col < next_tab.min(self.width) {
            self.put_char(' ');
        }
    }

    pub fn new_line(&mut self) {
        self.wrap_pending = false;
        self.cursor.pos.col = 0;
        if self.cursor.pos.row >= self.scroll_bottom {
            self.scroll_up_region(1);
        } else {
            self.cursor.pos.row += 1;
            self.dirty.mark_line(self.cursor.pos.row);
        }
    }

    pub fn carriage_return(&mut self) {
        self.wrap_pending = false;
        self.cursor.pos.col = 0;
    }

    pub fn backspace(&mut self) {
        self.wrap_pending = false;
        self.cursor.pos.col = self.cursor.pos.col.saturating_sub(1);
    }

    // ── Cursor movement ───────────────────────────────────────────────────

    pub fn cursor_up(&mut self, n: usize) {
        self.wrap_pending = false;
        self.cursor.pos.row = self.cursor.pos.row.saturating_sub(n.max(1));
    }

    pub fn cursor_down(&mut self, n: usize) {
        self.wrap_pending = false;
        self.cursor.pos.row = (self.cursor.pos.row + n.max(1)).min(self.height.saturating_sub(1));
    }

    pub fn cursor_forward(&mut self, n: usize) {
        self.wrap_pending = false;
        self.cursor.pos.col = (self.cursor.pos.col + n.max(1)).min(self.width.saturating_sub(1));
    }

    pub fn cursor_backward(&mut self, n: usize) {
        self.wrap_pending = false;
        self.cursor.pos.col = self.cursor.pos.col.saturating_sub(n.max(1));
    }

    pub fn move_cursor(&mut self, row: usize, col: usize) {
        self.wrap_pending = false;
        self.cursor.pos.row = row.min(self.height.saturating_sub(1));
        self.cursor.pos.col = col.min(self.width.saturating_sub(1));
    }

    /// CHA：光标水平绝对定位，列号从 1 开始。
    pub fn cursor_col(&mut self, col: usize) {
        self.wrap_pending = false;
        self.cursor.pos.col = col.saturating_sub(1).min(self.width.saturating_sub(1));
    }

    /// VPA：光标垂直绝对定位，行号从 1 开始。
    pub fn cursor_row(&mut self, row: usize) {
        self.wrap_pending = false;
        self.cursor.pos.row = row.saturating_sub(1).min(self.height.saturating_sub(1));
    }

    /// DECSC：保存光标。
    pub fn save_cursor(&mut self) {
        self.saved_cursor = self.cursor;
    }

    /// DECRC：恢复光标。
    pub fn restore_cursor(&mut self) {
        self.cursor = self.saved_cursor;
        self.cursor.pos.row = self.cursor.pos.row.min(self.height.saturating_sub(1));
        self.cursor.pos.col = self.cursor.pos.col.min(self.width.saturating_sub(1));
        self.wrap_pending = false;
    }

    pub fn set_cursor_visible(&mut self, visible: bool) {
        self.cursor.visible = visible;
    }

    // ── Erase operations ──────────────────────────────────────────────────

    pub fn clear_screen(&mut self) {
        for line in &mut self.lines {
            line.fill(Cell::blank(self.attrs));
        }
        self.cursor.pos = Cursor::default();
        self.dirty.mark_all();
    }

    /// ED：擦除显示区域（0=到末尾，1=到开头，2/3=全部）。
    pub fn erase_display(&mut self, mode: u16) {
        let row = self.cursor.pos.row;
        let col = self.cursor.pos.col;
        match mode {
            0 => {
                if let Some(line) = self.lines.get_mut(row) {
                    for cell in line.iter_mut().skip(col) {
                        *cell = Cell::blank(self.attrs);
                    }
                    self.dirty.mark_line(row);
                }
                for r in (row + 1)..self.height {
                    if let Some(line) = self.lines.get_mut(r) {
                        line.fill(Cell::blank(self.attrs));
                        self.dirty.mark_line(r);
                    }
                }
            }
            1 => {
                for r in 0..row {
                    if let Some(line) = self.lines.get_mut(r) {
                        line.fill(Cell::blank(self.attrs));
                        self.dirty.mark_line(r);
                    }
                }
                if let Some(line) = self.lines.get_mut(row) {
                    for cell in line.iter_mut().take(col + 1) {
                        *cell = Cell::blank(self.attrs);
                    }
                    self.dirty.mark_line(row);
                }
            }
            _ => {
                for line in &mut self.lines {
                    line.fill(Cell::blank(self.attrs));
                }
                self.dirty.mark_all();
            }
        }
    }

    /// EL 0：从光标擦除到行尾。
    pub fn erase_line(&mut self) {
        self.erase_in_line(0);
    }

    /// EL：擦除行（0=到末尾，1=到开头，2=整行）。
    pub fn erase_in_line(&mut self, mode: u16) {
        let row = self.cursor.pos.row;
        let col = self.cursor.pos.col;
        if let Some(line) = self.lines.get_mut(row) {
            match mode {
                0 => {
                    for cell in line.iter_mut().skip(col) {
                        *cell = Cell::blank(self.attrs);
                    }
                }
                1 => {
                    for cell in line.iter_mut().take(col + 1) {
                        *cell = Cell::blank(self.attrs);
                    }
                }
                _ => {
                    line.fill(Cell::blank(self.attrs));
                }
            }
            self.dirty.mark_line(row);
        }
    }

    /// DCH：删除光标处的 n 个字符。
    pub fn delete_chars(&mut self, n: usize) {
        let row = self.cursor.pos.row;
        let col = self.cursor.pos.col;
        if let Some(line) = self.lines.get_mut(row) {
            let end = line.len();
            let n = n.max(1).min(end.saturating_sub(col));
            if n > 0 {
                line.drain(col..col + n);
                line.resize(end, Cell::blank(self.attrs));
                self.dirty.mark_line(row);
            }
        }
    }

    /// ICH：在光标处插入 n 个空白字符。
    pub fn insert_chars(&mut self, n: usize) {
        let row = self.cursor.pos.row;
        let col = self.cursor.pos.col;
        if let Some(line) = self.lines.get_mut(row) {
            let end = line.len();
            for _ in 0..n.max(1) {
                if col < line.len() {
                    line.insert(col, Cell::blank(self.attrs));
                }
            }
            line.truncate(end);
            self.dirty.mark_line(row);
        }
    }

    /// IL：在光标所在行插入 n 个空白行（限制在滚动区域内）。
    pub fn insert_lines(&mut self, n: usize) {
        let row = self.cursor.pos.row;
        for _ in 0..n.max(1) {
            if row < self.lines.len() {
                self.lines
                    .insert(row, vec![Cell::blank(self.attrs); self.width]);
                if self.lines.len() > self.height {
                    self.lines.truncate(self.height);
                }
            }
        }
        self.dirty.mark_all();
    }

    /// DL：从光标所在行删除 n 行。
    pub fn delete_lines(&mut self, n: usize) {
        let row = self.cursor.pos.row;
        for _ in 0..n.max(1) {
            if row < self.lines.len() {
                self.lines.remove(row);
                self.lines
                    .push_back(vec![Cell::blank(self.attrs); self.width]);
            }
        }
        self.dirty.mark_all();
    }

    // ── Scroll region ─────────────────────────────────────────────────────

    /// DECSTBM：设置滚动区域，参数从 1 开始且包含边界。
    pub fn set_scroll_region(&mut self, top: usize, bottom: usize) {
        let top = top.saturating_sub(1).min(self.height.saturating_sub(1));
        let bottom = bottom.saturating_sub(1).min(self.height.saturating_sub(1));
        if top < bottom {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
        }
        self.cursor.pos = Cursor::default();
    }

    // ── Alternate screen ──────────────────────────────────────────────────

    /// smcup：切换到备用屏幕。
    pub fn enter_alt_screen(&mut self) {
        if self.alt_lines.is_none() {
            self.save_cursor();
            self.alt_lines = Some(std::mem::replace(
                &mut self.lines,
                (0..self.height)
                    .map(|_| vec![Cell::blank(self.attrs); self.width])
                    .collect(),
            ));
            self.cursor.pos = Cursor::default();
            self.dirty.mark_all();
        }
    }

    /// rmcup：返回主屏幕。
    pub fn exit_alt_screen(&mut self) {
        if let Some(primary) = self.alt_lines.take() {
            self.lines = primary;
            self.restore_cursor();
            self.dirty.mark_all();
        }
    }

    // ── SGR ───────────────────────────────────────────────────────────────

    pub fn set_graphic_rendition(&mut self, params: &[u16]) {
        if params.is_empty() {
            self.attrs = Attrs::default();
            return;
        }

        let mut idx = 0;
        while idx < params.len() {
            match params[idx] {
                0 => self.attrs = Attrs::default(),
                1 => self.attrs.flags.insert(AttrsFlags::BOLD),
                3 => self.attrs.flags.insert(AttrsFlags::ITALIC),
                4 => self.attrs.flags.insert(AttrsFlags::UNDERLINE),
                7 => self.attrs.flags.insert(AttrsFlags::INVERSE),
                22 => self.attrs.flags.remove(AttrsFlags::BOLD),
                23 => self.attrs.flags.remove(AttrsFlags::ITALIC),
                24 => self.attrs.flags.remove(AttrsFlags::UNDERLINE),
                27 => self.attrs.flags.remove(AttrsFlags::INVERSE),
                30..=37 => self.attrs.fg = Color::Indexed((params[idx] - 30) as u8),
                40..=47 => self.attrs.bg = Color::Indexed((params[idx] - 40) as u8),
                90..=97 => self.attrs.fg = Color::Indexed((params[idx] - 90 + 8) as u8),
                100..=107 => self.attrs.bg = Color::Indexed((params[idx] - 100 + 8) as u8),
                39 => self.attrs.fg = Color::Default,
                49 => self.attrs.bg = Color::Default,
                38 | 48 => {
                    let target_fg = params[idx] == 38;
                    if params.get(idx + 1) == Some(&5) {
                        if let Some(&color) = params.get(idx + 2) {
                            if target_fg {
                                self.attrs.fg = Color::Indexed(color as u8);
                            } else {
                                self.attrs.bg = Color::Indexed(color as u8);
                            }
                            idx += 2;
                        }
                    } else if params.get(idx + 1) == Some(&2) && idx + 4 < params.len() {
                        let color = Color::Rgb(
                            params[idx + 2] as u8,
                            params[idx + 3] as u8,
                            params[idx + 4] as u8,
                        );
                        if target_fg {
                            self.attrs.fg = color;
                        } else {
                            self.attrs.bg = color;
                        }
                        idx += 4;
                    }
                }
                _ => {}
            }
            idx += 1;
        }
    }

    // ── Internal scroll helpers ───────────────────────────────────────────

    fn scroll_up_region(&mut self, n: usize) {
        let top = self.scroll_top;
        let bottom = self.scroll_bottom.min(self.height.saturating_sub(1));
        for _ in 0..n {
            if top > bottom || top >= self.lines.len() {
                break;
            }
            // 移除滚动区域顶部行。VecDeque 的 pop_front 是 O(1)，remove(i>0) 是 O(N)。
            let removed = if top == 0 {
                self.lines.pop_front()
            } else {
                self.lines.remove(top)
            };
            // 将被挤出的行写入回滚历史。
            if top == 0
                && self.max_scrollback > 0
                && let Some(line) = removed
            {
                if self.scrollback.len() == self.max_scrollback {
                    self.scrollback.pop_front();
                }
                self.scrollback.push_back(line);
            }
            // 在滚动区域底部插入空白行。push_back 是 O(1)，insert(i<len-1) 是 O(N)。
            let blank = vec![Cell::blank(self.attrs); self.width];
            if bottom >= self.lines.len() {
                self.lines.push_back(blank);
            } else {
                self.lines.insert(bottom, blank);
            }
        }
        self.dirty.mark_all();
    }

    /// SU：在滚动区域内向上滚动 n 行。
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll_up_region(n.max(1));
    }

    /// SD：在滚动区域内向下滚动 n 行。
    pub fn scroll_down(&mut self, n: usize) {
        let top = self.scroll_top;
        let bottom = self.scroll_bottom.min(self.height.saturating_sub(1));
        for _ in 0..n.max(1) {
            // 移除区域底部行；如果在尾部，pop_back 是 O(1)。
            if bottom + 1 >= self.lines.len() {
                self.lines.pop_back();
            } else if bottom < self.lines.len() {
                self.lines.remove(bottom);
            }
            // 在区域顶部插入空白行；如果在头部，push_front 是 O(1)。
            let blank = vec![Cell::blank(self.attrs); self.width];
            if top == 0 {
                self.lines.push_front(blank);
            } else {
                self.lines.insert(top, blank);
            }
        }
        self.dirty.mark_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_basic_text() {
        let mut buffer = TerminalBuffer::new(TerminalSize::new(10, 3), 10);
        buffer.put_char('o');
        buffer.put_char('k');

        assert_eq!(buffer.line_text(0).unwrap()[..2].to_string(), "ok");
        assert_eq!(buffer.cursor(), Cursor { row: 0, col: 2 });
    }

    #[test]
    fn keeps_cursor_visible_at_last_column_until_next_printable() {
        let mut buffer = TerminalBuffer::new(TerminalSize::new(5, 2), 10);
        for ch in ['a', 'b', 'c', 'd', 'e'] {
            buffer.put_char(ch);
        }

        assert_eq!(buffer.line_text(0).unwrap()[..5].to_string(), "abcde");
        assert_eq!(buffer.cursor(), Cursor { row: 0, col: 4 });

        buffer.put_char('f');

        assert_eq!(buffer.line_text(1).unwrap().chars().next(), Some('f'));
        assert_eq!(buffer.cursor(), Cursor { row: 1, col: 1 });
    }

    #[test]
    fn advances_two_columns_for_wide_characters() {
        let mut buffer = TerminalBuffer::new(TerminalSize::new(6, 2), 10);
        buffer.put_char('中');
        buffer.put_char('a');

        let snapshot = buffer.snapshot();
        assert_eq!(snapshot.lines[0][0].ch, '中');
        assert!(snapshot.lines[0][1].is_continuation());
        assert_eq!(snapshot.lines[0][2].ch, 'a');
        assert_eq!(buffer.line_text(0).unwrap().trim_end(), "中a");
        assert_eq!(buffer.cursor(), Cursor { row: 0, col: 3 });
    }

    #[test]
    fn scrolls_when_bottom_line_overflows() {
        let mut buffer = TerminalBuffer::new(TerminalSize::new(5, 2), 10);
        buffer.put_char('a');
        buffer.new_line();
        buffer.put_char('b');
        buffer.new_line();
        buffer.put_char('c');

        assert_eq!(buffer.line_text(0).unwrap().chars().next(), Some('b'));
        assert_eq!(buffer.line_text(1).unwrap().chars().next(), Some('c'));
    }

    #[test]
    fn clears_screen() {
        let mut buffer = TerminalBuffer::new(TerminalSize::new(5, 2), 10);
        buffer.put_char('x');
        buffer.clear_screen();

        assert!(buffer.line_text(0).unwrap().trim().is_empty());
        assert_eq!(buffer.cursor(), Cursor::default());
    }

    #[test]
    fn applies_sgr_attributes() {
        let mut buffer = TerminalBuffer::new(TerminalSize::new(5, 2), 10);
        buffer.set_graphic_rendition(&[1, 31]);

        assert!(buffer.current_attrs().flags.contains(AttrsFlags::BOLD));
        assert_eq!(buffer.current_attrs().fg, Color::Indexed(1));
    }

    #[test]
    fn cursor_movement() {
        let mut buffer = TerminalBuffer::new(TerminalSize::new(20, 10), 0);
        buffer.move_cursor(5, 10);
        buffer.cursor_up(2);
        assert_eq!(buffer.cursor().row, 3);
        buffer.cursor_down(3);
        assert_eq!(buffer.cursor().row, 6);
        buffer.cursor_forward(4);
        assert_eq!(buffer.cursor().col, 14);
        buffer.cursor_backward(5);
        assert_eq!(buffer.cursor().col, 9);
    }

    #[test]
    fn erase_in_line_to_end() {
        let mut buffer = TerminalBuffer::new(TerminalSize::new(10, 3), 0);
        for _ in 0..10 {
            buffer.put_char('x');
        }
        buffer.move_cursor(0, 5);
        buffer.erase_in_line(0);
        let text = buffer.line_text(0).unwrap();
        assert_eq!(&text[..5], "xxxxx");
        assert!(text[5..].chars().all(|c| c == ' '));
    }

    #[test]
    fn insert_and_delete_chars() {
        let mut buffer = TerminalBuffer::new(TerminalSize::new(10, 3), 0);
        for ch in "hello".chars() {
            buffer.put_char(ch);
        }
        buffer.move_cursor(0, 2);
        buffer.insert_chars(2);
        let line = buffer.line_text(0).unwrap();
        let chars: Vec<char> = line.chars().collect();
        assert_eq!(chars[0], 'h');
        assert_eq!(chars[1], 'e');
        assert_eq!(chars[2], ' ');
        assert_eq!(chars[3], ' ');
        assert_eq!(chars[4], 'l');
        assert_eq!(chars[5], 'l');
        assert_eq!(chars[6], 'o');
    }

    #[test]
    fn alternate_screen_roundtrip() {
        let mut buffer = TerminalBuffer::new(TerminalSize::new(10, 3), 0);
        buffer.put_char('A');
        buffer.enter_alt_screen();
        assert_eq!(buffer.line_text(0).unwrap().trim(), "");
        buffer.put_char('B');
        buffer.exit_alt_screen();
        assert_eq!(buffer.line_text(0).unwrap().chars().next(), Some('A'));
    }
}
