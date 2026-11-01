# epaint 本地补丁(内存修复)

- **上游基线**: `epaint 0.37.0`(crates.io,与 `egui`/`eframe` 0.37 同版本)
- **改动面**(全部带 `MCE` 注释):

1. `src/texture_atlas.rs::take_delta` —— 部分增量(partial delta)的尺寸
   **量化到 2 的幂**。
2. `src/texture_atlas.rs::take_delta` —— 整图增量(full delta)与图集
   **共享同一块 `Arc<ColorImage>`**,不再深拷贝整图。
3. `src/texture_atlas.rs::take_delta` —— 部分增量使用**持久上传缓冲池**,
   同尺寸帧原地逐行拷贝,不再每帧新建 `ColorImage`。
4. `src/texture_atlas.rs::clear` + `src/text/glyph_atlas.rs::clear` ——
   0.8 填充率触发 clear 时**原地清空**(保留图集尺寸与像素缓冲容量),
   不再丢弃图集按 32 高重建。

- **原因(实测)**:
  - 部分增量按本帧脏区任意尺寸分配 `ColorImage`(现场 4–10MB 级);macOS
    malloc 按尺寸类缓存释放后的 VM 段(`Malloc Large (empty)`)。滚动时
    每帧脏区高度不同 → 尺寸类数量随滚动线性增长 → 缓存段累积(现场
    1.78–2.83GB,71–311 块)。量化把尺寸类压回 ≤13×13。
  - 量化只解决 partial;重度滚动下还有两项每轮 clear 的 churn:
    整图增量深拷贝(最大 64MB)、以及上游 `GlyphAtlas::clear` 丢弃图集后
    从 32 高重新倍增(每轮 4/8/16/32MB 临时分配,各留一块
    `Malloc Large (empty)`)。
  - 同口径 40s 对抗滚动(4096 图集、每轮 ~1s clear、见
    `crates/app/examples/scroll_peak.rs`):

    | 版本 | 峰值 | `Malloc Large (empty)` |
    |---|---|---|
    | 未量化(上游) | 2.65GB | 85 |
    | 仅量化 | 2.94GB | 111 |
    | 量化 + 2048 图集 | 1.23GB | 123 |
    | 量化 + Arc/缓冲池/原地 clear | **386MB** | **0** |

    重度滚动下量化单独几乎无收益(clear churn 占主导);三项合并后
    分配次数从每帧×40s 降到全程 7 次(诊断计数器
    `MCE_IMAGE_CLONES` / `MCE_UPLOAD_ALLOCS`)。

- **正确性**:
  - 部分增量只多传图集已有像素;`pos` 贴边时回挪,脏区完整覆盖。
  - 整图增量交出 `Arc` 后,若下游仍持有而图集需要写,`Arc::make_mut`
    退化为一次深拷贝——正确性不变,只是那一帧慢一点(eframe 在绘制后即
    丢弃 delta,实测 `MCE_IMAGE_CLONES == 0`)。
  - 上传缓冲只复用 `strong_count == 1`(下游已释放)的块;池上限
    8 块 / 64MB,只淘汰空闲块。
  - clear 后白像素与预栅格化圆盘重新写入,`cursor`/`dirty` 归零。

- **应用侧钉(突变验证均会红)**:
  - `epaint_partial_delta_is_size_quantized` — 尺寸 2 的幂、覆盖脏区、
    逐像素与图集一致。
  - `epaint_delta_size_classes_are_bounded` — 尺寸类 ≤16。
  - `epaint_full_delta_shares_atlas_image` — `Arc::ptr_eq`。
  - `epaint_partial_delta_reuses_upload_buffers` — 同尺寸 20 帧 ≤1 次分配,
    两种尺寸交替 ≤2 次。
  - `epaint_clear_keeps_atlas_size` — clear 保留尺寸与共享内存。
  - `epaint_glyph_atlas_clear_stays_in_place` — 端到端触发 0.8 clear,
    图集高度不回落 32。

- **维护**: `[patch.crates-io] epaint = { path = "vendor/epaint" }`(工作区根
  `Cargo.toml`)。**升级 egui/epaint 时必须重新同步**:按新版本重新 vendor,
  把上述四项移植到新版 `take_delta` / `clear`(新版字段名可能变化),并跑
  上面六个钉子;否则升级会悄悄丢掉本修复(钉子会红)。
- 工作区 `exclude` 已含 `vendor/epaint`,不参与本仓三件套门禁。
