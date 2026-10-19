# CLI 集成 v2.4 设计：JSON 输出 + 独立数据目录 + GUI 安装命令行工具

- 日期：2026-10-18
- 状态：设计草案（待评审）
- 关联：`docs/usage.md` 命令行章节、`docs/specs/v2-ui/product.md` 设置页
- 目标使用者：外部工具 等外部集成方，以及需要终端/脚本的开发者

## 1. 背景与目标

CLI 已经独立于 GUI（`cargo install --path crates/cli` 即可用；App 与 CLI 互不依赖，
共用同一 daemon 与索引）。但作为**外部集成接口**还缺三样：

1. 机器可读输出：`query/status` 目前只有人类可读文本，程序解析脆弱。
2. 数据目录隔离：数据目录写死为 `~/Library/Application Support/macEverything`，
   外部产品无法跑自己的索引/生命周期。
3. 可发现性：CLI 随 App 打包在 `Contents/MacOS/macEverything`，但普通用户不知道，
   也没有一键暴露到 PATH 的入口。

目标：

- `query` / `status` 支持 `--json`，带 `schema_version`，stdout 纯 JSON。
- 支持 `--data-dir` / `MCE_DATA_DIR`，外部集成可共享或隔离索引。
- 设置页新增「命令行工具」一行：安装/卸载到 PATH（符号链接）。
- 文档给出 外部工具 的最小接入示例。

## 2. 范围

**包含**

- CLI：`--json`、`--sort`/`--desc`（复用已有协议字段）、`--data-dir`、JSON 错误。
- 设置页：命令行工具状态/安装/卸载/失效修复。
- 文档、i18n、单测与实机验收。

**不包含**

- HTTP API、MCP、本地 socket 之外的服务化接口。
- 自动修改 `.zshrc`/`.bash_profile`、Homebrew formula。
- 系统授权弹窗自动安装到 `/usr/local/bin`（v1 只提供可复制的 `sudo` 命令）。
- 协议变更（JSON 是 CLI 输出层，不改 `protocol`）。
- Windows/Linux 移植。

## 3. CLI JSON 契约

### 3.1 用法

```bash
macEverything --data-dir <PATH> <command> ...
macEverything query [--json] [--sort name|size|mtime|path] [--desc] "关键词"
macEverything status [--json]
```

- `--json`、`--sort`、`--desc` 只对 `query`/`status` 有效；其他命令带 `--json` 报用法错误。
- `--sort` 非法值（非 `name|size|mtime|path`）按用法错误处理（退出码 2）。
- `--data-dir` 为全局选项（放在子命令前）；`MCE_DATA_DIR` 为等价环境变量。
- 人类可读输出保持现状；`--json` 时 stdout 只输出一个 JSON 对象，提示/日志走 stderr。

### 3.2 `query --json`

