# macEverything

macOS 上的全盘文件名即时搜索引擎——对标 Windows 的 [Everything](https://www.voidtools.com/zh-cn/support/everything/)。
**自建索引、不走 Spotlight**:Spotlight 静默搜不到的目录(隐私排除、`.noindex`、开发目录),这里全都能搜到,且权限缺口永远显式可见。

## 实测性能(Apple M5 + NVMe)

| 指标 | 实测 | 说明 |
|---|---|---|
| 首扫 | 百万文件 **1.0-1.6s** | 并行遍历;之后每次启动秒开(读落盘索引) |
| 查询 | P99 **2.9-3.6ms** | 123 万条目真实环境实测 2-3ms |
| 增量 | **≤ 0.3s** 可搜 | 文件系统变更实时进索引,空闲 CPU 0% |
| 完整性 | 与独立枚举**对拍零差异** | `find` 能看到的全能搜到 |
| 停机补齐 | 秒级 | 重启重放关机期间事件(mac 版 USN 日志),非全量重扫 |

## 快速开始

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

搜索语法:空格 AND、`*` `?` 通配、`path:` `case:` `whole:` 修饰——完整说明见[使用指南](docs/usage.md)。

## 文档

| 文档 | 内容 |
|---|---|
| [使用指南](docs/usage.md) | 五命令详解、语法规则、权限、数据目录、排障 |
| [架构总览](docs/architecture.md) | 引擎架构、六大设计决策、生命周期、性能设计 |
| [开发指南](docs/development.md) | 构建/测试/基准、实机冒烟、约定与踩坑记录 |
| [规格档案](docs/specs/v1-core/) | 产品规格(验收红线)/ 技术规格(6 ADR)/ 实现计划 |

## 项目结构

```
crates/engine/   纯逻辑库:索引(扁平 Entry + 双 arena)/ 查询(Finder 预编译)/
                 并行扫描 / mmap 落盘 / FSEvents 直调与停机重放 / 增量 diff
crates/cli/      daemon(引擎宿主)+ 五子命令 client(含离线降级)
docs/            使用 / 架构 / 开发文档 + 规格档案
scripts/         五项基准(bench.sh)/ 合成树 spike
```

核心设计一句话:**事件只当信号,目录级 diff 才是真相**——增量正确性建立在磁盘比对之上,FSEvents 的粗粒度语义(丢事件、合并、单边改名)全部内建兜底。

## 已知限制

- 扫描范围:用户主目录 + `/Applications`(外置盘/网络盘 v2 候选)
- `case:` 只作用于文件名侧(路径匹配恒不区分大小写)
- 无正则与 `size:`/`dm:` 过滤器(v2 候选)
- 图形界面(悬浮条/完整窗口)属 v2;v1 是引擎 + 命令行

## 环境要求

macOS(Apple Silicon)、Rust stable。数据目录 `~/Library/Application Support/macEverything/`。
「完全磁盘访问权限」可选授予(不授予时缺口显式列出,功能不受阻)。
