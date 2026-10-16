# 开发指南

## 前置

- macOS(Apple Silicon;引擎依赖 FSEvents,不支持其他平台)
- Rust stable(rustup;工具链锁定见 `rust-toolchain` 或直接用 stable 最新)
- crates.io 直连受限时用镜像:`cargo --config 'source.crates-io.replace-with="rsproxy"' ...`

## 目录结构

```
crates/
├── engine/                纯逻辑库(所有业务逻辑;对 GUI 零依赖)
│   └── src/
│       ├── entry.rs       Entry(24B)+ NFC/折叠归一 + 标志位
│       ├── arena.rs       追加式名称池
│       ├── index.rs       索引:增删改/目录 diff/子树级联/墓碑整理/目录路径池/IndexView trait
│       ├── query.rs       语法解析 → CPattern 预编译(Finder) → 匹配 → 打分排序
│       ├── scanner.rs     并行扫描(work-sharing)/ 排除判定(三条路径共用)
│       ├── persist.rs     落盘写(tmp+rename)/ mmap 加载 / SharedView / materialize
│       ├── fsevents.rs    FSEvents C API 直调(unsafe 集中;17 处全配 SAFETY)
│       ├── watcher.rs     DirBatcher(100ms 窗)+ apply_dir_diff
│       ├── reconcile.rs   保守判定器 / 兜底重扫 / 排除展开
│       ├── config.rs      config.toml 读写
│       └── error.rs       错误类型
│   ├── examples/          bench_query(引擎级查询分布)/ spike_fsevents(行为观测)
│   └── tests/             parity(对拍)/ fsevents_live(实机,#[ignore])
└── cli/
    └── src/
        ├── main.rs        五子命令分派 + start 后台化 + SIGPIPE 复位
        ├── protocol.rs    消息契约(长度前缀/版本字段/path 校验)
        ├── daemon.rs      引擎宿主(冷启动编排/增量循环/服务/单实例/落盘锁)
        └── client.rs      连接/离线降级/输出格式化

docs/
├── architecture.md        架构总览(实现态)
├── usage.md               使用指南
├── development.md         本文
└── specs/v1-core/         规格:product.md / tech.md(6 ADR)/ plan.md(21 任务)
scripts/
├── bench.sh               五项基准(隔离 MCE_ROOTS)
└── spike-readdir/         合成树生成 + readdir 吞吐 spike
```

## 构建与验证

```bash
cargo build --release      # 产物 target/release/macEverything
cargo fmt --check          # 格式门禁
cargo clippy --workspace --all-targets -- -D warnings   # 静态检查门禁(零容忍)
cargo test --workspace     # 全量测试(79 项:引擎 71 + 对拍 2 + CLI 6)

# FSEvents 实机集成(需真实文件系统事件;CI 默认跳过)
cargo test -p engine --test fsevents_live -- --ignored
```

**声明完成前必须全绿**:`fmt --check && clippy -D warnings && test`。涉及 FSEvents/守护进程的改动,另跑实机链路(见下)。

## 测试分层

| 层 | 位置 | 说明 |
|---|---|---|
| 单元(TDD) | 各模块 `#[cfg(test)]` | 解析/匹配/索引/持久化/批处理/判定器——纯逻辑,`tempfile` 受控树 |
| 对拍 | `tests/parity.rs` | 索引条目集 vs 独立实现的递归枚举,零差异;串行/并行/参考三方闭合 |
| 实机 | `tests/fsevents_live.rs` | FSEvents 建流/收事件/重放 HistoryDone;`#[ignore]`,实机跑 |
| 冒烟 | 手工 | `start → status → query → 增量 → stop → 离线 → 重启重放`(见下) |

**钉住测试纪律**:新增测试须做突变验证(临时改坏实现看测试是否变红),防止「恒绿假钉」。

## 实机冒烟(改动 daemon/增量后必跑)

