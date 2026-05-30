//! 进程和 PTY 集成的平台边界层。
//!
//! 协议包通过这一层访问平台能力，而不是直接依赖操作系统特定的进程 API。
//! 抽象面保持小而稳定，可以让终端和协议测试不引入 GTK 或应用 UI 代码。

mod pty;
mod shell;
mod toolchain;

pub use pty::{LocalPty, PtyConfig, PtyLaunchOptions};
pub use shell::{default_shell, default_shell_args};
pub use toolchain::{BuiltinToolchain, ToolchainDiscovery, ToolchainPathPriority};
