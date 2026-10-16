//! PoC-6：悬浮条高度变化的三种呈现方式实机对照（供 floatbar-visual §6.4「过渡动画加/不加」拍板取证）
//!
//! 背景：现实现把「丝滑」押在「每帧对原生窗口发 InnerSize」上（app `palette.rs` 动画期每帧
//! resize + 每帧重锚），合成器在窗口已变、新帧未出处把旧帧拉伸（app 注释自认「最多错一拍、
//! ≤30px」），四轮调参未收敛。本探针把高度变化的三种呈现做成可切换，供肉眼对照那 1 帧：
//!
//!   ① 瞬跳      内容与窗口同时到目标高；每次变化 1 次原生 resize（干脆式）
//!   ② 窗内平滑  窗口先撑到目标（增长）/动画完成再收（收缩），动画全在窗内绘制；
//!               每次变化 1 次原生 resize；过渡期窗口瞬时大于面板，静止时窗口=面板
//!   ③ 逐帧跟随  窗高 = 内容动画值，每帧发 InnerSize —— 现实现的做法（对照）
//!
//! 变体 G：`NSView.layerContentsPlacement = TopLeft`（resize 时旧帧钉左上、裁切而非缩放）。
//! 若伪影在 ①/② 下消失，则两式均可用；启动时打印 wantsLayer 等字段留证。
//!
//! 数值与配色取自设计稿导出（floatbar-visual 导出-悬浮条-深色/空态 .html）：
//! 面板宽 700 / 搜索行 64 / 行 56 间距 2 / 结果区上下 8 / 页脚 38 / 圆角 12 / 空态区 72；
//! 行数 0..=6 → 面板高 176..466（每行 +58）。
//!
//! 键位：空格 行数循环 0→6→0 · ↑/↓ 行数 ±1 · 1/2/3 三式 · G 顶左钉住 ·
//!       A 自动循环（900ms 一次变化）· P 暂停 · R 复位 · Q 退出
//! 运行：cargo run --release --manifest-path scripts/poc/poc6_height_resize/Cargo.toml

use std::time::{Duration, Instant};

use eframe::egui;
use objc2::rc::Retained;
use objc2_app_kit::{NSView, NSViewLayerContentsPlacement, NSWindow};
use objc2_foundation::NSPoint;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

const W: f32 = 700.0;
const SEARCH_H: f32 = 64.0;
const ROW_H: f32 = 56.0;
const ROW_GAP: f32 = 2.0;
const LIST_PAD: f32 = 8.0;
const FOOTER_H: f32 = 38.0;
const EMPTY_H: f32 = 72.0;
const MAX_ROWS: usize = 6;
/// 内容动画时长（visual.md §6.4 的建议值 120ms）。
const ANIM_SECS: f32 = 0.120;
/// 自动循环间隔（模拟打字时结果行数变化）。
const AUTO_MS: u64 = 900;

/// 面板高：搜索行 + 分隔线 + 结果区（空态 72 / n 行 56+2 / 封顶 6 行）+ 分隔线 + 页脚。
fn target_h(rows: usize) -> f32 {
    let list = if rows == 0 {
        EMPTY_H
    } else {
        let n = rows.min(MAX_ROWS) as f32;
        (n * ROW_H + (n - 1.0) * ROW_GAP + LIST_PAD * 2.0).min(
            MAX_ROWS as f32 * ROW_H + (MAX_ROWS as f32 - 1.0) * ROW_GAP + LIST_PAD * 2.0,
        )
    };
    SEARCH_H + 1.0 + list + 1.0 + FOOTER_H
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Instant,
    InWindow,
    PerFrame,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Mode::Instant => "① 瞬跳",
            Mode::InWindow => "② 窗内平滑",
            Mode::PerFrame => "③ 逐帧跟随(现状)",
        }
    }
}

/// 自动演示阶段：进入时施加 (mode, gravity, rows)，保持 hold 后推进下一阶段。
#[derive(Clone)]
struct Phase {
    mode: Mode,
    gravity: bool,
    rows: usize,
    hold: Duration,
    label: String,
}

