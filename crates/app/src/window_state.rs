//! 完整结果窗口尺寸持久化（product §2；visual.md §1.2）。
//!
//! 文件位于 App 数据目录：`window-state.toml`，内容为最小 TOML 子集：
//!
//! ```text
//! width=1080
//! height=627
//! ```
//!
//! 不新增依赖（手工 parse/serialize）；越界尺寸 clamp 到 [720×480, 3840×2160]，
//! 非法/缺字段视为无记录（用默认 1080×627）。写入走同目录 tmp + rename，
//! 失败只 warn、不 panic（启动/退出路径不得因持久化失败中断）。
//!
//! 解析只认 `width` / `height` 两个键：未知键、无 `=` 的行一律忽略（前向兼容），
//! width/height 的值非法仍视为无记录。
//!
//! 运行期保存由 [`WindowSizeTracker`] 做 **400ms 限频（合并写）**：首个变化
//! 立即写（leading），之后 400ms 内的连续变化合并；距上次写入不足 400ms 时
//! 提示调用方排一帧，到期补写最终尺寸。它**不是 trailing debounce**——后续变化
//! 不会重置计时，只按固定窗口限频。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 首次启动尺寸（visual.md §1.2）。
pub const DEFAULT_W: f32 = 1080.0;
pub const DEFAULT_H: f32 = 627.0;
/// 窗口最小尺寸（辉哥 2026-10-17 定案）。
pub const MIN_W: f32 = 720.0;
pub const MIN_H: f32 = 480.0;
/// 尺寸上限（超屏/异常值回落，防脏数据把窗口开到不可见位置）。
pub const MAX_W: f32 = 3840.0;
pub const MAX_H: f32 = 2160.0;
/// 运行期写盘限频窗（合并窗内的连续变化）。
pub const SAVE_THROTTLE: Duration = Duration::from_millis(400);

const FILE_NAME: &str = "window-state.toml";

/// 窗口逻辑尺寸（egui 点）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowSize {
    pub w: f32,
    pub h: f32,
}

impl Default for WindowSize {
    fn default() -> Self {
        Self {
            w: DEFAULT_W,
            h: DEFAULT_H,
        }
    }
}

impl WindowSize {
    /// 构造并 clamp（取整到逻辑点；越界不报错，落边界值）。
    pub fn new(w: f32, h: f32) -> Self {
        Self {
            w: clamp_dim(w, MIN_W, MAX_W),
            h: clamp_dim(h, MIN_H, MAX_H),
        }
    }

    /// 解析 `width=...` / `height=...`（行序无关；空行与 `#` 注释忽略）。
    /// 未知键与无 `=` 的行忽略（前向兼容）；width/height 缺字段、非数字、
    /// 非正数、NaN/Inf 一律 `None`；合法但越界 → clamp。
    pub fn parse(text: &str) -> Option<Self> {
        let mut width: Option<f32> = None;
        let mut height: Option<f32> = None;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue; // 无等号：未知行，忽略
            };
            match key.trim() {
                "width" => width = Some(parse_dim(value.trim())?),
                "height" => height = Some(parse_dim(value.trim())?),
                _ => {} // 未知键忽略，允许文件向前扩展
            }
        }
        Some(Self::new(width?, height?))
    }

    /// 序列化为 `width=...\nheight=...\n`（整数逻辑点）。
    pub fn serialize(self) -> String {
        format!("width={:.0}\nheight={:.0}\n", self.w, self.h)
    }
}

/// 取整并 clamp 到 [min, max]。
fn clamp_dim(value: f32, min: f32, max: f32) -> f32 {
    value.round().clamp(min, max)
}

/// 单维解析：只接受有限正数（拒绝 0 / 负数 / NaN / Inf / 非数字）。
fn parse_dim(value: &str) -> Option<f32> {
    let value: f32 = value.parse().ok()?;
    (value.is_finite() && value > 0.0).then_some(value)
}

/// 尺寸文件路径（`data_dir/window-state.toml`）。
pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

/// 读取尺寸：无文件/非法文件返回 `None`（调用方用默认值），IO 失败留痕不静默。
pub fn load(data_dir: &Path) -> Option<WindowSize> {
    let path = path(data_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            log::warn!("窗口尺寸读取失败（{}）：{err}", path.display());
            return None;
        }
    };
    let size = WindowSize::parse(&text);
    if size.is_none() {
        log::warn!("窗口尺寸文件无效，使用默认尺寸：{}", path.display());
    }
    size
}

