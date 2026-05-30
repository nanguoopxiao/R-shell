# GitHub 推送、版本控制与 Release 发布流程

这份文档记录 R-shell 的日常代码推送、版本 tag、GitHub Release 自动发布和回滚处理流程。默认远端仓库为 `origin`，主分支为 `main`。

## 基本原则

- 源码和文档提交到 Git；`target/`、`dist/`、`.msys64/` 等构建产物不提交。
- 日常开发只推送 `main` 分支；公开下载包通过 `v*` tag 触发 GitHub Actions 生成。
- 当前项目版本仍为 `0.1.0`，发布 tag 使用 `v0.1.0`。
- Release 包由 `.github/workflows/release.yml` 自动生成，不手动上传本地 zip，除非 Actions 故障需要临时处理。
- 每次发布前先保证工作区干净，并至少跑一次本地格式、测试和打包验证。

## 日常代码推送

查看状态：

```powershell
git status --short --branch
```

提交改动：

```powershell
git add <files>
git commit -m "简短说明本次改动"
```

推送主分支：

```powershell
git push origin main
```

如果推送时出现 `fetch first` 或 `non-fast-forward`，说明远端已有你本地没有的提交。先同步远端，再推送：

```powershell
git pull --rebase origin main
git push origin main
```

如果 rebase 产生冲突，解决冲突后执行：

```powershell
git add <resolved-files>
git rebase --continue
git push origin main
```

## 发布前检查

提交 Release 前建议运行：

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
.\scripts\dev-gtk.ps1 clippy
.\scripts\package-gtk.ps1 -SmokeTest
```

检查完成后确认没有未提交源码改动：

```powershell
git status --short --branch
```

`dist/` 里的体验包和 zip 是构建产物，应保持被 Git 忽略。

## 使用当前版本 v0.1.0 重新发布

当前阶段如果不想提升版本号，继续使用 `v0.1.0`。因为远端已经存在这个 tag，需要把 tag 移动到最新提交并强制推送该 tag。

确认 `main` 已推送到远端：

```powershell
git push origin main
```

更新本地 tag 到当前提交：

```powershell
git tag -f -a v0.1.0 -m "R-shell v0.1.0"
```

推送 tag 并触发 Release workflow：

```powershell
git push origin refs/tags/v0.1.0 --force
```

GitHub Actions 会自动执行以下步骤：

- 构建 Windows GTK release 版本。
- 运行 `scripts/package-gtk.ps1` 生成 `dist/windows-gtk`。
- 压缩为 `R-shell-windows-gtk-v0.1.0.zip`。
- 生成 `R-shell-windows-gtk-v0.1.0.zip.sha256`。
- 删除同一个 Release 下旧资产。
- 生成 Release 正文，列出从上一个 tag 到当前 tag 的提交摘要。
- 上传新的 zip 和 sha256 到 GitHub Release。

## 发布新版本

当需要正式提升版本号时，例如从 `0.1.0` 到 `0.1.1`：

1. 修改根目录 `Cargo.toml` 中的 workspace version。
2. 如果 README 中写死了示例版本号，同步更新 README。
3. 提交版本号改动。
4. 创建并推送新 tag。

示例：

```powershell
git status --short
git add Cargo.toml README.md README.en.md
git commit -m "Bump version to 0.1.1"
git tag -a v0.1.1 -m "R-shell v0.1.1"
git push origin main
git push origin refs/tags/v0.1.1
```

新版本 tag 不需要 `--force`。只有复用已经存在的 tag 时才需要强制推送 tag。

## Release 说明怎么生成

Release workflow 会生成 `release-notes.md`，并交给 `softprops/action-gh-release` 作为 GitHub Release 正文。

自动正文包含：

```text
## 本次更新
从上一个 tag 到当前 tag 的提交摘要

## 下载
Windows GTK zip 和 sha256 校验文件
```

如果复用并移动同一个 tag，例如继续用 `v0.1.0` 重新发布，workflow 可能找不到上一个 tag；此时会列出最近 20 条提交，避免发布说明为空。

同时 workflow 也启用了 `generate_release_notes: true`，GitHub 会附加自动生成的变更说明。发布完成后，可以在 GitHub Release 页面手动编辑正文，把提交摘要整理成更适合用户阅读的分组，例如：

- 新增功能
- 修复问题
- 打包变化
- 已知问题

## 发布后验证

发布 tag 后，在 GitHub 仓库页面检查：

- Actions 中 `Release` workflow 是否成功。
- Releases 页面是否出现 `v0.1.0`。
- Release 资产是否包含 zip 和 `.sha256`。
- zip 解压后根目录是否有 `R-shell.exe`。
- 解压目录中的 `R-shell.exe` 是否能启动。

本地查看远端 tag：

```powershell
git ls-remote --tags origin "v*"
```

本地同步远端 tag：

```powershell
git fetch --tags --force
```

## 常见处理

如果 Release 资产旧了但 tag 已经推送，可以重新推送同一个 tag：

```powershell
git tag -f -a v0.1.0 -m "R-shell v0.1.0"
git push origin refs/tags/v0.1.0 --force
```

如果 workflow 失败，先在 Actions 页面看失败步骤。常见原因包括：

- MSYS2 包安装失败或网络抖动。
- GTK 依赖路径异常。
- 打包脚本缺少必需命令或 DLL。
- GitHub Release 资产删除/上传权限问题。

修复 workflow 或脚本后，提交到 `main`，再重新推送 tag 触发发布。

## 回滚建议

如果某次 Release 有严重问题：

1. 不要删除源码历史。
2. 先修复问题并提交到 `main`。
3. 继续使用当前版本时，重新移动并推送 `v0.1.0` tag。
4. 如果已经进入正式语义化版本阶段，优先发布一个新的补丁版本，例如 `v0.1.2`。

除非明确要撤回公开版本，否则不要随意删除 GitHub Release；可以先编辑 Release 正文标注已知问题。