//! 流式 ANSI/VT 解析器。
//!
//! PTY 读取可能在任意字节位置切开 UTF-8 字符和转义序列。因此解析器维护一个
//! 很小的 UTF-8 暂存缓冲，并用状态机处理 ESC/CSI/OSC 序列，最后按解码后的
//! 字符或控制序列逐步修改 `TerminalBuffer`。

use crate::TerminalBuffer;

/// ANSI/VT100/xterm 转义序列解析器。
///
/// 通过状态机覆盖常见终端程序（bash、vim、htop、tmux 等）使用的序列。未知序列会被
/// 静默丢弃，最坏情况是显示乱码，而不是崩溃。
#[derive(Debug, Default)]
pub struct AnsiParser {
    state: ParserState,
    /// CSI / OSC 的中间字节缓冲。
    buf: String,
    /// 跨 PTY 读取被切开的 UTF-8 尾部字节。
    utf8_carry: Vec<u8>,
}

#[derive(Debug, Default)]
enum ParserState {
    #[default]
    Ground,
    /// 已收到 ESC，等待下一个字节。
    Escape,
    /// 处于 CSI（ESC [）序列内部。
    Csi,
    /// 处于 OSC（ESC ]）序列内部。
    Osc,
    /// 已收到 ESC ( 或 ESC ) 字符集指定序列；跳过一个字节。
    CharsetSkip,
}

impl AnsiParser {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed_bytes(&mut self, buffer: &mut TerminalBuffer, bytes: &[u8]) {
        // 在多次 PTY 读取之间保留未完成的多字节 UTF-8 尾部。非法字节会替换为
        // U+FFFD，避免异常数据流卡住解析器。
        self.utf8_carry.extend_from_slice(bytes);

        loop {
            match std::str::from_utf8(&self.utf8_carry) {
                Ok(_) => {
                    let text = String::from_utf8(std::mem::take(&mut self.utf8_carry))
                        .expect("validated UTF-8 must decode");
                    self.feed_str(buffer, &text);
                    break;
                }
                Err(err) => {
                    let valid_up_to = err.valid_up_to();
                    if valid_up_to > 0 {
                        let text = std::str::from_utf8(&self.utf8_carry[..valid_up_to])
                            .expect("valid UTF-8 prefix must decode")
                            .to_owned();
                        self.utf8_carry.drain(..valid_up_to);
                        self.feed_str(buffer, &text);
                        continue;
                    }

                    match err.error_len() {
                        Some(invalid_len) => {
                            self.utf8_carry
                                .drain(..invalid_len.min(self.utf8_carry.len()));
                            self.feed_char(buffer, '\u{FFFD}');
                        }
                        None => break,
                    }
                }
            }
        }
    }

    pub fn feed_str(&mut self, buffer: &mut TerminalBuffer, text: &str) {
        for ch in text.chars() {
            self.feed_char(buffer, ch);
        }
    }

