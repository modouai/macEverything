//! UI 层:三视口壳。
//!
//! 主视口(ROOT)= 完整结果窗口(`ui/results.rs`,Task 17:表格虚拟化/列排序/多选);
//! 第二视口 = 悬浮条(Task 12 UI;Task 14 接平台层:跨 Space/浮动级别/失焦隐藏/多屏定位);
//! 第三视口 = 设置窗口(v2.1:620×600 固定,deferred + 常驻;视口/行为见
//! `ui/settings.rs`,纯视图见 `ui/settings_view.rs`)。
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
//! 中文字体(2026-10-16 调查结论,不显式加载字体文件):
//! ① 苹方已无稳定路径——macOS 26 起移入 AssetsV2 哈希目录,本机实测
//!    `/System/Library/Fonts/PingFang.ttc` 不存在;
//! ② 稳定路径备选均为 .ttc 集合且 face 0 不是简体面(PingFang.ttc face0=HK、
//!    STHeiti Light.ttc face0=TC,fontTools 实查)——静默 pin index 0 会把简体
//!    用户引向错字形面;按名取面须自解析 name 表,成本/风险不划算;
//! ③ 系统回退已给出期望字体:CoreText(zh_CN 语境)对汉字回退即
//!    PingFangSC-Regular(`CTFontCreateForString` 探针实测);egui `system_fonts`
//!    的 fontique 走同一系统回退源。
//! 回退链(现状,实测生效):egui 默认字体(拉丁/图标)→ FontProvider 按缺失字形
//! 逐簇查系统字体(zh 语境 → 苹方;韩文 → AppleSDGothicNeo;emoji → Apple Color Emoji)。
//! `probe_fonts()` 启动留痕候选存在性;运行期实际命中字体在 `ui()` 留痕一次。

pub mod font;
pub mod hotkey_keys;
pub mod icons;
pub mod mascot;
pub mod menu_actions;
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
use std::sync::mpsc::Receiver;
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
use crate::window_state::{self, SizeUpdate, WindowSize, WindowSizeTracker};

/// 系统 CJK 字体候选路径(探测用;结论见模块注释「中文字体」)。
const CJK_FONT_CANDIDATES: [&str; 2] = [
    "/System/Library/Fonts/PingFang.ttc", // 苹方(macOS ≤ 15 路径)
    "/System/Library/Fonts/STHeiti Light.ttc", // 黑体(旧备选)
];

/// macOS 原生 UI 字体：拉丁优先 SF Pro / SF Mono；CJK 缺失字形仍由
/// `system_fonts` 的 CoreText 回退（苹方/AppleSDGothicNeo 等）。
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

/// 首帧接线轮询上限(悬浮条视口窗创建于注册 pass 之后,补丁须下一帧才找得到;
/// 上限防「窗口永不出现」时无限排帧烧 CPU)。
const PATCH_ATTACH_ATTEMPTS: u32 = 300;

/// 启动探测候选字体存在性并留痕(不加载——理由见模块注释「中文字体」)。
fn probe_fonts() {
    for path in CJK_FONT_CANDIDATES {
        log::info!(
            "CJK 字体候选 {path}:{}",
            if std::path::Path::new(path).exists() {
                "存在"
            } else {
                "不存在"
            }
        );
    }
}

