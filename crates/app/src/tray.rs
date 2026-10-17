//! 菜单栏托盘(Task 16;product §3「菜单栏(托盘)」)。
//!
//! 形态:菜单栏常驻图标(代码生成 RGBA,免 asset 文件);**左键** = 唤起/隐藏悬浮条
//! (切换语义);**右键** = 菜单[引擎状态(§7 表六态)/⚠ N 个目录未收录(有缺口才挂)/
//! 打开搜索 / 打开完整窗口 / 开机自启(勾选)/ 退出]。状态项在「未运行」时可点击拉起
//! 引擎;状态项与缺口项随引擎快照动态刷新。
//!
//! ## 主线程约束(实锁核对,禁凭记忆)
//!
//! - **菜单/图标创建与文本更新要求主线程**:tray-icon 的 macOS 实现取
//!   `MainThreadMarker::new()`(非主线程直接 Err);muda 文档同款(macOS 上菜单
//!   只能在主线程用)。安装点 = eframe `app_creator` —— 它在 winit 首个
//!   resumed(`StartCause::Init`)之下,正是 tray-icon 文档要求的「事件循环已在跑、
//!   不是刚创建」的最早时机(`eframe` 0.37 glow_integration.rs:535 `resumed` →
//!   `init_run_state` → 调用 app_creator);更新点 = `App::logic` 每帧 `sync()`。
//! - 左键不弹菜单 = `menu_on_left_click(false)`(builder 项;`TrayIconAttributes`
//!   默认 true = 左键也弹菜单)。左键点击事件为 `Click{Left, MouseButtonState::Up}`
//!   (实锁:macOS 实现在 mouseUp 发 Up,见 platform_impl/macos/mod.rs)。
//!
//! ## 事件泵(与 Task 15 热键同模式:阻塞收 + 叫帧)
//!
//! 两个事件源(TrayIconEvent / MenuEvent)各一个泵线程:`recv` 阻塞 park(零 CPU);
//! 命中才 `ctx.request_repaint_of(ROOT)` 叫帧——空闲期 eframe 不排帧,不叫帧事件
//! 会滞留到下一次出帧(hotkey.rs 模块注释的同一证据链)。消息经 mpsc 汇到 UI 帧。
//!
//! 引擎状态刷新走独立线程 + 请求通道(悬停/点击/拉起后触发;**无轮询**,守
//! 「隐藏态 CPU 0%」红线):悬停(`Enter`)期刷新让菜单弹出时状态项已是最新;
//! 快照存 `Arc<Mutex<_>>`,主线程 `sync()` 落到菜单项。
//!
//! ## 右键菜单自弹(2026-10-16 修:副屏点击 → 菜单落主屏)
//!
//! 托盘库内建弹菜单(右按下 → `NSStatusItem.setMenu` + `button.performClick`)把菜单
//! 锚定到**状态项所在的菜单栏窗**;多屏实机取证:该窗恒在主屏菜单栏(副屏点击时
//! 事件 rect = 主屏状态项窗 (823,0,38,33)@2x),菜单随之出现在主屏。故本模块
//! `with_menu_on_right_click(false)` 关掉内建弹出,右按下事件经**主队列直调**
//! (与热键同模式,`hotkey.rs` 模块注释「动作直调」;菜单须主线程弹且不等帧)
//! `platform::popup_tray_menu` —— 按光标所在屏自定位弹出。菜单仍是同一份 muda
//! `Menu`(勾选态/缺口项增删与原来一致;项选择经 muda 的 MenuEvent 回投)。

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use client::{DaemonPaths, StatusOutcome};
use eframe::egui;
use tray_icon::menu::{
    CheckMenuItem, ContextMenu, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem,
};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::platform;

/// 菜单项 id(分派键;字符串常量集中本处,禁散落)。
mod ids {
    pub const STATE: &str = "engine-state";
    pub const GAPS: &str = "gaps";
    pub const OPEN_SEARCH: &str = "open-search";
    pub const OPEN_WINDOW: &str = "open-window";
    pub const AUTOSTART: &str = "autostart";
    pub const QUIT: &str = "quit";
}