```json
{
  "schema_version": 1,
  "total": 3,
  "elapsed_ms": 2.1,
  "stat_ms": 0.4,
  "sort_echo": "relevance",
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
  与请求不一致时调用方可自行提示（旧 daemon 可能不生效）。
- `entries` 数量 ≤ `total`，最多 1000（既有上限）。
- `--sort` / `--desc` 直接传给 `client::query`，不新增协议字段。

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
- 离线直读：`state = "offline"`，`gaps = []`，`offline = true`。
- 无落盘索引：`state = "no_index"`，`entry_count = 0`，`offline = true`。
- 不再输出「需 daemon 在线查看缺口」等人类提示；需要缺口就保持在线。

### 3.4 错误与退出码

`--json` 下失败也输出 JSON（stdout），退出码非 0：

```json
{
  "schema_version": 1,
  "error": { "code": "warming", "message": "索引尚未就绪" }
}
```

- `code` 可能为 `null`（连接/离线路径无错误码）。
- 退出码：0 成功；1 运行错误；2 用法错误（与现有一致）。
- 人类模式错误文案不变。

## 4. 数据目录覆盖

### 4.1 优先级

1. `--data-dir <PATH>`（显式）
2. `MCE_DATA_DIR`（环境变量）
3. 默认 `$HOME/Library/Application Support/macEverything`

`DaemonPaths { data_dir }` 派生的 `daemon.sock` / `index.bin` / `config.toml` /
`daemon.log` 全部跟随该目录；单实例按目录隔离（不同目录 = 不同 daemon/索引）。

### 4.2 `start` 的后台子进程

`start` 会 spawn 自身（`MCE_FOREGROUND=1`）。父进程解析出最终数据目录后，**显式**
给子进程设置 `MCE_DATA_DIR`，保证 `--data-dir` 与 `MCE_DATA_DIR` 两种入口行为一致。

### 4.3 隔离与安全

- 目录不存在时由 CLI/daemon 创建，权限 `0700`；socket 仍为 `0600`。
- 自定义目录不要放在共享/世界可写位置；索引含全盘文件路径，属敏感数据。
- App 仍固定使用默认目录；**自定义目录的实例对 App 不可见**，这是预期行为。
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

## 5. GUI「命令行工具」

设置页 → 通用 → 新增一行「命令行工具」。

### 5.1 状态

| 状态 | 展示 |
|---|---|
| 未安装 | 「安装到 PATH」 |
| 已安装（指向本 App） | 路径 + 「卸载」 |
| 已安装（其他来源） | 路径 + 说明，不覆盖 |
| 链接失效（App 被移动） | 旧目标 + 「修复 / 移除」 |

判定：

- 随包 CLI 路径 = App 可执行文件同目录的 `macEverything`
  （`Contents/MacOS/macEverything`）；复用 `engine_launcher` 的同目录定位逻辑。
- 扫描 `PATH` 中的 `macEverything`，`canonicalize` 后与本 App CLI 比较。
- 开发态（`target/...`，不在 `.app` 内）不提供安装按钮，只显示手动命令。

### 5.2 安装

- 只做**符号链接**，不做复制（App 更新后链接仍有效）。
- 只在**已在 PATH 中且可写**的目录里选目标，顺序：
  `/opt/homebrew/bin` → `~/.local/bin` → `~/.cargo/bin` → `/usr/local/bin`。
- 若没有可用目录：显示可复制的命令
  `sudo ln -sf "<随包 CLI>" /usr/local/bin/macEverything`，不自动提权。
- 已存在同名文件/链接且不是我们安装的：不覆盖，显示「其他来源」。
- 安装/修复幂等；成功后立即刷新状态。

### 5.3 卸载

- 仅当 `PATH` 中的 `macEverything` 是**符号链接且目标 canonicalize 后等于本 App CLI**
  时删除；绝不删除 cargo/Homebrew 安装的实体文件。
- 链接失效时允许「移除」（仅删除指向旧 App 路径、且文件名匹配的链接）。

### 5.4 提示

- 「首次在终端运行可能需要完全磁盘访问权限；可在系统设置中授予终端或 CLI。」
  链接到 `docs/usage.md`「完全磁盘访问」章节。
- 安装后可用：`macEverything start` / `query` / `status`；与 App 共用默认索引。

### 5.5 i18n 与测试

- 新文案全部入 `i18n` 目录（行标题、状态、按钮、错误、FDA 提示）。
- 纯逻辑单测：目标目录选择（注入 PATH/writable）、链接目标匹配、安装/卸载幂等、
  不覆盖其他来源、开发态禁用。
- 实机：Homebrew/user 目录安装、无写权限时的复制命令、已有 cargo 安装、
  App 移动后的失效修复、双语界面。

## 6. 代码改造点

- `crates/cli/src/main.rs`
  - 解析 `--data-dir` / `MCE_DATA_DIR`；`data_dir()` 改为带优先级。
  - `start` 子进程显式设置 `MCE_DATA_DIR`。
  - `query` / `status` 解析 `--json`、`--sort`、`--desc`。
  - JSON 输出：用 `serde_json::json!` 构造（CLI 已有 serde_json 依赖）；
    `QueryOutcome` / `StatusReport` 字段直接映射，不新增 serde derive 到 client。
  - JSON 错误统一出口；人类输出路径不变。
- `crates/app/src/ui/settings*`
  - 新增「命令行工具」行（通用分组），状态/按钮/提示。
- `crates/app/src/cli_install.rs`（新增）
  - 纯逻辑：候选目录、状态判定、链接目标校验；fs 操作：symlink/remove/read_link。
  - 复用 `engine_launcher` 的随包 CLI 定位；开发态禁用。
- `crates/app/src/i18n/catalog.rs`
  - 新增 CLI 安装相关文案。
- 文档：`docs/usage.md`（JSON、数据目录、安装行、外部工具 示例、FDA 提示）。
- 不引入新依赖：CLI 已有 `serde_json`；App 侧安装逻辑只用 std（`std::os::unix::fs::symlink` 等）。

## 7. 错误处理

| 场景 | 行为 |
|---|---|
| `--json` 下查询/状态失败 | stdout JSON 错误对象 + 退出码 1 |
| 自定义数据目录不存在/不可写 | 明确报错，提示路径与权限 |
| 自定义目录已有 daemon | 单实例提示已在运行；不误连默认实例 |
| `start` 子进程丢失数据目录 | 父进程显式设置 `MCE_DATA_DIR`，避免回到默认目录 |
| 无可用 PATH 目录 | 显示可复制 `sudo ln -sf` 命令，不自动提权 |
| PATH 中已有其他 macEverything | 显示「其他来源」，不覆盖 |
| 链接指向旧 App 路径 | 状态「失效」，提供修复/移除 |
| 开发态 | 隐藏安装按钮，显示手动命令 |
| 卸载时发现是实体文件 | 不删除，提示由 cargo/Homebrew 管理 |

## 8. 测试与验收

**单测**

- 数据目录优先级：flag > env > default；`start` 子进程环境值。
- JSON 形状：query/status 成功、离线、无索引、错误；`schema_version` 恒在。
- 退出码：成功 0、运行错误 1、用法 2。
- 安装逻辑：候选目录顺序/可写过滤、链接目标匹配、幂等、其他来源不覆盖、
  实体文件不卸载、开发态禁用。

**实机**

- `query --json` 在线/离线；`jq` 能直接解析；stdout 无杂文本。
- `status --json` 四态 + offline + no_index。
- `--sort mtime --desc` 与完整窗口排序结果一致。
- `MCE_DATA_DIR` 独立实例：两份索引、两个 socket、互不影响；`stop` 只停目标实例。
- App 设置页安装/卸载/修复；Homebrew、`~/.local/bin`、`/usr/local/bin` 场景；
  已有 cargo 安装不被覆盖；双语界面。
- 外部工具 冒烟：按 §4.4 独立目录 start → query JSON → stop。

## 9. 风险与缓解

| 风险 | 缓解 |
|---|---|
| App 被移动/删除导致链接失效 | 状态检测 + 修复/移除；文档说明 |
| PATH 目录差异/无写权限 | 只选已在 PATH 的可写目录；否则给复制命令 |
| 与 cargo/Homebrew 安装冲突 | 不覆盖、不卸载非本 App 链接 |
| TCC/FDA 归属 | 首次运行提示；文档说明授予终端或 CLI |
| 自定义数据目录泄露索引 | 0700 权限；文档警告不要放共享位置 |
| JSON schema 演进 | `schema_version`；字段只增不删，变更需升版本 |
| 共享 daemon 被 外部工具 stop | 文档明确：共享实例不要 stop；独立实例用独立目录 |
| 协议/CLI 版本混跑 | 协议 v1 拒绝异版本；外部工具 锁定版本 |

## 10. 文档同步

- `docs/usage.md`：命令行章节增加 `--json`、`--sort/--desc`、`MCE_DATA_DIR`、
  外部工具 示例；设置页增加「命令行工具」说明；FDA 提示。
- `docs/specs/v2-ui/product.md`：设置页行/分组更新。
- `docs/specs/i18n-v2.1/design.md` / `plan.md`：新增 CLI 安装文案（若 i18n Step 4 已落地，
  随目录增补）。
- 新 spec 链接：本文件。

## 11. 非目标

- MCP / HTTP / 远程 API。
- 自动提权的安装、修改 shell 配置、Homebrew formula。
- 协议字段变更；`exclude` 的 JSON 输出（后续按需）。
- Windows/Linux 移植。

## 12. 决策记录

1. **JSON 由 CLI 输出层实现**：client 返回结构化数据，协议不变；外部集成可用文本 CLI，
   也可直接走 Unix socket。
2. **`--data-dir` + `MCE_DATA_DIR` 双入口**：flag 便于脚本，env 便于子进程/容器；
   `start` 子进程显式继承。
3. **符号链接而非复制**：跟随 App 更新；App 移动的失效作为可检测状态处理。
4. **不自动改 PATH/不提权**：v1 只动可写 PATH 目录；否则给可复制命令，安全边界清晰。
5. **共享 vs 隔离显式二选一**：文档写清成本（第二份索引）与生命周期（勿 stop 共享实例）。
6. **JSON `schema_version` 独立于协议版本**：CLI 输出可演进，不强迫 daemon 升级。
