//! 峰值对比 harness(图集 churn 修复的决策依据;不入产物)。
//!
//! 复现 A6b 触发面:结果窗口式虚拟化长列表在**程序内**逐帧推进滚动——
//! 不合成任何 OS 键鼠事件,不触碰桌面。每滚动过一屏引入新字形 → 图集增长
//! → 达 0.8 填充率整图集 clear + 全文本重排 + 纹理重烘。
//!
//! 环境变量:
//! - `MCE_HARNESS_SECS`   运行秒数(默认 80)
//! - `MCE_HARNESS_WARM=1` 首帧用 `atlas_charset.txt` 预热图集(≈生产常用字形)
//! - `MCE_HARNESS_REALISTIC=1` 文件名取自预热字符集(≈真实文件名),否则取
//!   BMP 汉字全区随机(对抗性长尾)
//! - `MCE_HARNESS_NOSHOT=1` 关闭 10s 截图(测量时关,避免额外分配)
//! - `MCE_HARNESS_PXPS`   滚动速度(px/s,默认 1500);对抗测量用 30000
//! - `MCE_HARNESS_NAMELEN` 每行字符数(默认 8);现场长文件名用 32
//! - `MCE_HARNESS_FPS`    帧率上限(活动窗口下避免跑到数百 fps)
//! - `MCE_HARNESS_ACTIVE=1` 窗口抢占前台(遮挡的窗口可能不再出帧)
//! - `MCE_HARNESS_CAP`    图集边长上限(默认 4096;2048 对照)
//!
//! 每 5s 输出:图集尺寸、填充率、**clear 次数**(填充率从 >0.5 骤降到 <0.2 视为
//! 一次 clear;跨 clear 也可由图集高度回落交叉验证)。退出时输出 vendor 补丁的
//! 诊断计数:`image_clones`(整图深拷贝次数,应为 0)与 `upload_allocs`
//! (部分上传缓冲的新分配次数,稳定后应只随尺寸类增加)。

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;

