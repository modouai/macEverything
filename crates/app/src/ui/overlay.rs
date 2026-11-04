//! 保存面板目录助手浮层视口:输入框 + 目录列表;↑↓/Enter/双击/Esc。
//! 查询复用悬浮条工作线程模式(palette.rs 查询段):防抖会话 + `client::query(dir_only=true)`
//! + mpsc 回投。daemon 离线不弹浮层由接线层守门,本模块只管已弹出后的体验。

use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui;
use protocol::HitEntry;

use client::DaemonPaths;

use crate::i18n;
use crate::session::Session;

/// 防抖窗口(与悬浮条同值 60ms)。
const DEBOUNCE: Duration = Duration::from_millis(60);
/// 单次查询上限:浮层列表短,无需滚动加载。
const QUERY_LIMIT: u32 = 30;

pub(crate) fn overlay_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("overlay")
}

/// 浮层尺寸(逻辑像素;窗口大小)。
pub const OVERLAY_W: f32 = 420.0;
pub const OVERLAY_H: f32 = 300.0;
pub const ROW_H: f32 = 26.0;

/// 按键决策涉及的键(纯逻辑用;绘制层从 `egui::Key` 映射)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayKey {
    Enter,
    Escape,
    ArrowDown,
    ArrowUp,
}

/// 按键决策结果(纯逻辑;IME 组合态下一律 None)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayKeyAction {
    /// 不处理(含 IME 组合/忙、空结果下的回车与方向键)。
    None,
    /// 提交当前选中项。
    Commit,
    /// 取消/关闭浮层。
    Dismiss,
    /// 移动到新选中下标(函数内已 clamp 到 `[0, result_count-1]`)。
    Select(usize),
}

/// 浮层按键决策(纯函数;IME 组合态下一律忽略)。
///
/// 语义:IME 组合中或本帧有 IME 事件 → None;Enter 且 `result_count > 0` → Commit
/// (空结果不提交);Escape → Dismiss;↑↓ 在结果非空时移动到 clamp 后的下标。
pub fn key_action(
    key: OverlayKey,
    selected: usize,
    result_count: usize,
    ime_composing: bool,
    ime_busy: bool,
) -> OverlayKeyAction {
    // 组合中的预编辑/选字键归输入法;选字回车与 ImeEvent::Commit 同帧到达,
    // 该帧整帧让位(与悬浮条同款防护)。
    if ime_composing || ime_busy {
        return OverlayKeyAction::None;
    }
    match key {
        OverlayKey::Enter if result_count > 0 => OverlayKeyAction::Commit,
        OverlayKey::Enter => OverlayKeyAction::None,
        OverlayKey::Escape => OverlayKeyAction::Dismiss,
        OverlayKey::ArrowDown if result_count > 0 => {
            OverlayKeyAction::Select(selected.saturating_add(1).min(result_count - 1))
        }
        OverlayKey::ArrowUp if result_count > 0 => {
            OverlayKeyAction::Select(selected.saturating_sub(1))
        }
        OverlayKey::ArrowDown | OverlayKey::ArrowUp => OverlayKeyAction::None,
    }
}

/// 浮层状态(Shared 持有)。
pub struct OverlayState {
    pub ctx: egui::Context,
    pub paths: DaemonPaths,
    pub visible: bool,
    pub input: String,
    pub results: Vec<HitEntry>,
    pub selected: usize,
    /// 最近一次 Shown 的面板 host pid(接线层 commit 时给 navigator 用)。
    #[allow(dead_code)] // Task 14 接线层写入/读取。
    pub panel_pid: Option<i32>,
    /// navigator 回退出口:路径已复制提示(design §5)。
    pub clipboard_hint: bool,
    /// 接线层请求标志(每帧消费后清零)。
    #[allow(dead_code)] // Task 14 接线层消费。
    pub commit_requested: bool,
    #[allow(dead_code)] // Task 14 接线层消费。
    pub dismiss_requested: bool,
    /// 输入法组合中(候选窗开着):不变更查询,Enter/Esc 归输入法。
    ime_composing: bool,
    /// 本帧有 IME 事件(预编辑/提交):选字回车与提交同帧到达,该帧键位归输入法。
    ime_busy: bool,
    /// 选中项变更后把该行滚入可见区(本帧消费后清零)。
    scroll_to_selected: bool,
    /// 查询会话(防抖 + 代际号唯一真相;在途响应以 `Session::accepts` 判定)。
    session: Session,
    query_tx: Sender<(u64, Vec<HitEntry>)>,
    query_rx: Receiver<(u64, Vec<HitEntry>)>,
}

