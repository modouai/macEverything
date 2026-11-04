//! UI 层:四视口壳。
//!
//! 主视口(ROOT)= 完整结果窗口(`ui/results.rs`,Task 17:表格虚拟化/列排序/多选);
//! 第二视口 = 悬浮条(Task 12 UI;Task 14 接平台层:跨 Space/浮动级别/失焦隐藏/多屏定位);
//! 第三视口 = 设置窗口(v2.1:620×600 固定,deferred + 常驻;视口/行为见
//! `ui/settings.rs`,纯视图见 `ui/settings_view.rs`);
//! 第四视口 = 保存面板目录助手浮层(随保存面板出现;视口/行为见 `ui/overlay.rs`,
//! 平台层显隐见 `platform/mac.rs` 的 `overlay_show`/`overlay_hide`)。
//!
//! ## 菜单栏托盘(Task 16)
//!
//! 托盘本体(图标/菜单/事件泵)在 `tray.rs`;本文件只做两件事:
//! ① `logic()` 每帧消费托盘动作(`drain_tray`:`TogglePalette` 与热键共用
//!    `palette_toggle`;`OpenResults` = 唤起完整窗口并聚焦搜索框;
//!    `ToggleAutostart` = SMAppService 注册/注销 + 用户意图标记(Task 20);
//!    `Quit` = 置 `quitting` 后走根窗关闭的干净退出路径,引擎常驻;
//!    `LaunchEngine` 放工作线程)并把最新引擎快照落到菜单项(`TrayRuntime::sync`)。
//! ② 三个带查询词 / 无查询词的「打开完整窗口」入口(托盘/⌥⇧F/悬浮条 ⌥↵)
//!    共用 `results_open` / `show_root_window`。
//!
//! ## 热键动作的主队列直调(Task 21)
//!
//! 热键(⌥Space/⌥⇧F)不走「泵线程叫帧 → logic 消费」——不可见窗口的跨线程
//! repaint 请求被 eframe 节流 ≥100ms,等帧即破「唤起 <100ms」红线。动作在主队列
//! 块里直调本文件的 `palette_toggle` / `results_open`(与帧内同一份实现);
//! `logic` 只消费直调拿不到锁时的兜底队列(细节与证据见 hotkey.rs 模块注释
//! 「动作直调与帧内兜底」;锁纪律见本文件「悬浮条显隐」段)。
//!
//! ## 视口形态(Task 14 定案,L1/L2 处置)
//!
//! 悬浮条用 **deferred 视口 + 常驻**(`show_viewport_deferred`,每次根视口 pass 注册):
//! - L1(immediate 视口只随根视口 pass 渲染 → 主窗被完全遮挡时悬浮条不能唤起/更新):
//!   deferred 视口有独立渲染 pass——自己的窗收到重绘即渲染,不依赖根视口是否被遮挡
//!   (eframe 源码:immediate 视口的事件被重定向给父窗,deferred 视口自行成 pass);
//! - L2(隐藏即销毁视口窗):隐藏走 `ViewportCommand::Visible(false)`(winit 落到
//!   `orderOut`),窗口保留不销毁——状态不丢、无 NSWindow 新建/销毁往返,
//!   也就没有「改类窗口被 winit drop → abort」的销毁面(还原类只留退出点一处)。
//!   窗口创建期 builder 必须逐 pass 一致(`ViewportBuilder::patch` 逐字段比差,
//!   差异会触发命令或重建);定位不依赖 builder 的 position,由平台层原生摆位。
//!
//! ## 显示/隐藏路径(tech 平台行为表「显示路径(定案)」)
//!
//! - 显示:平台层原生 `orderFrontRegardless` + `makeKeyWindow`(主线程;不激活 app、
//!   不切 Space;避开 eframe 对不可见窗口的 100ms 唤醒节流——PoC-4 实测对外 16.7ms);
//! - 隐藏:egui 命令 `Visible(false)`(零 CPU 实测达标);
//! - 失焦隐藏含「切 Space 豁免」判定(Task 14b,product §1 键位表 2026-10-16 定):
//!   判定面在 `ui/palette.rs::PaletteState::draw`(本文件只转发 `Action::Hide`)。
//!
//! ## 排帧纪律(L4)
//!
//! 只在窗可见期排帧:悬浮条 pass、根视口 `logic` 与设置 pass 各自按
//! `CHECK_INTERVAL` 上限自排主题跟随检查(根视口仅在可见时排;任一窗单独可见也须
//! 跟随系统外观——设置窗单独可见时由设置 pass 驱动);
//! 全隐藏期不排帧(隐藏态 CPU 0% 红线);唤起时 `ThemeFollow::check_now_with`
//! 立即应用锁外探测到的外观。
//!
//! 中文字体与覆盖链(2026-11-01 定案 v2;A6b 实测推翻 T5「极生僻字形偶发」假设):
//! 内置 OFL 字体注入 chain,`system_fonts`(fontique)回退**整体移除**。
//! 裁决依据:fontique 单次查询是**固定枚举开销**(MSL 实测 194MB/次,与命中
//! 字体无关);真实文件名长尾(符号/假名/生僻字)不可控,任何一次触发即击穿
//! 内存验收(A6b 实测:纯数字查询 + 滚动 → 峰值 2.4GB,验收线 400MB)。
//! 移除后,覆盖集外的未知字形按 `MissingGlyphPolicy::Tofu`(egui 默认)显示
//! ——纯显示退化,搜索/复制不受影响;彩色 emoji 退为 egui 内置黑白 NotoEmoji。
//! 覆盖链(每 family):SF Pro/SF Mono(拉丁)→ Noto Sans SC(汉字)→
//! egui 默认字体 → Noto Sans KR(韩文)→ Noto Sans Symbols/2(符号 ⌥⌘ 等);
//! KR 与 Symbols 排尾部的机制见 `BUNDLED_FONTS` 注释——长尾由此压到几乎
//! 不出现,扩展 B 级真生僻字显示 tofu。
//! 字节经 `include_bytes!` 进二进制只读段,`FontData::from_static` 零拷贝,
//! 载入无堆分配;图集上限见 `MAX_ATLAS_TEXTURE_SIDE`。
//! `ui()` 留痕 `discovered_fonts`:provider 已不存在,此列表应恒空——非空即
//! 有人重新接入了 provider,按内存回归处理(A6b 教训单点钉住)。

pub mod font;
pub mod hotkey_keys;
pub mod icons;
pub mod idle;
pub mod mascot;
pub mod menu_actions;
pub mod overlay;
pub mod overlay_layout;
pub mod palette;
pub mod results;
pub mod settings;
pub mod settings_layout;
pub mod settings_tokens;
pub mod settings_view;
pub mod theme;
pub mod tokens;
pub mod window_tokens;

use eframe::egui;
use global_hotkey::hotkey::HotKey;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::autostart;
use crate::engine_launcher;
use crate::hotkey;
use crate::i18n;
use crate::platform;
use crate::settings as store;
use crate::single_instance::Incoming;
use crate::tray::{self, TrayMsg};
use crate::update;
use crate::update::UpdatePhase;
use crate::window_state::{self, SizeUpdate, WindowSize, WindowSizeTracker};

/// 内置覆盖字体(全 SIL OFL 1.1,`assets/fonts/`;许可文本两份——
/// `OFL.txt`(noto-cjk:SC/KR)与 `OFL-NotoSansSymbols.txt`(notofonts:
/// Symbols/2),bundle.sh 打包时拷入 `Contents/Resources/fonts/` 随 app 分发,
/// 缺失在脚本自检段硬失败)。
///
/// chain 位置:`SC` 用 `Highest`(排拉丁之后,维持「拉丁 SF Pro、汉字 SC」);
/// `KR/Symbols/Symbols2` 用 `Lowest`(chain **尾部**,egui 默认字体之后)——
/// 韩文与 `⌥⌘` 类符号链上前段都没有,尾部只兜「别人都没有」的字形;
/// KR 自带的 KS 符号/汉字被前段(含 SC 的 20,976 汉字全集)遮蔽,**不改变
/// 现有任何字形的来源**,视觉零回退。
const NOTO_SANS_SC: &[u8] = include_bytes!("../../../../assets/fonts/NotoSansSC-Regular.otf");
const NOTO_SANS_KR: &[u8] = include_bytes!("../../../../assets/fonts/NotoSansKR-Regular.otf");
const NOTO_SYMBOLS: &[u8] = include_bytes!("../../../../assets/fonts/NotoSansSymbols-Regular.ttf");
const NOTO_SYMBOLS2: &[u8] =
    include_bytes!("../../../../assets/fonts/NotoSansSymbols2-Regular.ttf");

/// 内置覆盖字体表(Proportional 与 Monospace 同表)。
/// 顺序 = 注入序,即 Lowest 段的 chain 顺序(KR → Symbols → Symbols2);
/// 覆盖盘点(2026-11-01 fontTools 实测):SC = BMP 汉字 20,976(≈全集)+
/// 假名 + 全角标点;KR = 韩文音节 11,172(全集);Symbols/2 = 846/2,660 符号
/// (⌥⌘⇧⏎✓⚙ 等)。扩展 B 生僻字与彩色 emoji 无覆盖 → tofu / 黑白 emoji。
const BUNDLED_FONTS: [(&str, &[u8], egui::FontPriority); 4] = [
    ("Noto Sans SC", NOTO_SANS_SC, egui::FontPriority::Highest),
    ("Noto Sans KR", NOTO_SANS_KR, egui::FontPriority::Lowest),
    (
        "Noto Sans Symbols",
        NOTO_SYMBOLS,
        egui::FontPriority::Lowest,
    ),
    (
        "Noto Sans Symbols 2",
        NOTO_SYMBOLS2,
        egui::FontPriority::Lowest,
    ),
];

/// 诊断开关:置位后不注入任何内置覆盖字体(韩文/符号与汉字一样 tofu)——
/// 仅供 footprint A/B 对比与排障;生产不设置。
const CJK_FONT_DISABLE_ENV: &str = "MCE_NO_CJK_FONT";

/// macOS 原生 UI 字体:拉丁主字体 SF Pro / SF Mono;其外字形见
/// [`BUNDLED_FONTS`](内置覆盖链),未知字形 tofu(system_fonts 已移除)。
const SYSTEM_UI_FONT_SPECS: [(&str, &str, egui::FontFamily); 2] = [
    (
        "SF Pro",
        "/System/Library/Fonts/SFNS.ttf",
        egui::FontFamily::Proportional,
    ),
    (
        "SF Mono",
        "/System/Library/Fonts/SFNSMono.ttf",
        egui::FontFamily::Monospace,
    ),
];

/// 字形图集纹理边长上限(内存规格 tech.md §6.2.4,防扩散封顶)。
///
/// epaint 图集宽度 = `max_texture_side`(eframe 每帧从 GL_MAX_TEXTURE_SIZE
/// 塞进 RawInput,本机 8192),高度按倍增涨到与宽度同值,单块满图 8192² RGBA
/// ≈ 256MB;收到 4096 后上界 64MB。egui 对该值无公开配置面——style 里
/// `visuals.text_options.max_texture_side` 每帧被输入值覆盖(`update_fonts_mut`),
/// 唯一收口点是 [`egui::Plugin::input_hook`](输入处理前改 RawInput)。
const MAX_ATLAS_TEXTURE_SIDE: usize = 4096;

/// 纯函数:把渲染端提供的纹理边长统一收到 `Some(MAX)`。
///
/// **必须把 `None` 也统一成 `Some`**:eframe 只给挂了 `egui_winit` 的视口注入
/// `max_texture_side`(glow_integration.rs:`if let Some(egui_winit)`);悬浮条
/// 是 ADR-8 的 viewport hack,可能没有 egui_winit 实例 → 其 `RawInput` 的值为
/// `None`。若这里保留 `None`,root(Some(x))与 palette(None)之间 `TextOptions`
/// 每个 pass 交替变化,epaint `Fonts::begin_pass` 会**逐帧** `set_options` +
/// 清空 galley 缓存 + 重建整张字形图集(实测中文列表滚动时 GL 纹理重建 32 次、
/// 峰值 2.2GB)。统一收口后 TextOptions 恒定,重建只随图集真实增长发生。
fn clamp_texture_side(side: Option<usize>) -> Option<usize> {
    Some(side.map_or(MAX_ATLAS_TEXTURE_SIDE, |s| s.min(MAX_ATLAS_TEXTURE_SIDE)))
}

/// 图集边长收口插件(`install_atlas_cap` 注册;每 pass 输入处理前生效)。
struct AtlasSideCap;

impl egui::Plugin for AtlasSideCap {
    fn debug_name(&self) -> &'static str {
        "atlas-side-cap"
    }

    fn input_hook(&mut self, _ctx: &egui::Context, input: &mut egui::RawInput) {
        input.max_texture_side = clamp_texture_side(input.max_texture_side);
    }
}

/// 首帧接线轮询上限(悬浮条视口窗创建于注册 pass 之后,补丁须下一帧才找得到;
/// 上限防「窗口永不出现」时无限排帧烧 CPU)。
const PATCH_ATTACH_ATTEMPTS: u32 = 300;