    fn feed_char(&mut self, buffer: &mut TerminalBuffer, ch: char) {
        // CAN / SUB 在任意状态下都会取消当前序列。
        if matches!(ch, '\u{18}' | '\u{1a}') {
            self.state = ParserState::Ground;
            return;
        }

        match self.state {
            ParserState::Ground => match ch {
                '\u{07}' => {} // BEL – ignore in ground state
                '\u{1b}' => self.state = ParserState::Escape,
                '\n' | '\r' | '\u{8}' => buffer.put_char(ch),
                '\t' => buffer.tab(),
                ch if (ch as u32) < 0x20 => {} // ignore remaining C0
                ch => buffer.put_char(ch),
            },

            ParserState::Escape => match ch {
                '[' => {
                    self.buf.clear();
                    self.state = ParserState::Csi;
                }
                ']' => {
                    self.buf.clear();
                    self.state = ParserState::Osc;
                }
                '(' | ')' | '*' | '+' => {
                    self.state = ParserState::CharsetSkip;
                }
                'c' => {
                    // RIS：完全重置。
                    *buffer = TerminalBuffer::new(buffer.size(), 10_000);
                    self.state = ParserState::Ground;
                }
                '7' => {
                    buffer.save_cursor();
                    self.state = ParserState::Ground;
                }
                '8' => {
                    buffer.restore_cursor();
                    self.state = ParserState::Ground;
                }
                'D' => {
                    // IND：索引；当前实现中等同于 LF。
                    buffer.new_line();
                    self.state = ParserState::Ground;
                }
                'M' => {
                    // RI：反向索引，向下滚动一行。
                    buffer.scroll_down(1);
                    self.state = ParserState::Ground;
                }
                _ => self.state = ParserState::Ground,
            },

            ParserState::CharsetSkip => {
                // 丢弃字符集指定字节，例如 US-ASCII 对应的 'B'。
                self.state = ParserState::Ground;
            }

            ParserState::Osc => {
                // 持续收集，直到 ST（ESC \）、BEL（0x07）或另一个 ESC。
                match ch {
                    '\u{07}' => {
                        // BEL 结束 OSC。
                        self.handle_osc(buffer);
                        self.buf.clear();
                        self.state = ParserState::Ground;
                    }
                    '\u{1b}' => {
                        // ESC 可能是 ESC \（ST）的开始；转回 Escape 状态。这里会丢弃
                        // 当前挂起序列，但能避免解析器卡死。
                        self.state = ParserState::Escape;
                    }
                    '\\' if matches!(self.state, ParserState::Osc) => {
                        // '\\' alone inside OSC is unusual; just collect.
                        self.buf.push(ch);
                    }
                    _ => self.buf.push(ch),
                }
            }

            ParserState::Csi => {
                if ch.is_ascii_alphabetic() || ch == '@' {
                    let params = parse_csi_params(&self.buf);
                    handle_csi(buffer, &params, ch, &self.buf);
                    self.buf.clear();
                    self.state = ParserState::Ground;
                } else if ch.is_ascii_digit() || matches!(ch, ';' | '?' | ':' | '>' | '!' | ' ') {
                    self.buf.push(ch);
                } else {
                    // 遇到意外字节，放弃当前序列。
                    self.buf.clear();
                    self.state = ParserState::Ground;
                }
            }
        }
    }

    fn handle_osc(&self, _buffer: &mut TerminalBuffer) {
        // 常见 OSC：0;title，用于设置图标名和窗口标题。当前还没有窗口标题回调，
        // 因此静默忽略。
    }
}

fn parse_csi_params(raw: &str) -> Vec<u16> {
    // 真实终端输出中常见 xterm/DEC 私有前缀和冒号分隔的子参数。暂不支持的部分
    // 统一归一化为 0，让处理逻辑保持简单且防御性更强。
    let stripped = raw.trim_start_matches(['?', '>', '!']);
    stripped
        .split(';')
        .map(|part| part.split(':').next().unwrap_or_default())
        .map(|part| part.parse::<u16>().unwrap_or(0))
        .collect()
}

