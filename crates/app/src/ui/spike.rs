//! TEMP SPIKE(不提交):放大字形图集 → 等 30s 缩窗定时器清图集 → 外部 vmmap 对比。
//!
//! 时间线:1.5s 布局大量字形(ASCII×48 字号 + 3000 个汉字×3 字号);
//! 生产定时器(MCE_SURFACE_SHRINK_SECS=8)在 8s 缩窗+清图集并请求回收帧;
//! 16s 再显悬浮条(测首帧延迟);18s 重新布局(验证图集可重建);22s 结束。

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use eframe::egui;

use super::{palette_show, show_root_window, App};

pub(crate) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("MCE_SPIKE").is_some())
}

struct State {
    start: Instant,
    step: u32,
    show_at: Option<Instant>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn wall_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

fn grow_atlas(ctx: &egui::Context) {
    ctx.fonts_mut(|f| {
        let ascii: String = (0x20u8..0x7F).map(char::from).collect();
        for i in 0..48u32 {
            let _ = f.layout_no_wrap(
                ascii.clone(),
                egui::FontId::proportional(12.0 + i as f32),
                egui::Color32::WHITE,
            );
        }
        let cjk: String = (0x4E00u32..0x4E00 + 3000)
            .filter_map(char::from_u32)
            .collect();
        for size in [14.0, 15.0, 16.0] {
            let _ = f.layout_no_wrap(
                cjk.clone(),
                egui::FontId::proportional(size),
                egui::Color32::WHITE,
            );
        }
        log::info!("SPIKE atlas after grow: {:?}", f.font_image_size());
    });
}

pub(crate) fn tick(app: &App, ctx: &egui::Context) {
    if !enabled() {
        return;
    }
    let now = Instant::now();
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Some(State {
            start: now,
            step: 0,
            show_at: None,
        });
        log::info!("SPIKE start wall_ms={}", wall_ms());
    }
    let st = guard.as_mut().unwrap();
    let t = now.duration_since(st.start).as_millis();
    match st.step {
        0 if t >= 1500 => {
            grow_atlas(ctx);
            log::info!("SPIKE step 0 wall_ms={}", wall_ms());
            st.step = 1;
        }
        1 if t >= 3000 => {
            log::info!(
                "SPIKE atlas at 3s: {:?} wall_ms={}",
                ctx.fonts(|f| f.font_image_size()),
                wall_ms()
            );
            st.step = 2;
        }
        2 if t >= 10000 => {
            log::info!(
                "SPIKE atlas at 10s (after reclaim frame): {:?} wall_ms={}",
                ctx.fonts(|f| f.font_image_size()),
                wall_ms()
            );
            st.step = 3;
        }
        3 if t >= 16000 => {
            st.show_at = Some(now);
            let ok = palette_show(&app.shared, ctx);
            log::info!("SPIKE show palette -> {ok} wall_ms={}", wall_ms());
            st.step = 4;
        }
        4 if t >= 18000 => {
            grow_atlas(ctx);
            show_root_window(ctx);
            log::info!("SPIKE regrow+show root wall_ms={}", wall_ms());
            st.step = 5;
        }
        5 if t >= 22000 => {
            log::info!(
                "SPIKE atlas at 22s: {:?} wall_ms={}",
                ctx.fonts(|f| f.font_image_size()),
                wall_ms()
            );
            st.step = 6;
        }
        _ => {}
    }
    if st.step < 6 {
        let next = [1500u64, 3000, 10000, 16000, 18000, 22000]
            .get(st.step as usize)
            .copied()
            .unwrap_or(22000);
        ctx.request_repaint_after(Duration::from_millis(next.saturating_sub(t as u64).max(1)));
    }
}

pub(crate) fn note_palette_drawn() {
    if !enabled() {
        return;
    }
    let now = Instant::now();
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(st) = guard.as_mut() {
        if let Some(at) = st.show_at.take() {
            log::info!(
                "SPIKE palette first frame: {}ms wall_ms={}",
                now.duration_since(at).as_millis(),
                wall_ms()
            );
        }
    }
}