/// 安装 UI 字体:内置覆盖字体(`BUNDLED_FONTS`,Proportional + Monospace
/// 同表)+ macOS 原生拉丁字体(SF Pro / SF Mono)。
///
/// 优先级语义:egui 的 `Highest` = 插到 family 表**下标 0**(后插者排前),
/// `Lowest` = 追加到表尾。拉丁最后插 → 稳居首位;SC 以 Highest 紧随;
/// KR/Symbols 以 Lowest 追加到表尾(机制见 `BUNDLED_FONTS` 注释:只兜
/// 链上前段都没有的字形,不改变现有字形的来源)。system_fonts 回退已随
/// feature 移除(tech.md §6.2.1,A6b 裁决),未知字形走
/// `MissingGlyphPolicy::Tofu`(egui 默认,纯显示退化)。
fn install_ui_fonts(ctx: &egui::Context, install_bundled: bool) {
    if install_bundled {
        let total_kb: usize = BUNDLED_FONTS.iter().map(|(_, b, _)| b.len() / 1024).sum();
        let names: Vec<&str> = BUNDLED_FONTS.iter().map(|(n, _, _)| *n).collect();
        for (name, bytes, priority) in BUNDLED_FONTS {
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
        log::info!("内置覆盖字体已注册:{}(共 {total_kb} KB)", names.join(" / "));
    }
    for (name, path, family) in SYSTEM_UI_FONT_SPECS {
        match mapped_font_data(path) {
            Ok(data) => {
                let bytes = data.0.as_ref().len();
                ctx.add_font(egui::FontInsert::new(
                    name,
                    egui::FontData::from_blob(std::sync::Arc::new(data), 0),
                    vec![egui::InsertFontFamily {
                        family,
                        priority: egui::FontPriority::Highest,
                    }],
                ));
                log::info!("系统 UI 字体已加载:{name}({path},{bytes} 字节,mmap 只读)");
            }
            Err(err) => {
                log::warn!("系统 UI 字体加载失败({path}),保留 egui 默认字体: {err}");
            }
        }
    }
}

/// 只读 mmap 的字体文件(`FontData::from_blob` 的 Blob 载体)。
///
/// 堆粗查易修项(tech.md §6.2.5):原实现 `std::fs::read` + `from_owned` 把
/// SFNS.ttf/SFNSMono.ttf 各 ~4MB 拷进堆并终身持有(实测 Malloc 大块 7.7MB);
/// mmap 后为 clean file-backed 页,不计 phys_footprint,也没有堆拷贝。
struct MappedFont(memmap2::Mmap);

impl AsRef<[u8]> for MappedFont {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

/// 只读映射系统字体文件。
///
/// SAFETY:常规文件的只读映射,无写共享面;映射由 `MappedFont` 持有保活,
/// 生命周期与 `FontData` 一致。文件被系统替换时 mmap 语义仍是读到一致内容
/// (私有的已映射快照),不存在悬垂。
fn mapped_font_data(path: &str) -> std::io::Result<MappedFont> {
    let file = std::fs::File::open(path)?;
    // SAFETY:见函数注释。
    let mmap = unsafe { memmap2::Mmap::map(&file)? };
    Ok(MappedFont(mmap))
}

/// 生产入口:诊断开关 [`CJK_FONT_DISABLE_ENV`] 置位时跳过内置覆盖字体注入
/// (footprint A/B 对比用;system_fonts 已移除,关掉即覆盖集外全 tofu)。
fn install_system_ui_fonts(ctx: &egui::Context) {
    let install_bundled = std::env::var_os(CJK_FONT_DISABLE_ENV).is_none();
    if !install_bundled {
        log::info!("{CJK_FONT_DISABLE_ENV} 置位:不注入内置覆盖字体(覆盖集外全 tofu)");
    }
    install_ui_fonts(ctx, install_bundled);
}

/// 注册图集边长收口插件(见 [`MAX_ATLAS_TEXTURE_SIDE`])。
fn install_atlas_cap(ctx: &egui::Context) {
    ctx.add_plugin(AtlasSideCap);
}

/// 悬浮条视口 id(注册与命令定向同一处,勿散落)。
pub(crate) fn palette_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("palette")
}

/// 全局字号基准（纯函数：`[Small, Body, Button, Heading, Monospace]` 基准 × scale）。
///
/// 抽成纯函数便于测试：并行测试不得改写全局原子，否则会与其它 UI 测试竞态。
fn base_text_sizes(scale: f32) -> [f32; 5] {
    [11.0, 15.0, 15.0, 20.0, 15.0].map(|base| font::scaled(base, scale))
}

/// 全局字号基准(各视口共用;2026-10-16 辉哥实机反馈「整体字号偏小,放大两号」)。
///
/// 字号基准**单点设置**:悬浮条、完整窗口与设置窗口共用同一基准与全局倍率
/// (`font::scale()`,启动前由 `App::new` 按设置写入)。egui 默认档
/// (egui-0.37 style.rs:1524 实测):Small 9 / Body 13 / Button 13 / Heading 18 /
/// Monospace 13 → 统一 +2 得基准 11 / 15 / 15 / 20 / 15,再乘全局倍率。
/// 幂等(写死目标值,不累加);两主题都写(`all_styles_mut`——egui 的 Style 按主题
/// 各存一份,只改当前主题会在切换后丢)。悬浮条/完整窗口另有更细的基准字号档
/// (ui/palette.rs、ui/window_tokens.rs 的 `FONT_*` 常量,绘制时经 `font::size` 换算)。
pub(crate) fn install_base_text_styles(ctx: &egui::Context) {
    let [small, body, button, heading, monospace] = base_text_sizes(font::scale());
    ctx.all_styles_mut(|s| {
        let set = |s: &mut egui::Style, ts: egui::TextStyle, size: f32| {
            // 保留原 family(Monospace 档不能改成 proportional)
            let family = s
                .text_styles
                .get(&ts)
                .map_or(egui::FontFamily::Proportional, |f| f.family.clone());
            s.text_styles.insert(ts, egui::FontId::new(size, family));
        };
        set(s, egui::TextStyle::Small, small);
        set(s, egui::TextStyle::Body, body);
        set(s, egui::TextStyle::Button, button);
        set(s, egui::TextStyle::Heading, heading);
        set(s, egui::TextStyle::Monospace, monospace);
    });
}

/// 显示完整窗口(根视口):平台层原生唤起(activate + 置前),失败退回 egui 命令。
/// 悬浮条 ⌥↵、托盘「打开完整窗口」/ 缺口项、⌥⇧F 直达共用(单一实现,勿复制第四份)。
fn show_root_window(ctx: &egui::Context) {
    if let Err(err) = platform::show_root_window() {
        log::warn!("根窗原生唤起失败,退回 egui 命令:{err}");
        ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Visible(true));
    }
    ctx.request_repaint_of(egui::ViewportId::ROOT);
}

// ---------------------------------------------------------------------------
// 悬浮条显隐 / 完整窗口打开(单一实现;帧内与主队列直调共用)
//
// ## 锁纪律(硬要求,实测踩坑)
//
// 这些函数会从**主队列直调块**(hotkey.rs 的直显路径)调用。块内做 AppKit 调用时,
// AppKit 通知会被 winit **同步派发**(`app_state.rs::maybe_queue_event`:不在事件
// 处理中即立即 `handle_event`)→ 重入一帧 → 帧内 `logic`/`draw_palette` 要拿同一把
// `shared` 锁。实测栈(2026-10-16):show_palette → setFrameOrigin → windowDidMove →
// winit → eframe run_ui_and_paint → draw_palette → `Mutex::lock` → 主线程自锁。
// 故:**原生调用(平台层 show_palette/show_root_window)一律在锁外执行**;
// 状态读写收窄为短锁;`send_viewport_cmd`(只入 egui 队列,无 AppKit 调用)可持锁。

/// 取共享锁(毒锁兜底;锁内无 panic 期望)。主线程短占用。
/// 调用方不得已持有 `Shared` 锁（std `Mutex` 非重入）。
fn lock_shared(shared: &Arc<Mutex<Shared>>) -> std::sync::MutexGuard<'_, Shared> {
    match shared.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// 悬浮条隐藏的状态部分(不含命令下发;调用方可持锁调用)。
fn hide_palette_state(s: &mut Shared) {
    s.palette_visible = false;
    s.palette.reset(); // 隐藏后清空输入(product §1)
}

/// 下发悬浮条隐藏命令(egui 命令 → winit `orderOut`;视口与窗口保留,不销毁)。
/// 只入 egui 队列、无 AppKit 调用 → 可在外层持锁时调用。
fn send_palette_hide(ctx: &egui::Context, reason: &str) {
    // 垫窗立即收起(I3):隐藏态 draw_palette 在可见性守卫处 return,末尾的
    // `sync_blur(false)` 不会执行;只发命令会让磨砂矩形残留在屏上。
    platform::hide_blur();
    ctx.send_viewport_cmd_to(palette_viewport_id(), egui::ViewportCommand::Visible(false));
    // 四窗全隐后缩窗释放 GL surface(定时器触发时再核对真实可见性)。
    platform::schedule_surface_shrink();
    log::info!("悬浮条隐藏({reason})");
}

/// 隐藏悬浮条(帧内与直调共用;`reason` 只进日志,区分热键/托盘/Esc 等来源)。
fn palette_hide(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context, reason: &str) {
    hide_palette_state(&mut lock_shared(shared));
    send_palette_hide(ctx, reason);
}

/// 唤起悬浮条(帧内与直调共用):平台层原生显示 → 复位会话 → 主题即时补查 →
/// 请求悬浮条视口重绘。返回是否已唤起(false = 窗口未就绪,调用方决定兜底)。
///
/// 顺序(与 PoC-4 native 路径同构):先让窗口在屏(内容为上一帧),再复位状态并
/// 请求重绘——上屏延迟与内容刷新解耦。
fn palette_show(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context) -> bool {
    // 屏幕坐标 y 向下：窗口顶 = 面板顶 − 形象条；AppKit y 向上：窗口顶 y =
    // 面板顶 y + 形象条。平台层按 top_inset 上移窗口，面板顶边仍距可见区顶 110
    // （§2.9 / Task 9）。
    match platform::show_palette(palette::MASCOT_STRIP as f64) {
        Ok(w) => {
            // 外观探测是原生调用:在共享锁外完成(锁纪律)。
            let detected = theme::current_theme();
            {
                let mut s = lock_shared(shared);
                // 先按屏幕回填宽(窄屏收窄),再 reset——reset 内的 send_h 须带新宽。
                // 已知一帧观感:平台 resize 与 UI 宽度跨帧收敛,唤起首帧可能先画
                // 上一会话宽度的旧内容,下一帧即收敛;不做平台拆分(2026-10-17 review)。
                s.palette.set_panel_width(w as f32);
                s.palette.reset(); // 每次唤起都是全新查询(product §1)
                s.palette.on_shown(std::time::Instant::now()); // 首帧计时起点(诊断)
                s.palette_visible = true;
                s.theme.check_now_with(ctx, detected); // 隐藏期切过外观:亮起立即补查
            }
            ctx.request_repaint_of(palette_viewport_id());
            true
        }
        Err(err) => {
            // 窗口未就绪/非 macOS:native 路径不可用,如实留痕不静默
            log::warn!("悬浮条唤起失败(保持隐藏):{err}");
            false
        }
    }
}

/// 切换悬浮条显示态(托盘左键/「打开搜索」/热键/二次启动共用)。
fn palette_toggle(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context, reason: &str) -> bool {
    let visible = lock_shared(shared).palette_visible;
    if visible {
        palette_hide(shared, ctx, reason);
        true
    } else {
        palette_show(shared, ctx)
    }
}

/// 根视口 ⌘W：与红点关闭同语义——隐藏窗口，不退出 App。
/// 返回 true = 已处理（调用方负责落盘窗口尺寸）。
fn handle_root_close_shortcut(ctx: &egui::Context) -> bool {
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::W)) {
        crate::platform::quick_look_dismiss();
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        platform::schedule_surface_shrink();
        // 图标互查:设置窗口仍可见则保留(图标跟随「任一主窗口可见」)
        if platform::settings_visible() != Some(true) {
            platform::set_dock_icon(false);
        }
        true
    } else {
        false
    }
}

/// 托盘自启切换失败后的勾选值（纯函数，单测）：
/// 设置页已加载过实际态 → 用它（同帧同步值可能已被仲裁丢弃）；
/// 未知（设置页从未打开）→ 保持点击前值，避免把托盘安装时读到的真实态误改。
fn autostart_check_after_failure(known: Option<bool>, checked_after_click: bool) -> bool {
    known.unwrap_or(!checked_after_click)
}

/// 打开设置窗口(完整窗口齿轮/托盘「设置…」共用):置开合态 → (首次显示时)
/// 查询自启真源与当前外观 → 原生居中(仅首次)/置前 → 失败退回 egui 命令。
///
/// 查询只在隐藏→显示时进行:窗口已打开时再次点入口不得覆盖失败切换的行内错误。
/// `autostart::status()`/`theme::current_theme()`/`show_settings_window` 都是原生
/// 调用:一律在共享锁外执行(锁纪律见「悬浮条显隐」段),锁内只写状态。
/// 窗口未就绪(视口尚未创建)时退回既有 `Visible(true)` + `Focus` 命令。
fn open_settings(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context) {
    let (was_open, center) = {
        let mut s = lock_shared(shared);
        let was_open = s.settings.open;
        let center = s.settings.request_open(); // 首次打开 true 并记账
        (was_open, center)
    };
    if !was_open {
        // 锁外原生查询/探测。
        let autostart = autostart::status().map_err(|err| format!("读取开机自启状态失败:{err:#}"));
        let detected = theme::current_theme();
        let mut s = lock_shared(shared);
        s.settings.autostart_loaded(autostart);
        // 设置窗打开即用当前系统外观(与 results_open 同法;隐藏期可能切过外观)。
        s.theme.check_now_with(ctx, detected);
    }
    // 原生居中/置前:ROOT 隐藏(同 Space)时也能把设置窗带到前台。
    // 跨 Space 未做额外 collectionBehavior,待实机验证(不擅自新增 Space 行为)。
    // 首次打开居中,之后只置前(保留用户拖动位置);失败退回 egui 命令。
    if let Err(err) = platform::show_settings_window(center) {
        log::warn!("设置窗口原生定位/置前失败,退回 egui 命令:{err}");
        ctx.send_viewport_cmd_to(
            settings::viewport_id(),
            egui::ViewportCommand::Visible(true),
        );
        ctx.send_viewport_cmd_to(settings::viewport_id(), egui::ViewportCommand::Focus);
    }
    ctx.request_repaint_of(settings::viewport_id());
    ctx.request_repaint_of(egui::ViewportId::ROOT);
    log::info!(
        "打开设置窗口(此前{};{})",
        if was_open { "已打开" } else { "隐藏" },
        if center {
            "首次居中"
        } else {
            "保持位置/置前"
        }
    );
}

/// 工作线程退出即叫帧：正常/panic/断连都让根帧 tick 一次（看门狗/断连路径不至于无人唤醒）。
struct RepaintOnDrop(egui::Context);

impl Drop for RepaintOnDrop {
    fn drop(&mut self) {
        self.0.request_repaint_of(egui::ViewportId::ROOT);
    }
}

/// 启动一次后台检查（同一时刻至多一个；检查中重复调用直接返回）。
/// 调用方不得已持有 `Shared` 锁（std `Mutex` 非重入）。
pub(crate) fn spawn_update_check(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context) {
    let tx = {
        let mut guard = lock_shared(shared);
        match guard.update.begin() {
            Some(tx) => tx,
            None => return,
        }
    };
    let ctx = ctx.clone();
    // 立即叫一帧：根帧 tick 后把托盘/设置切到「检查中…」，不必等网络结果。
    ctx.request_repaint_of(egui::ViewportId::ROOT);
    let failure_ctx = ctx.clone();
    let spawned = std::thread::Builder::new()
        .name("update-check".to_owned())
        .spawn(move || {
            // panic 展开/断连同走 Drop：根帧必被唤醒，tick 的断连分支可把状态收敛。
            let _repaint = RepaintOnDrop(ctx.clone());
            let outcome = update::fetch_and_evaluate();
            if tx.send(outcome).is_ok() {
                ctx.request_repaint_of(egui::ViewportId::ROOT);
            }
        });
    if let Err(err) = spawned {
        log::error!("检查更新线程启动失败: {err}");
        let mut guard = lock_shared(shared);
        guard.update.spawn_failed(err.to_string());
        drop(guard);
        failure_ctx.request_repaint_of(egui::ViewportId::ROOT);
    }
}

/// 打开完整窗口(托盘「打开完整窗口」/「缺口面板」/⌥⇧F 直达共用):
/// 聚焦搜索框 + 补查引擎状态后原生唤到前台(原生调用在锁外,见锁纪律)。
fn results_open(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context) {
    // 外观探测是原生调用:在共享锁外完成(锁纪律);锁内只写状态与 egui 主题队列。
    let detected = theme::current_theme();
    {
        let mut s = lock_shared(shared);
        s.results.on_window_shown();
        // 悬浮条隐藏、全窗单独可见时,主题跟随由根视口 logic 驱动(见 App::logic);
        // 此处补查免「隐藏期切外观,开窗首帧仍旧主题」。
        s.theme.check_now_with(ctx, detected);
    }
    show_root_window(ctx);
}

/// 悬浮条视口 builder(常量;每 pass 传同一份——`patch` 无差异则无命令/无重建)。
/// 宽取**最大宽** `PANEL_W`:首次唤起时平台层按鼠标所在屏收窄,UI 随 `show_palette`
/// 返回的实际宽同步布局(窄屏走 `set_panel_width`;builder 不逐 pass 变,免重建)。
/// `visible(false)`:启动隐藏(唤起走平台层原生路径,不用 winit `Visible(true)`)。
fn palette_builder() -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_title(platform::PALETTE_TITLE)
        // 窗口高 = 面板高 + 形象条 70（Task 9；面板顶边仍距可见区顶 110）。
        .with_inner_size([palette::PANEL_W, palette::INITIAL_H + palette::MASCOT_STRIP])
        .with_decorations(false)
        .with_transparent(true)
        .with_has_shadow(false)
        .with_visible(false)
        .with_always_on_top()
}

/// 保存面板目录助手浮层视口 builder(常量;尺寸固定,定位由平台层原生摆位)。
/// 无装饰、不透明内容自绘、启动隐藏;`always_on_top` 与平台层 NSPanel level 双保险。
fn overlay_builder() -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_title(platform::OVERLAY_TITLE)
        .with_inner_size([overlay::OVERLAY_W, overlay::OVERLAY_H])
        .with_decorations(false)
        .with_transparent(true)
        .with_has_shadow(false)
        .with_visible(false)
        .with_always_on_top()
}

/// 后台跳转结果(工作线程 → `logic`;FellBack 时重现浮层提示已复制)。
#[derive(Debug, Clone, Copy)]
struct NavigateResult {
    pid: i32,
    outcome: platform::NavigateOutcome,
}