/// 演示脚本：两种放置 × 三式，各走「空态 → 6 行 → 2 行 → 6 行」。
fn demo_script() -> Vec<Phase> {
    let mut v = Vec::new();
    for (gravity, glabel) in [(false, "默认放置"), (true, "顶左钉住")] {
        for (mode, mlabel) in [
            (Mode::PerFrame, "③ 逐帧跟随（现状）"),
            (Mode::Instant, "① 瞬跳"),
            (Mode::InWindow, "② 窗内平滑"),
        ] {
            for (rows, rlabel) in [
                (0usize, "空态"),
                (6, "长到 6 行"),
                (2, "收到 2 行"),
                (6, "再长到 6 行"),
            ] {
                v.push(Phase {
                    mode,
                    gravity,
                    rows,
                    hold: Duration::from_millis(1300),
                    label: format!("{mlabel} · {glabel} · {rlabel}"),
                });
            }
        }
    }
    v
}

const NAMES: [&str; 6] = [
    "年度工作报告.docx",
    "财务报告-Q3.xlsx",
    "调研报告-初稿.md",
    "项目进展报告.pdf",
    "报告归档",
    "培训报告.pptx",
];
const DIRS: [&str; 6] = [
    "~/Documents/工作",
    "~/Documents/财务",
    "~/Documents/工作/调研",
    "~/Documents/工作/项目",
    "~/Documents/工作",
    "~/Documents/培训",
];
const HINTS: [(&str, &str); 4] = [("隐藏", "esc"), ("完整窗口", "⌥↵"), ("访达", "⌘↵"), ("打开", "↵")];

fn rgba(r: u8, g: u8, b: u8, a: u8) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(r, g, b, a)
}

struct PocApp {
    mode: Mode,
    rows: usize,
    /// 内容动画当前高（窗内绘制用；③ 下即窗高输出）。
    drawn_h: f32,
    /// 已下发的窗口高。
    win_h: f32,
    /// (from, to, start)。
    anim: Option<(f32, f32, Instant)>,
    gravity: bool,
    auto: bool,
    last_auto: Instant,
    resize_count: u32,
    win: Option<Retained<NSWindow>>,
    view: Option<Retained<NSView>>,
    /// 顶边钉住锚点：(x, top_y)，Cocoa 屏幕坐标。
    anchor: Option<(f64, f64)>,
    anchor_h: f64,
    diag_logged: bool,
    /// 自动演示（默认开；D 切换，任何手动键接管）。
    demo: bool,
    phases: Vec<Phase>,
    phase_idx: usize,
    phase_start: Option<Instant>,
    /// 阶段起点的 resize 计数（结算用）。
    phase_resize_base: u32,
    phase_label: String,
    /// 进程起点（日志时间戳）。
    t0: Instant,
}

impl PocApp {
    fn new() -> Self {
        // 定格模式（探针驱动用，绕过键盘）：
        //   POC6_MODE=instant|inwindow|perframe 定格某式（关闭自动演示轮播）
        //   POC6_G=1   顶左钉住；POC6_AUTO=1 定格下自动循环行数
        let pinned = std::env::var("POC6_MODE").ok().and_then(|m| match m.as_str() {
            "instant" => Some(Mode::Instant),
            "inwindow" => Some(Mode::InWindow),
            "perframe" => Some(Mode::PerFrame),
            _ => None,
        });
        let env_on = |k: &str| std::env::var(k).is_ok_and(|v| v == "1");
        let demo = pinned.is_none();
        let mode = pinned.unwrap_or(Mode::Instant);
        Self {
            mode,
            rows: 0,
            drawn_h: target_h(0),
            win_h: target_h(0),
            anim: None,
            gravity: env_on("POC6_G"),
            auto: env_on("POC6_AUTO"),
            last_auto: Instant::now(),
            resize_count: 0,
            win: None,
            view: None,
            anchor: None,
            anchor_h: 0.0,
            diag_logged: false,
            demo,
            phases: demo_script(),
            phase_idx: 0,
            phase_start: None,
            phase_resize_base: 0,
            phase_label: "启动中".to_string(),
            t0: Instant::now(),
        }
    }

