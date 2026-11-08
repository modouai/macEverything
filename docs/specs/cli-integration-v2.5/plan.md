# CLI 集成 v2.5 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 CLI 增加机器可读输出与独立数据目录，并在设置页提供安全的命令行工具安装/卸载/修复。

**Architecture:** 协议只新增可选 `desc_echo` 回显；JSON 由 CLI 输出层构造，不改变既有响应结构。数据目录解析集中在 CLI 参数层，`start` 与 App 拉起引擎都显式传递目录。安装器是独立的纯逻辑模块（PATH 探测、分类、动作），UI 只持有状态与工作线程，文件系统操作全部经注入的 `InstallerFs`，便于确定性测试。

**Tech Stack:** Rust 2021、serde/serde_json、libc、egui/eframe、Unix socket + 长度前缀 JSON。

---

## 文件结构

| 文件 | 职责 |
|---|---|
| `crates/protocol/src/lib.rs` | `Response::Query` 新增可选 `desc_echo` |
| `crates/client/src/lib.rs` | `QueryOutcome.desc_echo`；`StatusError`；离线索引不可读区分 |
| `crates/cli/src/args.rs`（新） | 参数语法、数据目录解析、排序键换算、用法错误 |
| `crates/cli/src/json.rs`（新） | `query/status/error` 的 JSON 构造与 `schema_version` |
| `crates/cli/src/main.rs` | 只做分派、人类输出、退出码 |
| `crates/cli/src/daemon.rs` | 0700 目录、0600 文件、父进程预检 |
| `crates/engine/src/persist.rs` | `index.bin` 0600 |
| `crates/cli/tests/cli_json.rs`（新） | 进程级 stdout/退出码/JSON 契约 |
| `crates/app/src/engine_launcher.rs` | spawn 时固定 `MCE_DATA_DIR` |
| `crates/app/src/cli_install/mod.rs`（新） | 安装器公开类型、`observe`/`apply` |
| `crates/app/src/cli_install/path_env.rs`（新） | 登录 shell PATH 探测与解析 |
| `crates/app/src/cli_install/classify.rs`（新） | PATH 扫描、九种分类、`effective`/`hits` |
| `crates/app/src/cli_install/actions.rs`（新） | 安装/修复/卸载、手动命令 |
| `crates/app/src/platform/mac.rs` | 进程组终止 helper（unsafe 集中处） |
| `crates/app/src/ui/settings.rs` | 安装器 UI 状态机、generation、single-flight |
| `crates/app/src/ui/settings_view.rs` | 「命令行工具」行；删除首屏断言 |
| `crates/app/src/i18n/catalog.rs` | 新增文案 |
| `docs/usage.md`、`README*.md` | 文档支持说明 |

发布版本：`2.5.0`。A 与 B 可独立提交、独立验证，C 跟随两者。

---

# Phase A：CLI 与数据目录

## Task 1: 协议新增 `desc_echo`

**Files:**
- Modify: `crates/protocol/src/lib.rs:87-101`
- Modify: `crates/cli/src/daemon.rs:190-266`
- Test: `crates/protocol/src/lib.rs` 测试模块

- [ ] **Step 1: 写失败测试**

在 `crates/protocol/src/lib.rs` 的测试模块中加入：

```rust
#[test]
fn desc_echo_backward_compatible() {
    let old_resp = br#"{"msg":"query","entries":[],"total":0,"elapsed_ms":0.0,"stat_ms":0.0,"sort_echo":"relevance"}"#;
    match serde_json::from_slice::<Response>(old_resp).unwrap() {
        Response::Query { desc_echo, .. } => assert_eq!(desc_echo, None),
        other => panic!("意外响应: {other:?}"),
    }

    let new_resp = Response::Query {
        entries: vec![],
        total: 0,
        elapsed_ms: 0.0,
        stat_ms: 0.0,
        sort_echo: "mtime".into(),
        desc_echo: Some(true),
    };
    let b = encode(&new_resp).unwrap();
    assert_eq!(decode_response(&b[4..]).unwrap(), new_resp);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p protocol desc_echo_backward_compatible`
Expected: 编译失败，`Response::Query` 没有 `desc_echo` 字段。

- [ ] **Step 3: 加字段**

`Response::Query` 末尾加入：

```rust
        /// 绝对方向回显(true = 降序, false = 升序, None = relevance 或旧
        /// daemon 未回显)。纯新增可选字段,不 bump 协议版本。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        desc_echo: Option<bool>,
```

- [ ] **Step 4: daemon 计算并填充**

`run_query` 中 `let (key, key_str) = parse_sort_key(sort);` 之后加入：

```rust
    // 绝对方向回显:NAME/SIZE/PATH 自然升序,MTIME 自然降序。
    let desc_echo = match key {
        SortKey::Relevance => None,
        SortKey::Mtime => Some(!desc),
        SortKey::Name | SortKey::Size | SortKey::Path => Some(desc),
    };
```

`Response::Query { ... }` 组装处加 `desc_echo,`。`encode_response` 的析构与重建两处都补上 `desc_echo`：

```rust
    let Response::Query {
        mut entries,
        total,
        elapsed_ms,
        stat_ms,
        sort_echo,
        desc_echo,
    } = resp
```

重建时加 `desc_echo,`。

- [ ] **Step 5: 跑测试**

Run: `cargo test -p protocol -p cli`
Expected: PASS（协议往返、旧 JSON 兼容、daemon 查询测试全绿）。

- [ ] **Step 6: 提交**

```bash
git add crates/protocol/src/lib.rs crates/cli/src/daemon.rs
git commit -m "feat(protocol): add optional desc_echo to query response"
```

---

## Task 2: client 增加 `desc_echo` 与带码状态错误

**Files:**
- Modify: `crates/client/src/lib.rs:126-320`
- Test: `crates/client/src/lib.rs` 测试模块

- [ ] **Step 1: 写失败测试**

在 `crates/client/src/lib.rs` 测试模块加入（复用现有 socket stub helper；若无则用 `std::os::unix::net::UnixListener` 起后台线程返回固定 JSON）：

```rust
#[test]
fn query_passes_desc_echo_through() {
    let (paths, _h) = stub_daemon(
        br#"{"msg":"query","entries":[],"total":0,"elapsed_ms":0.0,"stat_ms":0.0,"sort_echo":"mtime","desc_echo":true}"#,
    );
    let out = query(&paths, "x", 10, Some("mtime"), true).unwrap();
    assert_eq!(out.desc_echo, Some(true));
    assert_eq!(out.sort_echo, "mtime");
}

#[test]
fn status_keeps_daemon_error_code() {
    let (paths, _h) = stub_daemon(br#"{"msg":"error","code":"version","message":"bad"}"#);
    let err = status(&paths).unwrap_err();
    assert_eq!(err.code.as_deref(), Some("version"));
    assert_eq!(err.message, "bad");
}

#[test]
fn offline_status_corrupt_index_is_not_no_index() {
    let dir = tempfile::tempdir().unwrap();
    let paths = DaemonPaths { data_dir: dir.path().to_path_buf() };
    std::fs::write(paths.index_file(), b"not-an-index").unwrap();
    let err = offline_status(&paths).unwrap_err();
    assert_eq!(err.code.as_deref(), Some("index_unreadable"));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p client query_passes_desc_echo_through status_keeps_daemon_error_code offline_status_corrupt_index_is_not_no_index`
Expected: 编译失败（字段/错误类型不存在）。

- [ ] **Step 3: 实现**

`QueryOutcome` 加字段：

```rust
    /// 绝对方向回显(true = 降序, false = 升序, None = relevance 或旧 daemon)。
    pub desc_echo: Option<bool>,
```

`query` 的 `Response::Query` 析构加 `desc_echo`，构造 `QueryOutcome` 时带上；`offline_query` 返回 `desc_echo: None`。

新增：

```rust
/// status 失败(带响应错误码);连接/离线路径无码。
#[derive(Debug)]
pub struct StatusError {
    pub code: Option<String>,
    pub message: String,
}

impl std::fmt::Display for StatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for StatusError {}
```

`status` 签名改为：

```rust
pub fn status(paths: &DaemonPaths) -> Result<StatusOutcome, StatusError>
```

在线分支把 `Response::Error { code, message }` 映射为 `Err(StatusError { code: Some(code), message })`；其他失败映射 `code: None`。不要再使用 `expect_ok`（它会丢码）。

`offline_status`：

```rust
pub fn offline_status(paths: &DaemonPaths) -> Result<StatusOutcome, StatusError> {
    let index = paths.index_file();
    match std::fs::metadata(&index) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(StatusOutcome::NoIndex),
        Err(e) => {
            return Err(StatusError {
                code: Some("index_unreadable".into()),
                message: format!("索引不可读({}): {e}", index.display()),
            })
        }
        Ok(_) => {}
    }
    match persist::load_mmap(&index) {
        Ok(v) => {
            let live = v.entries().iter().filter(|e| e.is_live()).count() as u32;
            Ok(StatusOutcome::Reported(StatusReport {
                state: client::state::OFFLINE.to_string(),
                entry_count: live,
                gaps: vec![],
                io_error_count: 0,
                built_at: v.meta.built_at,
                index_file: index.to_string_lossy().into_owned(),
                offline: true,
            }))
        }
        Err(e) => Err(StatusError {
            code: Some("index_unreadable".into()),
            message: format!("索引损坏或不可读({}): {e}", index.display()),
        }),
    }
}
```

