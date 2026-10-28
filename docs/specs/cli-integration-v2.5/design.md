# CLI 集成 v2.5 设计：JSON 输出 + 独立数据目录 + GUI 安装命令行工具

- 日期：2026-10-18（初稿）/ 2026-10-28（定稿）
- 状态：已评审，待实施
- 关联：`docs/usage.md` 命令行章节、`docs/specs/v2-ui/product.md` 设置页
- 目标使用者：外部工具 等外部集成方、脚本与自动化、AI/agent 工具调用
- 产品定位：CLI 是配套能力，不作为面向普通用户的主卖点；官网与 README 只说明「支持命令行」，不占主视觉

## 1. 背景与目标

CLI 已经独立于 GUI（`cargo install --path crates/cli` 即可用；App 与 CLI 互不依赖，
共用同一 daemon 与索引）。定位上，CLI 面向集成与自动化，不是普通用户的主要入口；
这部分能力只需要「可发现、可脚本化」，不参与产品主卖点。官网保留一处支持说明，
供需要 AI/agent 工具调用的场景查阅。

但作为**外部集成接口**还缺三样：

1. 机器可读输出：`query/status` 目前只有人类可读文本，程序解析脆弱。
2. 数据目录隔离：数据目录写死为 `~/Library/Application Support/macEverything`，
   外部产品无法跑自己的索引/生命周期。
3. 可发现性：CLI 随 App 打包在 `Contents/MacOS/macEverything`，但需要命令行的用户
   不知道，也没有一键暴露到 PATH 的入口。

目标：

- `query` / `status` 支持 `--json`，带 `schema_version`，stdout 纯 JSON。
- 支持 `--data-dir` / `MCE_DATA_DIR`，外部集成可共享或隔离索引。
- 设置页新增「命令行工具」一行：安装/卸载到 PATH（符号链接）。
- 文档给出 外部工具 的最小接入示例。
- 官网与 README 提供简短的 CLI/JSON 支持说明与入口，不作为功能亮点。

## 2. 范围

**包含**

- CLI：`--json`、`--sort`/`--desc`（绝对降序）、`--data-dir`、JSON 错误。
- CLI/数据目录配套：`desc_echo` 可选回显、`client::status` 带码错误、目录/文件权限、
  App 拉起引擎时固定数据目录。
- 设置页：命令行工具完整状态机（安装/卸载/修复/拒绝/未知）。
- 文档、i18n、单测与实机验收。
- 官网中英文各一句 CLI/JSON 支持说明（不新增主视觉区块）。

**不包含**

- HTTP API、MCP、本地 socket 之外的服务化接口。
- 自动修改 `.zshrc`/`.bash_profile`、Homebrew formula。
- 系统授权弹窗自动安装到 `/usr/local/bin`（v1 只提供可复制的 `sudo` 命令）。
- 不做协议版本 bump（保持 v1）；仅新增可选 `desc_echo` 回显字段，旧 daemon 缺省 `null`。
- CLI 的对外营销与主视觉宣传（只在文档与官网支持说明中出现）。
- Windows/Linux 移植。

### 2.1 实施切片

- **A. CLI 与数据目录**：参数语法、`--json`、`--sort`/`--desc`、`--data-dir`/
  `MCE_DATA_DIR`、`desc_echo`、`client::status` 带码错误、0700/0600 权限、
  App 拉起引擎固定目录。
- **B. 设置页安装器**：闭合状态机、动作矩阵、原子替换、shell 探测、代际刷新。
- **C. 文档与验收**：usage.md、README/官网支持说明、i18n、布局测试、实机清单。

同版发布 v2.5.0；A 与 B 可独立提交、独立验证。

## 3. CLI JSON 契约

### 3.1 用法与参数

```bash
macEverything [--data-dir <PATH>] <command> [args...]
macEverything query [--json] [--sort <name|size|mtime|path>] [--desc] [--] <关键词>
macEverything status [--json]
```

- `--data-dir` 只出现在子命令前，也接受 `--data-dir=PATH`；`MCE_DATA_DIR` 为等价环境变量。
- `--json` 只对 `query`/`status` 有效；`--sort`/`--desc` 只对 `query` 有效；
  其他组合按用法错误处理。
- `--sort` 必须是 `name|size|mtime|path`；`--desc` 必须与 `--sort` 同时出现；
  重复的 `--data-dir`/`--sort`、未知选项都按用法错误处理。
- `query` 的关键词是第一个非选项参数，其余单词以空格拼接；关键词以 `-` 开头时用
  `--` 分隔。缺失或空关键词按用法错误处理。
- `--desc` 是**绝对降序**：`--sort mtime --desc` = 最新在前，`--sort mtime` =
  最旧在前。CLI 内部换算成协议的“反转自然方向”位（NAME/SIZE/PATH 自然升序，
  MTIME 自然降序）。