/// 浮层请求(绘制层置标志,`logic` 锁内取快照后执行)。
enum OverlayRequest {
    /// Esc/⌘W:关闭浮层并屏蔽该面板。
    Dismiss,
    /// Enter/双击:关闭浮层并后台跳转。
    Commit { pid: i32, path: String },
}

pub struct App {
    /// 二次启动唤起通道(app.sock 监听线程 → 主线程;main 接线)。
    incoming: Receiver<Incoming>,
    /// 各视口共享状态(悬浮条/设置回调与根视口逻辑共持同一份)。
    shared: Arc<Mutex<Shared>>,
    /// 运行期系统字体回退留痕一次(`ui()`;诊断用)。
    fonts_logged: bool,
    /// 首帧接线已尝试次数(见 `PATCH_ATTACH_ATTEMPTS`)。
    attach_attempts: u32,
    /// 根窗首帧隐藏纠偏已发出(产品形态四窗皆隐;见 `logic` 首帧纠偏注释)。
    root_hide_issued: bool,
    /// 纠偏回读已留痕一次(原生 `isVisible()` 核对;见 `logic`)。
    root_hide_verified: bool,
    /// 保存面板检测事件接收端(`App::new` 按设置启动 detector;全隐期由桥接
    /// 线程 request_repaint 唤醒 `logic` 消费)。
    detector_rx: Option<Receiver<platform::PanelEvent>>,
    /// 后台跳转结果回投(`commit_requested` → 工作线程 → `logic`)。
    navigate_rx: Receiver<NavigateResult>,
    /// 跳转结果发送端(每次 commit 克隆给工作线程)。
    navigate_tx: Sender<NavigateResult>,
    /// 退出流程:根窗关闭请求不再取消(dev 退出按钮;托盘「退出」Task 16 已接)。
    quitting: bool,
    /// 全局热键运行时(Task 15/21):manager 保活 + 事件泵;命中走主队列直调
    /// (本文件 `App::new` 回填目标),`logic` 只消费兜底队列(见 hotkey.rs)。
    hotkey: hotkey::HotkeyRuntime,
    /// 菜单栏托盘(Task 16);None = 安装失败(已留痕,其余入口照常)。
    tray: Option<tray::TrayRuntime>,
    /// 引擎路径(托盘「拉起引擎」用;PaletteState 内另持一份同源克隆)。
    paths: client::DaemonPaths,
    /// 完整窗口尺寸变化跟踪(400ms 限频合并写盘;见 `window_state`)。
    window_size: WindowSizeTracker,
}

