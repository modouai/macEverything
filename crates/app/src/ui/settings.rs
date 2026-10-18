//! 设置窗口（v2.1）：620×600 固定、原生交通灯 + 自绘 44 标题栏。
//!
//! 第三个 deferred 视口（与悬浮条同模式：隐藏走 `Visible(false)`，窗口保留不销毁）。
//! 本文件负责视口/运行期状态/行为编排：`draw` 短暂持锁取快照 → 锁外绘制
//! （`ui/settings_view.rs` 纯视图）→ 锁外落地字号/自启动作（保存与 SMAppService
//! 调用不持共享锁）；快捷键动作只入共享态命令队列，由主线程 `App::logic`
//! 调 `HotkeyRuntime`（Carbon manager 只能主线程）；排除目录的
//! `client::exclude`（list/add/remove/reset，后三者会触发全量重扫）一律在工作线程，
//! 经单操作 mpsc 回投，设置 pass 非阻塞收取。标题栏固定，内容区整页纵向滚动。
//!
//! 窗口交互：标题栏可拖拽（左侧预留原生交通灯）、红点/⌘W = 隐藏（不退出 App）。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui;
use global_hotkey::hotkey::HotKey;

use crate::autostart::{self, Status};
use crate::hotkey::{HotkeySlot, HotkeySnapshot};
use crate::i18n::{self, LanguageSetting};
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
        .with_title(crate::i18n::s().settings_title)
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

/// 设置页 → App 的快捷键命令（录制 / 取消 / 提交 / 恢复默认）。
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
    /// 恢复该槽内置默认（录制中 Delete/Backspace；App 侧注册默认并落盘）
    Reset(HotkeySlot),
}

/// 设置保存失败（原始细节在构造处进日志，界面显示本地化文案）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SettingsError {
    SaveFailed,
}

impl SettingsError {
    pub(crate) fn text(&self) -> String {
        match self {
            Self::SaveFailed => i18n::s().settings_save_failed.to_owned(),
        }
    }
}

/// 语言保存失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LanguageError {
    SaveFailed,
}

impl LanguageError {
    pub(crate) fn text(&self) -> String {
        match self {
            Self::SaveFailed => i18n::s().language_save_failed.to_owned(),
        }
    }
}

/// 开机自启相关错误（原始细节在构造处进日志）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AutostartError {
    ReadFailed,
    NotEffective(Status),
    SetFailed,
}

impl AutostartError {
    pub(crate) fn text(&self) -> String {
        match self {
            Self::ReadFailed => i18n::s().autostart_read_failed.to_owned(),
            Self::NotEffective(status) => {
                format!(
                    "{}: {}",
                    i18n::s().autostart_not_effective,
                    status_text(*status)
                )
            }
            Self::SetFailed => i18n::s().autostart_set_failed.to_owned(),
        }
    }
}

/// 自启状态说明（设置页行内错误使用；日志仍走 `Status::note()` 中文）。
pub(crate) fn status_text(status: Status) -> &'static str {
    match status {
        Status::Enabled => i18n::s().autostart_status_enabled,
        Status::RequiresApproval => i18n::s().autostart_status_requires_approval,
        Status::NotRegistered => i18n::s().autostart_status_not_registered,
        Status::NotFound => i18n::s().autostart_status_not_found,
    }
}

/// 排除目录错误（daemon 原始错误原样展示，其余本地化）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExcludeError {
    Daemon(String),
    NoReason,
    WorkerExited,
    StartFailed,
}

impl ExcludeError {
    pub(crate) fn text(&self) -> String {
        match self {
            Self::Daemon(detail) => detail.clone(),
            Self::NoReason => i18n::s().exclude_failed_no_reason.to_owned(),
            Self::WorkerExited => i18n::s().exclude_worker_exited.to_owned(),
            Self::StartFailed => i18n::s().exclude_worker_start_failed.to_owned(),
        }
    }
}

/// 排除目录后台操作的一次回投结果（工作线程 → 设置 pass）。
///
/// `error = Some` 时 `entries` 无意义（保持旧列表）；只有 `error = None`
/// 的 daemon 响应才允许覆盖 UI 列表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExcludeResult {
    pub(crate) entries: Vec<String>,
    pub(crate) error: Option<ExcludeError>,
}

/// `client::exclude` 结果 → UI 结果：仅 ok=true 覆盖列表；失败/离线保留旧列表。
fn exclude_result(outcome: anyhow::Result<client::ExcludeOutcome>) -> ExcludeResult {
    match outcome {
        Ok(outcome) if outcome.ok => ExcludeResult {
            entries: outcome.excluded,
            error: None,
        },
        Ok(outcome) => ExcludeResult {
            entries: Vec::new(),
            error: Some(match outcome.message {
                Some(message) => ExcludeError::Daemon(message),
                None => ExcludeError::NoReason,
            }),
        },
        Err(err) => {
            let detail = format!("{err:#}");
            log::warn!("排除目录 daemon 调用失败: {detail}");
            ExcludeResult {
                entries: Vec::new(),
                error: Some(ExcludeError::Daemon(detail)),
            }
        }
    }
}

