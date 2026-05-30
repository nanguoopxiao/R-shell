# Shell

<kbd>中文</kbd> <a href="README.en.md"><kbd>English</kbd></a>

Shell 是一个使用 Rust 编写的 GTK4/gtk-rs 跨平台终端客户端 MVP。
项目围绕轻量级终端核心、协议适配层和 GTK4 UI 组织；GTK4 界面通过 `gtk-ui` feature 显式启用，核心 crate 可以在没有 GTK4 系统库的环境中构建和测试。

## 当前 MVP 范围

- Rust Cargo workspace，拆分为 core、terminal、platform、protocol、storage、renderer 和 app 等 crate。
- CPU 优先的终端缓冲区和渲染架构。
- 基于 `portable-pty` 的本地 shell/PTY 支持。
- SSH 终端会话通过 PTY 内的 OpenSSH 客户端运行，主机密钥提示、密码和私钥口令仍保留在终端交互流里，应用不直接保存明文。
- Windows 发布包可以包含基于 MSYS2 的内置常用命令环境，例如 `curl`、`wget`、`ssh` 和 `telnet`。
- 原生 Telnet 字节流会话，包含基础 IAC 协商和 NAWS 窗口尺寸上报。
- 使用 `serialport` crate 的串口终端会话，当前以简单 8N1 场景为 MVP。
- GTK4 终端视图会把尺寸变化传播到 LocalShell、SSH 和 Telnet 会话。
- GTK4 应用位于 `gtk-ui` feature 后面，核心 crate 可在没有 GTK4 开发库的机器上构建和测试。

## 构建

