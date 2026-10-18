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

use eframe::egui;

use crate::autostart;
use crate::platform;
use crate::settings::{self as store, FontSize, SettingsFile};
use crate::ui::{
    font as ui_font, install_base_text_styles, lock_shared,
    settings_tokens::{
        palette as settings_palette, SETTINGS_H, SETTINGS_W, TITLEBAR_H, TITLE_FONT,
        TRAFFIC_LIGHTS_RESERVE, WINDOW_RADIUS, WINDOW_STROKE,
    },
    settings_view::{self, SettingsAction, SettingsData},
    Shared,
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
    /// 实际自启态；`None` = 尚未查询或查询失败。
    pub(crate) autostart: Option<bool>,
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
}

impl SettingsState {
    /// 启动/打开前的初始状态：排除目录按引擎默认种一次。
    pub(crate) fn new(file: SettingsFile, data_dir: PathBuf) -> Self {
        Self {
            open: false,
            file,
            data_dir,
            autostart: None,
            autostart_error: None,
            font_error: None,
            excluded: engine::config::default_config().excluded,
            palette_keys: vec!["⌥".to_owned(), "Space".to_owned()],
            window_keys: vec!["⌥".to_owned(), "⇧".to_owned(), "F".to_owned()],
            tray_autostart_sync: None,
        }
    }

    /// 打开设置页的自启查询结果：成功取实际态，失败按未知（显示关态）+ 内联错误。
    pub(crate) fn autostart_loaded(&mut self, result: Result<bool, String>) {
        match result {
            Ok(enabled) => {
                self.autostart = Some(enabled);
                self.autostart_error = None;
            }
            Err(err) => {
                self.autostart = None;
                self.autostart_error = Some(err);
            }
        }
    }

    /// 设置页自启切换成功：更新实际态、清错误、置托盘同步标记。
    pub(crate) fn autostart_succeeded(&mut self, enabled: bool) {
        self.autostart = Some(enabled);
        self.autostart_error = None;
        self.tray_autostart_sync = Some(enabled);
    }

    /// 设置页自启切换失败：保留旧状态，只落内联错误（开关显示实际/旧值）。
    pub(crate) fn autostart_failed(&mut self, error: String) {
        self.autostart_error = Some(error);
    }

    /// 字号选择的状态落地（纯状态；调用方已完成 `font::set_scale` 与保存）。
    /// 保存失败也更新选中态——界面须与已应用的全局倍率一致。
    pub(crate) fn set_font_size_result(&mut self, size: FontSize, save_error: Option<String>) {
        self.file.font_size = size;
        self.font_error = save_error;
    }

    /// 取走待同步给托盘的自启值（App::logic 每帧 drain）。
    pub(crate) fn take_tray_autostart_sync(&mut self) -> Option<bool> {
        self.tray_autostart_sync.take()
    }
}