/// 设置窗口运行期真源：开合 + 设置快照 + 自启/字号错误 + 渲染数据。
///
/// 本结构持有运行期数据（不每帧重建 `engine::config::default_config()`）；
/// `SettingsData` 只是出锁绘制用的复制快照。排除目录以 daemon 列表为唯一真源，
/// 首次**成功** list 响应前先展示引擎默认种子（失败/离线时保留并附错误）；
/// 后台操作同一时刻至多一个。
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
    pub(crate) autostart_error: Option<AutostartError>,
    /// 字号保存失败的内联错误（已应用的字号不因保存失败回退）。
    pub(crate) font_error: Option<SettingsError>,
    /// 语言保存失败的内联错误（语言已即时应用，仅落盘失败时留痕）。
    pub(crate) language_error: Option<LanguageError>,
    /// 语言切换后待 App 刷新的托盘文案标记。
    pub(crate) tray_relabel_pending: bool,
    /// 排除目录（daemon 响应为唯一真源；首次成功 list 响应前为引擎默认种子，
    /// 失败/离线时保留旧列表或种子）。
    pub(crate) excluded: Vec<String>,
    /// 排除目录后台操作进行中（list/add/remove/reset 同一时刻至多一个）。
    pub(crate) exclude_busy: bool,
    /// 在途操作是列表加载（「加载中…」；否则为变更重扫「重扫中…」）。
    pub(crate) exclude_busy_list: bool,
    /// 最近一次排除目录加载/变更失败原因。
    ///
    /// add/remove/reset 开始即清；自动 list 刷新**保留**（否则刚收到的失败文案
    /// 会被重开触发的刷新立刻吞掉），仅成功 list 响应清空。
    pub(crate) exclude_error: Option<ExcludeError>,
    /// 最近一次排除 worker 是否产生了终态结果（含 daemon 列表响应成功/失败，
    /// 以及 client 离线错误）；线程启动失败/无结果退出不算，保持 false。
    pub(crate) exclude_loaded: bool,
    /// 关闭→打开时置位；设置 pass 负责启动一次 list（busy 中则等本次结束）。
    pub(crate) exclude_refresh_pending: bool,
    /// 在途操作的唯一回投通道（one-op；不跨操作复用）。
    pub(crate) exclude_rx: Option<Receiver<ExcludeResult>>,
    /// 全局热键运行时快照（H2 启动种子；键帽展示/查重的唯一真源）。
    pub(crate) hotkey_snapshot: HotkeySnapshot,
    /// 当前录制槽（None = 未录制）；录制期全局热键已挂起。
    pub(crate) recording: Option<HotkeySlot>,
    /// 设置视口最近一次焦点观测（None = 尚未报告）；用于录制中失焦取消。
    pub(crate) viewport_focused: Option<bool>,
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
            language_error: None,
            tray_relabel_pending: false,
            excluded: engine::config::default_config().excluded,
            exclude_busy: false,
            exclude_busy_list: false,
            exclude_error: None,
            exclude_loaded: false,
            exclude_refresh_pending: false,
            exclude_rx: None,
            hotkey_snapshot,
            recording: None,
            viewport_focused: None,
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
        if !was_open {
            // 每次真正打开都重取 daemon 列表（隐藏期可能被 CLI 改动）。
            self.exclude_refresh_pending = true;
        }
        first_open
    }

    /// 标记隐藏（红点 / ⌘W 共用，纯状态）：清开合态/焦点观测并挂起录制取消。
    pub(crate) fn mark_hidden(&mut self) {
        self.open = false;
        self.viewport_focused = None;
        self.cancel_recording_pending = true;
    }

    /// 取走录制取消挂起（`App::logic` 消费；录制中由它恢复旧热键）。
    pub(crate) fn take_cancel_recording_pending(&mut self) -> bool {
        std::mem::take(&mut self.cancel_recording_pending)
    }

    /// 开始一次排除目录后台操作（锁内调用）：返回 false = 已有操作在途。
    ///
    /// `clear_error`：add/remove = true（新操作清旧提示）；自动 list 刷新 = false
    /// （保留已展示的失败文案，直到一次成功 list 才清）。busy 与通道同帧落地，
    /// 避免陈旧结果跨操作串台。
    pub(crate) fn begin_exclude(
        &mut self,
        list: bool,
        clear_error: bool,
        rx: Receiver<ExcludeResult>,
    ) -> bool {
        if self.exclude_busy {
            return false;
        }
        self.exclude_busy = true;
        self.exclude_busy_list = list;
        if clear_error {
            self.exclude_error = None;
        }
        self.exclude_rx = Some(rx);
        true
    }

    /// 设置 pass 的排除目录推进：非阻塞收结果，返回 true = 锁外应启动一次 list。
    ///
    /// 不在这里清 `exclude_refresh_pending`：spawn 流程接管（含线程启动失败的
    /// 终态处理）后才由调用方清除；busy 拒绝时挂起位保留待下次推进。
    pub(crate) fn advance_exclude(&mut self, open: bool) -> bool {
        self.tick_exclude();
        open && self.exclude_refresh_pending && !self.exclude_busy
    }

    /// 工作线程无法启动：清 busy/通道并落内联错误（旧列表保留）。
    ///
    /// 未拿到 daemon 响应，`exclude_loaded` 保持 false（种子语义不变）。
    pub(crate) fn exclude_spawn_failed(&mut self, detail: String) {
        log::warn!("排除目录工作线程启动失败:{detail}");
        self.exclude_busy = false;
        self.exclude_busy_list = false;
        self.exclude_rx = None;
        self.exclude_error = Some(ExcludeError::StartFailed);
        self.exclude_loaded = false;
    }

    /// 非阻塞收取后台结果（设置 pass 锁内调用；空 = 保持 busy 等下一帧）。
    ///
    /// 成功（daemon ok=true）覆盖列表并清错误；失败保留旧列表 + 内联错误。
    /// 线程无结果退出也按失败处理，避免 busy 永久卡死（同样不算 daemon 响应）。
    pub(crate) fn tick_exclude(&mut self) {
        let Some(rx) = self.exclude_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(result) => {
                if let Some(error) = result.error {
                    self.exclude_error = Some(error);
                } else {
                    self.excluded = result.entries;
                    self.exclude_error = None;
                }
                self.exclude_busy = false;
                self.exclude_busy_list = false;
                self.exclude_loaded = true;
            }
            Err(TryRecvError::Empty) => self.exclude_rx = Some(rx),
            Err(TryRecvError::Disconnected) => {
                log::warn!("排除目录工作线程异常退出(无结果)");
                self.exclude_busy = false;
                self.exclude_busy_list = false;
                self.exclude_error = Some(ExcludeError::WorkerExited);
                self.exclude_loaded = false;
            }
        }
    }

    /// 入队一条快捷键命令（设置 pass；纯状态，运行时调用仍在 App）。
    ///
    /// 切换槽位录制时补一条 `CancelRecord`：App 先恢复旧键再重新挂起，
    /// 保证「先 resume 再 suspend」的明确语义（幂等，不会丢键）。
    /// 同槽重复 Begin **照常入队**：陈旧视图可能重复发 Begin，由 App 按取消处理
    /// （否则录制态无法退出、热键会一直挂起）；正常视图同槽点击发的是 Cancel。
    pub(crate) fn queue_hotkey_command(&mut self, command: HotkeyCommand) {
        match command {
            HotkeyCommand::BeginRecord(slot) => {
                if self.recording.is_some_and(|current| current != slot) {
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

    /// App `suspend()` 完成后进入录制：切槽、开始接收按键、清旧错误。
    ///
    /// `suspend()` 在主线程同步完成，因此不存在可渲染的「准备中」中间态。
    pub(crate) fn begin_recording(&mut self, slot: HotkeySlot, snapshot: HotkeySnapshot) {
        self.recording = Some(slot);
        self.hotkey_error = None;
        self.hotkey_snapshot = snapshot;
    }

    /// 取消录制/恢复旧键完成后落地：清录制态与错误，刷新快照。
    pub(crate) fn end_recording(&mut self, snapshot: HotkeySnapshot) {
        self.recording = None;
        self.hotkey_error = None;
        self.hotkey_snapshot = snapshot;
    }

    /// 记录设置视口焦点，并判定是否因「录制中失焦」取消录制。
    ///
    /// 只在 `true → false` 跃迁时返回 true：窗口刚打开、尚未获得焦点的 `false`
    /// 不误取消。macOS 最小化同样表现为失焦；`None` = 平台未报告，保守不动作。
    pub(crate) fn observe_focus(&mut self, focused: Option<bool>, open: bool) -> bool {
        let lost = open
            && self.recording.is_some()
            && self.viewport_focused == Some(true)
            && focused == Some(false);
        if let Some(focused) = focused {
            self.viewport_focused = Some(focused);
        }
        lost
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

    /// 恢复默认成功（运行时已注册默认键）后落地：结束录制、清空目标槽的设置字段，
    /// 返回待落盘副本（`None` = 跟随内置默认，不写死当前键）。
    ///
    /// 保存由调用方在共享锁外执行；失败只回填内联错误，不回退已注册的键。
    pub(crate) fn reset_recorded_hotkey(
        &mut self,
        slot: HotkeySlot,
        snapshot: HotkeySnapshot,
    ) -> SettingsFile {
        self.end_recording(snapshot);
        match slot {
            HotkeySlot::Palette => self.file.palette_hotkey = None,
            HotkeySlot::Window => self.file.window_hotkey = None,
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
                log::warn!("读取开机自启状态失败: {err}");
                self.autostart = None;
                self.autostart_approval = false;
                self.autostart_error = Some(AutostartError::ReadFailed);
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
            Some(AutostartError::NotEffective(status))
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
                log::warn!("开机自启设置失败: {message}");
                self.autostart_approval = false;
                self.autostart_error = Some(AutostartError::SetFailed);
                AutostartUpdate {
                    enabled: self.autostart.unwrap_or(false),
                    confirmed: false,
                }
            }
        }
    }

    /// 字号选择的状态落地（纯状态；调用方已完成 `font::set_scale` 与保存）。
    /// 保存失败也更新选中态——界面须与已应用的全局倍率一致。
    pub(crate) fn set_font_size_result(
        &mut self,
        size: FontSize,
        save_error: Option<SettingsError>,
    ) {
        self.file.font_size = size;
        self.font_error = save_error;
    }

    /// 语言选择的状态落地（纯状态；调用方已完成全局切换与保存）。
    /// 保存失败也更新选中态——界面须与已生效的语言一致。
    pub(crate) fn set_language_result(
        &mut self,
        setting: LanguageSetting,
        save_error: Option<LanguageError>,
    ) {
        self.file.language = setting;
        self.language_error = save_error;
    }

    /// 语言切换后请求 App 刷新托盘文案（App::logic 消费）。
    pub(crate) fn request_tray_relabel(&mut self) {
        self.tray_relabel_pending = true;
    }

    /// 取走托盘文案刷新标记（App::logic 消费；接入点在后续托盘文案任务）。
    #[allow(dead_code)]
    pub(crate) fn take_tray_relabel_pending(&mut self) -> bool {
        std::mem::take(&mut self.tray_relabel_pending)
    }

    /// 语言切换时清掉一次性错误（跨帧常驻状态使用类型化文案，不清）。
    pub(crate) fn clear_transient_messages(&mut self) {
        self.hotkey_error = None;
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
            language: state.file.language,
            language_error: state.language_error.clone(),
            hotkeys: state.hotkey_snapshot.clone(),
            recording: state.recording,
            hotkey_error: state.hotkey_error.clone(),
            excluded: state.excluded.clone(),
            exclude_busy: state.exclude_busy,
            exclude_busy_list: state.exclude_busy_list,
            exclude_error: state.exclude_error.clone(),
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
    let focused = ctx.input(|i| i.viewport().focused);
    let (open, theme_changed, focus_lost, load_exclude) = {
        let mut s = lock_shared(shared);
        // 排除目录：先收后台结果（非阻塞），再按打开挂起位决定是否补一次 list。
        let open = s.settings.open;
        let load_exclude = s.settings.advance_exclude(open);
        let theme_changed = open && s.theme.maybe_apply(&ctx, now);
        let focus_lost = s.settings.observe_focus(focused, open);
        (open, theme_changed, focus_lost, load_exclude)
    };
    if load_exclude {
        // 工作线程执行阻塞的 client 调用；锁内只落 busy/通道，spawn 在锁外。
        // 只有 begin/spawn 真正成功才清挂起位；被拒时保留，下一 pass 重试。
        if spawn_exclude(&ctx, shared, "list", None) {
            lock_shared(shared).settings.exclude_refresh_pending = false;
        }
        ctx.request_repaint_of(viewport_id());
    }
    // 快照放在启动 list 之后：首帧即带 busy（避免旧快照允许在加载中点击添加）。
    let data = SettingsData::from_state(&lock_shared(shared).settings);
    if theme_changed {
        // `set_theme` 只改偏好；显式排一帧让本视口按新主题出图。
        ctx.request_repaint_of(viewport_id());
    }
    if focus_lost {
        // 失焦/最小化期间键盘事件不再进入设置窗；继续录制会让两槽热键长期挂起。
        // 交给 App 的取消命令走主线程 resume；只在焦点 true→false 跃迁时触发一次。
        enqueue_hotkey_command(shared, &ctx, HotkeyCommand::CancelRecord);
        log::info!("设置窗口失焦:取消快捷键录制");
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
        crate::i18n::s().settings_title,
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
        SettingsAction::SetLanguage(setting) => apply_language(shared, ctx, setting),
        SettingsAction::BeginHotkeyRecord(slot) => {
            enqueue_hotkey_command(shared, ctx, HotkeyCommand::BeginRecord(slot))
        }
        SettingsAction::CancelHotkeyRecord => {
            enqueue_hotkey_command(shared, ctx, HotkeyCommand::CancelRecord)
        }
        SettingsAction::CommitHotkey { slot, hotkey } => {
            enqueue_hotkey_command(shared, ctx, HotkeyCommand::Commit { slot, hotkey })
        }
        SettingsAction::ResetHotkey(slot) => {
            enqueue_hotkey_command(shared, ctx, HotkeyCommand::Reset(slot))
        }
        SettingsAction::HotkeyError { slot, message } => {
            // 视图侧校验/查重错误只落共享态；录制保持（下次按键可重试）。
            lock_shared(shared).settings.hotkey_rejected(slot, message);
            ctx.request_repaint_of(viewport_id());
        }
        SettingsAction::ResetExcludes => {
            // 原生模态:主线程调用且不持共享锁(与目录选择器同纪律)。取消 = 无状态
            // 变化;确认后才启动既有 reset worker(一次重扫)。
            if platform::confirm_action(
                i18n::s().reset_confirm_title,
                i18n::s().reset_confirm_message,
                i18n::s().reset_confirm_action,
            ) {
                spawn_exclude(ctx, shared, "reset", None);
            } else {
                log::info!("排除目录:取消恢复默认");
            }
            ctx.request_repaint_of(viewport_id());
        }
        SettingsAction::AddExclude => apply_add_exclude(shared, ctx),
        SettingsAction::RemoveExclude(path) => {
            spawn_exclude(ctx, shared, "remove", Some(path));
            ctx.request_repaint_of(viewport_id());
        }
    }
}

/// 「添加目录…」：原生选择器必须在主线程调用，且不持共享锁（模态期间冻结 egui）。
/// 取消/失败 = 无 busy、无状态变化；选中后交给后台 add 工作线程。
fn apply_add_exclude(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context) {
    let Some(path) = platform::pick_directory() else {
        log::info!("排除目录:取消添加(未选择目录)");
        return;
    };
    let path = path.to_string_lossy().into_owned();
    log::info!("排除目录:添加 {path}");
    spawn_exclude(ctx, shared, "add", Some(path));
    ctx.request_repaint_of(viewport_id());
}

/// 操作 → `begin_exclude(list, clear_error)` 标志。
///
/// 自动 list 刷新**保留**已展示的错误（失败文案不被重开吞掉）；add/remove/reset
/// 是新用户操作，开始即清旧提示。
fn exclude_begin_flags(op: &str) -> (bool, bool) {
    (op == "list", op != "list")
}

/// 启动一次排除目录后台操作（list/add/remove/reset）。
///
/// 共享锁只用于落 busy/错误/回投通道与取 `data_dir`，随即释放；`client::exclude`
/// （add/remove/reset 会触发 daemon 全量重扫）与线程启动都在锁外。完成时工作线程
/// 请求设置视口重绘，`tick_exclude` 下一帧收取结果。
///
/// 返回 true = 操作已开始（线程启动失败也算：失败已按终态落错误，调用方可清
/// 刷新挂起位）；false = 已有操作在途，未做任何状态修改。
fn spawn_exclude(
    ctx: &egui::Context,
    shared: &Arc<Mutex<Shared>>,
    op: &'static str,
    arg: Option<String>,
) -> bool {
    let (tx, rx) = mpsc::channel::<ExcludeResult>();
    let (list, clear_error) = exclude_begin_flags(op);
    let data_dir = {
        let mut s = lock_shared(shared);
        if !s.settings.begin_exclude(list, clear_error, rx) {
            log::warn!("排除目录操作 {op} 被忽略:已有操作在途");
            return false;
        }
        s.settings.data_dir.clone()
    };
    // busy 立即出图（工作线程可能较慢，尤其是变更后的全量重扫）。
    ctx.request_repaint_of(viewport_id());

    let thread_ctx = ctx.clone();
    let spawned = std::thread::Builder::new()
        .name("settings-exclude".to_owned())
        .spawn(move || {
            let result = exclude_result(client::exclude(
                &client::DaemonPaths { data_dir },
                op,
                arg.as_deref(),
            ));
            match &result.error {
                Some(error) => log::warn!("排除目录 {op} 失败:{error:?}"),
                None => log::info!("排除目录 {op} 完成({} 条)", result.entries.len()),
            }
            if let Err(err) = tx.send(result) {
                log::warn!("排除目录结果回投失败(设置页可能已退出):{err}");
            }
            thread_ctx.request_repaint_of(viewport_id());
        });
    if let Err(err) = spawned {
        lock_shared(shared)
            .settings
            .exclude_spawn_failed(err.to_string());
        ctx.request_repaint_of(viewport_id());
    }
    true
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
        let detail = format!("{err:#}");
        log::warn!("字号 {size:?} 已应用,但设置保存失败:{detail}");
        SettingsError::SaveFailed
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

/// 语言即时生效：解析 → 全局切换 → 落盘（失败仅内联留痕）→ 重绘三窗 + 托盘刷新标记。
fn apply_language(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context, setting: LanguageSetting) {
    let preferred = platform::preferred_languages().unwrap_or_else(|err| {
        log::warn!("读取系统语言失败,按简体中文: {err}");
        Vec::new()
    });
    let lang = crate::i18n::resolve_language(setting, &preferred);
    crate::i18n::set_lang(lang);

    let (mut file, data_dir) = {
        let s = lock_shared(shared);
        (s.settings.file.clone(), s.settings.data_dir.clone())
    };
    file.language = setting;

    let save_error = store::save(&data_dir, &file).err().map(|err| {
        let detail = format!("{err:#}");
        log::warn!("语言 {setting:?} 已应用,但设置保存失败:{detail}");
        LanguageError::SaveFailed
    });

    {
        let mut s = lock_shared(shared);
        s.settings.set_language_result(setting, save_error);
        s.settings.clear_transient_messages();
        s.settings.request_tray_relabel();
        s.results.clear_transient_messages();
    }
    log::info!("设置语言 → {setting:?}({lang:?})");

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
        assert!(state.language_error.is_none());
        assert!(!state.tray_relabel_pending);
        assert_eq!(state.excluded, engine::config::default_config().excluded);
        assert!(!state.exclude_busy);
        assert!(!state.exclude_busy_list);
        assert!(state.exclude_error.is_none());
        assert!(!state.exclude_loaded, "首次 list 前只显示种子列表");
        assert!(!state.exclude_refresh_pending, "构造时尚未打开");
        assert!(state.exclude_rx.is_none());
        assert_eq!(state.tray_autostart_sync, None);
        assert_eq!(
            state.hotkey_snapshot,
            HotkeySnapshot::default(),
            "快照由 App::new 植入,构造时为默认空值"
        );
        assert_eq!(state.recording, None);
        assert_eq!(state.viewport_focused, None);
        assert_eq!(state.hotkey_error, None);
        assert!(state.hotkey_commands.is_empty());
        assert!(!state.positioned_once, "首开前未定位");
        assert!(!state.cancel_recording_pending);
    }

    /// 关闭→打开才挂起一次列表刷新；已打开/重复 request_open 不重复挂起。
    #[test]
    fn exclude_refresh_pending_set_only_on_open_transition() {
        let mut state = test_state();
        assert!(!state.exclude_refresh_pending, "启动未打开不取列表");

        state.request_open();
        assert!(state.exclude_refresh_pending, "打开设置须加载真实列表");
        state.exclude_refresh_pending = false;
        state.request_open();
        assert!(
            !state.exclude_refresh_pending,
            "窗口已打开时重复请求不得重取列表"
        );

        state.mark_hidden();
        state.request_open();
        assert!(
            state.exclude_refresh_pending,
            "重开须重取 daemon 列表(隐藏期可能被 CLI 改动)"
        );
    }

    /// 假通道：开始一次操作并拿到回投端（不启真实 client/daemon）。
    /// list = 自动刷新（保留错误）；否则为 add/remove（清旧错误）。
    fn begin_fake_exclude(state: &mut SettingsState, list: bool) -> mpsc::Sender<ExcludeResult> {
        let (tx, rx) = mpsc::channel();
        assert!(
            state.begin_exclude(list, !list, rx),
            "空闲态首个操作应能开始"
        );
        tx
    }

    /// list 成功：空通道保持 busy；回投后覆盖列表、清错、结束 busy。
    #[test]
    fn exclude_list_success_replaces_entries() {
        let mut state = test_state();
        let seeded = state.excluded.clone();
        assert!(!state.exclude_loaded);

        let tx = begin_fake_exclude(&mut state, true);
        assert!(state.exclude_busy);
        assert!(state.exclude_busy_list, "list 须显示「加载中…」");
        state.tick_exclude();
        assert!(state.exclude_busy, "无结果时不得提前结束 busy");
        assert_eq!(state.excluded, seeded, "首批结果前保留种子列表");

        tx.send(ExcludeResult {
            entries: vec!["/Users/t/Videos".into(), "node_modules".into()],
            error: None,
        })
        .unwrap();
        state.tick_exclude();

        assert_eq!(
            state.excluded,
            vec!["/Users/t/Videos".to_owned(), "node_modules".to_owned()],
            "成功须以 daemon 响应覆盖列表"
        );
        assert!(state.exclude_error.is_none());
        assert!(!state.exclude_busy);
        assert!(!state.exclude_busy_list);
        assert!(state.exclude_loaded);
        assert!(state.exclude_rx.is_none(), "one-op 通道用完即弃");
    }

    /// list 失败：保留旧列表 + 原样错误；busy 结束且 loaded 记账（不无限重试）。
    #[test]
    fn exclude_list_failure_keeps_entries_and_reports_error() {
        let mut state = test_state();
        let seeded = state.excluded.clone();

        let tx = begin_fake_exclude(&mut state, true);
        tx.send(ExcludeResult {
            entries: Vec::new(),
            error: Some(ExcludeError::Daemon(
                "daemon 未在运行;排除变更需引擎在线".into(),
            )),
        })
        .unwrap();
        state.tick_exclude();

        assert_eq!(state.excluded, seeded, "失败不得清空/覆盖旧列表");
        assert_eq!(
            state.exclude_error,
            Some(ExcludeError::Daemon(
                "daemon 未在运行;排除变更需引擎在线".into()
            ))
        );
        assert!(!state.exclude_busy);
        assert!(state.exclude_loaded);
    }

    /// add/remove 成功：均以 daemon 返回列表覆盖；busy 类型为重扫。
    #[test]
    fn exclude_add_remove_success_replaces_entries() {
        for (label, entries) in [
            ("add", vec!["/Users/t/Documents".to_owned()]),
            ("remove", vec!["node_modules".to_owned()]),
        ] {
            let mut state = test_state();
            let tx = begin_fake_exclude(&mut state, false);
            assert!(
                state.exclude_busy && !state.exclude_busy_list,
                "{label} 须为重扫"
            );
            tx.send(ExcludeResult {
                entries: entries.clone(),
                error: None,
            })
            .unwrap();
            state.tick_exclude();
            assert_eq!(state.excluded, entries, "{label} 成功须刷新列表");
            assert!(state.exclude_error.is_none(), "{label} 成功须清旧错误");
            assert!(!state.exclude_busy, "{label} 完成须结束 busy");
        }
    }

    /// add/remove 失败：保留操作前列表 + daemon 原样文案（含越界校验）。
    #[test]
    fn exclude_add_remove_failure_keeps_entries() {
        for (label, message) in [
            ("add", "路径不在扫描根内: /etc(扫描根: /Users/t)"),
            ("remove", "条目不存在: node_modules"),
        ] {
            let mut state = test_state();
            let before = state.excluded.clone();
            let tx = begin_fake_exclude(&mut state, false);
            tx.send(ExcludeResult {
                entries: Vec::new(),
                error: Some(ExcludeError::Daemon(message.to_owned())),
            })
            .unwrap();
            state.tick_exclude();
            assert_eq!(state.excluded, before, "{label} 失败须保留旧列表");
            assert_eq!(
                state.exclude_error,
                Some(ExcludeError::Daemon(message.to_owned()))
            );
            assert!(!state.exclude_busy);
        }
    }

    /// 原生确认弹窗文案是行为契约的一部分（标题/正文/破坏性按钮），变更须同步文档。
    #[test]
    fn reset_native_confirmation_copy_matches_contract() {
        assert_eq!(i18n::s().reset_confirm_title, "恢复默认排除目录？");
        assert_eq!(
            i18n::s().reset_confirm_message,
            "将清空自定义排除项，恢复 ~/Library、node_modules、.git。此操作会触发一次索引重扫。"
        );
        assert_eq!(i18n::s().reset_confirm_action, "恢复默认");
    }

    /// reset worker 成功：响应三条默认覆盖旧列表、清错、结束 busy。
    #[test]
    fn exclude_reset_success_replaces_entries_with_defaults() {
        let mut state = test_state();
        state.excluded = vec!["/custom/a".into(), "node_modules".into()];
        state.exclude_error = Some(ExcludeError::Daemon("旧错误".into()));
        let tx = begin_fake_exclude(&mut state, false);
        assert!(
            state.exclude_busy && !state.exclude_busy_list,
            "reset 须为重扫"
        );

        let defaults = engine::config::default_config().excluded;
        tx.send(ExcludeResult {
            entries: defaults.clone(),
            error: None,
        })
        .unwrap();
        state.tick_exclude();

        assert_eq!(state.excluded, defaults, "成功须以响应覆盖为默认三条");
        assert!(state.exclude_error.is_none(), "reset 成功须清旧错误");
        assert!(!state.exclude_busy);
        assert!(state.exclude_loaded);
    }

    /// reset worker 失败/离线：保留旧列表 + 原样错误，并结束 busy。
    #[test]
    fn exclude_reset_failure_keeps_entries_and_reports_error() {
        let mut state = test_state();
        state.excluded = vec!["/custom/a".into(), "node_modules".into()];
        let before = state.excluded.clone();
        let tx = begin_fake_exclude(&mut state, false);
        tx.send(ExcludeResult {
            entries: Vec::new(),
            error: Some(ExcludeError::Daemon(
                "配置保存失败(磁盘只读),排除目录未改变".into(),
            )),
        })
        .unwrap();
        state.tick_exclude();

        assert_eq!(state.excluded, before, "失败不得覆盖旧列表");
        assert_eq!(
            state.exclude_error,
            Some(ExcludeError::Daemon(
                "配置保存失败(磁盘只读),排除目录未改变".into()
            ))
        );
        assert!(!state.exclude_busy, "失败也须结束 busy");
    }

    /// 同一时刻只允许一个操作；第二操作不得抢占 busy 类型/通道。
    #[test]
    fn exclude_single_operation_in_flight() {
        let mut state = test_state();
        let _tx = begin_fake_exclude(&mut state, true);
        let (_tx2, rx2) = mpsc::channel();
        assert!(
            !state.begin_exclude(false, true, rx2),
            "在途操作未回投前不得开始第二操作"
        );
        assert!(
            state.exclude_busy_list,
            "被拒绝的操作不得改变当前 busy 类型"
        );
    }

    /// 新操作开始即清旧错误（用户点添加/移除后旧提示不残留）。
    #[test]
    fn exclude_begin_clears_previous_error() {
        let mut state = test_state();
        state.exclude_error = Some(ExcludeError::Daemon("旧错误".into()));
        let _tx = begin_fake_exclude(&mut state, false);
        assert!(state.exclude_error.is_none());
    }

    /// 重要回归：add/remove 失败后紧接着自动 list 刷新，失败文案必须保留到
    /// 一次**成功** list；刷新失败则替换为新错误。
    #[test]
    fn exclude_auto_refresh_preserves_terminal_error_until_success() {
        let mut state = test_state();

        // 关窗期间 add 失败（结果在重开前落地）。
        let add = begin_fake_exclude(&mut state, false);
        add.send(ExcludeResult {
            entries: Vec::new(),
            error: Some(ExcludeError::Daemon("路径不在扫描根内: /etc".into())),
        })
        .unwrap();
        state.tick_exclude();
        assert_eq!(
            state.exclude_error,
            Some(ExcludeError::Daemon("路径不在扫描根内: /etc".into()))
        );

        // 重开触发自动刷新：list 开始不得清掉刚展示的失败文案。
        state.exclude_refresh_pending = true;
        let refresh = begin_fake_exclude(&mut state, true);
        assert_eq!(
            state.exclude_error,
            Some(ExcludeError::Daemon("路径不在扫描根内: /etc".into())),
            "自动 list 刷新不得吞掉终端错误"
        );
        assert!(state.exclude_busy_list);

        // 刷新自身失败：保留旧列表，错误替换为 list 的失败原因。
        refresh
            .send(ExcludeResult {
                entries: Vec::new(),
                error: Some(ExcludeError::Daemon("daemon 未在运行".into())),
            })
            .unwrap();
        state.tick_exclude();
        assert_eq!(
            state.exclude_error,
            Some(ExcludeError::Daemon("daemon 未在运行".into()))
        );

        // 成功 list 才清错误并覆盖列表。
        let refresh = begin_fake_exclude(&mut state, true);
        assert_eq!(
            state.exclude_error,
            Some(ExcludeError::Daemon("daemon 未在运行".into())),
            "失败文案仍保留到成功响应前"
        );
        refresh
            .send(ExcludeResult {
                entries: vec!["node_modules".into()],
                error: None,
            })
            .unwrap();
        state.tick_exclude();
        assert!(state.exclude_error.is_none(), "成功 list 才清错误");
        assert_eq!(state.excluded, vec!["node_modules".to_owned()]);
    }

    /// pass 推进：busy 时 pending 保留；结果落地后同一 pass 请求刷新（tick→refresh）。
    #[test]
    fn exclude_pass_ticks_result_then_requests_refresh() {
        let mut state = test_state();
        state.exclude_refresh_pending = true;
        let tx = begin_fake_exclude(&mut state, true);

        assert!(!state.advance_exclude(true), "busy 中不得重复启动 list");
        assert!(state.exclude_refresh_pending, "busy 时挂起位必须保留");

        tx.send(ExcludeResult {
            entries: vec!["/Users/t/Videos".into()],
            error: None,
        })
        .unwrap();
        assert!(
            state.advance_exclude(true),
            "结果落地后同一 pass 应请求一次刷新"
        );
        assert_eq!(state.excluded, vec!["/Users/t/Videos".to_owned()]);
        assert!(!state.exclude_busy);
        assert!(
            state.exclude_refresh_pending,
            "advance 不清挂起位；由 spawn 成功后清"
        );
    }

    /// 关闭期间 add/remove 结果落地，重开后同一 pass 应用并请求刷新。
    #[test]
    fn exclude_close_reopen_applies_inflight_result() {
        let mut state = test_state();
        state.request_open();
        state.exclude_refresh_pending = false;

        let tx = begin_fake_exclude(&mut state, false);
        state.mark_hidden();
        state.request_open();
        assert!(state.exclude_refresh_pending, "重开须补一次 list");

        tx.send(ExcludeResult {
            entries: vec!["node_modules".into()],
            error: None,
        })
        .unwrap();
        assert!(state.advance_exclude(true));
        assert_eq!(state.excluded, vec!["node_modules".to_owned()]);
        assert!(!state.exclude_busy);
        assert!(state.exclude_refresh_pending);
    }

    /// 工作线程启动失败：终态落错误、清 busy，但不算收到 daemon 响应。
    #[test]
    fn exclude_spawn_failure_is_terminal_without_response() {
        let mut state = test_state();
        let before = state.excluded.clone();
        state.exclude_spawn_failed("无法启动排除目录工作线程:资源不足".into());
        assert!(!state.exclude_busy);
        assert!(!state.exclude_busy_list);
        assert!(state.exclude_rx.is_none());
        assert!(
            !state.exclude_loaded,
            "未拿到 daemon 响应,loaded 保持 false"
        );
        assert_eq!(state.excluded, before);
        assert_eq!(state.exclude_error, Some(ExcludeError::StartFailed));
    }

    /// 工作线程无结果退出：不得让 busy 永久卡住；按失败展示并保留列表。
    #[test]
    fn exclude_worker_disconnect_ends_busy_with_error() {
        let mut state = test_state();
        let before = state.excluded.clone();
        let tx = begin_fake_exclude(&mut state, false);
        drop(tx);
        state.tick_exclude();
        assert!(!state.exclude_busy);
        assert!(!state.exclude_loaded, "无结果退出不算 daemon 响应");
        assert_eq!(state.excluded, before);
        assert_eq!(state.exclude_error, Some(ExcludeError::WorkerExited));
    }

    /// 只有自动 list 保留错误；add/remove/reset 开始即清旧提示（pass 级接线钉）。
    #[test]
    fn exclude_op_begin_flags_preserve_error_only_for_auto_list() {
        assert_eq!(exclude_begin_flags("list"), (true, false));
        assert_eq!(exclude_begin_flags("add"), (false, true));
        assert_eq!(exclude_begin_flags("remove"), (false, true));
        assert_eq!(
            exclude_begin_flags("reset"),
            (false, true),
            "reset 是变更操作：显示重扫中并清旧错误"
        );
    }

    /// `ExcludeOutcome` 映射：ok=true 才给覆盖列表；ok=false/Err 只给错误。
    #[test]
    fn exclude_outcome_mapping_marks_success_and_failures() {
        let ok = exclude_result(Ok(client::ExcludeOutcome {
            ok: true,
            excluded: vec!["/abs/a".into()],
            message: None,
        }));
        assert_eq!(
            ok,
            ExcludeResult {
                entries: vec!["/abs/a".into()],
                error: None,
            }
        );

        let rejected = exclude_result(Ok(client::ExcludeOutcome {
            ok: false,
            excluded: vec!["/abs/a".into()],
            message: Some("路径不在扫描根内".into()),
        }));
        assert!(rejected.entries.is_empty(), "失败不得携带可覆盖列表");
        assert_eq!(
            rejected.error,
            Some(ExcludeError::Daemon("路径不在扫描根内".into()))
        );

        let no_reason = exclude_result(Ok(client::ExcludeOutcome {
            ok: false,
            excluded: Vec::new(),
            message: None,
        }));
        assert_eq!(no_reason.error, Some(ExcludeError::NoReason));

        let offline = exclude_result(Err(anyhow::anyhow!(
            "daemon 未在运行;排除变更需引擎在线,先执行 macEverything start"
        )));
        assert!(offline.entries.is_empty());
        assert!(
            matches!(
                &offline.error,
                Some(ExcludeError::Daemon(detail)) if detail.contains("daemon 未在运行")
            ),
            "{:?}",
            offline.error
        );
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

    /// 队列顺序：切槽先补取消；同槽重复 Begin 交由 App 视为取消；Commit/Cancel FIFO。
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

        // 已在录制该槽：陈旧视图重复 Begin 照常入队，由 App 按取消处理。
        state.recording = Some(HotkeySlot::Window);
        state.queue_hotkey_command(HotkeyCommand::BeginRecord(HotkeySlot::Window));
        assert_eq!(
            state.take_hotkey_commands(),
            vec![HotkeyCommand::BeginRecord(HotkeySlot::Window)],
            "同槽重复 Begin 必须交给 App 视为取消（不得静默丢弃）"
        );

        state.queue_hotkey_command(HotkeyCommand::Commit {
            slot: HotkeySlot::Window,
            hotkey: alt_k(),
        });
        state.queue_hotkey_command(HotkeyCommand::Reset(HotkeySlot::Palette));
        state.queue_hotkey_command(HotkeyCommand::CancelRecord);
        assert_eq!(
            state.take_hotkey_commands(),
            vec![
                HotkeyCommand::Commit {
                    slot: HotkeySlot::Window,
                    hotkey: alt_k()
                },
                HotkeyCommand::Reset(HotkeySlot::Palette),
                HotkeyCommand::CancelRecord
            ],
            "命令须 FIFO 落地（含恢复默认）"
        );
        assert!(state.hotkey_commands.is_empty(), "取走即清");
    }

    /// 进入录制：清旧错误、快照保持运行时真源；结束录制清全部录制态。
    #[test]
    fn begin_and_end_recording_transitions() {
        let mut state = test_state();
        let before = hotkey_snapshot(Some(alt_space()), Some(alt_shift_f()));
        state.hotkey_error = Some((HotkeySlot::Palette, "旧错误".into()));

        state.begin_recording(HotkeySlot::Palette, before.clone());
        assert_eq!(state.recording, Some(HotkeySlot::Palette));
        assert_eq!(state.hotkey_error, None, "新录制清旧错误");
        assert_eq!(state.hotkey_snapshot, before, "挂起期快照仍显示旧键");

        let after = hotkey_snapshot(Some(alt_space()), None);
        state.hotkey_error = Some((HotkeySlot::Window, "提示".into()));
        state.end_recording(after.clone());
        assert_eq!(state.recording, None);
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

    /// 恢复默认成功：清空目标槽设置字段（跟随内置默认）并落盘；另一槽保留。
    #[test]
    fn reset_success_clears_file_field_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let custom = alt_k();
        let window = alt_shift_f();
        let mut state = SettingsState::new(
            SettingsFile {
                palette_hotkey: Some(custom),
                window_hotkey: Some(window),
                ..SettingsFile::default()
            },
            dir.path().to_path_buf(),
            hotkey_snapshot(Some(custom), Some(window)),
        );
        state.begin_recording(HotkeySlot::Palette, state.hotkey_snapshot.clone());

        let defaults = hotkey_snapshot(Some(alt_space()), Some(window));
        let file = state.reset_recorded_hotkey(HotkeySlot::Palette, defaults.clone());

        assert_eq!(state.recording, None);
        assert_eq!(state.hotkey_error, None);
        assert_eq!(state.hotkey_snapshot, defaults, "快照显示默认键");
        assert_eq!(
            state.file.palette_hotkey, None,
            "恢复默认 = 跟随内置默认（字段清空）"
        );
        assert_eq!(state.file.window_hotkey, Some(window), "另一槽设置不得被清");
        assert_eq!(file, state.file, "返回副本供锁外保存");

        crate::settings::save(dir.path(), &file).unwrap();
        let loaded = crate::settings::load(dir.path());
        assert_eq!(loaded.palette_hotkey, None, "重启后仍跟随内置默认");
        assert_eq!(loaded.window_hotkey, Some(window));
    }

    /// 恢复默认失败：运行时已恢复旧键 → 结束录制、按槽内联错误、设置文件不变。
    #[test]
    fn reset_failure_ends_recording_and_keeps_file_field() {
        let mut state = test_state();
        state.file.palette_hotkey = Some(alt_k());
        let old = hotkey_snapshot(Some(alt_k()), None);
        state.begin_recording(HotkeySlot::Palette, old.clone());

        state.recording_failed(
            HotkeySlot::Palette,
            "恢复默认快捷键失败(可能被系统或其他应用占用): 被占用".into(),
            old.clone(),
        );

        assert_eq!(state.recording, None, "失败退出录制（旧键已恢复）");
        assert_eq!(state.hotkey_snapshot, old, "回滚后的快照 = 旧键");
        assert_eq!(
            state.file.palette_hotkey,
            Some(alt_k()),
            "失败不得清空已保存的自定义键"
        );
        assert!(
            state.hotkey_error.as_ref().is_some_and(|(slot, message)| {
                *slot == HotkeySlot::Palette && message.contains("恢复默认快捷键失败")
            }),
            "失败须按槽落内联错误:{:?}",
            state.hotkey_error
        );
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
        assert_eq!(state.hotkey_error, None);
        assert_eq!(state.hotkey_snapshot, restored);

        // 未录制：信号仍被取走（不跨帧残留），App 分支为 no-op。
        let mut state = test_state();
        state.mark_hidden();
        assert!(state.take_cancel_recording_pending());
        assert_eq!(state.recording, None);
        assert!(!state.take_cancel_recording_pending(), "不得重复消费");
    }

    /// 录制中失焦/最小化 → 请求取消；只在 true→false 跃迁触发，避免插值/重复帧。
    #[test]
    fn focus_loss_cancels_recording_only_on_focused_transition() {
        let mut state = test_state();
        state.request_open();
        assert!(!state.observe_focus(Some(true), true), "首次获得焦点不取消");
        state.recording = Some(HotkeySlot::Palette);
        assert!(state.observe_focus(Some(false), true), "录制中失焦须取消");
        assert!(
            !state.observe_focus(Some(false), true),
            "持续失焦不得每帧重复取消"
        );
        assert!(!state.observe_focus(Some(true), true), "重新聚焦不取消");
        assert!(state.observe_focus(Some(false), true), "再次失焦须再取消");

        // 窗口刚打开且从未观测到聚焦：首个 false 不得误取消。
        let mut state = test_state();
        state.request_open();
        state.recording = Some(HotkeySlot::Window);
        assert!(
            !state.observe_focus(Some(false), true),
            "首次焦点报告为 false(瞬态)不取消"
        );

        // 未录制 / 窗口已关闭：失焦不触发。
        let mut state = test_state();
        state.request_open();
        state.observe_focus(Some(true), true);
        assert!(!state.observe_focus(Some(false), true), "未录制不取消");
        state.recording = Some(HotkeySlot::Palette);
        assert!(
            !state.observe_focus(Some(false), false),
            "窗口关闭时不再触发失焦取消"
        );

        // 关闭重置焦点观测，重开从 None 重新学习。
        state.mark_hidden();
        assert_eq!(state.viewport_focused, None);
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

        state.set_font_size_result(FontSize::Large, Some(SettingsError::SaveFailed));
        assert_eq!(
            state.file.font_size,
            FontSize::Large,
            "已应用字号须反映到选中态"
        );
        assert_eq!(state.font_error, Some(SettingsError::SaveFailed));
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
        assert_eq!(state.autostart_error, Some(AutostartError::ReadFailed));

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
        assert_eq!(
            state.autostart_error,
            Some(AutostartError::NotEffective(Status::NotFound)),
            "启用后未注册须落内联错误"
        );
        assert_eq!(state.take_tray_autostart_sync(false), Some(false));

        state.autostart = Some(true);
        state.autostart_approval = false;
        let update = state.autostart_toggled(false, Err("未打包".into()));
        assert!(!update.confirmed);
        assert_eq!(state.autostart, Some(true), "失败必须保持旧状态");
        assert!(!state.autostart_approval);
        assert_eq!(
            state.autostart_error,
            Some(AutostartError::SetFailed),
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
        state.file.language = LanguageSetting::En;
        state.autostart = Some(false);
        state.autostart_approval = true;
        state.autostart_error = Some(AutostartError::SetFailed);
        state.font_error = Some(SettingsError::SaveFailed);
        state.language_error = Some(LanguageError::SaveFailed);
        state.hotkey_snapshot.palette_keys = vec!["⌃".into(), "⌥".into(), "K".into()];
        state.hotkey_snapshot.window_keys = vec!["⇧".into(), "⌘".into(), "J".into()];
        state.recording = Some(HotkeySlot::Window);
        state.hotkey_error = Some((HotkeySlot::Palette, "需包含修饰键".into()));
        state.excluded = vec!["node_modules".into()];
        state.exclude_busy = true;
        state.exclude_busy_list = true;
        state.exclude_error = Some(ExcludeError::Daemon("daemon 未在运行".into()));

        let data = SettingsData::from_state(&state);
        assert_eq!(data.font_size, FontSize::Large);
        assert!(!data.autostart, "审批中显示为关");
        assert!(data.autostart_approval);
        assert_eq!(data.autostart_error, Some(AutostartError::SetFailed));
        assert_eq!(data.font_error, Some(SettingsError::SaveFailed));
        assert_eq!(data.language, LanguageSetting::En);
        assert_eq!(data.language_error, Some(LanguageError::SaveFailed));
        assert_eq!(
            data.hotkeys, state.hotkey_snapshot,
            "键帽展示唯一真源 = 快照"
        );
        assert_eq!(data.hotkeys.palette_keys, vec!["⌃", "⌥", "K"]);
        assert_eq!(data.hotkeys.window_keys, vec!["⇧", "⌘", "J"]);
        assert_eq!(data.recording, Some(HotkeySlot::Window));
        assert_eq!(
            data.hotkey_error,
            Some((HotkeySlot::Palette, "需包含修饰键".to_owned()))
        );
        assert_eq!(data.excluded, vec!["node_modules".to_owned()]);
        assert!(data.exclude_busy);
        assert!(data.exclude_busy_list);
        assert_eq!(
            data.exclude_error,
            Some(ExcludeError::Daemon("daemon 未在运行".into()))
        );
    }

    /// 语言落地：更新设置快照 + 错误；切换清一次性快捷键错误并挂起托盘刷新。
    #[test]
    fn language_state_updates_file_clears_transient_and_queues_tray_relabel() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = SettingsState::new(
            SettingsFile::default(),
            dir.path().to_path_buf(),
            HotkeySnapshot::default(),
        );
        state.hotkey_error = Some((HotkeySlot::Palette, "旧错误".into()));

        state.set_language_result(LanguageSetting::En, Some(LanguageError::SaveFailed));
        assert_eq!(
            state.file.language,
            LanguageSetting::En,
            "即时切换须反映到设置快照"
        );
        assert_eq!(state.language_error, Some(LanguageError::SaveFailed));
        assert_eq!(
            crate::settings::load(dir.path()).language,
            LanguageSetting::System,
            "纯状态转换不落盘"
        );

        state.clear_transient_messages();
        assert_eq!(state.hotkey_error, None, "语言切换清一次性错误");

        state.request_tray_relabel();
        assert!(state.take_tray_relabel_pending(), "须挂起一次托盘文案刷新");
        assert!(!state.take_tray_relabel_pending(), "取走即清");

        state.set_language_result(LanguageSetting::ZhHans, None);
        assert_eq!(state.file.language, LanguageSetting::ZhHans);
        assert!(state.language_error.is_none(), "保存成功须清错误");
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
        assert_eq!(
            b.title.as_deref(),
            Some(crate::i18n::s().settings_title),
            "设置窗标题取当前语言"
        );
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
