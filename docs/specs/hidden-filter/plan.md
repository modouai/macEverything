# 隐藏项搜索过滤（hidden-filter）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 增加全局「包含隐藏项」开关：名称以 `.` 开头的文件/文件夹（及其子树）默认不进入搜索结果，切换即时生效、不重建索引。

**Architecture:** 查询层过滤。`QueryPlan.include_hidden` 由 daemon 配置或协议覆盖位决定；`IndexView::is_hidden` 沿父链判定（合成视图跨 anchor 桥接）。配置新增三态字段 `include_hidden: Option<bool>`，旧配置按「有落盘索引 = 升级用户 → 包含」迁移并写回。协议新增可选 Query 字段与 `HiddenSet`/`HiddenPrefs` 变体，不 bump 版本。

**Tech Stack:** Rust workspace（engine / protocol / client / cli / app），egui/eframe，serde/toml，Unix socket 长度前缀 JSON。

**Specs:** `docs/specs/hidden-filter/product.md`、`docs/specs/hidden-filter/tech.md`

---

## Task 0: 建工作分支

- [ ] **Step 1: 从 main 建功能分支**

```bash
git switch -c feat/hidden-filter
```

Expected: `Switched to a new branch 'feat/hidden-filter'`

- [ ] **Step 2: 确认工作区干净**

```bash
git status --short
```

Expected: 无输出。

---

## Task 1: engine 配置字段与升级迁移

**Files:**
- Modify: `crates/engine/src/config.rs:14-59`（`Config`、`default_config`、`load` 之后加迁移入口）
- Test: `crates/engine/src/config.rs` 的 `mod tests`

- [ ] **Step 1: 写失败测试**

在 `crates/engine/src/config.rs` 的 `mod tests` 末尾追加：

```rust
    /// 旧配置（缺字段）+ 已有落盘索引 = 升级用户：迁移为包含并写回。
    #[test]
    fn load_migrating_legacy_config_with_index_includes_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "excluded = [\"node_modules\"]\n").unwrap();
        let c = load_migrating(&p, true);
        assert_eq!(c.include_hidden, Some(true));
        assert_eq!(load(&p).include_hidden, Some(true), "迁移结果须写回");
    }

    /// 旧配置 + 无落盘索引 = 新装：迁移为不包含并写回。
    #[test]
    fn load_migrating_fresh_install_excludes_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let c = load_migrating(&p, false);
        assert_eq!(c.include_hidden, Some(false));
        assert_eq!(load(&p).include_hidden, Some(false));
    }

    /// 显式值不被迁移覆盖（用户已关掉→升级后仍关）。
    #[test]
    fn load_migrating_keeps_explicit_value() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "include_hidden = false\n").unwrap();
        assert_eq!(load_migrating(&p, true).include_hidden, Some(false));
        assert_eq!(load(&p).include_hidden, Some(false));
    }

    /// 纯读入口不产生副作用：缺文件时不创建配置。
    #[test]
    fn load_does_not_write_config() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let c = load(&p);
        assert_eq!(c.include_hidden, None);
        assert!(!p.exists(), "load 不得写盘");
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p engine config::tests
```

Expected: 编译失败（`Config` 无 `include_hidden` 字段、`load_migrating` 未定义）。

- [ ] **Step 3: 实现字段、生效值与迁移入口**

`Config` 增加字段（放在 `volumes_excluded` 之后）：

```rust
    /// 搜索结果是否包含隐藏项（名称以 `.` 开头）。
    /// None = 旧配置缺字段，等待 [`load_migrating`] 迁移。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_hidden: Option<bool>,
```

`default_config()` 增加：

```rust
        include_hidden: None,
```

在 `Config` 定义之后加：

```rust
impl Config {
    /// 生效值；未迁移的纯内存路径将 None 视为不包含。
    pub fn include_hidden_effective(&self) -> bool {
        self.include_hidden.unwrap_or(false)
    }
}
```

在 `load` 之后加：

```rust
/// 读配置并完成旧配置迁移。
/// `index_present` = 数据目录已有落盘索引（升级信号）：
/// 字段缺失时写入 `Some(index_present)` 并原子落盘；写失败只告警，内存仍按迁移值。
pub fn load_migrating(path: &Path, index_present: bool) -> Config {
    let mut cfg = load(path);
    if cfg.include_hidden.is_none() {
        cfg.include_hidden = Some(index_present);
        if let Err(e) = save(&cfg, path) {
            log::warn!(
                "{} 隐藏项偏好迁移写盘失败（内存仍生效）: {e}",
                path.display()
            );
        }
    }
    cfg
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p engine config::tests
```

Expected: 全部通过（含既有 `roundtrip_and_defaults`、`corrupt_file_falls_back_to_default`）。

- [ ] **Step 5: 突变验证**

临时把 `load_migrating` 里的 `Some(index_present)` 改成 `Some(false)`，运行：

```bash
cargo test -p engine config::tests::load_migrating_legacy_config_with_index_includes_hidden
```

Expected: FAIL（迁移未按升级信号取 true）。改回后再次运行应为 PASS。

- [ ] **Step 6: 提交**

```bash
git add crates/engine/src/config.rs
git commit -m "feat(engine): add include_hidden config with legacy migration"
```

---

## Task 2: engine 查询层隐藏过滤

**Files:**
- Modify: `crates/engine/src/index.rs:498-568`（`IndexView` trait）
- Modify: `crates/engine/src/query.rs:30-48`（`QueryPlan`）、`parse`、`execute` 候选闭包
- Modify: `crates/engine/src/overlay.rs:568-700`（`CompositeView` 实现）
- Test: 三个文件各自的 `mod tests`

- [ ] **Step 1: 写 index 默认判定失败测试**

在 `crates/engine/src/index.rs` 的 `mod tests` 末尾追加：

```rust
    /// 隐藏判定：自身或祖先以 `.` 开头；根不判隐藏。
    #[test]
    fn is_hidden_checks_self_and_ancestors_not_root() {
        let mut i = ix();
        let root = i.insert("/tmp/root", NO_PARENT, true);
        let hidden = i.insert(".config", root, true);
        let nested = i.insert("init.lua", hidden, false);
        let visible = i.insert("notes.txt", root, false);
        assert!(!i.is_hidden(root), "根条目不参与隐藏判定");
        assert!(i.is_hidden(hidden));
        assert!(i.is_hidden(nested), "隐藏目录内的可见文件随祖先隐藏");
        assert!(!i.is_hidden(visible));
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p engine index::tests::is_hidden_checks_self_and_ancestors_not_root
```

Expected: 编译失败（方法不存在）。

- [ ] **Step 3: 实现 `IndexView::is_hidden` 默认实现**

在 `IndexView` trait 的 `display_name` 之后加：

