# macEverything

macOS 上的全盘文件名即时搜索引擎——对标 Windows 的 [Everything](https://www.voidtools.com/zh-cn/support/everything/)。
**自建索引、不走 Spotlight**:Spotlight 静默搜不到的目录(隐私排除、`.noindex`、开发目录),这里全都能搜到,且权限缺口永远显式可见。

## v2.0 图形界面

- **悬浮条**(⌥Space):跨全屏应用上方可唤起、输入即搜、键盘全操、打开后自动隐藏
- **完整结果窗口**(⌥⇧F):四列表格、列头排序、多选批量、原生右键菜单、状态栏(索引状态/权限缺口/排序提示)
- **菜单栏常驻**:左键切换悬浮条;右键菜单含引擎状态(六态)、缺口入口、开机自启开关、退出
- **开机自启 + 引擎自动拉起**:重启系统后无需手动操作;App 检测引擎,不在则按包内路径优先拉起
- GUI 与命令行两形态并存、互不依赖,共用同一引擎与数据

## 实测性能(Apple M5 + NVMe)

引擎口径:

| 指标 | 实测 | 说明 |
|---|---|---|
| 首扫 | 百万文件 **1.0-1.6s** | 并行遍历;之后每次启动秒开(读落盘索引) |
| 查询 | P99 **2.9-3.6ms** | 123 万条目真实环境实测 2-3ms |
| 增量 | **≤ 0.3s** 可搜 | 文件系统变更实时进索引,空闲 CPU 0% |
| 完整性 | 与独立枚举**对拍零差异** | `find` 能看到的全能搜到 |
| 停机补齐 | 秒级 | 重启重放关机期间事件(mac 版 USN 日志),非全量重扫 |

图形界面口径(2026-10-16 终验收,真实索引 127.8 万条目,release + ad-hoc 签名):

| 指标 | 实测 | 说明 |
|---|---|---|
| 悬浮条唤起 | 中位 **95.4ms** | ⌥Space 按下到可见(5 轮 4 轮 <100ms);系统热键投递约 51ms 为主 |
| 输入到结果 | 中位 **~67ms** | 逐字符输入到结果落地(8 轮 59~71ms) |
| 界面常驻内存 | **136MB** | eframe/glow/glutin 技术栈基线(两窗皆隐即 136M,与窗口使用无关);超「<100MB」设计红线,如实记录,详见[架构总览](docs/architecture.md) |
| 隐藏态 CPU | **0%** | 两窗皆隐 60s 实测零增量 |
| 增量可搜(界面侧) | **267ms** | 新建文件 → 悬浮条可搜 |

## 快速开始

### 图形界面(推荐)

```bash
# 构建 + 组装 + ad-hoc 签名（本地开发），输出 release/macEverything.app
bash scripts/bundle.sh

# 对外分发：Developer ID 签名 + Apple 公证 + DMG
# 需本机 Developer ID 证书与 notarytool 凭据（见 docs/development.md「发布」）
bash scripts/release.sh

# 运行(或把 .app 拖入 /Applications 或 ~/Applications 后双击)
open release/macEverything.app
```

启动后仅菜单栏出现图标(无窗口、无 Dock 图标——正常形态):

- **⌥Space** 唤起/隐藏悬浮条:输入即搜,↑↓ 选择,↵ 打开,⌘↵ 访达显示,⌥↵ 带词进完整窗口,Esc 隐藏
- **⌥⇧F** 打开完整结果窗口:四列(名称/大小/修改时间/路径)、点列头排序、多选、原生右键菜单
- 菜单栏图标:左键切换悬浮条;右键菜单(引擎状态/打开完整窗口/开机自启/退出)
- 首次运行自动注册开机自启;引擎由 App 自动拉起,无需手动操作
- 界面支持简体中文 / English:默认跟随系统,设置内切换,立即生效并持久化

### 命令行(可选,与图形界面并存)

```bash
# 构建 + 安装到 PATH
cargo build --release
ln -sf "$PWD/target/release/macEverything" ~/.cargo/bin/macEverything

# 启动(后台,首次全盘建索引约 1-3 秒)
macEverything start

# 即输即出
macEverything query "报告"
macEverything query "*.pdf 2026"
macEverything query "path:Documents 发票"

# 状态、停止
macEverything status
macEverything stop
```

App 与命令行互不依赖:装 App 不要求命令行在 PATH,反之亦然。App 拉起引擎时优先用**随包的**命令行(包内 `Contents/MacOS/macEverything`),PATH 仅作开发场景兜底。

搜索语法:空格 AND、`*` `?` 通配、`path:` `case:` `whole:` 修饰——完整说明见[使用指南](docs/usage.md)。

## 文档

| 文档 | 内容 |
|---|---|
| [使用指南](docs/usage.md) | 图形界面(悬浮条/完整窗口/托盘/自启)与五命令详解、语法规则、权限、数据目录、排障 |
| [架构总览](docs/architecture.md) | 引擎架构、六大设计决策 + v2 增补(ADR-7/8)、生命周期、性能实测与已知限制 |
| [开发指南](docs/development.md) | 构建/测试/基准、实机冒烟、约定与踩坑记录 |
| [规格档案](docs/specs/) | v1-core(引擎)/ v2-ui(图形界面):产品规格(验收红线)/ 技术规格 / 实现计划 |

## 项目结构

```
crates/engine/   纯逻辑库:索引(扁平 Entry + 双 arena)/ 查询(Finder 预编译)/
                 并行扫描 / mmap 落盘 / FSEvents 直调与停机重放 / 增量 diff
crates/cli/      daemon(引擎宿主)+ 五子命令 client(含离线降级)
crates/protocol/ 协议契约(消息/编解码/校验/版本;cli 与 app 共用)
crates/client/   客户端承载层(连接/按需 stat 读取/离线降级/状态常量;cli 与 app 共用)
crates/app/      GUI(eframe/egui):悬浮条 / 完整结果窗口 / 菜单栏托盘 / 全局热键 / 开机自启
scripts/         五项基准(bench.sh)/ .app 组装(bundle.sh)/ 平台 PoC 与 spike
docs/            使用 / 架构 / 开发文档 + 规格档案(v1-core / v2-ui)
site/            官网(静态多页站;SEO 与 Cloudflare Pages 部署见 docs/development.md)
```

核心设计一句话:**事件只当信号,目录级 diff 才是真相**——增量正确性建立在磁盘比对之上,FSEvents 的粗粒度语义(丢事件、合并、单边改名)全部内建兜底。

## 已知限制

- 扫描范围:用户主目录 + `/Applications`(外置盘/网络盘 v2 候选)
- `case:` 只作用于文件名侧(路径匹配恒不区分大小写)
- 无正则与 `size:`/`dm:` 过滤器(v2 候选)
- 界面常驻内存 **136MB**:图形界面技术栈基线,超「<100MB」设计红线(与窗口是否使用无关)
- 悬浮条被其他窗口完全遮挡时,输入到结果退化到 ~500ms(遮挡视口跳过渲染帧;悬浮条正常可见时不受影响)
- 离线(引擎不在)时完整窗口列排序不生效:状态栏提示并提供「重启引擎」

## 环境要求

macOS 13+(Apple Silicon)、Rust stable。数据目录 `~/Library/Application Support/macEverything/`。
「完全磁盘访问权限」可选授予(不授予时缺口显式列出,功能不受阻)。