```bash
# 隔离环境(不碰真实数据目录)
mkdir -p /tmp/mce-dev/Documents && echo x > /tmp/mce-dev/Documents/探针.md
HOME=/tmp/mce-dev MCE_ROOTS=/tmp/mce-dev ./target/release/macEverything start
HOME=/tmp/mce-dev ./target/release/macEverything status          # 就绪
HOME=/tmp/mce-dev ./target/release/macEverything query "探针"     # 命中 + 绝对路径
touch /tmp/mce-dev/Documents/增量探针.txt && sleep 1
HOME=/tmp/mce-dev ./target/release/macEverything query "增量探针" # 增量 ≤1s 可搜
HOME=/tmp/mce-dev ./target/release/macEverything stop
HOME=/tmp/mce-dev ./target/release/macEverything query "探针"     # 离线自答
touch /tmp/mce-dev/重放探针.txt
HOME=/tmp/mce-dev MCE_ROOTS=/tmp/mce-dev ./target/release/macEverything start
sleep 1 && HOME=/tmp/mce-dev ./target/release/macEverything query "重放探针"  # 重放补齐
HOME=/tmp/mce-dev ./target/release/macEverything stop
```

## 基准

```bash
bash scripts/bench.sh /tmp/mce-bench 1000000   # 五项:首扫/查询 P99/增量/落盘/停机重放
```

- 自动隔离(`MCE_ROOTS` 指向合成树 + 临时 HOME),测后清理;合成树由 `scripts/spike-readdir` 生成。
- 单轮测量受系统负载影响(实测偶见 40ms 噪声样本);判定以多轮中位为准。
- 引擎级查询分布(不经过 daemon):`cargo run --release -p engine --example bench_query -- <index.bin> <总数>`。

## 关键约定与陷阱(实现期踩坑记录)

- **事件只当信号,目录 diff 才是真相**:任何增量改动不得信任 FSEvents 的单事件语义。
- **`memmem::find` 会重建 Finder**:热路径一律用预编译 `Finder`(`CPattern` 模式)。
- **落盘并发**:`write_atomic` 的 tmp 名含 pid+序号;daemon 侧所有落盘走 `flush_lock`。新增落盘路径必须同样串行化。
- **排除判定三条路径共用** `scanner::is_excluded`(首扫/增量/补链),新路径不得另写判定。
- **FSEvents 拆除必须在 runloop 线程**:跨线程 Invalidate 会 SIGTRAP(实机实证)。
- **`/tmp` 是符号链接**:FSEvents 上报 canonical 路径;daemon 的 dir_map 键与事件路径经 canonicalize 对齐。
- **重放追平语义**:HistoryDone 前强制 drain pending 事件,否则「就绪」早于索引追上。
- **新目录事件先于父 diff 到达**:`diff_one` 的补链逻辑(逐级向下)不得简化回直查。

## 规格与文档

| 文档 | 内容 | 更新时机 |
|---|---|---|
| `docs/specs/v1-core/product.md` | 产品规格(验收红线/语法契约/数据契约) | 行为契约变更时 |
| `docs/specs/v1-core/tech.md` | 技术规格(6 ADR + 实测回填 + 实现偏差清单) | 架构决策或实现偏差时 |
| `docs/specs/v1-core/plan.md` | 实现计划(21 任务,含验证标准) | 历史存档,新任务开新 spec |
| `docs/architecture.md` | 架构总览(实现态) | 模块/数据流变更时 |
| `docs/usage.md` | 使用指南 | 命令/语法/排障变更时 |

## 发布

v1 为个人使用,ad-hoc 签名本地运行,无对外分发流程。若后续发布:Developer ID 签名 + 公证 + `SMAppService` 登录自启(v2 候选)。

## v2 方向(未排期)

- 图形界面:悬浮条(全局快捷键唤起)+ 完整窗口 + 设置;菜单栏常驻
- 界面 i18n(中英)
- 引擎候选:正则/`size:`/`dm:` 过滤器、外置盘扫描、`getattrlistbulk` 列信息加速