    fn set_rows(&mut self, rows: usize) {
        if rows == self.rows {
            return;
        }
        self.rows = rows;
        let target = target_h(rows);
        match self.mode {
            Mode::Instant => {
                self.anim = None;
                self.drawn_h = target;
            }
            _ => self.anim = Some((self.drawn_h, target, Instant::now())),
        }
    }

    fn set_mode(&mut self, m: Mode) {
        self.mode = m;
        let target = target_h(self.rows);
        match m {
            Mode::Instant => {
                self.anim = None;
                self.drawn_h = target;
            }
            _ => self.anim = Some((self.drawn_h, target, Instant::now())),
        }
    }

    fn set_gravity(&mut self, on: bool) {
        self.gravity = on;
        if let Some(v) = &self.view {
            let val = if on {
                NSViewLayerContentsPlacement::TopLeft
            } else {
                NSViewLayerContentsPlacement::ScaleAxesIndependently
            };
            v.setLayerContentsPlacement(val);
            eprintln!(
                "[poc6] 顶左钉住 = {}（layerContentsPlacement 读回 {}）",
                on,
                v.layerContentsPlacement().0
            );
        } else {
            eprintln!("[poc6] 顶左钉住 = {on}（无 NSView 句柄，仅记状态）");
        }
    }

    fn send_size(&mut self, ctx: &egui::Context, h: f32) {
        ctx.send_viewport_cmd_to(
            egui::ViewportId::ROOT,
            egui::ViewportCommand::InnerSize(egui::vec2(W, h)),
        );
        self.win_h = h;
        self.resize_count += 1;
        eprintln!(
            "[poc6] resize #{:<3} → 窗高 {h:.0} @{}ms（{} · {} 行）",
            self.resize_count,
            self.t0.elapsed().as_millis(),
            self.mode.label(),
            self.rows
        );
    }

    fn step_anim(&mut self, now: Instant) {
        if let Some((from, to, start)) = self.anim {
            let p = (now.duration_since(start).as_secs_f32() / ANIM_SECS).min(1.0);
            let e = p * p * (3.0 - 2.0 * p);
            self.drawn_h = from + (to - from) * e;
            if p >= 1.0 {
                self.drawn_h = to;
                self.anim = None;
            }
        }
    }

    /// 顶边钉住：resize 后把原点下移，保持顶边不动（同 app `platform::reanchor`）。
    fn reanchor(&mut self) {
        let Some(win) = self.win.clone() else { return };
        let f = win.frame();
        let h = f.size.height;
        let (x, top) = *self
            .anchor
            .get_or_insert((f.origin.x, f.origin.y + f.size.height));
        if (h - self.anchor_h).abs() >= 0.5 {
            win.setFrameOrigin(NSPoint::new(x, top - h));
            self.anchor_h = h;
        }
    }

    fn log_diag(&mut self) {
        if self.diag_logged {
            return;
        }
        let (Some(w), Some(v)) = (&self.win, &self.view) else {
            return;
        };
        let f = w.frame();
        eprintln!(
            "[poc6] 诊断：wantsLayer={} · layerContentsPlacement={} · redrawPolicy={} · 窗 frame origin=({:.0},{:.0}) size={:.0}x{:.0}",
            v.wantsLayer(),
            v.layerContentsPlacement().0,
            v.layerContentsRedrawPolicy().0,
            f.origin.x,
            f.origin.y,
            f.size.width,
            f.size.height
        );
        self.diag_logged = true;
    }

