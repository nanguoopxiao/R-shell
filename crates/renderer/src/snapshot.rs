use shell_terminal::{Cell, TerminalSnapshot};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderCell {
    pub cell: Cell,
    pub is_cursor: bool,
}

pub type RenderLine = Vec<RenderCell>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderSnapshot {
    pub lines: Vec<RenderLine>,
}

impl RenderSnapshot {
    #[must_use]
    pub fn from_terminal(snapshot: &TerminalSnapshot) -> Self {
        let cursor_row = snapshot.cursor.pos.row;
        let cursor_col = snapshot.cursor.pos.col;
        let cursor_visible = snapshot.cursor.visible;
        let lines = snapshot
            .lines
            .iter()
            .enumerate()
            .map(|(row, line)| {
                line.iter()
                    .enumerate()
                    .map(|(col, cell)| RenderCell {
                        cell: *cell,
                        is_cursor: cursor_visible && row == cursor_row && col == cursor_col,
                    })
                    .collect()
            })
            .collect();
        Self { lines }
    }
}

#[cfg(test)]
mod tests {
    use shell_core::TerminalSize;
    use shell_terminal::TerminalBuffer;

    use super::*;

    #[test]
    fn marks_cursor_cell() {
        let buffer = TerminalBuffer::new(TerminalSize::new(4, 2), 10);
        let render = RenderSnapshot::from_terminal(&buffer.snapshot());

        assert!(render.lines[0][0].is_cursor);
        assert!(!render.lines[0][1].is_cursor);
    }
}