/// 安装 macOS 原生 UI 字体（拉丁主字体）。
///
/// 只把 SF Pro / SF Mono 放到对应 family 的**首位**，保留 egui 默认字体与
/// system_fonts 回退链；中文等 CJK 字形仍由 CoreText 回退到苹方。
fn install_system_ui_fonts(ctx: &egui::Context) {
    for (name, path, family) in SYSTEM_UI_FONT_SPECS {
        match std::fs::read(path) {
            Ok(bytes) => {
                ctx.add_font(egui::FontInsert::new(
                    name,
                    egui::FontData::from_owned(bytes),
                    vec![egui::InsertFontFamily {
                        family,
                        priority: egui::FontPriority::Highest,
                    }],
                ));
                log::info!("系统 UI 字体已加载:{name}({path})");
            }
            Err(err) => {
                log::warn!("系统 UI 字体加载失败({path}),保留 egui 默认字体: {err}");
            }
        }
    }
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
    ctx.send_viewport_cmd_to(palette_viewport_id(), egui::ViewportCommand::Visible(false));
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
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
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

pub struct App {
    /// 二次启动唤起通道(app.sock 监听线程 → 主线程;main 接线)。
    incoming: Receiver<Incoming>,
    /// 各视口共享状态(悬浮条/设置回调与根视口逻辑共持同一份)。
    shared: Arc<Mutex<Shared>>,
    /// 运行期系统字体回退留痕一次(`ui()`;诊断用)。
    fonts_logged: bool,
    /// 首帧接线已尝试次数(见 `PATCH_ATTACH_ATTEMPTS`)。
    attach_attempts: u32,
    /// 根窗首帧隐藏纠偏已发出(产品形态三窗皆隐;见 `logic` 首帧纠偏注释)。
    root_hide_issued: bool,
    /// 纠偏回读已留痕一次(原生 `isVisible()` 核对;见 `logic`)。
    root_hide_verified: bool,
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
    /// 悬浮条可见标志(真源;排帧门禁与按钮态都读它)。
    pub palette_visible: bool,
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
        probe_fonts();
        // 拉丁主字体用 macOS 原生 SF Pro / SF Mono；CJK 仍走系统回退。
        install_system_ui_fonts(&cc.egui_ctx);
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
        let hotkey_snapshot = hotkey.snapshot();
        let hotkey_note = hotkey.note().cloned();
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
            palette_visible: false, // 启动隐藏(产品形态:三窗皆隐,仅菜单栏图标)
        }));
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
        Self {
            incoming,
            shared,
            fonts_logged: false,
            attach_attempts: 0,
            root_hide_issued: false,
            root_hide_verified: false,
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
        // 首帧纠偏(产品形态:三窗皆隐、仅菜单栏图标,启动不弹窗;Task 21):
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
            ctx.request_repaint(); // 排下一帧做原生回读(纠偏命令在本帧末才应用)
            log::info!("根窗首帧纠偏:Visible(false)(产品形态三窗皆隐)");
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

        // 完整窗口(Task 17):结果会话推进——防抖到期发查询、收两路回投。
        // 放 logic(而非 ui)使根窗隐藏期(close=隐藏)也经 eframe run_logic 收响应,
        // 状态不落后到下次显示;空闲无 pending 时不产生任何帧,不破隐藏零帧。
        self.shared().results.tick(Instant::now());

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

        // ⌘W = 关闭完整窗口（与红点同语义：隐藏，不退出 App）。
        // 仅根视口持焦时处理，避免悬浮条持焦时误关背后的完整窗口。
        let root_focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if root_focused && handle_root_close_shortcut(&ctx) {
            log::info!("⌘W:隐藏完整窗口(与红点关闭同语义)");
            self.window_size.flush(&self.paths.data_dir);
        }

        // 运行期字体回退留痕一次(诊断:实际命中的系统 CJK 字体;
        // 延迟到 ui() 因 ctx.fonts() 在首个 pass 前不可用)。
        if !self.fonts_logged {
            let names: Vec<String> = ctx.fonts(|f| {
                f.discovered_fonts()
                    .iter()
                    .map(|i| i.name.clone())
                    .collect()
            });
            if !names.is_empty() {
                log::info!("系统字体回退命中:{names:?}");
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
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
        }

        // 主视口:完整结果窗口(Task 17;product §2;右键菜单 Task 18 需 Frame 取窗)。
        // 图标缓存与悬浮条共用一份(UTI 纹理跨视口复用;勿开第二个 worker)。
        {
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
    /// （中文仍由 system_fonts 回退，不要求出现在定义表里）。
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
        install_system_ui_fonts(&ctx);
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
}