    /// 直接画在根 ui 上：eframe 0.37 的 `App::ui` 无默认背景（透明由 ViewportBuilder 提供）。
    fn draw(&mut self, ui: &mut egui::Ui) {
        let now = Instant::now();
        let o = ui.max_rect().min;
        let clip = egui::Rect::from_min_size(o, egui::vec2(W, self.drawn_h));
        let p = ui.painter().with_clip_rect(clip);
        let radius = egui::CornerRadius::same(12);

        // 面板底 + 内描边（导出稿：#191C21CC / 1px #FFFFFF14 / 圆角 12）
        let panel = egui::Rect::from_min_size(o, egui::vec2(W, self.drawn_h));
        p.rect_filled(panel, radius, rgba(0x19, 0x1C, 0x21, 0xCC));
        p.rect_stroke(
            panel,
            radius,
            egui::Stroke::new(1.0, rgba(0xFF, 0xFF, 0xFF, 0x14)),
            egui::StrokeKind::Inside,
        );

        self.draw_search(&p, o);
        p.rect_filled(
            egui::Rect::from_min_size(o + egui::vec2(0.0, SEARCH_H), egui::vec2(W, 1.0)),
            0,
            rgba(0xFF, 0xFF, 0xFF, 0x14),
        );

        if self.rows == 0 {
            self.draw_empty(&p, o);
        } else {
            self.draw_rows(&p, o);
        }

        let foot_y = o.y + SEARCH_H + 1.0 + list_h(self.rows) + 1.0;
        p.rect_filled(
            egui::Rect::from_min_size(egui::pos2(o.x, foot_y), egui::vec2(W, 1.0)),
            0,
            rgba(0xFF, 0xFF, 0xFF, 0x14),
        );
        self.draw_footer(&p, o, foot_y);

        // 演示横幅（探针专用浮层：当前正在播放哪一式 / 哪种放置）
        let banner = if self.demo {
            format!("自动演示 · {}（D 暂停 · 按键接管）", self.phase_label)
        } else {
            format!(
                "手动 · {} · {}",
                self.mode.label(),
                if self.gravity { "顶左钉住" } else { "默认放置" }
            )
        };
        let bw = measure(&p, &banner, 12.0) + 22.0;
        let chip = egui::Rect::from_center_size(
            egui::pos2(o.x + W / 2.0, o.y + 16.0),
            egui::vec2(bw, 22.0),
        );
        p.rect_filled(chip, egui::CornerRadius::same(11), rgba(0x0A, 0x84, 0xFF, 0xE6));
        p.text(
            chip.center(),
            egui::Align2::CENTER_CENTER,
            &banner,
            egui::FontId::proportional(12.0),
            egui::Color32::WHITE,
        );
    }

    fn draw_search(&self, p: &egui::Painter, o: egui::Pos2) {
        let cy = o.y + SEARCH_H / 2.0;
        // 搜索图标（20×20 位置）+ 光标占位
        let ic = egui::pos2(o.x + 30.0, cy);
        p.circle_stroke(ic, 6.5, egui::Stroke::new(1.8, rgba(0x8B, 0x92, 0x9A, 0xFF)));
        p.line_segment(
            [ic + egui::vec2(4.8, 4.8), ic + egui::vec2(9.0, 9.0)],
            egui::Stroke::new(1.8, rgba(0x8B, 0x92, 0x9A, 0xFF)),
        );
        // 打开完整窗口按钮（28×28 / #FFFFFF1F / 圆角 7）
        let btn = egui::Rect::from_min_size(
            egui::pos2(o.x + W - 20.0 - 28.0, cy - 14.0),
            egui::vec2(28.0, 28.0),
        );
        p.rect_filled(btn, egui::CornerRadius::same(7), rgba(0xFF, 0xFF, 0xFF, 0x1F));
        let bc = btn.center();
        let s = egui::Stroke::new(1.6, rgba(0xE8, 0xEA, 0xED, 0xFF));
        p.line_segment([bc + egui::vec2(-4.0, -4.0), bc + egui::vec2(2.5, 2.5)], s);
        p.line_segment([bc + egui::vec2(2.5, -4.5), bc + egui::vec2(2.5, 2.5)], s);
        p.line_segment([bc + egui::vec2(-4.5, 2.5), bc + egui::vec2(2.5, 2.5)], s);

        // HUD（探针专用：模式 · 行数 · 窗高 · resize 次数 · 顶左钉住态）
        let hud = format!(
            "{} · 行 {}/6 · 窗高 {:.0} · resize×{} · {}",
            self.mode.label(),
            self.rows,
            self.win_h,
            self.resize_count,
            if self.gravity { "顶左钉住" } else { "默认放置" }
        );
        p.text(
            egui::pos2(o.x + W / 2.0, cy),
            egui::Align2::CENTER_CENTER,
            hud,
            egui::FontId::proportional(13.0),
            rgba(0x8B, 0x92, 0x9A, 0xFF),
        );
    }

