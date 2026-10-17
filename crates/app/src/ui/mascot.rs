//! 悬浮条吉祥物（visual.md §2.9）：单张 PNG + 整体变换动作。
//!
//! 形象是**面板的兄弟**：绘制在面板之后、窗口范围内（面板 clip 之外）。
//! 窗口顶部预留 `MASCOT_STRIP`(70) 形象条；面板顶边仍在可见区顶 −110。
//! 坐标口径：屏幕坐标 y 向下时窗口顶 = 面板顶 − 70；AppKit y 向上时窗口顶
//! y = 面板顶 y + 70（平台层负责，本模块只认面板原点）。
//!
//! 当前素材是单张 PNG（眼睛/嘴巴烤在图里），所以只做整体位移/旋转；
//! 眨眼、转眼需要分层素材（§2.9「动作」），留待设计侧。动画排帧由
//! `ui::draw_palette` 在可见期负责（`FRAME_INTERVAL`），本模块只算姿态。

use std::time::Duration;

use eframe::egui;

use crate::session::State;
use crate::ui::tokens as tk;

/// 动画排帧间隔（≈30fps）：呼吸/摇摆是低频动作，30fps 足够。
pub const FRAME_INTERVAL: Duration = Duration::from_millis(33);
/// 「输入时」判定窗口：距末次输入小于此值视为正在打字（前倾）。
pub const TYPING_WINDOW: Duration = Duration::from_millis(300);

/// 内置素材（与设计稿同目录相对路径；文档移动即断，见 §2.9「素材与画布的关系」）。
const MASCOT_PNG: &[u8] =
    include_bytes!("../../../../docs/specs/floatbar-visual/蓝色文件夹与放大镜吉祥物-透明.png");
/// 纹理像素尺寸（2x：118×100 逻辑点 → 236×200 px；长边 ≥ 2 倍目标显示尺寸）。
/// 由 token 推导，防画布尺寸调整后纹理比例漂移。
const TEX_W: usize = tk::MASCOT_W as usize * 2;
const TEX_H: usize = tk::MASCOT_H as usize * 2;

/// 单帧姿态（整体变换；素材是单张 PNG，无分部件）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pose {
    /// 垂直偏移（逻辑点；正 = 向下）。
    pub dy: f32,
    /// 顺时针旋转角（度）。
    pub angle_deg: f32,
}

/// 状态 → 姿态（纯函数；`t` = 动画时钟秒数）。
///
/// 优先级：引擎启动中 > 无结果 > 输入 > 待机。
/// - 引擎启动中：左右摆 ±3°，周期 1.6s（张望）；
/// - 无结果：整体下沉 4px（耷拉）；
/// - 输入：前倾 2.5°（探头）；
/// - 待机：上下呼吸 1.5px，周期 2.4s。
pub fn pose_for(state: &State, typing: bool, t: f32) -> Pose {
    if matches!(state, State::EngineStarting) {
        return Pose {
            dy: 0.0,
            angle_deg: 3.0 * (std::f32::consts::TAU * t / 1.6).sin(),
        };
    }
    if matches!(state, State::Empty) {
        return Pose {
            dy: 4.0,
            angle_deg: 0.0,
        };
    }
    if typing {
        return Pose {
            dy: 0.0,
            angle_deg: 2.5,
        };
    }
    Pose {
        dy: 1.5 * (std::f32::consts::TAU * t / 2.4).sin(),
        angle_deg: 0.0,
    }
}

/// 形象矩形（纯函数；§2.9 锚点：右缘距面板右缘 60、下缘压过面板顶边 34）。
/// `panel_origin` = 面板左上角（窗口内已下移 `MASCOT_STRIP`）；`dy` 计入下沉。
///
/// y 锚「下缘压面板顶 34」：画布顶 = 面板顶 − (100 − 34) + dy。窗口顶在面板顶上方
/// `MASCOT_STRIP`(70)，故静止时画布顶距窗口顶 `MASCOT_HEADROOM`(4)，给旋转/呼吸留量。
pub fn mascot_rect(panel_origin: egui::Pos2, panel_w: f32, dy: f32) -> egui::Rect {
    let x = panel_origin.x + panel_w - tk::MASCOT_RIGHT_GAP - tk::MASCOT_W;
    let y = panel_origin.y - (tk::MASCOT_H - tk::MASCOT_OVERLAP) + dy;
    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(tk::MASCOT_W, tk::MASCOT_H))
}