/// 引擎未运行且无落盘索引的哨兵态(客户端侧;product §7 表末行「未运行且无落盘索引」
/// 在 status 数据面为 `StatusOutcome::NoIndex`,不带状态串,故此处自设一个)。
pub const NO_ENGINE: &str = "no-engine";

/// 图标边长(px;菜单栏显示时按 22pt 上限等比缩放——tray-icon macOS 实现)。
const ICON_SIZE: u32 = 64;
/// 菜单栏图标资源(应用图标 64px 版,与 `assets/AppIcon.png` 同源)。
/// 解码失败时回落下方代码生成的占位图标(保底显示,不静默无图标)。
const TRAY_ICON_PNG: &[u8] = include_bytes!("../../../assets/TrayIcon.png");
/// 占位图标:圆角方块底色(中蓝:深浅两色菜单栏上都可辨)与放大镜前景(白)。
const ICON_BG: [u8; 3] = [58, 124, 188];
const ICON_FG: [u8; 3] = [255, 255, 255];

/// 托盘动作(泵线程 → UI 帧内消费;与 plan Task 16 枚举逐字一致)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayMsg {
    /// 左键点击 / 菜单「打开搜索」:切换悬浮条显示态。
    TogglePalette,
    /// 菜单「打开完整窗口」:唤起完整窗口并聚焦搜索框。
    OpenResults,
    /// 缺口项:唤起完整窗口并展开缺口面板(product §3)。
    OpenGaps,
    /// 菜单「开机自启」(Task 20:真实注册态;muda 已按点击翻转勾选,成败由
    /// `ui::drain_tray` 经 `autostart::apply_user_choice` 校准)。
    ToggleAutostart,
    /// 菜单「退出」:退 App(引擎常驻,product §5)。
    Quit,
    /// 状态项点击(仅未运行时可点):拉起引擎。
    LaunchEngine,
}

/// 引擎快照(托盘视角;菜单状态项与缺口项的共同数据源)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrayStatus {
    /// daemon 状态串(`client::state::*`);空串 = 尚未取到;`NO_ENGINE` = 未运行且无索引。
    pub state: String,
    /// 权限缺口目录数(status.gaps;离线直读不可得 = 0)。
    pub gaps: u32,
}

/// 状态项文案(SOT:product §7 表「托盘菜单首项」列,逐字)。纯函数 + 单测。
pub fn state_label(state: &str) -> &'static str {
    match state {
        client::state::BUILDING => "引擎:构建中",
        client::state::READY => "引擎:就绪",
        client::state::RECONCILING => "引擎:补齐中",
        client::state::DEGRADED => "引擎:重扫中",
        client::state::OFFLINE => "引擎:未运行(离线索引)",
        // NO_ENGINE(未运行且无索引)、尚未取到(空串)与未知态同按未运行呈现。
        _ => "引擎:未运行",
    }
}

/// 状态项是否可点击(product §3:「未运行」时点击可启动)。已运行态点击无意义 → 菜单项置灰;
/// 未运行/尚未取到 → 可点击(拉起幂等:引擎已在跑时 `ensure_engine` 直接成功)。
pub fn engine_launchable(state: &str) -> bool {
    matches!(state, "" | NO_ENGINE | client::state::OFFLINE)
}

/// 缺口项文案(product §2/§3 逐字)。
pub fn gaps_label(gaps: u32) -> String {
    format!("⚠ {gaps} 个目录未收录")
}

/// 菜单项 id → 动作(纯函数,可测;未知 id = 非本模块菜单,返回 None 不误触发)。
fn dispatch(menu_id: &MenuId) -> Option<TrayMsg> {
    Some(match menu_id.0.as_str() {
        ids::STATE => TrayMsg::LaunchEngine,
        // 缺口项:完整窗口 + 展开缺口面板(product §3;Task 19 接)
        ids::GAPS => TrayMsg::OpenGaps,
        ids::OPEN_SEARCH => TrayMsg::TogglePalette,
        ids::OPEN_WINDOW => TrayMsg::OpenResults,
        ids::AUTOSTART => TrayMsg::ToggleAutostart,
        ids::QUIT => TrayMsg::Quit,
        _ => return None,
    })
}

