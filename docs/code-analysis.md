# R-shell 代码功能分析

本文档梳理当前 Rust/GTK4 终端客户端的模块职责、核心数据流、关键实现点和测试入口。目标是让后续开发能快速判断“某个功能应该改哪里”，同时避免把 GTK UI、协议适配、终端核心和存储逻辑耦合在一起。

## 1. 总体分层

项目是 Cargo workspace，按职责拆为多个 crate：

| Crate | 主要职责 | 是否依赖 GTK |
| --- | --- | --- |
| `shell-core` | 共享模型、事件、错误类型 | 否 |
| `shell-terminal` | 终端 cell、ANSI/VT 解析、屏幕缓冲、scrollback | 否 |
| `shell-platform` | OS shell 检测、本地 PTY 抽象 | 否 |
| `shell-protocol` | LocalShell/SSH/Telnet/Serial/FTP/SFTP 适配 | 否 |
| `shell-renderer` | UI 无关快照；`gtk` feature 下提供 GTK 终端视图 | 可选 |
| `shell-storage` | profiles/settings/secrets 持久化 | 否 |
| `shell-app` | GTK4 应用、页面、设置、SFTP 文件管理器 | 是，需 `gtk-ui` feature |

核心原则：

- 终端核心不依赖 GTK，可在无显示服务器环境测试。
- 协议适配层只输出统一事件，不直接操作 UI。
- GTK UI 只负责组合控件、调度后台任务和绑定事件。
- 热渲染路径避免全量 scrollback clone，优先按可见区域读取。
- 保存密码不进入 plaintext profile 文件。

## 2. 终端输出数据流

```text
LocalPty / protocol reader thread
    -> ProtocolEvent::Output(Vec<u8>)
    -> shell-app attach_protocol_receiver()
    -> GtkTerminalView::feed()
    -> AnsiParser::feed_bytes()
    -> TerminalBuffer mutation
    -> DrawingArea queue_draw()
    -> visible-slice Cairo/Pango render
```

关键文件：

- `crates/platform/src/pty.rs`
  - `LocalPty::spawn()` 创建 PTY、拆分 reader/writer、保留 master 用于 resize。
- `crates/protocol/src/pty_connection.rs`
  - `PtyConnection` 把阻塞 PTY read 转换为 `ProtocolEvent`。
- `crates/terminal/src/parser.rs`
  - `AnsiParser` 处理 split UTF-8、ESC、CSI、OSC、SGR、光标移动和滚屏控制。
- `crates/terminal/src/screen.rs`
  - `TerminalBuffer` 保存屏幕、cursor、attrs、bounded scrollback。
- `crates/renderer/src/gtk.rs`
  - `GtkTerminalView` 负责输入、选择、URL hover/click、语义高亮和绘制。
- `crates/app/src/gtk_app.rs`
  - `attach_protocol_receiver()` 在 GTK main loop 中分批消费协议事件，避免协议线程直接碰 GTK。

## 3. 终端长文本与内存控制

长输出风险主要有三类：

1. 超长单行连续输出导致大量自动换行。
2. 多字节 UTF-8 被 PTY 分片切开。
3. scrollback 不受控导致内存持续增长。

当前机制：

- `TerminalBuffer::new(size, max_scrollback)` 设置 scrollback 上限。
- `DEFAULT_SCROLLBACK_LINES` 当前为 2000。
- `TerminalBuffer::line_at()` 支持按行读取，不要求渲染端 clone 全缓冲。
- `GtkTerminalView` 通过 `ViewLayout` 计算可见 slice。
- `AnsiParser::feed_bytes()` 保存 `utf8_carry`，跨 read 修复 split UTF-8。

新增稳定性测试：

- `parses_very_long_wrapped_output_with_bounded_scrollback`
  - 大量连续字符输出后，验证 scrollback 上限、总行数和尾部内容。
- `parses_long_split_utf8_stream_without_losing_tail`
  - 用奇数大小 chunk 强制拆开 CJK/emoji，验证尾部 marker 不丢。

运行：

```powershell
cargo test -p shell-terminal parses_very_long_wrapped_output_with_bounded_scrollback
cargo test -p shell-terminal parses_long_split_utf8_stream_without_losing_tail
```

## 4. GTK UI 组织

`crates/app/src/gtk_app.rs` 是主 UI 文件，核心结构包括：

- `AppState`
  - GTK callbacks 共享的状态集合，包含 window、notebook、profiles、settings、terminal widgets、page contexts、connections、popover、SFTP sidebar 等。
- `SessionTabStrip`
  - 自定义顶部页签栏，替代 GTK Notebook 原生 tab。
  - 目标是稳定 close button 几何、保留可读标题、在多页签时横向滚动。
- `PageContext`
  - 页面与 `ConnectionProfile`、运行时密码的弱引用映射。
  - SFTP 快捷入口、当前页 host stats、SSH 重连都依赖它。
- `PageShutdown`
  - 页面关闭时执行协议 shutdown、清除 IO handler，避免 reader/PTY 泄漏。
