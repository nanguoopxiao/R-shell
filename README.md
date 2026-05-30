# R-shell

<kbd>中文</kbd> <a href="README.en.md"><kbd>English</kbd></a>

R-shell 是一个使用 Rust 和 GTK4 构建的跨平台终端客户端。它的目标不是只做一个本地 shell 窗口，而是把本地终端、远程连接、常用命令环境和文件传输能力逐步收拢到一个轻量、清晰、可维护的桌面工具里。

当前项目仍处于 MVP 阶段，但已经具备可运行的 Windows GTK 发布包、本地 PTY 终端、SSH/Telnet/Serial 会话、SFTP/FTP 相关能力，以及随包携带的常用命令环境。

## 适合谁

- 需要一个轻量桌面终端，同时希望 SSH、Telnet、串口等连接入口集中管理的用户。
- Windows 环境里经常遇到 `curl`、`wget`、`ssh`、`telnet` 等命令缺失，希望应用自带一套可用工具链的用户。
- 想研究 Rust、GTK4、PTY、终端渲染和协议适配如何拆分实现的开发者。

## 当前能力

- 本地 shell/PTY：基于 `portable-pty` 启动和管理本地终端会话。
- SSH 终端：通过 PTY 内的 OpenSSH 客户端运行，认证交互保留在终端流中。
- Telnet：原生字节流会话，包含基础 IAC 协商和 NAWS 窗口尺寸上报。
- Serial：基于 `serialport` crate 的串口终端会话，当前以常见 8N1 场景为 MVP。
- SFTP/FTP：包含连接配置、文件列表和传输面板相关实现。
- 内置命令环境：Windows 发布包可携带 `bash`、`curl`、`wget`、`ssh`、`telnet` 等常用命令。
- 连接配置存储：保存的 FTP/SFTP/SSH 密码可在 Windows 上安全存储，并镜像到本地加密 vault 中。
- CPU 优先渲染：终端核心与 GTK UI 解耦，热渲染路径优先使用可见区域数据，避免整缓冲区复制。

## 下载与运行

在 GitHub Releases 中下载 Windows GTK 包：

```text
R-shell-windows-gtk-v0.1.0.zip
```

解压后直接运行根目录中的：

```text
R-shell.exe
```

发布包不是单文件 exe。GTK4 需要随包携带 DLL、`share` 和 `lib` 运行时数据，因此请保持解压后的目录结构完整。

## 内置命令环境

Windows 发布包会把一套瘦身后的 MSYS2 派生命令工具链放在：

```text
tools\msys64
```

当该目录存在时，应用会自动提供 `Shell Tools (Bash)` 本地终端入口，也可以让 SSH 会话优先使用随包携带的 `ssh.exe`。设置页可以控制是否启用内置命令环境、是否注入到现有本地 shell、以及内置命令和系统 `PATH` 的优先级。

这套工具链只作用于 R-shell 启动的会话。它不会安装驱动，不会修改系统环境变量，也不会保存凭据。

## 项目状态

R-shell 现在是早期 MVP：核心架构已经拆开，主要连接路径可以运行，但界面体验、原生 SSH 后端、文件传输细节、跨平台打包和长期稳定性仍在持续推进。

目前优先级：

- 打磨 Windows 发布包体验和体积。
- 稳定本地终端、SSH、Telnet、Serial 会话模型。
- 完善 SFTP/FTP 文件传输体验。
- 保持终端核心独立于 GTK，便于无显示环境测试。
- 控制内存占用，避免热路径上的大缓冲区复制。

## 从源码运行

不依赖 GTK4 系统库的核心 workspace 构建：

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```

安装 GTK4 开发依赖后运行 GUI：

```powershell
cargo run -p shell-app --features gtk-ui
```

构建 release GUI：

```powershell
cargo build -p shell-app --release --features gtk-ui
```

`target\release\R-shell.exe` 是 Cargo 的原始构建产物，适合本地开发使用；Windows 可分发包请使用打包脚本生成。

## Windows GTK4 开发环境

Windows 上通常使用 MSYS2 提供 GTK4 开发库：

```powershell
winget install MSYS2.MSYS2
```

然后在 MSYS2 MinGW64 shell 中安装依赖：

```bash
pacman -S --needed mingw-w64-x86_64-gtk4 mingw-w64-x86_64-pkg-config mingw-w64-x86_64-gcc
```

构建 GTK4 feature 前，把 `C:\msys64\mingw64\bin` 加入 `PATH`；也可以把 MSYS2 安装到仓库本地 `.msys64` 下，并使用辅助脚本：

```powershell
.\scripts\dev-gtk.ps1 check
.\scripts\dev-gtk.ps1 build
.\scripts\dev-gtk.ps1 clippy
.\scripts\dev-gtk.ps1 run
```

## 打包

本地生成 Windows GTK 发布目录：

```powershell
.\scripts\package-gtk.ps1
```

构建并立即验证应用能启动：

```powershell
.\scripts\package-gtk.ps1 -SmokeTest
```

脚本会生成：

```text
dist\windows-gtk\R-shell.exe
```

手动压缩发布包：

```powershell
$Version = "v0.1.0"
$Archive = "R-shell-windows-gtk-$Version.zip"
Compress-Archive -Path "dist\windows-gtk\*" -DestinationPath $Archive -CompressionLevel Optimal -Force
Get-FileHash -Algorithm SHA256 $Archive | Format-List
```

仓库也包含自动发布工作流。推送 `v*` tag 后，GitHub Actions 会构建 Windows GTK 包、生成 zip 和 `.sha256` 校验文件，并创建 GitHub Release。Release 正文会自动包含从上一个 tag 到当前 tag 的提交摘要，并附加 GitHub 自动生成的发布说明。

推荐发布流程：

```powershell
git status --short
git tag v0.1.1
git push origin main --tags
```

如果需要更像正式产品公告，可以在 Release 创建后到 GitHub 页面手动编辑正文，把自动生成的提交摘要整理成「新增」「修复」「已知问题」等段落。

## 架构

- `shell-core`：共享模型、事件、错误和协议/会话类型。
- `shell-terminal`：终端 cell、ANSI/VT 解析、屏幕缓冲和 scrollback。
- `shell-platform`：默认 shell 检测、PTY 抽象和内置工具链发现。
- `shell-protocol`：LocalShell、SSH、Telnet、Serial、FTP、SFTP 等协议适配层。
- `shell-renderer`：UI 无关的渲染快照，以及可选 GTK4 终端视图。
- `shell-storage`：profiles、settings、secrets 持久化。
- `shell-app`：应用入口、GTK4 UI、会话页面和设置页。

## 安全说明

- SSH 终端认证默认委托给 OpenSSH 客户端，密码和私钥口令提示保留在 PTY 交互流里。
- 保存的连接密码会通过系统凭据能力和本地加密 vault 管理，不应写入明文配置文件。
- 内置命令环境是随应用携带的用户态工具链，不会修改全局系统环境。

## 路线图

- 用原生 SSH backend 替换或补充系统 OpenSSH 模式。
- 继续完善 SFTP/FTP 文件传输面板。
- 将 VNC 作为独立 framebuffer viewer 添加。
- 改善跨平台打包体验。
- 持续降低 Windows GTK runtime 体积和空闲内存占用。