/// 各视口共享的运行时状态。
pub struct Shared {
    /// 悬浮条状态与查询会话(Task 12)。
    pub palette: palette::PaletteState,
    /// 完整窗口(根视口)状态与查询会话(Task 17;与悬浮条各自防抖/代际)。
    pub results: results::ResultsState,
    /// 设置窗口运行期状态(v2.1:开合/设置快照/自启与字号错误/托盘同步标记)。
    pub settings: settings::SettingsState,
    /// 主题跟随(节流 + 已应用值;Task 13 起为局部状态,Task 14 收进共享态)。
    pub theme: theme::ThemeFollow,
    /// 发现新版本状态（托盘与设置页唯一真源）。
    pub update: update::UpdateState,
    /// 悬浮条可见标志(真源;排帧门禁与按钮态都读它)。
    pub palette_visible: bool,
    /// 保存面板目录助手浮层(事件接线见本文件 `drain_panel_events`)。
    pub overlay: overlay::OverlayState,
    /// 助手开关的有效状态(设置开启 **且** 辅助功能已授权;设置切换时更新)。
    pub panel_assist_enabled: bool,
    /// Esc 屏蔽(本面板存续内不再自动弹;`Closed` 解除)。
    pub dismissals: platform::Dismissals,
    /// 最近一次 Shown 的面板 AX frame(FellBack 重现浮层时重新定位用)。
    pub last_panel_frame: Option<[f64; 4]>,
    /// 当前归因面板的 pid(独立于 `overlay.panel_pid`:Esc 关闭浮层后仍需记住
    /// 该面板,等 `Closed` 解除屏蔽;`overlay.close()` 会清自己的 pid)。
    pub active_panel_pid: Option<i32>,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        incoming: Receiver<Incoming>,
        paths: client::DaemonPaths,
        settings_file: crate::settings::SettingsFile,
        hotkey: hotkey::HotkeyRuntime,
        tray: Option<tray::TrayRuntime>,
    ) -> Self {
        // 图集边长收口(防扩散;须在可能触发排版的首帧前注册)。
        install_atlas_cap(&cc.egui_ctx);
        // 拉丁主字体用 macOS 原生 SF Pro / SF Mono,汉字/韩文/符号用内置
        // OFL 覆盖链;system_fonts(fontique)已随 feature 移除(A6b 裁决),
        // 未知字形 tofu。诊断开关见 CJK_FONT_DISABLE_ENV。
        install_system_ui_fonts(&cc.egui_ctx);
        // 空闲死页归还看门狗(输入静默 60s 后归还 Malloc Large 滞留脏页)。
        idle::spawn_watchdog();
        // 全局字号倍率先于基准安装（设置 · 字号：只缩放文字，几何尺寸不变）。
        font::set_scale(settings_file.font_size.scale());
        // 字号基准(全局长大两号);悬浮条另有更细的字号档(ui/palette.rs 常量)。
        install_base_text_styles(&cc.egui_ctx);
        // 首个 pass 之前先应用一次外观:egui 首帧按 System 偏好的 fallback(Dark)
        // 出图,logic() 里首查已晚于该帧 → 浅色系统启动会闪一帧深色。
        let theme = theme::current_theme();
        theme::apply(&cc.egui_ctx, theme);
        log::info!("启动外观:{theme:?}");
        // 平台层:记录根窗句柄(悬浮条窗与根窗同为 NSApp.windows() 成员,须区分)。
        platform::attach_root(cc);
        // 切 Space(尤其进他人全屏)后主动唤醒悬浮条重绘并恢复层级:
        // 否则窗口可能停在一帧空白内容,点一下才恢复(2026-10-18 实机反馈)。
        platform::install_space_repaint(cc.egui_ctx.clone());
        // 全隐缩窗后清字形图集要请求重绘帧(见 platform/mac.rs release_glyph_atlas)。
        platform::install_surface_reclaim(cc.egui_ctx.clone());
        let hotkey_snapshot = hotkey.snapshot();
        let hotkey_note = hotkey.note().cloned();
        // 自动检查更新（默认开启）：先记账再移动 settings_file。
        let auto_update = settings_file.check_updates;
        // 保存面板助手:开关开且未授权 → 提示一次(仅 log + 托盘项,不弹窗;
        // design §3.2;托盘菜单项由 tray::install 在本调用前按同一条件挂载)。
        let panel_assist_trusted = crate::platform::panel_assist_trusted();
        let panel_assist_enabled = settings_file.save_panel_assist && panel_assist_trusted;
        if settings_file.save_panel_assist && !panel_assist_trusted {
            log::info!("保存面板助手待授权:设置页或托盘菜单可跳转系统设置");
        }
        let shared = Arc::new(Mutex::new(Shared {
            palette: palette::PaletteState::new(
                cc.egui_ctx.clone(),
                paths.clone(),
                palette_viewport_id(),
                hotkey_note,
            ),
            results: results::ResultsState::new(cc.egui_ctx.clone(), paths.clone()),
            settings: settings::SettingsState::new(
                settings_file,
                paths.data_dir.clone(),
                hotkey_snapshot,
            ),
            theme: theme::ThemeFollow::new(theme, theme::CHECK_INTERVAL),
            update: update::UpdateState::new(),
            palette_visible: false, // 启动隐藏(产品形态:四窗皆隐,仅菜单栏图标)
            overlay: overlay::OverlayState::new(cc.egui_ctx.clone(), paths.clone()),
            panel_assist_enabled,
            dismissals: platform::Dismissals::default(),
            last_panel_frame: None,
            active_panel_pid: None,
        }));
        if auto_update {
            spawn_update_check(&shared, &cc.egui_ctx);
            // 挂机期定时重查(update::AUTO_CHECK_INTERVAL,默认 24h):启动检查只覆盖
            // 一次,常驻形态此后全靠它。每轮到点重读 settings.toml 的 check_updates
            // ——用户运行中关掉「自动检查更新」即时不再发起;Checking 期到点由
            // UpdateState::begin 防重入闸吞掉,不叠加。常驻线程与热键事件泵同型。
            let shared_for_timer = Arc::clone(&shared);
            let ctx_for_timer = cc.egui_ctx.clone();
            let data_dir_for_timer = paths.data_dir.clone();
            let timer = std::thread::Builder::new()
                .name("update-auto-check".to_owned())
                .spawn(move || loop {
                    std::thread::sleep(update::AUTO_CHECK_INTERVAL);
                    if !crate::settings::load(&data_dir_for_timer).check_updates {
                        log::info!("定时检查更新:设置已关闭,跳过本轮");
                        continue;
                    }
                    log::info!("定时检查更新(每 24h)");
                    spawn_update_check(&shared_for_timer, &ctx_for_timer);
                });
            if let Err(err) = timer {
                log::error!("定时检查更新线程启动失败:{err}");
            }
        }
        // daemon 存活监控(每 5s):daemon 后台线程 panic 会 fail-fast 退出(见
        // cli/daemon.rs spawn_guarded);此处负责在 socket 断续后自动拉起,并清
        // UI 离线提示/重查。期望运行标志由 main/托盘/手动重启维护——启动即
        // 离线或用户主动 stop 时不会误拉起。
        {
            let shared = Arc::clone(&shared);
            let ctx = cc.egui_ctx.clone();
            engine_launcher::spawn_health_monitor(paths.clone(), move || {
                let mut s = lock_shared(&shared);
                s.results.on_engine_restarted();
                s.palette.on_engine_restarted();
                drop(s);
                ctx.request_repaint_of(egui::ViewportId::ROOT);
                ctx.request_repaint_of(palette_viewport_id());
            });
        }
        // 热键动作的主队列直调目标(hotkey.rs 模块注释「动作直调」):
        // 预检拿不到共享锁(帧内/嵌套 run loop 持锁,如原生菜单跟踪中)返回 false,
        // 动作转帧内兜底队列,不阻塞。预检通过后,实际实现内部短锁读写、
        // 原生调用在锁外(锁纪律见本文件「悬浮条显隐」段)。
        let dispatch_target: Arc<dyn Fn(hotkey::Action) -> bool + Send + Sync> = {
            let shared = Arc::clone(&shared);
            let ctx = cc.egui_ctx.clone();
            Arc::new(move |action| {
                match shared.try_lock() {
                    Ok(g) => drop(g),                               // 锁空闲:继续
                    Err(std::sync::TryLockError::Poisoned(_)) => {} // 毒锁:继续(无锁内 panic 期望)
                    Err(std::sync::TryLockError::WouldBlock) => return false,
                }
                match action {
                    hotkey::Action::TogglePalette => palette_toggle(&shared, &ctx, "热键"),
                    hotkey::Action::OpenResults => {
                        results_open(&shared, &ctx);
                        true
                    }
                }
            })
        };
        hotkey.attach({
            let t = Arc::clone(&dispatch_target);
            move |action| t(action)
        });
        // 保存面板检测器:设置开启且已授权才启动(常驻不可停止;关闭时由接线层
        // 忽略事件即可)。detector 的 mpsc 不持有 egui ctx,这里加一层桥接线程:
        // 全隐期没有排帧,收到事件必须主动 request_repaint 唤醒 `logic` 消费。
        let detector_rx = if panel_assist_enabled {
            platform::panel_detector_start().and_then(|rx| {
                let (tx, fwd_rx) = std::sync::mpsc::channel();
                let ctx = cc.egui_ctx.clone();
                let spawned = std::thread::Builder::new()
                    .name("panel-events".to_owned())
                    .spawn(move || {
                        for ev in rx {
                            if tx.send(ev).is_err() {
                                break; // 接收端已随 App 退出
                            }
                            ctx.request_repaint_of(egui::ViewportId::ROOT);
                        }
                    });
                match spawned {
                    Ok(handle) => {
                        drop(handle); // 分离线程:detector 常驻到进程退出
                        log::info!("保存面板检测器已启动");
                        Some(fwd_rx)
                    }
                    Err(err) => {
                        log::error!("保存面板事件桥接线程创建失败:{err}");
                        None
                    }
                }
            })
        } else {
            None
        };
        // 跳转结果通道一次创建、跨提交复用。
        let (navigate_tx, navigate_rx) = std::sync::mpsc::channel();

        Self {
            incoming,
            shared,
            fonts_logged: false,
            attach_attempts: 0,
            root_hide_issued: false,
            root_hide_verified: false,
            detector_rx,
            navigate_rx,
            navigate_tx,
            quitting: false,
            hotkey,
            tray,
            paths,
            window_size: WindowSizeTracker::new(),
        }
    }

    /// 取共享状态(poisoned 兜底:锁内无 panic 期望,毒锁也继续用,不静默丢状态)。
    fn shared(&self) -> std::sync::MutexGuard<'_, Shared> {
        match self.shared.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// 窗口尺寸变化 → 400ms 限频合并写盘(`window-state.toml`)。
    ///
    /// 隐藏门禁用**原生** `isVisible`(`platform::root_visible`):egui-winit 0.37
    /// 的 `ViewportInfo.visible` / `minimized` 在 macOS 不刷新,不能作为判断依据
    /// (实测恒 `None`)——两者仅作已知平台限制留档,真正兜底是 `inner_rect == None`
    /// (窗口隐藏/未创建时平台不提供内容矩形)。最小化由 `isVisible == false` 覆盖。
    ///
    /// 全屏跳过:`ViewportInfo.fullscreen` 每帧刷新;全屏尺寸不是用户对普通窗口的
    /// 调整意图,退出全屏后系统恢复原尺寸,记成全屏尺寸会在下次启动开出超屏窗口。
    fn persist_window_size(&mut self, ctx: &egui::Context, now: Instant) {
        if platform::root_visible() == Some(false) {
            return; // 根窗隐藏/最小化:不落盘
        }
        let (inner_rect, fullscreen) = ctx.input(|i| {
            let viewport = i.viewport();
            (viewport.inner_rect, viewport.fullscreen)
        });
        if fullscreen == Some(true) {
            return; // 全屏期间不写盘
        }
        let Some(rect) = inner_rect else {
            return; // 平台不提供窗口尺寸(隐藏期/未创建):本帧不观察
        };
        if rect.width() < window_state::MIN_W || rect.height() < window_state::MIN_H {
            return; // 最小尺寸生效前的过渡帧不落盘
        }
        match self
            .window_size
            .observe(WindowSize::new(rect.width(), rect.height()), now)
        {
            SizeUpdate::Idle => {}
            SizeUpdate::Defer(delay) => ctx.request_repaint_after(delay),
            SizeUpdate::Save(size) => window_state::save(&self.paths.data_dir, size),
        }
    }

    /// 二次启动唤起:置显示开关 + 请求重绘(本任务换 native
    /// orderFrontRegardless 显示路径)。
    fn drain_incoming(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.incoming.try_recv() {
            match ev {
                Incoming::ShowPalette => {
                    log::info!("二次启动唤起:显示悬浮条");
                    self.show_palette(ctx);
                }
            }
        }
    }

    /// 消费保存面板检测事件(每帧;锁内状态读写、原生调用锁外)。
    ///
    /// 事件 → 动作由跨平台纯函数 [`platform::panel_assist::decide`] 决定;
    /// daemon 在线状态读健康监控缓存(`engine_launcher::daemon_online`),
    /// 不在主线程做 Ping 往返。
    fn drain_panel_events(&mut self, ctx: &egui::Context) {
        while let Some(ev) = self.detector_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            let decision = {
                let s = self.shared();
                let blocked = match &ev {
                    platform::PanelEvent::Shown { pid, .. } => s.dismissals.blocked(*pid),
                    _ => false,
                };
                platform::panel_assist::decide(
                    &ev,
                    s.panel_assist_enabled,
                    s.overlay.visible,
                    blocked,
                    engine_launcher::daemon_online(),
                )
            };
            match decision {
                platform::Decision::Ignore => {}
                platform::Decision::Show => {
                    let platform::PanelEvent::Shown { pid, frame } = ev else {
                        continue; // 决策不变量:Show 只来自 Shown
                    };
                    {
                        let mut s = self.shared();
                        s.last_panel_frame = Some(frame);
                        s.active_panel_pid = Some(pid); // 归因,供 Closed 解除屏蔽
                        s.overlay.open(); // 复位上一次输入/结果/瞬态标志
                        s.overlay.panel_pid = Some(pid); // open() 会清 pid,须在其后写
                    }
                    if let Err(err) = platform::overlay_show(frame) {
                        // 原生唤起失败(窗口未就绪/屏幕查不到):回滚可见态,
                        // 否则后续 Shown 会被「已显示」门永久吞掉。
                        log::warn!("浮层显示失败 pid={pid}: {err}");
                        self.shared().overlay.close();
                        continue;
                    }
                    ctx.request_repaint_of(overlay::overlay_viewport_id());
                }
                platform::Decision::HideKeepBlock => {
                    self.shared().overlay.close();
                    // 锁外调用(原生 orderOut 可被 winit 同步派发)。
                    if let Err(err) = platform::overlay_hide() {
                        log::warn!("浮层隐藏失败(宿主切换): {err}");
                    }
                }
                platform::Decision::HideAndClear => {
                    let pid = {
                        let mut s = self.shared();
                        // Esc 关闭浮层时 `overlay.panel_pid` 已被 close() 清掉,
                        // 故用独立归因字段取回该面板 pid 再解除屏蔽。
                        let pid = s.active_panel_pid.take();
                        s.overlay.close();
                        if let Some(pid) = pid {
                            s.dismissals.clear(pid); // 面板销毁 → 解除 Esc 屏蔽
                        }
                        pid
                    };
                    if let Err(err) = platform::overlay_hide() {
                        log::warn!("浮层隐藏失败(面板关闭 pid={pid:?}): {err}");
                    }
                }
            }
        }
    }

    /// 消费后台跳转结果:FellBack 时在面板旁重现浮层并提示路径已复制。
    fn drain_navigate_results(&mut self, ctx: &egui::Context) {
        while let Ok(result) = self.navigate_rx.try_recv() {
            if result.outcome != platform::NavigateOutcome::FellBackToClipboard {
                continue; // Done/Aborted 无事
            }
            let frame = {
                let mut s = self.shared();
                // open() 会复位 pid/提示标志,故先 open 再写入。
                s.overlay.open();
                s.overlay.panel_pid = Some(result.pid);
                s.overlay.clipboard_hint = true;
                s.last_panel_frame
            };
            match frame {
                Some(frame) => {
                    if let Err(err) = platform::overlay_show(frame) {
                        log::warn!("路径已复制,但浮层重现失败 pid={}: {err}", result.pid);
                        self.shared().overlay.close();
                        continue;
                    }
                    ctx.request_repaint_of(overlay::overlay_viewport_id());
                }
                None => log::warn!("路径已复制,但缺少面板定位 frame,无法重现浮层"),
            }
        }
    }

    /// 消费浮层请求(Esc/⌘W 关闭、Enter/双击提交跳转),一帧至多一个。
    fn handle_overlay_requests(&mut self, ctx: &egui::Context) {
        // 锁内只做状态读写;隐藏/线程启动在锁外。
        let request = {
            let mut s = self.shared();
            if s.overlay.dismiss_requested {
                let pid = s.overlay.panel_pid;
                s.overlay.close();
                if let Some(pid) = pid {
                    s.dismissals.dismiss(pid); // 本面板存续内不再自动弹
                }
                Some(OverlayRequest::Dismiss)
            } else if s.overlay.commit_requested {
                let pid = s.overlay.panel_pid;
                let path = s.overlay.selected_path();
                s.overlay.close();
                match (pid, path) {
                    (Some(pid), Some(path)) => Some(OverlayRequest::Commit { pid, path }),
                    _ => {
                        // 二者任一缺失:关闭并忽略(无选中项/面板已消失)。
                        log::warn!("浮层提交缺少面板 pid 或选中路径,忽略");
                        Some(OverlayRequest::Dismiss)
                    }
                }
            } else {
                None
            }
        };
        match request {
            None => {}
            Some(OverlayRequest::Dismiss) => {
                if let Err(err) = platform::overlay_hide() {
                    log::warn!("浮层隐藏失败: {err}");
                }
            }
            Some(OverlayRequest::Commit { pid, path }) => {
                if let Err(err) = platform::overlay_hide() {
                    log::warn!("浮层隐藏失败: {err}");
                }
                let tx = self.navigate_tx.clone();
                let ctx = ctx.clone();
                let log_path = path.clone();
                let spawned = std::thread::Builder::new()
                    .name("panel-navigate".to_owned())
                    .spawn(move || {
                        let outcome = platform::navigate_panel(pid, &path);
                        log::info!("保存面板跳转 pid={pid} path={log_path} → {outcome:?}");
                        let _ = tx.send(NavigateResult { pid, outcome });
                        ctx.request_repaint_of(egui::ViewportId::ROOT);
                    });
                if let Err(err) = spawned {
                    log::error!("保存面板跳转线程创建失败:{err}");
                }
            }
        }
    }

    /// 唤起悬浮条(帧内版本;实现与热键主队列直调共用 `palette_show`)。
    fn show_palette(&mut self, ctx: &egui::Context) {
        palette_show(&self.shared, ctx);
    }

    /// 切换悬浮条显示态(托盘左键 / 菜单「打开搜索」与热键兜底共用)。
    fn toggle_palette(&mut self, ctx: &egui::Context, reason: &str) {
        palette_toggle(&self.shared, ctx, reason);
    }

    /// 打开完整窗口(托盘「打开完整窗口」/「缺口面板」/ ⌥⇧F 直达共用)。
    fn open_results(&mut self, ctx: &egui::Context) {
        results_open(&self.shared, ctx);
    }

    /// 消费托盘动作(Task 16):泵线程已为本帧请求 ROOT 重绘(与热键同模式,见 tray.rs)。
    /// 返回本帧是否消费了「开机自启」切换(供设置页同步标记仲裁)。
    fn drain_tray(&mut self, ctx: &egui::Context) -> bool {
        let mut tray_toggled_autostart = false;
        while let Some(msg) = self.tray.as_ref().and_then(tray::TrayRuntime::poll) {
            match msg {
                TrayMsg::TogglePalette => {
                    log::info!("托盘:切换悬浮条");
                    self.toggle_palette(ctx, "托盘");
                }
                TrayMsg::OpenResults => {
                    // 完整窗口(Task 17):聚焦搜索框 + 补查引擎状态后原生唤到前台。
                    log::info!("托盘:打开完整窗口");
                    self.open_results(ctx);
                }
                TrayMsg::OpenGaps => {
                    // 缺口项(product §3):同一开窗路径 + 展开缺口面板(Task 19);
                    // 清单经 on_window_shown 触发的状态补查落地(status.gaps 同源)。
                    log::info!("托盘:打开完整窗口(缺口面板)");
                    self.shared().results.open_gaps();
                    self.open_results(ctx);
                }
                TrayMsg::OpenSettings => {
                    // 设置窗口(v2.1 Task 1):与完整窗口齿轮同一开窗路径。
                    log::info!("托盘:打开设置");
                    open_settings(&self.shared, ctx);
                }
                TrayMsg::PanelAssistAuth => {
                    // 待授权项(design §3.2):深链系统设置辅助功能面板;
                    // 失败静默(面板未装/链接失效时无害)。
                    log::info!("托盘:保存面板助手去授权,打开系统设置");
                    platform::open_accessibility_settings();
                }
                TrayMsg::UpdateAction => {
                    let phase = { self.shared().update.phase().clone() };
                    match phase {
                        UpdatePhase::Available { .. } => {
                            log::info!("托盘:打开下载页");
                            if let Err(err) = update::open_download_page() {
                                log::warn!("打开下载页失败: {err}");
                            }
                        }
                        UpdatePhase::Checking => {
                            log::debug!("托盘:检查进行中,忽略重复点击");
                        }
                        _ => {
                            log::info!("托盘:检查更新");
                            spawn_update_check(&self.shared, ctx);
                            if self.shared().settings.open {
                                ctx.request_repaint_of(settings::viewport_id());
                            }
                        }
                    }
                }
                TrayMsg::ToggleAutostart => {
                    // Task 20:勾选动作已由 muda 在菜单侧翻转;此处按**操作后真实状态**
                    // 校准:仅 Enabled 勾选;RequiresApproval / NotRegistered / NotFound
                    // 一律取消勾选;失败复原勾选。用户意图标记语义不变。
                    tray_toggled_autostart = true;
                    let checked = self.tray.as_ref().is_some_and(|t| t.autostart_checked());
                    let result = autostart::apply_user_choice(&self.paths.data_dir, checked)
                        .map_err(|err| format!("{err:#}"));
                    match &result {
                        Ok(status) => log::info!("托盘:开机自启 → {}", status.note()),
                        Err(err) => log::warn!("托盘:开机自启设置失败: {err}"),
                    }
                    let (update, open, known_autostart) = {
                        let mut s = self.shared();
                        let update = s.settings.autostart_changed_externally(checked, result);
                        // 失败分支 `autostart` 保持切换前值；None = 设置页从未加载过
                        // 真源（此时不能拿 `unwrap_or(false)` 当实际值）。
                        (update, s.settings.open, s.settings.autostart)
                    };
                    if let Some(tray) = &self.tray {
                        if update.confirmed {
                            tray.set_autostart_checked(update.enabled);
                            if update.enabled != checked {
                                log::warn!(
                                    "托盘:自启未达成目标态(期望 {checked},实际 {});勾选按实际校准",
                                    update.enabled
                                );
                            }
                        } else {
                            // 注册/状态查询失败:设置页已加载过真源 → 按该实际值校准
                            //（同帧设置页排队的同步值会被本轮托盘动作仲裁丢弃；
                            // 用点击前值会留下与设置页不一致的勾选）。
                            // 真源未知（设置页从未打开）→ 保持点击前值，避免把
                            // 托盘安装时读到的已启用态误改成关。
                            let actual = autostart_check_after_failure(known_autostart, checked);
                            tray.set_autostart_checked(actual);
                            log::warn!(
                                "托盘:自启设置失败,勾选校准为 {actual}(点击前 {};设置页实际 {known_autostart:?})",
                                !checked
                            );
                        }
                    }
                    if open {
                        ctx.request_repaint_of(settings::viewport_id());
                    }
                }
                TrayMsg::Quit => {
                    // 退出只退 App,引擎常驻(product §5)。走 dev 退出按钮同一条干净
                    // 退出路径(置 quitting 放行根窗关闭 → eframe 收尾 → on_exit 还原
                    // 窗口类;`std::process::exit` 会跳过 on_exit,platform/mac.rs 的
                    // 改类还原是干净退出的必要步骤,故不用)。
                    // 退出前把 400ms 限频窗内未落盘的最终尺寸补写(on_exit 兜底同一动作)。
                    self.window_size.flush(&self.paths.data_dir);
                    log::info!("托盘:退出 App(引擎常驻)");
                    self.quitting = true;
                    ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
                TrayMsg::LaunchEngine => {
                    // 状态项仅在未运行时可点(tray.rs engine_launchable);拉起阻塞
                    // (≤3s 轮询)放工作线程,完成后请求状态刷新校准状态项。
                    log::info!("托盘:拉起引擎");
                    let paths = self.paths.clone();
                    let refresh = self.tray.as_ref().map(tray::TrayRuntime::refresh_handle);
                    std::thread::spawn(move || {
                        // 用户显式拉起 = 期望运行(失败也保留,由存活监控重试)
                        engine_launcher::set_expected_running(true);
                        match engine_launcher::ensure_engine(&paths) {
                            Ok(()) => log::info!("引擎拉起完成"),
                            Err(err) => log::warn!("引擎拉起失败:{err:#}"),
                        }
                        if let Some(tx) = refresh {
                            if let Err(err) = tx.send(()) {
                                log::warn!("托盘状态刷新请求失败:{err}");
                            }
                        }
                    });
                }
            }
        }
        tray_toggled_autostart
    }

    /// 消费排队命令与关闭取消（主线程；运行时调用一律在共享锁外）。
    ///
    /// 设置 pass 只入队（Carbon manager 只能主线程），命令按 FIFO 在此落地。
    /// **顺序**：先逐条执行命令，再消费关闭/⌘W 的取消信号——关闭当帧排队的
    /// `Begin` 会因 `open=false` 丢弃；已在录制中的状态则由随后关闭取消恢复旧键。
    fn process_hotkey_events(&mut self, ctx: &egui::Context) {
        let (commands, close_pending) = {
            let mut s = self.shared();
            (
                s.settings.take_hotkey_commands(),
                s.settings.take_cancel_recording_pending(),
            )
        };
        for event in hotkey_frame_events(commands, close_pending) {
            match event {
                HotkeyFrameEvent::Command(command) => self.apply_hotkey_command(ctx, command),
                HotkeyFrameEvent::CloseCancel => {
                    self.cancel_hotkey_recording(ctx, "设置窗口关闭");
                }
            }
        }
    }

    /// 落地一条快捷键命令（运行时调用不在共享锁内）。
    fn apply_hotkey_command(&mut self, ctx: &egui::Context, command: settings::HotkeyCommand) {
        match command {
            settings::HotkeyCommand::BeginRecord(slot) => self.begin_hotkey_recording(ctx, slot),
            settings::HotkeyCommand::CancelRecord => self.cancel_hotkey_recording(ctx, "取消录制"),
            settings::HotkeyCommand::Commit { slot, hotkey } => {
                self.commit_hotkey(ctx, slot, hotkey)
            }
            settings::HotkeyCommand::Reset(slot) => self.reset_hotkey(ctx, slot),
        }
    }

    /// 开始/切换录制：首槽 suspend；切槽时运行时保持挂起（旧键仍在 `suspended`）。
    fn begin_hotkey_recording(&mut self, ctx: &egui::Context, slot: hotkey::HotkeySlot) {
        let (open, already, focused) = {
            let s = self.shared();
            (
                s.settings.open,
                s.settings.recording,
                s.settings.viewport_focused,
            )
        };
        let decision = begin_decision(open, focused, already, slot);
        match decision {
            BeginDecision::DropClosed => {
                log::info!("快捷键录制开始被忽略:设置窗口已关闭");
                return;
            }
            BeginDecision::DropUnfocused => {
                // 点击后到命令落地之间窗口已失焦（罕见竞态）：不能在无键盘输入的
                // 窗口里开始录制，否则热键会一直挂起。
                log::info!("快捷键录制开始被忽略:设置窗口当前未聚焦");
                return;
            }
            BeginDecision::CancelDuplicate => {
                // 陈旧视图（或重复命令）在录制中再次 Begin 同槽：按取消处理，
                // 否则录制态无法退出、全局热键会一直挂起。
                log::info!("重复开始录制({slot:?}):按取消处理");
                self.cancel_hotkey_recording(ctx, "重复开始录制");
                return;
            }
            BeginDecision::Suspend | BeginDecision::Switch => {}
        }
        // suspend 幂等：切槽时保持挂起。无 manager 时返回 false，日志如实描述。
        let suspended = self.hotkey.suspend();
        match decision {
            BeginDecision::Suspend => log::info!("快捷键录制开始({slot:?})"),
            BeginDecision::Switch => {
                log::info!("快捷键录制切槽({already:?} → {slot:?})");
            }
            BeginDecision::DropClosed
            | BeginDecision::DropUnfocused
            | BeginDecision::CancelDuplicate => unreachable!(),
        }
        if suspended {
            log::info!("全局热键已挂起(录制期不会触发旧动作)");
        } else {
            log::warn!("全局热键运行时不可用,录制仍继续(提交将失败)");
        }
        let snapshot = self.hotkey.snapshot();
        {
            let mut s = self.shared();
            s.settings.begin_recording(slot, snapshot);
            s.palette.hotkey_note = s.settings.hotkey_snapshot.note.clone();
        }
        request_hotkey_repaint(ctx);
    }

    /// 取消录制并恢复旧键（Esc/点击他处/失焦/窗口关闭共用；未录制 = 静默 no-op）。
    fn cancel_hotkey_recording(&mut self, ctx: &egui::Context, reason: &str) {
        let recording = self.shared().settings.recording;
        let Some(slot) = recording else {
            log::debug!("{reason}:当前未录制快捷键(忽略)");
            return;
        };
        let report = self.hotkey.resume();
        let snapshot = self.hotkey.snapshot();
        {
            let mut s = self.shared();
            s.settings.end_recording(snapshot);
            s.palette.hotkey_note = s.settings.hotkey_snapshot.note.clone();
        }
        if !report.resumed {
            log::info!("{reason}:快捷键录制结束({slot:?}),运行时无挂起旧键");
        } else if report.fully_restored() {
            log::info!("{reason}:快捷键录制结束({slot:?}),旧热键已恢复");
        } else {
            log::warn!(
                "{reason}:快捷键录制结束({slot:?}),旧热键部分恢复失败(palette_ok={}, window_ok={})",
                report.palette_ok,
                report.window_ok
            );
        }
        request_hotkey_repaint(ctx);
    }

    /// 提交录制结果：注册成功 → 写设置文件（锁外保存）→ 刷新快照；
    /// 失败时运行时已回滚旧键并退出挂起 → 结束录制 + 按槽内联错误。
    fn commit_hotkey(&mut self, ctx: &egui::Context, slot: hotkey::HotkeySlot, hotkey: HotKey) {
        // 同一帧若先取消后提交（点击他处 + 按键），以取消为准：不得在非录制态换键。
        let recording = self.shared().settings.recording;
        if !commit_allowed(recording, slot) {
            log::warn!("快捷键提交被忽略({slot:?}):当前录制槽为 {recording:?}");
            return;
        }
        let readable = hotkey_keys::display_keys(hotkey).concat();
        match self.hotkey.commit(slot, hotkey) {
            Ok(snapshot) => {
                let (file, data_dir) = {
                    let mut s = self.shared();
                    let file = s.settings.commit_recorded_hotkey(slot, hotkey, snapshot);
                    s.palette.hotkey_note = s.settings.hotkey_snapshot.note.clone();
                    (file, s.settings.data_dir.clone())
                };
                // 保存与运行时调用都不持共享锁；失败只留内联错误，键保持生效。
                // 原始错误只进日志，界面仅展示当前语言的句子。
                let save_error = store::save(&data_dir, &file).err().map(|err| {
                    log::warn!("快捷键 {slot:?} 已生效,但设置保存失败: {err:#}");
                    i18n::s().hotkey_saved_but_save_failed.to_owned()
                });
                let saved = save_error.is_none();
                if let Some(message) = save_error {
                    let mut s = self.shared();
                    s.settings.hotkey_save_failed(slot, message);
                }
                log::info!(
                    "快捷键 {readable} 已注册({slot:?};设置已保存:{})",
                    if saved { "是" } else { "否" }
                );
                request_hotkey_repaint(ctx);
            }
            Err(message) => {
                // 运行时失败已恢复旧键并退出挂起(H2 语义);这里只结束录制展示错误。
                let snapshot = self.hotkey.snapshot();
                {
                    let mut s = self.shared();
                    s.settings.recording_failed(slot, message.clone(), snapshot);
                    s.palette.hotkey_note = s.settings.hotkey_snapshot.note.clone();
                }
                log::warn!("快捷键提交失败({slot:?}):{message}");
                request_hotkey_repaint(ctx);
            }
        }
    }

    /// 恢复该槽内置默认（录制中 Delete/Backspace）。
    ///
    /// 运行时注册默认并退出挂起；成功清空设置文件对应字段（跟随内置默认）后锁外
    /// 保存，失败由运行时恢复旧键 → 结束录制并按槽内联错误。仅当前录制槽有效。
    fn reset_hotkey(&mut self, ctx: &egui::Context, slot: hotkey::HotkeySlot) {
        let recording = self.shared().settings.recording;
        if !commit_allowed(recording, slot) {
            log::warn!("快捷键恢复默认被忽略({slot:?}):当前录制槽为 {recording:?}");
            return;
        }
        match self.hotkey.reset_slot(slot) {
            Ok(snapshot) => {
                let (file, data_dir) = {
                    let mut s = self.shared();
                    let file = s.settings.reset_recorded_hotkey(slot, snapshot);
                    s.palette.hotkey_note = s.settings.hotkey_snapshot.note.clone();
                    (file, s.settings.data_dir.clone())
                };
                // 保存与运行时调用都不持共享锁；失败只留内联错误，默认键保持生效。
                // 原始错误只进日志，界面仅展示当前语言的句子。
                let save_error = store::save(&data_dir, &file).err().map(|err| {
                    log::warn!("快捷键 {slot:?} 已恢复默认,但设置保存失败: {err:#}");
                    i18n::s().hotkey_reset_but_save_failed.to_owned()
                });
                let saved = save_error.is_none();
                if let Some(message) = save_error {
                    let mut s = self.shared();
                    s.settings.hotkey_save_failed(slot, message);
                }
                log::info!(
                    "快捷键已恢复默认({slot:?};设置已保存:{})",
                    if saved { "是" } else { "否" }
                );
                request_hotkey_repaint(ctx);
            }
            Err(message) => {
                // 运行时失败已恢复旧键并退出挂起;这里只结束录制展示错误。
                let snapshot = self.hotkey.snapshot();
                {
                    let mut s = self.shared();
                    s.settings.recording_failed(slot, message.clone(), snapshot);
                    s.palette.hotkey_note = s.settings.hotkey_snapshot.note.clone();
                }
                log::warn!("快捷键恢复默认失败({slot:?}):{message}");
                request_hotkey_repaint(ctx);
            }
        }
    }
}