```rust
    /// 隐藏项判定（查询层过滤）：条目自身或任一祖先名称以 `.` 开头。
    /// 根条目名为绝对路径（首字节 `/`），天然不判隐藏；父链到 NO_PARENT 止。
    /// 合成视图覆写此方法以跨 anchor 桥接。
    fn is_hidden(&self, idx: u32) -> bool {
        let mut cur = idx;
        loop {
            if self.display_name(cur).first() == Some(&b'.') {
                return true;
            }
            let p = self.parent_idx(cur);
            if p == NO_PARENT {
                return false;
            }
            cur = p;
        }
    }
```

- [ ] **Step 4: 写查询过滤失败测试**

在 `crates/engine/src/query.rs` 的 `mod tests` 末尾追加：

```rust
    /// 隐藏过滤：自身或祖先隐藏 → 候选层丢弃，total 同口径。
    /// 突变「删除 execute 中的 is_hidden 判定」→ 此测必红。
    #[test]
    fn hidden_entries_filtered_out_unless_include_hidden() {
        let mut i = Index::new();
        let root = i.insert("/tmp/root", NO_PARENT, true);
        let hidden_dir = i.insert(".config", root, true);
        i.insert("init.lua", hidden_dir, false);
        i.insert(".zshrc", root, false);
        i.insert("notes.txt", root, false);

        let mut plan = parse("*").unwrap();
        assert_eq!(execute(&i, &plan, 100).total, 5, "parse 默认不过滤");

        plan.include_hidden = false;
        let out = execute(&i, &plan, 100);
        assert_eq!(out.total, 2, "仅根与 notes.txt 可见");
        let mut names: Vec<String> = out
            .hits
            .iter()
            .map(|&h| String::from_utf8_lossy(i.display_name(h)).into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["/tmp/root".to_string(), "notes.txt".to_string()]
        );
    }
```

- [ ] **Step 5: 跑测试确认失败**

```bash
cargo test -p engine query::tests::hidden_entries_filtered_out_unless_include_hidden
```

Expected: 编译失败（`QueryPlan` 无 `include_hidden`）。

- [ ] **Step 6: 实现 QueryPlan 字段与候选过滤**

`QueryPlan` 增加：

```rust
    /// 搜索结果是否包含隐藏项（名称以 `.` 开头）。parse 恒 true（旧调用方行为不变）；
    /// daemon/client 按配置与协议位覆盖。
    pub include_hidden: bool,
```

`parse` 的 `QueryPlan { ... }` 初始化增加：

```rust
        include_hidden: true,
```

`execute` 的候选闭包，在 `whole_word` 判定之后、`Some((...))` 之前插入：

```rust
                // 隐藏过滤放在匹配之后：只对真正命中的候选做父链判定，
                // 避免百万条目逐条上溯。total 在 rows 上取，天然同口径。
                if !plan.include_hidden && view.is_hidden(idx) {
                    return None;
                }
```

- [ ] **Step 7: 写合成视图跨 anchor 失败测试**

在 `crates/engine/src/overlay.rs` 的 `mod tests` 末尾追加：

```rust
    /// 隐藏判定必须穿过桥接 anchor：`.config/foo` 下增量新增的文件，
    /// overlay 父链在 anchor(foo) 处 NO_PARENT，需经 anchor_base 切回 base
    /// 父链才能看到祖先 `.config`。突变「删除 CompositeView::is_hidden 覆写」
    /// → 此测必红。
    #[test]
    fn composite_view_hidden_walks_through_anchor_to_base() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        let foo = root.join(".config/foo");
        std::fs::create_dir_all(&foo).unwrap();
        let base = scan(root);
        let foo_id = id_of(&base, &foo);

        std::fs::write(foo.join("new.txt"), "").unwrap();
        let mut ov = Overlay::new();
        ov.apply_diff(&base, DiffTarget::Base(foo_id), &foo, &[])
            .unwrap();
        let view = ov.composite_view(&base);
        let out = query::execute(&view, &query::parse("new.txt").unwrap(), 10);
        assert_eq!(out.total, 1, "增量新增文件应可见（本测只钉隐藏判定）");
        assert!(
            view.is_hidden(out.hits[0]),
            "overlay 条目应经 anchor 链判为隐藏"
        );
    }
```

- [ ] **Step 8: 跑测试确认失败**

```bash
cargo test -p engine overlay::tests::composite_view_hidden_walks_through_anchor_to_base
```

Expected: FAIL（默认实现到 anchor 即 `NO_PARENT` 返回 false）。

- [ ] **Step 9: 实现 CompositeView 覆写**

在 `impl IndexView for CompositeView<'_>` 内、`entry_valid` 之后加：

```rust
    /// 隐藏判定跨 anchor：overlay 条目沿 overlay 父链上溯，到桥接 anchor 后经
    /// `anchor_base` 切回 base 父链；缺此覆写时 `.config/foo` 下增量新增的文件
    /// 会在 anchor(foo) 处截断，误判为可见。
    fn is_hidden(&self, idx: u32) -> bool {
        if idx < self.base_count {
            return self.base.is_hidden(idx);
        }
        let mut cur = idx - self.base_count;
        loop {
            if self.overlay.index.display_name_of(cur).first() == Some(&b'.') {
                return true;
            }
            let e = &self.overlay.index.entries[cur as usize];
            if e.parent == NO_PARENT {
                let Some(&b) = self.overlay.anchor_base.get(&cur) else {
                    return false;
                };
                let p = self.base.entry(b).parent;
                return p != NO_PARENT && self.base.is_hidden(p);
            }
            cur = e.parent;
        }
    }
```

- [ ] **Step 10: 跑 engine 全量测试**

```bash
cargo test -p engine
```

Expected: 全部通过。

- [ ] **Step 11: 突变验证**

1. 删除 `execute` 中的 `is_hidden` 判定 → `cargo test -p engine query::tests::hidden_entries_filtered_out_unless_include_hidden` 必红；恢复。
2. 删除 `CompositeView::is_hidden` 覆写（改回默认）→ `cargo test -p engine overlay::tests::composite_view_hidden_walks_through_anchor_to_base` 必红；恢复。

- [ ] **Step 12: 提交**

```bash
git add crates/engine/src/index.rs crates/engine/src/query.rs crates/engine/src/overlay.rs
git commit -m "feat(engine): filter hidden entries at query layer"
```

---

## Task 3: protocol 字段与消息变体

**Files:**
- Modify: `crates/protocol/src/lib.rs:14-92`（`Request::Query`、新变体、`version()`）
- Modify: `crates/protocol/src/lib.rs:92-160`（`Response`）
- Test: `crates/protocol/src/lib.rs` 的 `mod tests`

- [ ] **Step 1: 写失败测试**

在 `crates/protocol/src/lib.rs` 的 `mod tests` 末尾追加：

