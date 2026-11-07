# 嵌套 App 不再置顶设计

- 日期：2026-11-07
- 状态：已确认，待实现计划
- 范围：engine 查询排序 + 新增 bundle 判定模块；索引格式、扫描、增量、持久化均无改动
- 功能代号：nested-app-rank

## 1. 背景

默认相关度排序把 `.app` 目录整体置顶（product §4）。判定只看「是目录 + display 名以 `.app` 结尾」，不看在树中的位置，于是应用包内部的嵌套 `.app` 与真正的应用同级置顶。

实测：`/Applications/WPS Office.app/Contents/SharedSupport/.../kaideployserver.app`、`BaiduNetdisk_mac.app/Contents/Frameworks/.../BaiduNetdisk_mac Helper.app`、`WorkBuddy.app/Contents/Frameworks/.../WorkBuddy Repair.app` 全部进入置顶组。搜 `ai` 时首屏 16 条中 11 条来自包内，真正的应用被挤后。置顶的本意是让「用户要启动的应用」优先，包内可执行文件不满足这个意图。

这是排序缺陷，与「是否索引包内文件」是两个独立问题。本设计只修排序，不改索引内容，不删召回。

## 2. 目标与非目标

### 目标

1. 只有**不在任何 bundle 内部**的 `.app` 条目参与置顶。
2. 索引内容、条目数、搜索命中集不变；变化的只有默认相关度排序。
3. 索引格式、落盘、扫描、增量、daemon 均无改动。

### 非目标

- 不剪枝包内文件（见 `2026-11-07-bundle-prune-design.md`，暂缓）。
- 不改变列头排序（名称/大小/时间/路径）。
- 不改变置顶组内部的排序规则。

## 3. 行为契约

置顶判定（默认相关度排序的最高维度）：

```
is_dir
&& display 名以 ".app" 结尾（ASCII 大小写不敏感）
&& 从父条目到根的任一祖先，其 display 名不以任何 bundle 扩展名结尾
```

bundle 扩展名集合与剪枝设计共用：

```
.app  .framework  .bundle  .plugin  .appex  .xpc  .kext  .prefpane  .qlgenerator
```

用全集合而非仅 `.app`：`Chromium.framework/Helpers/X.app` 这类路径同样是包内。

示例：

| 条目 | 置顶 |
|---|---|
| `/Applications/Safari.app` | 是 |
| `/Applications/WPS Office.app/Contents/.../kaideployserver.app` | 否 |
| `/Applications/BaiduNetdisk_mac.app/.../Helper.app` | 否 |
| `/Applications/X.app/Contents/Frameworks/Chromium.framework/Helpers/Y.app` | 否 |
| `/Users/me/notes.app.txt`（文件） | 否 |
| `/Users/me/notes.app.txt/` 下的 `Z.app`（祖先不以 bundle 扩展名结尾） | 是 |

## 4. 实现

### 4.1 新增 `engine/src/bundle.rs`

```rust
pub const BUNDLE_EXTENSIONS: [&str; 9] = [...];

/// 条目名是否以 bundle 扩展名结尾（ASCII 大小写不敏感）。
pub fn is_bundle_name(display: &[u8]) -> bool;

/// 路径任一组件是否以 bundle 扩展名结尾（逐组件复用 is_bundle_name）。
pub fn is_bundle_path(p: &Path) -> bool;
```

独立成模块而非塞进 `scanner`：判定同时被查询排序消费，且剪枝若日后实施可直接复用。`lib.rs` 导出。

### 4.2 查询候选行携带置顶位

`crates/engine/src/query.rs` 的候选行从 `(u8, usize, u32)` 改为 `(u16, usize, u32)`：

- 第一字段 = `(ranks_first as u16) << 8 | score as u16`。
- 元组大小不变（u16 + usize + u32 对齐后仍为 16B），百万命中的临时内存不增长。
- 比较器退化为单次 `b.0.cmp(&a.0)` 再按名长、条目号决胜；置顶组（高 8 位为 1）整体优先，组内仍按 score 降序。

`ranks_first` 在候选收集阶段计算（只对命中的 `.app` 目录条目调用，数量小）：

```rust
fn ranks_first(view: &dyn IndexView, idx: u32) -> bool {
    let e = view.entry(idx);
    if !is_app_entry(e.is_dir(), view.display_name(idx)) {
        return false;
    }
    let mut p = view.parent_idx(idx);
    while p != NO_PARENT {
        if is_bundle_name(view.display_name(p)) {
            return false;
        }
        p = view.parent_idx(p);
    }
    true
}
```

要点：

- 用 `view.parent_idx` 而非 `entry(idx).parent`：合成视图（base + overlay）需要全局合成号，直接读父号会跨空间取错。
- 上溯在候选收集的并行块内完成，`IndexView: Sync` 已满足。
- 候选收集是 `filter_map`，`ranks_first` 只在「目录且名以 `.app` 结尾」时触发父链，普通条目零额外成本。

### 4.3 保留现有判定

`is_app_entry(is_dir, display)` 保留为「目录 + `.app` 后缀」的名字判定，`ranks_first` 在其上叠加位置判定。现有针对大小写、非目录的测试继续有效。

## 5. 测试

全部做突变验证。

1. 根下 `Tool.app` 置顶（既有测试保持）。
2. `Outer.app/Contents/Inner.app`：`Inner.app` 不置顶，`Outer.app` 置顶。
3. `Chromium.framework/Helpers/X.app`：不置顶（祖先为 `.framework`）。
4. 祖先名仅含 `.app` 子串但非后缀（`notes.app.txt/` 下的 `Z.app`）：仍置顶。
5. 大小写：`Tools.APP` 在 `Outer.app` 内不置顶。
6. `case:` 查询下行为一致。
7. top-limit 截断：低分非包内 `.app` 仍先于高分包内 `.app` 入选。
8. bundle.rs 单元：扩展名表逐项命中；`notes.app.txt`、`应用.app备份`、`Contents` 不命中；`/a/X.app/Contents` 路径命中。

## 6. 文档同步

| 文档 | 改动 |
|---|---|
| `docs/specs/v1-core/product.md` §4 | 「App 包整体置顶」补「位于其它 bundle 内部的 `.app` 不置顶」 |
| `docs/specs/v1-core/tech.md` §打分排序 | 同步判定条件 |
| `docs/usage.md` 排序说明 | 同步一句 |

## 7. 取舍

- **召回不变**：包内 `.app` 与包内文件仍出现在结果集，只是不置顶。若实际使用中包内文件仍是噪声，再议剪枝。
- **父链上溯成本**：仅对命中的 `.app` 目录条目，深度通常 ≤ 6；百万命中下 `.app` 命中数很小，不计入 P99 风险。实现后跑既有基准确认。
- **不缓存父链判定**：一次查询内同一 `.app` 条目只判定一次（候选收集时），无需跨查询缓存。