/// 托盘运行时(挂 `ui::App` 状态;drop 即移除菜单栏图标)。
pub struct TrayRuntime {
    /// 图标保活(最后一份 drop → 图标消失;勿丢)。
    _tray: TrayIcon,
    /// 菜单句柄(缺口项动态增删;与图标持有的是同一份 NSMenu——muda `Menu` 为句柄克隆)。
    menu: Menu,
    /// 状态项(六态文本 + 未运行时可点)。
    state_item: MenuItem,
    /// 缺口项(有缺口才挂;归零即移除)。
    gaps_item: Option<MenuItem>,
    /// 「开机自启」勾选项(勾选态 = 真实注册态;Task 20 起,操作成败在
    /// `ui::drain_tray` 里校准——muda 只按点击翻转,不反映注册结果)。
    autostart: CheckMenuItem,
    /// 动作消息(泵线程 → 帧内 `poll`)。
    rx: Receiver<TrayMsg>,
    /// 状态刷新请求(悬停/点击/拉起后;非阻塞发,刷新线程阻塞收)。
    refresh_tx: Sender<()>,
    /// 最新引擎快照(刷新线程写;主线程 `sync` 读)。
    snapshot: Arc<Mutex<TrayStatus>>,
    /// 已落到菜单的快照(变化才动菜单项,免每帧写 NSAttributedTitle 面)。
    applied: TrayStatus,
}

impl TrayRuntime {
    /// 帧内消费一条动作消息(无则 None;泵线程负责叫帧)。
    pub fn poll(&self) -> Option<TrayMsg> {
        self.rx.try_recv().ok()
    }

    /// 刷新请求通道的克隆(交工作线程:拉起引擎完成后请求校准状态项;
    /// 悬停/点击触发的刷新在泵线程内直接发,不经过这里)。
    pub fn refresh_handle(&self) -> Sender<()> {
        self.refresh_tx.clone()
    }

    /// 「开机自启」当前勾选态(菜单项即勾选真源:安装时按真实注册态初始化,
    /// 点击由 muda 翻转,失败时经 [`Self::set_autostart_checked`] 复原)。
    pub fn autostart_checked(&self) -> bool {
        self.autostart.is_checked()
    }

    /// 校准勾选态(注册/注销失败时复原;muda 自身只按点击翻转,不反映调用成败)。
    pub fn set_autostart_checked(&self, checked: bool) {
        self.autostart.set_checked(checked);
    }

    /// 主线程每帧调用:快照有变才动菜单项(状态文本/可点击性、缺口项增删)。
    pub fn sync(&mut self) {
        let snapshot = match self.snapshot.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        if snapshot.state != self.applied.state {
            let label = state_label(&snapshot.state);
            self.state_item.set_text(label);
            self.state_item
                .set_enabled(engine_launchable(&snapshot.state));
            log::info!("托盘状态项:{label}");
            self.applied.state = snapshot.state;
        }
        if snapshot.gaps != self.applied.gaps {
            self.apply_gaps(snapshot.gaps);
            self.applied.gaps = snapshot.gaps;
        }
    }

    /// 缺口项:归零 → 移除;>0 → 挂到状态项下方(位置 1)并更新计数。
    fn apply_gaps(&mut self, gaps: u32) {
        if gaps == 0 {
            if let Some(item) = self.gaps_item.take() {
                if let Err(err) = self.menu.remove(&item) {
                    log::warn!("托盘缺口项移除失败: {err}");
                }
            }
            return;
        }
        match &self.gaps_item {
            Some(item) => item.set_text(gaps_label(gaps)),
            None => {
                let item = MenuItem::with_id(ids::GAPS, gaps_label(gaps), true, None);
                match self.menu.insert(&item, 1) {
                    Ok(()) => {
                        log::info!("托盘缺口项挂载:{}", gaps_label(gaps));
                        self.gaps_item = Some(item);
                    }
                    Err(err) => log::warn!("托盘缺口项挂载失败: {err}"),
                }
            }
        }
    }
}