impl OverlayState {
    pub fn new(ctx: egui::Context, paths: DaemonPaths) -> Self {
        // 通道一次创建、跨查询复用(与悬浮条同型:不在每次查询时新建 channel)。
        let (query_tx, query_rx) = std::sync::mpsc::channel();
        Self {
            ctx,
            paths,
            visible: false,
            input: String::new(),
            results: Vec::new(),
            selected: 0,
            panel_pid: None,
            clipboard_hint: false,
            commit_requested: false,
            dismiss_requested: false,
            ime_composing: false,
            ime_busy: false,
            scroll_to_selected: false,
            session: Session::new(DEBOUNCE),
            query_tx,
            query_rx,
        }
    }

    /// 打开浮层(清空上一次输入/结果/提示,复位全部瞬态标志)。
    pub fn open(&mut self) {
        self.visible = true;
        self.session.reset(); // 推进代际号,作废上一轮在途响应
        self.input.clear();
        self.results.clear();
        self.selected = 0;
        self.clipboard_hint = false;
        self.commit_requested = false;
        self.dismiss_requested = false;
        self.panel_pid = None;
        self.ime_composing = false;
        self.ime_busy = false;
        self.scroll_to_selected = false;
        // 首帧内容:平台层 orderFront 后须有一次本视口的 pass,否则窗内是旧帧/空白。
        self.ctx.request_repaint_of(overlay_viewport_id());
    }

    #[allow(dead_code)] // Task 14 接线层调用(Esc/面板关闭/宿主切换)。
    pub fn close(&mut self) {
        self.visible = false;
        self.session.reset(); // 关闭后在途响应不得回填
        self.input.clear();
        self.results.clear();
        self.selected = 0;
        self.clipboard_hint = false;
        self.commit_requested = false;
        self.dismiss_requested = false;
        self.panel_pid = None;
        self.ime_composing = false;
        self.ime_busy = false;
        self.scroll_to_selected = false;
    }

    /// 输入变更 → 防抖会话;清空输入立即清结果并作废在途响应。
    pub fn on_input(&mut self, now: Instant) {
        self.session.on_input(&self.input, now);
        if self.input.is_empty() {
            // 空词:Session::on_input 已推进代际号(在途旧代不被接受),这里只清展示。
            self.results.clear();
            self.selected = 0;
        } else {
            // 防抖到期帧(与悬浮条同因:输入帧的立即重绘会吞掉防抖排帧,
            // 此处显式补排,免查询拖到光标闪烁帧才发出)。
            self.ctx
                .request_repaint_after_for(DEBOUNCE, overlay_viewport_id());
        }
    }

    /// 每帧:发起到期查询 + 收回投结果(代际校验)。仿 palette 查询段。
    pub fn poll(&mut self, now: Instant) {
        // 在途防抖逐帧补排(根因见 `Session::pending_delay`:同帧 0ms 立即重绘
        // 请求会吞掉防抖请求;不补排则要等下一次偶然重绘才发查询)。
        if let Some(delay) = self.session.pending_delay(now) {
            self.ctx
                .request_repaint_after_for(delay, overlay_viewport_id());
        }
        if let Some(fire) = self.session.pending_at(now) {
            // 代际号由 Session 唯一持有:发起时取 fire.generation,回收时用 accepts 校验。
            let generation = fire.generation;
            let query = fire.query;
            let paths = self.paths.clone();
            let ctx = self.ctx.clone();
            let tx = self.query_tx.clone();
            // UI 线程不做阻塞查询(与悬浮条同一执行模型)。
            std::thread::spawn(move || {
                let entries = match client::query(&paths, &query, QUERY_LIMIT, None, false, true) {
                    Ok(out) => out.entries,
                    Err(err) => {
                        // 浮层不展示错误态:daemon 离线由接线层守门;查询级失败按空结果处理。
                        log::warn!("浮层目录查询失败:{}", err.message);
                        Vec::new()
                    }
                };
                let _ = tx.send((generation, entries));
                // 跨线程唤醒浮层视口(隐藏期不误唤醒根视口)。
                ctx.request_repaint_of(overlay_viewport_id());
            });
        }
        while let Ok((generation, entries)) = self.query_rx.try_recv() {
            // 以 Session 为唯一真相:清空输入/关闭重开/新查询都会推进其代际号。
            if !self.session.accepts(generation) {
                continue; // 旧代响应丢弃(慢响应后到不得覆盖新结果)
            }
            self.results = entries;
            self.selected = 0;
            self.scroll_to_selected = false;
        }
    }

