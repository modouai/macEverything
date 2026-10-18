//! 设置窗口（v2.1）：620×600 固定、原生交通灯 + 自绘 44 标题栏。
//!
//! 第三个 deferred 视口（与悬浮条同模式：隐藏走 `Visible(false)`，窗口保留不销毁）。
//! 本文件负责视口/运行期状态/行为编排：`draw` 短暂持锁取快照 → 锁外绘制
//! （`ui/settings_view.rs` 纯视图）→ 锁外落地字号/自启动作（保存与 SMAppService
//! 调用不持共享锁）。标题栏固定，内容区整页纵向滚动。
//!
//! 窗口交互：标题栏可拖拽（左侧预留原生交通灯）、红点/⌘W = 隐藏（不退出 App）。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui;

use crate::autostart::{self, Status};
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
/// `visible(false)`：启动隐藏；打开走 `open_settings` 的 `Visible(true)` + `Focus`。
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

/// 设置窗口运行期真源：开合 + 设置快照 + 自启/字号错误 + 渲染数据。
///
/// 本结构持有运行期数据（不每帧重建 `engine::config::default_config()`）；
/// `SettingsData` 只是出锁绘制用的复制快照。排除目录先种引擎默认值，
/// 真实 daemon 列表由 Task 8 接入。
pub struct SettingsState {
    pub(crate) open: bool,
    /// 已加载的 `settings.toml` 快照（字号写入在此更新）。
    pub(crate) file: SettingsFile,
    /// 设置文件所在数据目录（保存字号用）。
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
    /// 唤起快捷键键帽（内置默认；快捷键 plan 接入后更新）。
    pub(crate) palette_keys: Vec<String>,
    /// 完整窗口快捷键键帽（内置默认）。
    pub(crate) window_keys: Vec<String>,
    /// 设置页切换自启成功后待同步给托盘的值（App::logic 消费）。
    pub(crate) tray_autostart_sync: Option<bool>,
    /// 首次打开已完成定位决策：之后重开只置前，保留用户拖动后的位置。
    pub(crate) positioned_once: bool,
    /// 关闭/⌘W 后待消费的「取消录制」信号（快捷键 plan 接入；当前无录制状态，
    /// `App::logic` 取走并留痕，避免标志跨帧残留）。
    pub(crate) cancel_recording_pending: bool,
}

impl SettingsState {
    /// 启动/打开前的初始状态：排除目录按引擎默认种一次。
    pub(crate) fn new(file: SettingsFile, data_dir: PathBuf) -> Self {
        Self {
            open: false,
            file,
            data_dir,
            autostart: None,
            autostart_approval: false,
            autostart_error: None,
            font_error: None,
            excluded: engine::config::default_config().excluded,
            palette_keys: vec!["⌥".to_owned(), "Space".to_owned()],
            window_keys: vec!["⌥".to_owned(), "⇧".to_owned(), "F".to_owned()],
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

    /// 取走录制取消挂起（`App::logic` 消费；快捷键 plan 接入后在此恢复旧热键）。
    pub(crate) fn take_cancel_recording_pending(&mut self) -> bool {
        std::mem::take(&mut self.cancel_recording_pending)
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
            palette_keys: state.palette_keys.clone(),
            window_keys: state.window_keys.clone(),
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
    }
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
/// 录制取消挂起由 `App::logic` 消费（快捷键 plan 接入点，当前无录制状态）。
fn hide(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context) {
    lock_shared(shared).settings.mark_hidden();
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    // 唤醒根视口消费取消录制挂起（根窗隐藏时也尽力排一帧；无录制态时仅留痕）。
    ctx.request_repaint_of(egui::ViewportId::ROOT);
    log::info!("设置窗口隐藏(open=false;录制取消挂起)");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{FontSize, SettingsFile};

    fn test_state() -> SettingsState {
        SettingsState::new(
            SettingsFile::default(),
            PathBuf::from("/tmp/mace-everything-settings-test"),
        )
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
        assert_eq!(state.palette_keys, vec!["⌥".to_owned(), "Space".to_owned()]);
        assert_eq!(
            state.window_keys,
            vec!["⌥".to_owned(), "⇧".to_owned(), "F".to_owned()]
        );
        assert_eq!(state.tray_autostart_sync, None);
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

    /// 字号纯状态：保存失败也更新 file.font_size + 留内联错误；成功清错误。
    /// 持久化单独验证（生产 `set_scale` 不在此测试，避免写全局原子）。
    #[test]
    fn font_size_state_records_error_and_persists_separately() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = SettingsState::new(SettingsFile::default(), dir.path().to_path_buf());

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

    /// 快照与运行期真源一致（语言/键帽/目录/错误/审批均复制）。
    #[test]
    fn snapshot_copies_state_for_painting() {
        let mut state = test_state();
        state.file.font_size = FontSize::Large;
        state.autostart = Some(false);
        state.autostart_approval = true;
        state.autostart_error = Some("未打包".into());
        state.font_error = Some("保存失败".into());
        state.palette_keys.push("K".into());
        state.excluded = vec!["node_modules".into()];

        let data = SettingsData::from_state(&state);
        assert_eq!(data.font_size, FontSize::Large);
        assert!(!data.autostart, "审批中显示为关");
        assert!(data.autostart_approval);
        assert_eq!(data.autostart_error.as_deref(), Some("未打包"));
        assert_eq!(data.font_error.as_deref(), Some("保存失败"));
        assert_eq!(data.language, "简体中文");
        assert_eq!(data.palette_keys, state.palette_keys);
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
