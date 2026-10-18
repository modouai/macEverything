//! 设置窗口（v2.1）：620×600 固定、原生交通灯 + 自绘 44 标题栏。
//!
//! 第三个 deferred 视口（与悬浮条同模式：隐藏走 `Visible(false)`，窗口保留不销毁）。
//! 本文件负责视口/运行期状态/行为编排：`draw` 短暂持锁取快照 → 锁外绘制
//! （`ui/settings_view.rs` 纯视图）→ 锁外落地字号/自启动作（保存与 SMAppService
//! 调用不持共享锁）；快捷键动作只入共享态命令队列，由主线程 `App::logic`
//! 调 `HotkeyRuntime`（Carbon manager 只能主线程）。标题栏固定，内容区整页纵向滚动。
//!
//! 窗口交互：标题栏可拖拽（左侧预留原生交通灯）、红点/⌘W = 隐藏（不退出 App）。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui;
use global_hotkey::hotkey::HotKey;

use crate::autostart::{self, Status};
use crate::hotkey::{HotkeySlot, HotkeySnapshot};
use crate::platform;
use crate::settings::{self as store, FontSize, SettingsFile};
use crate::ui::{
    font as ui_font, install_base_text_styles, lock_shared,
    settings_tokens::{
        palette as settings_palette, SETTINGS_H, SETTINGS_W, TITLEBAR_H, TITLE_FONT,
        TRAFFIC_LIGHTS_RESERVE, WINDOW_RADIUS, WINDOW_STROKE,
    },
    settings_view::{self, SettingsAction, SettingsData},
    theme, Shared,
};

/// 设置视口 id（注册与命令定向同一处，勿散落）。
pub fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("settings")
}

/// 设置视口 builder（常量；每 pass 传同一份——`patch` 无差异则无命令/无重建）。
///
/// min/max 同时锁死尺寸（`resizable(false)` 之外的双保险：builder 尺寸即规格）。
/// `visible(false)`：启动隐藏；打开走 `open_settings` 的原生居中/置前，
/// egui `Visible(true)` + `Focus` 仅为原生窗口未就绪时的退回路径。
/// 标题与标题栏关闭 + 全尺寸内容视图 = 原生交通灯浮在自绘 44px 标题栏上。
pub fn builder() -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_title(platform::SETTINGS_TITLE)
        .with_inner_size([SETTINGS_W, SETTINGS_H])
        .with_min_inner_size([SETTINGS_W, SETTINGS_H])
        .with_max_inner_size([SETTINGS_W, SETTINGS_H])
        .with_resizable(false)
        .with_title_shown(false)
        .with_titlebar_shown(false)
        .with_fullsize_content_view(true)
        .with_has_shadow(true)
        .with_visible(false)
}

/// 设置页 → App 的快捷键命令（录制 / 取消 / 提交）。
///
/// 设置 pass 只入队；注册/注销必须在主线程由 `App::logic` 调
/// `HotkeyRuntime`（Carbon manager 约束），因此命令跨帧存放在共享态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HotkeyCommand {
    /// 点击键帽字段进入录制（App 侧挂起全局热键）。
    BeginRecord(HotkeySlot),
    /// 取消录制（App 侧恢复旧键）。
    CancelRecord,
    /// 提交录制结果（App 侧校验/注册/落盘；失败退出录制并回滚旧键）。
    Commit { slot: HotkeySlot, hotkey: HotKey },
}

/// 设置窗口运行期真源：开合 + 设置快照 + 自启/字号错误 + 渲染数据。
///
/// 本结构持有运行期数据（不每帧重建 `engine::config::default_config()`）；
/// `SettingsData` 只是出锁绘制用的复制快照。排除目录先种引擎默认值，
/// 真实 daemon 列表由 Task 8 接入。
pub struct SettingsState {
    pub(crate) open: bool,
    /// 已加载的 `settings.toml` 快照（字号/快捷键写入在此更新）。
    pub(crate) file: SettingsFile,
    /// 设置文件所在数据目录（保存字号/快捷键用）。
    pub(crate) data_dir: PathBuf,
    /// 实际自启态（仅 `Status::Enabled` 为 true）；`None` = 尚未查询或查询失败。
    pub(crate) autostart: Option<bool>,
    /// 已注册但等待系统「登录项」允许（`RequiresApproval`）：显示关态 + 行内提示。
    pub(crate) autostart_approval: bool,
    pub(crate) autostart_error: Option<String>,
    /// 字号保存失败的内联错误（已应用的字号不因保存失败回退）。
    pub(crate) font_error: Option<String>,
    /// 排除目录（Task 8 前为引擎默认值，仅种一次）。
    pub(crate) excluded: Vec<String>,
    /// 全局热键运行时快照（H2 启动种子；键帽展示/查重的唯一真源）。
    pub(crate) hotkey_snapshot: HotkeySnapshot,
    /// 当前录制槽（None = 未录制）；录制期全局热键已挂起。
    pub(crate) recording: Option<HotkeySlot>,
    /// App 已完成 suspend、可以开始接收按键（准备期显示「准备录制…」）。
    pub(crate) recording_ready: bool,
    /// 最近一条快捷键内联错误（按槽定位：校验/冲突/注册失败/保存失败）。
    pub(crate) hotkey_error: Option<(HotkeySlot, String)>,
    /// 待 `App::logic` 消费的快捷键命令（FIFO；设置 pass 不碰运行时）。
    pub(crate) hotkey_commands: VecDeque<HotkeyCommand>,
    /// 设置页切换自启成功后待同步给托盘的值（App::logic 消费）。
    pub(crate) tray_autostart_sync: Option<bool>,
    /// 首次打开已完成定位决策：之后重开只置前，保留用户拖动后的位置。
    pub(crate) positioned_once: bool,
    /// 关闭/⌘W 后待消费的「取消录制」信号；`App::logic` 取走并在录制中恢复旧键。
    pub(crate) cancel_recording_pending: bool,
}

