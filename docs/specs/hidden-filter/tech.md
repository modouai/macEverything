# 隐藏项搜索过滤（hidden-filter）技术规格

- 日期：2026-11-08
- 状态：待实现
- 产品契约：`product.md`

## 1. 分层与数据流

```
config.toml(include_hidden)
   │  daemon 冷启动 load_migrating / HiddenSet 写入
   ▼
daemon cfg ──► run_query(include_hidden 覆盖?)──► QueryPlan.include_hidden
                                                      │
client::query(None/Some)                              ▼
   │                                    engine::query::execute
   └─ 离线：直读 config.toml ──────────► 候选中不满足 → 丢弃
                                                      │
                                                      ▼
                                          IndexView::is_hidden(idx)
```

- 在线：`Request::Query.include_hidden: Option<bool>`，`None` = daemon 配置，`Some` = 单次覆盖。
- 离线：client 读 `config.toml`（经同一迁移入口），请求级 `Some` 仍优先。
- 索引与增量逻辑**零改动**；过滤仅在查询候选层。

## 2. engine

### 2.1 `config.rs`

```rust
pub struct Config {
    // ...既有字段
    /// 搜索结果是否包含隐藏项（名称以 `.` 开头）。
    /// None = 旧配置缺字段，等待 load_migrating 迁移。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_hidden: Option<bool>,
}

impl Config {
    /// 生效值；None 视为不包含（仅在迁移前的纯内存调用路径出现）。
    pub fn include_hidden_effective(&self) -> bool {
        self.include_hidden.unwrap_or(false)
    }
}
```

- `default_config()` 的 `include_hidden` 取 `None`（未定），由迁移入口定案。
- 新增：

```rust
/// 读配置并完成旧配置迁移。
/// `index_present` = 数据目录已有落盘索引（升级信号）。
/// 字段缺失时写入 `Some(index_present)` 并原子落盘；写失败只告警，内存仍按迁移值。
pub fn load_migrating(path: &Path, index_present: bool) -> Config
```

- `load()` 保持纯读语义，字段缺失返回 `None`。
- `ExcludeReset` 路径（`daemon::reset_excludes`）写 `default_config()` 前必须
  `defaults.include_hidden = current.include_hidden`，防止排除项重置连带清掉本开关。
- 其余写配置路径（exclude add/remove、volumes_set）整份保存，自然保留该字段。

### 2.2 `query.rs`

- `QueryPlan` 新增 `pub include_hidden: bool`；`parse` 置 `true`（旧调用方语义不变）。
- `execute` 的候选闭包，在名称/path 匹配全部通过、`push` 之前：

```rust
if !plan.include_hidden && view.is_hidden(idx) {
    return None;
}
```

  放在匹配之后是刻意的：只对真正的候选中选者做父链判定，避免对百万条目逐条上溯。
- `total = rows.len()` 天然同口径（过滤在计数前）。
- `execute_sorted` 复用同一 `execute`，无额外改动。

### 2.3 `index.rs` / `overlay.rs`

```rust
// IndexView 默认实现（单源视图）
fn is_hidden(&self, idx: u32) -> bool {
    let mut cur = idx;
    loop {
        if self.display_name(cur).first() == Some(&b'.') { return true; }
        let p = self.parent_idx(cur);
        if p == NO_PARENT { return false; }
        cur = p;
    }
}
```

- 根条目名为绝对路径（首字节 `/`），天然不判隐藏；不做特别分支。
- 非 UTF-8 名称按首字节判断，无解码成本。
- `CompositeView` 覆写：overlay 条目沿 overlay `parent` 链上溯；到 `NO_PARENT`（桥接
  anchor）后经 `anchor_base[&cur]` 切回 base 目录号，继续 base 父链。必须覆写，否则
  `.config/` 下增量新建的文件会漏判（anchor 截断父链）。实现与 `full_path_composite`
  同一条链，但不分配路径。
- 该判定不读 `entry_valid`；无效条目已在候选层先被剔除。

## 3. protocol

`crates/protocol/src/lib.rs`，沿用「纯新增可选字段/新变体不 bump v」惯例：

```rust
Request::Query {
    // ...既有字段
    /// None = 跟随 daemon 配置；Some = 单次覆盖。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    include_hidden: Option<bool>,
}

Request::HiddenSet { v: u32, include: bool },
Request::HiddenPrefs { v: u32 },

Response::HiddenPrefs { include: bool, message: Option<String> },
```

- `Request::version()` 补两个新变体。
- 旧 daemon 解码未知变体失败 → 设置页按既有「请重启引擎」兜底文案处理。
- 测试：Query 序列化仅在 `Some` 时带字段；`None` 与旧客户端消息同形；新消息往返。

## 4. client

- `query(paths, q, limit, sort, desc, dir_only, include_hidden: Option<bool>)`。
  - 在线：透传 `include_hidden`（`None` 不发送）。
  - 离线：`include_hidden.unwrap_or_else(|| load_migrating(...).include_hidden_effective())`。