- 人类可读输出保持现状；`--json` 时 stdout 只输出一个 JSON 对象，提示/日志走 stderr。
- 用法错误统一写 stderr 并以退出码 2 结束，即使带 `--json` 也不输出 JSON（参数尚未通过校验）。

### 3.2 `query --json`

```json
{
  "schema_version": 1,
  "total": 3,
  "elapsed_ms": 2.1,
  "stat_ms": 0.4,
  "sort_echo": "relevance",
  "desc_echo": null,
  "offline_built_at": null,
  "entries": [
    {
      "name": "季度报告.md",
      "path": "/Users/you/Documents/季度报告.md",
      "size": 24576,
      "mtime": 1760000000,
      "is_dir": false
    }
  ]
}
```

- 字段口径与 `protocol::HitEntry` 一致：`size` 字节（读不到为 `null`）、`mtime`
  Unix 秒（读不到为 `null`）、`is_dir` 目录标记。
- `offline_built_at`：在线为 `null`；离线直读落盘索引时为索引生成时刻（Unix 秒），
  不再输出人类文本提示。
- `sort_echo`：实际生效排序键（`relevance`/`name`/`size`/`mtime`/`path`）；
  空串 = 旧 daemon 缺省（实际按 relevance）。与请求不一致时调用方可自行提示。
- `desc_echo`：`true` = 降序，`false` = 升序，`null` = relevance 或旧 daemon 未回显
  （方向无法验证）。新 daemon 对列排序回显绝对方向；CLI 的 `--desc` 在调用前换算成
  协议位，JSON 仍按绝对方向输出。
- `entries` 数量 ≤ `total`，最多 1000（既有上限）。
- `--sort`/`--desc` 通过 `client::query` 传给 daemon；协议只新增可选 `desc_echo` 回显字段。

### 3.3 `status --json`

```json
{
  "schema_version": 1,
  "state": "ready",
  "entry_count": 1233836,
  "gaps": [{ "path": "/Users/you/.Trash", "reason": "Operation not permitted (os error 1)" }],
  "io_error_count": 0,
  "built_at": 1760000000,
  "index_file": "/Users/you/Library/Application Support/macEverything/index.bin",
  "offline": false
}
```

- 在线四态照旧：`building` / `ready` / `reconciling` / `degraded`。
- 离线直读：`state = "offline"`，`offline = true`；`gaps = []`、`io_error_count = 0`
  （缺口只在 daemon 内存中，离线不可得）。
- 无落盘索引：仅当 `index.bin` 不存在时为 `state = "no_index"`，`entry_count = 0`，
  `offline = true`；其余字段固定为 `gaps = []`、`io_error_count = 0`、`built_at = 0`，
  `index_file` 仍输出该数据目录下的预期路径。字段集在任何状态下保持一致。
- `index.bin` 存在但读取/校验失败（损坏、截断、EACCES）：不报 `no_index`；
  `status --json` 输出错误对象 `code = "index_unreadable"`，退出码 1。人类模式打印
  同样的错误并保留 `index_file` 路径。
- 在线 daemon 返回错误时保留其 `code`（`client::status` 改为带码错误类型），
  不丢弃成 `code: null`。
- 不再输出「需 daemon 在线查看缺口」等人类提示；需要缺口就保持在线。

### 3.4 错误与退出码

`--json` 下失败也输出 JSON（stdout），退出码非 0：

```json
{
  "schema_version": 1,
  "error": { "code": "warming", "message": "索引尚未就绪" }
}
```

- `code`：daemon 返回的错误保留原码（如 `warming`、`version`）；连接/离线路径失败为
  `null`；索引存在但不可读为 `index_unreadable`。
- 退出码：0 成功；1 运行错误；2 用法错误（v2.5 起所有用法错误统一为 2）。
- 人类模式错误文案不变；`--json` 失败只写一个 JSON 对象到 stdout。

## 4. 数据目录覆盖

### 4.1 优先级

1. `--data-dir <PATH>`（显式）
2. `MCE_DATA_DIR`（环境变量）
3. 默认 `$HOME/Library/Application Support/macEverything`

两个入口都要求非空绝对路径，不以 cwd 拼接（`start` 会 spawn 自身，cwd 语义不可靠）：

- `--data-dir` 缺失值、空值、相对路径 → 用法错误，退出码 2。
- `MCE_DATA_DIR` 为空、仅空白或相对路径 → 运行错误，退出码 1（不静默回落默认目录）。

所有子命令（`start` / `stop` / `query` / `status` / `exclude`）都使用同一个解析结果；
`query`/`status` 不创建数据目录，只有 `start` 创建。`HOME` 未设置且没有显式目录 →
运行错误；有 `--data-dir`/`MCE_DATA_DIR` 时不受 `HOME` 影响。

`DaemonPaths { data_dir }` 派生的 `daemon.sock` / `index.bin` / `config.toml` /
`daemon.log` 全部跟随该目录；单实例按目录隔离（不同目录 = 不同 daemon/索引）。

### 4.2 `start` 的后台子进程

