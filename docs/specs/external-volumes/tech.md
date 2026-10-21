# 外接磁盘索引 v1 技术设计

- 日期：2026-10-21
- 关联：`product.md`；`crates/engine/src/{index,scanner,watcher,fsevents}.rs`；`crates/cli/src/daemon.rs`

## 1. 卷发现与本地性判定（daemon 轮询，无新 FFI）

`VolumeMonitor` 线程每 3s `read_dir("/Volumes")`，对每个非 `.` 前缀目录 `statfs`：

- `MNT_LOCAL` 标志位为真，且 `f_fstypename` ∉ {`smbfs`, `nfs`, `afp`, `webdav`,
  `msdos`×网络形态}（v1 用 MNT_LOCAL + 显式网络类型黑名单双保险）；
- 目录名 ∉ `config.volumes_excluded`；`config.auto_index_volumes == true`。

⇒ 「期望挂载集」。每轮与「当前索引卷集」对账（对账语义见 §3）。
选轮询而非 DiskArbitration：零新增 unsafe FFI、3s 延迟可接受、
「事件只当信号，轮询对账才是真相」与 v1-core 哲学一致。

实机校准（2026-10-21）：`/Volumes/Macintosh HD` 是 Firmlink 数据卷入口
（statfs 穿透后挂载点为 `/System/Volumes/Data`）、`/Volumes/Recovery` 带
`MNT_DONTBROWSE`——两者都不是「插入的外接盘」。判定补两条：
- **自挂载校验**：`f_mntonname` 必须等于该 `/Volumes` 路径本身；
- **MNT_DONTBROWSE 剔除**（系统卷标志）。

已挂载的 DMG（安装器镜像）属本地卷，默认索引；用户可按卷名排除。

## 2. 状态与权威

- `state.roots` 增加卷根（`/Volumes/<名>`）；`IndexMeta.scan_roots` 天然反映，
  协议不改。
- 卷的**运行期 FSEvents 覆盖**用**按卷独立流**：主流（HOME + /Applications）
  创建后不动；运行期挂载的卷各自 `start_stream`（SINCE_NOW）+ 各自
  `consume_events(state, paths, stream, rx, is_replay=false, false)` 线程。
  事件汇入同一状态机（event id 水位 fetch_max；`dropped` 降级路径复用——
  代价：卷流降级会触发全量重扫（含卷），接受，罕见路径）。
- **启动路径**：`cold_start` 把「当前挂载的期望卷」并入初始 roots
  （落盘索引路径：流覆盖含卷，停机窗口重放追平漂移；首扫路径：一并首扫）。
- 注册表 `volume_streams: Mutex<HashMap<String, FseStream>>` 记运行期卷流，卸载时 stop。

## 3. 对账（每轮轮询收敛到期望态）

| 差异 | 动作 |
| --- | --- |
| 新卷挂载 | roots += 卷根 → 后台全量扫该卷（批间检查卷目录仍在，消失即中止转清除）→ flush |
| 卷消失/被排除/主开关关 | **前缀清除**（§4）→ 停该卷流 → roots -= 卷根 → flush |
| 主开关关闭 | 对全部已索引卷执行上行动作 |

「期望挂载集」变化才动作；静止期零操作（同 L4 哲学）。

## 4. 前缀清除（engine 新能力）

`index::remove_prefix(index, dir_map, prefix)`：遍历 `dir_map`，键路径以
`prefix/` 开头的目录 → 其 `children_of` 全部 `remove`（墓碑）+ 删除 dir_map 键。
墓碑走既有 compaction 机制。启动时同样用于清除「index 有、当前未挂载」的
卷前缀（冷启动加载落盘索引后、建流前执行一次）。

复杂度 O(目录数)；卷条目占比小，可接受。**不用**「重扫剩余 roots」替代
（HOME 全量重扫秒级起步，代价过高）。

## 5. 配置与协议

`config.toml` 新字段（daemon 唯一写者，serde default 兜底旧文件）：

```toml
auto_index_volumes = true     # 主开关
volumes_excluded = []         # 卷目录名列表
```

设置页写通道与 exclude 同模式：protocol 新增 `SetVolumePrefs` 消息
（auto + excluded），daemon 收到 → 更新 Config + save → VolumeMonitor
下轮对账生效。VolumeMonitor 每 3s 同时重读 `state.cfg`（内存），不直读文件。

## 6. 边界与已知取舍

- **卷改名** = 旧卷消失 + 新卷出现（全量重扫新名）；v1 接受（UUID 键控留后续）。
- **根分区**：`/` 不经 `/Volumes` 暴露，天然不在管理范围。
- **扫描中拔盘**：批次间 `exists()` 检查中止；残余墓碑由清除路径兜底。
- **扫盘中拔出又立刻重插**（`exists()` 对重插后的卷恒真）：卸载推进
  **扫描世代号**（`volume_scan_epoch`），在途批次批间校验世代漂移即放弃，
  不与新扫交错应用产生重复条目（自审 2026-10-21）。
- **网络卷误判**：双保险仍误挂时，用户可在设置里关掉该卷（卷列表按
  MNT_LOCAL 过滤后仍列出的即是证据）。
- **性能**：外接 HDD 全量扫描耗时受 USB 带宽支配；扫描期间查询不阻塞
  （批间释放写锁，同首扫策略）。
- **卷元数据目录**（`.fseventsd`/`.Trashes` 等）：default_config 排除列表已扩充
  （名字式,对卷内同名生效）。