/// 快捷键状态变化后唤醒设置页与根视口（命令由 ROOT 帧的 `logic` 消费）；
/// 悬浮条常驻警告可能随快照变化，一并重绘（隐藏期由 eframe 节流）。
fn request_hotkey_repaint(ctx: &egui::Context) {
    ctx.request_repaint_of(settings::viewport_id());
    ctx.request_repaint_of(egui::ViewportId::ROOT);
    ctx.request_repaint_of(palette_viewport_id());
}

/// `BeginRecord` 的落地决策（纯判定；实际 suspend/resume 仍在主线程执行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BeginDecision {
    /// 设置窗口已关闭：丢弃，避免挂起热键后无人可取消。
    DropClosed,
    /// 已观测到失焦（点击→命令落地之间的竞态）：丢弃，不在无键盘输入的窗口录制。
    DropUnfocused,
    /// 陈旧视图重复 Begin 同槽：按取消处理，防止录制态无法退出。
    CancelDuplicate,
    /// 已录制另一槽：运行时保持挂起，只切录制槽。
    Switch,
    /// 空闲：调用 suspend 进入录制。
    Suspend,
}

fn begin_decision(
    open: bool,
    focused: Option<bool>,
    recording: Option<hotkey::HotkeySlot>,
    slot: hotkey::HotkeySlot,
) -> BeginDecision {
    if !open {
        BeginDecision::DropClosed
    } else if recording == Some(slot) {
        // 陈旧重复 Begin 优先按取消处理（可能同时已失焦，取消更安全）。
        BeginDecision::CancelDuplicate
    } else if focused == Some(false) {
        BeginDecision::DropUnfocused
    } else if recording.is_some() {
        BeginDecision::Switch
    } else {
        BeginDecision::Suspend
    }
}

/// 提交只对当前录制槽有效（缺席/错槽一律忽略）。
fn commit_allowed(recording: Option<hotkey::HotkeySlot>, slot: hotkey::HotkeySlot) -> bool {
    recording == Some(slot)
}

/// 单帧快捷键事件：排队命令先于关闭/⌘W 取消（顺序的单一真源，便于单测）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HotkeyFrameEvent {
    Command(settings::HotkeyCommand),
    CloseCancel,
}

fn hotkey_frame_events(
    commands: Vec<settings::HotkeyCommand>,
    close_pending: bool,
) -> Vec<HotkeyFrameEvent> {
    let mut events: Vec<HotkeyFrameEvent> = commands
        .into_iter()
        .map(HotkeyFrameEvent::Command)
        .collect();
    if close_pending {
        events.push(HotkeyFrameEvent::CloseCancel);
    }
    events
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 首帧纠偏(产品形态:四窗皆隐、仅菜单栏图标,启动不弹窗;Task 21):
        // eframe 0.37 在首帧 post_rendering 里**无条件** `set_visible(true)`
        //(`native/epi_integration.rs:443-450`,白闪修复;`create_glutin_windowed_context`
        // 也把它压着 `.with_visible(false)`),故 builder 的 with_visible(false) 本身
        // 不足以保持隐藏。纠偏:首帧 logic 发 `Visible(false)` —— 该命令在同帧
        // `handle_viewport_output` 应用(post_rendering 之后,中间不回事件循环),
        // 窗口不经 WindowServer 合成,全程无 onscreen 条目(实测见 poc-results)。
        if !self.root_hide_issued {
            self.root_hide_issued = true;
            ctx.send_viewport_cmd_to(
                egui::ViewportId::ROOT,
                egui::ViewportCommand::Visible(false),
            );
            // 启动即四窗全隐:排缩窗定时器(否则首帧渲染留下的 surface 常驻)。
            platform::schedule_surface_shrink();
            ctx.request_repaint(); // 排下一帧做原生回读(纠偏命令在本帧末才应用)
            log::info!("根窗首帧纠偏:Visible(false)(产品形态四窗皆隐)");
        } else if !self.root_hide_verified {
            self.root_hide_verified = true;
            log::info!(
                "三窗皆隐回读:根窗 isVisible={:?} 悬浮条 isVisible={:?} 设置 isVisible={:?}(期望恒 false;None = 尚未创建)",
                platform::root_visible(),
                platform::palette_visible(),
                platform::settings_visible()
            );
        }

        // 二次启动唤起:每帧收取 app.sock 事件(投递方已 request_repaint 唤醒)
        self.drain_incoming(ctx);

        // 保存面板助手:检测事件 → 浮层显隐;浮层请求 → 关闭/后台跳转;
        // 跳转结果 → FellBack 时重现提示。锁纪律:状态锁内、原生调用锁外。
        self.drain_panel_events(ctx);
        self.drain_navigate_results(ctx);
        self.handle_overlay_requests(ctx);

        // 发现新版本：非阻塞收结果/看门狗；变化时唤醒设置页；托盘条目每帧同步。
        let (phase, changed, settings_open) = {
            let mut s = self.shared();
            let changed = s.update.tick();
            (s.update.phase().clone(), changed, s.settings.open)
        };
        if changed {
            log::info!("更新状态: {phase:?}");
            if settings_open {
                ctx.request_repaint_of(settings::viewport_id());
            }
        }
        if let Some(tray) = &mut self.tray {
            tray.sync_update(phase);
        }

        // 完整窗口(Task 17):结果会话推进——防抖到期发查询、收两路回投。
        // 放 logic(而非 ui)使根窗隐藏期(close=隐藏)也经 eframe run_logic 收响应,
        // 状态不落后到下次显示;空闲无 pending 时不产生任何帧,不破隐藏零帧。
        self.shared().results.tick(Instant::now());

        // 保存面板助手浮层:查询会话推进(与悬浮条同一工作线程模式)。
        // 仅在可见期 poll(审查残留 #5:隐藏期不消费旧 pending);浮层可见时其
        // pass 也会驱动 poll(根视口被遮挡、logic 不跑时的兜底)。
        {
            let mut s = self.shared();
            if s.overlay.visible {
                s.overlay.poll(Instant::now());
            }
        }

        // 完整窗口尺寸持久化(visual.md §1.2):可见期跟踪,400ms 限频合并写盘。
        self.persist_window_size(ctx, Instant::now());

        // 主题跟随(根视口可见期):悬浮条隐藏、全窗单独可见时只有这里驱动探测。
        // 节流窗上限排一帧(主题变化后 `set_theme` 自带重绘);根窗隐藏/最小化不排帧,
        // 守 L4 隐藏零帧红线。`root_visible()==None` = 平台未接/非 macOS,同样不排。
        if platform::root_visible() == Some(true) {
            self.shared().theme.maybe_apply(ctx, Instant::now());
            ctx.request_repaint_after(theme::CHECK_INTERVAL);
        }

        // 托盘(Task 16):先消费用户动作,再把最新引擎快照落到菜单项(菜单项增删/
        // 文本更新要求主线程——muda 约束;快照由刷新线程写、悬停/点击/拉起后触发)。
        // 本帧若托盘也切换了自启,设置页排队的旧同步值必须作废(否则会用旧值
        // 覆盖刚按真实状态校准过的勾选)——由 `take_tray_autostart_sync` 仲裁。
        let tray_toggled_autostart = self.drain_tray(ctx);
        let autostart_sync = {
            let mut s = self.shared();
            s.settings.take_tray_autostart_sync(tray_toggled_autostart)
        };
        if let Some(desired) = autostart_sync {
            if let Some(tray) = &self.tray {
                tray.set_autostart_checked(desired);
                log::info!("设置页自启:同步托盘勾选 {desired}");
            }
        }
        // 快捷键录制命令(设置页 → App):挂起/恢复/提交只能主线程执行。
        // 顺序由 `hotkey_frame_events` 单点定义：先命令，后关闭/⌘W 取消。
        self.process_hotkey_events(ctx);
        // 语言切换后原生托盘菜单不随 egui 重绘:主线程消费挂起标记并原地刷新文案,
        // 再走常规快照同步(状态项/缺口项也按当前语言重写)。
        let relabel_tray = self.shared().settings.take_tray_relabel_pending();
        if let Some(tray) = &mut self.tray {
            if relabel_tray {
                tray.relabel();
            }
            tray.sync();
        }

        // 全局热键(Task 15/21):正常路径已在主队列直调完成(不等帧,绕开不可见
        // 窗口 ≥100ms 的 repaint 节流;hotkey.rs 模块注释「动作直调与帧内兜底」),
        // 此处只消费**兜底队列**(直调目标未回填或共享锁被持时的罕见遗留)。
        while let Some(action) = hotkey::poll_action(&self.hotkey) {
            match action {
                hotkey::Action::TogglePalette => {
                    log::info!(
                        "热键 {} 兜底:切换悬浮条",
                        self.hotkey.name().as_deref().unwrap_or("(未注册)")
                    );
                    self.toggle_palette(ctx, "热键兜底");
                }
                hotkey::Action::OpenResults => {
                    // 键位可自定义,日志不写死 ⌥⇧F(兜底路径罕见,快照克隆成本可忽略)。
                    let keys = self.hotkey.snapshot().window_keys.concat();
                    log::info!(
                        "热键 {} 兜底:打开完整窗口",
                        if keys.is_empty() {
                            "(未注册)"
                        } else {
                            &keys
                        }
                    );
                    self.open_results(ctx);
                }
            }
        }

        // 平台补丁:找悬浮条窗(首见即施加)/漂移核对(可见期)。
        // 视口窗由 eframe 在注册 pass 末尾创建:首帧找不到属正常,下帧重试(有上限)。
        if let Err(err) = platform::ensure_patched() {
            log::warn!("悬浮条平台补丁失败: {err}");
        }
        // 浮层窗同款补丁(视口创建后首见即换类;隐藏期 reassert 为 no-op)。
        if let Err(err) = platform::ensure_overlay_patched() {
            log::warn!("浮层平台补丁失败: {err}");
        }
        if !platform::attached() && self.attach_attempts < PATCH_ATTACH_ATTEMPTS {
            self.attach_attempts += 1;
            ctx.request_repaint();
        } else if !platform::attached() && self.attach_attempts == PATCH_ATTACH_ATTEMPTS {
            self.attach_attempts += 1; // 只留痕一次
            log::warn!("悬浮条窗 {PATCH_ATTACH_ATTEMPTS} 帧内未出现:平台补丁未施加");
        }

        // 排帧与主题跟随都在悬浮条可见期由悬浮条 callback 自驱(L4);
        // 全隐藏期此处不排帧 → 事件循环回 Wait(隐藏态 CPU 0% 红线)。
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // 空闲判定采样(根视口;悬浮条/设置/浮层在各自绘制入口采样)。采样点必须在
        // 真渲染 pass:logic() 在根窗隐藏期也会被 eframe 唤起,而那时读到的是
        // 上一显示帧的陈旧 events,反复记活跃会把 60s 静默计时无限后推,
        // 「全隐藏归还」就永远不会触发。
        idle::note_viewport_activity(&ctx);

        // ⌘W = 关闭完整窗口（与红点同语义：隐藏，不退出 App）。
        // 仅根视口持焦时处理，避免悬浮条持焦时误关背后的完整窗口。
        let root_focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if root_focused && handle_root_close_shortcut(&ctx) {
            log::info!("⌘W:隐藏完整窗口(与红点关闭同语义)");
            self.window_size.flush(&self.paths.data_dir);
        }

        // 回退留痕哨兵(延迟到 ui() 因 ctx.fonts() 在首个 pass 前不可用):
        // system_fonts 已移除、provider 恒零,discovered_fonts 应**恒空**;
        // 非空 = 有人重新接入了 provider——fontique 单次查询固定枚举开销
        // 194MB(A6b 实测),按内存回归立即处理。
        if !self.fonts_logged {
            let names: Vec<String> = ctx.fonts(|f| {
                f.discovered_fonts()
                    .iter()
                    .map(|i| i.name.clone())
                    .collect()
            });
            if !names.is_empty() {
                log::warn!("字体 provider 回归(应恒空):{names:?}");
                self.fonts_logged = true;
            }
        }

        // 根窗关闭 = 隐藏(关闭窗口不退出 App,product §2);真退出走 dev 按钮/托盘。
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.quitting {
                log::info!("退出:根窗关闭请求放行");
            } else {
                log::info!("根窗关闭请求:取消关闭,改为隐藏窗口(「退出 App」按钮才真退)");
                // 隐藏后不再有帧观察尺寸:限频窗内未写的最终尺寸在此补写,
                // 否则「调整后 400ms 内点红点」会丢最后一次调整。
                self.window_size.flush(&self.paths.data_dir);
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                platform::quick_look_dismiss();
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                platform::schedule_surface_shrink();
                // 图标互查:设置窗口仍可见则保留
                if platform::settings_visible() != Some(true) {
                    platform::set_dock_icon(false);
                }
            }
        }

        // 主视口:完整结果窗口(Task 17;product §2;右键菜单 Task 18 需 Frame 取窗)。
        // 图标缓存与悬浮条共用一份(UTI 纹理跨视口复用;勿开第二个 worker)。
        //
        // 根窗隐藏时不绘制:eframe 在 macOS 上把 `ViewportInfo.visible()` 报成 None,
        // 任何一帧都会跑完整 UI pass——隐藏期绘制不仅白耗 CPU,还会让已清空的
        // 字形图集重新长出来(全隐缩窗后的回收帧必须保持空 pass)。原生 isVisible
        // 为准(None = 未接平台,保留绘制)。
        if platform::root_visible() != Some(false) {
            let mut s = self.shared();
            let Shared {
                palette, results, ..
            } = &mut *s;
            results.draw(ui, frame, palette.icons_mut());
        }

        // 齿轮点击只置请求位(results 绘制内不重入共享锁);锁释放后统一开窗。
        let settings_requested = self.shared().results.take_settings_request();
        if settings_requested {
            log::info!("齿轮:打开设置窗口");
            open_settings(&self.shared, &ctx);
        }

        // 悬浮条第二视口:常驻注册(deferred;隐藏不销毁,L1/L2 选路 A)。
        // 回调在悬浮条自己的 pass 里跑(根窗被完全遮挡时照常渲染)。
        let shared = Arc::clone(&self.shared);
        ctx.show_viewport_deferred(
            palette_viewport_id(),
            palette_builder(),
            move |ui, _class| draw_palette(ui, &shared),
        );

        // 设置第三视口:常驻注册(deferred;与悬浮条同模式,隐藏走 Visible(false))。
        let shared = Arc::clone(&self.shared);
        ctx.show_viewport_deferred(
            settings::viewport_id(),
            settings::builder(),
            move |ui, _class| settings::draw(ui, &shared),
        );

        // 保存面板目录助手浮层第四视口:常驻注册(deferred;隐藏不销毁)。
        // 显示/隐藏与定位由平台层原生完成(Task 14 接线);本回调只负责绘制。
        let shared = Arc::clone(&self.shared);
        ctx.show_viewport_deferred(
            overlay::overlay_viewport_id(),
            overlay_builder(),
            move |ui, _class| draw_overlay(ui, &shared),
        );
    }

    /// 清屏色 = 全透明(各视口共用;eframe 默认 `(12,12,12,180)` 半透明黑)。
    ///
    /// 悬浮条圆角外区域靠这一层透出桌面:eframe 默认色会把整圈染成暗色
    /// (实测圆角外 (20,21,24) = 默认色叠桌面 (41,44,53) 的合成——透明已生效但
    /// 仍显"黑角"的另一半)。根视口不受影响:`ui/results.rs::draw_impl` 每帧
    /// 最先铺整窗窗口底色(`window_tokens::FullWindowPalette::window_fill`,
    /// 含透明标题栏下方区域),清屏色不可见。
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // 退出兜底:400ms 限频窗内最后一次调整若未写盘,退出前补写(隐藏路径、
        // 托盘退出路径已各调一次;flush 幂等,重复调用无副作用)。
        self.window_size.flush(&self.paths.data_dir);
        // 补丁 b:窗口销毁前还原窗口类(改类窗口被 winit drop → abort;
        // PoC-1b 三方对照实证「还原即干净退出」)。
        platform::revert_class();
    }
}