`start` 会 spawn 自身（`MCE_FOREGROUND=1`）。父进程解析出最终数据目录后：

1. 先创建目录（`0700`）并检查可写；失败时在父进程直接报出路径与 errno（子进程的
   stdout/stderr 接 `/dev/null`，父进程不预检就只剩一条泛化错误）。
2. 显式给子进程设置 `MCE_DATA_DIR=<解析结果>`，保证 `--data-dir` 与 `MCE_DATA_DIR`
   两种入口行为一致，也保证 `--data-dir` 不会在 spawn 后丢失。

### 4.3 隔离与安全

- 目录由 `start` 创建：`DirBuilder` 0700；已存在目录尝试 chmod 0700，失败记 warn。
- `index.bin` / `config.toml` / `daemon.log` 创建为 0600；已存在文件也尝试 chmod 0600；
  socket 仍为 0600。
- 自定义目录不要放在共享/世界可写位置；索引含全盘文件路径，属敏感数据。
- App 仍固定使用默认目录；**自定义目录的实例对 App 不可见**，这是预期行为。
  实现上由 `engine_launcher` 显式固定环境，见 §4.5。
- 扫描范围仍可用 `MCE_ROOTS`（冒号分隔）与 exclude 配置收窄。

### 4.4 外部工具 接入示例

共享默认索引（省一次全盘扫描）：

```bash
macEverything start
macEverything query --json --sort mtime --desc "发票 2026" | jq '.entries[].path'
```

独立索引（外部工具 自己管理生命周期）：

```bash
export MCE_DATA_DIR="$HOME/Library/Application Support/macEverything"
macEverything start
macEverything query --json "合同" | jq '.total'
```

- 独立实例会再建一份索引（磁盘/内存成本）；共享实例不要调用 `stop`，
  否则会停掉 App 的引擎。
- 版本：协议 `v1`，JSON `schema_version=1`；接入方应锁定版本并在升级时回归。

### 4.5 App 拉起引擎

`engine_launcher::ensure_engine` 目前直接继承 App 环境。若 App 从带
`MCE_DATA_DIR` 的终端或 `launchctl setenv` 环境启动，子进程 daemon 会用自定义目录，
App 仍连默认 socket，启动必然超时。改为 spawn 时显式
`.env("MCE_DATA_DIR", paths.data_dir)`：新 CLI 使用该值，旧 CLI 忽略后仍走默认目录，
两者都指向 App 的目录。测试：给 App 进程设置一个不同的 `MCE_DATA_DIR`，拉起的
daemon 仍绑定 App 的 socket。

## 5. GUI「命令行工具」

设置页 → 通用 → 新增一行「命令行工具」。窗口保持 620×600；新增行允许把关于组
推到首屏外，整页滚动。删除「关于组之前的内容须在首屏内」的布局断言，改为
「分组顺序不变 + 可滚动」。

### 5.0 一次判定产出三组观测

状态与动作共用同一判定；动作在点击时重新执行完整判定。

**A. PATH 来源（provenance）**

- `LoginInteractive`：`$SHELL -lic 'command env'` 成功，解析到完整 `PATH=` 行。
- `LoginOnly`：`-lic` 失败/超时，`$SHELL -lc 'command env'` 成功。
- `ProcessEnv`：前两者都失败，回落进程 PATH。
- `Failed`：连进程 PATH 都不可用。

只有 `LoginInteractive` 可以得出 `Absent`、选择安装目录或宣称安装成功。
`LoginOnly`/`ProcessEnv` 显示「无法读取登录 shell PATH，状态未知」+ 重试；保留上一次
成功判定并标注「可能过期」，不降级为 `Absent`。

探测在后台线程执行，单飞 + 缓存；`-lic` 总超时 3s，进程组整体终止并回收；读取线程
上限 64 KiB；解析 bytes，取最后一个完整 `PATH=` 行；不记录原始 env。

**B. PATH 扫描**

每个 PATH 段先做绝对路径/去尾斜杠/跳空段/去重，再按顺序 `lstat(dir/macEverything)`：

1. `ENOENT`/`ENOTDIR`：该段无条目。
2. 其他错误：记 `Unreadable(path, errno)`，该段未知。
3. 符号链接：`read_link`；相对目标按链接父目录解析并做词法 `..` 归一；`stat(entry)`
   跟随：
   - `ENOENT`/`ENOTDIR` → 断链；目标文本按路径组件匹配
     `*.app/Contents/MacOS/macEverything` → `BrokenApp`，否则 `BrokenForeign`。
   - `EACCES`/`ELOOP`/`EIO` → `Unreadable`，不提供任何动作。
   - 成功：与随包 CLI 比较 `dev`+`ino` → 同 → `Ours`；否则目标组件匹配 app 形态 →
     `OtherApp`（记录执行位）；否则 `Foreign`。
4. 普通文件：即使与随包 CLI `dev`+`ino` 相同也不是 `Ours`（硬链接不是我们装的），
   记 `Foreign`，记录执行位。