保留现有 `Ok(v)` 分支字段构造。

- [ ] **Step 4: 修 App 调用点**

`crates/app/src/tray.rs:483`、`crates/app/src/ui/palette.rs:1522`、`crates/app/src/ui/results.rs:4578` 使用 `match client::status`，错误类型变化后应能直接编译；若 `?` 到 `anyhow`，`StatusError` 已实现 `Error`。修到 `cargo check --workspace` 通过。

- [ ] **Step 5: 跑测试**

Run: `cargo test -p client && cargo check --workspace`
Expected: PASS。

- [ ] **Step 6: 提交**

```bash
git add crates/client/src/lib.rs crates/app/src/tray.rs crates/app/src/ui/palette.rs crates/app/src/ui/results.rs
git commit -m "feat(client): expose desc echo and coded status errors"
```

---

## Task 3: CLI 参数模块

**Files:**
- Create: `crates/cli/src/args.rs`
- Modify: `crates/cli/src/main.rs:1-15`

- [ ] **Step 1: 写纯逻辑测试（文件内 `#[cfg(test)]`）**

创建 `crates/cli/src/args.rs`，先只放类型、`unimplemented!` 与测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn data_dir_flag_wins_over_env() {
        let got = resolve_data_dir(
            Some(PathBuf::from("/tmp/flag")),
            Some(std::ffi::OsString::from("/tmp/env")),
            Some(std::ffi::OsString::from("/Users/u")),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/tmp/flag"));
    }

    #[test]
    fn env_relative_is_error() {
        assert_eq!(
            resolve_data_dir(None, Some("rel".into()), Some("/Users/u".into())),
            Err(DataDirError::EnvRelative)
        );
    }

    #[test]
    fn query_parses_flags_and_text() {
        let got = parse(a(&["query", "--json", "--sort", "mtime", "--desc", "quarter", "report"])).unwrap();
        assert_eq!(
            got.command,
            Command::Query {
                json: true,
                sort: Some(SortKey::Mtime),
                desc: true,
                query: "quarter report".into()
            }
        );
    }

    #[test]
    fn desc_without_sort_is_usage_error() {
        assert_eq!(parse(a(&["query", "--desc", "x"])), Err(UsageError::DescWithoutSort));
    }

    #[test]
    fn data_dir_must_precede_command() {
        assert_eq!(
            parse(a(&["query", "--data-dir", "/tmp/x", "a"])),
            Err(UsageError::UnexpectedArg("--data-dir".into()))
        );
    }

    #[test]
    fn sort_translation_is_absolute() {
        assert!(SortKey::Name.protocol_desc(true));
        assert!(!SortKey::Name.protocol_desc(false));
        assert!(!SortKey::Mtime.protocol_desc(true)); // 绝对降序 = 反转 mtime 自然降序
        assert!(SortKey::Mtime.protocol_desc(false));
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p macEverything-cli args`
Expected: 编译失败（`parse`/`resolve_data_dir` 未实现）。若 crate 名不同，用 `cargo test -p cli`。

- [ ] **Step 3: 实现解析**

```rust
use std::ffi::OsString;
use std::path::PathBuf;

pub const USAGE: &str = "\
macEverything — macOS 全盘文件名即时搜索

用法:
  macEverything [--data-dir <PATH>] start
  macEverything [--data-dir <PATH>] stop
  macEverything [--data-dir <PATH>] query [--json] [--sort <name|size|mtime|path>] [--desc] [--] <关键词>
  macEverything [--data-dir <PATH>] status [--json]
  macEverything [--data-dir <PATH>] exclude add|remove|list|reset [路径]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey { Name, Size, Mtime, Path }

impl SortKey {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Size => "size",
            Self::Mtime => "mtime",
            Self::Path => "path",
        }
    }
    fn natural_desc(self) -> bool { matches!(self, Self::Mtime) }
    /// 绝对 desc → 协议的“反转自然方向”位。
    pub fn protocol_desc(self, absolute_desc: bool) -> bool {
        self.natural_desc() != absolute_desc
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Start,
    Stop,
    Query { json: bool, sort: Option<SortKey>, desc: bool, query: String },
    Status { json: bool },
    Exclude { action: String, path: Option<String> },
}

#[derive(Debug, PartialEq, Eq)]
pub struct Args { pub data_dir: Option<PathBuf>, pub command: Command }

#[derive(Debug, PartialEq, Eq)]
pub enum UsageError {
    MissingCommand,
    UnknownCommand(String),
    MissingDataDir,
    DuplicateDataDir,
    DataDirRelative,
    MissingSortValue,
    InvalidSort(String),
    DuplicateSort,
    DescWithoutSort,
    MissingQuery,
    UnexpectedArg(String),
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCommand => f.write_str("缺少子命令"),
            Self::UnknownCommand(c) => write!(f, "未知子命令: {c}"),
            Self::MissingDataDir => f.write_str("--data-dir 缺少路径"),
            Self::DuplicateDataDir => f.write_str("--data-dir 重复"),
            Self::DataDirRelative => f.write_str("--data-dir 必须是绝对路径"),
            Self::MissingSortValue => f.write_str("--sort 缺少值"),
            Self::InvalidSort(v) => write!(f, "非法排序键: {v}(可选 name|size|mtime|path)"),
            Self::DuplicateSort => f.write_str("--sort 重复"),
            Self::DescWithoutSort => f.write_str("--desc 必须与 --sort 同时使用"),
            Self::MissingQuery => f.write_str("缺少查询关键词"),
            Self::UnexpectedArg(a) => write!(f, "意外参数: {a}"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum DataDirError { EnvEmpty, EnvRelative, HomeUnset }

impl std::fmt::Display for DataDirError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EnvEmpty => f.write_str("MCE_DATA_DIR 为空"),
            Self::EnvRelative => f.write_str("MCE_DATA_DIR 必须是绝对路径"),
            Self::HomeUnset => f.write_str("HOME 未设置,无法确定默认数据目录"),
        }
    }
}