```rust
    /// Query.include_hidden 三态：缺省 None；Some 才上线；往返保留。
    #[test]
    fn query_include_hidden_is_optional() {
        let old = br#"{"msg":"query","v":1,"query":"a","limit":10}"#;
        let r: Request = serde_json::from_slice(old).unwrap();
        assert!(matches!(r, Request::Query { include_hidden: None, .. }));

        let r = Request::Query {
            v: PROTOCOL_VERSION,
            query: "a".into(),
            limit: 10,
            sort: None,
            desc: None,
            dir_only: None,
            include_hidden: Some(true),
        };
        let bytes = encode(&r).unwrap();
        let body = String::from_utf8(bytes[4..].to_vec()).unwrap();
        assert!(body.contains(r#""include_hidden":true"#));
        assert_eq!(decode_request(&bytes[4..]).unwrap(), r);

        let none = Request::Query {
            v: PROTOCOL_VERSION,
            query: "a".into(),
            limit: 10,
            sort: None,
            desc: None,
            dir_only: None,
            include_hidden: None,
        };
        let body = String::from_utf8(encode(&none).unwrap()[4..].to_vec()).unwrap();
        assert!(!body.contains("include_hidden"), "None 不得上线: {body}");
    }

    /// 隐藏项偏好消息编解码。
    #[test]
    fn hidden_prefs_roundtrip() {
        let set = Request::HiddenSet {
            v: PROTOCOL_VERSION,
            include: true,
        };
        assert_eq!(
            decode_request(&encode(&set).unwrap()[4..]).unwrap(),
            set
        );
        let prefs = Request::HiddenPrefs {
            v: PROTOCOL_VERSION,
        };
        assert_eq!(
            decode_request(&encode(&prefs).unwrap()[4..]).unwrap(),
            prefs
        );
        let resp = Response::HiddenPrefs {
            include: false,
            message: Some("x".into()),
        };
        assert_eq!(
            decode_response(&encode(&resp).unwrap()[4..]).unwrap(),
            resp
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p protocol
```

Expected: 编译失败（新字段/新变体不存在）。

- [ ] **Step 3: 实现协议变更**

`Request::Query` 增加最后一个字段：

```rust
        /// None = 跟随 daemon 配置；Some = 单次覆盖（CLI --include-hidden）。
        /// 同 sort/desc：纯新增可选字段不 bump v，缺省不发送。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        include_hidden: Option<bool>,
```

`Request` 在 `VolumesPrefs` 之后加：

```rust
    /// 隐藏项偏好写（设置页）：daemon 更新配置并落盘，立即影响后续查询。
    HiddenSet {
        v: u32,
        include: bool,
    },
    /// 隐藏项偏好回显。
    HiddenPrefs {
        v: u32,
    },
```

`Request::version()` 的 match 增加：

```rust
            | Request::HiddenSet { v, .. }
            | Request::HiddenPrefs { v } => *v,
```

`Response` 在 `VolumesPrefs` 之后加：

```rust
    /// 隐藏项偏好回显（HiddenSet 的应答与 HiddenPrefs 的读应答共用）。
    HiddenPrefs {
        include: bool,
        /// 失败原因（落盘失败等）；成功时缺省不序列化。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
```

- [ ] **Step 4: 修正既有 `Request::Query` 字面量**

在以下位置补 `include_hidden: None,`（保持既有测试语义）：

- `crates/protocol/src/lib.rs`：`request_roundtrip`、`query_sort_field_backward_compatible` 之后的排序测试、`dir_only` 相关测试（搜索 `Request::Query {` 逐个补齐）。
- `crates/cli/src/daemon.rs:3717` 的测试字面量。
- `crates/client/src/lib.rs:212` 的生产构造在 Task 4 改为透传参数。

- [ ] **Step 5: 跑 protocol 测试**

```bash
cargo test -p protocol
```

Expected: 全部通过。

- [ ] **Step 6: 提交**

```bash
git add crates/protocol/src/lib.rs
git commit -m "feat(protocol): add include_hidden query field and hidden prefs messages"
```

---

## Task 4: client 查询参数、离线配置与偏好 API

**Files:**
- Modify: `crates/client/src/lib.rs:196-295`（`query` / `offline_query`）
- Modify: `crates/client/src/lib.rs`（新增 hidden 偏好 API，放在 volumes API 之后）
- Test: `crates/client/src/lib.rs` 的 `mod tests`

- [ ] **Step 1: 写失败测试**

在 `crates/client/src/lib.rs` 的 `mod tests` 末尾追加：

```rust
    /// include_hidden 线路三态：None 不发送；Some 原样上线。
    #[test]
    fn query_sends_include_hidden_only_when_some() {
        for (flag, want) in [
            (Some(true), Some(r#""include_hidden":true"#)),
            (Some(false), Some(r#""include_hidden":false"#)),
            (None, None),
        ] {
            let (paths, _h, captured) = stub_daemon_capturing(
                br#"{"msg":"query","entries":[],"total":0,"elapsed_ms":0.0,"stat_ms":0.0,"sort_echo":"relevance","desc_echo":null}"#,
            );
            query(&paths, "x", 10, None, false, false, flag).unwrap();
            let body = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
            match want {
                Some(needle) => assert!(body.contains(needle), "请求体: {body}"),
                None => assert!(!body.contains("include_hidden"), "请求体: {body}"),
            }
        }
    }

    /// 离线默认值：显式覆盖优先；否则读 config.toml；旧配置 + 有索引 → 迁移为包含。
    #[test]
    fn offline_include_hidden_reads_config_and_applies_override() {
        let dir = tempfile::tempdir().unwrap();
        let paths = DaemonPaths {
            data_dir: dir.path().to_path_buf(),
        };
        std::fs::write(paths.config_file(), "include_hidden = false\n").unwrap();
        assert!(!offline_include_hidden(&paths, None));
        assert!(offline_include_hidden(&paths, Some(true)));

        let dir2 = tempfile::tempdir().unwrap();
        let paths2 = DaemonPaths {
            data_dir: dir2.path().to_path_buf(),
        };
        std::fs::write(paths2.config_file(), "excluded = []\n").unwrap();
        std::fs::write(paths2.index_file(), b"").unwrap();
        assert!(
            offline_include_hidden(&paths2, None),
            "旧配置 + 已有索引应迁移为包含"
        );
        assert_eq!(
            engine::config::load(&paths2.config_file()).include_hidden,
            Some(true),
            "迁移须写回"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p client
```

Expected: 编译失败（`query` 参数数量不符、`offline_include_hidden` 未定义）。

- [ ] **Step 3: 实现 `query` 参数与离线解析**

`query` 签名增加最后一个参数：

```rust
pub fn query(
    paths: &DaemonPaths,
    q: &str,
    limit: u32,
    sort: Option<&str>,
    desc: bool,
    dir_only: bool,
    include_hidden: Option<bool>,
) -> Result<QueryOutcome, QueryError> {
```

在线请求构造增加：

```rust
                include_hidden,
```

离线分支改为：

```rust
        offline_query(
            paths,
            q,
            limit,
            dir_only,
            offline_include_hidden(paths, include_hidden),
        )
        .map_err(no_code)
```

在 `query` 之前加纯函数：

```rust
/// 离线查询的隐藏项默认值：请求级覆盖优先；否则读 config.toml
/// （经迁移入口，旧配置 + 已有索引按升级用户处理）。
fn offline_include_hidden(paths: &DaemonPaths, include_hidden: Option<bool>) -> bool {
    include_hidden.unwrap_or_else(|| {
        engine::config::load_migrating(&paths.config_file(), paths.index_file().exists())
            .include_hidden_effective()
    })
}
```