    fn draw_rows(&self, p: &egui::Painter, o: egui::Pos2) {
        let n = self.rows.min(MAX_ROWS);
        for i in 0..n {
            let y = o.y + SEARCH_H + 1.0 + LIST_PAD + i as f32 * (ROW_H + ROW_GAP);
            let row = egui::Rect::from_min_size(
                egui::pos2(o.x + LIST_PAD, y),
                egui::vec2(W - LIST_PAD * 2.0, ROW_H),
            );
            let selected = i == 0;
            if selected {
                p.rect_filled(row, egui::CornerRadius::same(8), rgba(0xFF, 0xFF, 0xFF, 0x1F));
            }
            let icon = egui::Rect::from_min_size(
                egui::pos2(row.min.x + 12.0, row.center().y - 16.0),
                egui::vec2(32.0, 32.0),
            );
            p.rect_filled(icon, egui::CornerRadius::same(6), rgba(0xFF, 0xFF, 0xFF, 0x14));
            let ext = NAMES[i].rsplit('.').next().unwrap_or("·").to_uppercase();
            let ext = if NAMES[i].contains('.') { ext } else { "DIR".to_string() };
            p.text(
                icon.center(),
                egui::Align2::CENTER_CENTER,
                &ext,
                egui::FontId::proportional(9.0),
                rgba(0xC6, 0xCB, 0xD1, 0xFF),
            );
            let tx = icon.max.x + 12.0;
            p.text(
                egui::pos2(tx, row.center().y - 10.0),
                egui::Align2::LEFT_CENTER,
                NAMES[i],
                egui::FontId::proportional(15.0),
                if selected { rgba(0xFF, 0xFF, 0xFF, 0xFF) } else { rgba(0xE4, 0xE7, 0xEB, 0xFF) },
            );
            p.text(
                egui::pos2(tx, row.center().y + 10.0),
                egui::Align2::LEFT_CENTER,
                DIRS[i],
                egui::FontId::proportional(12.0),
                rgba(0xA2, 0xAA, 0xB4, 0xFF),
            );
            if selected {
                p.text(
                    egui::pos2(row.max.x - 12.0, row.center().y),
                    egui::Align2::RIGHT_CENTER,
                    "打开",
                    egui::FontId::proportional(12.0),
                    rgba(0xC6, 0xCB, 0xD1, 0xFF),
                );
            }
        }
    }

    fn draw_empty(&self, p: &egui::Painter, o: egui::Pos2) {
        let c = egui::pos2(o.x + W / 2.0, o.y + SEARCH_H + 1.0 + EMPTY_H / 2.0);
        p.text(
            egui::pos2(c.x, c.y - 10.0),
            egui::Align2::CENTER_CENTER,
            "输入关键词开始搜索",
            egui::FontId::proportional(14.0),
            rgba(0xE4, 0xE7, 0xEB, 0xFF),
        );
        p.text(
            egui::pos2(c.x, c.y + 12.0),
            egui::Align2::CENTER_CENTER,
            "可搜文件名，也可搜路径",
            egui::FontId::proportional(12.0),
            rgba(0xA2, 0xAA, 0xB4, 0xFF),
        );
    }