pub fn resolve_data_dir(
    flag: Option<PathBuf>,
    env: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, DataDirError> {
    if let Some(p) = flag { return Ok(p); }
    if let Some(v) = env {
        if v.to_string_lossy().trim().is_empty() { return Err(DataDirError::EnvEmpty); }
        let p = PathBuf::from(v);
        if !p.is_absolute() { return Err(DataDirError::EnvRelative); }
        return Ok(p);
    }
    let home = home.ok_or(DataDirError::HomeUnset)?;
    Ok(PathBuf::from(home).join("Library/Application Support/macEverything"))
}

fn parse_sort(v: &str) -> Result<SortKey, UsageError> {
    match v {
        "name" => Ok(SortKey::Name),
        "size" => Ok(SortKey::Size),
        "mtime" => Ok(SortKey::Mtime),
        "path" => Ok(SortKey::Path),
        other => Err(UsageError::InvalidSort(other.into())),
    }
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Args, UsageError> {
    let mut it = args.into_iter().peekable();
    let mut data_dir = None;

    while let Some(a) = it.peek().cloned() {
        let value = if a == "--data-dir" {
            it.next();
            Some(it.next().ok_or(UsageError::MissingDataDir)?)
        } else if let Some(v) = a.strip_prefix("--data-dir=") {
            let v = v.to_string();
            it.next();
            Some(v)
        } else {
            None
        };
        if let Some(v) = value {
            if data_dir.is_some() { return Err(UsageError::DuplicateDataDir); }
            let p = PathBuf::from(v);
            if !p.is_absolute() { return Err(UsageError::DataDirRelative); }
            data_dir = Some(p);
        } else {
            break;
        }
    }

    let cmd = it.next().ok_or(UsageError::MissingCommand)?;
    let command = match cmd.as_str() {
        "start" => { no_extra(&mut it)?; Command::Start }
        "stop" => { no_extra(&mut it)?; Command::Stop }
        "status" => {
            let mut json = false;
            for a in it {
                if a == "--json" { json = true; } else { return Err(UsageError::UnexpectedArg(a)); }
            }
            Command::Status { json }
        }
        "query" => parse_query(it)?,
        "exclude" => {
            let action = it.next().unwrap_or_else(|| "list".into());
            let path = it.next();
            if let Some(extra) = it.next() { return Err(UsageError::UnexpectedArg(extra)); }
            Command::Exclude { action, path }
        }
        other => return Err(UsageError::UnknownCommand(other.into())),
    };
    Ok(Args { data_dir, command })
}

fn no_extra<I: Iterator<Item = String>>(it: &mut I) -> Result<(), UsageError> {
    if let Some(a) = it.next() { Err(UsageError::UnexpectedArg(a)) } else { Ok(()) }
}

fn parse_query<I: Iterator<Item = String>>(mut it: I) -> Result<Command, UsageError> {
    let mut json = false;
    let mut sort = None;
    let mut desc = false;
    let mut text = Vec::new();
    let mut literal = false;

    while let Some(a) = it.next() {
        if literal {
            text.push(a);
            continue;
        }
        if a == "--" { literal = true; continue; }
        if !text.is_empty() {
            if a.starts_with('-') { return Err(UsageError::UnexpectedArg(a)); }
            text.push(a);
            continue;
        }
        match a.as_str() {
            "--json" => json = true,
            "--desc" => desc = true,
            "--sort" => {
                if sort.is_some() { return Err(UsageError::DuplicateSort); }
                let v = it.next().ok_or(UsageError::MissingSortValue)?;
                sort = Some(parse_sort(&v)?);
            }
            other if other.starts_with("--sort=") => {
                if sort.is_some() { return Err(UsageError::DuplicateSort); }
                sort = Some(parse_sort(&other["--sort=".len()..])?);
            }
            other if other.starts_with('-') => return Err(UsageError::UnexpectedArg(other.into())),
            _ => text.push(a),
        }
    }

    if desc && sort.is_none() { return Err(UsageError::DescWithoutSort); }
    let query = text.join(" ");
    if query.trim().is_empty() { return Err(UsageError::MissingQuery); }
    Ok(Command::Query { json, sort, desc, query })
}
```

- [ ] **Step 4: 跑测试**

Run: `cargo test -p cli args`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add crates/cli/src/args.rs crates/cli/src/main.rs
git commit -m "feat(cli): add argument parsing and data-dir resolution"
```

---

## Task 4: CLI JSON 输出与分派

**Files:**
- Create: `crates/cli/src/json.rs`
- Modify: `crates/cli/src/main.rs`
- Test: `crates/cli/tests/cli_json.rs`

- [ ] **Step 1: 写 JSON 构造函数测试**

`crates/cli/src/json.rs` 先放测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use client::{QueryOutcome, StatusReport};
    use protocol::HitEntry;

    #[test]
    fn query_shape_has_all_keys() {
        let out = QueryOutcome {
            entries: vec![HitEntry {
                name: "a".into(),
                path: "/a".into(),
                size: None,
                mtime: None,
                is_dir: false,
            }],
            total: 1,
            elapsed_ms: 1.0,
            stat_ms: 0.0,
            sort_echo: "relevance".into(),
            desc_echo: None,
            offline_built_at: None,
        };
        let v = query_json(&out);
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["total"], 1);
        assert!(v["entries"][0]["size"].is_null());
        assert!(v["entries"][0]["mtime"].is_null());
        assert!(v["desc_echo"].is_null());
    }

    #[test]
    fn no_index_shape_is_stable() {
        let v = no_index_json("/tmp/mce");
        assert_eq!(v["state"], "no_index");
        assert_eq!(v["entry_count"], 0);
        assert_eq!(v["offline"], true);
        assert_eq!(v["gaps"], serde_json::json!([]));
        assert_eq!(v["index_file"], "/tmp/mce/index.bin");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p cli json::`
Expected: 编译失败。

- [ ] **Step 3: 实现 JSON 构造**

```rust
use client::{QueryOutcome, StatusOutcome};
use serde_json::{json, Value};
use std::path::Path;

pub const SCHEMA_VERSION: u32 = 1;

pub fn query_json(out: &QueryOutcome) -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "total": out.total,
        "elapsed_ms": out.elapsed_ms,
        "stat_ms": out.stat_ms,
        "sort_echo": out.sort_echo,
        "desc_echo": out.desc_echo,
        "offline_built_at": out.offline_built_at,
        "entries": out.entries.iter().map(|e| json!({
            "name": e.name,
            "path": e.path,
            "size": e.size,
            "mtime": e.mtime,
            "is_dir": e.is_dir,
        })).collect::<Vec<_>>(),
    })
}

pub fn status_json(outcome: &StatusOutcome, data_dir: &Path) -> Value {
    match outcome {
        StatusOutcome::Reported(r) => json!({
            "schema_version": SCHEMA_VERSION,
            "state": r.state,
            "entry_count": r.entry_count,
            "gaps": r.gaps.iter().map(|g| json!({"path": g.path, "reason": g.reason})).collect::<Vec<_>>(),
            "io_error_count": r.io_error_count,
            "built_at": r.built_at,
            "index_file": r.index_file,
            "offline": r.offline,
        }),
        StatusOutcome::NoIndex => no_index_json(&data_dir.join("index.bin").to_string_lossy()),
    }
}

pub fn no_index_json(index_file: &str) -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "state": "no_index",
        "entry_count": 0,
        "gaps": [],
        "io_error_count": 0,
        "built_at": 0,
        "index_file": index_file,
        "offline": true,
    })
}

pub fn error_json(code: Option<&str>, message: &str) -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "error": { "code": code, "message": message },
    })
}
```

（`status_json` 的 `data_dir` 参数其实只用 `index_file`；若想更直接，签名改为 `status_json(outcome, index_file: &Path)`。保持与 `NoIndex` 一致即可。）

- [ ] **Step 4: 写进程级测试**

`crates/cli/tests/cli_json.rs`：

```rust
use std::process::Command;

fn bin() -> &'static str { env!("CARGO_BIN_EXE_macEverything") }

fn isolated() -> (tempfile::TempDir, Command) {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Command::new(bin());
    c.env("HOME", dir.path())
        .env("MCE_DATA_DIR", dir.path().join("data"));
    (dir, c)
}

#[test]
fn no_args_is_usage_error_with_empty_stdout() {
    let out = Command::new(bin()).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(!out.stderr.is_empty());
}

