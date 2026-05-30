//! 终端内容的渲染边界层。
//!
//! 快照类型不依赖 UI，便于测试。GTK 渲染器放在 `gtk` feature 后面，保证非 UI
//! 渲染包可以在没有 GTK4 开发库的环境中编译。

mod snapshot;

pub use snapshot::{RenderCell, RenderLine, RenderSnapshot};

#[cfg(feature = "gtk")]
pub mod gtk;