/// 悬浮条视口的绘制回调(deferred:自己的 pass 内运行,含 tick/主题/动作落地)。
fn draw_palette(ui: &mut egui::Ui, shared: &Arc<Mutex<Shared>>) {
    let ctx = ui.ctx().clone();
    // 隐藏期不绘制(理由同根视口:eframe 会跑隐藏视口的 pass,绘制会让
    // 回收帧重新长字形图集)。原生 isVisible 为准;None = 平台未接,保留绘制。
    if platform::palette_visible() == Some(false) {
        return;
    }
    // 空闲判定采样(悬浮条视口输入;常驻交互频繁,空闲口径见 idle 模块注释)。
    idle::note_viewport_activity(&ctx);
    let now = Instant::now();
    let mut s = match shared.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };

    // 主题跟随:探帧上限 = 节流窗(悬浮条 pass 自排;根窗被全遮时仍跟系统外观)。
    s.theme.maybe_apply(&ctx, now);
    // 查询会话驱动:防抖到期发查询线程 + 收结果(UI 线程不做阻塞查询;
    // 根视口 logic 未跑时由此驱动,如主窗被完全遮挡)。
    s.palette.tick(now);
    // 顶边钉住:高度随内容变化时重锚(平台层原生摆位)。
    platform::reanchor();

    // 视口关闭请求(如 ⌘W):视同隐藏并压回隐藏态,不让视口悬空
    // (eframe 对子视口的关闭请求不自行处理,须由应用消费)。
    if ctx.input(|i| i.viewport().close_requested()) {
        hide_palette_state(&mut s);
        send_palette_hide(&ctx, "视口关闭请求");
    }

    let action = s.palette.draw(ui);
    match action {
        Some(palette::Action::Hide) => {
            // 状态部分就地改(已持锁);命令下发共用 `send_palette_hide`
            // (egui 命令只入队列、无 AppKit 调用,持锁安全)。
            hide_palette_state(&mut s);
            send_palette_hide(&ctx, "Esc/打开成功/失焦");
        }
        Some(palette::Action::FullWindow) => {
            // ⌥↵:查询词灌入完整窗口并立即查询(空词 = 清空态开窗);随后聚焦 + 补查状态。
            if let Some(q) = s.palette.full_window_request.take() {
                log::info!("⌥↵:打开完整窗口并带查询词 {q:?}");
                s.results.set_query(&q);
            }
            s.results.on_window_shown();
            s.palette_visible = false;
            s.palette.reset();
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            platform::schedule_surface_shrink();
            // 根窗走平台层原生(activate + 置前;单发 egui 命令不切 Space,实测):
            // 出锁再调(锁纪律——原生调用可被 winit 同步派发,持锁即自锁)。
            drop(s);
            // 垫窗随面板收掉(本分支提前 return,末尾统一同步到不了这里)
            platform::sync_blur(
                false,
                palette::PANEL_RADIUS_F64,
                palette::MASCOT_STRIP as f64,
                0.0,
            );
            show_root_window(&ctx);
            return; // 开窗后本 pass 结束(悬浮条已隐藏,下面不再排帧)
        }
        None => {}
    }

    // 可见期排下一帧(主题跟随节流上限);隐藏后不再排帧(L4 隐藏零帧)。
    if s.palette_visible {
        ctx.request_repaint_after(theme::CHECK_INTERVAL);
        // 吉祥物动作排帧(≈30fps;Task 9)。只在可见期排,隐藏后随 L4 归零。
        ctx.request_repaint_after_for(mascot::FRAME_INTERVAL, palette_viewport_id());
    }
    // 毛玻璃垫窗:显隐随面板(PoC-7 定案)。**放在动作处理之后**——隐藏当帧即
    // orderOut,否则按 L4「隐藏后不排帧」可能长期残留一块磨砂矩形(review 2026-10-17)。
    // 垫窗只覆盖**面板**(窗口去顶部形象条),否则形象条会变成一块磨砂矩形。
    // 高度跟本帧**可见面板高**走:过渡期窗口先停在旧高,整窗垫窗会在可见面板
    // 下方露磨砂带 / 增长时先铺整块目标高磨砂。
    platform::sync_blur(
        s.palette_visible,
        palette::PANEL_RADIUS_F64,
        palette::MASCOT_STRIP as f64,
        s.palette.visible_panel_h() as f64,
    );
}