impl SettingsState {
    /// 启动/打开前的初始状态：排除目录按引擎默认种一次；热键快照由 `App::new`
    /// 从 `HotkeyRuntime::snapshot()` 种入（注册成功后的真实键位/提示）。
    pub(crate) fn new(
        file: SettingsFile,
        data_dir: PathBuf,
        hotkey_snapshot: HotkeySnapshot,
    ) -> Self {
        Self {
            open: false,
            file,
            data_dir,
            autostart: None,
            autostart_approval: false,
            autostart_error: None,
            font_error: None,
            excluded: engine::config::default_config().excluded,
            hotkey_snapshot,
            recording: None,
            recording_ready: false,
            hotkey_error: None,
            hotkey_commands: VecDeque::new(),
            tray_autostart_sync: None,
            positioned_once: false,
            cancel_recording_pending: false,
        }
    }

    /// 标记打开（纯状态）：返回 true = 本次为首次打开，调用方应做原生居中。
    ///
    /// 首次打开即记账（`positioned_once`）：即使原生窗口未就绪、退回 egui 默认
    /// 位置，后续重开也只置前——不把用户拖动过的窗口再拉回中央。
    pub(crate) fn request_open(&mut self) -> bool {
        let was_open = self.open;
        self.open = true;
        let first_open = !was_open && !self.positioned_once;
        if first_open {
            self.positioned_once = true;
        }
        first_open
    }

    /// 标记隐藏（红点 / ⌘W 共用，纯状态）：清开合态并挂起录制取消。
    pub(crate) fn mark_hidden(&mut self) {
        self.open = false;
        self.cancel_recording_pending = true;
    }

    /// 取走录制取消挂起（`App::logic` 消费；录制中由它恢复旧热键）。
    pub(crate) fn take_cancel_recording_pending(&mut self) -> bool {
        std::mem::take(&mut self.cancel_recording_pending)
    }

    /// 入队一条快捷键命令（设置 pass；纯状态，运行时调用仍在 App）。
    ///
    /// 切换槽位录制时补一条 `CancelRecord`：App 先恢复旧键再重新挂起，
    /// 保证「先 resume 再 suspend」的明确语义（幂等，不会丢键）。
    /// 已在录制目标槽时忽略重复 Begin（视图对同槽点击发的是 Cancel）。
    pub(crate) fn queue_hotkey_command(&mut self, command: HotkeyCommand) {
        match command {
            HotkeyCommand::BeginRecord(slot) => {
                if self.recording == Some(slot) {
                    return;
                }
                if self.recording.is_some() {
                    self.hotkey_commands.push_back(HotkeyCommand::CancelRecord);
                }
                self.hotkey_commands.push_back(command);
            }
            other => self.hotkey_commands.push_back(other),
        }
    }

    /// 取走全部待处理命令（App::logic 在共享锁外逐条执行；FIFO）。
    pub(crate) fn take_hotkey_commands(&mut self) -> Vec<HotkeyCommand> {
        self.hotkey_commands.drain(..).collect()
    }

    /// App `suspend()` 完成后进入录制：切槽、可接收按键、清旧错误。
    pub(crate) fn begin_recording(&mut self, slot: HotkeySlot, snapshot: HotkeySnapshot) {
        self.recording = Some(slot);
        self.recording_ready = true;
        self.hotkey_error = None;
        self.hotkey_snapshot = snapshot;
    }

    /// 取消录制/恢复旧键完成后落地：清录制态与错误，刷新快照。
    pub(crate) fn end_recording(&mut self, snapshot: HotkeySnapshot) {
        self.recording = None;
        self.recording_ready = false;
        self.hotkey_error = None;
        self.hotkey_snapshot = snapshot;
    }

    /// 提交成功（运行时已注册新键）后落地：结束录制、写设置文件快照并返回待落盘副本。
    ///
    /// 保存由调用方在共享锁外执行；失败只回填内联错误，不回退已注册的键。
    pub(crate) fn commit_recorded_hotkey(
        &mut self,
        slot: HotkeySlot,
        hotkey: HotKey,
        snapshot: HotkeySnapshot,
    ) -> SettingsFile {
        self.end_recording(snapshot);
        match slot {
            HotkeySlot::Palette => self.file.palette_hotkey = Some(hotkey),
            HotkeySlot::Window => self.file.window_hotkey = Some(hotkey),
        }
        self.file.clone()
    }

    /// 提交失败（运行时已回滚旧键并退出挂起）后落地：结束录制 + 按槽内联错误。
    pub(crate) fn recording_failed(
        &mut self,
        slot: HotkeySlot,
        message: String,
        snapshot: HotkeySnapshot,
    ) {
        self.recording = None;
        self.recording_ready = false;
        self.hotkey_error = Some((slot, message));
        self.hotkey_snapshot = snapshot;
    }

    /// 提交成功但设置保存失败：键已生效，仅按槽落内联错误。
    pub(crate) fn hotkey_save_failed(&mut self, slot: HotkeySlot, message: String) {
        self.hotkey_error = Some((slot, message));
    }

    /// 视图侧校验/查重失败：录制继续，仅按槽显示错误（Esc/下次按键可重试）。
    pub(crate) fn hotkey_rejected(&mut self, slot: HotkeySlot, message: String) {
        self.hotkey_error = Some((slot, message));
    }

    /// 打开设置页的自启查询结果：成功按**真实状态**落地（审批中 → 关态 + 提示），
    /// 失败按未知（关态）+ 内联错误。
    pub(crate) fn autostart_loaded(&mut self, result: Result<Status, String>) {
        match result {
            Ok(status) => {
                self.apply_autostart_status(false, status);
            }
            Err(err) => {
                self.autostart = None;
                self.autostart_approval = false;
                self.autostart_error = Some(err);
            }
        }
    }

    /// 设置页切换：按真实状态落地，成功时把**实际启用值**排队给托盘同步
    ///（审批中 / 未生效 → false，避免托盘勾选虚高）。
    pub(crate) fn autostart_toggled(
        &mut self,
        desired: bool,
        result: Result<Status, String>,
    ) -> AutostartUpdate {
        let update = self.apply_autostart_result(desired, result);
        if update.confirmed {
            self.tray_autostart_sync = Some(update.enabled);
        }
        update
    }