不依赖 GTK4 系统库的核心 workspace 构建：

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```

安装 GTK4 开发依赖后运行 GTK4 UI：

```powershell
cargo run -p shell-app --features gtk-ui
```

构建 release GUI：

```powershell
cargo build -p shell-app --release --features gtk-ui
```

`target\release\shell-app.exe` 是 Cargo 的原始构建产物，适合本地开发使用；但在 Windows 上，它本身不是一个可双击分发的完整 GTK 应用包。

在 Windows 上构建包含 GTK 运行时文件的可分发包：

```powershell
.\scripts\package-gtk.ps1
```

构建并立即验证打包后的应用能启动：

```powershell
.\scripts\package-gtk.ps1 -SmokeTest
```

然后启动：

```text
dist\windows-gtk\bin\shell-app.exe
```

本项目中，`target` 是编译输出和缓存目录，`dist` 是交付给用户的可分发目录。构建继续使用 `target`，发布包继续输出到 `dist`。

如果需要真正的单文件 `.exe`，当前 GTK4 + MSYS2 运行时不适合这种打包模型。GTK 需要 DLL，以及 `share` 和 `lib` 下的运行时数据；因此目前支持的交付形态是 `dist\windows-gtk` 文件夹，而不是单个独立可执行文件。

使用本仓库约定的本地 MSYS2 环境时，可以通过辅助脚本构建：

```powershell
.\scripts\dev-gtk.ps1 check
.\scripts\dev-gtk.ps1 build
.\scripts\dev-gtk.ps1 clippy
.\scripts\dev-gtk.ps1 run
```

## 发布版本

本地打包 Windows GTK 版本：

```powershell
.\scripts\package-gtk.ps1 -SmokeTest
```

脚本会生成：

```text
dist\windows-gtk\bin\shell-app.exe
```

如果要手动上传到 GitHub Releases，先把发布目录压缩成 zip：

```powershell
$Version = "v0.1.0"
$Archive = "shell-windows-gtk-$Version.zip"
Compress-Archive -Path "dist\windows-gtk\*" -DestinationPath $Archive -Force
Get-FileHash -Algorithm SHA256 $Archive | Format-List
```

仓库也包含自动发布工作流。创建并推送 `v*` tag 后，GitHub Actions 会自动构建 Windows GTK 包、生成 zip 和 `.sha256` 校验文件，并创建 GitHub Release：

```powershell
git tag v0.1.0
git push origin v0.1.0
```

发布新版本前建议同步更新 [Cargo.toml](Cargo.toml) 里的 workspace 版本号，例如从 `0.1.0` 改到 `0.1.1`。如果只是测试打包流程，也可以在 GitHub Actions 页面手动运行 `Release` workflow；手动运行不会创建 Release，只会生成可下载的 workflow artifact。

## 连接能力

GTK 应用启动时会打开一个本地 shell 页签。连接栏当前支持：

- `Local`：打开另一个本地 PTY shell。
- `SSH`：使用 `host`、可选 `user` 和可选 `port`，默认端口为 22。
- `Telnet`：使用 `host` 和可选 `port`，默认端口为 23。
- `Serial`：使用串口字段和波特率字段，默认波特率为 115200。

保存的 FTP/SFTP/SSH 密码可以在 Windows 上安全存储，并镜像到本地加密 vault 中，以便在系统凭据环偶发不可用时仍能恢复。导入导出会以同一 Windows 账号可解密的形式包含这些已保存密码。SSH 终端认证仍委托给 PTY 内运行的 OpenSSH 客户端；当存在已保存密码时，应用会把它回放到密码提示中。

## 内置命令环境

在 Windows 上，GTK 打包脚本会把一个便携的 MSYS2 派生命令工具链复制到：

```text
dist\windows-gtk\tools\msys64
```

当该目录包含所需命令时，应用会新增 `Shell Tools (Bash)` 本地终端入口，并可以在 SSH 终端会话中使用 bundled `ssh.exe`。设置页可以控制是否启用内置命令环境、是否注入到现有本地 shell、SSH 会话是否优先使用内置 OpenSSH 客户端，以及内置命令是否优先于系统 `PATH`。

第一批要求的命令包括 `bash`、`sh`、`curl`、`wget`、`ssh` 和 `telnet`。发布包中存在的其他 GNU/MSYS2 工具会通过同一套 `PATH` overlay 暴露出来。PowerShell 默认保留自己的别名行为；在专门的 PowerShell 工具 profile 加入之前，`Shell Tools (Bash)` 是 GNU 风格命令最可预期的入口。

内置工具链是用户态命令环境。它不会安装驱动，不会修改全局环境变量，也不会存储凭据。发布包含 MSYS2 二进制文件时，需要随包包含对应 license 文件，并定期刷新工具链包以获得安全更新。

## Windows GTK4 环境

只有启用 `gtk-ui` feature 时才需要 GTK4 开发库。Windows 上通常使用 MSYS2：

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
.\scripts\dev-gtk.ps1 run
```

## 架构

- `shell-core`：共享模型、事件、错误和协议/会话类型。
- `shell-terminal`：终端单元格、屏幕缓冲区、ANSI 起步解析器和 scrollback。
- `shell-platform`：默认 shell 检测和 PTY 抽象。
- `shell-protocol`：LocalShell、SSH、Telnet 和 Serial 等协议适配层。
- `shell-renderer`：与 UI 无关的渲染快照，以及可选 GTK4 终端视图。
- `shell-storage`：连接配置持久化。
- `shell-app`：应用入口和 GTK4 UI。

## 内存预算

- 内存使用是产品要求，不是后期优化项。
- 热渲染路径应避免整缓冲区 clone，优先使用可见区域切片。
- scrollback 默认保持有界；除非明确加入用户可配置项，新页签应维持适中的历史行预算。
- 在 Windows 上，当前 GTK4 运行时即使在应用空闲 UI 下也会加载 GStreamer 相关模块。应用层仍应尽量减少 shell/terminal 内存增长，但进一步降低空闲内存还需要更精简的 GTK 运行时选择，而不只是 Rust 侧优化。

## 后续协议工作

- 在凭据存储和 host-key 信任流程设计稳定后，用原生 SSH backend 替换或补充系统 OpenSSH 模式。
- 在终端会话模型稳定后加入 SFTP/FTP 文件传输面板。
- 将 VNC 作为独立 framebuffer viewer 添加，而不是终端渲染器的一部分。