`offline_query` 签名与计划赋值：

```rust
pub fn offline_query(
    paths: &DaemonPaths,
    q: &str,
    limit: u32,
    dir_only: bool,
    include_hidden: bool,
) -> Result<QueryOutcome> {
    // ...
    plan.dir_only = dir_only;
    plan.include_hidden = include_hidden;
```

- [ ] **Step 4: 实现隐藏项偏好 API**

在 `set_volume_prefs` 之后加：

```rust
/// 隐藏项偏好结果：message 为落盘失败等可操作提示（成功为 None）。
#[derive(Debug)]
pub struct HiddenPrefsOutcome {
    pub include: bool,
    pub message: Option<String>,
}

/// 隐藏项偏好读/写共用管道；旧 daemon 不认识变体会断连，调用方按失败展示。
fn hidden_call(paths: &DaemonPaths, req: Request) -> Result<HiddenPrefsOutcome> {
    let Some(mut s) = connect(paths) else {
        bail!("daemon 未在运行；隐藏项偏好需引擎在线，先执行 macEverything start");
    };
    let resp = call(&mut s, &req).map_err(|err| {
        anyhow!(
            "{err:#};若引擎为旧版本，请重启引擎:macEverything stop && macEverything start（或托盘拉起）"
        )
    })?;
    match expect_ok(resp)? {
        Response::HiddenPrefs { include, message } => Ok(HiddenPrefsOutcome { include, message }),
        other => bail!("意外响应: {other:?}"),
    }
}

/// 读当前隐藏项偏好（设置页回显）。
pub fn hidden_prefs(paths: &DaemonPaths) -> Result<HiddenPrefsOutcome> {
    hidden_call(
        paths,
        Request::HiddenPrefs {
            v: protocol::PROTOCOL_VERSION,
        },
    )
}

/// 写隐藏项偏好；daemon 更新内存配置并落盘，后续查询立即生效（无需重扫）。
pub fn set_hidden_prefs(paths: &DaemonPaths, include: bool) -> Result<HiddenPrefsOutcome> {
    hidden_call(
        paths,
        Request::HiddenSet {
            v: protocol::PROTOCOL_VERSION,
            include,
        },
    )
}
```

- [ ] **Step 5: 修正既有 `query` 调用**

```bash
rg -n "query\(&paths|offline_query\(" crates/client/src crates/app/src crates/cli/src
```

为每个调用补最后一个参数：

- 既有 client 测试：`query(&paths, "x", 10, Some("mtime"), true, false).unwrap()` → 末尾加 `None`；同理 `dir_only` 测试。
- `crates/app/src/ui/palette.rs:1522`、`:1572` → 末尾加 `None`。
- `crates/cli/src/main.rs:158` → Task 6 改为 `include_hidden.then_some(true)`。

- [ ] **Step 6: 跑 client 测试**

```bash
cargo test -p client
```

Expected: 全部通过。

- [ ] **Step 7: 突变验证**

把 `offline_include_hidden` 的 `include_hidden.unwrap_or_else(...)` 改成忽略参数、恒 `false`，运行：

```bash
cargo test -p client tests::offline_include_hidden_reads_config_and_applies_override
```

Expected: FAIL（显式覆盖/迁移断言红）。恢复。

- [ ] **Step 8: 提交**

```bash
git add crates/client/src/lib.rs crates/app/src/ui/palette.rs
git commit -m "feat(client): forward include_hidden and add hidden prefs API"
```

---

## Task 5: daemon 配置迁移、查询覆盖与偏好消息

**Files:**
- Modify: `crates/cli/src/daemon.rs:684-692`（`EngineState::new` 字面量）
- Modify: `crates/cli/src/daemon.rs:763-830`（`run_query`）
- Modify: `crates/cli/src/daemon.rs:2438`（`cold_start`）
- Modify: `crates/cli/src/daemon.rs:2734-2791`（dispatch）
- Modify: `crates/cli/src/daemon.rs:2878-2900`（`reset_excludes`）
- Modify: `crates/cli/src/daemon.rs:3768`（测试夹具字面量）
- Test: `crates/cli/src/daemon.rs` 的 `mod tests`

- [ ] **Step 1: 写失败测试**

在 `crates/cli/src/daemon.rs` 的 `mod tests` 末尾追加：

```rust
    /// 查询遵循 daemon 配置，请求级 Some 可覆盖；total 同口径。
    #[test]
    fn run_query_honors_hidden_config_and_override() {
        let mut idx = Index::new();
        let root = idx.insert("/tmp/r", NO_PARENT, true);
        idx.insert(".hidden.txt", root, false);
        idx.insert("visible.txt", root, false);
        let state = Arc::new(EngineState::new());
        *state.holder.write_fail_fast("holder") = IndexHolder::Mutable(idx);
        state.cfg.lock_recover().include_hidden = Some(false);

        let total = |r: Response| match r {
            Response::Query { total, .. } => total,
            other => panic!("意外响应: {other:?}"),
        };
        assert_eq!(
            total(run_query(&state, "*", 10, None, false, false, None)),
            2,
            "配置 false：根 + visible.txt"
        );
        assert_eq!(
            total(run_query(&state, "*", 10, None, false, false, Some(true))),
            3,
            "请求级 true 覆盖配置"
        );
    }

    /// HiddenSet 更新内存 + 落盘；HiddenPrefs 读回显。
    #[test]
    fn hidden_set_persists_and_prefs_echoes() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = DaemonPaths {
            data_dir: tmp.path().join("data"),
        };
        let state = Arc::new(EngineState::new());
        let resp = dispatch(
            Request::HiddenSet {
                v: protocol::PROTOCOL_VERSION,
                include: false,
            },
            &state,
            &paths,
        );
        match resp {
            Response::HiddenPrefs { include, message } => {
                assert!(!include);
                assert!(message.is_none());
            }
            other => panic!("意外响应: {other:?}"),
        }
        assert_eq!(state.cfg.lock_recover().include_hidden, Some(false));
        assert_eq!(
            config::load(&paths.config_file()).include_hidden,
            Some(false),
            "偏好须落盘"
        );
        match dispatch(
            Request::HiddenPrefs {
                v: protocol::PROTOCOL_VERSION,
            },
            &state,
            &paths,
        ) {
            Response::HiddenPrefs { include, .. } => assert!(!include),
            other => panic!("意外响应: {other:?}"),
        }
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p cli run_query_honors_hidden_config_and_override
```

Expected: 编译失败（`run_query` 参数数量不符、`include_hidden` 字段不存在、dispatch 无新变体）。

- [ ] **Step 3: 实现配置字面量与 `run_query`**

`EngineState::new` 的 `config::Config` 字面量增加：

```rust
                include_hidden: None,
```

`exclude_fixture` 的 `config::Config` 字面量增加：