/// 安装托盘(主线程调用;须在 winit 事件循环已跑之后——见模块注释「主线程约束」)。
/// 失败返回 Err(调用方决定降级;本模块不 panic)。
pub fn install(ctx: &egui::Context, paths: DaemonPaths) -> anyhow::Result<TrayRuntime> {
    // —— 菜单(主线程;muda 硬约束) ——
    let menu = Menu::new();
    // 状态项:初始按「未运行」占位(首次刷新即校准);可点击 = 未运行态。
    let state_item = MenuItem::with_id(ids::STATE, state_label(""), true, None);
    let open_search = MenuItem::with_id(ids::OPEN_SEARCH, "打开搜索", true, None);
    let open_window = MenuItem::with_id(ids::OPEN_WINDOW, "打开完整窗口", true, None);
    // Task 20:勾选态 = 真实注册态(SMAppService):勾选 = 登录时会自动启动
    // (= status Enabled;requiresApproval 视为未启用——用户尚未在系统设置放行)。
    // 启动默认注册已在 main 里跑过(产品 §4「首次启动 App 时注册自启」),此处查询
    // 即反映注册结果;开发形态(未打包)查询失败 → 未勾选呈现,点击时经
    // apply_user_choice 报「未打包」并复原。
    let autostart_checked = crate::autostart::is_enabled().unwrap_or_else(|err| {
        log::debug!("托盘:开机自启状态不可用(按未勾选呈现): {err:#}");
        false
    });
    let autostart =
        CheckMenuItem::with_id(ids::AUTOSTART, "开机自启", true, autostart_checked, None);
    let quit = MenuItem::with_id(ids::QUIT, "退出", true, None);
    let separator = PredefinedMenuItem::separator();
    menu.append_items(&[
        &state_item,
        &separator,
        &open_search,
        &open_window,
        &autostart,
        &quit,
    ])
    .map_err(|e| anyhow::anyhow!("托盘菜单构建失败: {e}"))?;

    let tray = TrayIconBuilder::new()
        .with_icon(icon(ICON_SIZE)?)
        .with_tooltip("macEverything")
        // 左键留给「唤起/隐藏切换」(product §3);右键菜单**自弹**(见模块注释
        // 「右键菜单自弹」)——两个内建弹出开关都关,托盘库不再碰菜单。
        .with_menu_on_left_click(false)
        .with_menu_on_right_click(false)
        .build()
        .map_err(|e| anyhow::anyhow!("托盘图标创建失败: {e}"))?;
    // 菜单句柄交平台层(自弹按光标所在屏定位;裸指针在主线程注册一次)。
    platform::register_tray_menu(menu.ns_menu());

    // —— 事件泵与状态刷新(见模块注释) ——
    let snapshot = Arc::new(Mutex::new(TrayStatus::default()));
    let (msg_tx, msg_rx) = mpsc::channel::<TrayMsg>();
    let (refresh_tx, refresh_rx) = mpsc::channel::<()>();
    spawn_tray_pump(ctx.clone(), msg_tx.clone(), refresh_tx.clone());
    spawn_menu_pump(ctx.clone(), msg_tx);
    spawn_state_refresher(ctx.clone(), paths, Arc::clone(&snapshot), refresh_rx);
    // 首次拉取(线程已起,不会丢在 channel 外)。
    if let Err(err) = refresh_tx.send(()) {
        log::warn!("托盘首次状态刷新请求失败:{err}");
    }

    // 注:此处不读 `tray.rect()`——状态项窗的布局在下一轮 run loop 才定,创建期读到的
    // 是占位值(实机实测 (0,屏幕下缘) 76x0),写进日志反而误导。
    log::info!("菜单栏托盘已安装(左键切换悬浮条;右键菜单)");
    Ok(TrayRuntime {
        _tray: tray,
        menu,
        state_item,
        gaps_item: None,
        autostart,
        rx: msg_rx,
        refresh_tx,
        snapshot,
        applied: TrayStatus::default(),
    })
}

/// 发动作消息;失败(UI 已退出)= 进程收尾中,留痕并返回 false(不叫帧)。
fn send_msg(tx: &Sender<TrayMsg>, msg: TrayMsg) -> bool {
    if let Err(err) = tx.send(msg) {
        log::warn!("托盘消息发送失败:{err}");
        return false;
    }
    true
}

