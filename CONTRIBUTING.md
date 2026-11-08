# 参与贡献

欢迎 Issue 与 Pull Request。这个项目是 macOS 专用的文件名搜索引擎，所有功能逻辑在 `crates/engine`，GUI 在 `crates/app`，协议与客户端分别位于 `crates/protocol`、`crates/client`。

## 开发环境

- macOS 13+（Apple Silicon）
- Rust stable
- 无需 Xcode 工程：图形界面由 `bash scripts/bundle.sh` 组装成 `.app`

## 必过检查

提交前以下三项必须全部通过：

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

FSEvents 实机测试默认忽略，在真实 Mac 上手动运行：

```bash
cargo test -p engine --test fsevents_live -- --ignored
```

## 提交约定

- 提交信息使用 Conventional Commits 前缀：`feat:`、`fix:`、`refactor:`、`docs:`、`test:`、`chore:`
- 一个提交只做一件事；行为契约变更先写 `docs/specs/` 下的规格
- 新增测试需做突变验证（临时改坏实现，确认测试变红）

## 代码位置

- `crates/engine`：索引、查询编译、扫描、mmap 落盘、FSEvents 增量
- `crates/protocol` / `crates/client`：daemon 协议与客户端承载层
- `crates/cli`：daemon 与五个子命令
- `crates/app`：eframe/egui 图形界面
- `site/`：静态官网（不参与 Rust 构建）

更多细节见 [开发指南](docs/development.md) 与 [架构总览](docs/architecture.md)。