```rust
            include_hidden: None,
```

`run_query` 签名增加最后一个参数：

```rust
fn run_query(
    state: &EngineState,
    q: &str,
    limit: u32,
    sort: Option<&str>,
    desc: bool,
    dir_only: bool,
    include_hidden: Option<bool>,
) -> Response {
```

在 `plan.dir_only = dir_only;` 之后加：

```rust
    // 请求级覆盖优先；None 跟随 daemon 配置。配置读取只持锁一瞬，
    // 不跨 with_view（查询读锁）。
    let include_hidden = include_hidden
        .unwrap_or_else(|| state.cfg.lock_recover().include_hidden_effective());
    plan.include_hidden = include_hidden;
```

- [ ] **Step 4: 修正所有 `run_query` 调用**

在 dispatch 与既有测试调用末尾补 `None`：

```bash
rg -n "run_query\(" crates/cli/src/daemon.rs
```

已知位置（行号会漂移，按内容定位）：dispatch 的 `Request::Query` 分支传入 `include_hidden`；测试中的
`run_query(&state, "a.txt", 10, None, false, false)` 等全部补 `, None`。新增测试中的 `Some(true)` 保留。

- [ ] **Step 5: 实现 cold_start 迁移与 dispatch 新变体**

`cold_start` 开头改为：

```rust
    let cfg = config::load_migrating(&paths.config_file(), paths.index_file().exists());
```

dispatch 的 `Request::Query` 匹配增加 `include_hidden,` 并传入 `run_query(..., include_hidden)`；在 `VolumesPrefs` 分支之后加：

```rust
        Request::HiddenSet { include, .. } => hidden_set(state, paths, include),
        Request::HiddenPrefs { .. } => hidden_prefs_response(state),
```

在 `volumes_prefs_response` 之后加：

```rust
/// 隐藏项偏好写入：更新内存 + 落盘；保存失败仍临时生效并如实告知。
fn hidden_set(state: &Arc<EngineState>, paths: &DaemonPaths, include: bool) -> Response {
    let (include_now, message) = {
        let mut c = state.cfg.lock_recover();
        c.include_hidden = Some(include);
        match config::save(&c, &paths.config_file()) {
            Ok(()) => (include, None),
            Err(e) => {
                log::error!("隐藏项偏好落盘失败: {e}");
                (include, Some(format!("已临时生效但写入配置失败: {e}")))
            }
        }
    };
    log::info!("隐藏项偏好更新:include={include_now}");
    Response::HiddenPrefs {
        include: include_now,
        message,
    }
}

/// 隐藏项偏好读（设置页回显）。
fn hidden_prefs_response(state: &Arc<EngineState>) -> Response {
    Response::HiddenPrefs {
        include: state.cfg.lock_recover().include_hidden_effective(),
        message: None,
    }
}
```

- [ ] **Step 6: 保留 `reset_excludes` 的开关值**

`reset_excludes` 中把：

```rust
    let defaults = config::default_config();
```

改为：

```rust
    let current_hidden = state.cfg.lock_recover().include_hidden;
    let mut defaults = config::default_config();
    defaults.include_hidden = current_hidden;
```

并在既有测试 `exclude_reset_restores_defaults_and_rescans_with_new_excludes` 里补断言。测试开头设置：

```rust
        state.cfg.lock_recover().include_hidden = Some(true);
        config::save(&state.cfg.lock_recover(), &paths.config_file()).unwrap();
```

reset 成功后追加：

```rust
        assert_eq!(
            state.cfg.lock_recover().include_hidden,
            Some(true),
            "排除项重置不得清掉隐藏项开关"
        );
        assert_eq!(
            config::load(&paths.config_file()).include_hidden,
            Some(true),
            "隐藏项开关须随重置一起保留到落盘配置"
        );
```

- [ ] **Step 7: 跑 daemon 测试**

```bash
cargo test -p cli
```

Expected: 全部通过（如遇未补参数的 `run_query`/`Config` 字面量，逐处补 `None`）。

- [ ] **Step 8: 突变验证**

1. 把 `plan.include_hidden = include_hidden;` 改成 `plan.include_hidden = true;` → `cargo test -p cli run_query_honors_hidden_config_and_override` 必红；恢复。
2. 删除 `reset_excludes` 里的 `defaults.include_hidden = current_hidden;` → reset 测试必红；恢复。

- [ ] **Step 9: 提交**

```bash
git add crates/cli/src/daemon.rs
git commit -m "feat(daemon): apply hidden filtering and hidden prefs protocol"
```

---

## Task 6: CLI `--include-hidden`

**Files:**
- Modify: `crates/cli/src/args.rs:10-25`（USAGE）、`Command::Query`、`parse_query`、测试字面量
- Modify: `crates/cli/src/main.rs:149-160`
- Test: `crates/cli/src/args.rs`

- [ ] **Step 1: 写失败测试**

在 `crates/cli/src/args.rs` 的 `mod tests` 末尾追加：

```rust
    /// --include-hidden 解析为 true；缺省 false；可与排序选项共存。
    #[test]
    fn query_include_hidden_flag() {
        let got = parse(a(&["query", "--include-hidden", "x"])).unwrap();
        assert_eq!(
            got.command,
            Command::Query {
                json: false,
                sort: None,
                desc: false,
                include_hidden: true,
                query: "x".into(),
            }
        );
        let got = parse(a(&["query", "x"])).unwrap();
        match got.command {
            Command::Query { include_hidden, .. } => assert!(!include_hidden),
            other => panic!("意外命令: {other:?}"),
        }
        assert!(parse(a(&["query", "--include-hidden", "--sort", "size", "x"])).is_ok());
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p cli args::tests::query_include_hidden_flag
```

Expected: 编译失败（`Command::Query` 无字段）。

- [ ] **Step 3: 实现解析与用法文案**

USAGE 的 query 行改为：

```text
  macEverything [--data-dir <PATH>] query [--json] [--sort <name|size|mtime|path>] [--desc] [--include-hidden] [--] <关键词>
```

`Command::Query` 增加：

```rust
        include_hidden: bool,
```

`parse_query` 增加：

```rust
    let mut include_hidden = false;
```

match 分支增加：

```rust
            "--include-hidden" => include_hidden = true,
```

返回处增加：

```rust
        include_hidden,
```

- [ ] **Step 4: 修正既有测试字面量**

`crates/cli/src/args.rs` 中所有 `Command::Query { ... }` 字面量补 `include_hidden: false,`（搜索 `Command::Query {`）。

- [ ] **Step 5: 接入 main**

`crates/cli/src/main.rs` 的 `Command::Query` 解构增加 `include_hidden`，调用改为：

```rust
            match client::query(
                &paths,
                &query,
                1000,
                sort_str,
                protocol_desc,
                false,
                include_hidden.then_some(true),
            ) {
```

- [ ] **Step 6: 跑 CLI 测试与用法冒烟**

```bash
cargo test -p cli
cargo run -p cli -- query --help 2>&1 | head -20
```

Expected: 测试通过；用法行含 `--include-hidden`。