#[allow(clippy::too_many_lines)]
fn handle_csi(buffer: &mut TerminalBuffer, params: &[u16], command: char, raw: &str) {
    let p0 = params.first().copied().unwrap_or(0);
    let p1 = params.get(1).copied().unwrap_or(0);

    // DEC 私有序列以 '?' 开头。
    let is_dec = raw.starts_with('?');

    match command {
        // SGR：选择图形渲染属性。
        'm' => buffer.set_graphic_rendition(params),

        // 光标移动。
        'A' => buffer.cursor_up(p0.max(1) as usize),
        'B' => buffer.cursor_down(p0.max(1) as usize),
        'C' => buffer.cursor_forward(p0.max(1) as usize),
        'D' => buffer.cursor_backward(p0.max(1) as usize),
        // CNL：光标移动到后续行。
        'E' => {
            buffer.cursor_down(p0.max(1) as usize);
            buffer.carriage_return();
        }
        // CPL：光标移动到前序行。
        'F' => {
            buffer.cursor_up(p0.max(1) as usize);
            buffer.carriage_return();
        }
        // CHA：光标水平绝对定位，参数从 1 开始。
        'G' => buffer.cursor_col(p0.max(1) as usize),
        // CUP / HVP：光标行列定位，行列参数从 1 开始。
        'H' | 'f' => {
            let row = p0.max(1).saturating_sub(1) as usize;
            let col = p1.max(1).saturating_sub(1) as usize;
            buffer.move_cursor(row, col);
        }
        // ED：擦除显示区域。
        'J' => buffer.erase_display(p0),
        // EL：擦除行。
        'K' => buffer.erase_in_line(p0),
        // IL：插入行。
        'L' => buffer.insert_lines(p0.max(1) as usize),
        // DL：删除行。
        'M' => buffer.delete_lines(p0.max(1) as usize),
        // DCH：删除字符。
        'P' => buffer.delete_chars(p0.max(1) as usize),
        // SU：向上滚动。
        'S' => buffer.scroll_up(p0.max(1) as usize),
        // SD：向下滚动。
        'T' => buffer.scroll_down(p0.max(1) as usize),
        // ECH：擦除字符；当前按从光标开始填充空白处理。
        'X' => {
            let n = p0.max(1) as usize;
            let col = buffer.cursor().col;
            let row = buffer.cursor().row;
            // 为每个被擦除的单元格临时推进光标。
            for offset in 0..n {
                let _ = offset;
                buffer.erase_in_line(0);
                if buffer.cursor().col + 1 < buffer.size().cols as usize {
                    buffer.cursor_forward(1);
                }
            }
            buffer.move_cursor(row, col);
        }
        // VPA：垂直绝对定位，参数从 1 开始。
        'd' => buffer.cursor_row(p0.max(1) as usize),
        // ICH：插入字符。
        '@' => buffer.insert_chars(p0.max(1) as usize),
        // DECSTBM：设置滚动区域。
        'r' => {
            let top = p0.max(1) as usize;
            let bottom = if p1 == 0 {
                buffer.size().rows as usize
            } else {
                p1 as usize
            };
            buffer.set_scroll_region(top, bottom);
        }
        // DECSC：保存光标（CSI 形式）。
        's' => buffer.save_cursor(),
        // DECRC：恢复光标（CSI 形式）。
        'u' => buffer.restore_cursor(),
        // DEC 私有模式设置/重置（h/l）。
        'h' | 'l' => {
            let enable = command == 'h';
            if is_dec {
                match p0 {
                    // DECTCEM：光标可见性。
                    25 => buffer.set_cursor_visible(enable),
                    // smcup / rmcup：备用屏幕。
                    47 | 1047 | 1049 => {
                        if enable {
                            buffer.enter_alt_screen();
                        } else {
                            buffer.exit_alt_screen();
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use shell_core::TerminalSize;

    use super::*;
    use crate::{AttrsFlags, Color};

    fn make_parser_and_buf(cols: u16, rows: u16) -> (AnsiParser, TerminalBuffer) {
        (
            AnsiParser::new(),
            TerminalBuffer::new(TerminalSize::new(cols, rows), 10),
        )
    }

    #[test]
    fn parses_text_and_newlines() {
        let (mut p, mut buf) = make_parser_and_buf(10, 3);
        p.feed_str(&mut buf, "one\ntwo");
        assert_eq!(buf.line_text(0).unwrap()[..3].to_string(), "one");
        assert_eq!(buf.line_text(1).unwrap()[..3].to_string(), "two");
    }

    #[test]
    fn parses_sgr_sequences() {
        let (mut p, mut buf) = make_parser_and_buf(10, 3);
        p.feed_str(&mut buf, "\u{1b}[1;32mX");
        let snap = buf.snapshot();
        let cell = snap.lines[0][0];
        assert_eq!(cell.ch, 'X');
        assert!(cell.attrs.flags.contains(AttrsFlags::BOLD));
        assert_eq!(cell.attrs.fg, Color::Indexed(2));
    }

    #[test]
    fn parses_clear_screen() {
        let (mut p, mut buf) = make_parser_and_buf(10, 3);
        p.feed_str(&mut buf, "hello\u{1b}[2J");
        // ED 2 会擦除屏幕，但不移动光标。
        assert!(buf.line_text(0).unwrap().trim().is_empty());
    }

    #[test]
    fn cursor_up_down_left_right() {
        let (mut p, mut buf) = make_parser_and_buf(20, 10);
        p.feed_str(&mut buf, "\u{1b}[5;5H"); // CUP row=5 col=5 (1-based)
        assert_eq!(buf.cursor().row, 4);
        assert_eq!(buf.cursor().col, 4);
        p.feed_str(&mut buf, "\u{1b}[2A"); // up 2
        assert_eq!(buf.cursor().row, 2);
        p.feed_str(&mut buf, "\u{1b}[3B"); // down 3
        assert_eq!(buf.cursor().row, 5);
        p.feed_str(&mut buf, "\u{1b}[2C"); // right 2
        assert_eq!(buf.cursor().col, 6);
        p.feed_str(&mut buf, "\u{1b}[4D"); // left 4
        assert_eq!(buf.cursor().col, 2);
    }

    #[test]
    fn dectcem_cursor_visibility() {
        let (mut p, mut buf) = make_parser_and_buf(10, 5);
        assert!(buf.cursor_state().visible);
        p.feed_str(&mut buf, "\u{1b}[?25l"); // hide
        assert!(!buf.cursor_state().visible);
        p.feed_str(&mut buf, "\u{1b}[?25h"); // show
        assert!(buf.cursor_state().visible);
    }

    #[test]
    fn alternate_screen_via_escape() {
        let (mut p, mut buf) = make_parser_and_buf(10, 3);
        p.feed_str(&mut buf, "primary");
        p.feed_str(&mut buf, "\u{1b}[?1049h"); // enter alt
        assert_eq!(buf.line_text(0).unwrap().trim(), "");
        p.feed_str(&mut buf, "\u{1b}[?1049l"); // exit alt
        assert_eq!(buf.line_text(0).unwrap()[..7].to_string(), "primary");
    }

    #[test]
    fn erase_display_from_cursor() {
        let (mut p, mut buf) = make_parser_and_buf(10, 4);
        p.feed_str(&mut buf, "aaa\nbbb\nccc\nddd");
        p.feed_str(&mut buf, "\u{1b}[2;1H"); // move to row 2 col 1
        p.feed_str(&mut buf, "\u{1b}[0J"); // erase from cursor to end
        assert_eq!(buf.line_text(0).unwrap()[..3].to_string(), "aaa");
        assert!(buf.line_text(1).unwrap().trim().is_empty());
    }

    #[test]
    fn osc_sequence_ignored_gracefully() {
        let (mut p, mut buf) = make_parser_and_buf(10, 3);
        // OSC 0 ; title BEL。
        p.feed_str(&mut buf, "\u{1b}]0;My Title\u{07}hello");
        assert_eq!(buf.line_text(0).unwrap()[..5].to_string(), "hello");
    }

    #[test]
    fn scroll_region_and_scroll_up() {
        let (mut p, mut buf) = make_parser_and_buf(10, 5);
        p.feed_str(&mut buf, "\u{1b}[2;4r"); // scroll region rows 2-4
        p.feed_str(&mut buf, "\u{1b}[2;1H"); // cursor to row 2
        p.feed_str(&mut buf, "\u{1b}[1S"); // scroll up 1 in region
        // 只断言不 panic，且缓冲区尺寸不变。
        assert_eq!(buf.size().rows, 5);
    }

    #[test]
    fn parses_split_utf8_sequences_across_feeds() {
        let (mut p, mut buf) = make_parser_and_buf(20, 3);
        let text = "中🙂𠀀";
        let bytes = text.as_bytes();

        p.feed_bytes(&mut buf, &bytes[..1]);
        p.feed_bytes(&mut buf, &bytes[1..4]);
        p.feed_bytes(&mut buf, &bytes[4..7]);
        p.feed_bytes(&mut buf, &bytes[7..10]);
        p.feed_bytes(&mut buf, &bytes[10..]);

        assert_eq!(buf.line_text(0).unwrap().trim_end(), text);
    }

    #[test]
    fn parses_very_long_wrapped_output_with_bounded_scrollback() {
        // 超长且无换行的单行是自动换行和回滚历史的最坏情况：每满一屏宽度
        // 都会触发滚动，但历史行数必须保持有界。
        let max_scrollback = 128;
        let mut parser = AnsiParser::new();
        let mut buffer = TerminalBuffer::new(TerminalSize::new(80, 24), max_scrollback);
        let payload = "A".repeat(80 * (max_scrollback + 64));

        for chunk in payload.as_bytes().chunks(4096) {
            parser.feed_bytes(&mut buffer, chunk);
        }
        parser.feed_str(&mut buffer, "TAIL");

        assert_eq!(buffer.scrollback_len(), max_scrollback);
        assert_eq!(buffer.total_line_count(), max_scrollback + 24);
        assert!(buffer.cursor().row < 24);
        assert!(buffer.cursor().col <= 4);
        assert!(
            buffer
                .line_text(buffer.cursor().row)
                .unwrap()
                .starts_with("TAIL")
        );
    }

    #[test]
    fn parses_long_split_utf8_stream_without_losing_tail() {
        // 用奇数大小的字节块喂入，强制在 CJK/emoji 标量中间切分。最后的 ASCII
        // 标记用于证明数据流尾部仍然可见且没有丢失。
        let mut parser = AnsiParser::new();
        let mut buffer = TerminalBuffer::new(TerminalSize::new(40, 8), 32);
        let payload = "中🙂𠀀".repeat(4096);

        for chunk in payload.as_bytes().chunks(7) {
            parser.feed_bytes(&mut buffer, chunk);
        }
        parser.feed_str(&mut buffer, "END");

        assert_eq!(buffer.scrollback_len(), 32);
        assert!(
            buffer
                .line_text(buffer.cursor().row)
                .unwrap()
                .contains("END")
        );
    }
}