    /// 托盘切换：与设置页共用同一策略更新显示；托盘勾选由调用方按返回结果校准
    ///（失败时优先按已加载的实际态；未知则复原点击前值）。
    pub(crate) fn autostart_changed_externally(
        &mut self,
        desired: bool,
        result: Result<Status, String>,
    ) -> AutostartUpdate {
        self.apply_autostart_result(desired, result)
    }

    /// 切换结果落地（纯状态）：仅 `Enabled` 显示开；`RequiresApproval` 显示关 +
    /// 待允许提示；启用后仍 `NotRegistered`/`NotFound` → 关 + 内联错误。
    fn apply_autostart_status(&mut self, desired: bool, status: Status) -> AutostartUpdate {
        let enabled = status == Status::Enabled;
        self.autostart = Some(enabled);
        self.autostart_approval = status == Status::RequiresApproval;
        self.autostart_error = if !enabled && status != Status::RequiresApproval && desired {
            Some(format!("开机自启未生效:{}", status.note()))
        } else {
            None
        };
        AutostartUpdate {
            enabled,
            confirmed: true,
        }
    }

    /// 注册/注销或状态查询失败：保持旧启用显示，清除审批提示、落内联错误。
    fn apply_autostart_result(
        &mut self,
        desired: bool,
        result: Result<Status, String>,
    ) -> AutostartUpdate {
        match result {
            Ok(status) => self.apply_autostart_status(desired, status),
            Err(message) => {
                self.autostart_approval = false;
                self.autostart_error = Some(format!("开机自启设置失败:{message}"));
                AutostartUpdate {
                    enabled: self.autostart.unwrap_or(false),
                    confirmed: false,
                }
            }
        }
    }

    /// 字号选择的状态落地（纯状态；调用方已完成 `font::set_scale` 与保存）。
    /// 保存失败也更新选中态——界面须与已应用的全局倍率一致。
    pub(crate) fn set_font_size_result(&mut self, size: FontSize, save_error: Option<String>) {
        self.file.font_size = size;
        self.font_error = save_error;
    }

    /// 取走待同步给托盘的自启值（App::logic 每帧 drain）。
    /// 本帧托盘也切换了自启 → 丢弃设置页旧值（托盘动作更新，且勾选已按真实态校准）。
    pub(crate) fn take_tray_autostart_sync(&mut self, tray_toggled: bool) -> Option<bool> {
        let queued = self.tray_autostart_sync.take();
        if tray_toggled {
            None
        } else {
            queued
        }
    }
}

/// 自启切换的统一落地结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AutostartUpdate {
    /// 实际启用（仅 `Status::Enabled`）。
    pub(crate) enabled: bool,
    /// 是否拿到确定状态；false = 失败（托盘须复原勾选、不排队同步）。
    pub(crate) confirmed: bool,
}

impl SettingsData {
    /// 从运行期状态复制一帧快照（锁内调用，绘制在锁外）。
    ///
    /// 放在行为模块：视图只依赖 `SettingsData`，构造真源仍在 `SettingsState`。
    pub(crate) fn from_state(state: &SettingsState) -> Self {
        Self {
            font_size: state.file.font_size,
            autostart: state.autostart.unwrap_or(false),
            autostart_approval: state.autostart_approval,
            autostart_error: state.autostart_error.clone(),
            font_error: state.font_error.clone(),
            language: settings_view::LANGUAGE_LABEL,
            palette_keys: state.hotkey_snapshot.palette_keys.clone(),
            window_keys: state.hotkey_snapshot.window_keys.clone(),
            hotkeys: state.hotkey_snapshot.clone(),
            recording: state.recording,
            recording_ready: state.recording_ready,
            hotkey_error: state.hotkey_error.clone(),
            excluded: state.excluded.clone(),
        }
    }
}

/// 设置视口 pass（deferred 回调）。
pub fn draw(ui: &mut egui::Ui, shared: &Arc<Mutex<Shared>>) {
    let ctx = ui.ctx().clone();
    let now = Instant::now();

    // 关闭=隐藏：红点（close_requested）与 ⌘W 同一路径。子视口须自行消费关闭请求
    //（根视口的 ⌘W 处理器不会为它运行）；隐藏不销毁视口，也不退出 App。
    let close_requested = ctx.input(|i| i.viewport().close_requested());
    let cmd_w = consume_close_shortcut(&ctx);
    if close_requested || cmd_w {
        hide(shared, &ctx);
    }

    // 可见期跟随系统外观：根窗与悬浮条都隐藏时，设置 pass 是唯一驱动点。
    // 与 palette/root 同模式——`maybe_apply` 按 CHECK_INTERVAL 节流探测；
    // 应用后本视口立即重绘，并在本 pass 末尾排下一帧（隐藏后不排，守 L4）。
    let (data, open, theme_changed) = {
        let mut s = lock_shared(shared);
        let open = s.settings.open;
        let theme_changed = open && s.theme.maybe_apply(&ctx, now);
        let data = SettingsData::from_state(&s.settings);
        (data, open, theme_changed)
    };
    if theme_changed {
        // `set_theme` 只改偏好；显式排一帧让本视口按新主题出图。
        ctx.request_repaint_of(viewport_id());
    }

    // 整窗底色（标题栏与内容同底；原生交通灯浮于其上）。
    let palette = settings_palette(ui.visuals().dark_mode);
    ui.painter()
        .rect_filled(ui.max_rect(), egui::CornerRadius::ZERO, palette.window_fill);

    // 44px 标题栏：原生交通灯在左，标题居中（13pt 基准 × 全局字号倍率）。
    let bar = egui::Rect::from_min_size(
        ui.max_rect().min,
        egui::vec2(ui.max_rect().width(), TITLEBAR_H),
    );

    // 拖拽区 = 交通灯预留区之后到标题栏右缘（与结果窗 draw_titlebar 同法；
    // 左侧原生交通灯自带命中，不接管其附近）。
    let drag_rect = egui::Rect::from_min_max(
        egui::pos2(bar.left() + TRAFFIC_LIGHTS_RESERVE, bar.top()),
        bar.max,
    );
    let drag = ui.interact(
        drag_rect,
        ui.scope_id().with("settings-titlebar-drag"),
        egui::Sense::click_and_drag(),
    );
    if drag.drag_started() {
        log::info!("设置窗口标题栏拖拽:StartDrag");
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }

    ui.painter().text(
        bar.center(),
        egui::Align2::CENTER_CENTER,
        platform::SETTINGS_TITLE,
        ui_font::id(TITLE_FONT),
        palette.text_primary,
    );

    // 快照已在锁内复制，绘制与动作执行都在锁外（自启/保存/装样式不持共享锁）。
    let actions = settings_view::draw_body(ui, &data).actions;

    // 窗口内 1px 描边（原生窗口负责圆角裁剪与投影；内容层按同半径内侧补线）。
    ui.painter().rect_stroke(
        ui.max_rect(),
        egui::CornerRadius::same(WINDOW_RADIUS),
        egui::Stroke::new(WINDOW_STROKE, palette.window_stroke),
        egui::StrokeKind::Inside,
    );

    for action in actions {
        apply_action(shared, &ctx, action);
    }

    // 开窗期排下一帧（主题跟随节流上限）；隐藏后不再排帧（L4 隐藏零帧）。
    if open {
        ctx.request_repaint_after(theme::CHECK_INTERVAL);
    }
}

