# 安全策略

## 支持的版本

只对最新发布版本提供安全修复。旧版本的安装包与索引格式不保证兼容。

## 报告漏洞

请通过 GitHub 的私密漏洞报告提交：

1. 打开仓库的 **Security** 标签页
2. 点击 **Report a vulnerability**
3. 说明复现步骤、受影响版本与影响范围

不要为安全漏洞开公开 Issue。我们会在确认后回复，并在修复发布后致谢（除非你希望匿名）。

## 范围

- macOS 应用（`crates/app`）与命令行（`crates/cli`）
- 搜索引擎（`crates/engine`）与 daemon 协议（`crates/protocol`）

## 已知设计说明

- 索引保存在本机 `~/Library/Application Support/macEverything/`，不上传
- 应用唯一的联网行为是检查新版本（读取 `version.json`）
