# 使用指南

## 安装

```bash
cd macEverything
cargo build --release

# 方式一:软链进 PATH(推荐,改代码重编译后自动生效)
ln -sf "$PWD/target/release/macEverything" ~/.cargo/bin/macEverything

# 方式二:cargo 安装(独立副本)
cargo install --path crates/cli
```

验证:`macEverything`(不带参数)打印用法。

## 五个命令

### `macEverything start` — 启动引擎(后台)

```bash
$ macEverything start
引擎已在后台启动(pid 12345)。
查看进度:macEverything status;查询:macEverything query "关键词"
```

- 首次启动:全盘扫描(用户主目录 + `/Applications`),百万文件约 1-3 秒;期间即可查询(结果随扫描进度逐步完整,`status` 显示「构建中 N」)。
- 之后每次启动:**秒开**(读落盘索引)→ 后台重放停机窗口事件补齐 → 就绪。
- 单实例:重复 start 提示已在运行。
- 调试需要看实时日志:`MCE_FOREGROUND=1 macEverything start` 前台运行。

### `macEverything stop` — 停止(落盘后退出)

```bash
$ macEverything stop
已停止,最终条目数 1233836
```

响应返回时最终落盘已保证。落盘失败会如实报错(详情见 daemon.log)。

### `macEverything query "关键词"` — 查询

```bash
$ macEverything query "季度报告"
共 1 项 · 匹配 2.1 ms
     24 KB  季度报告.md  /Users/you/Documents/季度报告.md
```

- 输出:命中总数 · 引擎耗时;每条 = 大小(按需读取)· 文件名 · 完整路径(可直接复制使用)。
- 上限单轮 1000 条,超出提示;`| head` 等管道用法正常。
- **daemon 未运行时自动离线**:直读落盘索引并标注「离线索引 · 生成于 …」。

### `macEverything status` — 索引状态

```bash
$ macEverything status
状态: 就绪
条目数: 1233836
索引生成: 2026-10-16 00:12:00 UTC
索引文件: /Users/you/Library/Application Support/macEverything/index.bin
权限缺口(1 个目录,未收录):
  /Users/you/.Trash  (Operation not permitted (os error 1))
```

状态四态:构建中 / 就绪 / 补齐中(重放) / 重扫中(兜底)。权限缺口逐目录列出。

### `macEverything exclude add|remove|list <绝对路径>` — 排除目录

```bash
$ macEverything exclude add /Users/you/Videos
排除目录(4 条):
  ~/Library
  node_modules
  .git
  /Users/you/Videos
```

变更即时生效(引擎重扫)并写入配置文件;daemon 未运行时此命令报错(需引擎在线)。

## 搜索语法

| 语法 | 语义 | 示例 |
|---|---|---|
| 关键词 | 文件名包含该子串;空格分隔 = 全部命中(AND) | `季度 报告` |
| `*` | 通配任意长度字符(含空) | `*.pdf`、`报告*` |
| `?` | 通配单个字符 | `报告?`、`invoice_??.pdf` |
| `path:` | **仅紧跟其后的一个词**匹配完整路径;其余词仍匹配文件名 | `path:Documents 发票` |
| `case:` | 本轮区分大小写(默认不区分) | `case:README` |
| `whole:` | 本轮整词匹配(默认子串) | `whole:init` |

规则细节:

- 默认大小写不敏感**覆盖整个 Unicode**(`café` 能搜到 `CAFÉ`);NFC/NFD 两种归一形态互搜。
- 排序:文件名前缀命中 > 词边界命中 > 子串命中;同级按名更短优先。
- 语法前缀须小写;`*`/`?` 在词中就是通配符(没有转义)。
- 整词边界:`_` 与字母数字同属词字符——`whole:init` 命中 `init.rs`,不命中 `__init__.py`。
- 组合示例:`path:项目 *.md`(路径含「项目」且文件名以 .md 结尾)、`path:Documents 发票 2026`(Documents 路径下文件名同时含「发票」与「2026」)。

## 权限:完全磁盘访问

macOS 的 TCC 保护会让部分目录(如 `.Trash`、邮件数据库)不可读,`status` 会如实列出缺口。授予方式:

1. 系统设置 → 隐私与安全性 → 完全磁盘访问权限
2. 添加 `macEverything`(软链安装时添加终端更省事)
3. `macEverything stop && macEverything start`

未授权不影响其他功能;缺口永远可见(区别于 Spotlight 的静默遗漏)。

## 数据目录

`~/Library/Application Support/macEverything/`:

| 文件 | 内容 |
|---|---|
| `index.bin` | 索引(百万条目 ~63MB;与内存布局同构,mmap 零拷贝加载) |
| `config.toml` | 排除目录列表(手工编辑后重启生效;运行时改动用 `exclude` 命令) |
| `daemon.sock` | 服务套接字(0600;崩溃残留会自动清理重绑) |
| `daemon.log` | 引擎日志 |

## 排障

| 现象 | 处理 |
|---|---|
| `macEverything: command not found` | 未安装到 PATH——见「安装」 |
| `start` 报「引擎未启动(退出码)」 | 多半已在运行(`status` 确认);否则看 `daemon.log` |
| 搜不到明知存在的文件 | ①`status` 看是否「构建中/重扫中」;②确认不在排除目录;③`status` 看权限缺口 |
| 索引文件被删/损坏 | 下次 `start` 自动重建,不需手工处理 |
| 想强制全量重建 | `macEverything stop` 后删除 `index.bin` 再 `start` |
| 占用空间 | 索引约为主目录条目数 × 55 字节;`stop` 后索引文件仍保留(这是秒开的前提) |

## 环境变量

| 变量 | 用途 |
|---|---|
| `MCE_ROOTS` | 覆盖扫描根(冒号分隔绝对路径);基准/隔离测试用,生产不要设 |
| `MCE_FOREGROUND` | `start` 前台运行(调试用) |
| `HOME` | 数据目录与默认扫描根随之变化(隔离测试用) |