- [ ] **Step 7: 提交**

```bash
git add crates/cli/src/args.rs crates/cli/src/main.rs
git commit -m "feat(cli): add --include-hidden query flag"
```

---

## Task 7: app 查询会话重发

**Files:**
- Modify: `crates/app/src/session.rs:72-95`（`Session`）
- Test: `crates/app/src/session.rs` 的 `mod tests`

- [ ] **Step 1: 写失败测试**

在 `crates/app/src/session.rs` 的 `mod tests` 末尾追加：

```rust
    /// 隐藏项开关变化：同串重发、不改变排序；空会话不动。
    #[test]
    fn hidden_change_refires_same_query_without_touching_sort() {
        let base = Instant::now();
        let mut s = Session::new(Duration::from_millis(60));
        s.on_input("报告", base);
        let first = s
            .pending_at(base + Duration::from_millis(70))
            .expect("防抖到期应发起");
        assert_eq!(first.query, "报告");
        assert_eq!(first.sort, SortKey::Relevance);

        s.on_hidden_change(base + Duration::from_millis(200));
        let again = s
            .pending_at(base + Duration::from_millis(300))
            .expect("开关变化应重发");
        assert_eq!(again.query, "报告");
        assert_eq!(again.sort, SortKey::Relevance);
        assert_eq!(again.desc, first.desc);

        let mut empty = Session::new(Duration::from_millis(60));
        empty.on_hidden_change(base);
        assert!(empty.pending_at(base + Duration::from_millis(100)).is_none());
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p app session::tests::hidden_change_refires_same_query_without_touching_sort
```

Expected: 编译失败（方法不存在）。

- [ ] **Step 3: 实现 `on_hidden_change`**

在 `on_sort_change` 之后加：

```rust
    /// 隐藏项开关变化：同串重发（语义同排序变更，但不改排序/方向）。
    /// 窗内有 pending 保持原串、刷新时刻；无 pending 按 last_fired 的串构造；
    /// 两者皆无（空词/从未发起）→ 不动。
    pub fn on_hidden_change(&mut self, now: Instant) {
        let query = self
            .pending
            .as_ref()
            .map(|(q, _)| q.clone())
            .or_else(|| self.last_fired.as_ref().map(|(q, _)| q.clone()));
        if let Some(query) = query {
            self.pending = Some((query, now));
        }
    }
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p app session::tests
```

Expected: 全部通过。

- [ ] **Step 5: 提交**

```bash
git add crates/app/src/session.rs
git commit -m "feat(app): refire active query when hidden preference changes"
```

---

## Task 8: app 设置页开关、偏好读写与 i18n

**Files:**
- Modify: `crates/app/src/i18n/catalog.rs:40-60`
- Modify: `crates/app/src/ui/settings_view.rs`（`SettingsData`、`HiddenData`、`SettingsAction`、`RowKind`、索引页分组、`draw` 行绘制、测试夹具）
- Modify: `crates/app/src/ui/settings.rs`（`HiddenPrefsResult`、`SettingsState` 字段/方法、`request_open`、`draw` 加载分支、动作处理、worker）
- Test: `crates/app/src/ui/settings_view.rs`、`crates/app/src/ui/settings.rs`

- [ ] **Step 1: 加 i18n 文案**

在 `crates/app/src/i18n/catalog.rs` 的 `catalog!` 中（`group_volumes` 附近）加：

```rust
    group_search: "搜索" / "Search",
    row_include_hidden: "包含隐藏项" / "Include Hidden Items",
    hidden_desc: "名称以 . 开头的文件和文件夹。关闭后不进入搜索结果。" / "Files and folders whose names start with a dot. Turn off to exclude them from search results.",
    hidden_loading: "加载中…" / "Loading…",
    hidden_worker_exited: "隐藏项偏好工作线程异常退出" / "Hidden items preferences worker exited unexpectedly",
```

- [ ] **Step 2: 写视图失败测试**

在 `crates/app/src/ui/settings_view.rs` 的 `mod tests` 中，参照 `panel_assist_click_emits_toggle_action` 加：

```rust
    /// 「包含隐藏项」开关中心（右对齐，几何公式同自启/自动检查/助手开关）。
    fn include_hidden_switch_center(out: &egui::FullOutput) -> egui::Pos2 {
        let label = text_center(find_text(&all_texts(out), i18n::s().row_include_hidden));
        egui::pos2(
            SETTINGS_W - CONTENT_PAD.1 - ROW_PAD_X - SWITCH_SIZE.0 / 2.0,
            label.y,
        )
    }

    /// 点击「包含隐藏项」→ 回传目标态（开→关、关→开两个方向）。
    #[test]
    fn include_hidden_click_emits_toggle_action() {
        for (current, expected) in [(true, false), (false, true)] {
            let ctx = egui::Context::default();
            ctx.set_visuals(egui::Visuals::dark());
            let mut data = with_tab(layout_test_data(), SettingsTab::Index);
            data.hidden.include = current;
            data.hidden.loaded = true;
            let (out, _) = run_frame_sized(&ctx, &data, Vec::new(), TALL_TEST_H);
            let output = click_at_sized(
                &ctx,
                &data,
                include_hidden_switch_center(&out),
                TALL_TEST_H,
            );
            assert_eq!(
                output.actions,
                vec![SettingsAction::SetHidden(expected)],
                "当前 {current} 应请求 {expected}"
            );
        }
    }
```

- [ ] **Step 3: 跑测试确认失败**

```bash
cargo test -p app include_hidden_click_emits_toggle_action
```

Expected: 编译失败（`hidden` 字段、`SetHidden`、文案不存在）。

- [ ] **Step 4: 实现视图数据与分组**

`settings_view.rs`：

`SettingsData` 增加字段：

```rust
    /// 隐藏项偏好（daemon 回显；设置页开关）。
    pub(crate) hidden: HiddenData,
```

`VolumesData` 之后加：

```rust
/// 隐藏项偏好视图数据（整块 Default，测试字面量与状态拷贝共用）。
#[derive(Default, Clone, Debug, PartialEq)]
pub(crate) struct HiddenData {
    /// daemon 偏好已回显（首次成功 HiddenPrefs 前 false，开关按缺省渲染）。
    pub(crate) loaded: bool,
    /// 搜索结果是否包含隐藏项。
    pub(crate) include: bool,
    /// 后台读/写进行中。
    pub(crate) busy: bool,
    /// 加载/写入失败文案。
    pub(crate) error: Option<String>,
}
```

`SettingsAction` 增加：

```rust
    /// 切换「包含隐藏项」。
    SetHidden(bool),
```

`RowKind` 增加：

```rust
    /// 「包含隐藏项」开关行。
    IncludeHidden,
```

`draw_index_page` 在 `draw_volumes_group` 之前调用：

```rust
    draw_search_group(ui, full_w, data, palette, extra_h, actions);
    ui.add_space(GROUP_GAP);
```

`draw_volumes_group` 之前加：

