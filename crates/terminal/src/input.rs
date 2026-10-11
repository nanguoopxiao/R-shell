//! Terminal input modes shared by the parser and UI, without a GUI dependency.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalModes {
    pub application_cursor_keys: bool,
    pub bracketed_paste: bool,
    pub origin: bool,
    pub auto_wrap: bool,
    pub newline: bool,
}

impl Default for TerminalModes {
    fn default() -> Self {
        Self {
            application_cursor_keys: false,
            bracketed_paste: false,
            origin: false,
            auto_wrap: true,
            newline: false,
        }
    }
}

/// Normalize clipboard newlines and strip control characters which could be
/// interpreted as keystrokes or an injected end-of-paste escape sequence.
#[must_use]
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let mut result = Vec::with_capacity(text.len() + 12);
    if bracketed {
        result.extend_from_slice(b"\x1b[200~");
    }
    let mut previous_cr = false;
    for ch in text.chars() {
        match ch {
            '\r' => {
                result.push(b'\r');
                previous_cr = true;
            }
            '\n' => {
                if !previous_cr {
                    result.push(b'\r');
                }
                previous_cr = false;
            }
            '\t' => {
                result.push(b'\t');
                previous_cr = false;
            }
            ch => {
                previous_cr = false;
                if !ch.is_control() {
                    let mut bytes = [0; 4];
                    result.extend_from_slice(ch.encode_utf8(&mut bytes).as_bytes());
                }
            }
        }
    }
    if bracketed {
        result.extend_from_slice(b"\x1b[201~");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_normalizes_newlines_and_preserves_unicode() {
        assert_eq!(
            encode_paste("a\r\nb\nc\r中\t文", false),
            "a\rb\rc\r中\t文".as_bytes()
        );
    }

    #[test]
    fn bracketed_paste_cannot_inject_an_end_marker_or_control_key() {
        assert_eq!(
            encode_paste("a\x1b[201~\x03b\0\u{009b}", true),
            b"\x1b[200~a[201~b\x1b[201~"
        );
    }
}
