# R-shell 跨电脑开发与 GitHub 版本控制教程

这份教程只按当前项目写，命令都落到 R-shell 的真实路径、真实远端、真实分支和真实文件名。

当前项目固定信息：

```text
本地路径：E:\shell (2)
GitHub 仓库：https://github.com/nanguoopxiao/R-shell
Git 远端：https://github.com/nanguoopxiao/R-shell.git
主分支：main
远端名：origin
提交用户名：nanguoopxiao
提交邮箱：37125971+nanguoopxiao@users.noreply.github.com
```

核心习惯只有一句话：

```text
开始写之前先 pull，写完之后 commit，再 push，换电脑之后再 pull。
```

## 1. 在当前电脑确认项目连接正确

打开 PowerShell，进入当前项目：

```powershell
Set-Location -LiteralPath "E:\shell (2)"
```

查看当前分支：

```powershell
git branch --show-current
```

当前项目应该输出：

```text
main
```

查看 GitHub 远端：

```powershell
git remote -v
```

当前项目应该看到：

```text
origin  https://github.com/nanguoopxiao/R-shell.git (fetch)
origin  https://github.com/nanguoopxiao/R-shell.git (push)
```

查看本地状态：

```powershell
git status --short --branch
```

当前项目的正常分支关系应该包含：

```text
## main...origin/main
```

这表示本地 `main` 正在跟踪 GitHub 上的 `origin/main`。

## 2. 在两台电脑都配置同一个 Git 身份

家里电脑和公司电脑都执行下面这些命令。这样两台电脑提交出来的 commit 作者会一致。

```powershell
git config --global user.name "nanguoopxiao"
```

```powershell
git config --global user.email "37125971+nanguoopxiao@users.noreply.github.com"
```

```powershell
git config --global init.defaultBranch main
```

检查配置：

```powershell
git config --global user.name
```

```powershell
git config --global user.email
```

应该分别看到：

```text
nanguoopxiao
```

```text
37125971+nanguoopxiao@users.noreply.github.com
```

## 3. 公司电脑第一次下载 R-shell

如果公司电脑还没有这个项目，就从 GitHub 克隆一份。下面命令把项目放到和当前电脑一样的位置：`E:\shell (2)`。

