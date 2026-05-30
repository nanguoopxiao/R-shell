//! 不依赖 UI 的终端模型。
//!
//! 该包负责单元格属性、ANSI 解析、光标移动、自动换行和有界回滚历史。
//! 它刻意不依赖 GTK；渲染器通过 `TerminalBuffer` 读取可见切片，而不是克隆
//! 整个缓冲区。

mod cell;
mod parser;
mod screen;

pub use cell::{Attrs, AttrsFlags, Cell, Color};
pub use parser::AnsiParser;
pub use screen::{Cursor, CursorState, DirtyRegion, TerminalBuffer, TerminalSnapshot};
