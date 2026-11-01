# epaint 本地补丁(内存修复)

- **上游基线**: `epaint 0.37.0`(crates.io,与 `egui`/`eframe` 0.37 同版本)
- **唯一改动**: `src/texture_atlas.rs` 的 `TextureAtlas::take_delta` —— 部分增量
  (partial delta)的尺寸**量化到 2 的幂**(见文件内 `MCE 本地补丁` 标记)。
- **原因**: 增量按本帧脏区任意尺寸分配 `ColorImage`(实测 4–10MB 级);macOS
  malloc 按尺寸类缓存释放后的 VM 段(`Malloc Large (empty)`)。滚动时每帧
  脏区高度不同 → 尺寸类数量随滚动线性增长 → 缓存段累积,实测滚动峰值
  1.9–2.8GB(71 块 × 4–10MB)。量化后尺寸类 ≤13×13,缓存段可复用、内存有界。
- **正确性**: 多传的行/列取自图集现有像素(只多传,不改内容);`pos` 在贴近
  边界时回挪,保证脏区完整覆盖。应用侧钉:
  `epaint_partial_delta_is_size_quantized`(逐像素比对增量与图集)。
- **维护**: `[patch.crates-io] epaint = { path = "vendor/epaint" }`(工作区根
  `Cargo.toml`)。**升级 egui/epaint 时必须重新同步**:按新版本重新 vendor,
  将同一量化逻辑移植到新版 `take_delta`,并跑上面两个钉子;否则升级会悄悄
  丢掉本修复(钉子会红)。
- 工作区 `exclude` 已含 `vendor/epaint`,不参与本仓三件套门禁。