进入 `E:\`：

```powershell
Set-Location -LiteralPath "E:\"
```

从当前公开仓库克隆：

```powershell
git clone https://github.com/nanguoopxiao/R-shell.git "shell (2)"
```

进入项目：

```powershell
Set-Location -LiteralPath "E:\shell (2)"
```

确认远端：

```powershell
git remote -v
```

确认分支：

```powershell
git status --short --branch
```

第一次克隆公开仓库时不需要登录。以后要 `git push origin main` 时，Git 会要求登录 GitHub。

## 4. 每次开始写代码前

家里电脑和公司电脑都一样。先进入 R-shell：

```powershell
Set-Location -LiteralPath "E:\shell (2)"
```

查看当前状态：

```powershell
git status --short --branch
```

拉取 GitHub 上的最新代码：

```powershell
git pull --rebase origin main
```

如果这一步成功，说明当前电脑已经拿到 GitHub 上最新的 `main`。

如果提示本地有未提交改动，先临时保存这些改动：

```powershell
git stash push -m "WIP before pulling R-shell main"
```

然后再拉取：

```powershell
git pull --rebase origin main
```

再把刚才临时保存的改动拿回来：

```powershell
git stash pop
```

## 5. 只提交这份教程文档

如果你现在只想把本文档提交到公开仓库，执行这组命令：

```powershell
Set-Location -LiteralPath "E:\shell (2)"
```

```powershell
git status --short
```

```powershell
git diff -- docs/git-cross-device-workflow.md
```

```powershell
git add -f docs/git-cross-device-workflow.md
```

```powershell
git commit -m "Document R-shell cross-device GitHub workflow"
```

```powershell
git push origin main
```

推送后，打开这个地址就能看到 GitHub 上的文档：

```text
https://github.com/nanguoopxiao/R-shell/blob/main/docs/git-cross-device-workflow.md
```

## 6. 提交当前工作区里这批改动

我当前看到的工作区改动包含这些文件：

```text
.gitignore
Cargo.toml
README.en.md
README.md
crates/app/src/gtk_app.rs
crates/app/src/gtk_app/formatting.rs
crates/app/src/gtk_app/session_tabs.rs
crates/app/src/gtk_app/sftp_ui.rs
crates/protocol/src/sftp.rs
docs/code-analysis.md
docs/git-cross-device-workflow.md
docs/release-process.md
```

如果你确认这些都要一起进入公开仓库，通常一个命令就可以加入暂存区：

```powershell
Set-Location -LiteralPath "E:\shell (2)"
```

```powershell
git add -A
```

`git add -A` 会暂存当前仓库里所有新增、修改和删除的可跟踪文件。当前这份教程第一次加入仓库时，因为 `.gitignore` 里有 `/docs/`，需要额外强制加入一次；加入过以后，后续修改也可以被 `git add -A` 正常暂存。

提交：

```powershell
git commit -m "Update R-shell documentation and GTK SFTP code"
```

推送到当前公开仓库：

```powershell
git push origin main
```

推送完成后，GitHub 仓库地址是：

```text
https://github.com/nanguoopxiao/R-shell
```

## 7. 家里电脑和公司电脑的日常接力

家里电脑开始写：

```powershell
Set-Location -LiteralPath "E:\shell (2)"
```

```powershell
git pull --rebase origin main
```

家里电脑写完后检查：

```powershell
git status --short
```

提交家里电脑这次修改过的 R-shell 文件。下面以当前这份教程为例：

```powershell
git add -f docs/git-cross-device-workflow.md
```

```powershell
git commit -m "Refine R-shell GitHub workflow guide"
```

```powershell
git push origin main
```

到了公司电脑，先同步家里电脑刚推上去的内容：

```powershell
Set-Location -LiteralPath "E:\shell (2)"
```

```powershell
git pull --rebase origin main
```

公司电脑写完后同样提交并推送。下面以 SFTP UI 文件为例：

```powershell
git add crates/app/src/gtk_app/sftp_ui.rs
```

```powershell
git add crates/protocol/src/sftp.rs
```

```powershell
git commit -m "Improve R-shell SFTP transfer UI"
```

```powershell
git push origin main
```

回到家里电脑，再拉一次：

```powershell
Set-Location -LiteralPath "E:\shell (2)"
```

```powershell
git pull --rebase origin main
```

这就是跨电脑开发 R-shell 的完整循环。

## 8. 公开仓库提交前的安全检查

R-shell 是公开仓库，所以每次 `git push origin main` 之前都建议做这些检查。

进入项目：

```powershell
Set-Location -LiteralPath "E:\shell (2)"
```

查看准备提交的文件：

```powershell
git status --short
```

查看已经暂存、即将提交的内容：

```powershell
git diff --cached
```

检查 `.env` 是否会被忽略：

```powershell
git check-ignore -v .env
```

检查 `target/` 是否会被忽略：

```powershell
git check-ignore -v target
```

检查 `dist/` 是否会被忽略：

```powershell
git check-ignore -v dist
```

检查当前 `.gitignore` 是否会忽略新增文档：

```powershell
git check-ignore -v --no-index docs/git-cross-device-workflow.md
```

当前 `.gitignore` 包含 `/docs/`，所以提交这份新增教程时要使用：

```powershell
git add -f docs/git-cross-device-workflow.md
```

搜索仓库里已经被 Git 跟踪的敏感词：

```powershell
git grep -n "password"
```

```powershell
git grep -n "token"
```

```powershell
git grep -n "secret"
```

这些命令如果没有输出，通常表示当前被 Git 跟踪的文件里没搜到对应词。

当前 `.gitignore` 已经包含这些忽略规则，其中 `/docs/` 会让新增文档默认无法加入暂存区：

```text
/target/
/dist/
/artifacts/
/ui-probes/
/picture/
/docs/
/.msys64/
/.idea/
/.vscode/
*.log
*.local
.env
.env.*
**/secrets.json
**/profiles.json
**/settings.json
**/local-terminals-cache.json
```

如果公开仓库里已经提交过密码、token 或私有配置，只删除文件再提交是不够的。应该先去对应平台作废旧密钥，再清理 Git 历史。

## 9. 推送失败时怎么处理

如果执行：

```powershell
git push origin main
```

看到 `fetch first` 或 `non-fast-forward`，说明 GitHub 上有当前电脑还没有的提交。

先拉取并整理历史：

```powershell
git pull --rebase origin main
```

再推送：

```powershell
git push origin main
```

如果 rebase 出现冲突，先查看冲突文件：

```powershell
git status --short
```

假设冲突发生在当前教程文件，打开并手动整理：

```text
docs/git-cross-device-workflow.md
```

整理完后标记已解决：

```powershell
git add -f docs/git-cross-device-workflow.md
```

继续 rebase：

```powershell
git rebase --continue
```

最后推送：

```powershell
git push origin main
```

如果处理冲突时发现方向错了，可以取消这次 rebase：

```powershell
git rebase --abort
```

## 10. 使用分支开发 R-shell 功能

个人项目可以直接推 `main`。如果一个功能要改几天，建议开分支。

进入项目：

```powershell
Set-Location -LiteralPath "E:\shell (2)"
```

先同步主分支：

```powershell
git switch main
```

```powershell
git pull --rebase origin main
```

创建 SFTP UI 功能分支：

```powershell
git switch -c feature/sftp-ui
```

开发完成后加入相关文件：

```powershell
git add crates/app/src/gtk_app.rs
```

```powershell
git add crates/app/src/gtk_app/sftp_ui.rs
```

```powershell
git add crates/protocol/src/sftp.rs
```

提交功能分支：

```powershell
git commit -m "Improve R-shell SFTP UI"
```

推送功能分支：

```powershell
git push -u origin feature/sftp-ui
```

然后在 GitHub 打开 Pull Request 页面：

```text
https://github.com/nanguoopxiao/R-shell/pulls
```

把 `feature/sftp-ui` 合并到 `main`。

Pull Request 合并后，回到本地 `main`：

```powershell
git switch main
```

```powershell
git pull --rebase origin main
```

删除本地功能分支：

```powershell
git branch -d feature/sftp-ui
```

删除 GitHub 上的功能分支：

```powershell
git push origin --delete feature/sftp-ui
```

## 11. 查看历史和定位问题

进入项目：

```powershell
Set-Location -LiteralPath "E:\shell (2)"
```

查看最近 10 次提交：

```powershell
git log --oneline --decorate -10
```

查看当前教程文件的修改：

```powershell
git diff -- docs/git-cross-device-workflow.md
```

查看 README 的修改：

```powershell
git diff -- README.md
```

查看某次提交的内容。当前仓库最近有一个提交是 `ac2730e`：

```powershell
git show ac2730e
```

查看当前本地分支：

```powershell
git branch
```

查看本地和远端分支：

```powershell
git branch -a
```

## 12. 撤销当前项目里的本地修改

这些命令会丢弃本地修改，执行前要确认真的不要这些改动。

丢弃当前教程文件的本地修改：

```powershell
git restore docs/git-cross-device-workflow.md
```

丢弃 README 的本地修改：

```powershell
git restore README.md
```

把已经 `git add` 的教程文件从暂存区拿出来，但保留文件内容：

```powershell
git restore --staged docs/git-cross-device-workflow.md
```

把已经 `git add` 的 README 从暂存区拿出来，但保留文件内容：

```powershell
git restore --staged README.md
```

## 13. 和 R-shell Release 流程的关系

日常跨电脑开发只需要这几步：

```powershell
Set-Location -LiteralPath "E:\shell (2)"
```

```powershell
git pull --rebase origin main
```

```powershell
git add -f docs/git-cross-device-workflow.md
```

```powershell
git commit -m "Refine R-shell GitHub workflow guide"
```

```powershell
git push origin main
```

正式发布版本时，再看这个文件：

```text
docs/release-process.md
```

Release 才需要处理 `Cargo.toml` 版本号、`v0.1.0` 这类 tag、GitHub Actions 和压缩包资产。平时在两台电脑之间开发，不需要每次都打 tag。

## 14. 最短记忆版

每次打开 R-shell 项目：

```powershell
Set-Location -LiteralPath "E:\shell (2)"
```

```powershell
git pull --rebase origin main
```

写完当前教程后：

```powershell
git add -f docs/git-cross-device-workflow.md
```

```powershell
git commit -m "Refine R-shell GitHub workflow guide"
```

```powershell
git push origin main
```

换另一台电脑：

```powershell
Set-Location -LiteralPath "E:\shell (2)"
```

```powershell
git pull --rebase origin main
```

照这个顺序做，家里电脑、公司电脑和 GitHub 上的 `nanguoopxiao/R-shell` 就能保持同步。