- `SshReconnectSession`
  - 每个 SSH 页签独立的断线重连状态。

页签关闭关键点：

- close handler 捕获 page widget，不捕获 index。
- `sync_session_tab_strip(None)` 代表关闭/普通重排，不自动 reveal 当前页，避免 close button 横向跳动。
- 多页签时 tab width clamp 到可读最小值，溢出交给水平滚动。

## 5. SSH 连接与重连

SSH 终端不是 native libssh2 channel，而是系统 `ssh` 命令运行在 PTY 中：

- 好处：复用 OpenSSH host-key prompt、password prompt、私钥行为和兼容参数。
- 代价：输出/断线来自 PTY EOF 或 reader error，需要 UI 侧状态机识别。

关键函数：

- `build_ssh_config_from_profile()`
  - 从保存的 profile 构造 `SshConfig`。
  - 首次连接和原页签重连共用它，保证参数一致。
- `openssh_compatibility_args()`
  - 根据设置拼接 OpenSSH legacy/国密/旧算法参数。
- `ssh_disconnected_input_action()`
  - 纯状态机：断线后第一次 Enter 提示，第二次 Enter 重连，非 Enter 忽略并重置。
- `mark_ssh_session_disconnected()`
  - 由 EOF、error 或发送失败触发，只在第一次断线时打印提示。
- `reconnect_ssh_session()`
  - 保留原 `GtkTerminalView` 和 notebook page，只替换底层 `SshConnection` 与 receiver。

## 6. SFTP 文件管理器

SFTP 位于 `crates/protocol/src/sftp.rs` 和 `crates/app/src/gtk_app.rs` 两层：

- 协议层 `SftpSession`
  - 负责 connect、list、cd、upload/download、resume、recursive transfer、mkdir、delete、rename、chmod、copy、compress。
  - `Sftp`、`Session`、cwd 均在 `Arc<Mutex<_>>` 中，便于 GTK 后台任务 clone session。
- UI 层 `SftpSidebar`
  - 负责 compact/full 两种布局、列表、右键菜单、上传/下载文件选择器、拖拽上传、进度条和状态文本。

SFTP live 稳定性测试：

- `sftp_stability_large_file_resume_and_directory_roundtrip`
  - 默认 ignored，因为需要真实服务器并会创建/删除远端测试目录。
  - 覆盖大文件上传断点续传、下载断点续传、checksum、远端 copy/rename/chmod、目录递归上传下载和结构校验。

需要环境变量：

```powershell
$env:SHELL_SFTP_TEST_HOST = "192.168.x.x"
$env:SHELL_SFTP_TEST_USERNAME = "user"
$env:SHELL_SFTP_TEST_PASSWORD = "password"
$env:SHELL_SFTP_TEST_PORT = "22"                         # 可选
$env:SHELL_SFTP_TEST_REMOTE_ROOT = "/tmp"                 # 可选
$env:SHELL_SFTP_TEST_LARGE_MB = "8"                       # 可选
$env:SHELL_SFTP_TEST_FILE_COUNT = "64"                    # 可选
```

运行：

```powershell
cargo test -p shell-protocol sftp_stability_large_file_resume_and_directory_roundtrip -- --ignored --nocapture
```

或：

```powershell
.\scripts\test-stability.ps1 -Sftp
```

## 7. 设置、语言和样式

存储层：

- `AppSettings`
  - terminal font、renderer backend、semantic highlighting、language、OpenSSH compatibility。
- `ProfileStore`
  - 管理 profiles、settings 和 secret vault。
- `OpensshCompatibilitySettings`
  - master switch + 具体 legacy/地域/国密算法组。

UI 层：

- `tr(language, zh, en)` 做轻量中英文切换。
- `StableComboBox` 避免 GTK 原生下拉项 hover 几何抖动。
- 全局 CSS 统一暗色、popover、context menu、settings card、tab strip 和按钮风格。

## 8. 测试与验证入口

常规验证：

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

GTK 环境验证：

```powershell
.\scripts\dev-gtk.ps1 clippy
.\scripts\dev-gtk.ps1 build
```

打包：

```powershell
.\scripts\package-gtk.ps1 -SmokeTest
```

稳定性测试：

```powershell
.\scripts\test-stability.ps1
.\scripts\test-stability.ps1 -Sftp
```

## 9. 修改建议

- 新协议：优先在 `shell-protocol` 中独立适配为 `ProtocolEvent` 或独立 API，再由 `shell-app` 绑定 UI。
- 新终端能力：优先改 `shell-terminal` 并加单元测试，再让 renderer 消费新状态。
- 新设置项：先扩展 `shell-storage::AppSettings`，再更新 settings UI 和持久化测试。
- UI 回归：涉及 hover、popover、tab strip、drag/drop 时应尽量补自动化脚本或可重复的手工测试步骤。
- 性能回归：关注 scrollback 上限、visible-slice 渲染、Pango layout 复用和后台任务是否阻塞 GTK main loop。