/// ⌘W 关闭快捷键（消费；返回 true = 本 pass 应隐藏设置窗）。
/// 红点关闭走 `ViewportInfo::close_requested`，两者在 `draw` 汇合。
fn consume_close_shortcut(ctx: &egui::Context) -> bool {
    ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::W))
}

/// 落地一条设置动作（已出共享锁）。
fn apply_action(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context, action: SettingsAction) {
    match action {
        SettingsAction::SetFontSize(size) => apply_font_size(shared, ctx, size),
        SettingsAction::SetAutostart(enabled) => apply_autostart(shared, ctx, enabled),
        SettingsAction::BeginHotkeyRecord(slot) => {
            enqueue_hotkey_command(shared, ctx, HotkeyCommand::BeginRecord(slot))
        }
        SettingsAction::CancelHotkeyRecord => {
            enqueue_hotkey_command(shared, ctx, HotkeyCommand::CancelRecord)
        }
        SettingsAction::CommitHotkey { slot, hotkey } => {
            enqueue_hotkey_command(shared, ctx, HotkeyCommand::Commit { slot, hotkey })
        }
        SettingsAction::HotkeyError { slot, message } => {
            // 视图侧校验/查重错误只落共享态；录制保持（下次按键可重试）。
            lock_shared(shared).settings.hotkey_rejected(slot, message);
            ctx.request_repaint_of(viewport_id());
        }
    }
}

/// 快捷键命令入队（设置 pass 的唯一动作路径；不调用运行时 manager）。
///
/// 注册/注销只能在主线程 `App::logic` 执行，这里只入队并唤醒根视口消费；
/// 根窗可能隐藏，repaint 有 ≥100ms 节流但设置页操作可接受（design §2.3）。
fn enqueue_hotkey_command(
    shared: &Arc<Mutex<Shared>>,
    ctx: &egui::Context,
    command: HotkeyCommand,
) {
    lock_shared(shared).settings.queue_hotkey_command(command);
    ctx.request_repaint_of(egui::ViewportId::ROOT);
    ctx.request_repaint_of(viewport_id());
}

/// 字号即时生效：全局倍率 → 重装基础样式 → 落盘（失败仅内联留痕）→ 更新状态 → 重绘。
///
/// 不持共享锁调用 `font`/`save`；保存失败时**保留已应用倍率**，选中态也同步更新。
fn apply_font_size(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context, size: FontSize) {
    let (mut file, data_dir) = {
        let s = lock_shared(shared);
        (s.settings.file.clone(), s.settings.data_dir.clone())
    };
    file.font_size = size;

    // 1/2. 运行时倍率与两主题的基础字号档立即生效（两个窗口共用）。
    ui_font::set_scale(size.scale());
    install_base_text_styles(ctx);

    // 3. 原子保存；失败只留内联错误，不回退已应用字号。
    let save_error = store::save(&data_dir, &file).err().map(|err| {
        let message = format!("设置保存失败:{err:#}");
        log::warn!("字号 {size:?} 已应用,但{message}");
        message
    });

    // 4. 设置快照/选中态与运行期倍率一致。
    {
        let mut s = lock_shared(shared);
        s.settings.set_font_size_result(size, save_error);
    }
    log::info!("设置字号 → {:?}(scale {:.2})", size, size.scale());

    // 5. ROOT + 悬浮条 + 设置页全部重绘（两个窗口文字即时变化）。
    ctx.request_repaint_of(egui::ViewportId::ROOT);
    ctx.request_repaint_of(crate::ui::palette_viewport_id());
    ctx.request_repaint_of(viewport_id());
}

/// 自启开关即时生效：SMAppService 调用在锁外；按**操作后真实状态**落地。
///
/// 仅 `Status::Enabled` 显示开；审批中（已注册待允许）显示关 + 行内提示；
/// 启用后仍未注册 → 关 + 内联错误。成功拿到状态时把实际值排队同步托盘。
fn apply_autostart(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context, enabled: bool) {
    let data_dir = {
        let s = lock_shared(shared);
        s.settings.data_dir.clone()
    };
    let result = autostart::apply_user_choice(&data_dir, enabled).map_err(|err| format!("{err:#}"));
    if let Err(err) = &result {
        log::warn!("设置页:开机自启设置失败: {err}");
    }
    let update = {
        let mut s = lock_shared(shared);
        s.settings.autostart_toggled(enabled, result)
    };
    log::info!(
        "设置页:开机自启 → 实际{}",
        if update.enabled {
            "启用"
        } else {
            "未启用"
        }
    );
    ctx.request_repaint_of(viewport_id());
    if update.confirmed {
        // 让 App::logic 尽快 drain `tray_autostart_sync`（根窗可能隐藏）。
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    }
}

