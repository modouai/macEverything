# 嵌套 App 不再置顶实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 默认相关度排序只置顶「不在任何 bundle 内部」的 `.app` 目录，消除包内 Helper / 嵌套子应用抢占首屏。

**Architecture:** 新增 `engine/src/bundle.rs` 承载 bundle 扩展名判定；`query.rs` 在候选收集阶段对命中的 `.app` 条目上溯父链算出置顶位，编进行元组的高 8 位；比较器退化为单次整数比较。索引、扫描、增量、持久化均不改动。

**Tech Stack:** Rust workspace（`engine` crate），rayon 并行分块，现有 `IndexView` 抽象与 `Index` 测试夹具。

**依据规格:** `docs/superpowers/specs/2026-11-07-nested-app-rank-design.md`

---

## 文件结构

| 文件 | 责任 |
|---|---|
| `crates/engine/src/bundle.rs`（新建） | bundle 扩展名常量与 `is_bundle_name` 纯函数。O1 剪枝实施时在此补 `is_bundle_path` / `is_inside_bundle` |
| `crates/engine/src/lib.rs` | 注册 `pub mod bundle;` |
| `crates/engine/src/query.rs` | 候选行编码置顶位、`ranks_first` 判定、比较器简化 |
| `docs/specs/v1-core/product.md` | 排序条款 |
| `docs/specs/v1-core/tech.md` | 打分排序实现说明 |
| `docs/usage.md` | 用户可见排序说明 |

---

### Task 1: bundle 判定模块

**Files:**
- Create: `crates/engine/src/bundle.rs`
- Modify: `crates/engine/src/lib.rs:1-13`
- Test: `crates/engine/src/bundle.rs`（模块内 `#[cfg(test)]`）

- [ ] **Step 1: 写失败测试**

创建 `crates/engine/src/bundle.rs`，先只放测试与空实现占位：

```rust
//! bundle 判定：扩展名清单与后缀匹配（查询排序与包内剪枝共用）。

pub const BUNDLE_EXTENSIONS: [&str; 9] = [
    ".app",
    ".framework",
    ".bundle",
    ".plugin",
    ".appex",
    ".xpc",
    ".kext",
    ".prefpane",
    ".qlgenerator",
];

/// 条目名是否以 bundle 扩展名结尾（ASCII 大小写不敏感）。
pub fn is_bundle_name(_display: &[u8]) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 清单逐项命中，大小写不敏感。
    #[test]
    fn bundle_extensions_match_case_insensitively() {
        for ext in BUNDLE_EXTENSIONS {
            let lower = format!("X{ext}");
            let upper = format!("X{}", ext.to_ascii_uppercase());
            assert!(is_bundle_name(lower.as_bytes()), "{lower}");
            assert!(is_bundle_name(upper.as_bytes()), "{upper}");
        }
    }

    /// 后缀必须完整匹配：`.app` 子串、中文后缀、无点形态均不命中。
    #[test]
    fn non_bundle_names_do_not_match() {
        for name in [
            "notes.app.txt",
            "应用.app备份",
            "Contents",
            ".application",
            "app",
            "X.apps",
        ] {
            assert!(!is_bundle_name(name.as_bytes()), "{name}");
        }
    }
}
```

- [ ] **Step 2: 注册模块并跑测试确认失败**

在 `crates/engine/src/lib.rs` 的模块列表中按字母序加一行：

```rust
pub mod arena;
pub mod bundle;
pub mod config;
```

Run: `cargo test -p engine bundle::tests`
Expected: FAIL，`bundle_extensions_match_case_insensitively` 报 `assertion failed`（空实现恒 `false`）；`non_bundle_names_do_not_match` 此时通过，Step 5 的突变负责把它钉住。

- [ ] **Step 3: 实现最小判定**

把 `bundle.rs` 的空实现替换为：

```rust
/// 条目名是否以 bundle 扩展名结尾（ASCII 大小写不敏感）。
pub fn is_bundle_name(display: &[u8]) -> bool {
    BUNDLE_EXTENSIONS.iter().any(|ext| {
        display.len() >= ext.len()
            && display[display.len() - ext.len()..].eq_ignore_ascii_case(ext.as_bytes())
    })
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p engine bundle::tests`
Expected: PASS，2 tests passed。

- [ ] **Step 5: 突变验证**

临时把实现改成恒 `true`，重跑：

```bash
cargo test -p engine bundle::tests
```

Expected: `non_bundle_names_do_not_match` 失败。确认后改回正确实现，再跑一次确认全绿。

- [ ] **Step 6: 提交**

