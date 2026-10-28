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

# 网络层实机校验(拉取官网首页,不校验 version.json;可选,需能访问官网)
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

## 官网(site/)

官网是静态多页站，源文件都在 `site/`，不参与 Rust 构建。页面清单：

| 页面 | 中文 | 英文 |
|---|---|---|
| 首页 | `/` | `/en/` |
| Mac 版 Everything | `/everything-for-mac/` | `/en/everything-for-mac/` |
| Spotlight 搜不到文件 | `/spotlight-not-finding-files/` | `/en/spotlight-not-finding-files/` |
| Mac 文件搜索指南 | `/mac-file-search-guide/` | `/en/mac-file-search-guide/` |
| 交流与反馈 | `/community/` | `/en/community/` |

- 语言：`/` 固定中文，`/en/` 固定英文；语言切换是普通链接，不再依赖 `localStorage`。
- 共享资源：`site/assets/`（CSS、首页 JS、图标、吉祥物、分享图）；CSS/JS 引用带 `?v=YYYYMMDD` 版本串，资源改动时同步更新。
- 字体：全站使用系统字体栈（SF Pro / PingFang SC / SF Mono），不加载 Web 字体。
- 视觉：站点沿用 App 的 token（靛蓝选中色 `#5B6FD9`、深色 `#1A1D22`、12/8/6/4 圆角）；悬浮条演示为 700 宽、结果窗口为 1080×627。
- 演示图标：`site/assets/file-icons/` 由 macOS `NSWorkspace` 按文件类型渲染为 PNG（与访达一致），不是图标库字形；吉祥物为 `site/assets/mascot.png`（透明 PNG）。
- SEO：每页包含 canonical、hreflang、Open Graph、Twitter 卡片与 JSON-LD；`site/sitemap.xml` 与页面 hreflang 互指。
- 本地预览：`cd site && python3 -m http.server 8000`，打开 `http://127.0.0.1:8000/`。
- 下载包：把 `release/macEverything-<version>.dmg` 复制到 `site/downloads/`，按钮指向 `/downloads/macEverything-<version>.dmg`。`site/downloads/` 已加入 `.gitignore`，DMG 不入库。
- 发布前必须运行 `python3 scripts/site/check_seo.py`，退出码 0 才能部署。检查项包括 canonical/hreflang、sitemap 覆盖、JSON-LD、字体引用、站内链接、版本与 DMG 一致性。
- `scripts/release.sh` 生成 DMG 后会同步写入 `site/version.json`；`check_seo.py` 会校验它与 Cargo 版本一致。该文件随 `site/` 一起部署，`_headers` 为它设置 `Cache-Control: no-cache`。
- 分享图源文件：`scripts/site/og-image.html`，渲染为 `site/assets/og-image.png`（1200×630）。
- 官网不写源码构建、终端安装和开发文档入口，只提供成品 DMG。

### Cloudflare Pages

- 项目：`maceverything`；生产分支：`main`；地址：`https://maceverything.pages.dev`。
- 首次创建（新版 Pages 会尝试委派到 Workers，`--force` 使用经典 Pages）：
  `npx wrangler pages project create maceverything --production-branch main --force`
- 发布：

  ```bash
  cd site
  npx wrangler pages deploy . --project-name maceverything --branch main --commit-dirty=true
  ```

- 未登录先 `npx wrangler login`。OAuth 凭据可能在登录与部署之间过期，直接串联：
  `npx wrangler login && npx wrangler pages deploy . --project-name maceverything --branch main --commit-dirty=true`
- `site/_headers` 给 `/downloads/*` 设置一年缓存。
- 自定义域名：Cloudflare Pages 控制台 → Custom domains。`pages.dev` 与 Cloudflare 免费节点在国内访问不稳定；国内速度用阿里云 OSS + 已备案二级域名，上传同一份 `site/` 内容，绑定 CNAME 与 HTTPS 证书。

## v2 方向(未排期)

- 图形界面:悬浮条(全局快捷键唤起)+ 完整窗口 + 设置;菜单栏常驻
- 界面 i18n(中英，v2.1 已交付；繁中/日/韩规划中)
- 引擎候选:正则/`size:`/`dm:` 过滤器、外置盘扫描、`getattrlistbulk` 列信息加速