5. 目录/FIFO/socket：`Foreign(NotAFile)`，永不触碰。

`effective` = 第一个「跟随目标是普通文件且 `access(X_OK)` 通过」的条目，即终端实际
会执行的那个；`hits` 保留全部同名项。显示用 `effective`，没有则用第一个 `hits`；
动作在 `hits` 中按类型定位。

**C. 运行环境**

- `bundle`：`current_exe` 的父目录必须形如 `<...>.app/Contents/MacOS`，并 canonicalize；
  否则 `Unavailable(DevBuild)`。每次判定重新 stat 随包 CLI，不缓存 inode（App 可在
  运行中被替换）。
- 随包 CLI = `<bundle>/Contents/MacOS/macEverything`：必须是普通文件、非空、
  `access(X_OK)` 通过；否则 `Unavailable(BundledCliMissing|NotExecutable)`。
- 位置：路径组件含 `AppTranslocation` → `Unavailable(Translocated)`；`statfs(bundle)`
  为 `MNT_RDONLY` → `Unavailable(ReadOnlyVolume)`。提示「移到 Applications 并重启」。
- `Unavailable`、`Unknown` 或 `path_source != LoginInteractive` 时禁用全部动作，
  显示原因 + 重试；分类可作为只读信息展示。

### 5.1 分类与展示

| 分类 | 条件 | 展示 |
|---|---|---|
| `Ours` | 符号链接，目标 `dev`+`ino` == 随包 CLI | 路径 + 卸载（被遮蔽时标注） |
| `OtherApp` | 符号链接，目标存在，组件匹配 app 形态，非同一文件 | 路径 + 「指向另一个 macEverything，不覆盖」 |
| `Foreign` | 普通/目录/FIFO/socket，或链接到非 app 目标 | 路径/类型 + 「其他来源」 |
| `BrokenApp` | 断链，目标文本组件匹配 app 形态；记录卷是否挂载 | 旧目标 + 修复（卷未挂载则禁用） |
| `BrokenForeign` | 断链，不匹配 app 形态 | 路径 + 「其他来源的失效链接」 |
| `Unreadable` | `lstat`/`read_link`/`stat` 非 ENOENT 错误 | 路径 + errno，禁用动作 |
| `Absent` | 无有效可执行命中且无 `Ours`/`OtherApp`/`BrokenApp` | 安装 |
| `Unavailable` | DevBuild / Translocated / ReadOnlyVolume / BundledCliMissing / NotExecutable | 原因 + 移动/重试提示 |
| `Unknown` | PATH 来源非交互登录 shell，或有 `Unreadable` 段 | 原因 + 重试，禁用动作 |

- `Absent` 且 PATH 前部有 `BrokenForeign`/非可执行 `Foreign`：显示「未安装」并提示
  这些条目会被终端跳过；安装仍进允许目录。
- 有 `Ours` 但被更早的 `Foreign`/`BrokenForeign` 遮蔽：主状态显示遮蔽项，另加一行
  「另有指向本 App 的链接（被遮蔽）」+ 卸载。
- 有 `OtherApp`/`Ours` 时安装一律拒绝，不制造第二个副本。

### 5.2 动作矩阵

| 观测 | 安装 | 修复 | 卸载 |
|---|---|---|---|
| `Absent` + 前置全部通过 | 允许 | — | no-op 成功 |
| `Ours` | no-op 成功 | — | 允许（第一个 `Ours`） |
| `BrokenApp`（卷已挂载、父目录可写） | 拒绝 | 允许（就地原子替换） | 拒绝 |
| `BrokenApp`（卷未挂载） | 拒绝 | 禁用，提示挂载/稍后 | 拒绝 |
| `OtherApp` / `Foreign` / `BrokenForeign` | 拒绝 | 拒绝 | 拒绝 |
| `Unreadable` / `Unknown` / `Unavailable` | 拒绝 | 拒绝 | 拒绝 |

- **安装目标**：PATH 顺序中第一个「在允许列表内、存在、可写」的目录；允许列表为
  `/opt/homebrew/bin`、`~/.local/bin`、`~/.cargo/bin`、`/usr/local/bin`。目录身份按
  `dev`+`ino` 比较，不按字符串前缀。可写性用 `access(W_OK)` 预判，创建失败仍按
  errno 处理。没有可用目录时，从 PATH 中选第一个允许目录生成带守卫的可复制命令
  （`[ -e ] || [ -L ]` 拒绝；不带无条件 `-f`），必要时加 `sudo`；不硬编码
  `/usr/local/bin`。若 PATH 中没有允许目录，提示先把允许目录加入 PATH（不修改 shell
  配置）并给示例。
- **安装**：`symlink` 创建；`EEXIST` 立即重分类，`Ours` → 成功，否则报「已被占用，
  不覆盖」。创建后复核 `Ours` 才算成功。