```rust
/// 搜索组：单行「包含隐藏项」开关 + 常驻说明。
fn draw_search_group(
    ui: &mut egui::Ui,
    full_w: f32,
    data: &SettingsData,
    palette: &SettingsPalette,
    extra_h: f32,
    actions: &mut Vec<SettingsAction>,
) {
    let loading = data.hidden.busy && !data.hidden.loaded;
    let error = data.hidden.error.as_deref();
    let extra = extra_h
        + extra_for(error.is_some(), extra_h)
        + extra_for(loading, extra_h);
    let rows = [GroupRow {
        kind: RowKind::IncludeHidden,
        spec: RowSpec::new(ROW_H).with_extra(extra),
        error,
        note: loading.then_some(i18n::s().hidden_loading),
        note2: Some(i18n::s().hidden_desc),
        hint: None,
    }];
    draw_card(
        ui,
        full_w,
        Some(i18n::s().group_search),
        &rows,
        data,
        palette,
        actions,
    );
}
```

`draw_row` 的 match 增加（放在 `VolumesAuto` 之前）：

```rust
        RowKind::IncludeHidden => {
            let control = draw_row_label(
                ui,
                body,
                i18n::s().row_include_hidden,
                egui::vec2(SWITCH_SIZE.0, SWITCH_SIZE.1),
                palette,
            );
            if draw_switch(
                ui,
                control,
                data.hidden.include,
                "settings-include-hidden",
                palette,
            ) {
                actions.push(SettingsAction::SetHidden(!data.hidden.include));
            }
        }
```

`SettingsData` 的构造（`settings.rs::from_state`）增加：

```rust
            hidden: state.hidden.clone(),
```

测试夹具 `layout_test_data` / `recording_data` 等所有 `SettingsData { ... }` 字面量增加：

```rust
            hidden: HiddenData::default(),
```

- [ ] **Step 5: 实现运行期状态与 worker**

`settings.rs`：

`VolumePrefsResult` 附近加：

```rust
/// 隐藏项偏好后台结果（读/写共用）。
pub(crate) struct HiddenPrefsResult {
    pub(crate) include: bool,
    pub(crate) error: Option<String>,
    /// 是否由 SetHidden 发起（成功时触发视图重查；初始加载不触发）。
    pub(crate) changed: bool,
}

fn hidden_result(
    outcome: anyhow::Result<client::HiddenPrefsOutcome>,
    changed: bool,
) -> HiddenPrefsResult {
    match outcome {
        Ok(o) => HiddenPrefsResult {
            include: o.include,
            error: o.message,
            changed,
        },
        Err(e) => HiddenPrefsResult {
            include: false,
            error: Some(format!("{e:#}")),
            changed,
        },
    }
}
```

`SettingsState` 增加字段（紧跟 volumes 字段）：

```rust
    /// 隐藏项偏好(daemon 回显)。
    pub(crate) hidden: settings_view::HiddenData,
    /// 隐藏项偏好后台结果通道(读/写共用,同一时刻至多一个在途)。
    pub(crate) hidden_rx: Option<Receiver<HiddenPrefsResult>>,
    /// 打开设置时挂起的隐藏项偏好刷新。
    pub(crate) hidden_refresh_pending: bool,
```

`SettingsState::new` 初始化增加：

```rust
            hidden: settings_view::HiddenData::default(),
            hidden_rx: None,
            hidden_refresh_pending: false,
```

`request_open` 中 `volumes_refresh_pending = true;` 之后加：

```rust
            self.hidden_refresh_pending = true;
```

`SettingsState` 方法区（`advance_volumes` 之后）加：

```rust
    /// 设置 pass 的隐藏项推进:非阻塞收结果;返回 (是否应发起加载, 刚应用过写入)。
    pub(crate) fn advance_hidden(&mut self, open: bool) -> (bool, bool) {
        let changed = self.tick_hidden();
        (
            open && self.hidden_refresh_pending && !self.hidden.busy,
            changed,
        )
    }

    /// 工作线程无法启动：清 busy/通道并落内联错误。
    pub(crate) fn hidden_spawn_failed(&mut self, detail: String) {
        log::warn!("隐藏项偏好工作线程启动失败:{detail}");
        self.hidden.busy = false;
        self.hidden_rx = None;
        self.hidden.error = Some(i18n::s().hidden_worker_exited.to_owned());
    }

    /// 非阻塞收取隐藏项偏好结果；成功覆盖 include 并清错误，失败保留旧值。
    /// 返回 true = 本次是写入成功（调用方据此触发当前查询重发）。
    fn tick_hidden(&mut self) -> bool {
        let Some(rx) = self.hidden_rx.take() else {
            return false;
        };
        match rx.try_recv() {
            Ok(result) => {
                self.hidden.busy = false;
                if let Some(error) = result.error {
                    self.hidden.error = Some(error);
                    false
                } else {
                    self.hidden.include = result.include;
                    self.hidden.loaded = true;
                    self.hidden.error = None;
                    result.changed
                }
            }
            Err(TryRecvError::Empty) => {
                self.hidden_rx = Some(rx);
                false
            }
            Err(TryRecvError::Disconnected) => {
                self.hidden.busy = false;
                self.hidden.error = Some(i18n::s().hidden_worker_exited.to_owned());
                false
            }
        }
    }

    pub(crate) fn begin_hidden(&mut self, rx: Receiver<HiddenPrefsResult>) -> bool {
        if self.hidden.busy {
            return false;
        }
        self.hidden.busy = true;
        self.hidden_rx = Some(rx);
        true
    }
```

`draw` 的锁内元组增加 `load_hidden` 与 `hidden_changed`：

```rust
        let load_volumes = s.settings.advance_volumes(open);
        let (load_hidden, hidden_changed) = s.settings.advance_hidden(open);
        if hidden_changed {
            let now = Instant::now();
            s.results.session.on_hidden_change(now);
            s.palette.session.on_hidden_change(now);
        }
```

并把 `load_hidden`、`hidden_changed` 一并返回（`hidden_changed` 只用于上面的重发，不必返回；元组只加 `load_hidden`）：

```rust
        (
            open,
            theme_changed,
            focus_lost,
            load_exclude,
            load_volumes,
            load_hidden,
            load_cli,
        )
```

在 `if load_volumes { ... }` 之后加：

```rust
    if load_hidden {
        let (tx, rx) = mpsc::channel::<HiddenPrefsResult>();
        let started = {
            let mut s = lock_shared(shared);
            if !s.settings.begin_hidden(rx) {
                false
            } else {
                let data_dir = s.settings.data_dir.clone();
                drop(s);
                let result = hidden_result(
                    client::hidden_prefs(&client::DaemonPaths { data_dir }),
                    false,
                );
                let _ = tx.send(result);
                true
            }
        };
        if started {
            lock_shared(shared).settings.hidden_refresh_pending = false;
        }
        ctx.request_repaint_of(viewport_id());
    }
```

`spawn_volumes` 之后加写偏好 worker：