/// 浮层视口绘制回调(deferred:自己的 pass 内运行)。
///
/// 锁纪律同 `draw_palette`:短锁取 `Shared` 后在本回调内绘制(无原生调用);
/// 隐藏期直接返回,保持全隐缩窗后的回收帧为空 pass(不重新长字形图集)。
fn draw_overlay(ui: &mut egui::Ui, shared: &Arc<Mutex<Shared>>) {
    let ctx = ui.ctx().clone();
    let mut s = lock_shared(shared);
    if !s.overlay.visible {
        return;
    }
    idle::note_viewport_activity(&ctx);
    // 子视口关闭请求(⌘W/系统红点):eframe 不自行处理,转成 dismiss_requested 交给
    // 接线层隐藏(与 palette/settings 的隐藏惯例一致),请求不悬空。
    if ctx.input(|i| i.viewport().close_requested()) {
        s.overlay.dismiss_requested = true;
    }
    // 查询会话驱动:根视口被完全遮挡、`logic` 不跑时仍由此推进(与悬浮条同因)。
    s.overlay.poll(Instant::now());
    // 浮层不透明纯色底(不做毛玻璃);内容留白由 CentralPanel 默认内边距提供。
    egui::CentralPanel::default().show(ui, |ui| {
        overlay::show_overlay(ui, &mut s.overlay);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_w(modifiers: egui::Modifiers) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            events: vec![egui::Event::Key {
                key: egui::Key::W,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }],
            ..Default::default()
        }
    }

    fn root_commands(out: &egui::FullOutput) -> Vec<egui::ViewportCommand> {
        out.viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.commands.clone())
            .unwrap_or_default()
    }

    /// 端到端：两主题的五档基础字号均按全局倍率安装，family 保持。
    ///
    /// 非默认倍率只用线程局部覆盖（`with_test_scale`），不写进程级原子。
    #[test]
    fn install_base_text_styles_scales_both_themes() {
        let ctx = egui::Context::default();
        let expected = [
            (egui::TextStyle::Small, egui::FontFamily::Proportional, 11.0),
            (egui::TextStyle::Body, egui::FontFamily::Proportional, 15.0),
            (
                egui::TextStyle::Button,
                egui::FontFamily::Proportional,
                15.0,
            ),
            (
                egui::TextStyle::Heading,
                egui::FontFamily::Proportional,
                20.0,
            ),
            (
                egui::TextStyle::Monospace,
                egui::FontFamily::Monospace,
                15.0,
            ),
        ];

        for scale in [1.0, 1.15] {
            font::with_test_scale(scale, || {
                install_base_text_styles(&ctx);
                for theme in [egui::Theme::Dark, egui::Theme::Light] {
                    let style = ctx.style_of(theme);
                    for (text_style, family, base) in &expected {
                        let installed = style
                            .text_styles
                            .get(text_style)
                            .unwrap_or_else(|| panic!("{theme:?} 缺少 {text_style:?} 基础字号"));
                        assert!(
                            (installed.size - base * scale).abs() < 1e-5,
                            "{theme:?} {text_style:?}: 期望 {}×{scale}，实际 {}",
                            base,
                            installed.size
                        );
                        assert_eq!(
                            &installed.family, family,
                            "{theme:?} {text_style:?} family 不得改变"
                        );
                    }
                }
            });
        }
    }

    /// macOS 原生拉丁字体：SF Pro / SF Mono 必须排在 family 首位，且能实际排版
    /// （中文由内置 Noto Sans SC 覆盖，见 cjk 字体专项测试）。
    #[cfg(target_os = "macos")]
    #[test]
    fn system_ui_fonts_are_primary_and_rasterize() {
        for (name, path, _) in SYSTEM_UI_FONT_SPECS {
            assert!(
                std::path::Path::new(path).exists(),
                "{name} 字体不存在: {path}"
            );
        }
        let ctx = egui::Context::default();
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(200.0, 100.0),
            )),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input(), |_ui| {});
        out.textures_delta.clear();
        install_ui_fonts(&ctx, true);
        let mut out = ctx.run_ui(input(), |_ui| {});
        out.textures_delta.clear();

        let (families, galley) = ctx.fonts_mut(|fonts| {
            let families = fonts.definitions().families.clone();
            let galley = fonts.layout_no_wrap(
                "English 中文 123".to_owned(),
                egui::FontId::proportional(15.0),
                egui::Color32::WHITE,
            );
            (families, galley)
        });
        for (name, _, family) in SYSTEM_UI_FONT_SPECS {
            let list = families.get(&family).expect("字体 family 缺失");
            assert_eq!(
                list.first().map(String::as_str),
                Some(name),
                "{family:?} 首位应为 {name}，实际 {list:?}"
            );
        }
        assert!(galley.size().x > 0.0, "SF Pro 排版失败（空 galley）");
    }

    /// 生成一批「文件名」行(去重,galley 缓存必 miss,模拟结果窗口滚动逐行
    /// 布局的真实形态)。字符集含真实长尾:汉字 + 韩文 + 假名 + 符号 + 数字
    /// ——A6b 实测「纯数字查询也能命中长尾字形」,行集必须混入。
    fn cjk_result_rows(n: usize) -> Vec<String> {
        const CHARSET: [char; 60] = [
            '项', '目', '文', '件', '夹', '报', '告', '终', '稿', '会', '议', '纪', '要', '设',
            '计', '图', '纸', '数', '据', '备', '份', '照', '片', '视', '频', '音', '乐', '系',
            '统', '日', '志', '配', '置', '用', '户', '下', '载', '客', '户', '端', '服', '务',
            '器', '端', '口', '密', '码', '한', '국', '어', 'メ', 'モ', '⌘', '⌥', '✓', '★', '→',
            '•', '§', '⚙',
        ];
        (0..n)
            .map(|i| {
                let a = CHARSET[i % CHARSET.len()];
                let b = CHARSET[(i * 7 + 3) % CHARSET.len()];
                let c = CHARSET[(i * 13 + 11) % CHARSET.len()];
                format!("{a}{b}{c}_报告_Q{i}.docx")
            })
            .collect()
    }

    /// 肯定无字形的探针字符(CJK 扩展 B 生僻字;fontTools 实测四个内置字体
    /// 与 SF Pro 均无 cmap 项),以及对应文件名行。
    const MISSING_GLYPH_PROBE: char = '\u{2A6B2}';

    /// 测试哨兵:统计 provider 被询问的次数(恒返回 None → 缺字形走 tofu,
    /// 但「有没有问」正是被测信号)。生产构建已无任何 provider;把它挂上
    /// 是为了证明「内置覆盖链内的字形根本不会问到 provider」——即使未来
    /// 有人误接了 provider(A6b 回归面),覆盖链也把它挡住。
    #[derive(Default)]
    struct CountingProvider(std::sync::atomic::AtomicUsize);

    impl egui::epaint::text::FontProvider for CountingProvider {
        fn font_for(
            &self,
            _request: &egui::epaint::text::FallbackRequest<'_>,
        ) -> Option<egui::FontInsert> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            None
        }
    }

    fn probe_input() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(600.0, 400.0),
            )),
            ..Default::default()
        }
    }

    fn run_layout_pass(ctx: &egui::Context, rows: &[String]) {
        let mut out = ctx.run_ui(probe_input(), |ui| {
            for row in rows {
                ui.label(row.as_str());
            }
        });
        out.textures_delta.clear();
    }

    /// 覆盖链钉(tech.md §6.2.1,A6b 后形态):每 family 前两位 =「拉丁 +
    /// Noto Sans SC」(Highest),尾部 = KR/Symbols/Symbols2(Lowest);
    /// 汉字/韩文/符号长尾全部命中已装字体——provider 即使存在也轮不到。
    #[cfg(target_os = "macos")]
    #[test]
    fn bundled_font_chain_order_and_long_tail_coverage() {
        let ctx = egui::Context::default();
        install_ui_fonts(&ctx, true);
        let mut out = ctx.run_ui(probe_input(), |_ui| {});
        out.textures_delta.clear();

        let families = ctx.fonts(|f| f.definitions().families.clone());
        let tail: Vec<&str> = BUNDLED_FONTS
            .iter()
            .filter(|(_, _, p)| *p == egui::FontPriority::Lowest)
            .map(|(n, _, _)| *n)
            .collect();
        for (family, latin) in [
            (egui::FontFamily::Proportional, "SF Pro"),
            (egui::FontFamily::Monospace, "SF Mono"),
        ] {
            let list = families.get(&family).expect("family 表缺失");
            assert_eq!(
                &list[0..2],
                &[latin, "Noto Sans SC"],
                "{family:?} 前两位应为「{latin} + Noto Sans SC」,实际 {list:?}"
            );
            assert_eq!(
                &list[list.len() - tail.len()..],
                &tail[..],
                "{family:?} 尾部应为 KR/Symbols Lowest 段,实际 {list:?}"
            );
        }
        // 按键帽全覆盖钉:键帽文案是**字面量**而非长尾样本,必须在生产同款
        // 字体链里逐一可渲染(曾因 `↵` U+21B5 在所有内置字体与 SF Pro 中均缺失,
        // 移除 system_fonts 后状态栏显示 tofu;测试只覆盖 `⏎` 漏掉了它)。
        // 新增键帽符号时此处自动纳入,不必再靠人工挑样本。
        ctx.fonts_mut(|f| {
            let keys: Vec<&str> = crate::ui::palette::footer_groups()
                .iter()
                .map(|(_, keys)| *keys)
                .collect();
            for keys in keys {
                for family in [
                    egui::FontId::proportional(11.0),
                    egui::FontId::monospace(11.0),
                ] {
                    assert!(
                        f.has_glyphs(&family, keys),
                        "键帽 {keys:?} 在 {family:?} 下缺字形(检查 BUNDLED_FONTS 覆盖)"
                    );
                }
            }
        });

        // 长尾字形全部命中已装字体(汉字/韩文/假名/符号/数字混排)。
        ctx.fonts_mut(|f| {
            for sample in [
                "项目文件报告终稿.docx……《会议》纪要！（数据）备份～v2",
                "한국어_문서_대하여.hwp",
                "メモ_設定ファイル.txt",
                "ショートカット ⌥⌘⇧ ⏎ ✓★→•§⚙ 123",
            ] {
                assert!(
                    f.has_glyphs(&egui::FontId::proportional(15.0), sample),
                    "长尾字形应全部由已装字体覆盖:{sample}"
                );
                assert!(
                    f.has_glyphs(&egui::FontId::monospace(13.0), sample),
                    "Monospace family 同样要覆盖:{sample}"
                );
            }
        });
    }

    /// 长尾布局不得询问 provider(A6b 回归钉):内置覆盖链就位时,即使有人
    /// 误挂了 provider(fontique 194MB/次固定枚举开销),汉字/韩文/符号混排
    /// 行也一次都不该问到它。
    #[cfg(target_os = "macos")]
    #[test]
    fn long_tail_layout_never_consults_provider() {
        let ctx = egui::Context::default();
        let provider = std::sync::Arc::new(CountingProvider::default());
        ctx.add_font_provider(std::sync::Arc::clone(&provider) as _);
        install_ui_fonts(&ctx, true);
        // 字体经 add_font 排队,首个 pass 应用;之后分批布局去重行
        // (galley 缓存 miss,等价结果窗口滚动)。
        run_layout_pass(&ctx, &[]);
        let rows = cjk_result_rows(300);
        for chunk in rows.chunks(64) {
            run_layout_pass(&ctx, chunk);
        }

        assert_eq!(
            provider.0.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "覆盖链内的字形不得触发 provider 询问"
        );
        assert!(
            ctx.fonts(|f| f.discovered_fonts().is_empty()),
            "provider 命中过的字体不得出现在 discovered_fonts"
        );
    }

    /// 未知字形行为钉(A6b 裁决形态):覆盖集外的生僻字(扩展 B)在
    /// **无任何 provider** 的生产形态下——
    /// ① 按 `MissingGlyphPolicy::Tofu`(egui 默认)显示:galley 照常产出
    ///    非 tofu 零宽;
    /// ② `has_glyph` 如实报 false(供上层判断覆盖面);
    /// ③ 不 panic、不拖垮布局。
    #[cfg(target_os = "macos")]
    #[test]
    fn unknown_glyph_renders_tofu_without_any_provider() {
        assert_eq!(
            MISSING_GLYPH_PROBE, '\u{2A6B2}',
            "探针字符须为扩展 B 生僻字(资源实测无字形)"
        );
        let ctx = egui::Context::default();
        assert_eq!(
            ctx.missing_glyph_policy(),
            egui::epaint::text::MissingGlyphPolicy::Tofu,
            "tofu 是 egui 默认策略,显式依赖之(勿改 Panic/Invisible)"
        );
        install_ui_fonts(&ctx, true);
        run_layout_pass(&ctx, &[]);
        ctx.fonts_mut(|f| {
            assert!(
                !f.has_glyph(&egui::FontId::proportional(15.0), MISSING_GLYPH_PROBE),
                "探针字符应确实无字形(否则失去探针意义)"
            );
            let galley = f.layout_no_wrap(
                format!("报告_{MISSING_GLYPH_PROBE}_终稿.docx"),
                egui::FontId::proportional(15.0),
                egui::Color32::WHITE,
            );
            assert!(
                galley.size().x > 0.0,
                "含未知字形的行必须照常布局(tofu 占位,非空 galley)"
            );
        });
    }

    /// 突变验证:同一探针行,若(误)有 provider 在场——fontique 慢路径的
    /// 入口 `FontProvider::font_for` **必须**被问到。证明上两钉真的拦在
    /// 「provider 不存在 + 覆盖链兜住」上,而非断言恒绿。
    #[cfg(target_os = "macos")]
    #[test]
    fn unknown_glyph_would_consult_a_provider_if_one_existed() {
        let ctx = egui::Context::default();
        let provider = std::sync::Arc::new(CountingProvider::default());
        ctx.add_font_provider(std::sync::Arc::clone(&provider) as _);
        install_ui_fonts(&ctx, false); // 突变:覆盖链缺位(等价 MCE_NO_CJK_FONT)
        run_layout_pass(&ctx, &[]);
        ctx.fonts_mut(|f| {
            f.layout_no_wrap(
                MISSING_GLYPH_PROBE.to_string(),
                egui::FontId::proportional(15.0),
                egui::Color32::WHITE,
            );
        });

        assert!(
            provider.0.load(std::sync::atomic::Ordering::Relaxed) > 0,
            "无覆盖链且有 provider 时,未知字形应触发询问(sentinel 必须有效)"
        );
    }

    /// 系统 UI 字体走只读 mmap(堆粗查易修项 §6.2.5):映射字节与文件一致、
    /// 带有效字体魔数;突变(退回 fs::read 拷贝)不经过本函数,由堆采样对账。
    #[cfg(target_os = "macos")]
    #[test]
    fn system_fonts_load_via_readonly_mmap() {
        for (name, path, _) in SYSTEM_UI_FONT_SPECS {
            let mapped = mapped_font_data(path).unwrap_or_else(|e| panic!("{name} mmap 失败: {e}"));
            let bytes = mapped.0.as_ref();
            let file_len = std::fs::metadata(path).expect("元数据").len();
            assert_eq!(bytes.len() as u64, file_len, "{name} 映射长度应等于文件");
            // TrueType sfnt 版本:SFNS.ttf 是 0x00010000(OTTO/CFF 是 "OTTO")。
            let sfnt = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            assert!(
                sfnt == 0x0001_0000 || &bytes[0..4] == b"OTTO",
                "{name} 首 4 字节应为 sfnt 版本,实际 {sfnt:#010x}"
            );
        }
    }

    /// 图集边长收口(tech.md §6.2.4):插件在输入处理前把 8192 收到 4096;
    /// egui 由此值定 epaint 图集宽度。
    #[test]
    fn atlas_side_cap_clamps_raw_input() {
        assert_eq!(
            clamp_texture_side(None),
            Some(4096),
            "None 必须统一为 Some:视口间 None/Some 交替会让 epaint 逐帧清空图集"
        );
        assert_eq!(clamp_texture_side(Some(2048)), Some(2048), "小值不动");
        assert_eq!(clamp_texture_side(Some(8192)), Some(4096));
        assert_eq!(clamp_texture_side(Some(16384)), Some(4096));

        let ctx = egui::Context::default();
        install_atlas_cap(&ctx);
        let mut input = probe_input();
        input.max_texture_side = Some(8192);
        let mut out = ctx.run_ui(input, |_ui| {});
        out.textures_delta.clear();
        assert_eq!(
            ctx.input(|i| i.max_texture_side),
            MAX_ATLAS_TEXTURE_SIDE,
            "插件收口必须先于输入处理生效(本 pass 的图集宽度由此值决定)"
        );
    }

    /// footprint 采样(`#[ignore]`,单进程单跑,一次只测一侧):
    /// 注入侧 `cargo test -p macEverything-app -- --ignored --nocapture ignored_cjk_footprint`;
    /// 对照侧前置 `MCE_NO_CJK_FONT=1`(与生产同一开关;provider 已移除,对照侧
    /// = 覆盖集外全 tofu 的最小形态)。行集含 A6b 长尾(韩文/假名/符号/数字),
    /// 峰值应停留在百 MB 内——若显著回升,优先怀疑有人重新接入 provider。
    #[cfg(target_os = "macos")]
    #[ignore = "footprint 采样:手动单跑,输出供规格验收(A6/A6b)引用;MCE_NO_CJK_FONT=1 选对照侧"]
    #[test]
    fn ignored_cjk_footprint() {
        let install_bundled = std::env::var_os(CJK_FONT_DISABLE_ENV).is_none();
        footprint_harness(install_bundled);
    }

    /// 当前进程 phys_footprint(MB;口径与 scripts/mem-watch.sh 相同)。
    /// `/usr/bin/footprint -f bytes` 的 `phys_footprint:` 行。
    #[cfg(target_os = "macos")]
    fn phys_footprint_mb() -> u64 {
        let out = std::process::Command::new("/usr/bin/footprint")
            .args(["-f", "bytes", &std::process::id().to_string()])
            .output()
            .expect("运行 /usr/bin/footprint 失败");
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(rest) = line.trim().strip_prefix("phys_footprint:") {
                let bytes: u64 = rest
                    .split_whitespace()
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                return bytes / (1024 * 1024);
            }
        }
        0
    }

    /// 进程生命周期峰值 footprint(MB;同 `footprint` 工具的
    /// `phys_footprint_peak:` 行,免起 vmmap 附加)。
    #[cfg(target_os = "macos")]
    fn lifetime_peak_mb() -> u64 {
        let out = std::process::Command::new("/usr/bin/footprint")
            .args(["-f", "bytes", &std::process::id().to_string()])
            .output()
            .expect("运行 /usr/bin/footprint 失败");
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(rest) = line.trim().strip_prefix("phys_footprint_peak:") {
                let bytes: u64 = rest
                    .split_whitespace()
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                return bytes / (1024 * 1024);
            }
        }
        0
    }

    #[cfg(target_os = "macos")]
    #[expect(clippy::print_stderr)]
    fn footprint_harness(install_bundled: bool) {
        let started = std::time::Instant::now();
        let ctx = egui::Context::default();
        // 生产同形态:无任何 provider(system_fonts 已随 feature 移除)。
        install_ui_fonts(&ctx, install_bundled);
        run_layout_pass(&ctx, &[]);
        let rows = cjk_result_rows(2000);
        for chunk in rows.chunks(64) {
            run_layout_pass(&ctx, chunk);
        }
        let discovered = ctx.fonts(|f| {
            f.discovered_fonts()
                .iter()
                .map(|i| i.name.clone())
                .collect::<Vec<_>>()
        });
        eprintln!(
            "HARNESS install_bundled={install_bundled} rows={} elapsed={:?} discovered_fonts={discovered:?} phys_footprint={} MB lifetime_peak={} MB",
            rows.len(),
            started.elapsed(),
            phys_footprint_mb(),
            lifetime_peak_mb(),
        );
    }

    /// 本地补丁钉(vendor/epaint 的 MCE 标记):字体图集**部分增量**必须量化到
    /// 2 的幂尺寸。原因:每帧增量按脏区任意尺寸分配 ColorImage,macOS malloc 按
    /// 尺寸类缓存 VM 段 → 滚动时尺寸类无界、Malloc Large (empty) 持续累积
    /// (实测 71 块 × 4–10MB、峰值 1.9–2.8GB)。量化把尺寸类压回十几个。
    /// 突变(去掉补丁 / 上游升级覆盖 vendor)→ 尺寸不再为 2 的幂,本测红。
    #[test]
    fn epaint_partial_delta_is_size_quantized() {
        use egui::epaint::{ImageData, TextOptions, TextureAtlas};
        let mut atlas = TextureAtlas::new([4096, 256], TextOptions::default());
        let _ = atlas.take_delta(); // 清掉初始 EVERYTHING
        let ((_x, _y), region) = atlas.allocate((7, 30));
        for px in region.pixels.iter_mut() {
            *px = egui::Color32::from_rgb(1, 2, 3);
        }
        let delta = atlas.take_delta().expect("脏区应产生增量");
        let [w, h] = delta.image.size();
        assert!(
            w.is_power_of_two() && h.is_power_of_two(),
            "增量尺寸应量化到 2 的幂,实际 {w}x{h}"
        );
        assert!(w >= 7 && h >= 30, "量化不得裁掉脏区({w}x{h} < 7x30)");
        let pos = delta.pos.expect("部分增量应带 pos");
        assert!(
            pos[0] + w <= 4096 && pos[1] + h <= 256,
            "量化后应仍在图集内"
        );
        // 多传区域必须取自图集现有像素(只多传,不改变内容)。
        let ImageData::Color(img) = &delta.image;
        let atlas_img = atlas.image();
        let aw = atlas_img.width();
        for yy in 0..h {
            for xx in 0..w {
                let a = atlas_img.pixels[(pos[1] + yy) * aw + pos[0] + xx];
                let b = img.pixels[yy * w + xx];
                assert_eq!(
                    (a.r(), a.g(), a.b(), a.a()),
                    (b.r(), b.g(), b.b(), b.a()),
                    "增量像素应与图集一致 @({xx},{yy})"
                );
            }
        }
    }

    /// 本地补丁钉:不同脏区高度只产生**有界**的尺寸类(量化到 2 的幂),
    /// 这是「Malloc Large (empty) 不再随滚动线性累积」的机制保证。
    #[test]
    fn epaint_delta_size_classes_are_bounded() {
        use egui::epaint::{TextOptions, TextureAtlas};
        let mut atlas = TextureAtlas::new([4096, 2048], TextOptions::default());
        let _ = atlas.take_delta();
        let mut classes = std::collections::BTreeSet::new();
        for h in 1..=40usize {
            let _ = atlas.allocate((7, h));
            if let Some(delta) = atlas.take_delta() {
                classes.insert(delta.image.size());
            }
        }
        assert!(!classes.is_empty(), "应产生增量(否则测试失去意义)");
        assert!(
            classes.len() <= 16,
            "增量尺寸类应有界(≤16),实际 {} 个: {classes:?}",
            classes.len()
        );
    }

    /// 本地补丁钉:整图增量(clear/扩容)与图集**共享同一块内存**,不再深拷贝。
    /// 突变(改回 `self.image.clone()` 深拷贝)→ `Arc::ptr_eq` 为假,本测红。
    #[test]
    fn epaint_full_delta_shares_atlas_image() {
        use egui::epaint::{ImageData, TextOptions, TextureAtlas};
        use std::sync::Arc;
        let mut atlas = TextureAtlas::new([4096, 256], TextOptions::default());
        let delta = atlas.take_delta().expect("初始应产生整图增量");
        let ImageData::Color(img) = delta.image;
        assert!(
            Arc::ptr_eq(&img, &atlas.image_shared()),
            "整图增量应与图集共享同一块内存(不得深拷贝)"
        );
    }

    /// 本地补丁钉:部分增量复用持久上传缓冲(按量化尺寸成池),
    /// 同尺寸帧不新建 `ColorImage`,尺寸交替时也只各留一块。
    /// 突变(每帧 `ColorImage::filled`)→ 分配次数随帧数线性增长,本测红。
    #[test]
    fn epaint_partial_delta_reuses_upload_buffers() {
        use egui::epaint::{TextOptions, TextureAtlas};
        let mut atlas = TextureAtlas::new([4096, 256], TextOptions::default());
        let _ = atlas.take_delta(); // 清掉初始整图

        for _ in 0..20 {
            let _ = atlas.allocate((7, 8));
            let _ = atlas.take_delta().expect("应有部分增量");
        }
        let same_size_allocs = atlas.upload_allocations();
        assert!(
            same_size_allocs <= 1,
            "同尺寸帧应复用同一缓冲,实际分配 {same_size_allocs} 次"
        );

        for i in 0..20 {
            let h = if i % 2 == 0 { 8 } else { 16 };
            let _ = atlas.allocate((7, h));
            let _ = atlas.take_delta().expect("应有部分增量");
        }
        let grew = atlas.upload_allocations() - same_size_allocs;
        assert!(
            grew <= 2,
            "两种尺寸应各复用一块(≤2 次新分配),实际新增 {grew} 次"
        );
    }

    /// 本地补丁钉:clear 是**原地清空**,保留图集尺寸与像素缓冲,
    /// 不得重置到 32 高再走倍增链(每轮产生 4/8/16/32MB 临时分配)。
    /// 突变(`*self = Self::new(..)` 或 clear 内重建)→ 尺寸回落到 32,本测红。
    #[test]
    fn epaint_clear_keeps_atlas_size() {
        use egui::epaint::{ImageData, TextOptions, TextureAtlas};
        use std::sync::Arc;
        let mut atlas = TextureAtlas::new([4096, 256], TextOptions::default());
        let _ = atlas.take_delta();
        let _ = atlas.allocate((7, 300)); // 触发一次高度倍增(256 -> 512)
        let _ = atlas.take_delta();
        let grown = atlas.size();
        assert!(grown[1] >= 512, "前置:图集应已长大,实际 {grown:?}");

        atlas.clear();
        assert_eq!(
            atlas.size(),
            grown,
            "clear 应保留尺寸(原地清空),实际 {}",
            format_args!("{:?}", atlas.size())
        );
        assert!(atlas.fill_ratio() < 0.01, "clear 后填充率应归零");
        let delta = atlas.take_delta().expect("clear 应产生整图增量");
        let ImageData::Color(img) = delta.image;
        assert!(
            Arc::ptr_eq(&img, &atlas.image_shared()),
            "clear 后的整图增量仍应共享图集内存"
        );
    }

    /// 本地补丁钉(端到端):字形图集触达 0.8 clear 时走**原地清空**,
    /// 图集尺寸不回落到 32;否则每轮 clear 都要重新倍增,产生 4/8/16/32MB
    /// 临时分配。突变(GlyphAtlas::clear 改回 `*self = Self::new(..)`)→ 本测红。
    #[test]
    fn epaint_glyph_atlas_clear_stays_in_place() {
        let ctx = egui::Context::default();
        install_ui_fonts(&ctx, true);
        let mut input = probe_input();
        input.max_texture_side = Some(1024); // 小图集,一个 pass 即可填满
        let chars: String = include_str!("atlas_charset.txt")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(chars.chars().count() > 1000, "前置:字符集应足够填满图集");

        // pass 1:布局全部字符 → 图集增长/溢出。
        let mut out = ctx.run_ui(input.clone(), |ui| {
            ui.add(egui::Label::new(egui::RichText::new(&chars)).wrap());
        });
        out.textures_delta.clear();
        let grown = ctx.fonts(|f| f.font_image_size());
        assert!(grown[1] > 32, "前置:图集应已长大,实际 {grown:?}");

        // pass 2:空 pass 开头 fill>0.8 → clear;无文本,不再增长。
        let mut out = ctx.run_ui(input, |_ui| {});
        out.textures_delta.clear();
        let after = ctx.fonts(|f| f.font_image_size());
        assert!(
            after[1] > 32,
            "clear 应原地保留尺寸(实际回落到 {after:?},说明重建了图集)"
        );
        assert!(
            ctx.fonts(|f| f.font_atlas_fill_ratio()) < 0.1,
            "clear 后填充率应归零"
        );
    }

    /// 字号基准纯函数：1.0 / 1.15 两档，测试不改全局倍率。
    #[test]
    fn base_text_sizes_follow_scale() {
        assert_eq!(base_text_sizes(1.0), [11.0, 15.0, 15.0, 20.0, 15.0]);
        for (got, base) in base_text_sizes(1.15)
            .iter()
            .zip([11.0, 15.0, 15.0, 20.0, 15.0])
        {
            assert!(
                (got - base * 1.15).abs() < 1e-5,
                "基准 {base} × 1.15，实际 {got}"
            );
        }
    }

    /// I3 回归钉:隐藏悬浮条必须**立即**收起毛玻璃垫窗。隐藏态 `draw_palette`
    /// 在可见性守卫处直接 return,末尾的 `sync_blur(false)` 永不执行;只发
    /// `Visible(false)` 命令会让磨砂矩形残留在屏上。
    /// 突变:删掉 `send_palette_hide` 里的 `platform::hide_blur()` → 本测红。
    #[test]
    fn send_palette_hide_immediately_hides_blur_layer() {
        use std::sync::atomic::Ordering;
        let before = platform::HIDE_BLUR_CALLS.load(Ordering::Relaxed);
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            send_palette_hide(ui.ctx(), "test");
        });
        // run_ui 的纹理增量必须显式取走(epaint 对未处理增量 drop 会 panic)。
        out.textures_delta.clear();
        let after = platform::HIDE_BLUR_CALLS.load(Ordering::Relaxed);
        assert!(
            after > before,
            "隐藏悬浮条必须先收起垫窗(hide_blur 未被调用)"
        );
    }

    /// ⌘W = 根窗隐藏（与红点关闭同语义）；裸 W 不触发。
    #[test]
    fn cmd_w_hides_root_window() {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(key_w(egui::Modifiers::COMMAND), |ui| {
            assert!(handle_root_close_shortcut(ui.ctx()), "⌘W 应被消费");
        });
        let cmds = root_commands(&out);
        assert!(
            cmds.iter()
                .any(|c| matches!(c, egui::ViewportCommand::Visible(false))),
            "⌘W 应发 Visible(false)"
        );
        out.textures_delta.clear();

        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(key_w(egui::Modifiers::NONE), |ui| {
            assert!(!handle_root_close_shortcut(ui.ctx()), "裸 W 不消费");
        });
        let cmds = root_commands(&out);
        assert!(
            !cmds
                .iter()
                .any(|c| matches!(c, egui::ViewportCommand::Visible(false))),
            "裸 W 不得隐藏根窗"
        );
        out.textures_delta.clear();
    }

    /// 托盘自启切换失败：已知实际态优先（修复同帧同步被仲裁丢弃后的旧勾选）；
    /// 未知态保持点击前值（托盘安装时可能已读到真实启用态）。
    #[test]
    fn autostart_tray_failure_reconciles_known_state_or_reverts() {
        assert!(
            autostart_check_after_failure(Some(true), false),
            "设置页实际为开 → 勾选"
        );
        assert!(
            !autostart_check_after_failure(Some(false), true),
            "设置页实际为关 → 取消勾选"
        );
        assert!(
            autostart_check_after_failure(None, false),
            "未知 → 保持点击前值（开）"
        );
        assert!(
            !autostart_check_after_failure(None, true),
            "未知 → 保持点击前值（关）"
        );
    }

    fn alt_k() -> HotKey {
        HotKey::new(
            Some(global_hotkey::hotkey::Modifiers::ALT),
            global_hotkey::hotkey::Code::KeyK,
        )
    }

    /// Begin 判定：关闭丢弃 > 同槽重复取消 > 已失焦丢弃 > 切槽 > 空闲挂起。
    #[test]
    fn begin_decision_covers_closed_duplicate_unfocused_switch_and_fresh() {
        use hotkey::HotkeySlot::{Palette, Window};

        assert_eq!(
            begin_decision(false, Some(true), None, Palette),
            BeginDecision::DropClosed,
            "关闭后到达的 Begin 必须丢弃（不得挂起无人恢复）"
        );
        assert_eq!(
            begin_decision(false, None, Some(Window), Palette),
            BeginDecision::DropClosed,
            "关闭优先于其它分支"
        );
        assert_eq!(
            begin_decision(true, Some(true), Some(Palette), Palette),
            BeginDecision::CancelDuplicate,
            "陈旧视图重复 Begin 同槽 = 取消"
        );
        assert_eq!(
            begin_decision(true, Some(false), Some(Palette), Palette),
            BeginDecision::CancelDuplicate,
            "重复 Begin 优先取消（即使窗口已失焦）"
        );
        assert_eq!(
            begin_decision(true, Some(true), Some(Palette), Window),
            BeginDecision::Switch
        );
        assert_eq!(
            begin_decision(true, Some(true), None, Palette),
            BeginDecision::Suspend
        );
        assert_eq!(
            begin_decision(true, None, None, Palette),
            BeginDecision::Suspend,
            "焦点未知（尚未报告）不得拒绝正常开始"
        );
        assert_eq!(
            begin_decision(true, Some(false), None, Palette),
            BeginDecision::DropUnfocused,
            "已观测失焦后落地的 Begin 必须丢弃（否则录制长期挂起）"
        );
        assert_eq!(
            begin_decision(true, Some(false), Some(Palette), Window),
            BeginDecision::DropUnfocused,
            "失焦时不得借切槽开始新录制"
        );
    }

    /// Commit 只接受当前录制槽；缺席/错槽忽略。
    #[test]
    fn commit_requires_matching_recording_slot() {
        use hotkey::HotkeySlot::{Palette, Window};

        assert!(commit_allowed(Some(Palette), Palette));
        assert!(!commit_allowed(None, Palette), "无录制态不得换键");
        assert!(!commit_allowed(Some(Window), Palette), "错槽提交不得换键");
        assert!(!commit_allowed(Some(Palette), Window));
    }

    /// 排队命令先于关闭取消执行：关闭当帧的 Commit 仍先落地，Begin 则由
    /// `open=false` 丢弃；队列内部保持 FIFO。
    #[test]
    fn hotkey_frame_commands_run_before_close_cancel() {
        use hotkey::HotkeySlot::Palette;

        let begin = settings::HotkeyCommand::BeginRecord(Palette);
        let commit = settings::HotkeyCommand::Commit {
            slot: Palette,
            hotkey: alt_k(),
        };
        let reset = settings::HotkeyCommand::Reset(Palette);
        assert_eq!(
            hotkey_frame_events(vec![begin, commit, reset], true),
            vec![
                HotkeyFrameEvent::Command(begin),
                HotkeyFrameEvent::Command(commit),
                HotkeyFrameEvent::Command(reset),
                HotkeyFrameEvent::CloseCancel,
            ],
            "命令保持 FIFO（含恢复默认），关闭取消永远最后"
        );

        let events = hotkey_frame_events(vec![commit], true);
        assert_eq!(events.first(), Some(&HotkeyFrameEvent::Command(commit)));
        assert_eq!(events.last(), Some(&HotkeyFrameEvent::CloseCancel));
        assert!(
            commit_allowed(Some(Palette), Palette),
            "关闭取消尚未执行时 Commit 仍是当前槽，先提交"
        );

        assert!(hotkey_frame_events(vec![], false).is_empty());
        assert_eq!(
            hotkey_frame_events(vec![], true),
            vec![HotkeyFrameEvent::CloseCancel],
            "无命令也须消费关闭取消"
        );
    }

    /// 全隐释放 GPU 纹理:字形图集必须能缩回最小高度,并让下一帧带整图增量
    /// (渲染端据此 `glTexImage2D` 重建成小纹理),之后还能重新增长。
    ///
    /// 突变钉:注释掉 `FontsView::clear_glyph_atlas` 里的 `reset_to_min_size`
    /// (或让 `TextureAtlas::reset_to_min_size` 直接返回),`after[1] == 32`
    /// 断言即红——防止「只清内容不缩尺寸」让 GPU 纹理继续常驻。
    #[test]
    fn glyph_atlas_reset_shrinks_image_and_emits_full_delta() {
        let ctx = egui::Context::default();
        let run = |ctx: &egui::Context| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 300.0),
                    )),
                    ..Default::default()
                },
                |_| {},
            )
        };
        let mut out = run(&ctx);
        out.textures_delta.clear();
        let ascii: String = (0x20u8..0x7F).map(char::from).collect();
        // 同一批字符 × 40 个字号 = 40 组独立字形键,足以让图集高度越过 32。
        let grow = |ctx: &egui::Context| {
            ctx.fonts_mut(|f| {
                for i in 0..40 {
                    let size = 14.0 + i as f32;
                    let _ = f.layout_no_wrap(
                        ascii.clone(),
                        egui::FontId::proportional(size),
                        egui::Color32::WHITE,
                    );
                }
                f.font_image_size()
            })
        };
        let before = grow(&ctx);
        assert!(
            before[1] > 32,
            "测试前提:多字号布局必须让图集高度增长,实际 {before:?}"
        );
        let mut out = run(&ctx);
        out.textures_delta.clear();

        ctx.fonts_mut(|f| f.clear_glyph_atlas());
        let after = ctx.fonts(|f| f.font_image_size());
        assert_eq!(after[1], 32, "清空后高度必须缩回最小值(否则 GPU 纹理不缩)");
        assert!(after[0] >= 1024, "宽度保持 max_texture_side");

        let mut out = run(&ctx);
        let font_delta = out
            .textures_delta
            .set
            .get(&egui::TextureId::default())
            .and_then(|deltas| deltas.last())
            .expect("清空后的下一帧必须带字形图集增量");
        assert!(
            font_delta.pos.is_none(),
            "必须是整图增量(局部增量不会重建纹理尺寸)"
        );
        assert_eq!(font_delta.image.size(), after, "增量尺寸 = 缩小后的图集");
        out.textures_delta.clear();

        let regrown = grow(&ctx);
        assert!(regrown[1] > 32, "显示期重新布局必须能再次增长图集");
    }
}