```bash
cargo fmt --check
cargo clippy -p engine --all-targets -- -D warnings
git add crates/engine/src/bundle.rs crates/engine/src/lib.rs
git commit -m "feat(engine): 新增 bundle 扩展名判定模块

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 2: 查询候选行编入置顶位

**Files:**
- Modify: `crates/engine/src/query.rs:207-295`（候选行类型、收集、比较器）
- Modify: `crates/engine/src/query.rs:303-310`（`is_app_entry` 后新增 `ranks_first`）
- Test: `crates/engine/src/query.rs` 测试模块（`app_bundles_rank_first_in_relevance_order` 之后）

- [ ] **Step 1: 写失败测试**

在 `crates/engine/src/query.rs` 的测试模块中，`app_first_survives_top_limit_truncation` 之后插入：

```rust
    /// 嵌套在另一个 .app 内的 .app 不置顶：外层应用照常置顶，
    /// 包内 Helper 回到普通条目组按打分排序。
    /// 突变「ranks_first 去掉祖先循环」时必红。
    #[test]
    fn nested_app_inside_app_does_not_rank_first() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let outer = i.insert("Outer.app", d, true);
        let contents = i.insert("Contents", outer, true);
        let _ = i.insert("Inner.app", contents, true);
        let _ = i.insert("alpha.txt", d, false); // 前缀命中(score 3)
        let out = execute(&i, &parse("a").unwrap(), 100);
        assert_eq!(
            names(&i, &out.hits),
            vec!["Outer.app", "alpha.txt", "Inner.app"],
            "包内 .app 不得置顶"
        );
    }

    /// 祖先为 .framework 的 .app 同样不置顶（判定用全 bundle 扩展名集合）。
    #[test]
    fn app_inside_framework_does_not_rank_first() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let fw = i.insert("Chromium.framework", d, true);
        let helpers = i.insert("Helpers", fw, true);
        let _ = i.insert("X.app", helpers, true);
        let _ = i.insert("alpha.txt", d, false);
        let out = execute(&i, &parse("a").unwrap(), 100);
        assert_eq!(
            names(&i, &out.hits),
            vec!["alpha.txt", "X.app", "Chromium.framework"],
            ".framework 内的 .app 不得置顶"
        );
    }

    /// 祖先名只是包含 .app 子串、并非 bundle 后缀：子 .app 仍置顶。
    #[test]
    fn app_under_non_bundle_lookalike_still_ranks_first() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let lookalike = i.insert("notes.app.txt", d, true);
        let _ = i.insert("Z.app", lookalike, true);
        let out = execute(&i, &parse("a").unwrap(), 100);
        assert_eq!(
            names(&i, &out.hits),
            vec!["Z.app", "notes.app.txt"],
            "祖先不是 bundle 时仍置顶"
        );
    }

    /// 祖先后缀大写同样识别为 bundle。
    #[test]
    fn nested_uppercase_app_does_not_rank_first() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let outer = i.insert("Outer.app", d, true);
        let _ = i.insert("Inner.APP", outer, true);
        let _ = i.insert("alpha.txt", d, false);
        let out = execute(&i, &parse("a").unwrap(), 100);
        assert_eq!(
            names(&i, &out.hits),
            vec!["Outer.app", "alpha.txt", "Inner.APP"],
            "大写 bundle 后缀的祖先同样生效"
        );
    }

    /// top-limit 截断使用同一比较子：包内 .app 不得挤掉普通高分条目。
    #[test]
    fn nested_app_does_not_survive_top_limit() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let outer = i.insert("Outer.app", d, true);
        let contents = i.insert("Contents", outer, true);
        let _ = i.insert("Inner.app", contents, true);
        let _ = i.insert("alpha.txt", d, false);
        let out = execute(&i, &parse("a").unwrap(), 2);
        assert_eq!(
            names(&i, &out.hits),
            vec!["Outer.app", "alpha.txt"],
            "截断集不得包含包内 .app"
        );
    }

    /// case: 查询下置顶判定读 display 原形态，嵌套判定同样生效。
    #[test]
    fn nested_app_does_not_rank_first_under_case_sensitive_query() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let outer = i.insert("Outer.app", d, true);
        let _ = i.insert("InnerA.app", outer, true); // 含大写 A，子串命中(score 1)
        let _ = i.insert("Another.txt", d, false); // 大写 A 前缀命中(score 3)
        let out = execute(&i, &parse("case:A").unwrap(), 100);
        assert_eq!(
            names(&i, &out.hits),
            vec!["Another.txt", "InnerA.app"],
            "case: 下包内 .app 仍不置顶"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p engine query::tests::nested`
Expected: FAIL，至少 `nested_app_inside_app_does_not_rank_first` 的实际顺序把 `Inner.app` 排到了最前。

Run: `cargo test -p engine query::tests::app_inside_framework_does_not_rank_first`
Expected: FAIL，`X.app` 被置顶。

- [ ] **Step 3: 实现置顶位**

在 `crates/engine/src/query.rs` 的 `is_app_entry` 之后新增：

```rust
/// 默认相关度排序的最高维度：非包内的 `.app` 目录置顶。
/// 判定 = 目录 + display 名 `.app` 后缀 + 祖先链无 bundle 组件。
/// 只看命中的 `.app` 条目（数量小），父链上溯在候选收集阶段完成一次，
/// 比较器只读编好的位，不做路径解析。
fn ranks_first(view: &dyn IndexView, idx: u32) -> bool {
    let e = view.entry(idx);
    if !is_app_entry(e.is_dir(), view.display_name(idx)) {
        return false;
    }
    // 用 parent_idx 而非 entry(idx).parent：合成视图需要全局合成号。
    let mut p = view.parent_idx(idx);
    while p != NO_PARENT {
        if crate::bundle::is_bundle_name(view.display_name(p)) {
            return false;
        }
        p = view.parent_idx(p);
    }
    true
}
```

- [ ] **Step 4: 改候选行类型与收集**

把 `crates/engine/src/query.rs` 中候选行声明

```rust
    let mut rows: Vec<(u8, usize, u32)> = blocks
```

改为

```rust
    // 候选行：(置顶位 | 分数) 组合键, 名长, 条目号。置顶位占高 8 位,
    // 低 8 位为 score(1..3)——单次整数比较即完成两级排序,行大小不变(16B)。
    let mut rows: Vec<(u16, usize, u32)> = blocks
```

把收集处

```rust
                Some((score_of(&target, &c_name), target.len(), idx))
```

改为

```rust
                Some((
                    ((ranks_first(view, idx) as u16) << 8)
                        | u16::from(score_of(&target, &c_name)),
                    target.len(),
                    idx,
                ))
```

- [ ] **Step 5: 改比较器**

把

```rust
    // APP 包置顶为最高维度(spec v1-core):候选行不携带额外字段,比较时按条目
    // 判定(display 名 `.app` 后缀,4 字节比较,O(log n) 次比较开销可忽略)。
    let cmp = |a: &(u8, usize, u32), b: &(u8, usize, u32)| {
        let app = |row: &(u8, usize, u32)| {
            is_app_entry(view.entry(row.2).is_dir(), view.display_name(row.2))
        };
        app(b)
            .cmp(&app(a))
            .then(b.0.cmp(&a.0))
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
    };
```

改为

```rust
    // 置顶位为最高维度（候选收集时已编入高 8 位），组内按 score 降序、
    // 名长升序、条目号决胜。
    let cmp = |a: &(u16, usize, u32), b: &(u16, usize, u32)| {
        b.0.cmp(&a.0)
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
    };
```

`rows.into_iter().map(|(_, _, i)| i)` 与 `let total = rows.len() as u32;` 不变。

- [ ] **Step 6: 跑全部 query 测试确认通过**

Run: `cargo test -p engine query::`
Expected: PASS，既有 4 条 APP 置顶测试与新增 6 条嵌套测试全部通过。

- [ ] **Step 7: 突变验证**

临时把 `ranks_first` 的祖先循环整段删掉（只保留 `is_app_entry` 判定），重跑：

```bash
cargo test -p engine query::tests::nested
```

Expected: `nested_app_inside_app_does_not_rank_first`、`app_inside_framework_does_not_rank_first`、`nested_uppercase_app_does_not_rank_first`、`nested_app_does_not_survive_top_limit` 均失败。改回后重跑确认全绿。

- [ ] **Step 8: 提交**

```bash
cargo fmt --check
cargo clippy -p engine --all-targets -- -D warnings
git add crates/engine/src/query.rs
git commit -m "fix(engine): 嵌套在 bundle 内的 .app 不再置顶

排序最高维度改为「非包内 .app」:候选收集阶段上溯父链判定,
置顶位编进行元组高 8 位,比较器单次整数比较完成两级排序。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 3: 文档同步

**Files:**
- Modify: `docs/specs/v1-core/product.md:151`
- Modify: `docs/specs/v1-core/tech.md:122`
- Modify: `docs/usage.md:347`

- [ ] **Step 1: 改 product 排序条款**

把

```markdown
- 匹配排序:App 包(`.app` 目录,大小写不敏感)整体置顶,组内与普通条目同级按「文件名前缀命中 > 词边界命中 > 子串命中;同级按文件名长度升序」。仅作用于默认匹配排序,列头名称/大小/时间/路径排序不受影响。
```

改为

```markdown
- 匹配排序:App 包(`.app` 目录,大小写不敏感)整体置顶;位于其它 bundle(`.app`/`.framework`/`.bundle`/`.plugin`/`.appex`/`.xpc`/`.kext`/`.prefpane`/`.qlgenerator`,大小写不敏感)内部的 `.app` 不置顶。组内与普通条目同级按「文件名前缀命中 > 词边界命中 > 子串命中;同级按文件名长度升序」。仅作用于默认匹配排序,列头名称/大小/时间/路径排序不受影响。
```

- [ ] **Step 2: 改 tech 打分排序说明**

把 tech.md 第 4 条中的

```markdown
4. 打分排序:App 包置顶(`is_dir` 且 display 名以 `.app` 结尾、ASCII 大小写不敏感;列头排序不受影响),组内按前缀命中(3)> 词边界命中(2)> 子串命中(1)
```

改为

```markdown
4. 打分排序:App 包置顶(`is_dir` 且 display 名以 `.app` 结尾、ASCII 大小写不敏感,且祖先链无 bundle 扩展名组件——嵌套在其它包内的 `.app` 不置顶;列头排序不受影响),组内按前缀命中(3)> 词边界命中(2)> 子串命中(1)
```

- [ ] **Step 3: 改 usage 排序说明**

把

```markdown
- 排序:App 包(`.app` 目录)整体置顶,组内与普通条目按「文件名前缀命中 > 词边界命中 > 子串命中;同级按名更短优先」。列头排序不受影响。
```

改为

```markdown
- 排序:App 包(`.app` 目录)整体置顶;嵌套在其它应用包/框架内的 `.app` 不置顶。组内与普通条目按「文件名前缀命中 > 词边界命中 > 子串命中;同级按名更短优先」。列头排序不受影响。
```

- [ ] **Step 4: 提交**

```bash
git add docs/specs/v1-core/product.md docs/specs/v1-core/tech.md docs/usage.md
git commit -m "docs: 排序契约补充嵌套 app 不置顶

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 4: 三门禁与基准回归

**Files:** 无代码改动（验证任务）

- [ ] **Step 1: 跑三门禁**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: 三条全部退出码 0。全量测试应包含 `engine` 的既有 parity 与 query 测试。

- [ ] **Step 2: 基准回归**

```bash
bash scripts/bench.sh /tmp/mce-bench 1000000
```

Expected: 查询 P99 与改动前同档（规格红线 < 10ms）。重点看「查询」一项；候选行仍是 16B，比较器从多次函数调用降为单次整数比较，理论上不劣于改动前。把改动前后的查询数字记录到提交说明或验收记录。

- [ ] **Step 3: 实机冒烟**

```bash
bash scripts/bundle.sh
open release/macEverything.app
```

在完整结果窗口搜索 `ai`，确认：

1. `BaiduNetdisk_mac.app`、`智谱AI开放平台.app`、`AI_Excel_Processor.app` 等非包内应用仍置顶。
2. `kaideployserver.app`、`WorkBuddy Repair.app`、`BaiduNetdisk_mac Helper*.app` 不再出现在置顶组（它们仍在结果集中，按普通条目排序）。
3. 列头按名称排序时行为不变。

- [ ] **Step 4: 记录基准数字（仅当任务要求留痕）**

本任务的验收证据是 Step 1–3 的输出。若维护者要求把查询 P99 前后对比写进文档，追加到 `docs/superpowers/specs/2026-11-07-nested-app-rank-design.md` §7，并单独提交：

```bash
git add docs/superpowers/specs/2026-11-07-nested-app-rank-design.md
git commit -m "docs: 记录嵌套 app 排序修复的基准回归

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

否则跳过本步。

---

## 完成标准

- [ ] `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` 全绿。
- [ ] 新增 6 条 query 测试与 2 条 bundle 测试，均通过突变验证。
- [ ] 实机搜索 `ai` 的首屏不再被包内 `.app` 占据。
- [ ] 三处文档与实现一致。
- [ ] 剪枝开关打开后的排序回归测试（规格 §5 第 9 条）留待 O1 计划实施时补，本期不实现配置项。

## 不在本计划内

- 包内剪枝（O1）：见 `docs/superpowers/specs/2026-11-07-bundle-prune-design.md`，独立计划。
- product.md 问题陈述与目标 7/10/12 的重写：属于 O1 的规格同步，不在排序修复范围。