- **修复**：只替换第一个 `BrokenApp` 的路径（不重选目录）。复核目标仍是不存在的链接
  且文本匹配；父目录不可写时保留旧链接并给带守卫的 `sudo` 命令；可写时：
  ① `rename(link_path, old_tmp)` 把旧链接移开（不覆盖任何新项）；② `lstat` +
  `read_link` 复核 `old_tmp`，不匹配则移回并中止；③ `symlink(cli, link_path)` 创建
  新链接，`EEXIST` 或失败则移回 `old_tmp`；④ 成功后删除 `old_tmp`。卷未挂载时禁用。
- **卸载**：只删第一个 `Ours`；父目录不可写时给 `sudo rm <path>`，不尝试；可写时先
  `rename(path, tmp)`，再 `lstat` + `read_link` 复核 `dev`+`ino`，通过后删 tmp，失败
  恢复；`ENOENT` 算成功。卸载后刷新并报告剩余的同名项（如有）。
- 所有动作使用点击时的重新判定；动作结果带 generation，完成后以动作自己的复核结果
  为准，再刷新；刷新失败保留动作结果。

### 5.3 异步、日志与错误

- UI 状态：`Idle` / `Resolving` / `Acting` / `Ready(status)` / `Error(message, retry)`。
- 单飞：同一时刻一个探测或动作；动作使在飞探测失效；generation 丢弃旧结果；动作
  按钮在 busy 时全部禁用。
- 每个 fs/探测操作记录操作、绝对路径、errno、PATH 来源；状态转移记 info；不记录
  原始 env。
- 用户文案区分 `EACCES`（给手动命令）、`EROFS`（只读卷）、`ENOSPC`（磁盘满）、
  `EEXIST`（被占用）、`ENOENT`（已不存在）、`ELOOP`（链接循环）。

### 5.4 提示

- 「首次在终端运行可能需要完全磁盘访问权限；可在系统设置中授予终端或 CLI。」
  文档入口用浏览器打开仓库 `docs/usage.md` 的网页地址，不在 App 内渲染 markdown。
- 安装后可用：`macEverything start` / `query` / `status`；与 App 共用默认索引。
- App 运行在 DMG/translocation 时提示「先拖入 Applications 并重启 App」。
- App 在可移动卷上时提示「卷卸载后链接会失效」。

### 5.5 已知限制（v2.5 不覆盖）

- shell alias/function/hash 表不识别；状态以 PATH 中可执行文件为准。
- 只管理第一个 `Ours`；PATH 后面的同名链接不自动清理。
- 多用户共享目录（如 `/opt/homebrew/bin`）的链接对其他用户可能不可用。
- noexec 挂载只记录执行位，不做运行验证。
- 旧 daemon 不支持 `desc_echo` 时方向无法验证。
- 手册 `sudo` 安装的链接，App 卸载只给手动命令。
- App 在可移动卷上时，卷拔出后链接变为 `BrokenApp`。
- 自己的 CLI 副本（普通文件，非符号链接）按 `Foreign` 处理，不由安装器管理。

### 5.6 i18n 与测试

- 新文案全部入 `i18n` 目录（行标题、九种分类/禁用的展示、按钮、错误、FDA 与移动提示）。
- 纯逻辑单测：PATH 解析（`-lic`/`-lc`/进程/失败 + 超时终止）、首个有效命中、
  九种分类、PATH 顺序 + 可写过滤、安装/卸载幂等（inode 不变）、原子修复、拒绝覆盖、
  `EEXIST` 重分类、`ENOENT` 卸载成功、EACCES/ELOOP 不删、相对链接、多跳链接、
  目录/FIFO、translocation/只读卷、随包 CLI 缺失、旧 daemon `desc_echo=null`。
- 实机：Homebrew/user 目录安装、重复点击、无写权限的手动命令、已有 cargo 安装不被
  覆盖、App 移动后的修复、DMG 中拒绝安装、卷未挂载时禁用修复、双语界面。

## 6. 代码改造点

- `crates/cli/src/main.rs`
  - 参数语法：`--data-dir` 前置、`--json`/`--sort`/`--desc` 校验、`--` 分隔、
    缺失/重复/未知参数 → 退出码 2。
  - `--desc` 换算成协议的“反转自然方向”位；`--desc` 无 `--sort` 报用法错误。
  - `data_dir()` 带优先级；`MCE_DATA_DIR` 空/相对 → 运行错误；`start` 子进程
    显式设置 `MCE_DATA_DIR`，父进程预检目录 0700 + 可写。
  - JSON 输出：`serde_json::json!` 构造；`QueryOutcome`/`StatusReport` 直接映射；
    `desc_echo`、`sort_echo` 空串、错误码按 §3 输出。
  - `status --json` 使用 `client` 的新带码错误类型；`index_unreadable` 走运行错误。
  - 更新无参用法文案。
- `crates/cli/src/daemon.rs`
  - 数据目录创建 0700（已存在则 chmod 0700，失败 warn）；`config.toml`/`daemon.log`
    创建 0600。