- 新增 `HiddenPrefsOutcome { include, message }` 与 `hidden_prefs()` / `set_hidden_prefs()`，
  镜像 `volumes_call` 的连接复用、错误兜底与旧引擎文案。
- `offline_query` 增加 `include_hidden: Option<bool>` 参数，置 `plan.include_hidden`。

## 5. cli / daemon

- `cold_start`：`config::load` → `config::load_migrating(&paths.config_file(), paths.index_file().exists())`。
- `run_query` 增加 `include_hidden: Option<bool>`：`plan.include_hidden = include_hidden.unwrap_or_else(|| state.cfg.lock_recover().include_hidden_effective())`。
- dispatch：
  - `HiddenSet` → 更新 `state.cfg`（`Some(include)`）、`config::save`；保存失败返回
    `message`（内存已临时生效），与 `volumes_set` 同纪律。
  - `HiddenPrefs` → 读回显。
- `reset_excludes`：保留当前 `include_hidden`（见 §2.1）。
- 参数层（`args.rs`）：`Command::Query` 增加 `include_hidden: bool`；解析 `--include-hidden`
  （布尔旗标，重复出现不报错）；USAGE 同步。`main.rs` 传 `include_hidden.then_some(true)`。
- 保存面板助手（`dir_only`）调用点传 `None`，自动遵循全局配置。

## 6. app

- `settings_view.rs`：
  - `SettingsData` 增加 `hidden: HiddenData { loaded: bool, include: bool, busy: bool, error: Option<String> }`。
  - 索引页顶部新增「搜索」分组（单行开关 + 说明），复用 `draw_switch` 与行模型；
    `SettingsAction::SetHidden(bool)`。
- `settings.rs`：
  - 打开设置时（`request_open` 挂起位）读 `client::hidden_prefs`，模式同 volumes（设置 pass 内调用、锁外执行）。
  - `SetHidden`：后台 `client::set_hidden_prefs`；以 daemon 回显更新开关（落盘失败但内存已生效时同样应用新值并保留行内错误）；传输失败保留旧值、只落错误。
  - 成功写入后对 `results.session` 与 `palette.session` 调用 `on_hidden_change`，并 `request_repaint_of(ROOT/palette)` 唤醒两视口；初始读取不触发重查。
  - **旧引擎**：不新增重启逻辑。`engine_launcher::ensure_engine` 已在 App 启动（及存活监控
    拉起）时比对 `Status.daemon_version` 与 App 版本，不一致即 stop → 拉起随包 CLI，
    升级后设置页打开前引擎已换新。本特性只保留既有错误展示：`hidden_prefs` 失败时
    行内报错（含「请重启引擎」兜底文案），开关保持上次回显值。
- `session.rs`：新增 `on_hidden_change(now)`，语义同 `on_sort_change` 但不改排序：
  有 pending 刷新时刻；否则用 `last_fired` 的串构造 pending；空会话不动。
- i18n（`catalog.rs`）：`row_include_hidden`、`hidden_desc`
  等中英条目。
- `client::query` 调用点（`palette.rs` 两条、`main.rs`）补 `None`。

## 7. 测试（含突变验证）

| 层 | 用例 |
|---|---|
| engine query | 自身隐藏排除；祖先隐藏排除；根不判隐藏；`include_hidden=true` 放行；`total` 口径；`path:` 词下的隐藏条目 |
| engine overlay | 增量新建 `.config/new.txt`（anchor 链）在关闭开关时不可见；删除 `CompositeView::is_hidden` 覆写 → 测试必须红 |
| config | 缺失字段 + 索引存在 → true 且落盘；无索引 → false；显式值保持；`load` 不写盘；reset 保留 |
| protocol | Query 可选字段序列化；HiddenSet/HiddenPrefs 编解码 |
| client | 在线透传/覆盖；离线读配置；离线请求覆盖 |
| daemon | 配置 false 隐藏、`Some(true)` 覆盖；`HiddenSet` 后同连接查询生效；`reset_excludes` 保留 |
| cli args | `--include-hidden` 解析、与 `--sort/--desc` 共存、USAGE |
| app | 开关动作发出；回显/失败回滚；`on_hidden_change` 生成重查 Fire；空词不重查 |

## 8. 风险与兼容

- **兼容**：Query 新字段可选、新消息变体不 bump 版本。旧 daemon 的换新由既有
  `ensure_engine` 版本偏差机制负责（App 启动时 `daemon_version` 不一致即 stop → 拉起）；
  设置页只保留既有错误展示作为兜底。
- **迁移写盘**：`load_migrating` 在首次启动写配置；已有 `config::save` 的原子写与 0600 权限。
- **热路径**：隐藏判定只对匹配候选执行，深度为目录层级；百万级查询无可测回退（随基准复核）。
- **文档**：`docs/usage.md` 搜索行为与 CLI 小节、设置页索引页说明；规格归档于本目录。