    fn draw_footer(&self, p: &egui::Painter, o: egui::Pos2, foot_y: f32) {
        // 页脚底 #FFFFFF0F
        p.rect_filled(
            egui::Rect::from_min_size(egui::pos2(o.x, foot_y), egui::vec2(W, FOOTER_H)),
            0,
            rgba(0xFF, 0xFF, 0xFF, 0x0F),
        );
        let cy = foot_y + FOOTER_H / 2.0;
        if self.rows > 0 {
            p.text(
                egui::pos2(o.x + 16.0, cy),
                egui::Align2::LEFT_CENTER,
                &format!("共 {} 项", self.rows),
                egui::FontId::proportional(12.0),
                rgba(0x8B, 0x92, 0x9A, 0xFF),
            );
        }
        // 右侧键位提示：从右往左摆（组间距 16）
        let mut cursor = o.x + W - 16.0;
        for (label, key) in HINTS.iter() {
            let kw = measure(p, key, 11.0) + 12.0;
            let chip = egui::Rect::from_center_size(
                egui::pos2(cursor - kw / 2.0, cy),
                egui::vec2(kw, 19.0),
            );
            p.rect_filled(chip, egui::CornerRadius::same(4), rgba(0xFF, 0xFF, 0xFF, 0x1A));
            p.text(
                chip.center(),
                egui::Align2::CENTER_CENTER,
                key,
                egui::FontId::proportional(11.0),
                rgba(0xC6, 0xCB, 0xD1, 0xFF),
            );
            let lw = measure(p, label, 12.0);
            p.text(
                egui::pos2(cursor - kw - 6.0, cy),
                egui::Align2::RIGHT_CENTER,
                label,
                egui::FontId::proportional(12.0),
                rgba(0x8B, 0x92, 0x9A, 0xFF),
            );
            cursor -= kw + 6.0 + lw + 16.0;
        }
    }
}

fn list_h(rows: usize) -> f32 {
    target_h(rows) - SEARCH_H - 1.0 - 1.0 - FOOTER_H
}

fn measure(p: &egui::Painter, s: &str, size: f32) -> f32 {
    p.layout_no_wrap(
        s.to_owned(),
        egui::FontId::proportional(size),
        egui::Color32::WHITE,
    )
    .size()
    .x
}

impl eframe::App for PocApp {
    /// 每帧驱动（输入 / 三式高度策略 / 重锚 / 诊断）。绘制在 `ui()`。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = Instant::now();