- `crates/engine/src/persist.rs`
  - `index.bin` 创建 0600（`OpenOptionsExt::mode` 或创建后 chmod）。
- `crates/protocol/src/lib.rs`
  - `Response::Hits` 新增可选 `desc_echo: Option<bool>`，旧 daemon 缺省。
- `crates/client/src/lib.rs`
  - `QueryOutcome` 新增 `desc_echo`。
  - `status` / `offline_status` 改为带码错误类型（保留 daemon code；索引存在但
    不可读 → `index_unreadable`，不映射为 `NoIndex`）。错误类型实现 `Display`/`Error`，
    App 侧现有 `?` 与 match 继续可用。
- `crates/app/src/engine_launcher.rs`
  - spawn 引擎时显式 `.env("MCE_DATA_DIR", paths.data_dir)`，覆盖外部同名变量；
    其余环境照常继承。
- `crates/app/src/cli_install.rs`（新增）
  - 登录 shell PATH 探测（`-lic` → `-lc` → 进程，进程组终止、64 KiB 上限、单飞）；
    PATH 扫描、九种分类、`effective`/`hits`、动作矩阵、原子替换/卸载、带守卫的
    手动命令。
  - 随包 CLI 从 `.app/Contents/MacOS/` 结构推导并校验，**不复用**
    `engine_launcher::engine_candidates()` 作为身份来源；开发态/translocation/只读卷禁用。
- `crates/app/src/ui/settings_view.rs`
  - `draw_general_group` 新增「命令行工具」行；删除首屏断言，保留分组顺序与滚动测试。
- `crates/app/src/ui/settings.rs`
  - 安装器 `Idle/Resolving/Acting/Ready/Error` 状态机、generation、single-flight。
- `crates/app/src/i18n/catalog.rs`
  - 新增 CLI 安装相关文案（九种分类、禁用原因、手动命令、FDA/移动提示）。
- 文档：`docs/usage.md`（JSON、数据目录、安装行、外部工具 示例、FDA 提示）、
  `docs/specs/floatbar-visual/settings-visual.md`（删除首屏约束说明）。
- 不引入新依赖：CLI/App 已有 `serde_json`、`libc`；安装器只用 std + libc。

## 7. 错误处理

| 场景 | 行为 |
|---|---|
| `--json` 下查询/状态失败 | stdout 单个 JSON 错误对象 + 退出码 1 |
| `--json` 下用法错误 | stderr 文本 + 退出码 2，stdout 为空 |
| `--data-dir` 缺失/空/相对 | 用法错误，退出码 2 |
| `MCE_DATA_DIR` 空/相对 | 运行错误，退出码 1，不回落默认目录 |
| 自定义目录不存在/不可写 | `start` 父进程预检失败，报路径 + errno；`query`/`status` 不创建目录 |
| 自定义目录已有 daemon | 单实例提示已在运行；按目录隔离，不误连默认实例 |
| `start` 子进程丢失数据目录 | 父进程显式设置 `MCE_DATA_DIR` |
| 登录 shell PATH 解析失败/超时 | 依次回落 `-lc`、进程 PATH；非 `LoginInteractive` → `Unknown`，禁用动作 |
| PATH 某段不可读 | 该段 `Unreadable`；整体 `Unknown`，禁用动作，记 errno |
| 无可用 PATH 目录 | 从 PATH 中选允许目录生成带守卫的 `sudo` 命令，不自动提权 |
| 首个命中是 `OtherApp`/`Foreign` | 显示来源，安装拒绝，不覆盖 |
| 存在 `Ours`（含被遮蔽） | 显示并允许卸载第一个 `Ours` |
| `BrokenApp` 卷已挂载 | 就地原子修复；父目录不可写给守卫 `sudo` 命令 |
| `BrokenApp` 卷未挂载 | 禁用修复，提示挂载或稍后 |
| `EACCES`/`ELOOP`/`EIO` | `Unreadable`，禁用所有动作，记录 errno |
| 判定后目标被占用（`EEXIST`） | 重分类：`Ours` → 成功，否则报占用不覆盖 |
| 卸载时 `ENOENT` | 成功（幂等） |
| 卸载时父目录不可写 | 给 `sudo rm` 命令，不尝试 |
| 重复安装/卸载 | no-op 成功，inode 不变 |
| App 在 DMG/translocation/只读卷 | `Unavailable`，禁用动作，提示移到 Applications 并重启 |
| 随包 CLI 缺失/不可执行 | `Unavailable`，禁用安装 |
| 开发态 | `Unavailable(DevBuild)`，隐藏安装 |
| 索引存在但不可读 | `index_unreadable`，退出码 1，不报 `no_index` |
| daemon 返回错误 | 保留原 `code`；`status` 不再丢弃 |

## 8. 测试与验收

**进程级（`crates/cli/tests/`，`CARGO_BIN_EXE_macEverything`）**