/// 托盘图标事件泵:左键抬起 → 切换悬浮条;悬停 → 顺带刷新引擎状态(见模块注释)。
fn spawn_tray_pump(ctx: egui::Context, tx: Sender<TrayMsg>, refresh: Sender<()>) {
    let spawned = std::thread::Builder::new()
        .name("tray-pump".to_string())
        .spawn(move || {
            let rx = TrayIconEvent::receiver();
            while let Ok(event) = rx.recv() {
                let acted = match event {
                    // 左键抬起 = 唤起/隐藏切换(product §3)。menu_on_left_click(false) 下
                    // 左键不弹菜单(mouseDown 只高亮),此处即左键的完整语义。
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } => {
                        log::info!("托盘左键:切换悬浮条");
                        send_msg(&tx, TrayMsg::TogglePalette)
                    }
                    // 右键按下 = 弹菜单(自弹;见模块注释「右键菜单自弹」)。
                    // 主队列直调(与热键同模式):菜单须主线程弹,且不等帧——
                    // 不可见窗口的跨线程 repaint 请求被 eframe 节流 ≥100ms。
                    TrayIconEvent::Click {
                        button: MouseButton::Right,
                        button_state: MouseButtonState::Down,
                        ..
                    } => {
                        dispatch2::DispatchQueue::main().exec_async(platform::popup_tray_menu);
                        false // 不叫帧:菜单是原生窗,egui 无需重绘
                    }
                    // 悬停 → 顺带刷新引擎状态(菜单弹出前状态项已是最新;弹出路径见
                    // 模块注释「右键菜单自弹」,不经托盘库)。
                    TrayIconEvent::Enter { .. } => {
                        if let Err(err) = refresh.send(()) {
                            log::warn!("托盘状态刷新请求失败:{err}");
                            false
                        } else {
                            true
                        }
                    }
                    // Move/Leave/按下/右键等无动作:不叫帧(悬停移动不发帧,守零帧红线)。
                    _ => false,
                };
                if acted {
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
            }
        });
    if let Err(err) = spawned {
        log::error!("托盘事件泵线程启动失败(左键切换将无响应):{err}");
    }
}

/// 菜单事件泵:菜单项 id → 动作(未知 id 忽略不误触发)。
fn spawn_menu_pump(ctx: egui::Context, tx: Sender<TrayMsg>) {
    let spawned = std::thread::Builder::new()
        .name("tray-menu-pump".to_string())
        .spawn(move || {
            let rx = MenuEvent::receiver();
            while let Ok(event) = rx.recv() {
                let Some(msg) = dispatch(&event.id) else {
                    log::warn!("托盘菜单未知 id({}):忽略", event.id.0);
                    continue;
                };
                if msg == TrayMsg::OpenGaps {
                    log::info!("托盘缺口项点击:打开完整窗口并展开缺口面板");
                }
                if send_msg(&tx, msg) {
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
            }
        });
    if let Err(err) = spawned {
        log::error!("托盘菜单事件泵线程启动失败(菜单项将无响应):{err}");
    }
}

/// 引擎状态刷新线程:请求驱动(悬停/点击/拉起后),**无轮询**——空闲阻塞在 recv,
/// 守「隐藏态 CPU 0%」红线。取到快照即写槽(失败保留上次快照)+ 叫 ROOT 帧。
fn spawn_state_refresher(
    ctx: egui::Context,
    paths: DaemonPaths,
    snapshot: Arc<Mutex<TrayStatus>>,
    rx: Receiver<()>,
) {
    let spawned = std::thread::Builder::new()
        .name("tray-status".to_string())
        .spawn(move || {
            while rx.recv().is_ok() {
                // 已知边界(实机实测):引擎刚拉起时本次刷新可能赶在首扫完成前,
                // 缺口数读到 0(扫描根校验随全量扫描产出)——下一次刷新(悬停/点击)校准;
                // 缺口项非实时数据(product §3 只要求可见与可点),不为此加轮询。
                let status = match client::status(&paths) {
                    Ok(StatusOutcome::Reported(report)) => TrayStatus {
                        state: report.state,
                        gaps: report.gaps.len() as u32,
                    },
                    Ok(StatusOutcome::NoIndex) => TrayStatus {
                        state: NO_ENGINE.to_string(),
                        gaps: 0,
                    },
                    Err(err) => {
                        log::warn!("托盘状态刷新失败(保留上次快照):{err}");
                        continue;
                    }
                };
                match snapshot.lock() {
                    Ok(mut slot) => *slot = status,
                    Err(poisoned) => *poisoned.into_inner() = status,
                }
                ctx.request_repaint_of(egui::ViewportId::ROOT);
            }
        });
    if let Err(err) = spawned {
        log::error!("托盘状态刷新线程启动失败(状态项将不刷新):{err}");
    }
}

/// 托盘图标:优先用内置资源(`assets/TrayIcon.png`,与应用图标同源);
/// 资源解码失败时回落到代码生成的占位图标,保底可见。
fn icon(size: u32) -> anyhow::Result<Icon> {
    if let Some(icon) = asset_icon(size) {
        return Ok(icon);
    }
    log::warn!("托盘图标资源解码失败,回落内置占位图标");
    Icon::from_rgba(icon_rgba(size), size, size)
        .map_err(|e| anyhow::anyhow!("托盘图标数据非法(内部构造,不应发生): {e}"))
}

/// 解码内置菜单栏图标并缩放到 `size`;失败返回 None(调用方回落占位图标)。
fn asset_icon(size: u32) -> Option<Icon> {
    let decoded = image::load_from_memory(TRAY_ICON_PNG).ok()?.to_rgba8();
    let rgba = if decoded.width() == size && decoded.height() == size {
        decoded
    } else {
        image::imageops::resize(&decoded, size, size, image::imageops::FilterType::Lanczos3)
    };
    Icon::from_rgba(rgba.into_raw(), size, size).ok()
}

/// 生成 RGBA 位图:圆角方块底 + 白色放大镜(镜圈 + 手柄)。
/// 每像素 2x2 超采样取均值(边缘抗锯齿;菜单栏缩到 22pt 仍平滑)。
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let s = size as f64;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let mut acc = [0u32; 4];
            for (dx, dy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                let sample = sample(s, x as f64 + dx, y as f64 + dy);
                for (a, v) in acc.iter_mut().zip(sample) {
                    *a += v as u32;
                }
            }
            for v in acc {
                out.push((v / 4) as u8);
            }
        }
    }
    out
}