/// 原子写尺寸：同目录 tmp + rename；失败只 warn，不 panic。
pub fn save(data_dir: &Path, size: WindowSize) {
    let path = path(data_dir);
    let tmp = data_dir.join(format!("{FILE_NAME}.tmp"));
    if let Err(err) = std::fs::create_dir_all(data_dir) {
        log::warn!("窗口尺寸目录创建失败（{}）：{err}", data_dir.display());
        return;
    }
    if let Err(err) = std::fs::write(&tmp, size.serialize()) {
        log::warn!("窗口尺寸写入失败（{}）：{err}", tmp.display());
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if let Err(err) = std::fs::rename(&tmp, &path) {
        log::warn!("窗口尺寸替换失败（{}）：{err}", path.display());
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    log::info!("窗口尺寸已保存：{:.0}×{:.0}", size.w, size.h);
}

/// 观察结果（调用方据此决定写盘 / 排帧）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SizeUpdate {
    /// 首帧建立基线，或尺寸未变。
    Idle,
    /// 尺寸变了但距上次写盘不足 400ms：调用方排一帧，到期再观察。
    Defer(Duration),
    /// 限频窗已过且尺寸已变：立即写盘（合并窗内多次变化）。
    Save(WindowSize),
}

/// 尺寸变化跟踪 + 400ms 限频（纯逻辑，可单测）。
///
/// 首帧只建基线（不写盘）：避免启动时把屏幕/系统归一化后的等价尺寸误写，
/// 也避免「用户从未调整过」时产生写入。之后以**上次写盘尺寸**为比较基准：
/// 首个变化立即写（leading），之后 400ms 内的变化合并，窗口到期由调用方
/// 排帧补写最终值。不是 trailing debounce（后续变化不重置计时）。
#[derive(Debug, Default)]
pub struct WindowSizeTracker {
    last_saved: Option<WindowSize>,
    last_save_at: Option<Instant>,
}

impl WindowSizeTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入当前尺寸与时刻（已通过可见性/最小值过滤）。
    pub fn observe(&mut self, current: WindowSize, now: Instant) -> SizeUpdate {
        match self.last_saved {
            None => {
                self.last_saved = Some(current);
                SizeUpdate::Idle
            }
            Some(last) if last == current => SizeUpdate::Idle,
            Some(_) => {
                let elapsed = self.last_save_at.map(|t| now.saturating_duration_since(t));
                match elapsed {
                    Some(elapsed) if elapsed < SAVE_THROTTLE => {
                        SizeUpdate::Defer(SAVE_THROTTLE - elapsed)
                    }
                    _ => {
                        self.last_saved = Some(current);
                        self.last_save_at = Some(now);
                        SizeUpdate::Save(current)
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_file() {
        let size = WindowSize::parse("width=1080\nheight=627\n").expect("合法尺寸");
        assert_eq!(
            size,
            WindowSize {
                w: 1080.0,
                h: 627.0
            }
        );
        // 行序无关、空白与注释容忍、未知键忽略
        let size =
            WindowSize::parse("# 窗口\nheight = 700\nfuture=1\nwidth=1440\n").expect("合法尺寸");
        assert_eq!(
            size,
            WindowSize {
                w: 1440.0,
                h: 700.0
            }
        );
    }

    /// 前向兼容：未知键（值非法也不影响）与无 `=` 的行一律忽略。
    #[test]
    fn parse_ignores_unknown_lines() {
        let text = "width=1080\nfuture=abc\nno equals here\nheight=627\n";
        assert_eq!(
            WindowSize::parse(text),
            Some(WindowSize {
                w: 1080.0,
                h: 627.0
            }),
            "未知键/无等号行不得让合法尺寸失效"
        );
        assert_eq!(
            WindowSize::parse("garbage\nwidth=900\nheight=600\n"),
            Some(WindowSize { w: 900.0, h: 600.0 })
        );
    }

    #[test]
    fn parse_missing_or_invalid_returns_none() {
        assert_eq!(WindowSize::parse(""), None, "空文件");
        assert_eq!(WindowSize::parse("width=1080\n"), None, "缺 height");
        assert_eq!(WindowSize::parse("height=627\n"), None, "缺 width");
        assert_eq!(WindowSize::parse("width=abc\nheight=627\n"), None, "非数字");
        assert_eq!(WindowSize::parse("width=0\nheight=627\n"), None, "0 非法");
        assert_eq!(
            WindowSize::parse("width=-100\nheight=627\n"),
            None,
            "负数非法"
        );
        assert_eq!(
            WindowSize::parse("width=NaN\nheight=627\n"),
            None,
            "NaN 非法"
        );
        assert_eq!(
            WindowSize::parse("width=inf\nheight=627\n"),
            None,
            "Inf 非法"
        );
        assert_eq!(WindowSize::parse("no equals sign\n"), None, "缺等号");
    }

    #[test]
    fn parse_clamps_out_of_range() {
        assert_eq!(
            WindowSize::parse("width=100\nheight=100\n"),
            Some(WindowSize { w: MIN_W, h: MIN_H }),
            "过小 clamp 到下限"
        );
        assert_eq!(
            WindowSize::parse("width=9999\nheight=9999\n"),
            Some(WindowSize { w: MAX_W, h: MAX_H }),
            "过大 clamp 到上限"
        );
        assert_eq!(
            WindowSize::parse("width=1000\nheight=100\n"),
            Some(WindowSize {
                w: 1000.0,
                h: MIN_H
            }),
            "两维独立 clamp"
        );
    }

    #[test]
    fn serialize_round_trips() {
        let size = WindowSize::new(1200.4, 700.6);
        assert_eq!(size.serialize(), "width=1200\nheight=701\n");
        assert_eq!(WindowSize::parse(&size.serialize()), Some(size));
        assert_eq!(
            WindowSize::default().serialize(),
            "width=1080\nheight=627\n"
        );
    }

    /// 默认尺寸与 product/token 口径（visual.md §1.2：1080×627）交叉钉住。
    #[test]
    fn default_size_matches_spec() {
        assert_eq!((DEFAULT_W, DEFAULT_H), (1080.0, 627.0));
        assert_eq!(
            WindowSize::default(),
            WindowSize {
                w: 1080.0,
                h: 627.0
            }
        );
        assert_eq!(
            WindowSize::default(),
            WindowSize::new(1080.0, 627.0),
            "默认值须在 clamp 区间内"
        );
    }

    #[test]
    fn file_load_save_round_trip() {
        let dir = std::env::temp_dir().join(format!(
            "mce-window-state-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load(&dir), None, "目录不存在 = 无记录");
        save(
            &dir,
            WindowSize {
                w: 1280.0,
                h: 800.0,
            },
        );
        assert_eq!(
            load(&dir),
            Some(WindowSize {
                w: 1280.0,
                h: 800.0
            })
        );
        // 覆盖写：rename 替换旧文件
        save(&dir, WindowSize { w: 900.0, h: 600.0 });
        assert_eq!(load(&dir), Some(WindowSize { w: 900.0, h: 600.0 }));
        assert!(
            !path(&dir).with_extension("toml.tmp").exists(),
            "临时文件已清理"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_file_loads_none() {
        let dir = std::env::temp_dir().join(format!(
            "mce-window-state-invalid-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        std::fs::write(path(&dir), "garbage").expect("写坏文件");
        assert_eq!(load(&dir), None, "非法文件回落默认");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tracker_baselines_then_throttles() {
        let t0 = Instant::now();
        let mut tracker = WindowSizeTracker::new();
        let a = WindowSize {
            w: 1080.0,
            h: 627.0,
        };
        assert_eq!(tracker.observe(a, t0), SizeUpdate::Idle, "首帧只建基线");
        assert_eq!(tracker.observe(a, t0), SizeUpdate::Idle, "未变不写");

        // 首次变化：距上次写盘无记录 → 立即 Save
        let b = WindowSize {
            w: 1200.0,
            h: 700.0,
        };
        assert_eq!(
            tracker.observe(b, t0 + Duration::from_millis(10)),
            SizeUpdate::Save(b)
        );

        // 400ms 内的再次变化：Defer 剩余时长，不写盘
        let c = WindowSize {
            w: 1300.0,
            h: 750.0,
        };
        let defer = tracker.observe(c, t0 + Duration::from_millis(200));
        assert_eq!(defer, SizeUpdate::Defer(Duration::from_millis(210)));

        // 到期后落最终尺寸（中间值 c）
        assert_eq!(
            tracker.observe(c, t0 + Duration::from_millis(410)),
            SizeUpdate::Save(c)
        );
        assert_eq!(
            tracker.observe(c, t0 + Duration::from_secs(1)),
            SizeUpdate::Idle
        );
    }

    /// 连续变化流（每 50ms 一次，持续 1s）：首个**实际变化**立即写（i=1，t=60ms），
    /// 之后 400ms 限频，总共只写 3 次（60 / 460 / 860ms）——既不是逐帧写，也不是
    /// trailing debounce（后者要等变化停止才写 1 次）。
    #[test]
    fn tracker_continuous_resize_writes_at_fixed_throttle() {
        let t0 = Instant::now();
        let mut tracker = WindowSizeTracker::new();
        tracker.observe(
            WindowSize {
                w: 1000.0,
                h: 600.0,
            },
            t0,
        );

        let mut saves = Vec::new();
        for i in 0..20 {
            let now = t0 + Duration::from_millis(10 + i * 50);
            let size = WindowSize {
                w: 1000.0 + i as f32,
                h: 600.0,
            };
            if let SizeUpdate::Save(size) = tracker.observe(size, now) {
                saves.push((i, size.w));
            }
        }
        assert_eq!(
            saves,
            vec![(1, 1001.0), (9, 1009.0), (17, 1017.0)],
            "400ms 限频窗口"
        );
    }

    #[test]
    fn tracker_ignores_return_to_saved_size() {
        let t0 = Instant::now();
        let mut tracker = WindowSizeTracker::new();
        let a = WindowSize {
            w: 1080.0,
            h: 627.0,
        };
        tracker.observe(a, t0);
        let b = WindowSize {
            w: 1200.0,
            h: 700.0,
        };
        tracker.observe(b, t0 + Duration::from_millis(10)); // Save b
        let c = WindowSize {
            w: 1100.0,
            h: 650.0,
        };
        assert!(matches!(
            tracker.observe(c, t0 + Duration::from_millis(100)),
            SizeUpdate::Defer(_)
        ));
        // 回到已保存尺寸：无需再写
        assert_eq!(
            tracker.observe(b, t0 + Duration::from_millis(200)),
            SizeUpdate::Idle
        );
    }
}