- stdout 纯净：`query/status --json` 只输出一个 JSON 值；离线提示、缺口提示不得混入。
- 退出码：成功 0、运行错误 1、用法错误 2；用法错误时 stdout 为空。
- 参数语法：`--data-dir` 前置/`=` 形式、`--` 分隔、缺失/重复/未知参数、空关键词、
  `--desc` 无 `--sort`、`status --sort`。
- 精确 JSON：各状态 key 集、`schema_version == 1`、`desc_echo` 三态、`sort_echo`
  空串、`offline_built_at` null/整数、`no_index` 固定字段、`index_unreadable` 错误码。
- 数据目录：flag > env > default、空/相对拒绝、`HOME` 未设、`start` 子进程环境、
  `stop`/`exclude` 路由到目标 socket、`index_file` 跟随目录。
- 权限：自定义目录 0700、`index.bin`/`config.toml`/`daemon.log` 0600。
- 人类输出回归：离线 banner、status 缺口提示、start 文案不变；无参用法含新选项。

**安装器纯逻辑（注入 fs / shell / statfs）**

- PATH 探测：`-lic` 成功短路、超时终止进程组并回落 `-lc`、再回落进程；parser
  处理最后完整 `PATH=`、CRLF、噪声、空段、非 UTF-8、截断；不记录原始 env。
- 分类：九种状态 + `effective`/`hits`；`ENOENT` vs `EACCES`/`ELOOP`；相对/多跳链接；
  目录/FIFO/socket；大小写变体；硬链接不算 `Ours`；卷未挂载 `BrokenApp`。
- 动作矩阵：每种状态允许/拒绝的动作；`Foreign` 在前、`Ours` 在后仍可卸载；
  `Ours` 在前、`Foreign` 在后安装拒绝；`BrokenApp` 就地修复。
- 幂等：重复安装/卸载 inode 不变；`EEXIST` 重分类；卸载 `ENOENT` 成功。
- 安全：EACCES 不进删除路径；修复创建失败恢复旧链接；卸载交换后不删非目标；
  手动命令拒绝已存在的文件/链接、路径含空格/引号时正确引用、不带无条件 `-f`。
- 前置：dev/translocation/只读卷/随包 CLI 缺失；translocation 按路径组件匹配。
- UI：generation 丢弃旧刷新、single-flight、动作后刷新失败保留动作结果；
  布局测试删除首屏断言、保留分组顺序；九种状态的中英文渲染。

**实机**

- `query --json` 在线/离线；`jq` 直接解析；stdout 无杂文本。
- `status --json` 四态 + offline + no_index + 损坏索引。
- `--sort mtime` = 最旧在前，`--sort mtime --desc` = 最新在前；与完整窗口排序对照。
- `MCE_DATA_DIR` 独立实例：两份索引、两个 socket、互不影响；`stop` 只停目标实例。
- App 在环境里带 `MCE_DATA_DIR` 启动：仍连接默认目录并成功拉起引擎。
- 设置页安装/卸载/修复；重复点击；Homebrew、`~/.local/bin`、`/usr/local/bin`；
  已有 cargo 安装不被覆盖；App 移动后的修复；DMG 中拒绝安装；卷未挂载时禁用修复；
  双语界面。
- 外部工具 冒烟：按 §4.4 独立目录 start → query JSON → stop。

## 9. 风险与缓解

| 风险 | 缓解 |
|---|---|
| App 被移动/删除导致链接失效 | `BrokenApp` 检测 + 就地修复；不自动删除 |
| `BrokenApp` 目标在未挂载卷上 | 检测 `/Volumes/<name>` 挂载状态，禁用修复，提示挂载/稍后 |
| GUI 进程 PATH 与用户 shell 不一致 | 带 provenance 的 `-lic`/`-lc`/进程回落；只有 `LoginInteractive` 能判定 `Absent` |
| PATH 段不可读 | `Unreadable` → `Unknown`，禁用动作，不当作 `Absent` |
| PATH 目录差异/无写权限 | 按 PATH 顺序选允许目录；否则给带守卫的手动命令 |
| App 从 DMG/translocation/只读卷运行 | 检测路径组件与 `statfs(MNT_RDONLY)`，禁用动作，提示移动并重启 |
| 判定与动作间的竞态 | 动作时重新判定；`symlink` 不覆盖；修复/卸载用重命名到临时名再校验 |
| 修复创建失败丢旧链接 | 先建临时链接再 `rename` 替换；失败恢复旧链接并报错 |
| 误删用户自建断链 | 只删除 `Ours`；`BrokenApp` 只修复不删除 |
| 与 cargo/Homebrew 安装冲突 | 不覆盖、不卸载非本 App 链接 |
| App 拉起的引擎继承 `MCE_DATA_DIR` | `engine_launcher` 显式设置 App 的目录 |
| shell 探测卡住/泄漏进程 | 进程组终止、reader 上限、单飞、缓存、总超时 |
| 旧 daemon 忽略 `--desc` | `desc_echo = null`，文档说明方向无法验证 |
| TCC/FDA 归属 | 首次运行提示；文档说明授予终端或 CLI |
| 自定义目录泄露索引 | 目录 0700、文件 0600；文档警告不要放共享位置 |
| 设置页新增行破坏首屏 | 放弃首屏约束，保留分组顺序 + 滚动测试 |
| JSON schema 演进 | `schema_version`；字段只增不删，变更需升版本 |
| 共享 daemon 被 外部工具 stop | 文档明确：共享实例不要 stop；独立实例用独立目录 |
| 协议/CLI 版本混跑 | 协议 v1 拒绝异版本；`desc_echo` 缺省 null；外部工具 锁定版本 |