/// 单样点颜色(未预乘);(x, y) 在像素坐标域 [0, size)。
fn sample(s: f64, x: f64, y: f64) -> [u8; 4] {
    if !in_rounded_square(s, x, y) {
        return [0, 0, 0, 0];
    }
    if in_magnifier(s, x, y) {
        return [ICON_FG[0], ICON_FG[1], ICON_FG[2], 255];
    }
    [ICON_BG[0], ICON_BG[1], ICON_BG[2], 255]
}

/// 圆角方块(圆角半径 22% 边长)。
fn in_rounded_square(s: f64, x: f64, y: f64) -> bool {
    let r = s * 0.22;
    let cx = x.clamp(r, s - r);
    let cy = y.clamp(r, s - r);
    let (dx, dy) = (x - cx, y - cy);
    dx * dx + dy * dy <= r * r
}

/// 放大镜:镜圈(圆心 40%,半径 19%,线宽 7.5%)+ 45° 手柄(到 78% 处,半宽 4.5%)。
fn in_magnifier(s: f64, x: f64, y: f64) -> bool {
    let (cx, cy) = (0.40 * s, 0.40 * s);
    let ring_r = 0.19 * s;
    let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
    if (d - ring_r).abs() <= 0.0375 * s {
        return true;
    }
    let t = std::f64::consts::FRAC_1_SQRT_2;
    let p0 = (cx + ring_r * t, cy + ring_r * t);
    let p1 = (0.78 * s, 0.78 * s);
    dist_to_segment(x, y, p0, p1) <= 0.045 * s
}