const ROWS: usize = 20_000;
/// 滚动速度(px/秒,按 **时间** 推进而非逐帧固定像素——实测框架帧率会随
/// 窗口可见性/遮挡在 60–560fps 间漂移,逐帧步进会让跨后端/跨次测量不可比)。
/// 1500px/s ≈ 50 行/秒,与快速拖动滚动条同量级。
fn scroll_speed() -> f32 {
    std::env::var("MCE_HARNESS_PXPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1500.0)
}

const CHARSET: &str = include_str!("../src/ui/atlas_charset.txt");
const SC: &[u8] = include_bytes!("../../../assets/fonts/NotoSansSC-Regular.otf");
const KR: &[u8] = include_bytes!("../../../assets/fonts/NotoSansKR-Regular.otf");
const SYM: &[u8] = include_bytes!("../../../assets/fonts/NotoSansSymbols-Regular.ttf");
const SYM2: &[u8] = include_bytes!("../../../assets/fonts/NotoSansSymbols2-Regular.ttf");

fn env_flag(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

fn duration() -> Duration {
    std::env::var("MCE_HARNESS_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(80))
}

/// 与生产同序的字体链(SF Pro/SF Mono mmap + 内置四字体;见 ui/mod.rs)。
fn install_fonts(ctx: &egui::Context) {
    for (name, bytes, priority) in [
        ("Noto Sans SC", SC, egui::FontPriority::Highest),
        ("Noto Sans KR", KR, egui::FontPriority::Lowest),
        ("Noto Sans Symbols", SYM, egui::FontPriority::Lowest),
        ("Noto Sans Symbols 2", SYM2, egui::FontPriority::Lowest),
    ] {
        ctx.add_font(egui::FontInsert::new(
            name,
            egui::FontData::from_static(bytes),
            vec![
                egui::InsertFontFamily {
                    family: egui::FontFamily::Proportional,
                    priority,
                },
                egui::InsertFontFamily {
                    family: egui::FontFamily::Monospace,
                    priority,
                },
            ],
        ));
    }
    for (name, path) in [
        ("SF Pro", "/System/Library/Fonts/SFNS.ttf"),
        ("SF Mono", "/System/Library/Fonts/SFNSMono.ttf"),
    ] {
        if let Ok(file) = std::fs::File::open(path) {
            // SAFETY:只读映射,Blob 持有保活。
            if let Ok(mmap) = unsafe { memmap2::Mmap::map(&file) } {
                ctx.add_font(egui::FontInsert::new(
                    name,
                    egui::FontData::from_blob(Arc::new(mmap), 0),
                    vec![egui::InsertFontFamily {
                        family: egui::FontFamily::Proportional,
                        priority: egui::FontPriority::Highest,
                    }],
                ));
            }
        }
    }
}

/// 用 `atlas_charset.txt`(GB2312 一级汉字 + 标点/假名/符号)按生产字号
/// 一次性布局,模拟「常用字形已在图集里」的起点。
fn warm(ctx: &egui::Context) -> ([usize; 2], f32, Duration) {
    let started = Instant::now();
    let chars: Vec<char> = CHARSET.chars().filter(|c| !c.is_whitespace()).collect();
    let ids = [
        egui::FontId::proportional(15.0),
        egui::FontId::proportional(12.0),
    ];
    ctx.fonts_mut(|f| {
        for id in &ids {
            for chunk in chars.chunks(256) {
                let text: String = chunk.iter().collect();
                let _ = f.layout(text, id.clone(), egui::Color32::WHITE, f32::INFINITY);
            }
        }
    });
    (
        ctx.fonts(|f| f.font_image_size()),
        ctx.fonts(|f| f.font_atlas_fill_ratio()),
        started.elapsed(),
    )
}

/// 生成 2 万行文件名。
///
/// - realistic:每行字符从预热字符集里按大步长取样(≈真实文件名:常用字为主,
///   偶有新字形);
/// - random(对抗):BMP 汉字全区随机,每行 `MCE_HARNESS_NAMELEN` 个互不相同的
///   字——持续引入新字形,用于给修复效果一个下界。
fn rows(realistic: bool) -> Vec<String> {
    const HAN_START: u32 = 0x4E00;
    const HAN_COUNT: u32 = 20_902;
    // 每行字符数:真实文件名远超 8 字;行越长,单帧新栅格化的字形越多,
    // 图集脏区越接近现场形态(4096 宽 × 256–632 高)。
    let name_len: u32 = std::env::var("MCE_HARNESS_NAMELEN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    let pool: Vec<char> = CHARSET.chars().filter(|c| !c.is_whitespace()).collect();
    let pick = |seed: u32| -> char {
        if realistic {
            pool[(seed.wrapping_mul(2_654_435_761) as usize) % pool.len()]
        } else {
            char::from_u32(HAN_START + (seed.wrapping_mul(2_654_435_761) % HAN_COUNT))
                .unwrap_or('中')
        }
    };
    (0..ROWS)
        .map(|i| {
            let i = i as u32;
            let mut name = String::new();
            for k in 0..name_len {
                name.push(pick(i.wrapping_mul(131).wrapping_add(k.wrapping_mul(7919))));
            }
            name.push_str(&format!("_Q{i}.docx"));
            name
        })
        .collect()
}

/// 与生产一致:图集边长收到 4096(ui/mod.rs AtlasSideCap 同款插件)。
struct AtlasCap;
impl egui::Plugin for AtlasCap {
    fn debug_name(&self) -> &'static str {
        "atlas-side-cap"
    }
    fn input_hook(&mut self, _ctx: &egui::Context, input: &mut egui::RawInput) {
        let cap: usize = std::env::var("MCE_HARNESS_CAP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4096);
        input.max_texture_side = input.max_texture_side.map(|s| s.min(cap));
    }
}

struct Harness {
    rows: Vec<String>,
    started: Instant,
    offset: f32,
    last_report: Instant,
    shot: bool,
    prev_fill: f32,
    clears: u32,
    warmed: bool,
    warm_on: bool,
    last_frame: Instant,
    frames: u32,
}

impl eframe::App for Harness {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if !self.warmed && self.started.elapsed() < duration() {
            self.warmed = true;
            if self.warm_on {
                let (atlas, fill, elapsed) = warm(&ctx);
                eprintln!("HARNESS warm: atlas={atlas:?} fill={fill:.3} elapsed={elapsed:?}");
            }
        }
        if self.started.elapsed() > duration() {
            use std::sync::atomic::Ordering::Relaxed;
            eprintln!(
                "HARNESS counters image_clones={} upload_allocs={}",
                egui::epaint::MCE_IMAGE_CLONES.load(Relaxed),
                egui::epaint::MCE_UPLOAD_ALLOCS.load(Relaxed),
            );
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        // 时间步进(见 SCROLL_PX_PER_SEC 注释);dt 上限防卡顿帧一次跳过整屏。
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().min(0.05);
        self.last_frame = now;
        self.frames += 1;
        self.offset += scroll_speed() * dt;
        let max = self.rows.len() as f32 * 22.0;
        if self.offset > max {
            self.offset = 0.0;
        }

        let row_height = ui.text_style_height(&egui::TextStyle::Body);
        egui::ScrollArea::vertical()
            .vertical_scroll_offset(self.offset)
            .show_rows(ui, row_height, self.rows.len(), |ui, range| {
                for row in &self.rows[range] {
                    ui.label(row);
                }
            });

        // clear 计数:填充率从高位骤降(整图集 clear 后从 0 重新填)。
        let fill = ctx.fonts(|f| f.font_atlas_fill_ratio());
        if self.prev_fill > 0.5 && fill < self.prev_fill - 0.3 {
            self.clears += 1;
            // 实验:clear 后立刻归还滞留脏页(每轮 clear 的 64MB 级临时分配
            // 若滞留成 Malloc Large (empty),长滚动下会线性累积成 GB 级)。
            if env_flag("MCE_HARNESS_RELIEF") {
                engine::malloc_pressure::release_heap_pressure();
            }
            eprintln!(
                "HARNESS clear#{:<2} t={:?} fill {:.2} -> {:.2}",
                self.clears,
                self.started.elapsed(),
                self.prev_fill,
                fill
            );
        }
        self.prev_fill = fill;

        if self.last_report.elapsed() >= Duration::from_secs(5) {
            self.last_report = Instant::now();
            // 实验:滚动期主动归还 malloc 死页(A6b 峰值的实际形态是
            // Malloc Large (empty) 滞留页,clear 后大块被 free 但不归还内核)。
            if env_flag("MCE_HARNESS_RELIEF") {
                engine::malloc_pressure::release_heap_pressure();
            }
            let (atlas, clears) = (ctx.fonts(|f| f.font_image_size()), self.clears);
            eprintln!(
                "HARNESS t={:?} offset={:.0}/{max:.0} atlas={atlas:?} fill={fill:.3} clears={clears} fps={:.0}",
                self.started.elapsed(),
                self.offset,
                self.frames as f32 / self.started.elapsed().as_secs_f32(),
            );
            if !env_flag("MCE_HARNESS_NOSHOT")
                && !self.shot
                && self.started.elapsed() >= Duration::from_secs(10)
            {
                self.shot = true;
                ctx.request_screenshot(|img| {
                    let mut rgba = Vec::with_capacity(img.pixels.len() * 4);
                    for p in &img.pixels {
                        rgba.extend_from_slice(&[p.r(), p.g(), p.b(), p.a()]);
                    }
                    match image::save_buffer(
                        "/tmp/t5-harness-shot.png",
                        &rgba,
                        img.size[0] as u32,
                        img.size[1] as u32,
                        image::ColorType::Rgba8,
                    ) {
                        Ok(()) => eprintln!("HARNESS screenshot /tmp/t5-harness-shot.png"),
                        Err(e) => eprintln!("HARNESS screenshot failed: {e}"),
                    }
                });
            }
        }
        // 活动窗口下 winit 可能跑到数百 fps;用帧率上限把「每帧新字形数」
        // 拉回现场量级(30000px/s ÷ 65fps ≈ 21 行/帧),使跨次测量可比。
        let fps: Option<f32> = std::env::var("MCE_HARNESS_FPS")
            .ok()
            .and_then(|v| v.parse().ok());
        match fps {
            Some(fps) => ctx.request_repaint_after(std::time::Duration::from_secs_f32(1.0 / fps)),
            None => ctx.request_repaint(),
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("scroll_peak harness")
            .with_inner_size([1080.0, 627.0])
            // 遮挡的窗口可能收不到重绘事件,测量会静默停止推进;需要计时完成时
            // 用 MCE_HARNESS_ACTIVE=1 抢占前台(winit 对隐藏窗口的 App Nap 行为)。
            .with_active(env_flag("MCE_HARNESS_ACTIVE")),
        ..Default::default()
    };
    let warm_on = env_flag("MCE_HARNESS_WARM");
    let realistic = env_flag("MCE_HARNESS_REALISTIC");
    eprintln!(
        "HARNESS mode warm={warm_on} realistic={realistic} secs={}",
        duration().as_secs()
    );
    eframe::run_native(
        "scroll_peak",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.add_plugin(AtlasCap);
            install_fonts(&cc.egui_ctx);
            let now = Instant::now();
            Ok(Box::new(Harness {
                rows: rows(realistic),
                started: now,
                offset: 0.0,
                last_report: now,
                shot: false,
                prev_fill: 0.0,
                clears: 0,
                warmed: false,
                warm_on,
                last_frame: now,
                frames: 0,
            }))
        }),
    )
}
