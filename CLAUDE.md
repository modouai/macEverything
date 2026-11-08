# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目是什么

macOS 全盘文件名即时搜索引擎（对标 Windows Everything），**自建索引、不走 Spotlight**。Rust workspace：引擎库 + daemon/CLI + eframe/egui 图形界面。仅支持 macOS（引擎直调 FSEvents C API）。

详细文档（CLAUDE.md 只保留要点，细节以它们为准）：

- `docs/development.md` — 构建/测试/基准/发布、实机冒烟脚本、关键约定与陷阱
- `docs/architecture.md` — 实现态架构、六大设计决策、ADR-7/8
- `docs/usage.md` — 命令与搜索语法
- `docs/specs/` — 规格档案（v1-core 引擎 / v2-ui 图形界面 / 各版本特性）

## 常用命令

```bash
cargo build --release                                     # 产物 target/release/macEverything
cargo fmt --check                                         # 格式门禁
cargo clippy --workspace --all-targets -- -D warnings     # 静态检查门禁（零容忍）
cargo test --workspace                                    # 全量测试
cargo test -p engine <名称>                               # 单个测试
cargo test -p engine --test fsevents_live -- --ignored    # FSEvents 实机集成（#[ignore]，CI 默认跳过）
```

**声明完成前必须全绿**：`fmt --check && clippy -D warnings && test` 三门禁同时通过。改动涉及 daemon/增量逻辑后另跑实机冒烟（`docs/development.md` 有完整脚本，用 `HOME=/tmp/mce-dev MCE_ROOTS=...` 隔离环境，不碰真实数据目录）。

图形界面：`bash scripts/bundle.sh`（构建 + 组装 .app + ad-hoc 签名 → `release/macEverything.app`）；对外分发 `bash scripts/release.sh`（Developer ID 签名 + 公证 + DMG + 同步站点仓 `version.json`）。

基准：`bash scripts/bench.sh /tmp/mce-bench 1000000`（五项基准，自动隔离环境）。

## 架构（大图）

单 daemon 模型：`cli` 的 daemon 是引擎宿主；`cli` 与 `app`（GUI）是它的两个**独立**客户端，经 Unix socket + 长度前缀 JSON 通信，互不依赖。五个 crate：

- `engine` — 全部业务逻辑的纯逻辑库，对 GUI 零依赖：扁平 Entry 索引（24B/条 + 双名称池）、查询预编译（Finder）、并行扫描、mmap 落盘（内存布局镜像，零反序列化）、FSEvents 直调与停机事件重放、目录级 diff 增量。
- `protocol` — 协议契约的唯一真相（消息/编解码/校验/版本），cli 与 app 共用。
- `client` — 客户端承载层的唯一行为真相（连接/按需 stat/离线降级/状态常量），cli 与 app 共用。
- `cli` — daemon（冷启动编排/增量循环/落盘锁）+ 五子命令 `start/stop/query/status/exclude`（含离线降级）。
- `app` — GUI：悬浮条（⌥Space）+ 完整结果窗口（⌥⇧F）双视口、托盘、全局热键、开机自启、引擎拉起（`engine_launcher.rs`）、发现新版本（`update.rs`）、i18n。

核心设计：FSEvents 事件只是提示，daemon 收到后重读该目录、与索引 diff，始终以磁盘为准；任何增量改动不得信任 FSEvents 的单事件语义。

GUI 平台层（ADR-8，改动前先读 `docs/specs/v2-ui/tech.md`）：悬浮条 = eframe 单栈 + 运行期 `object_setClass` 换成自定义 NSPanel 子类，五要素组合（canBecomeKeyWindow/NonactivatingPanel/Floating/CanJoinAllSpaces/orderFrontRegardless）缺一不可；改类后窗口销毁前必须还原窗口类（否则 winit drop abort）。GUI 平台层 unsafe 集中在 `crates/app/src/platform/`（mac.rs + mac_quicklook.rs + mod.rs）；例外零散分布在 `autostart.rs`、`cli_install/`、`ui/mod.rs`、`ui/results.rs`、`ui/theme.rs`（收敛为后续项，不在本次迁移）。

## 关键约定（全部实测踩坑，见 development.md）

- 热路径匹配一律用预编译 `Finder`（`CPattern`）；`memmem::find` 每次调用重建 Finder，是百万条目查询的性能杀手。
- daemon 侧所有落盘走 `flush_lock` 串行化 + tmp 名唯一化（`write_atomic`）；新增落盘路径必须同样串行化——并发写曾实测互相踩踏损坏索引。
- 排除判定三条路径（首扫/增量/补链）共用 `scanner::is_excluded`，新路径不得另写判定。
- FSEvents 流拆除（Stop/Invalidate/Release）必须在流所属 runloop 线程执行——跨线程 Invalidate 实测 SIGTRAP。
- `/tmp` 是符号链接：daemon 的 dir_map 键与事件路径经 canonicalize 对齐。
- 重放追平：HistoryDone 标记前强制 drain 全部 pending 事件再置「就绪」，否则就绪早于索引追上。
- 新目录事件可能先于其父目录 diff 到达：`diff_one` 的补链逻辑（逐级向下）不得简化回直查。

## 测试纪律

- 新增测试须做**突变验证**（临时改坏实现，确认测试变红），防止「恒绿假钉」。
- 测试分层：单元（各模块 `#[cfg(test)]`，tempfile 受控树）→ 对拍（`engine/tests/parity.rs`，索引条目集 vs 独立递归枚举零差异）→ 实机（`#[ignore]` 标注，手动跑）。

## 规格（spec-first）

行为契约变更先写规格再动代码：`docs/specs/<feature>/{product,tech,plan}.md`；superpowers 流程的设计/计划文档在 `docs/superpowers/{specs,plans}/`。文档与实现的同步时机见 `docs/development.md`「规格与文档」表。

## 发版流程（每个新版本）

按顺序执行，前一步不通过不进入下一步。版本号唯一来源是 `Cargo.toml` 的 `[workspace.package].version`。

1. **升版**：改 `[workspace.package].version`（严格 `major.minor.patch`）。
2. **三门禁**：`cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`。
3. **出包**：`bash scripts/release.sh` —— Developer ID 签名 + 公证 + staple + DMG + SHA256。
4. **验收**：本机安装新包确认「关于」页版本号。

