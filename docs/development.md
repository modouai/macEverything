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
│       ├── bundle.rs      bundle 扩展名判定(ASCII 大小写不敏感后缀)
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
        ├── main.rs        子命令分派 + start 后台化 + SIGPIPE 复位 + 退出码(0/1/2)
        ├── args.rs        参数解析(--data-dir 前置/`=` 形式、--json/--sort/--desc、绝对降序换算、数据目录优先级)
        ├── json.rs        --json 输出构造(query/status/错误对象,schema_version=1)
        ├── daemon.rs      引擎宿主(冷启动编排/增量循环/服务/单实例/落盘锁/目录 0700·文件 0600)
        └── daemon/        volumes 子模块

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
cargo test --workspace     # 全量测试

# FSEvents 实机集成(需真实文件系统事件;CI 默认跳过)
cargo test -p engine --test fsevents_live -- --ignored

# 网络层实机校验(拉取公开 HTTPS 地址;可选,需要网络)
cargo test -p macEverything-app platform::mac::tests::live_fetch_returns_bytes -- --ignored

# 开发时临时覆盖清单地址(HTTPS;仅用于验收)
MACEEVERYTHING_UPDATE_URL=https://example.com/version.json cargo run -p macEverything-app
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

## CLI 集成(v2.5)

协议与 JSON 契约的对外入口是 `docs/usage.md`「--json 输出契约」;实现要点:

- **数据目录覆盖**:`--data-dir`(仅子命令前,空格/`=` 均可)> `MCE_DATA_DIR` > `$HOME` 默认(`cli/args.rs::resolve_data_dir`);env 为空/相对路径报错,**不回落默认目录**——静默换目录会让客户端连错 daemon。
- **App 拉起引擎固定目录**:`engine_launcher` spawn 时显式 `.env("MCE_DATA_DIR", paths.data_dir)`;外部同名变量(终端启动/`launchctl setenv`)不继承,App 永远连默认目录。
- **安装器**:`crates/app/src/cli_install/`(登录 shell PATH 探测 + 分类状态机 + 原子安装/修复/卸载);测试经 `test_fs.rs` 注入文件系统与 shell 结果,不触真实磁盘,新增分支同样走注入测试。

## 关键约定与陷阱(实现期踩坑记录)

- **事件只当信号,目录 diff 才是真相**:任何增量改动不得信任 FSEvents 的单事件语义。
- **`memmem::find` 会重建 Finder**:热路径一律用预编译 `Finder`(`CPattern` 模式)。
- **落盘并发**:`write_atomic` 的 tmp 名含 pid+序号;daemon 侧所有落盘走 `flush_lock`。新增落盘路径必须同样串行化。
- **排除判定三条路径共用** `scanner::is_excluded`(首扫/增量/补链),新路径不得另写判定。
- **FSEvents 拆除必须在 runloop 线程**:跨线程 Invalidate 会 SIGTRAP(实机实证)。
- **`/tmp` 是符号链接**:FSEvents 上报 canonical 路径;daemon 的 dir_map 键与事件路径经 canonicalize 对齐。
- **重放追平语义**:HistoryDone 前强制 drain pending 事件,否则「就绪」早于索引追上。
- **新目录事件先于父 diff 到达**:`diff_one` 的补链逻辑(逐级向下)不得简化回直查。
- **换 Base 必重建 dir_map**:任何替换 holder 的路径(flush 合并写/重扫挂载/冷启动)必须同时重建 dir_map,否则旧条目号命中新索引无关活条目 → diff 副本风暴 → 索引膨胀 + 挂起。
- **负缓存键用路径不用条目号**:`compact` 全量重排条目号,条目号键会错杀无辜目录(内容静默不更新)。
- **图标解码不要走 TIFFRepresentation**:每个图标 37–74MB 多页 TIFF(APP 图标 16bit float),一屏 30 个 = 2GB 级临时块;用 `NSImage.drawInRect` 直接光栅化到目标尺寸(16KB)。`fromRect` 必须是图像完整逻辑边界,传目标矩形会只画左上 1/4。
- **图集增量尺寸要收敛**:epaint `take_delta` 按任意脏区尺寸分配 ColorImage,macOS malloc 按尺寸类缓存 → 无界增长;vendor patch 已做(2 的幂量化 + 缓冲复用 + Arc 共享 + 原地 clear)。
- **`max_texture_side` 必须把所有视口统一成 Some**:eframe 只给挂了 egui_winit 的视口注入值,悬浮条(ADR-8 viewport hack)可能为 None;None/Some 交替会让 epaint 每 pass 清空图集并重排全部文本。

## 规格与文档

| 文档 | 内容 | 更新时机 |
|---|---|---|
| `docs/specs/v1-core/product.md` | 产品规格(验收红线/语法契约/数据契约) | 行为契约变更时 |
| `docs/specs/v1-core/tech.md` | 技术规格(6 ADR + 实测回填 + 实现偏差清单) | 架构决策或实现偏差时 |
| `docs/specs/v1-core/plan.md` | 实现计划(21 任务,含验证标准) | 历史存档,新任务开新 spec |
| `docs/architecture.md` | 架构总览(实现态) | 模块/数据流变更时 |
| `docs/usage.md` | 使用指南 | 命令/语法/排障变更时 |

## 发布

对外分发使用 Developer ID 签名 + Apple 公证：

- 版本号：`Cargo.toml` 的 `[workspace.package].version`（写入 `Info.plist` 与 DMG 文件名）。
- 本地开发：`bash scripts/bundle.sh` 默认 ad-hoc 签名。
- Developer ID 构建：`MACEEVERYTHING_SIGN_IDENTITY="Developer ID Application: ..." bash scripts/bundle.sh`（hardened runtime + timestamp）。
- 一键发布：`bash scripts/release.sh`（ZIP 公证 → staple App → 生成 DMG → 签名/公证/staple → SHA256）。
- 公证凭据：优先 keychain profile（默认 `maceverything-notary`；先执行 `xcrun notarytool store-credentials`），钥匙串锁定时回落到 `APPLE_ID` / `APPLE_TEAM_ID` / `APPLE_APP_SPECIFIC_PASSWORD`。
- 产物：`release/macEverything-<version>.dmg`（+ `.sha256`）与 `.zip`（公证中间产物）。
- 验收：`spctl --assess --type execute`（App）、`spctl --assess --type open --context context:primary-signature`（DMG）、`xcrun stapler validate`、`hdiutil verify`。

## v2 方向(未排期)

- 图形界面:悬浮条(全局快捷键唤起)+ 完整窗口 + 设置;菜单栏常驻
- 界面 i18n(中英，v2.1 已交付；繁中/日/韩规划中)
- 引擎候选:正则/`size:`/`dm:` 过滤器、外置盘扫描、`getattrlistbulk` 列信息加速