/// 设置视口 pass（deferred 回调）。
pub fn draw(ui: &mut egui::Ui, shared: &Arc<Mutex<Shared>>) {
    let ctx = ui.ctx().clone();

    // 关闭=隐藏：红点（close_requested）与 ⌘W 同一路径。子视口须自行消费关闭请求
    //（根视口的 ⌘W 处理器不会为它运行）；隐藏不销毁视口，也不退出 App。
    let close_requested = ctx.input(|i| i.viewport().close_requested());
    let cmd_w = ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::W));
    if close_requested || cmd_w {
        hide(shared, &ctx);
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

    // 快照在锁内复制，绘制与动作执行都在锁外（自启/保存/装样式不持共享锁）。
    let data = {
        let s = lock_shared(shared);
        SettingsData::from_state(&s.settings)
    };
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

/// 自启开关即时生效：SMAppService 调用在锁外；成功队列托盘勾选同步。
fn apply_autostart(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context, enabled: bool) {
    let data_dir = {
        let s = lock_shared(shared);
        s.settings.data_dir.clone()
    };
    match autostart::apply_user_choice(&data_dir, enabled) {
        Ok(note) => {
            log::info!("设置页:开机自启 → {note}");
            {
                let mut s = lock_shared(shared);
                s.settings.autostart_succeeded(enabled);
            }
            ctx.request_repaint_of(viewport_id());
            // 让 App::logic 尽快 drain `tray_autostart_sync`（根窗可能隐藏）。
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
        Err(err) => {
            let message = format!("开机自启设置失败:{err:#}");
            log::warn!("{message}");
            {
                let mut s = lock_shared(shared);
                s.settings.autostart_failed(message);
            }
            ctx.request_repaint_of(viewport_id());
        }
    }
}

/// 隐藏设置窗口（红点关闭 / ⌘W 共用）：开合态置 false + 下发隐藏命令。
/// 命令只入 egui 队列，不销毁视口——再次打开仍是同一实例。
fn hide(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context) {
    lock_shared(shared).settings.open = false;
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    log::info!("设置窗口隐藏(open=false)");
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
        assert!(state.autostart_error.is_none());
        assert!(state.font_error.is_none());
        assert_eq!(state.excluded, engine::config::default_config().excluded);
        assert_eq!(state.palette_keys, vec!["⌥".to_owned(), "Space".to_owned()]);
        assert_eq!(
            state.window_keys,
            vec!["⌥".to_owned(), "⇧".to_owned(), "F".to_owned()]
        );
        assert_eq!(state.tray_autostart_sync, None);
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

    /// 自启查询失败 → 未知（显示关态）+ 内联错误；成功 → 实际态并清错误。
    #[test]
    fn autostart_load_updates_known_state_or_error() {
        let mut state = test_state();
        state.autostart = Some(true);
        state.autostart_loaded(Err("读取开机自启状态失败".into()));
        assert_eq!(state.autostart, None, "查询失败按未知，开关显示关态");
        assert_eq!(
            state.autostart_error.as_deref(),
            Some("读取开机自启状态失败")
        );

        state.autostart_loaded(Ok(true));
        assert_eq!(state.autostart, Some(true));
        assert!(state.autostart_error.is_none());
    }

    /// 自启成功：更新状态 + 置/取托盘同步标记；失败：保持旧值 + 错误、不同步托盘。
    #[test]
    fn autostart_transitions_sync_tray_once() {
        let mut state = test_state();
        state.autostart = Some(false);
        state.autostart_error = Some("旧错误".into());

        state.autostart_succeeded(true);
        assert_eq!(state.autostart, Some(true));
        assert!(state.autostart_error.is_none());
        assert_eq!(state.take_tray_autostart_sync(), Some(true));
        assert_eq!(
            state.take_tray_autostart_sync(),
            None,
            "同步标记只能被消费一次"
        );

        state.autostart = Some(true);
        state.autostart_failed("开机自启设置失败:未打包".into());
        assert_eq!(state.autostart, Some(true), "失败必须保持旧状态");
        assert_eq!(
            state.autostart_error.as_deref(),
            Some("开机自启设置失败:未打包")
        );
        assert_eq!(state.take_tray_autostart_sync(), None, "失败不得同步托盘");
    }

    /// 快照与运行期真源一致（语言/键帽/目录/错误均复制）。
    #[test]
    fn snapshot_copies_state_for_painting() {
        let mut state = test_state();
        state.file.font_size = FontSize::Large;
        state.autostart = None;
        state.autostart_error = Some("未打包".into());
        state.font_error = Some("保存失败".into());
        state.palette_keys.push("K".into());
        state.excluded = vec!["node_modules".into()];

        let data = SettingsData::from_state(&state);
        assert_eq!(data.font_size, FontSize::Large);
        assert!(!data.autostart, "未知自启态显示为关");
        assert_eq!(data.autostart_error.as_deref(), Some("未打包"));
        assert_eq!(data.font_error.as_deref(), Some("保存失败"));
        assert_eq!(data.language, "简体中文");
        assert_eq!(data.palette_keys, state.palette_keys);
        assert_eq!(data.excluded, vec!["node_modules".to_owned()]);
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