#[test]
fn status_json_no_index_is_one_object() {
    let (_d, mut c) = isolated();
    let out = c.args(["status", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["state"], "no_index");
    assert_eq!(v["entry_count"], 0);
    assert_eq!(v["offline"], true);
}

#[test]
fn query_json_without_index_is_coded_null_error() {
    let (_d, mut c) = isolated();
    let out = c.args(["query", "--json", "x"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema_version"], 1);
    assert!(v["error"]["code"].is_null());
}

#[test]
fn desc_without_sort_exits_2_with_empty_stdout() {
    let (_d, mut c) = isolated();
    let out = c.args(["query", "--json", "--desc", "x"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}
```

run: `cargo test -p cli --test cli_json`；先失败，后实现。

- [ ] **Step 5: 重写 main.rs 分派**

保留现有 `print_query` / `print_status` / `human_size` / `fmt_time`，把 `main` 改为：

```rust
mod args;
mod daemon;
mod json;

use args::{Command, DataDirError, SortKey, UsageError};
use client::StatusError;
use std::io::Write;
use std::process::ExitCode;

enum CliError {
    Usage(UsageError),
    Runtime { code: Option<String>, message: String, json: bool },
}

impl CliError {
    fn exit(self) -> ExitCode {
        match self {
            Self::Usage(e) => {
                eprintln!("{e}\n\n{}", args::USAGE);
                ExitCode::from(2)
            }
            Self::Runtime { code, message, json } => {
                if json {
                    println!("{}", json::error_json(code.as_deref(), &message));
                } else {
                    eprintln!("{message}");
                }
                ExitCode::from(1)
            }
        }
    }
}

fn resolve_paths(flag: Option<PathBuf>, want_json: bool) -> Result<client::DaemonPaths, CliError> {
    let env = std::env::var_os("MCE_DATA_DIR");
    let home = std::env::var_os("HOME");
    args::resolve_data_dir(flag, env, home)
        .map(|data_dir| client::DaemonPaths { data_dir })
        .map_err(|e: DataDirError| CliError::Runtime {
            code: None,
            message: e.to_string(),
            json: want_json,
        })
}

fn command_json(cmd: &Command) -> bool {
    matches!(
        cmd,
        Command::Query { json: true, .. } | Command::Status { json: true }
    )
}

fn run() -> Result<(), CliError> {
    let parsed = args::parse(std::env::args().skip(1)).map_err(CliError::Usage)?;
    let want_json = command_json(&parsed.command);
    let paths = resolve_paths(parsed.data_dir, want_json)?;
    match parsed.command {
        Command::Start => start(&paths),
        Command::Stop => {
            let n = client::stop(&paths).map_err(runtime)?;
            println!("已停止,最终条目数 {n}");
            Ok(())
        }
        Command::Query { json: want_json, sort, desc, query } => {
            let sort_str = sort.map(SortKey::as_str);
            let protocol_desc = sort.map(|s| s.protocol_desc(desc)).unwrap_or(false);
            match client::query(&paths, &query, 1000, sort_str, protocol_desc) {
                Ok(out) if want_json => {
                    println!("{}", json::query_json(&out));
                    Ok(())
                }
                Ok(out) => { print_query(&out); Ok(()) }
                Err(e) => Err(CliError::Runtime {
                    code: e.code,
                    message: e.message,
                    json: want_json,
                }),
            }
        }
        Command::Status { json: want_json } => {
            match client::status(&paths) {
                Ok(outcome) if want_json => {
                    println!("{}", json::status_json(&outcome, &paths.data_dir));
                    Ok(())
                }
                Ok(outcome) => { print_status_outcome(&outcome); Ok(()) }
                Err(e) => Err(CliError::Runtime {
                    code: e.code,
                    message: e.message,
                    json: want_json,
                }),
            }
        }
        Command::Exclude { action, path } => {
            let out = client::exclude(&paths, &action, path.as_deref()).map_err(runtime)?;
            println!("排除目录({} 条):", out.excluded.len());
            for e in &out.excluded { println!("  {e}"); }
            if !out.ok { return Err(runtime(anyhow::anyhow!(out.message.unwrap_or_else(|| "操作未生效".into())))); }
            Ok(())
        }
    }
}

fn runtime(e: impl std::fmt::Display) -> CliError {
    CliError::Runtime { code: None, message: e.to_string(), json: false }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => e.exit(),
    }
}
```

`start`：

```rust
fn start(paths: &client::DaemonPaths) -> Result<(), CliError> {
    // 父进程预检:0700 + 可写;失败在 spawn 前报出
    if let Err(e) = daemon::ensure_data_dir(&paths.data_dir) {
        return Err(CliError::Runtime {
            code: None,
            message: format!("数据目录不可用({}): {e}", paths.data_dir.display()),
            json: false,
        });
    }
    if std::env::var_os("MCE_FOREGROUND").is_some() {
        return daemon::serve(paths.clone()).map_err(runtime);
    }
    spawn_background(paths)
}
```

`spawn_background` 保留现有逻辑，只加 `.env("MCE_DATA_DIR", &paths.data_dir)`，并把 `paths` 传入。

`print_status_outcome` 把现有 `print_status` 的 `Reported`/`NoIndex` 分支包一层；错误已由 `StatusError` 处理。

- [ ] **Step 6: 跑进程测试**

Run: `cargo test -p cli --test cli_json`
Expected: PASS。

- [ ] **Step 7: 提交**

```bash
git add crates/cli/src/main.rs crates/cli/src/json.rs crates/cli/tests/cli_json.rs
git commit -m "feat(cli): add json output, absolute sort direction, and usage errors"
```

---

## Task 5: 数据目录权限与 App 拉起引擎固定目录

**Files:**
- Modify: `crates/cli/src/daemon.rs:1176`
- Modify: `crates/engine/src/persist.rs:97`
- Modify: `crates/app/src/engine_launcher.rs:145-160`
- Test: `crates/cli/src/daemon.rs`、`crates/engine/src/persist.rs`、`crates/app/src/engine_launcher.rs`

- [ ] **Step 1: 写权限测试**

`daemon.rs` 测试模块：

```rust
#[test]
fn data_dir_is_0700_and_log_is_0600() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    ensure_data_dir(&data).unwrap();
    assert_eq!(std::fs::metadata(&data).unwrap().permissions().mode() & 0o777, 0o700);
    init_logger(&DaemonPaths { data_dir: data.clone() });
    let log = data.join("daemon.log");
    assert_eq!(std::fs::metadata(&log).unwrap().permissions().mode() & 0o777, 0o600);
}
```

`persist.rs` 测试：写索引到 tempdir，断言 `0o600`。

`engine_launcher.rs` 测试：

```rust
#[test]
fn engine_command_pins_data_dir() {
    let paths = client::DaemonPaths { data_dir: PathBuf::from("/tmp/app-data") };
    let cmd = engine_command(Path::new("/tmp/macEverything"), &paths);
    let env: Vec<_> = cmd.get_envs().collect();
    assert!(env.iter().any(|(k, v)| *k == "MCE_DATA_DIR" && *v == Some(std::ffi::OsStr::new("/tmp/app-data"))));
}
```

- [ ] **Step 2: 实现目录/文件权限**

`daemon.rs`：

```rust
pub fn ensure_data_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true).mode(0o700);
    b.create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}
```

`serve` 首行改为 `ensure_data_dir(&paths.data_dir).context("创建数据目录失败")?;`。

`init_logger` 的 `OpenOptions` 加：

```rust
            .mode(0o600)
```

并在文件已存在时 `let _ = std::fs::set_permissions(paths.log_file(), std::fs::Permissions::from_mode(0o600));`（放在 open 前）。

`persist::save` 的 `File::create(&tmp)` 改为：

```rust
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
```

（顶部 `#[cfg(unix)] use std::os::unix::fs::OpenOptionsExt;`；项目仅 macOS。）

配置文件写入处（`daemon.rs` 的 `OpenOptions`）同样加 `.mode(0o600)`。

- [ ] **Step 3: App launcher 固定目录**

在 `engine_launcher.rs` 加：

```rust
fn engine_command(cand: &Path, paths: &client::DaemonPaths) -> std::process::Command {
    let mut c = std::process::Command::new(cand);
    c.arg("start")
        .env("MCE_DATA_DIR", &paths.data_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    c
}
```

`ensure_engine` 中 `Command::new(cand)...spawn()` 改为 `engine_command(cand, paths).spawn()`。

- [ ] **Step 4: 跑测试**

Run: `cargo test -p cli -p engine -p app engine_command_pins_data_dir data_dir_is_0700_and_log_is_0600`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add crates/cli/src/daemon.rs crates/engine/src/persist.rs crates/app/src/engine_launcher.rs
git commit -m "feat(data): enforce 0700/0600 and pin app engine data dir"
```

---

# Phase B：设置页安装器

## Task 6: 安装器类型与 PATH 探测

**Files:**
- Create: `crates/app/src/cli_install/mod.rs`
- Create: `crates/app/src/cli_install/path_env.rs`
- Modify: `crates/app/src/main.rs:8-19`（`mod cli_install;`）
- Modify: `crates/app/src/platform/mac.rs`（进程组 kill helper）
- Modify: `crates/app/src/platform/mod.rs`（非 macOS stub）

- [ ] **Step 1: 写 PATH 解析测试**

`path_env.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct FakeRunner {
        outputs: Vec<Option<&'static [u8]>>,
        cursor: Cell<usize>,
    }

    impl FakeRunner {
        fn new(outputs: &[Option<&'static [u8]>]) -> Self {
            Self { outputs: outputs.to_vec(), cursor: Cell::new(0) }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, _shell: &Path, _flag: &str, _timeout: Duration) -> Option<Vec<u8>> {
            let i = self.cursor.get();
            self.cursor.set(i + 1);
            self.outputs.get(i).copied().flatten().map(|b| b.to_vec())
        }
    }

    #[test]
    fn last_complete_path_line_wins_and_noise_is_ignored() {
        let out = b"hello\nPATH=/a:/b\nMANPATH=/x\nPATH=/c:/d\r\n";
        assert_eq!(parse_path_env(out), Some(std::ffi::OsString::from("/c:/d")));
    }

    #[test]
    fn empty_or_truncated_path_is_ignored() {
        assert_eq!(parse_path_env(b"PATH=\n"), None);
        // 无换行收尾 = 截断尾巴(64 KiB 上限 / 超时 SIGKILL 半路),不算完整行。
        assert_eq!(parse_path_env(b"PATH=/a:/b"), None);
        assert_eq!(parse_path_env(b"PATH=/a:/b\nPATH="), Some(std::ffi::OsString::from("/a:/b")));
    }

    #[test]
    fn provenance_falls_back_in_order() {
        let r = FakeRunner::new(&[Some(b"PATH=/a\n".as_slice()), Some(b"PATH=/b\n".as_slice())]);
        assert_eq!(probe_path_env(&r, Some("/proc:/bin")), (PathSource::LoginInteractive, Some("/a".into())));
        let r = FakeRunner::new(&[None, Some(b"PATH=/b\n".as_slice())]);
        assert_eq!(probe_path_env(&r, Some("/proc:/bin")), (PathSource::LoginOnly, Some("/b".into())));
        let r = FakeRunner::new(&[None, None]);
        assert_eq!(probe_path_env(&r, Some("/proc:/bin")), (PathSource::ProcessEnv, Some("/proc:/bin".into())));
    }
}
```

`FakeRunner` 实现下面定义的 trait。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p app path_env`
Expected: 编译失败。

- [ ] **Step 3: 实现类型与探测**

`path_env.rs`：

```rust
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathSource { LoginInteractive, LoginOnly, ProcessEnv, Failed }

pub trait CommandRunner {
    /// 运行 `$SHELL <flag> 'command env'`，返回 stdout；超时/失败返回 None。
    fn run(&self, shell: &Path, flag: &str, timeout: Duration) -> Option<Vec<u8>>;
}

/// 只认最后一个**完整** `PATH=` 行(以 `\n` 收尾);空值不算。
/// 末尾无换行的残余段是截断尾巴(64 KiB 读上限 / 超时 SIGKILL 半路),
/// 可能是「写了一半的 PATH=…」,不当完整行(设计 §5.0-A / §8)。
pub fn parse_path_env(bytes: &[u8]) -> Option<OsString> {
    let mut found = None;
    for line in bytes.split_inclusive(|&b| b == b'\n') {
        let Some(line) = line.strip_suffix(b"\n") else {
            continue; // 截断尾巴,不认
        };
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if let Some(v) = line.strip_prefix(b"PATH=") {
            if !v.is_empty() {
                found = Some(std::ffi::OsStr::from_bytes(v).to_os_string());
            }
        }
    }
    found
}

pub fn probe_path_env(runner: &dyn CommandRunner, process_path: Option<&str>) -> (PathSource, Option<OsString>) {
    let shell = std::env::var_os("SHELL").map(std::path::PathBuf::from).unwrap_or_else(|| "/bin/zsh".into());
    let timeout = Duration::from_secs(3);
    if let Some(out) = runner.run(&shell, "-lic", timeout) {
        if let Some(p) = parse_path_env(&out) { return (PathSource::LoginInteractive, Some(p)); }
    }
    if let Some(out) = runner.run(&shell, "-lc", timeout) {
        if let Some(p) = parse_path_env(&out) { return (PathSource::LoginOnly, Some(p)); }
    }
    match process_path {
        Some(p) if !p.trim().is_empty() => (PathSource::ProcessEnv, Some(OsString::from(p))),
        _ => (PathSource::Failed, None),
    }
}
```

`mod.rs` 先只声明 Task 6 需要的模块，Task 7 再加 `actions`/`classify`：

```rust
pub mod path_env;

pub use path_env::PathSource;
```

- [ ] **Step 4: 平台进程组 helper**

`platform/mod.rs` 加：

```rust
pub fn kill_process_group(pid: u32, signal: i32) { imp::kill_process_group(pid, signal) }
```

`platform/mac.rs`：

```rust
pub fn kill_process_group(pid: u32, signal: i32) {
    // SAFETY: kill(2) 只作用于传入的进程组;pid 由 Child::id 取得,
    // 调用方在超时路径上使用,不存在内存安全问题。
    unsafe {
        libc::kill(-(pid as i32), signal);
    }
}
```

`platform` 的非 macOS stub 直接空实现（项目仅 macOS，但保持别名一致）。

真实 `CommandRunner`（放 `path_env.rs` 或 `actions.rs`）：

```rust
pub struct RealRunner;

impl CommandRunner for RealRunner {
    fn run(&self, shell: &Path, flag: &str, timeout: Duration) -> Option<Vec<u8>> {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new(shell)
            .arg(flag)
            .arg("command env")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .ok()?;
        let mut stdout = child.stdout.take()?;
        let reader = std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            while buf.len() < 64 * 1024 {
                match stdout.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            buf
        });
        let start = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(20)),
                _ => {
                    crate::platform::kill_process_group(child.id(), libc::SIGTERM);
                    let grace = std::time::Instant::now();
                    while child.try_wait().ok().flatten().is_none() && grace.elapsed() < Duration::from_millis(200) {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    crate::platform::kill_process_group(child.id(), libc::SIGKILL);
                    let _ = child.wait();
                    break;
                }
            }
        }
        reader.join().ok()
    }
}
```

- [ ] **Step 5: 跑测试**

Run: `cargo test -p app path_env`
Expected: PASS。

- [ ] **Step 6: 提交**

```bash
git add crates/app/src/cli_install crates/app/src/platform crates/app/src/main.rs
git commit -m "feat(installer): add shell path probe and installer types"
```

---

## Task 7: PATH 分类与动作矩阵

**Files:**
- Modify: `crates/app/src/cli_install/mod.rs`
- Create: `crates/app/src/cli_install/classify.rs`
- Create: `crates/app/src/cli_install/actions.rs`

- [ ] **Step 1: 定义 `InstallerFs` 与真实实现**

`mod.rs` 加：

```rust
pub mod actions;
pub mod classify;

pub use classify::{Availability, EntryState, Observation};
pub use path_env::PathSource;

use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    pub dev: u64,
    pub ino: u64,
    pub is_file: bool,
    pub is_symlink: bool,
    pub mode: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatFs { pub read_only: bool }

pub trait InstallerFs {
    fn lstat(&self, p: &Path) -> io::Result<Meta>;
    fn stat(&self, p: &Path) -> io::Result<Meta>;
    fn read_link(&self, p: &Path) -> io::Result<PathBuf>;
    fn access_x(&self, p: &Path) -> bool;
    fn access_w(&self, p: &Path) -> bool;
    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn remove_file(&self, p: &Path) -> io::Result<()>;
    fn statfs(&self, p: &Path) -> io::Result<StatFs>;
}

pub struct RealFs;

impl RealFs {
    pub fn new() -> Self { Self }
}

impl InstallerFs for RealFs {
    fn lstat(&self, p: &Path) -> io::Result<Meta> {
        let m = std::fs::symlink_metadata(p)?;
        Ok(meta(&m))
    }
    fn stat(&self, p: &Path) -> io::Result<Meta> {
        let m = std::fs::metadata(p)?;
        Ok(meta(&m))
    }
    fn read_link(&self, p: &Path) -> io::Result<PathBuf> { std::fs::read_link(p) }
    fn access_x(&self, p: &Path) -> bool { access(p, libc::X_OK) }
    fn access_w(&self, p: &Path) -> bool { access(p, libc::W_OK) }
    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> { std::fs::rename(from, to) }
    fn remove_file(&self, p: &Path) -> io::Result<()> { std::fs::remove_file(p) }
    fn statfs(&self, p: &Path) -> io::Result<StatFs> {
        let c = cstr(p);
        let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(StatFs { read_only: (s.f_flag as i32) & libc::MNT_RDONLY != 0 })
    }
}

fn meta(m: &std::fs::Metadata) -> Meta {
    use std::os::unix::fs::MetadataExt;
    Meta {
        dev: m.dev(),
        ino: m.ino(),
        is_file: m.is_file(),
        is_symlink: m.file_type().is_symlink(),
        mode: m.mode(),
    }
}

fn cstr(p: &Path) -> std::ffi::CString {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(p.as_os_str().as_bytes()).expect("路径不含 NUL")
}

fn access(p: &Path, mode: libc::c_int) -> bool {
    // SAFETY: access(2) 只读路径权限位,不涉及内存安全。
    unsafe { libc::access(cstr(p).as_ptr(), mode) == 0 }
}
```

- [ ] **Step 2: 写分类测试**

`classify.rs` 的测试先用内存 `FakeFs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_install::{InstallerFs, Meta, StatFs};
    use std::collections::HashMap;
    use std::io;
    use std::path::{Path, PathBuf};

    struct FakeFs {
        stat: HashMap<PathBuf, Meta>,
        link: HashMap<PathBuf, PathBuf>,
        no_volumes: bool,
    }
    impl FakeFs {
        fn new() -> Self { Self { stat: HashMap::new(), link: HashMap::new(), no_volumes: false } }
        fn file(&mut self, p: &str, dev: u64, ino: u64) {
            self.stat.insert(PathBuf::from(p), Meta { dev, ino, is_file: true, is_symlink: false, mode: 0o755 });
        }
        fn link(&mut self, p: &str, target: &str) {
            self.link.insert(PathBuf::from(p), PathBuf::from(target));
            self.stat.insert(PathBuf::from(p), Meta { dev: 1, ino: 1, is_file: false, is_symlink: true, mode: 0o777 });
        }
    }
    impl InstallerFs for FakeFs {
        fn lstat(&self, p: &Path) -> io::Result<Meta> { self.stat.get(p).copied().ok_or(io::ErrorKind::NotFound.into()) }
        fn stat(&self, p: &Path) -> io::Result<Meta> {
            if let Some(t) = self.link.get(p) { return self.stat.get(t).copied().ok_or(io::ErrorKind::NotFound.into()); }
            self.stat.get(p).copied().ok_or(io::ErrorKind::NotFound.into())
        }
        fn read_link(&self, p: &Path) -> io::Result<PathBuf> { self.link.get(p).cloned().ok_or(io::ErrorKind::InvalidInput.into()) }
        fn access_x(&self, p: &Path) -> bool { self.stat(p).map(|m| m.is_file && m.mode & 0o111 != 0).unwrap_or(false) }
        fn access_w(&self, _p: &Path) -> bool { true }
        fn symlink(&self, _t: &Path, _l: &Path) -> io::Result<()> { Ok(()) }
        fn rename(&self, _f: &Path, _t: &Path) -> io::Result<()> { Ok(()) }
        fn remove_file(&self, _p: &Path) -> io::Result<()> { Ok(()) }
        fn statfs(&self, p: &Path) -> io::Result<StatFs> {
            if self.no_volumes && p.starts_with("/Volumes") { return Err(io::ErrorKind::NotFound.into()); }
            Ok(StatFs { read_only: false })
        }
    }

    #[test]
    fn app_pattern_requires_components() {
        assert!(is_app_target(Path::new("/A/Foo.app/Contents/MacOS/macEverything")));
        assert!(!is_app_target(Path::new("/tmp/notapp/Contents/MacOS/macEverything")));
        assert!(!is_app_target(Path::new("/A/Foo.app/Contents/MacOS/macEverything.bak")));
    }

    #[test]
    fn relative_link_resolves_against_parent() {
        let mut fs = FakeFs::new();
        fs.file("/A/app/Contents/MacOS/macEverything", 9, 9);
        fs.link("/bin/macEverything", "../A/app/Contents/MacOS/macEverything");
        let s = classify_entry(&fs, Path::new("/bin/macEverything"), Path::new("/A/app/Contents/MacOS/macEverything")).unwrap().unwrap();
        assert!(matches!(s, EntryState::Ours { .. }));
    }

    #[test]
    fn eacces_is_unreadable_not_broken() {
        // FakeFs 的 lstat 返回 PermissionDenied;scan_path 必须产出 Unreadable。
        struct Deny;
        impl InstallerFs for Deny {
            fn lstat(&self, _: &Path) -> io::Result<Meta> { Err(io::ErrorKind::PermissionDenied.into()) }
            fn stat(&self, _: &Path) -> io::Result<Meta> { Err(io::ErrorKind::PermissionDenied.into()) }
            fn read_link(&self, _: &Path) -> io::Result<PathBuf> { Err(io::ErrorKind::PermissionDenied.into()) }
            fn access_x(&self, _: &Path) -> bool { false }
            fn access_w(&self, _: &Path) -> bool { false }
            fn symlink(&self, _: &Path, _: &Path) -> io::Result<()> { Ok(()) }
            fn rename(&self, _: &Path, _: &Path) -> io::Result<()> { Ok(()) }
            fn remove_file(&self, _: &Path) -> io::Result<()> { Ok(()) }
            fn statfs(&self, _: &Path) -> io::Result<StatFs> { Ok(StatFs { read_only: false }) }
        }
        let (hits, _, unknown) = scan_path(&Deny, std::ffi::OsStr::new("/denied"), Path::new("/A/app/Contents/MacOS/macEverything"));
        assert!(unknown);
        assert!(matches!(hits[0], EntryState::Unreadable { .. }));
    }

    #[test]
    fn broken_app_volume_unmounted_disables_repair() {
        let mut fs = FakeFs::new();
        fs.no_volumes = true;
        fs.file("/A/app/Contents/MacOS/macEverything", 9, 9);
        fs.link("/bin/macEverything", "/Volumes/Backup/A/app/Contents/MacOS/macEverything");
        let s = classify_entry(&fs, Path::new("/bin/macEverything"), Path::new("/A/app/Contents/MacOS/macEverything")).unwrap().unwrap();
        match s {
            EntryState::BrokenApp { volume_mounted, .. } => assert!(!volume_mounted),
            other => panic!("意外分类: {other:?}"),
        }
    }
}
```

- [ ] **Step 3: 实现分类**

```rust
use crate::cli_install::{InstallerFs, Meta};
use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryState {
    Ours { path: PathBuf, target: PathBuf },
    OtherApp { path: PathBuf, target: PathBuf, executable: bool },
    Foreign { path: PathBuf, executable: bool },
    BrokenApp { path: PathBuf, target: PathBuf, volume_mounted: bool },
    BrokenForeign { path: PathBuf, target: PathBuf },
    Unreadable { path: PathBuf, error: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Ready, DevBuild, Translocated, ReadOnlyVolume,
    BundledCliMissing, BundledCliNotExecutable,
}

pub struct Observation {
    pub path_source: crate::cli_install::PathSource,
    pub hits: Vec<EntryState>,
    pub effective: Option<usize>,
    pub availability: Availability,
    pub bundled_cli: Option<PathBuf>,
    /// PATH 扫描中出现过非 ENOENT 错误（UI 视为 Unknown）。
    pub path_unknown: bool,
}

fn absent(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory)
}

fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => { out.pop(); }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub fn is_app_target(p: &Path) -> bool {
    if !p.is_absolute() { return false; }
    let c: Vec<_> = p.components().collect();
    if c.len() < 4 { return false; }
    let n = c.len();
    let app = c[n - 4].as_os_str().to_string_lossy();
    app.ends_with(".app")
        && c[n - 3].as_os_str().to_string_lossy().eq_ignore_ascii_case("Contents")
        && c[n - 2].as_os_str().to_string_lossy().eq_ignore_ascii_case("MacOS")
        && c[n - 1].as_os_str().to_string_lossy().eq_ignore_ascii_case("macEverything")
}

fn volume_mounted(fs: &dyn InstallerFs, target: &Path) -> bool {
    let c: Vec<_> = target.components().collect();
    if c.len() >= 3 && c[0] == Component::RootDir && c[1].as_os_str() == "Volumes" {
        let root = Path::new("/Volumes").join(c[2].as_os_str());
        return fs.stat(&root).is_ok();
    }
    true
}

pub fn classify_entry(
    fs: &dyn InstallerFs,
    path: &Path,
    bundle_cli: &Path,
) -> io::Result<Option<EntryState>> {
    let bundle = fs.stat(bundle_cli)?;
    let lm = match fs.lstat(path) {
        Ok(m) => m,
        Err(e) if absent(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
    if lm.is_symlink {
        let text = fs.read_link(path)?;
        let target = lexical(&if text.is_absolute() {
            text
        } else {
            path.parent().unwrap_or(Path::new("/")).join(text)
        });
        return match fs.stat(path) {
            Ok(m) => {
                if m.is_file && (m.dev, m.ino) == (bundle.dev, bundle.ino) {
                    Ok(Some(EntryState::Ours { path: path.into(), target }))
                } else if is_app_target(&target) {
                    Ok(Some(EntryState::OtherApp {
                        path: path.into(),
                        target,
                        executable: fs.access_x(path),
                    }))
                } else {
                    Ok(Some(EntryState::Foreign {
                        path: path.into(),
                        executable: m.is_file && fs.access_x(path),
                    }))
                }
            }
            Err(e) if absent(&e) => {
                if is_app_target(&target) {
                    Ok(Some(EntryState::BrokenApp {
                        path: path.into(),
                        target: target.clone(),
                        volume_mounted: volume_mounted(fs, &target),
                    }))
                } else {
                    Ok(Some(EntryState::BrokenForeign { path: path.into(), target }))
                }
            }
            Err(e) => Err(e),
        };
    }
    Ok(Some(EntryState::Foreign {
        path: path.into(),
        executable: lm.is_file && fs.access_x(path),
    }))
}

/// 返回 (hits, effective, has_unreadable)。
pub fn scan_path(
    fs: &dyn InstallerFs,
    path_var: &std::ffi::OsStr,
    bundle_cli: &Path,
) -> (Vec<EntryState>, Option<usize>, bool) {
    let mut hits = Vec::new();
    let mut effective = None;
    let mut unknown = false;
    for seg in std::env::split_paths(path_var) {
        if !seg.is_absolute() { continue; }
        let entry = seg.join("macEverything");
        match classify_entry(fs, &entry, bundle_cli) {
            Ok(None) => {}
            Ok(Some(state)) => {
                if effective.is_none()
                    && fs.stat(&entry).map(|m| m.is_file && fs.access_x(&entry)).unwrap_or(false)
                {
                    effective = Some(hits.len());
                }
                hits.push(state);
            }
            Err(e) => {
                unknown = true;
                hits.push(EntryState::Unreadable { path: entry, error: e.to_string() });
            }
        }
    }
    (hits, effective, unknown)
}

/// 从当前 App 可执行文件推导随包 CLI 与位置状态。
pub fn availability(fs: &dyn InstallerFs, exe: &Path) -> (Availability, Option<PathBuf>) {
    let Some(dir) = exe.parent() else { return (Availability::DevBuild, None) };
    let c: Vec<_> = dir.components().collect();
    if c.len() < 3 {
        return (Availability::DevBuild, None);
    }
    let n = c.len();
    let app = c[n - 3].as_os_str().to_string_lossy();
    let shape = app.ends_with(".app")
        && c[n - 2].as_os_str().to_string_lossy().eq_ignore_ascii_case("Contents")
        && c[n - 1].as_os_str().to_string_lossy().eq_ignore_ascii_case("MacOS");
    if !shape {
        return (Availability::DevBuild, None);
    }
    let cli = dir.join("macEverything");
    let m = match fs.lstat(&cli) {
        Ok(m) => m,
        Err(_) => return (Availability::BundledCliMissing, None),
    };
    if !m.is_file || !fs.access_x(&cli) {
        return (Availability::BundledCliNotExecutable, None);
    }
    if exe.components().any(|c| c.as_os_str() == "AppTranslocation") {
        return (Availability::Translocated, None);
    }
    if fs.statfs(dir).map(|s| s.read_only).unwrap_or(false) {
        return (Availability::ReadOnlyVolume, None);
    }
    (Availability::Ready, Some(cli))
}

/// 一次完整观测：真实 fs + 登录 shell PATH。
pub fn observe() -> Observation {
    let fs = crate::cli_install::RealFs::new();
    let exe = std::env::current_exe().unwrap_or_default();
    let (availability, bundled_cli) = availability(&fs, &exe);
    let (path_source, path_var) = crate::cli_install::path_env::probe_path_env(
        &crate::cli_install::path_env::RealRunner,
        std::env::var("PATH").ok().as_deref(),
    );
    let (hits, effective, path_unknown) = match (&bundled_cli, &path_var) {
        (Some(cli), Some(p)) => scan_path(&fs, p, cli),
        _ => (vec![], None, true),
    };
    Observation { path_source, hits, effective, availability, bundled_cli, path_unknown }
}
```

- [ ] **Step 4: 写动作测试**

先把 `FakeFs` 从 `classify::tests` 提到 `crates/app/src/cli_install/test_fs.rs`（`#[cfg(test)] pub(crate) mod test_fs;`），供 classify 与 actions 共用。

`actions.rs` 测试复用 `classify::tests::FakeFs`（把它提到 `#[cfg(test)] pub(crate)`）：

```rust
#[test]
fn install_is_idempotent() {
    let mut fs = FakeFs::new();
    fs.file("/A/app/Contents/MacOS/macEverything", 9, 9);
    fs.file("/bin/macEverything", 9, 9); // 已经是 Ours 的普通文件? 需要 symlink:
    // 用 link + 目标文件构造 Ours,再调用 install;FakeFs::symlink 记录调用次数,
    // 断言没有再次创建。
}

#[test]
fn repair_restores_old_link_on_create_failure() {
    // FakeFs 增加 fail_symlink=true;repair 后 old 目标仍存在。
}

#[test]
fn uninstall_does_not_delete_foreign() {
    let entry = EntryState::Foreign { path: "/bin/macEverything".into(), executable: true };
    assert!(uninstall(&FakeFs::new(), &entry, Path::new("/A/app/Contents/MacOS/macEverything")).is_err());
}

#[test]
fn manual_command_refuses_existing_entry() {
    let cmd = manual_install_command(Path::new("/opt/homebrew/bin"), Path::new("/A/My App.app/Contents/MacOS/macEverything"), false);
    assert!(cmd.contains("[ -e "));
    assert!(!cmd.contains("-sf"));
}
```

- [ ] **Step 5: 实现动作**

```rust
use crate::cli_install::classify::{classify_entry, EntryState};
use crate::cli_install::InstallerFs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum ActionError {
    NotAllowed,
    Io(io::Error),
    NonWritable(PathBuf),
}

fn tmp(path: &Path, tag: &str) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(".{name}.{tag}.{}", std::process::id()))
}

pub fn install(
    fs: &dyn InstallerFs,
    dst: &Path,
    target: &Path,
    bundle_cli: &Path,
) -> Result<(), ActionError> {
    match fs.symlink(target, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            match classify_entry(fs, dst, bundle_cli) {
                Ok(Some(EntryState::Ours { .. })) => Ok(()),
                _ => Err(ActionError::NotAllowed),
            }
        }
        Err(e) => Err(ActionError::Io(e)),
    }
}

pub fn repair(
    fs: &dyn InstallerFs,
    entry: &EntryState,
    new_target: &Path,
    bundle_cli: &Path,
) -> Result<(), ActionError> {
    let EntryState::BrokenApp { path, volume_mounted, .. } = entry else {
        return Err(ActionError::NotAllowed);
    };
    if !*volume_mounted { return Err(ActionError::NotAllowed); }
    let parent = path.parent().ok_or(ActionError::NotAllowed)?;
    if !fs.access_w(parent) { return Err(ActionError::NonWritable(parent.into())); }

    let old = tmp(path, "old");
    fs.rename(path, &old).map_err(ActionError::Io)?;
    let ok = matches!(classify_entry(fs, &old, bundle_cli), Ok(Some(EntryState::BrokenApp { .. })));
    if !ok {
        let _ = fs.rename(&old, path);
        return Err(ActionError::NotAllowed);
    }
    match fs.symlink(new_target, path) {
        Ok(()) => { let _ = fs.remove_file(&old); Ok(()) }
        Err(e) => {
            let _ = fs.rename(&old, path);
            Err(ActionError::Io(e))
        }
    }
}

pub fn uninstall(
    fs: &dyn InstallerFs,
    entry: &EntryState,
    bundle_cli: &Path,
) -> Result<(), ActionError> {
    let EntryState::Ours { path, .. } = entry else {
        return Err(ActionError::NotAllowed);
    };
    let parent = path.parent().ok_or(ActionError::NotAllowed)?;
    if !fs.access_w(parent) { return Err(ActionError::NonWritable(parent.into())); }

    let moved = tmp(path, "rm");
    fs.rename(path, &moved).map_err(ActionError::Io)?;
    if matches!(classify_entry(fs, &moved, bundle_cli), Ok(Some(EntryState::Ours { .. }))) {
        fs.remove_file(&moved).map_err(ActionError::Io)
    } else {
        let _ = fs.rename(&moved, path);
        Err(ActionError::NotAllowed)
    }
}

pub fn manual_install_command(dir: &Path, cli: &Path, sudo: bool) -> String {
    fn q(p: &Path) -> String {
        format!("'{}'", p.to_string_lossy().replace('\'', "'\\''"))
    }
    let dst = dir.join("macEverything");
    let (dst, cli) = (q(&dst), q(cli));
    let prefix = if sudo { "sudo " } else { "" };
    format!(
        "if [ -e {dst} ] || [ -L {dst} ]; then echo 'refusing: {dst} exists'; else {prefix}ln -s {cli} {dst}; fi"
    )
}
```

- [ ] **Step 6: 跑测试**

Run: `cargo test -p app cli_install`
Expected: PASS。

- [ ] **Step 7: 提交**

```bash
git add crates/app/src/cli_install
git commit -m "feat(installer): classify path entries and implement safe actions"
```

---

## Task 8: 安装器 UI 状态与后台探测

**Files:**
- Modify: `crates/app/src/ui/settings.rs`
- Modify: `crates/app/src/ui/settings_view.rs`
- Modify: `crates/app/src/i18n/catalog.rs`

- [ ] **Step 1: 写纯状态机测试**

`settings.rs` 测试：

```rust
#[cfg(test)]
mod cli_install_state_tests {
    use super::*;
    use crate::cli_install::{Availability, Observation, PathSource};

    fn obs() -> Observation {
        Observation {
            path_source: PathSource::LoginInteractive,
            hits: vec![],
            effective: None,
            availability: Availability::Ready,
            bundled_cli: Some("/A.app/Contents/MacOS/macEverything".into()),
            path_unknown: false,
        }
    }

    #[test]
    fn stale_generation_is_ignored() {
        let mut ui = CliInstallUi::default();
        let refresh = ui.begin_resolve();
        let action = ui.begin_action();
        ui.apply(CliInstallResult { generation: refresh, result: Ok(obs()), action_error: None });
        assert!(matches!(ui.phase, CliInstallPhase::Acting));
        ui.apply(CliInstallResult { generation: action, result: Ok(obs()), action_error: None });
        assert!(matches!(ui.phase, CliInstallPhase::Ready(_)));
        assert!(!ui.busy);
    }

    #[test]
    fn error_keeps_last_observation() {
        let mut ui = CliInstallUi::default();
        let g = ui.begin_resolve();
        ui.apply(CliInstallResult { generation: g, result: Ok(obs()), action_error: None });
        let g2 = ui.begin_resolve();
        ui.apply(CliInstallResult { generation: g2, result: Err("probe failed".into()), action_error: None });
        assert!(matches!(ui.phase, CliInstallPhase::Ready(_)));
        assert_eq!(ui.error.as_deref(), Some("probe failed"));
    }

    #[test]
    fn action_blocks_refresh_result() {
        let mut ui = CliInstallUi::default();
        let refresh = ui.begin_resolve();
        let action = ui.begin_action();
        ui.apply(CliInstallResult { generation: refresh, result: Ok(obs()), action_error: None });
        assert!(ui.busy);
        assert!(matches!(ui.phase, CliInstallPhase::Acting));
        ui.apply(CliInstallResult { generation: action, result: Ok(obs()), action_error: None });
        assert!(!ui.busy);
    }
}
```

- [ ] **Step 2: 实现纯状态机**

```rust
#[derive(Debug, Clone)]
pub(crate) enum CliInstallPhase {
    Idle,
    Resolving,
    Acting,
    Ready(crate::cli_install::Observation),
}

#[derive(Debug, Clone)]
pub(crate) struct CliInstallUi {
    pub phase: CliInstallPhase,
    pub error: Option<String>,
    pub busy: bool,
    pub generation: u64,
    pub last: Option<crate::cli_install::Observation>,
}

impl Default for CliInstallUi {
    fn default() -> Self {
        Self { phase: CliInstallPhase::Idle, error: None, busy: false, generation: 0, last: None }
    }
}

impl CliInstallUi {
    pub fn begin_resolve(&mut self) -> u64 {
        self.generation += 1;
        self.phase = CliInstallPhase::Resolving;
        self.busy = true;
        self.error = None;
        self.generation
    }
    pub fn begin_action(&mut self) -> u64 {
        self.generation += 1;
        self.phase = CliInstallPhase::Acting;
        self.busy = true;
        self.error = None;
        self.generation
    }
    pub fn apply(&mut self, r: CliInstallResult) {
        if r.generation < self.generation {
            return; // 旧刷新/旧动作结果丢弃
        }
        if let Some(e) = r.action_error {
            self.error = Some(e);
            self.busy = false;
            return;
        }
        match r.result {
            Ok(o) => {
                self.last = Some(o.clone());
                self.phase = CliInstallPhase::Ready(o);
                self.error = None;
            }
            Err(e) => {
                self.error = Some(e);
                self.phase = match &self.last {
                    Some(o) => CliInstallPhase::Ready(o.clone()),
                    None => CliInstallPhase::Idle,
                };
            }
        }
        self.busy = false;
    }
}

pub(crate) struct CliInstallResult {
    pub generation: u64,
    pub result: Result<crate::cli_install::Observation, String>,
    pub action_error: Option<String>,
}
```

`SettingsState` 加：

```rust
pub(crate) cli_install: CliInstallUi,
pub(crate) cli_rx: Option<Receiver<CliInstallResult>>,
pub(crate) cli_refresh_pending: bool,
```

- [ ] **Step 3: worker 与 generation**

仿照 `spawn_exclude`（`settings.rs:1032`）：锁内置 `busy/Resolving`，锁外 `std::thread::Builder::new().name("cli-install".into()).spawn`：

```rust
fn spawn_cli_install(ctx: &SharedSettings, shared: Arc<Mutex<SettingsState>>, action: CliInstallAction) -> bool {
    let (tx, rx) = mpsc::channel();
    let generation = {
        let mut s = shared.lock().unwrap();
        let g = match action {
            CliInstallAction::Refresh => s.cli_install.begin_resolve(),
            _ => s.cli_install.begin_action(),
        };
        s.cli_rx = Some(rx);
        g
    };
    let spawned = std::thread::Builder::new()
        .name("cli-install".into())
        .spawn(move || {
            let (result, action_error) = match action {
                CliInstallAction::Refresh => (Ok(crate::cli_install::classify::observe()), None),
                CliInstallAction::Install { dst, cli } => (
                    Ok(crate::cli_install::classify::observe()),
                    crate::cli_install::actions::install(&crate::cli_install::RealFs::new(), &dst, &cli, &cli)
                        .err().map(|e| format!("{e:?}")),
                ),
                CliInstallAction::Repair { entry, cli } => (
                    Ok(crate::cli_install::classify::observe()),
                    crate::cli_install::actions::repair(&crate::cli_install::RealFs::new(), &entry, &cli, &cli)
                        .err().map(|e| format!("{e:?}")),
                ),
                CliInstallAction::Uninstall { entry, cli } => (
                    Ok(crate::cli_install::classify::observe()),
                    crate::cli_install::actions::uninstall(&crate::cli_install::RealFs::new(), &entry, &cli)
                        .err().map(|e| format!("{e:?}")),
                ),
            };
            let _ = tx.send(CliInstallResult { generation, result, action_error });
            let _ = ctx; // ctx 仅用于与 spawn_exclude 相同的唤醒/上下文约定
        });
    if let Err(err) = spawned {
        log::warn!("命令行工具工作线程启动失败:{err}");
        let mut s = shared.lock().unwrap();
        s.cli_install.error = Some(err.to_string());
        s.cli_install.busy = false;
        s.cli_rx = None;
        return false;
    }
    true
}
```

`CliInstallAction` 定义在 `cli_install::actions` 或 `settings.rs`；动作携带 `PathBuf` / `EntryState` / 随包 CLI。`tick_cli_install` 从 `cli_rx.try_recv()` 取结果并调用 `apply`。

`spawn_cli_install` 里 `Install` 的 `cli` 参数既是 target 又是 bundle_cli，实际应从 `Observation.bundled_cli` 取；实现时把 `Install { dst, bundle_cli }` 分开，`target == bundle_cli`。修正为 `Install { dst, bundle_cli }`，安装 `symlink(bundle_cli, dst)`，再以 `bundle_cli` 复核。

- [ ] **Step 4: 设置行**

在 `draw_general_group` 的 `rows` 加一行 `RowKind::CliInstall`，绘制：
- `Ready(Absent)`：按钮「安装到 PATH」
- `Ready(Ours)`：路径 + 「卸载」
- `Ready(BrokenApp)`：旧目标 + 「修复为当前 App」（卷未挂载禁用）
- `Ready(其他)`：路径 + 原因，无按钮
- `Unavailable/Unknown`：原因 + 「重试」
- `busy`：按钮禁用 + 「检查中…」

`SettingsAction` 加 `CliInstallInstall` / `CliInstallRepair(PathBuf)` / `CliInstallUninstall(PathBuf)` / `CliInstallRefresh`，由 `main` 的 settings pass 调 worker。

- [ ] **Step 5: 文案与布局**

`catalog.rs` 加：

```rust
    row_cli_tool: "命令行工具" / "Command-Line Tool",
    cli_install_button: "安装到 PATH" / "Install to PATH",
    cli_uninstall_button: "卸载" / "Uninstall",
    cli_repair_button: "修复为当前 App" / "Repair for This App",
    cli_retry_button: "重试" / "Retry",
    cli_checking: "检查中…" / "Checking…",
    cli_state_absent: "未安装" / "Not installed",
    cli_state_ours: "已安装，指向本 App" / "Installed for this app",
    cli_state_shadowed: "另有指向本 App 的链接（被遮蔽）" / "Another link to this app is shadowed",
    cli_state_other_app: "指向另一个 macEverything，不覆盖" / "Points to another macEverything; not overwritten",
    cli_state_foreign: "其他来源，不管理" / "Managed by another source",
    cli_state_broken: "链接失效" / "Link is broken",
    cli_state_broken_volume: "目标卷未挂载" / "Target volume is not mounted",
    cli_state_unreadable: "无法读取：{err}" / "Cannot read: {err}",
    cli_unavailable_dev: "开发构建不提供安装" / "Not available in a development build",
    cli_unavailable_moved: "先移到 Applications 并重启 App" / "Move to Applications and relaunch the app",
    cli_unknown_path: "无法读取登录 shell 的 PATH" / "Couldn't read the login shell PATH",
    cli_fda_hint: "首次在终端运行可能需要完全磁盘访问权限" / "The first terminal run may need Full Disk Access",
```

`settings_view.rs` 删除 `max_before_about <= SETTINGS_H - CONTENT_PAD.0` 断言，保留分组顺序断言；更新 `settings_layout_has_all_rows` 的文案列表。

- [ ] **Step 6: 跑测试**

Run: `cargo test -p app settings cli_install`
Expected: PASS。

- [ ] **Step 7: 提交**

```bash
git add crates/app/src/ui/settings.rs crates/app/src/ui/settings_view.rs crates/app/src/i18n/catalog.rs
git commit -m "feat(ui): add command-line tool installer row"
```

---

## Task 9: 安装器实机验收

- [ ] `bash scripts/bundle.sh` 构建 `.app`。
- [ ] 安装到 `/Applications`，打开设置 → 通用 → 命令行工具。
- [ ] 安装：确认链接落在 PATH 中的允许目录；新开终端 `macEverything status` 可用。
- [ ] 重复点击安装：no-op，链接 inode 不变。
- [ ] 已有 cargo/Homebrew 安装：显示「其他来源」，不被覆盖。
- [ ] 把 App 移到废纸篓：重新打开设置，状态为失效；从新位置修复。
- [ ] 从 DMG 运行：安装禁用，提示移动到 Applications。
- [ ] 手工建立一个指向缺失 App 的链接：状态为断链；卷未挂载时修复禁用。
- [ ] 卸载：仅删除本 App 链接；cargo 安装的实体文件保留。
- [ ] 中英文界面逐项检查文案。

---

# Phase C：文档与发布

## Task 10: 文档与官网

**Files:**
- Modify: `docs/usage.md`
- Modify: `README.md` / `README.zh-CN.md`
- Modify: `docs/architecture.md` / `docs/development.md`
- Modify: `docs/specs/floatbar-visual/settings-visual.md`

- [ ] `docs/usage.md`：增加参数语法、`--json` 示例、`--desc` 绝对降序、`MCE_DATA_DIR`、退出码、`desc_echo`、`index_unreadable`、外部工具 示例、数据目录权限、设置页安装器说明、FDA 提示。
- [ ] `README.md` / `README.zh-CN.md`：从功能亮点移除「CLI + GUI」，在安装/使用章节保留 CLI 支持说明，补一句 `--json` 可供脚本与 AI/agent 工具调用。
- [ ] 官网中英文各加一句 CLI/JSON 支持说明（FAQ 或功能说明），不新增主视觉区块。
- [ ] `docs/architecture.md` / `docs/development.md`：补数据目录覆盖、App 固定 `MCE_DATA_DIR`、JSON 契约入口。
- [ ] `docs/specs/floatbar-visual/settings-visual.md`：删除首屏约束，改为固定 620×600 + 整页滚动。
- [ ] 提交：

```bash
git add docs README.md README.zh-CN.md site Cargo.toml
git commit -m "docs: document CLI JSON, data dir, and installer"
```

---

## Task 11: 三门禁与实机回归

- [ ] `cargo fmt --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `cargo test -p engine --test fsevents_live -- --ignored`
- [ ] CLI 实机：在线/离线 `query --json`、`status --json`、`--sort mtime`/`--desc`、独立 `MCE_DATA_DIR` 实例、`stop` 只停目标实例。
- [ ] App 实机：设置页安装/卸载/修复、DMG 拒绝、卷未挂载禁用、双语。
- [ ] `bash scripts/bench.sh /tmp/mce-bench 1000000`，确认查询与启动无回归。
- [ ] 按 `docs/development.md` 的实机冒烟脚本跑一遍 daemon 增量链路。

提交修复后：

```bash
git add -A
git commit -m "chore(release): v2.5.0"
```

---

## 自检清单

- [ ] 规格 §3 的 JSON 字段、`desc_echo`、错误码、退出码都有对应 Task/测试。
- [ ] 规格 §4 的数据目录优先级、权限、`start` 子进程、App launcher 都有对应 Task/测试。
- [ ] 规格 §5 的九种分类、`effective`/`hits`、动作矩阵、EACCES/ELOOP、translocation、卷未挂载、手动命令守卫都有测试。
- [ ] 规格 §8 的进程级测试、注入式测试、突变验证都已列出。
- [ ] 没有 TBD/TODO；所有类型名、函数名在前后任务中一致。