        // —— 输入 ——
        let (space, up, down, m1, m2, m3, g, a, p_key, r, q, d_key) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Num1),
                i.key_pressed(egui::Key::Num2),
                i.key_pressed(egui::Key::Num3),
                i.key_pressed(egui::Key::G),
                i.key_pressed(egui::Key::A),
                i.key_pressed(egui::Key::P),
                i.key_pressed(egui::Key::R),
                i.key_pressed(egui::Key::Q),
                i.key_pressed(egui::Key::D),
            )
        });
        if q {
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
        }
        if space {
            self.set_rows((self.rows + 1) % (MAX_ROWS + 1));
        }
        if up {
            self.set_rows(self.rows.saturating_sub(1));
        }
        if down {
            self.set_rows((self.rows + 1).min(MAX_ROWS));
        }
        if m1 {
            self.set_mode(Mode::Instant);
        }
        if m2 {
            self.set_mode(Mode::InWindow);
        }
        if m3 {
            self.set_mode(Mode::PerFrame);
        }
        if g {
            self.set_gravity(!self.gravity);
        }
        if a {
            self.auto = !self.auto;
            self.last_auto = now;
            eprintln!("[poc6] 自动循环 = {}", self.auto);
        }
        if p_key {
            self.auto = false;
            eprintln!("[poc6] 自动循环暂停");
        }
        if r {
            self.auto = false;
            self.mode = Mode::Instant;
            self.rows = 0;
            self.anim = None;
            self.drawn_h = target_h(0);
            self.anchor = None; // 重新锚定
        }
        if self.auto && now.duration_since(self.last_auto) >= Duration::from_millis(AUTO_MS) {
            self.last_auto = now;
            self.set_rows((self.rows + 1) % (MAX_ROWS + 1));
        }

        // —— 自动演示（默认开：D 切换；任何手动键即接管）——
        if space || up || down || m1 || m2 || m3 || g || a || p_key || r {
            if self.demo {
                self.demo = false;
                eprintln!("[poc6] 演示暂停：手动接管");
            }
        }
        if d_key {
            self.demo = !self.demo;
            self.phase_start = None;
            eprintln!("[poc6] 自动演示 = {}", self.demo);
        }
        if self.demo {
            let hold = self.phases[self.phase_idx].hold;
            let expired = self.phase_start.is_none_or(|s| now.duration_since(s) >= hold);
            if expired {
                if let Some(s) = self.phase_start {
                    eprintln!(
                        "[poc6] ↑ 阶段结算：历时 {} ms · resize {} 次",
                        now.duration_since(s).as_millis(),
                        self.resize_count - self.phase_resize_base
                    );
                }
                let ph = self.phases[self.phase_idx].clone();
                self.set_mode(ph.mode);
                self.set_gravity(ph.gravity);
                self.set_rows(ph.rows);
                eprintln!(
                    "[poc6] 演示 {}/{}：{} · 行 {}",
                    self.phase_idx + 1,
                    self.phases.len(),
                    ph.label,
                    ph.rows
                );
                self.phase_label = ph.label.clone();
                self.phase_start = Some(now);
                self.phase_resize_base = self.resize_count;
                self.phase_idx = (self.phase_idx + 1) % self.phases.len();
            }
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        // —— 三式的高度/窗口驱动 ——
        let target = target_h(self.rows);
        match self.mode {
            Mode::Instant => {
                if (self.win_h - target).abs() > 0.5 {
                    self.send_size(ctx, target);
                }
            }
            Mode::InWindow => {
                // 增长：窗口先撑到目标（1 次 resize），内容动画在窗内绘制
                if target > self.win_h + 0.5 {
                    self.send_size(ctx, target);
                }
                self.step_anim(now);
                // 收缩：动画完成且内容已到位，才把窗口收到目标（1 次 resize）
                if target < self.win_h - 0.5
                    && self.anim.is_none()
                    && (self.drawn_h - target).abs() < 0.5
                {
                    self.send_size(ctx, target);
                }
            }
            Mode::PerFrame => {
                self.step_anim(now);
                if (self.win_h - self.drawn_h).abs() > 0.5 {
                    self.send_size(ctx, self.drawn_h);
                }
            }
        }

        self.reanchor();
        self.log_diag();

        if self.anim.is_some() || self.auto {
            ctx.request_repaint();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

/// 主路径（同 poc1b）：`cc.window_handle()` → NSView → `NSView::window()`。
fn ns_window_from_cc(cc: &eframe::CreationContext<'_>) -> Result<Retained<NSWindow>, String> {
    let handle = cc
        .window_handle()
        .map_err(|e| format!("window_handle(): {e}"))?;
    match handle.as_raw() {
        RawWindowHandle::AppKit(h) => {
            // SAFETY: ns_view 指向 winit 为本 app 创建、当前存活的 NSView（主线程持有），
            // 此处只借读出其所属 NSWindow，不改变所有权。
            let view: &NSView = unsafe { h.ns_view.cast::<NSView>().as_ref() };
            view.window()
                .ok_or_else(|| "NSView::window() 返回 None".to_string())
        }
        other => Err(format!("非 AppKit 窗口句柄：{other:?}")),
    }
}

fn main() -> eframe::Result<()> {
    eprintln!(
        "[poc6] 自动演示默认开启（两种放置 × 三式 循环）。键位：D 暂停/继续演示 · 空格 行数 · ↑/↓ · 1/2/3 三式 · G 顶左钉住 · A 自动循环 · P 暂停 · R 复位 · Q 退出"
    );
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([W, target_h(0)])
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_has_shadow(false)
        .with_position([300.0, 140.0]);
    let opts = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "poc6-height-resize",
        opts,
        Box::new(|cc| {
            let mut app = PocApp::new();
            match ns_window_from_cc(cc) {
                Ok(w) => {
                    app.view = w.contentView();
                    app.win = Some(w);
                    if app.gravity {
                        app.set_gravity(true); // 把 POC6_G=1 落到 NSView 上
                    }
                }
                Err(e) => {
                    eprintln!("[poc6] 取窗失败：{e}（顶左钉住变体不可用，其余照常）");
                }
            }
            Ok(Box::new(app))
        }),
    )
}