    /// 当前选中目录完整路径。
    #[allow(dead_code)] // Task 14 接线层读取。
    pub fn selected_path(&self) -> Option<String> {
        self.results.get(self.selected).map(|e| e.path.clone())
    }

    /// 输入法组合态跟踪(与悬浮条同款):IME 组合中(候选窗开着)Enter/Esc 属输入法。
    /// 依据 `Event::Ime`:Preedit 非空 = 组合中;Preedit 空 / Commit = 组合结束。
    fn track_ime(&mut self, ui: &egui::Ui) {
        let (composing, busy) = ui.input(|i| {
            let mut composing = self.ime_composing;
            let mut busy = false; // 本帧有 IME 事件(预编辑或提交)
            for ev in &i.events {
                match ev {
                    egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => {
                        composing = !text.is_empty();
                        busy = true;
                    }
                    egui::Event::Ime(egui::ImeEvent::Commit(_)) => {
                        composing = false;
                        busy = true;
                    }
                    _ => {}
                }
            }
            (composing, busy)
        });
        self.ime_composing = composing;
        self.ime_busy = busy;
    }
}

/// 绘制(ui/mod.rs 的 show_viewport_deferred 回调调用;短锁取 state 后绘制)。
/// 键盘:↑↓ 改选中、Enter 置 commit_requested、Esc 置 dismiss_requested;
/// 行点击选中、双击 = 选中 + commit_requested。
pub fn show_overlay(ui: &mut egui::Ui, st: &mut OverlayState) {
    let strings = i18n::s();
    ui.add_space(8.0);

    // IME 组合态须在读取本帧事件前跟踪(选字回车与 Commit 同帧到达,见 track_ime)。
    st.track_ime(ui);

    let input = ui.add(
        egui::TextEdit::singleline(&mut st.input)
            .hint_text(strings.overlay_hint)
            .desired_width(f32::INFINITY),
    );
    // 组合中的预编辑文本不触发查询(IME 未上屏;提交帧 composing 已复位,照常查询)。
    if input.changed() && !st.ime_composing {
        st.on_input(Instant::now());
    }
    input.request_focus();

    // 键盘动作:IME 组合/忙时整帧让位输入法;其余交给纯函数决策。
    let (up, down, enter, escape) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowUp),
            i.key_pressed(egui::Key::ArrowDown),
            i.key_pressed(egui::Key::Enter),
            i.key_pressed(egui::Key::Escape),
        )
    });
    let result_count = st.results.len();
    for (pressed, key) in [
        (up, OverlayKey::ArrowUp),
        (down, OverlayKey::ArrowDown),
        (enter, OverlayKey::Enter),
        (escape, OverlayKey::Escape),
    ] {
        if !pressed {
            continue;
        }
        match key_action(
            key,
            st.selected,
            result_count,
            st.ime_composing,
            st.ime_busy,
        ) {
            OverlayKeyAction::None => {}
            OverlayKeyAction::Commit => st.commit_requested = true,
            OverlayKeyAction::Dismiss => st.dismiss_requested = true,
            OverlayKeyAction::Select(index) => {
                if index != st.selected {
                    st.selected = index;
                    st.scroll_to_selected = true; // 本帧把新选中行滚入可见区
                }
            }
        }
    }

    if st.clipboard_hint {
        ui.add_space(2.0);
        ui.add(egui::Label::new(egui::RichText::new(strings.overlay_clipboard_hint).weak()).wrap());
    }

    ui.add_space(4.0);
    let mut clicked_row = None;
    let mut commit = false;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            if st.results.is_empty() {
                if !st.input.is_empty() {
                    ui.add_space(6.0);
                    ui.add(egui::Label::new(
                        egui::RichText::new(strings.empty_no_match).weak(),
                    ));
                }
                return;
            }
            for (i, entry) in st.results.iter().enumerate() {
                let response = draw_row(ui, entry, i == st.selected);
                // 键盘移动后跟随滚动(与悬浮条同款):选中行必须留在可视区内。
                if i == st.selected && st.scroll_to_selected {
                    ui.scroll_to_rect(response.rect, None);
                }
                if response.clicked() {
                    clicked_row = Some(i);
                }
                if response.double_clicked() {
                    clicked_row = Some(i);
                    commit = true;
                }
            }
        });
    st.scroll_to_selected = false; // 本帧已消费(点击/查询回填同样不重复滚动)
    if let Some(i) = clicked_row {
        st.selected = i;
    }
    if commit {
        st.commit_requested = true;
    }
}

