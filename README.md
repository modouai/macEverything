# macEverything

macOS 上的全盘文件名即时搜索引擎——对标 Windows 的 [Everything](https://www.voidtools.com/zh-cn/support/everything/)。
自建索引、不走 Spotlight:Spotlight 搜不到的目录(隐私排除、`.noindex`、开发目录),这里全都能搜到。

## 特性

- **快**:百万文件首扫 ~1.5 秒;查询 P99 < 4 毫秒;冷启动秒开
- **全**:扫描根内所有可读目录 100% 覆盖,权限缺口显式可查;与 `find` 对拍零差异
- **准**:实时增量(文件系统变更 ≤ 500ms 可搜)
- **停机窗口事件重放**:重启后自动补齐关机期间的变更(mac 版 USN 日志,秒级)
- **索引落盘**:mmap 零拷贝加载;daemon 不在时 query/status 直读落盘索引离线自答

## 用法

```bash
macEverything start            # 启动常驻引擎(后台建索引)
macEverything stop             # 停止(最终落盘后退出)
macEverything query "关键词"    # 查询
macEverything status           # 索引状态 / 条目数 / 权限缺口
macEverything exclude add|remove|list <绝对路径>   # 排除目录管理
```

**搜索语法**(与 Everything 对齐的最小集):

| 语法 | 语义 | 示例 |
|---|---|---|
| 关键词 | 文件名包含该子串;空格分隔 = 全部命中(AND) | `季度 报告` |
| `*` | 通配任意长度字符 | `*.pdf` |
| `?` | 通配单个字符 | `报告?` |
| `path:` | 紧跟的词匹配完整路径而非仅文件名 | `path:合同 2026` |
| `case:` | 整轮区分大小写(默认不区分) | `case:README` |
| `whole:` | 整轮整词匹配(默认子串) | `whole:init` |

默认大小写不敏感覆盖整个 Unicode(é/É 互搜);中文、emoji 文件名全程支持。

数据目录:`~/Library/Application Support/macEverything/`(索引 `index.bin`、配置 `config.toml`、日志 `daemon.log`)。
默认扫描根:用户主目录 + `/Applications`;默认排除:`~/Library`、`node_modules`、`.git`。

## 构建与测试

```bash
cargo build --release                                  # 产物 target/release/macEverything
cargo test --workspace                                 # 单元 + 集成(79 项)
cargo test -p engine --test fsevents_live -- --ignored # FSEvents 实机集成(需真实事件)
bash scripts/bench.sh <树路径> <文件数>                  # 五项基准(隔离 MCE_ROOTS)
```

## 架构

```
crates/engine/   纯逻辑库:索引(扁平 Entry 数组 + 双 arena)/ 查询(解析+Finder 预编译匹配)/
                 并行扫描 / 落盘(mmap 同构布局)/ FSEvents 直调封装 / 增量 diff / 兜底重扫
crates/cli/      daemon(引擎宿主 + Unix socket 服务)+ 五子命令 client
docs/specs/      产品规格 / 技术规格(6 个 ADR)/ 实现计划
scripts/         五项基准 / 隔离 spike
```

设计要点(详见 `docs/specs/v1-core/tech.md`):

- **事件只当信号,目录级 diff 才是真相**——增量正确性不赌 FSEvents 的粗粒度语义
- **停机重放**:落盘记录 `last_event_id`,重启从系统事件日志位置重放;不可用时保守判定器(零事件 + 停机超阈值)转全量重扫兜底
- **mmap 零反序列化**:索引文件与内存布局同构,冷启动亚秒;写入走临时文件 + rename 原子替换
- **Finder 预编译**:查询主开销的消除(百万条目零重建)

## 许可

私有项目,未发布。
