//! 诊断 harness:验证「每次 glTexSubImage2D 上传是否产生一次驱动侧大块分配」。
//!
//! 真实窗口 + glow 后端;启动时建一张 4096×4096 纹理,之后**每帧**用
//! `TextureHandle::set_partial` 上传一条 4096×256（4MB）子区域(纵向轮转),
//! 持续 `MCE_PROBE_SECS` 秒。外部采样 `footprint -f bytes` 的
//! `Malloc Large (empty)` 区域数:若随上传次数线性增长,则「每次上传一次分配」
//! 成立;停止上传后观察是否回落(驱动延迟释放)。
//!
//! 用途:决定是否值得给 egui_glow 打 PBO 复用补丁(见分支报告)。

use std::time::{Duration, Instant};

use eframe::egui;

const TEX_W: usize = 4096;
const TEX_H: usize = 4096;
const STRIP_H: usize = 256;

struct Probe {
    started: Instant,
    tex: Option<egui::TextureHandle>,
    y: usize,
    frames: u64,
    last_log: Instant,
    warmed: bool,
    /// `MCE_PROBE_STOP_AT`:提前停止上传(验证「停后是否回落」)。
    stop_at: Option<Duration>,
}

impl eframe::App for Probe {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let elapsed = self.started.elapsed();
        let stop_at = self.stop_at;
        if elapsed > probe_secs() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if self.tex.is_none() {
            // 一次性建 4096×4096 纹理(全透明)。
            let img = egui::ColorImage::new(
                [TEX_W, TEX_H],
                vec![egui::Color32::TRANSPARENT; TEX_W * TEX_H],
            );
            self.tex = Some(ctx.load_texture("probe", img, egui::TextureOptions::LINEAR));
        }
        let uploading = stop_at.is_none_or(|s| elapsed < s);
        if mode() == "malloc" && uploading {
            // 纯 CPU 实验:每帧分配一块**尺寸不同**的大内存并立刻释放,复现
            // 「malloc 按尺寸类缓存 VM 段 → Malloc Large (empty) 累积」。
            // 尺寸模拟 4096×h×4(h 每帧 +1 像素):4MB 起步,逐帧变大。
            let mut h = 256 + (self.frames as usize % 2500);
            if quantized() {
                // 量化到 2 的幂尺寸类(拟改 epaint take_delta:partial 高度向上取整)
                h = h.next_power_of_two().min(TEX_H);
            }
            let mut v: Vec<u8> = vec![1u8; TEX_W * h * 4];
            v[0] = 2;
            std::hint::black_box(&v);
            drop(v);
            self.frames += 1;
            self.warmed = true;
        } else if uploading {
            if let Some(tex) = &mut self.tex {
                let v = (self.frames % 255) as u8;
                if resize_mode() {
                    // 模拟字形图集 clear+重长:每帧按新高度**重新指定存储**
                    // (glTexImage2D 全量),高度在 256..4096 间循环。
                    const HEIGHTS: [usize; 5] = [256, 512, 1024, 2048, 4096];
                    let h = HEIGHTS[(self.frames as usize / 2) % HEIGHTS.len()];
                    let img = egui::ColorImage::new(
                        [TEX_W, h],
                        vec![egui::Color32::from_rgb(v, v, v); TEX_W * h],
                    );
                    tex.set(img, egui::TextureOptions::LINEAR);
                } else {
                    // 每帧 partial 上传;高度**变化**(模拟字形图集每帧新增的
                    // 行数不定:4096×256/632/1024×4 = 4/9.9/16MB)。
                    const HEIGHTS: [usize; 3] = [256, 632, 1024];
                    let h = if varied() {
                        HEIGHTS[(self.frames as usize) % HEIGHTS.len()]
                    } else {
                        STRIP_H
                    };
                    let img = egui::ColorImage::new(
                        [TEX_W, h],
                        vec![egui::Color32::from_rgb(v, v, v); TEX_W * h],
                    );
                    self.y = self.y.min(TEX_H - h);
                    tex.set_partial([0, self.y], img, egui::TextureOptions::LINEAR);
                    self.y = (self.y + h) % (TEX_H - h + 1);
                }
                self.frames += 1;
            }
        }
        if self.last_log.elapsed() >= Duration::from_secs(2) {
            self.last_log = Instant::now();
            eprintln!(
                "PROBE t={:?} frames={} uploading={uploading}",
                elapsed, self.frames
            );
        }
        // 关键:本帧**采样**该纹理(等同字体图集被绘制)。若驱动为「上传且被采样」
        // 的纹理临时分配 staging,只上传不绘制是测不出来的。
        if let Some(tex) = &self.tex {
            let rect = egui::Rect::from_min_size(
                ui.min_rect().min + egui::vec2(8.0, 40.0),
                egui::vec2(200.0, 120.0),
            );
            ui.painter().image(
                tex.id(),
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(0.05, 0.03)),
                egui::Color32::WHITE,
            );
        }
        ui.label(format!("probe frames={} y={}", self.frames, self.y));
        ctx.request_repaint();
    }
}

fn quantized() -> bool {
    std::env::var_os("MCE_PROBE_QUANT").is_some()
}

fn mode() -> String {
    std::env::var("MCE_PROBE_MODE").unwrap_or_else(|_| "partial".to_owned())
}

fn varied() -> bool {
    std::env::var_os("MCE_PROBE_VARIED").is_some()
}

fn resize_mode() -> bool {
    std::env::var_os("MCE_PROBE_MODE")
        .map(|v| v == "resize")
        .unwrap_or(false)
}

fn probe_secs() -> Duration {
    std::env::var("MCE_PROBE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(60))
}

fn main() -> eframe::Result<()> {
    let stop_at = std::env::var("MCE_PROBE_STOP_AT")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_secs);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("tex_upload_probe")
            .with_inner_size([640.0, 400.0])
            .with_active(false),
        ..Default::default()
    };
    eframe::run_native(
        "tex_upload_probe",
        options,
        Box::new(move |_cc| {
            eprintln!(
                "PROBE upload {}x{} strip/frame={}KB for {}s (stop_at={:?})",
                TEX_W,
                TEX_H,
                TEX_W * STRIP_H * 4 / 1024,
                probe_secs().as_secs(),
                stop_at
            );
            Ok(Box::new(Probe {
                started: Instant::now(),
                tex: None,
                y: 0,
                frames: 0,
                last_log: Instant::now(),
                warmed: false,
                stop_at,
            }))
        }),
    )
}
