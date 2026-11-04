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
    session: Session,
    query_tx: Sender<(u64, Vec<HitEntry>)>,
    query_rx: Receiver<(u64, Vec<HitEntry>)>,
    /// 浮层自己的代际号:空输入/关闭/重新打开时递增,作废在途响应
    /// (Session 的代际号不可读,且清空输入后不会再有新查询可用来抬高它)。
    gen: u64,
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
            session: Session::new(DEBOUNCE),
            query_tx,
            query_rx,
            gen: 0,
        }
    }

    /// 打开浮层(清空上一次输入/结果/提示)。
    pub fn open(&mut self) {
        self.visible = true;
        self.gen = self.gen.wrapping_add(1); // 作废上一轮在途响应
        self.session = Session::new(DEBOUNCE);
        self.input.clear();
        self.results.clear();
        self.selected = 0;
        self.clipboard_hint = false;
        self.commit_requested = false;
        self.dismiss_requested = false;
        // 首帧内容:平台层 orderFront 后须有一次本视口的 pass,否则窗内是旧帧/空白。
        self.ctx.request_repaint_of(overlay_viewport_id());
    }

    #[allow(dead_code)] // Task 14 接线层调用(Esc/面板关闭/宿主切换)。
    pub fn close(&mut self) {
        self.visible = false;
        self.gen = self.gen.wrapping_add(1); // 关闭后在途响应不得回填
        self.session = Session::new(DEBOUNCE);
        self.input.clear();
        self.results.clear();
        self.selected = 0;
        self.clipboard_hint = false;
    }

    /// 输入变更 → 防抖会话;清空输入立即清结果并作废在途响应。
    pub fn on_input(&mut self, now: Instant) {
        self.session.on_input(&self.input, now);
        if self.input.is_empty() {
            self.gen = self.gen.wrapping_add(1);
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
            self.gen = self.gen.wrapping_add(1);
            let generation = self.gen;
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
            if generation != self.gen {
                continue; // 旧代响应丢弃(慢响应后到不得覆盖新结果)
            }
            self.results = entries;
            self.selected = 0;
        }
    }

    /// 当前选中目录完整路径。
    #[allow(dead_code)] // Task 14 接线层读取。
    pub fn selected_path(&self) -> Option<String> {
        self.results.get(self.selected).map(|e| e.path.clone())
    }
}

/// 绘制(ui/mod.rs 的 show_viewport_deferred 回调调用;短锁取 state 后绘制)。
/// 键盘:↑↓ 改选中、Enter 置 commit_requested、Esc 置 dismiss_requested;
/// 行点击选中、双击 = 选中 + commit_requested。
pub fn show_overlay(ui: &mut egui::Ui, st: &mut OverlayState) {
    let strings = i18n::s();
    ui.add_space(8.0);

    let input = ui.add(
        egui::TextEdit::singleline(&mut st.input)
            .hint_text(strings.overlay_hint)
            .desired_width(f32::INFINITY),
    );
    if input.changed() {
        st.on_input(Instant::now());
    }
    input.request_focus();

    // 键盘动作:↑↓ 选中、Enter 提交、Esc 取消(原始按键读取,不受输入框消费影响)。
    let (up, down, enter, escape) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowUp),
            i.key_pressed(egui::Key::ArrowDown),
            i.key_pressed(egui::Key::Enter),
            i.key_pressed(egui::Key::Escape),
        )
    });
    if !st.results.is_empty() {
        if down {
            st.selected = (st.selected + 1).min(st.results.len() - 1);
        }
        if up {
            st.selected = st.selected.saturating_sub(1);
        }
        if enter {
            st.commit_requested = true;
        }
    }
    if escape {
        st.dismiss_requested = true;
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
                if response.clicked() {
                    clicked_row = Some(i);
                }
                if response.double_clicked() {
                    clicked_row = Some(i);
                    commit = true;
                }
            }
        });
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

    /// 关闭/重新打开必须作废在途响应,并清空输入、结果与提示。
    #[test]
    fn reopen_resets_and_invalidates_inflight() {
        let mut st = state();
        st.open();
        st.input = "报告".to_owned();
        let stale_gen = st.gen;
        st.results = vec![HitEntry {
            name: "a".into(),
            path: "/a".into(),
            size: None,
            mtime: None,
            is_dir: true,
        }];
        st.selected = 1;
        st.clipboard_hint = true;
        st.close();
        assert!(!st.visible);
        assert!(st.input.is_empty());
        assert!(st.results.is_empty());
        assert_eq!(st.selected, 0);
        assert!(!st.clipboard_hint);
        assert_ne!(st.gen, stale_gen, "关闭后旧代响应应作废");
    }

    /// 清空输入立即清结果并推进代际号(旧响应不得回填)。
    #[test]
    fn empty_input_clears_results_and_invalidates() {
        let mut st = state();
        st.open();
        st.input = "报".to_owned();
        let before = st.gen;
        st.on_input(Instant::now());
        assert_eq!(st.gen, before, "非空输入不推进代际号");
        st.input.clear();
        st.on_input(Instant::now());
        assert!(st.results.is_empty());
        assert_ne!(st.gen, before, "清空输入须作废在途响应");
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
