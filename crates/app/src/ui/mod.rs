//! UI 层:双视口壳。
//!
//! 主视口(ROOT)= 完整结果窗口(`ui/results.rs`,Task 17:表格虚拟化/列排序/多选);
//! 第二视口 = 悬浮条(Task 12 UI;Task 14 接平台层:跨 Space/浮动级别/失焦隐藏/多屏定位)。
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
//! 只在悬浮条可见期排帧(悬浮条 pass 自排 `CHECK_INTERVAL` 上限;含主题跟随检查);
//! 全隐藏期不排帧(隐藏态 CPU 0% 红线);唤起时 `ThemeFollow::check_now` 立即补查外观。
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

pub mod menu_actions;
pub mod palette;
pub mod results;
pub mod theme;

use eframe::egui;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::autostart;
use crate::engine_launcher;
use crate::hotkey;
use crate::platform;
use crate::single_instance::Incoming;
use crate::tray::{self, TrayMsg};

/// 系统 CJK 字体候选路径(探测用;结论见模块注释「中文字体」)。
const CJK_FONT_CANDIDATES: [&str; 2] = [
    "/System/Library/Fonts/PingFang.ttc", // 苹方(macOS ≤ 15 路径)
    "/System/Library/Fonts/STHeiti Light.ttc", // 黑体(旧备选)
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

/// 悬浮条视口 id(注册与命令定向同一处,勿散落)。
fn palette_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("palette")
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
    match platform::show_palette() {
        Ok(()) => {
            {
                let mut s = lock_shared(shared);
                s.palette.reset(); // 每次唤起都是全新查询(product §1)
                s.palette_visible = true;
                s.theme.check_now(ctx); // 隐藏期切过外观:亮起立即补查
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

/// 打开完整窗口(托盘「打开完整窗口」/「缺口面板」/⌥⇧F 直达共用):
/// 聚焦搜索框 + 补查引擎状态后原生唤到前台(原生调用在锁外,见锁纪律)。
fn results_open(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context) {
    lock_shared(shared).results.on_window_shown();
    show_root_window(ctx);
}

/// 悬浮条视口 builder(常量;每 pass 传同一份——`patch` 无差异则无命令/无重建)。
/// `visible(false)`:启动隐藏(唤起走平台层原生路径,不用 winit `Visible(true)`)。
fn palette_builder() -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_title(platform::PALETTE_TITLE)
        .with_inner_size([palette::PANEL_W, palette::INITIAL_H])
        .with_decorations(false)
        .with_transparent(true)
        .with_has_shadow(false)
        .with_visible(false)
        .with_always_on_top()
}

pub struct App {
    /// 二次启动唤起通道(app.sock 监听线程 → 主线程;main 接线)。
    incoming: Receiver<Incoming>,
    /// 两视口共享状态(悬浮条回调与根视口逻辑共持同一份)。
    shared: Arc<Mutex<Shared>>,
    /// 运行期系统字体回退留痕一次(`ui()`;诊断用)。
    fonts_logged: bool,
    /// 首帧接线已尝试次数(见 `PATCH_ATTACH_ATTEMPTS`)。
    attach_attempts: u32,
    /// 根窗首帧隐藏纠偏已发出(产品形态两窗皆隐;见 `logic` 首帧纠偏注释)。
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
}

/// 两视口共享的运行时状态。
pub struct Shared {
    /// 悬浮条状态与查询会话(Task 12)。
    pub palette: palette::PaletteState,
    /// 完整窗口(根视口)状态与查询会话(Task 17;与悬浮条各自防抖/代际)。
    pub results: results::ResultsState,
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
        hotkey: hotkey::HotkeyRuntime,
        tray: Option<tray::TrayRuntime>,
    ) -> Self {
        probe_fonts();
        // 首个 pass 之前先应用一次外观:egui 首帧按 System 偏好的 fallback(Dark)
        // 出图,logic() 里首查已晚于该帧 → 浅色系统启动会闪一帧深色。
        let theme = theme::current_theme();
        theme::apply(&cc.egui_ctx, theme);
        log::info!("启动外观:{theme:?}");
        // 平台层:记录根窗句柄(悬浮条窗与根窗同为 NSApp.windows() 成员,须区分)。
        platform::attach_root(cc);
        let hotkey_note = hotkey.note().map(str::to_string);
        let shared = Arc::new(Mutex::new(Shared {
            palette: palette::PaletteState::new(
                cc,
                paths.clone(),
                palette_viewport_id(),
                hotkey_note,
            ),
            results: results::ResultsState::new(cc.egui_ctx.clone(), paths.clone()),
            theme: theme::ThemeFollow::new(theme, theme::CHECK_INTERVAL),
            palette_visible: false, // 启动隐藏(产品形态:两窗皆隐,仅菜单栏图标)
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
        }
    }

    /// 取共享状态(poisoned 兜底:锁内无 panic 期望,毒锁也继续用,不静默丢状态)。
    fn shared(&self) -> std::sync::MutexGuard<'_, Shared> {
        match self.shared.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
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
    fn drain_tray(&mut self, ctx: &egui::Context) {
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
                TrayMsg::ToggleAutostart => {
                    // Task 20:勾选动作已由 muda 在菜单侧翻转;此处按真实注册结果校准——
                    // 成功保留勾选并落下/清除「用户关闭」标记(下次启动不再默认注册),
                    // 失败(如开发形态未打包)复原勾选 + 日志(无角标面,不静默)。
                    let checked = self.tray.as_ref().is_some_and(|t| t.autostart_checked());
                    match autostart::apply_user_choice(&self.paths.data_dir, checked) {
                        Ok(note) => log::info!("托盘:开机自启 → {note}"),
                        Err(err) => {
                            log::warn!("托盘:开机自启设置失败(勾选复原): {err:#}");
                            if let Some(tray) = &self.tray {
                                tray.set_autostart_checked(!checked);
                            }
                        }
                    }
                }
                TrayMsg::Quit => {
                    // 退出只退 App,引擎常驻(product §5)。走 dev 退出按钮同一条干净
                    // 退出路径(置 quitting 放行根窗关闭 → eframe 收尾 → on_exit 还原
                    // 窗口类;`std::process::exit` 会跳过 on_exit,platform/mac.rs 的
                    // 改类还原是干净退出的必要步骤,故不用)。
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
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 首帧纠偏(产品形态:两窗皆隐、仅菜单栏图标,启动不弹窗;Task 21):
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
            log::info!("根窗首帧纠偏:Visible(false)(产品形态两窗皆隐)");
        } else if !self.root_hide_verified {
            self.root_hide_verified = true;
            log::info!(
                "两窗皆隐回读:根窗 isVisible={:?} 悬浮条 isVisible={:?}(期望恒 false;None = 尚未创建)",
                platform::root_visible(),
                platform::palette_visible()
            );
        }

        // 二次启动唤起:每帧收取 app.sock 事件(投递方已 request_repaint 唤醒)
        self.drain_incoming(ctx);

        // 完整窗口(Task 17):结果会话推进——防抖到期发查询、收两路回投。
        // 放 logic(而非 ui)使根窗隐藏期(close=隐藏)也经 eframe run_logic 收响应,
        // 状态不落后到下次显示;空闲无 pending 时不产生任何帧,不破隐藏零帧。
        self.shared().results.tick(Instant::now());

        // 托盘(Task 16):先消费用户动作,再把最新引擎快照落到菜单项(菜单项增删/
        // 文本更新要求主线程——muda 约束;快照由刷新线程写、悬停/点击/拉起后触发)。
        self.drain_tray(ctx);
        if let Some(tray) = &mut self.tray {
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
                        self.hotkey.name().unwrap_or("(未注册)")
                    );
                    self.toggle_palette(ctx, "热键兜底");
                }
                hotkey::Action::OpenResults => {
                    log::info!("热键 ⌥⇧F 兜底:打开完整窗口");
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
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
        }

        // 主视口:完整结果窗口(Task 17;product §2;右键菜单 Task 18 需 Frame 取窗)。
        {
            let mut s = self.shared();
            s.results.draw(ui, frame);
        }

        // 悬浮条第二视口:常驻注册(deferred;隐藏不销毁,L1/L2 选路 A)。
        // 回调在悬浮条自己的 pass 里跑(根窗被完全遮挡时照常渲染)。
        let shared = Arc::clone(&self.shared);
        ctx.show_viewport_deferred(
            palette_viewport_id(),
            palette_builder(),
            move |ui, _class| draw_palette(ui, &shared),
        );
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
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
            show_root_window(&ctx);
            return; // 开窗后本 pass 结束(悬浮条已隐藏,下面不再排帧)
        }
        None => {}
    }

    // 可见期排下一帧(主题跟随节流上限);隐藏后不再排帧(L4 隐藏零帧)。
    if s.palette_visible {
        ctx.request_repaint_after(theme::CHECK_INTERVAL);
    }
}