/// 吉祥物纹理（跨帧持有；`PaletteState::reset` 不清）。
#[derive(Default)]
pub struct Mascot {
    texture: Option<egui::TextureHandle>,
    /// 加载失败已留痕（只 warn 一次，不逐帧刷日志）。
    failed: bool,
    /// false = 不加载纹理（测试构造用；生产恒 true）。
    enabled: bool,
}

impl Mascot {
    pub fn new() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }

    /// 测试构造：不预热、也不惰性加载纹理（单测只验姿态/几何，避免每次解码 PNG）。
    #[cfg(test)]
    pub fn without_texture() -> Self {
        Self {
            enabled: false,
            ..Self::new()
        }
    }

    /// 绘制（面板之后调用；不可交互）。`t` = 动画时钟（秒）。
    pub fn draw(
        &mut self,
        ui: &egui::Ui,
        state: &State,
        typing: bool,
        panel_origin: egui::Pos2,
        panel_w: f32,
        t: f32,
    ) {
        let Some(texture) = self.texture(ui.ctx()) else {
            return;
        };
        let pose = pose_for(state, typing, t);
        let rect = mascot_rect(panel_origin, panel_w, pose.dy);
        // 旋转支点在画布底边中点（归一化 UV (0.5, 1.0)）——距面板顶边 34px（压顶量）；
        // 前倾/摇摆时头顶摆动、下缘压顶关系不变。正角 = 顺时针。
        // 实锁 0.37:`Image::new` 只收 source(无尺寸参数);尺寸由 `paint_at` 的
        // rect 决定(纹理本身是 2x 像素,布局尺寸按 118×100 画布给)。
        egui::Image::new(texture)
            .rotate(pose.angle_deg.to_radians(), egui::vec2(0.5, 1.0))
            .paint_at(ui, rect);
    }

    /// 启动期预热（`PaletteState::new` 调用；窗口此时隐藏，避开首次唤起首帧解码）。
    pub fn ensure_texture(&mut self, ctx: &egui::Context) {
        if self.enabled {
            let _ = self.texture(ctx);
        }
    }

    /// 取纹理：未预热/预热失败时惰性重试一次；失败 warn 一次后本会话不再尝试。
    /// 测试构造（`without_texture`）恒返回 None，不触碰解码。
    fn texture(&mut self, ctx: &egui::Context) -> Option<&egui::TextureHandle> {
        if !self.enabled {
            return None;
        }
        if self.texture.is_none() && !self.failed {
            match decode_image() {
                Ok(image) => {
                    self.texture = Some(ctx.load_texture(
                        "palette-mascot",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
                Err(err) => {
                    self.failed = true;
                    log::warn!("吉祥物纹理加载失败（内置 PNG 解码异常）：{err}；本会话不绘制");
                }
            }
        }
        self.texture.as_ref()
    }
}

/// 内置 PNG → 2x 画布尺寸的**预乘** alpha `ColorImage`。
///
/// PNG 的 alpha 是**直通**（非预乘）——与系统图标路径（NSImage/TIFF 预乘）不同。
/// 缩放前必须先预乘：`image` 的采样假设通道是预乘的，直接对直通 alpha 做 Lanczos
/// 会让透明区的白色向边缘渗色（白边）。流程：解码 → 浮点预乘 → Lanczos 缩放 →
/// 转回 u8 → `from_rgba_premultiplied`。
fn decode_image() -> Result<egui::ColorImage, String> {
    let decoded = image::load_from_memory(MASCOT_PNG).map_err(|e| format!("PNG 解码失败:{e}"))?;
    let resized = premultiply_resize(&decoded.to_rgba8(), TEX_W as u32, TEX_H as u32);
    let mut rgba = Vec::with_capacity(TEX_W * TEX_H * 4);
    for px in resized.pixels() {
        rgba.extend_from_slice(&px.0);
    }
    Ok(egui::ColorImage::from_rgba_premultiplied(
        [TEX_W, TEX_H],
        &rgba,
    ))
}

/// 直通 alpha RGBA → 浮点预乘 → Lanczos 缩放到 `w×h` → u8 **预乘** RGBA。
///
/// 预乘在 f32 上做（避免 u8 量化误差），缩放后的越界值在转 u8 时 clamp。
fn premultiply_resize(src: &image::RgbaImage, w: u32, h: u32) -> image::RgbaImage {
    let (sw, sh) = (src.width(), src.height());
    let mut premul = Vec::with_capacity(sw as usize * sh as usize * 4);
    for px in src.pixels() {
        let a = px[3] as f32 / 255.0;
        premul.extend_from_slice(&[
            px[0] as f32 / 255.0 * a,
            px[1] as f32 / 255.0 * a,
            px[2] as f32 / 255.0 * a,
            a,
        ]);
    }
    let float = image::ImageBuffer::<image::Rgba<f32>, Vec<f32>>::from_raw(sw, sh, premul)
        .expect("像素数与尺寸一致");
    let resized = image::imageops::resize(&float, w, h, image::imageops::FilterType::Lanczos3);
    let mut bytes = Vec::with_capacity(w as usize * h as usize * 4);
    for px in resized.pixels() {
        for c in px.0 {
            bytes.push((c * 255.0).round().clamp(0.0, 255.0) as u8);
        }
    }
    image::RgbaImage::from_raw(w, h, bytes).expect("像素数与尺寸一致")
}

/// 变换后包围盒（测试辅助；旋转原点与 `Image::rotate` 一致：画布底边中点）。
#[cfg(test)]
pub(crate) fn rotated_bounds(rect: egui::Rect, angle_deg: f32) -> egui::Rect {
    let pivot = rect.min + egui::vec2(0.5, 1.0) * rect.size();
    let rot = egui::emath::Rot2::from_angle(angle_deg.to_radians());
    let mut min = egui::pos2(f32::INFINITY, f32::INFINITY);
    let mut max = egui::pos2(f32::NEG_INFINITY, f32::NEG_INFINITY);
    for corner in [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ] {
        let q = pivot + rot * (corner - pivot);
        min = egui::pos2(min.x.min(q.x), min.y.min(q.y));
        max = egui::pos2(max.x.max(q.x), max.y.max(q.y));
    }
    egui::Rect::from_min_max(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-4;

    /// 待机呼吸：幅度 1.5px、周期 2.4s；0/2.4s 归零，0.6s 最高、1.8s 最低。
    #[test]
    fn idle_pose_breathes_one_and_half_px_every_2_4s() {
        let idle = |t: f32| pose_for(&State::Idle, false, t);
        assert!(idle(0.0).dy.abs() < EPS);
        assert!((idle(0.6).dy - 1.5).abs() < EPS, "{}", idle(0.6).dy);
        assert!((idle(1.8).dy + 1.5).abs() < EPS, "{}", idle(1.8).dy);
        assert!(idle(2.4).dy.abs() < EPS, "一个周期回到原点");
        assert_eq!(idle(0.6).angle_deg, 0.0, "呼吸不带旋转");
    }

    /// 引擎启动中：左右摆 ±3°、周期 1.6s（0.4s 最大、1.2s 反向最大）。
    #[test]
    fn engine_starting_pose_sways_three_deg_every_1_6s() {
        let sway = |t: f32| pose_for(&State::EngineStarting, false, t);
        assert!((sway(0.4).angle_deg - 3.0).abs() < EPS);
        assert!((sway(1.2).angle_deg + 3.0).abs() < EPS);
        assert!(sway(1.6).angle_deg.abs() < EPS, "一个周期回到中位");
        assert_eq!(sway(0.4).dy, 0.0, "摇摆不带位移");
    }

    /// 优先级：引擎启动中 > 无结果 > 输入 > 待机。
    #[test]
    fn pose_priority_engine_over_empty_over_typing_over_idle() {
        // 引擎启动中压过「无结果」与「输入」。
        let engine = pose_for(&State::EngineStarting, true, 0.4);
        assert!((engine.angle_deg - 3.0).abs() < EPS);
        assert_eq!(engine.dy, 0.0);
        // 无结果压过「输入」：下沉 4px、不倾斜。
        let empty = pose_for(&State::Empty, true, 0.6);
        assert_eq!(empty.dy, 4.0);
        assert_eq!(empty.angle_deg, 0.0);
        // 输入压过待机：前倾 2.5°、不呼吸。
        let typing = pose_for(&State::Searching, true, 0.6);
        assert_eq!(typing.angle_deg, 2.5);
        assert_eq!(typing.dy, 0.0);
        // 待机：呼吸。
        assert!((pose_for(&State::Searching, false, 0.6).dy - 1.5).abs() < EPS);
    }

    /// 几何（§2.9）：右缘距面板右 60、下缘压过面板顶 34（dy 计入）、头顶余量 4、窄宽左移。
    #[test]
    fn mascot_rect_anchors_right_gap_and_overlap() {
        // 窗口内面板原点 y = 形象条高（70）；窗口顶 = 0。
        let panel_origin = egui::pos2(100.0, tk::MASCOT_STRIP);
        let r = mascot_rect(panel_origin, 700.0, 0.0);
        assert_eq!(r.size(), egui::vec2(118.0, 100.0));
        assert_eq!(r.right(), 100.0 + 700.0 - 60.0, "右缘距面板右 60");
        assert_eq!(
            r.bottom() - panel_origin.y,
            34.0,
            "下缘压过面板顶 34（画布高 100 − 露出条 70）"
        );
        assert_eq!(
            panel_origin.y - r.top(),
            66.0,
            "画布顶距面板顶 66（= 100 − 压顶 34）"
        );
        assert_eq!(
            r.top(),
            tk::MASCOT_HEADROOM,
            "静止时画布顶距窗口顶恰为头顶余量 4"
        );
        // 下沉 4px 计入下缘。
        let sunk = mascot_rect(panel_origin, 700.0, 4.0);
        assert_eq!(sunk.bottom(), r.bottom() + 4.0);
        // 窄宽：右缘随面板右缘左移，形象整体左移。
        let narrow = mascot_rect(panel_origin, 480.0, 0.0);
        assert_eq!(r.right() - narrow.right(), 220.0);
        assert_eq!(r.width(), narrow.width());
    }

    /// 四种姿态（呼吸最高 / 前倾 / 摇摆 ±3°）旋转后的包围盒不越窗口顶。
    /// 旋转支点 = 画布底边中点；头顶余量 4px 由几何推导覆盖最坏 ±3°。
    #[test]
    fn rotated_bounds_stay_inside_headroom_for_all_poses() {
        let panel_origin = egui::pos2(100.0, tk::MASCOT_STRIP);
        let cases = [
            // 待机呼吸最高点（dy = −1.5）
            pose_for(&State::Idle, false, 1.8),
            // 输入前倾 2.5°
            pose_for(&State::Searching, true, 0.0),
            // 引擎启动中左右摆 ±3°
            pose_for(&State::EngineStarting, false, 0.4),
            pose_for(&State::EngineStarting, false, 1.2),
        ];
        for pose in cases {
            let rect = mascot_rect(panel_origin, 700.0, pose.dy);
            let bbox = rotated_bounds(rect, pose.angle_deg);
            assert!(
                bbox.min.y >= 0.0,
                "姿态 {pose:?} 旋转后头部越出窗口顶:bbox.min.y={}",
                bbox.min.y
            );
        }
        // 最坏 ±3° 摇摆的理论上探量约 2.95px < 4px 余量（剩余 ~1.05px）。
        let worst = rotated_bounds(mascot_rect(panel_origin, 700.0, 0.0), 3.0);
        assert!(
            worst.min.y > 0.0 && worst.min.y < tk::MASCOT_HEADROOM,
            "3° 摇摆后头顶应仍在窗口内且吃掉部分余量:{}",
            worst.min.y
        );
    }

    /// 直通 alpha 先预乘再缩放：透明区的白色不得向边缘渗色（Lanczos 假设预乘）。
    #[test]
    fn premultiplied_resize_keeps_transparent_white_from_bleeding() {
        // 1×2：左 = 白色全透明，右 = 红色不透明。
        let src = image::RgbaImage::from_raw(2, 1, vec![255, 255, 255, 0, 255, 0, 0, 255])
            .expect("尺寸一致");
        let out = premultiply_resize(&src, 4, 1);
        // 预乘后透明白 = (0,0,0,0)；缩放只混合 R/A，G/B 应恒为 0。
        for px in out.pixels() {
            assert!(
                px[1] <= 2 && px[2] <= 2,
                "透明白向缩放边缘渗色（G/B 漂白）:{px:?}"
            );
        }
        // 不透明红侧仍保持红色（R 通道有值），排除「全黑也算过」。
        assert!(
            out.pixels().any(|px| px[0] > 200),
            "不透明红色在缩放后应保留"
        );
    }

    /// 避让（§2.9 硬约束，Task 8 预留的回归网）：形象与按钮、角标/输入右缘
    /// 均留 12px 净空，且水平不重叠。
    #[test]
    fn mascot_clears_button_and_search_row_content() {
        let panel_w = tk::PANEL_W;
        let r = mascot_rect(egui::pos2(0.0, tk::MASCOT_STRIP), panel_w, 0.0);
        // 按钮：右→左布局，「打开完整窗口」按钮左缘 = 面板右 − 20 − 28。
        let button_left = panel_w - tk::SEARCH_PAD_X - tk::BTN_SIZE;
        assert!(
            button_left - r.right() >= tk::MASCOT_BADGE_GAP,
            "形象右缘 {} 与按钮左缘 {button_left} 净空不足 12",
            r.right()
        );
        // 角标（或输入框）右缘 = 按钮左缘 − 间距 − 形象净空。
        let content_right =
            panel_w - tk::SEARCH_PAD_X - tk::BTN_SIZE - tk::SEARCH_GAP - tk::MASCOT_CLEAR_W;
        assert!(
            r.left() - content_right >= tk::MASCOT_BADGE_GAP,
            "形象左缘 {} 与角标/输入右缘 {content_right} 净空不足 12",
            r.left()
        );
    }

    /// 内置 PNG 可解码且为设计稿原始尺寸 901×762（素材替换即红）。
    #[test]
    fn embedded_png_decodes_at_source_size() {
        let img = image::load_from_memory(MASCOT_PNG).expect("内置吉祥物 PNG 必须可解码");
        assert_eq!((img.width(), img.height()), (901, 762));
    }

    /// 纹理尺寸与 token 画布 1:1（2x）：`MASCOT_W×2 × MASCOT_H×2`，且保留 alpha。
    /// 断言直接用 token 推导值（不复用解码侧的 `TEX_W/TEX_H` 自证）。
    #[test]
    fn decoded_texture_is_2x_canvas_with_alpha() {
        let image = decode_image().expect("内置 PNG 应可解码");
        let expect = [(tk::MASCOT_W * 2.0) as usize, (tk::MASCOT_H * 2.0) as usize];
        assert_eq!(image.size, expect);
        assert!(
            image.pixels.iter().any(|p| p.a() > 0),
            "素材应包含非透明像素"
        );
    }
}