/// 隐藏设置窗口（红点关闭 / ⌘W 共用）：开合态置 false + 挂起录制取消 + 下发隐藏命令。
/// 命令只入 egui 队列，不销毁视口——再次打开仍是同一实例。
/// 录制取消挂起由 `App::logic` 消费：录制中恢复旧热键，未录制则无操作。
fn hide(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context) {
    hide_with_command(&mut lock_shared(shared).settings, ctx);
    log::info!("设置窗口隐藏(open=false;录制取消挂起)");
}

/// 隐藏的状态 + egui 命令（纯粘合，headless 测）：开合态/录制取消挂起 +
/// `Visible(false)` + 唤醒根视口消费挂起（录制中恢复旧热键）。
fn hide_with_command(state: &mut SettingsState, ctx: &egui::Context) {
    state.mark_hidden();
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    ctx.request_repaint_of(egui::ViewportId::ROOT);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{FontSize, SettingsFile};
    use global_hotkey::hotkey::{Code, Modifiers};

    fn test_state() -> SettingsState {
        SettingsState::new(
            SettingsFile::default(),
            PathBuf::from("/tmp/mace-everything-settings-test"),
            HotkeySnapshot::default(),
        )
    }

    fn hotkey(mods: Modifiers, code: Code) -> HotKey {
        HotKey::new(Some(mods), code)
    }

    fn alt_space() -> HotKey {
        hotkey(Modifiers::ALT, Code::Space)
    }

    fn alt_shift_f() -> HotKey {
        hotkey(Modifiers::ALT | Modifiers::SHIFT, Code::KeyF)
    }

    fn alt_k() -> HotKey {
        hotkey(Modifiers::ALT, Code::KeyK)
    }

    /// 构造展示键帽与键位一致的快照（避免测试自己拼 ⌥/Space 文案）。
    fn hotkey_snapshot(palette: Option<HotKey>, window: Option<HotKey>) -> HotkeySnapshot {
        HotkeySnapshot {
            palette_keys: palette
                .map(crate::ui::hotkey_keys::display_keys)
                .unwrap_or_default(),
            window_keys: window
                .map(crate::ui::hotkey_keys::display_keys)
                .unwrap_or_default(),
            palette,
            window,
            note: None,
            window_note: None,
        }
    }

    /// 运行期真源只种一次默认值：排除目录 = 引擎默认、默认键帽、自启未知。
    #[test]
    fn state_seeds_runtime_truth() {
        let state = test_state();
        assert!(!state.open);
        assert_eq!(state.file.font_size, FontSize::Standard);
        assert_eq!(state.autostart, None, "打开前未知");
        assert!(!state.autostart_approval);
        assert!(state.autostart_error.is_none());
        assert!(state.font_error.is_none());
        assert_eq!(state.excluded, engine::config::default_config().excluded);
        assert_eq!(state.tray_autostart_sync, None);
        assert_eq!(
            state.hotkey_snapshot,
            HotkeySnapshot::default(),
            "快照由 App::new 植入,构造时为默认空值"
        );
        assert_eq!(state.recording, None);
        assert!(!state.recording_ready);
        assert_eq!(state.hotkey_error, None);
        assert!(state.hotkey_commands.is_empty());
        assert!(!state.positioned_once, "首开前未定位");
        assert!(!state.cancel_recording_pending);
    }

    /// 首次打开居中记账一次；已打开/关闭后重开只置前（保留用户拖动位置）。
    #[test]
    fn first_open_centers_once_then_keeps_user_position() {
        let mut state = test_state();
        assert!(state.request_open(), "首次打开须请求原生居中");
        assert!(state.open);
        assert!(state.positioned_once);
        assert!(!state.request_open(), "窗口已打开时再点入口只置前");

        state.mark_hidden();
        assert!(!state.open);
        assert!(
            !state.request_open(),
            "关闭后重开不得重新居中（保留用户位置）"
        );
        assert!(state.positioned_once);
    }

    /// 关闭/⌘W 挂钩：置取消录制挂起，`App::logic` 取走即清（快捷键 plan 接入点）。
    #[test]
    fn hide_queues_recording_cancel_for_hotkey_plan() {
        let mut state = test_state();
        state.request_open();
        state.mark_hidden();
        assert!(!state.open);
        assert!(state.cancel_recording_pending, "关闭须挂起取消录制");
        assert!(state.take_cancel_recording_pending());
        assert!(!state.cancel_recording_pending, "取走即清");
        assert!(
            !state.take_cancel_recording_pending(),
            "不得跨帧残留重复消费"
        );
    }

    /// 隐藏粘合（headless）：状态置隐藏/挂起录制取消，同时下发 `Visible(false)`。
    #[test]
    fn hide_glue_marks_state_and_emits_visible_false() {
        let ctx = egui::Context::default();
        let mut state = test_state();
        state.request_open();
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(SETTINGS_W, SETTINGS_H),
                )),
                ..Default::default()
            },
            |ui| hide_with_command(&mut state, ui.ctx()),
        );
        assert!(!state.open);
        assert!(state.cancel_recording_pending);
        let cmds = out
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.commands.clone())
            .unwrap_or_default();
        assert!(
            cmds.iter()
                .any(|cmd| matches!(cmd, egui::ViewportCommand::Visible(false))),
            "隐藏须下发 Visible(false)"
        );
        out.textures_delta.clear();
    }

    /// 队列顺序：切槽先补取消；同槽重复 Begin 忽略；Commit/Cancel 保持 FIFO。
    #[test]
    fn hotkey_commands_queue_in_order() {
        let mut state = test_state();

        // 正在录制唤起槽时切到完整窗口槽 → App 先 resume 再 suspend。
        state.recording = Some(HotkeySlot::Palette);
        state.queue_hotkey_command(HotkeyCommand::BeginRecord(HotkeySlot::Window));
        assert_eq!(
            state.take_hotkey_commands(),
            vec![
                HotkeyCommand::CancelRecord,
                HotkeyCommand::BeginRecord(HotkeySlot::Window)
            ],
            "切换录制槽必须先取消旧槽"
        );

        // 已在录制该槽：重复 Begin 忽略（视图对同槽点击发 Cancel）。
        state.recording = Some(HotkeySlot::Window);
        state.queue_hotkey_command(HotkeyCommand::BeginRecord(HotkeySlot::Window));
        assert!(state.hotkey_commands.is_empty(), "同槽重复 Begin 不得入队");

        state.queue_hotkey_command(HotkeyCommand::Commit {
            slot: HotkeySlot::Window,
            hotkey: alt_k(),
        });
        state.queue_hotkey_command(HotkeyCommand::CancelRecord);
        assert_eq!(
            state.take_hotkey_commands(),
            vec![
                HotkeyCommand::Commit {
                    slot: HotkeySlot::Window,
                    hotkey: alt_k()
                },
                HotkeyCommand::CancelRecord
            ],
            "命令须 FIFO 落地"
        );
        assert!(state.hotkey_commands.is_empty(), "取走即清");
    }

    /// 进入录制：清旧错误、置 ready、快照保持运行时真源；结束录制清全部录制态。
    #[test]
    fn begin_and_end_recording_transitions() {
        let mut state = test_state();
        let before = hotkey_snapshot(Some(alt_space()), Some(alt_shift_f()));
        state.hotkey_error = Some((HotkeySlot::Palette, "旧错误".into()));

        state.begin_recording(HotkeySlot::Palette, before.clone());
        assert_eq!(state.recording, Some(HotkeySlot::Palette));
        assert!(state.recording_ready, "App suspend 完成后即可接收按键");
        assert_eq!(state.hotkey_error, None, "新录制清旧错误");
        assert_eq!(state.hotkey_snapshot, before, "挂起期快照仍显示旧键");

        let after = hotkey_snapshot(Some(alt_space()), None);
        state.hotkey_error = Some((HotkeySlot::Window, "提示".into()));
        state.end_recording(after.clone());
        assert_eq!(state.recording, None);
        assert!(!state.recording_ready);
        assert_eq!(state.hotkey_error, None);
        assert_eq!(state.hotkey_snapshot, after);
    }

    /// 提交成功：结束录制、目标槽写入设置文件并原子落盘；另一槽不变。
    #[test]
    fn commit_success_updates_file_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = SettingsState::new(
            SettingsFile::default(),
            dir.path().to_path_buf(),
            hotkey_snapshot(Some(alt_space()), Some(alt_shift_f())),
        );
        state.begin_recording(HotkeySlot::Palette, state.hotkey_snapshot.clone());

        let committed = alt_k();
        let next = hotkey_snapshot(Some(committed), Some(alt_shift_f()));
        let file = state.commit_recorded_hotkey(HotkeySlot::Palette, committed, next.clone());

        assert_eq!(state.recording, None);
        assert!(!state.recording_ready);
        assert_eq!(state.hotkey_error, None);
        assert_eq!(state.hotkey_snapshot, next, "提交成功须刷新展示快照");
        assert_eq!(state.file.palette_hotkey, Some(committed));
        assert_eq!(state.file.window_hotkey, None, "未提交的槽不得被写入默认键");
        assert_eq!(file, state.file, "返回副本供锁外保存");

        crate::settings::save(dir.path(), &file).unwrap();
        let loaded = crate::settings::load(dir.path());
        assert_eq!(loaded.palette_hotkey, Some(committed), "重启后读回");
        assert_eq!(loaded.window_hotkey, None);
    }

    /// 提交失败：运行时已恢复旧键 → 结束录制、按槽落内联错误、文件不变。
    #[test]
    fn commit_failure_ends_recording_and_keeps_old_key() {
        let mut state = test_state();
        let old = hotkey_snapshot(Some(alt_space()), None);
        state.begin_recording(HotkeySlot::Palette, old.clone());

        state.recording_failed(
            HotkeySlot::Palette,
            "注册失败(可能被系统或其他应用占用): 被占用".into(),
            old.clone(),
        );

        assert_eq!(state.recording, None, "失败退出录制（旧键已恢复）");
        assert!(!state.recording_ready);
        assert_eq!(state.hotkey_snapshot, old, "回滚后的快照 = 旧键");
        assert_eq!(
            state.hotkey_error,
            Some((
                HotkeySlot::Palette,
                "注册失败(可能被系统或其他应用占用): 被占用".to_owned()
            ))
        );
        assert_eq!(state.file.palette_hotkey, None, "失败不得落盘新键");
    }

    /// 保存失败：键已注册/展示，仅按槽留内联错误；内存设置文件保留新键。
    #[test]
    fn save_failure_keeps_registered_key_and_reports_inline() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = SettingsState::new(
            SettingsFile::default(),
            dir.path().to_path_buf(),
            hotkey_snapshot(Some(alt_space()), None),
        );
        // 目标路径被目录占用 → rename 失败（与 settings::save 的失败测同法）。
        std::fs::create_dir(crate::settings::path(dir.path())).unwrap();

        state.begin_recording(HotkeySlot::Palette, state.hotkey_snapshot.clone());
        let committed = alt_k();
        let next = hotkey_snapshot(Some(committed), None);
        let file = state.commit_recorded_hotkey(HotkeySlot::Palette, committed, next.clone());
        let err = crate::settings::save(&state.data_dir, &file).unwrap_err();
        state.hotkey_save_failed(
            HotkeySlot::Palette,
            format!("快捷键已生效,但设置保存失败:{err:#}"),
        );

        assert_eq!(
            state.hotkey_snapshot.palette,
            Some(committed),
            "保存失败不得回退已注册键"
        );
        assert_eq!(state.file.palette_hotkey, Some(committed));
        assert_eq!(state.recording, None);
        assert!(
            state.hotkey_error.as_ref().is_some_and(|(slot, message)| {
                *slot == HotkeySlot::Palette && message.contains("设置保存失败")
            }),
            "保存失败须落内联错误:{:?}",
            state.hotkey_error
        );
    }

    /// 视图侧校验/查重错误不结束录制（用户可重按或 Esc）。
    #[test]
    fn rejected_key_keeps_recording_with_inline_error() {
        let mut state = test_state();
        state.begin_recording(HotkeySlot::Palette, HotkeySnapshot::default());
        state.hotkey_rejected(HotkeySlot::Palette, "需包含修饰键".into());
        assert_eq!(state.recording, Some(HotkeySlot::Palette));
        assert!(state.recording_ready);
        assert_eq!(
            state.hotkey_error,
            Some((HotkeySlot::Palette, "需包含修饰键".to_owned()))
        );
    }

    /// 关闭/⌘W：挂起信号在录制中消费 = App 恢复旧键并结束录制；未录制作 no-op。
    #[test]
    fn close_cancel_signal_resolves_recording_only_when_active() {
        let mut state = test_state();
        state.request_open();
        state.recording = Some(HotkeySlot::Window);
        state.recording_ready = true;
        state.hotkey_error = Some((HotkeySlot::Window, "错误".into()));
        state.mark_hidden();

        assert!(!state.open);
        assert!(state.take_cancel_recording_pending(), "关闭须挂起取消");
        assert_eq!(
            state.recording,
            Some(HotkeySlot::Window),
            "状态清理由 App resume 后调用 end_recording"
        );

        let restored = hotkey_snapshot(Some(alt_space()), Some(alt_shift_f()));
        state.end_recording(restored.clone());
        assert_eq!(state.recording, None);
        assert!(!state.recording_ready);
        assert_eq!(state.hotkey_error, None);
        assert_eq!(state.hotkey_snapshot, restored);

        // 未录制：信号仍被取走（不跨帧残留），App 分支为 no-op。
        let mut state = test_state();
        state.mark_hidden();
        assert!(state.take_cancel_recording_pending());
        assert_eq!(state.recording, None);
        assert!(!state.take_cancel_recording_pending(), "不得重复消费");
    }

    /// 字号纯状态：保存失败也更新 file.font_size + 留内联错误；成功清错误。
    /// 持久化单独验证（生产 `set_scale` 不在此测试，避免写全局原子）。
    #[test]
    fn font_size_state_records_error_and_persists_separately() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = SettingsState::new(
            SettingsFile::default(),
            dir.path().to_path_buf(),
            HotkeySnapshot::default(),
        );

        state.set_font_size_result(FontSize::Large, Some("设置保存失败:磁盘只读".into()));
        assert_eq!(
            state.file.font_size,
            FontSize::Large,
            "已应用字号须反映到选中态"
        );
        assert_eq!(state.font_error.as_deref(), Some("设置保存失败:磁盘只读"));
        assert_eq!(
            crate::settings::load(dir.path()).font_size,
            FontSize::Standard,
            "纯状态转换不落盘"
        );

        // 单独的持久化路径（与 UI 无关；不触发全局倍率）。
        crate::settings::save(dir.path(), &state.file).unwrap();
        assert_eq!(crate::settings::load(dir.path()).font_size, FontSize::Large);

        state.set_font_size_result(FontSize::Small, None);
        assert_eq!(state.file.font_size, FontSize::Small);
        assert!(state.font_error.is_none(), "保存成功须清错误");
    }

    /// 打开设置页：查询失败 → 未知（关态）+ 错误；真实状态 → 按状态显示。
    /// `RequiresApproval` 必须关态 + 审批提示，不得显示已启用。
    #[test]
    fn autostart_load_uses_actual_status() {
        let mut state = test_state();
        state.autostart = Some(true);
        state.autostart_loaded(Err("读取开机自启状态失败".into()));
        assert_eq!(state.autostart, None, "查询失败按未知，开关显示关态");
        assert_eq!(
            state.autostart_error.as_deref(),
            Some("读取开机自启状态失败")
        );

        state.autostart_loaded(Ok(Status::Enabled));
        assert_eq!(state.autostart, Some(true));
        assert!(!state.autostart_approval);
        assert!(state.autostart_error.is_none());

        state.autostart_loaded(Ok(Status::RequiresApproval));
        assert_eq!(state.autostart, Some(false), "待审批不得显示已启用");
        assert!(state.autostart_approval);
        assert!(state.autostart_error.is_none(), "审批提示走 note，不是错误");

        state.autostart_loaded(Ok(Status::NotRegistered));
        assert_eq!(state.autostart, Some(false));
        assert!(!state.autostart_approval);
        assert!(state.autostart_error.is_none(), "打开时未注册不是错误");
    }

    /// 切换策略：仅 Enabled 开；RequiresApproval 关 + 提示；启用后未注册关 + 错误。
    /// 托盘同步只排**实际值**；失败保持旧态且不同步。
    #[test]
    fn autostart_toggle_policy_and_tray_sync() {
        let mut state = test_state();
        state.autostart = Some(false);

        let update = state.autostart_toggled(true, Ok(Status::Enabled));
        assert!(update.enabled && update.confirmed);
        assert_eq!(state.autostart, Some(true));
        assert!(!state.autostart_approval);
        assert!(state.autostart_error.is_none());
        assert_eq!(state.take_tray_autostart_sync(false), Some(true));

        let update = state.autostart_toggled(true, Ok(Status::RequiresApproval));
        assert!(!update.enabled && update.confirmed);
        assert_eq!(state.autostart, Some(false), "待审批显示关态");
        assert!(state.autostart_approval);
        assert!(state.autostart_error.is_none());
        assert_eq!(
            state.take_tray_autostart_sync(false),
            Some(false),
            "待审批托盘勾选必须为 OFF"
        );

        let update = state.autostart_toggled(true, Ok(Status::NotFound));
        assert!(!update.enabled && update.confirmed);
        assert_eq!(state.autostart, Some(false));
        assert!(!state.autostart_approval);
        assert!(
            state
                .autostart_error
                .as_deref()
                .is_some_and(|msg| msg.contains("开机自启未生效")),
            "启用后未注册须落内联错误:{:?}",
            state.autostart_error
        );
        assert_eq!(state.take_tray_autostart_sync(false), Some(false));

        state.autostart = Some(true);
        state.autostart_approval = false;
        let update = state.autostart_toggled(false, Err("未打包".into()));
        assert!(!update.confirmed);
        assert_eq!(state.autostart, Some(true), "失败必须保持旧状态");
        assert!(!state.autostart_approval);
        assert!(
            state
                .autostart_error
                .as_deref()
                .is_some_and(|msg| msg.contains("未打包")),
            "失败须落内联错误"
        );
        assert_eq!(
            state.take_tray_autostart_sync(false),
            None,
            "失败不得同步托盘"
        );
    }

    /// 关闭自启成功（操作后 `NotRegistered`）：显示关、无错误，托盘同步 OFF。
    #[test]
    fn autostart_disable_success_lands_off_and_syncs_tray() {
        let mut state = test_state();
        state.autostart = Some(true);
        state.autostart_approval = true;

        let update = state.autostart_toggled(false, Ok(Status::NotRegistered));
        assert!(update.confirmed);
        assert!(!update.enabled);
        assert_eq!(state.autostart, Some(false));
        assert!(!state.autostart_approval);
        assert!(state.autostart_error.is_none(), "正常关闭不是错误");
        assert_eq!(state.take_tray_autostart_sync(false), Some(false));
    }

    /// 托盘侧使用同一策略，但不排队托盘同步（勾选由托盘调用方校准）。
    #[test]
    fn autostart_external_change_shares_policy() {
        let mut state = test_state();
        let update = state.autostart_changed_externally(true, Ok(Status::RequiresApproval));
        assert!(!update.enabled && update.confirmed);
        assert_eq!(state.autostart, Some(false));
        assert!(state.autostart_approval);
        assert_eq!(state.take_tray_autostart_sync(false), None);
    }

    /// 设置→托盘同步与同帧托盘动作的仲裁：托盘动作优先，旧同步值被丢弃。
    #[test]
    fn tray_toggle_drops_queued_settings_sync() {
        let mut state = test_state();
        state.tray_autostart_sync = Some(false);
        assert_eq!(
            state.take_tray_autostart_sync(true),
            None,
            "同帧托盘切换自启须丢弃设置页旧同步"
        );
        assert_eq!(
            state.take_tray_autostart_sync(false),
            None,
            "被丢弃的值不得留下"
        );

        state.tray_autostart_sync = Some(true);
        assert_eq!(state.take_tray_autostart_sync(false), Some(true));
    }

    /// 快照与运行期真源一致（语言/键帽/目录/错误/审批/录制态均复制）。
    #[test]
    fn snapshot_copies_state_for_painting() {
        let mut state = test_state();
        state.file.font_size = FontSize::Large;
        state.autostart = Some(false);
        state.autostart_approval = true;
        state.autostart_error = Some("未打包".into());
        state.font_error = Some("保存失败".into());
        state.hotkey_snapshot.palette_keys = vec!["⌃".into(), "⌥".into(), "K".into()];
        state.hotkey_snapshot.window_keys = vec!["⇧".into(), "⌘".into(), "J".into()];
        state.recording = Some(HotkeySlot::Window);
        state.recording_ready = true;
        state.hotkey_error = Some((HotkeySlot::Palette, "需包含修饰键".into()));
        state.excluded = vec!["node_modules".into()];

        let data = SettingsData::from_state(&state);
        assert_eq!(data.font_size, FontSize::Large);
        assert!(!data.autostart, "审批中显示为关");
        assert!(data.autostart_approval);
        assert_eq!(data.autostart_error.as_deref(), Some("未打包"));
        assert_eq!(data.font_error.as_deref(), Some("保存失败"));
        assert_eq!(data.language, "简体中文");
        assert_eq!(data.palette_keys, vec!["⌃", "⌥", "K"]);
        assert_eq!(data.window_keys, vec!["⇧", "⌘", "J"]);
        assert_eq!(data.hotkeys, state.hotkey_snapshot, "键帽展示真源 = 快照");
        assert_eq!(data.recording, Some(HotkeySlot::Window));
        assert!(data.recording_ready);
        assert_eq!(
            data.hotkey_error,
            Some((HotkeySlot::Palette, "需包含修饰键".to_owned()))
        );
        assert_eq!(data.excluded, vec!["node_modules".to_owned()]);
    }

    fn key_w(modifiers: egui::Modifiers) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(SETTINGS_W, SETTINGS_H),
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

    /// ⌘W 被消费且同帧只消费一次；裸 W 不触发隐藏（设置窗 headless 快捷键路径）。
    #[test]
    fn close_shortcut_consumes_cmd_w_only() {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(key_w(egui::Modifiers::COMMAND), |ui| {
            assert!(consume_close_shortcut(ui.ctx()), "⌘W 应触发隐藏");
            assert!(!consume_close_shortcut(ui.ctx()), "同帧二次消费应为 false");
        });
        out.textures_delta.clear();

        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(key_w(egui::Modifiers::NONE), |ui| {
            assert!(!consume_close_shortcut(ui.ctx()), "裸 W 不得隐藏设置窗");
        });
        out.textures_delta.clear();
    }

    #[test]
    fn window_size_matches_spec() {
        assert_eq!((SETTINGS_W, SETTINGS_H), (620.0, 600.0));
        assert_eq!(TITLEBAR_H, 44.0);
    }

    /// 视口 builder：固定尺寸、启动隐藏、无标题/标题栏 + 全尺寸内容视图
    ///（原生交通灯浮在自绘标题栏上）。
    #[test]
    fn builder_matches_spec() {
        let b = builder();
        assert_eq!(b.title.as_deref(), Some(platform::SETTINGS_TITLE));
        assert_eq!(b.inner_size, Some(egui::vec2(SETTINGS_W, SETTINGS_H)));
        assert_eq!(b.min_inner_size, Some(egui::vec2(SETTINGS_W, SETTINGS_H)));
        assert_eq!(b.max_inner_size, Some(egui::vec2(SETTINGS_W, SETTINGS_H)));
        assert_eq!(b.resizable, Some(false));
        assert_eq!(b.visible, Some(false), "启动隐藏");
        assert_eq!(b.titlebar_shown, Some(false));
        assert_eq!(b.title_shown, Some(false));
        assert_eq!(b.fullsize_content_view, Some(true));
        assert_eq!(b.has_shadow, Some(true));
    }
}