/// 单行:选中底色 + 「目录名 — 完整路径」(单行截断);整行可点击。
fn draw_row(ui: &mut egui::Ui, entry: &HitEntry, selected: bool) -> egui::Response {
    let width = ui.available_width();
    let fill = if selected {
        ui.visuals().selection.bg_fill
    } else {
        egui::Color32::TRANSPARENT
    };
    let response = egui::Frame::new()
        .fill(fill)
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.set_min_width((width - 12.0).max(0.0));
            ui.set_min_height(ROW_H - 4.0);
            ui.horizontal_centered(|ui| {
                ui.add(egui::Label::new(egui::RichText::new(&entry.name).strong()).truncate());
                ui.add(egui::Label::new(egui::RichText::new("—").weak()));
                ui.add(egui::Label::new(egui::RichText::new(&entry.path).weak()).truncate());
            });
        })
        .response;
    response.interact(egui::Sense::click())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> OverlayState {
        // 测试不发起真实查询:`poll` 仅在输入非空时才会 spawn;这里只测状态机。
        OverlayState::new(
            egui::Context::default(),
            DaemonPaths {
                data_dir: std::path::PathBuf::new(),
            },
        )
    }

    fn entry(name: &str, path: &str, is_dir: bool) -> HitEntry {
        HitEntry {
            name: name.into(),
            path: path.into(),
            size: None,
            mtime: None,
            is_dir,
        }
    }

    /// 按键决策表:IME 组合/忙整帧让位;空结果不提交/不选中;方向键在函数内 clamp。
    #[test]
    fn key_action_table() {
        const KEYS: [OverlayKey; 4] = [
            OverlayKey::Enter,
            OverlayKey::Escape,
            OverlayKey::ArrowDown,
            OverlayKey::ArrowUp,
        ];
        for key in KEYS {
            assert_eq!(
                key_action(key, 0, 3, true, false),
                OverlayKeyAction::None,
                "{key:?} 组合中一律忽略"
            );
            assert_eq!(
                key_action(key, 0, 3, false, true),
                OverlayKeyAction::None,
                "{key:?} IME 事件帧一律忽略"
            );
        }
        assert_eq!(
            key_action(OverlayKey::Enter, 0, 0, false, false),
            OverlayKeyAction::None,
            "空结果不提交"
        );
        assert_eq!(
            key_action(OverlayKey::Enter, 0, 1, false, false),
            OverlayKeyAction::Commit
        );
        assert_eq!(
            key_action(OverlayKey::Escape, 0, 0, false, false),
            OverlayKeyAction::Dismiss
        );
        assert_eq!(
            key_action(OverlayKey::ArrowDown, 0, 3, false, false),
            OverlayKeyAction::Select(1)
        );
        assert_eq!(
            key_action(OverlayKey::ArrowDown, 2, 3, false, false),
            OverlayKeyAction::Select(2),
            "↓到底 clamp 到 count-1"
        );
        assert_eq!(
            key_action(OverlayKey::ArrowUp, 2, 3, false, false),
            OverlayKeyAction::Select(1)
        );
        assert_eq!(
            key_action(OverlayKey::ArrowUp, 0, 3, false, false),
            OverlayKeyAction::Select(0),
            "↑到顶 clamp 0"
        );
        assert_eq!(
            key_action(OverlayKey::ArrowDown, 0, 0, false, false),
            OverlayKeyAction::None,
            "空结果不选中"
        );
    }

    /// 关闭必须作废在途响应,并清空输入、结果、提示与全部瞬态标志。
    #[test]
    fn reopen_resets_and_invalidates_inflight() {
        let mut st = state();
        st.open();
        st.input = "报告".to_owned();
        // 产生一个在途代际(验证 close 后作废)。
        let base = Instant::now();
        st.session.on_input("报告", base);
        let inflight = st
            .session
            .pending_at(base + DEBOUNCE)
            .expect("窗口到应发起")
            .generation;
        assert!(st.session.accepts(inflight));
        st.results = vec![entry("a", "/a", true)];
        st.selected = 1;
        st.clipboard_hint = true;
        st.commit_requested = true;
        st.dismiss_requested = true;
        st.panel_pid = Some(123);
        st.close();
        assert!(!st.visible);
        assert!(st.input.is_empty());
        assert!(st.results.is_empty());
        assert_eq!(st.selected, 0);
        assert!(!st.clipboard_hint);
        assert!(!st.commit_requested, "关闭须清提交请求");
        assert!(!st.dismiss_requested, "关闭须清取消请求");
        assert_eq!(st.panel_pid, None, "关闭须清面板 pid");
        assert!(!st.session.accepts(inflight), "关闭后旧代响应应作废");
    }

    /// 清空输入立即清结果并推进 Session 代际(旧响应不得回填)。
    #[test]
    fn empty_input_clears_results_and_invalidates() {
        let mut st = state();
        st.open();
        let base = Instant::now();
        st.input = "报".to_owned();
        st.on_input(base);
        // 发起一次查询取得基线代际(直接驱动会话,不 spawn 线程)。
        let baseline = st
            .session
            .pending_at(base + DEBOUNCE)
            .expect("窗口到应发起")
            .generation;
        assert!(st.session.accepts(baseline), "非空输入不推进代际号");
        // 先预置非空结果/选中:否则「清空」断言在恒空状态下恒真(假钉)。
        st.results = vec![entry("a", "/a", true), entry("b", "/b", true)];
        st.selected = 1;
        st.input.clear();
        st.on_input(base);
        assert!(st.results.is_empty());
        assert_eq!(st.selected, 0);
        assert!(!st.session.accepts(baseline), "清空输入须作废在途响应");
    }

    /// poll 只回填当前代际的响应:过期代际即使后到也不得覆盖当前结果。
    #[test]
    fn poll_discards_stale_generation() {
        let mut st = state();
        st.open();
        // 连发两代查询:第一代在第二代发出后即为过期代。
        let base = Instant::now();
        st.session.on_input("a", base);
        let stale = st
            .session
            .pending_at(base + DEBOUNCE)
            .expect("窗口到应发起")
            .generation;
        st.session.on_input("b", base);
        let current = st
            .session
            .pending_at(base + DEBOUNCE)
            .expect("窗口到应发起")
            .generation;
        assert_ne!(stale, current);
        // 过期代先到:不得回填。
        st.query_tx
            .send((stale, vec![entry("old", "/old", false)]))
            .unwrap();
        st.poll(base);
        assert!(st.results.is_empty(), "过期代结果不得回填");
        // 当前代随后到:回填并复位选中。
        st.selected = 2;
        st.query_tx
            .send((current, vec![entry("dir", "/dir", true)]))
            .unwrap();
        st.poll(base);
        assert_eq!(st.results.len(), 1);
        assert_eq!(st.results[0].path, "/dir");
        assert_eq!(st.selected, 0, "新结果到达复位选中");
    }

    /// 选中路径取当前行;越界/空结果返回 None。
    #[test]
    fn selected_path_tracks_selection() {
        let mut st = state();
        assert_eq!(st.selected_path(), None);
        st.results = vec![
            HitEntry {
                name: "a".into(),
                path: "/a".into(),
                size: None,
                mtime: None,
                is_dir: true,
            },
            HitEntry {
                name: "b".into(),
                path: "/b".into(),
                size: None,
                mtime: None,
                is_dir: true,
            },
        ];
        assert_eq!(st.selected_path().as_deref(), Some("/a"));
        st.selected = 1;
        assert_eq!(st.selected_path().as_deref(), Some("/b"));
        st.selected = 9;
        assert_eq!(st.selected_path(), None);
    }
}