/// 点到线段距离(手柄圆头线段的距离场)。
fn dist_to_segment(x: f64, y: f64, p0: (f64, f64), p1: (f64, f64)) -> f64 {
    let (vx, vy) = (p1.0 - p0.0, p1.1 - p0.1);
    let len2 = vx * vx + vy * vy;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (((x - p0.0) * vx + (y - p0.1) * vy) / len2).clamp(0.0, 1.0)
    };
    let (qx, qy) = (p0.0 + t * vx, p0.1 + t * vy);
    ((x - qx).powi(2) + (y - qy).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 状态项文案与 product §7 表「托盘菜单首项」列逐字一致(含「未运行」无索引与未知态)。
    #[test]
    fn state_label_maps_six_states() {
        assert_eq!(state_label(client::state::BUILDING), "引擎:构建中");
        assert_eq!(state_label(client::state::READY), "引擎:就绪");
        assert_eq!(state_label(client::state::RECONCILING), "引擎:补齐中");
        assert_eq!(state_label(client::state::DEGRADED), "引擎:重扫中");
        assert_eq!(state_label(client::state::OFFLINE), "引擎:未运行(离线索引)");
        assert_eq!(state_label(NO_ENGINE), "引擎:未运行", "未运行且无落盘索引");
        assert_eq!(state_label(""), "引擎:未运行", "尚未取到状态");
        assert_eq!(state_label("未来的新态"), "引擎:未运行", "未知态兜底");
    }

    /// 可点击性:仅未运行/未知可点(已运行态点击无意义 → 置灰)。
    #[test]
    fn engine_launchable_only_when_not_running() {
        assert!(engine_launchable(client::state::OFFLINE));
        assert!(engine_launchable(NO_ENGINE));
        assert!(engine_launchable(""), "尚未取到:可点(拉起幂等)");
        for state in [
            client::state::BUILDING,
            client::state::READY,
            client::state::RECONCILING,
            client::state::DEGRADED,
        ] {
            assert!(!engine_launchable(state), "{state} 运行中不可点");
        }
    }

    /// 缺口项文案(product §2/§3 逐字「⚠ N 个目录未收录」)。
    #[test]
    fn gaps_label_matches_product_copy() {
        assert_eq!(gaps_label(3), "⚠ 3 个目录未收录");
        assert_eq!(gaps_label(1), "⚠ 1 个目录未收录");
    }

    /// 菜单项 id 分派:六项各就其位;未知 id 不误触发。
    #[test]
    fn dispatch_maps_menu_ids() {
        let cases = [
            (ids::STATE, TrayMsg::LaunchEngine),
            (ids::GAPS, TrayMsg::OpenGaps),
            (ids::OPEN_SEARCH, TrayMsg::TogglePalette),
            (ids::OPEN_WINDOW, TrayMsg::OpenResults),
            (ids::AUTOSTART, TrayMsg::ToggleAutostart),
            (ids::QUIT, TrayMsg::Quit),
        ];
        for (id, expected) in cases {
            assert_eq!(
                dispatch(&MenuId::new(id)),
                Some(expected),
                "id {id} 分派错误"
            );
        }
        assert_eq!(dispatch(&MenuId::new("other")), None, "未知 id 忽略");
    }

    /// 图标位图:尺寸、圆角透明、底色与放大镜(镜圈/手柄)各自命中。
    /// 菜单栏图标资源必须可解码且为 ICON_SIZE 见方(改素材/尺寸即红)。
    #[test]
    fn tray_icon_asset_decodes_at_expected_size() {
        let img = image::load_from_memory(super::TRAY_ICON_PNG).expect("内置托盘图标应可解码");
        assert_eq!(
            (img.width(), img.height()),
            (super::ICON_SIZE, super::ICON_SIZE),
            "托盘图标资源尺寸须与 ICON_SIZE 一致"
        );
    }

    #[test]
    fn icon_bitmap_shape_and_colors() {
        const S: u32 = 64;
        let buf = icon_rgba(S);
        assert_eq!(buf.len(), (S * S * 4) as usize, "RGBA 长度 = 宽*高*4");
        assert_eq!(px(&buf, S, 0, 0), [0, 0, 0, 0], "左上圆角外透明");
        assert_eq!(px(&buf, S, S - 1, S - 1), [0, 0, 0, 0], "右下圆角外透明");
        let bg = px(&buf, S, 16, 48);
        assert_eq!(bg[3], 255, "方块内不透明");
        assert_eq!(&bg[..3], &ICON_BG[..], "放大镜外 = 底色");
        assert_eq!(px(&buf, S, 37, 25), [255, 255, 255, 255], "镜圈像素 = 白");
        assert_eq!(px(&buf, S, 44, 44), [255, 255, 255, 255], "手柄像素 = 白");
    }

    fn px(buf: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * size + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }
}