```rust
/// 写隐藏项偏好（后台；结果回投后由 tick_hidden 更新回显）。
fn spawn_hidden_set(
    ctx: &egui::Context,
    shared: &Arc<Mutex<Shared>>,
    include: bool,
) -> bool {
    let (tx, rx) = mpsc::channel::<HiddenPrefsResult>();
    let data_dir = {
        let mut s = lock_shared(shared);
        if !s.settings.begin_hidden(rx) {
            log::warn!("隐藏项偏好操作被忽略:已有操作在途");
            return false;
        }
        s.settings.data_dir.clone()
    };
    ctx.request_repaint_of(viewport_id());
    let thread_ctx = ctx.clone();
    let spawned = std::thread::Builder::new()
        .name("settings-hidden".to_owned())
        .spawn(move || {
            let result = hidden_result(
                client::set_hidden_prefs(&client::DaemonPaths { data_dir }, include),
                true,
            );
            if let Err(err) = tx.send(result) {
                log::warn!("隐藏项偏好结果回投失败(设置页可能已退出):{err}");
            }
            thread_ctx.request_repaint();
        });
    if let Err(err) = spawned {
        lock_shared(shared)
            .settings
            .hidden_spawn_failed(err.to_string());
        return false;
    }
    true
}
```

动作处理（`SetVolumesAuto` 之前）加：

```rust
        SettingsAction::SetHidden(include) => {
            spawn_hidden_set(ctx, shared, include);
            ctx.request_repaint_of(viewport_id());
        }
```

- [ ] **Step 6: 跑 app 测试**

```bash
cargo test -p app
```

Expected: 全部通过（若测试夹具字面量漏字段，按编译器提示补 `hidden: HiddenData::default()`）。

- [ ] **Step 7: 突变验证**

在 `settings.rs` 的 `mod tests` 加映射测试并做突变：

```rust
    /// 写入结果须标记 changed（触发视图重发）；加载结果不标记。
    #[test]
    fn hidden_result_marks_writes_as_changed() {
        let ok = client::HiddenPrefsOutcome {
            include: true,
            message: None,
        };
        assert!(hidden_result(Ok(ok), true).changed);
        let err = hidden_result(Err(anyhow::anyhow!("boom")), true);
        assert!(err.changed && err.error.is_some(), "失败也要保留写入标记");
        assert!(!hidden_result(Err(anyhow::anyhow!("boom")), false).changed);
    }
```

运行：

```bash
cargo test -p app settings::tests::hidden_result_marks_writes_as_changed
```

Expected: PASS。随后把 `hidden_result` 的 `changed` 透传改成恒 `false`，重跑 → 必红；恢复。
再删除 `draw_search_group` 的调用 → `include_hidden_click_emits_toggle_action` 必红；恢复。

- [ ] **Step 8: 提交**

```bash
git add crates/app/src/i18n/catalog.rs crates/app/src/ui/settings_view.rs crates/app/src/ui/settings.rs
git commit -m "feat(app): add hidden items toggle in settings"
```

---

## Task 9: 文档、三门禁与实机冒烟

**Files:**
- Modify: `docs/usage.md:130-160`（图形界面搜索行为）、`:176-240`（命令与 query 章节）
- Verify: 全仓三门禁

- [ ] **Step 1: 更新 usage 文档**

在「图形界面」中靠近搜索行为的位置加一段：

```markdown
### 隐藏项

- 设置 → **索引** →「包含隐藏项」：开启后搜索结果包含名称以 `.` 开头的文件和文件夹
  （默认关闭；升级用户首次启动保持旧行为，即包含）。
- 过滤在查询层完成，切换即时生效，不重建索引；隐藏目录内的可见文件同样不显示。
- 重装/新装默认不包含；`macEverything exclude reset` 不会重置该开关。
```

命令用法行改为：

```text
macEverything [--data-dir <PATH>] query [--json] [--sort <name|size|mtime|path>] [--desc] [--include-hidden] [--] <关键词>
```

query 章节补：

```markdown
- `--include-hidden`：本次查询强制包含隐藏项（不修改全局设置）；缺省遵循全局配置。
```

- [ ] **Step 2: 格式与静态检查**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: 均无输出/无警告，退出码 0。

- [ ] **Step 3: 全量测试**

```bash
cargo test --workspace
```

Expected: 全部通过。

- [ ] **Step 4: 实机冒烟（隔离数据目录，不碰真实数据）**

```bash
cargo build --release
rm -rf /tmp/mce-hidden && mkdir -p /tmp/mce-hidden/roots/Documents /tmp/mce-hidden/roots/.private
echo x > /tmp/mce-hidden/roots/Documents/visible-probe.txt
echo x > /tmp/mce-hidden/roots/.private/hidden-probe.txt
MCE_ROOTS=/tmp/mce-hidden/roots ./target/release/macEverything --data-dir /tmp/mce-hidden/data start
sleep 1
MCE_ROOTS=/tmp/mce-hidden/roots ./target/release/macEverything --data-dir /tmp/mce-hidden/data query "probe"            # 新装默认：仅 visible-probe
MCE_ROOTS=/tmp/mce-hidden/roots ./target/release/macEverything --data-dir /tmp/mce-hidden/data query --include-hidden "probe"  # 两条
```

Expected：第一条只出现 `visible-probe.txt`；第二条出现 `hidden-probe.txt`。

- [ ] **Step 5: 实机验证开关持久化与迁移**

```bash
# 模拟旧配置（删掉 include_hidden 行）+ 已有索引 → 重启后应迁移为 true
sed -i '' '/include_hidden/d' /tmp/mce-hidden/data/config.toml
./target/release/macEverything --data-dir /tmp/mce-hidden/data stop
MCE_ROOTS=/tmp/mce-hidden/roots ./target/release/macEverything --data-dir /tmp/mce-hidden/data start
sleep 1
grep include_hidden /tmp/mce-hidden/data/config.toml   # 应为 include_hidden = true
./target/release/macEverything --data-dir /tmp/mce-hidden/data query "probe"  # 升级用户：两条
./target/release/macEverything --data-dir /tmp/mce-hidden/data stop
```

Expected：`grep` 输出 `include_hidden = true`；查询出现两条。

- [ ] **Step 6: 清理**

```bash
rm -rf /tmp/mce-hidden
```

- [ ] **Step 7: 提交文档**

```bash
git add docs/usage.md
git commit -m "docs: document hidden items filter"
```

---

## Self-Review 记录

- **Spec coverage**：product §2 语义 → Task 2（祖先判定/根豁免）；§3 默认与迁移 → Task 1/5；§4.1 设置页 → Task 8；§4.2 两视口/助手 → Task 4 调用点与全局配置；§4.3 CLI → Task 6；§6 验收 → Task 9 冒烟。
- **Placeholder scan**：无 TBD/TODO；所有代码步骤给出具体实现。
- **Type consistency**：`include_hidden`（bool，QueryPlan）、`include_hidden: Option<bool>`（Config/Request）、`HiddenPrefsOutcome { include, message }`、`HiddenData { loaded, include, busy, error }` 在各任务中一致。