## 10. 文档同步

- `docs/usage.md`：命令行章节增加 `--json`、`--sort/--desc`（绝对降序）、参数语法、
  `MCE_DATA_DIR`、退出码、`desc_echo`、`index_unreadable`、外部工具 示例；设置页增加
  「命令行工具」说明；FDA 提示；数据目录 0700/0600 说明。
- `README.md` / `README.zh-CN.md`：把「CLI + GUI」从功能亮点列表移除，保留
  安装/使用章节的 CLI 支持说明，补一句 `--json` 可供脚本与 AI/agent 工具调用。
- `site/index.html` / `site/en/index.html`：在功能说明或 FAQ 中加一句 CLI/JSON
  支持说明（中英各一处），不新增主视觉区块。
- `docs/specs/v2-ui/product.md`：设置页行/分组更新。
- `docs/specs/floatbar-visual/settings-visual.md`：删除「关于组之前内容须在首屏内」，
  改为固定 620×600 + 整页滚动；同步 CLI 行说明。
- `docs/architecture.md` / `docs/development.md`：补数据目录覆盖、`MCE_DATA_DIR`
  固定、CLI JSON 契约入口。
- `docs/specs/i18n-v2.1/design.md` / `plan.md`：新增 CLI 安装文案（若 i18n Step 4 已落地，
  随目录增补）。
- 新 spec 链接：本文件。

## 11. 非目标

- MCP / HTTP / 远程 API。
- 自动提权的安装、修改 shell 配置、Homebrew formula。
- 协议版本 bump；新增字段仅限可选 `desc_echo`。`exclude` 的 JSON 输出后续按需。
- Windows/Linux 移植。

## 12. 决策记录

1. **JSON 由 CLI 输出层实现**：client 返回结构化数据；协议仅新增可选 `desc_echo`，
   外部集成可用文本 CLI，也可直接走 Unix socket。
2. **`--data-dir` + `MCE_DATA_DIR` 双入口**：flag 便于脚本，env 便于子进程/容器；
   `start` 子进程显式设置解析结果。
3. **符号链接而非复制**：跟随 App 更新；App 移动的失效作为可检测状态处理。
4. **不自动改 PATH/不提权**：v1 只动可写 PATH 目录；否则给可复制命令，安全边界清晰。
5. **共享 vs 隔离显式二选一**：文档写清成本（第二份索引）与生命周期（勿 stop 共享实例）。
6. **JSON `schema_version` 独立于协议版本**：CLI 输出可演进，不强迫 daemon 升级。
7. **CLI 定位为配套能力**：满足集成、脚本与 AI/agent 工具调用；官网与 README 只作支持说明，
   不作为功能亮点或主视觉。
8. **保留设置页安装器**：CLI 不宣传，但已安装 App 的用户需要一键暴露到 PATH；
   安装器只做符号链接，不接管 cargo/Homebrew 等其他来源。
9. **安装器以交互登录 shell PATH 的 `effective` 命中为准**：GUI 进程 PATH 不可靠；
   状态与动作共用同一判定，动作时重新判定，不信任页面缓存。
10. **不覆盖策略由 symlink 语义兜底**：创建不覆盖（`EEXIST` 即失败），替换仅限
    `BrokenApp`；修复用 `rename` 替换链接项本身，不使用会跟随/覆盖目标的覆盖式写入。
11. **`--desc` 是绝对降序**：CLI 内部换算成协议的“反转自然方向”位；`--desc` 必须
    与 `--sort` 同时出现。
12. **设置页保持 620×600，放弃首屏约束**：新增行允许关于组下移，整页滚动。
13. **安装器采用闭合状态机**：九种分类 + `Unknown`/`Unavailable` + 动作矩阵；
    EACCES/ELOOP 不进入删除路径。
14. **App 固定默认数据目录**：`engine_launcher` 显式设置 `MCE_DATA_DIR`，不继承
    外部环境。
15. **`desc_echo` 为可选回显**：新 daemon 返回绝对方向，旧 daemon 缺省 `null`，
    调用方可识别方向未验证。
16. **数据目录 0700、文件 0600**：索引含全盘路径，权限在 daemon 侧落实。
