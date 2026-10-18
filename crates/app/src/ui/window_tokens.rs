//! 完整结果窗口设计 token（`floatbar-visual/visual.md` §4.2 / §4.3 / §4.4 / §4.5）。
//!
//! Task 1 一次建全表，Task 2–4 逐段接入消费（搜索行 / 表头与表格 / 状态栏）。
//! 数值真源 = 规格表；接入前由本文件单测逐项钉住，防手滑改错。
//!
//! 规则同悬浮条 token：绘制代码只读这里，禁散落色值/字号/几何值
//! （visual.md §6.6「视图层不写条件分支」）。

use eframe::egui;

/// `#RRGGBBAA` → `Color32`。
fn c(hex: u32) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        ((hex >> 24) & 0xFF) as u8,
        ((hex >> 16) & 0xFF) as u8,
        ((hex >> 8) & 0xFF) as u8,
        (hex & 0xFF) as u8,
    )
}

/// 完整结果窗口全套颜色（§4.2 逐行 + 标题栏齿轮两枚）。
#[derive(Clone, Copy)]
pub struct FullWindowPalette {
    pub window_fill: egui::Color32,
    /// §4.2 窗口内描边（Task 4 起由内容层整窗内描边消费）。
    pub window_stroke: egui::Color32,
    /// §4.2 原生阴影参考值：由系统窗口阴影提供，不自绘；保留供规格对账。
    #[allow(dead_code)] // 原生阴影不由应用绘制，字段仅供 §4.2 表对账（Task 4 定案）。
    pub window_shadow: egui::Color32,
    pub divider: egui::Color32,
    pub search_fill: egui::Color32,
    pub search_icon: egui::Color32,
    /// 搜索框空词 hint（§4.2 表外补充：查询文本色的弱化值）。
    pub search_hint: egui::Color32,
    pub query_text: egui::Color32,
    pub header_text: egui::Color32,
    pub sort_arrow: egui::Color32,
    pub row_selected: egui::Color32,
    pub file_name: egui::Color32,
    /// 大小 / 修改时间 / 路径三列共用的次要文本色。
    pub column_text: egui::Color32,
    pub status_fill: egui::Color32,
    pub status_text: egui::Color32,
    /// 状态栏错误提示（排序未生效 / 查询失败等；§4.2 表外新增，与悬浮条
    /// `badge_error_text` 同源——同一错误红在两种底上各取一值）。
    pub error_text: egui::Color32,
    pub gap_badge_fill: egui::Color32,
    /// 缺口徽标文字与图标同色（§4.2 末行）。
    pub gap_badge_text: egui::Color32,
    /// 标题栏齿轮图标（settings-visual.md §3.1；§4.2 表外补充）。
    pub title_icon: egui::Color32,
    /// 标题栏齿轮悬停底（settings-visual.md §3.1「实现态」）。
    pub title_hover_fill: egui::Color32,
}

pub fn dark() -> FullWindowPalette {
    FullWindowPalette {
        window_fill: c(0x1A1D22FF),
        window_stroke: c(0xFFFFFF1F),
        window_shadow: c(0x000000B3),
        divider: c(0xFFFFFF14),
        search_fill: c(0xFFFFFF14),
        search_icon: c(0x8B929AFF),
        search_hint: c(0x8B929AFF),
        query_text: c(0xF2F4F7FF),
        header_text: c(0x8B929AFF),
        sort_arrow: c(0xC6CBD1FF),
        row_selected: c(0xFFFFFF1F),
        file_name: c(0xE4E7EBFF),
        column_text: c(0x8B929AFF),
        status_fill: c(0xFFFFFF0A),
        status_text: c(0x8B929AFF),
        error_text: c(0xF87171FF),
        gap_badge_fill: c(0xF59E0B29),
        gap_badge_text: c(0xF0B357FF),
        title_icon: c(0xC6CBD1FF),
        title_hover_fill: c(0xFFFFFF1F),
    }
}

pub fn light() -> FullWindowPalette {
    FullWindowPalette {
        window_fill: c(0xFFFFFFFF),
        window_stroke: c(0x0F172A1F),
        window_shadow: c(0x1E293B38),
        divider: c(0x00000012),
        search_fill: c(0x0000000A),
        search_icon: c(0x6B7280FF),
        search_hint: c(0x6B7280FF),
        query_text: c(0x1A1D22FF),
        header_text: c(0x6B7280FF),
        sort_arrow: c(0x4B5563FF),
        row_selected: c(0x5B6FD91F),
        file_name: c(0x1F242BFF),
        column_text: c(0x5F6774FF),
        status_fill: c(0x00000005),
        status_text: c(0x5F6774FF),
        error_text: c(0xB91C1CFF),
        gap_badge_fill: c(0xF59E0B1F),
        gap_badge_text: c(0xB45309FF),
        title_icon: c(0x4B5563FF),
        title_hover_fill: c(0x0000000F),
    }
}

/// 按主题取整表（每帧一次调用，按值返回即可）。
pub fn palette(is_dark: bool) -> FullWindowPalette {
    if is_dark {
        dark()
    } else {
        light()
    }
}

// —— 字号（§4.3，逻辑点）——
//
// **全部为基准点（base pt），不是最终 pt**：绘制与测量必须经 `font::size` /
// `font::id` 乘全局倍率（设置 · 字号）；几何尺寸不随字号缩放。
pub const FONT_QUERY: f32 = 14.0;
pub const FONT_FILENAME: f32 = 13.0;
/// 表头 / 大小 / 修改时间 / 路径 / 空态副文案。
pub const FONT_TABLE: f32 = 12.0;
/// 状态栏 / 缺口徽标。
pub const FONT_STATUS: f32 = 11.0;
/// 空态主文案（floatbar-visual/visual.md §2.7：14pt，取文件名色）。
pub const FONT_EMPTY_TITLE: f32 = 14.0;
/// 空态主副文案间距（§2.7：4）。
pub const EMPTY_TEXT_GAP: f32 = 4.0;

// —— 标题栏（§3.1 / §4.5；齿轮取 settings-visual.md §3.1）——
pub const TITLEBAR_H: f32 = 32.0;
/// 标题栏左右内边距（§4.5：20）。
pub const TITLEBAR_PAD_X: f32 = 20.0;
/// 左侧原生交通灯预留宽：约 80，不绘制任何内容。
pub const TRAFFIC_LIGHTS_RESERVE: f32 = 80.0;
pub const GEAR_SIZE: f32 = 28.0;
pub const GEAR_ICON: f32 = 16.0;
pub const GEAR_RADIUS: u8 = 7;

// —— 搜索行（§3.3；Task 2 接入）——
pub const SEARCH_ROW_H: f32 = 52.0;
pub const SEARCH_BOX_H: f32 = 36.0;
pub const SEARCH_BOX_RADIUS: u8 = 8;
/// 搜索框内边距：上下 / 左右（§3.3 [0,14]）。
pub const SEARCH_BOX_PAD: (f32, f32) = (0.0, 14.0);
/// 搜索框内子元素间距（图标与查询区）。
pub const SEARCH_BOX_GAP: f32 = 10.0;
pub const SEARCH_ICON: f32 = 16.0;

// —— 表头 / 表格（§3.4 / §3.5；Task 3 接入）——
pub const HEADER_H: f32 = 32.0;
pub const ROW_H: f32 = 30.0;
/// 搜索行 / 表头 / 行 / 状态栏共用的左右内边距。
pub const CONTENT_PAD_X: f32 = 16.0;
/// 表头 / 行内的列间距。
pub const COLUMN_GAP: f32 = 16.0;
pub const ROW_ICON: f32 = 16.0;
/// 名称 cell 内图标与文件名的间距。
pub const ICON_NAME_GAP: f32 = 8.0;
pub const COL_SIZE_W: f32 = 96.0;
pub const COL_MTIME_W: f32 = 160.0;
pub const COL_PATH_W: f32 = 300.0;
pub const DIVIDER_H: f32 = 1.0;
/// 表头排序箭头（§3.4 / §5.2 `chevron-up` / `chevron-down`）。
pub const SORT_ARROW: f32 = 12.0;
/// 表头文字与排序箭头的间距（§4.5：6）。
pub const SORT_ARROW_GAP: f32 = 6.0;
/// 表头列间竖线的上下内缩（§3.4，2026-10-18：不顶头）。
pub const HEADER_SEP_INSET_Y: f32 = 8.0;

// —— 状态栏（§3.6）——
pub const STATUS_H: f32 = 28.0;
pub const STATUS_GAP: f32 = 16.0;
pub const GAP_BADGE_RADIUS: u8 = 6;
/// 缺口徽标内边距：上下 / 左右（§3.6 [3,10]）。
pub const GAP_BADGE_PAD: (f32, f32) = (3.0, 10.0);
pub const GAP_BADGE_GAP: f32 = 6.0;
pub const GAP_ICON: f32 = 12.0;

// —— 窗口（§3.2 / §4.4）——
/// 窗口圆角（原生窗口裁剪；内容层内描边复用同值，Task 4 起消费）。
pub const WINDOW_RADIUS: u8 = 12;
/// 窗口内描边宽（§4.2 window_stroke；内容层整窗绘制，Task 4 起消费）。
pub const WINDOW_STROKE: f32 = 1.0;

#[cfg(test)]
mod tests {
    use super::*;

    /// 钉住：深/浅两套颜色与 §4.2 表逐项相等。
    #[test]
    fn full_window_palette_matches_spec_table() {
        let rows: [(egui::Color32, egui::Color32, u32, u32, &str); 17] = [
            (
                dark().window_fill,
                light().window_fill,
                0x1A1D22FF,
                0xFFFFFFFF,
                "窗口底",
            ),
            (
                dark().window_stroke,
                light().window_stroke,
                0xFFFFFF1F,
                0x0F172A1F,
                "窗口描边",
            ),
            (
                dark().window_shadow,
                light().window_shadow,
                0x000000B3,
                0x1E293B38,
                "窗口阴影",
            ),
            (
                dark().divider,
                light().divider,
                0xFFFFFF14,
                0x00000012,
                "分隔线",
            ),
            (
                dark().search_fill,
                light().search_fill,
                0xFFFFFF14,
                0x0000000A,
                "搜索框底",
            ),
            (
                dark().search_icon,
                light().search_icon,
                0x8B929AFF,
                0x6B7280FF,
                "搜索图标",
            ),
            (
                dark().query_text,
                light().query_text,
                0xF2F4F7FF,
                0x1A1D22FF,
                "查询文本",
            ),
            (
                dark().header_text,
                light().header_text,
                0x8B929AFF,
                0x6B7280FF,
                "表头文字",
            ),
            (
                dark().sort_arrow,
                light().sort_arrow,
                0xC6CBD1FF,
                0x4B5563FF,
                "排序箭头",
            ),
            (
                dark().row_selected,
                light().row_selected,
                0xFFFFFF1F,
                0x5B6FD91F,
                "选中行底",
            ),
            (
                dark().file_name,
                light().file_name,
                0xE4E7EBFF,
                0x1F242BFF,
                "文件名",
            ),
            (
                dark().column_text,
                light().column_text,
                0x8B929AFF,
                0x5F6774FF,
                "大小/修改时间/路径",
            ),
            (
                dark().status_fill,
                light().status_fill,
                0xFFFFFF0A,
                0x00000005,
                "状态栏底",
            ),
            (
                dark().status_text,
                light().status_text,
                0x8B929AFF,
                0x5F6774FF,
                "状态栏文本",
            ),
            (
                dark().error_text,
                light().error_text,
                0xF87171FF,
                0xB91C1CFF,
                "状态栏错误提示",
            ),
            (
                dark().gap_badge_fill,
                light().gap_badge_fill,
                0xF59E0B29,
                0xF59E0B1F,
                "缺口徽标底",
            ),
            (
                dark().gap_badge_text,
                light().gap_badge_text,
                0xF0B357FF,
                0xB45309FF,
                "缺口徽标文字/图标",
            ),
        ];
        for (dark_val, light_val, dark_hex, light_hex, name) in rows {
            assert_eq!(
                dark_val.to_array(),
                c(dark_hex).to_array(),
                "{name} 深色值不符 §4.2"
            );
            assert_eq!(
                light_val.to_array(),
                c(light_hex).to_array(),
                "{name} 浅色值不符 §4.2"
            );
        }
    }

    /// 钉住：搜索框空词 hint 色（§4.2 表外补充：查询文本的弱化值，
    /// 深/浅两主题均与搜索图标同灰度）。
    #[test]
    fn search_hint_is_weakened_query_text() {
        assert_eq!(dark().search_hint.to_array(), c(0x8B929AFF).to_array());
        assert_eq!(light().search_hint.to_array(), c(0x6B7280FF).to_array());
    }

    /// 钉住：标题栏齿轮两枚颜色（settings-visual.md §3.1）。
    #[test]
    fn gear_tokens_match_settings_spec() {
        assert_eq!(dark().title_icon.to_array(), c(0xC6CBD1FF).to_array());
        assert_eq!(light().title_icon.to_array(), c(0x4B5563FF).to_array());
        assert_eq!(dark().title_hover_fill.to_array(), c(0xFFFFFF1F).to_array());
        assert_eq!(
            light().title_hover_fill.to_array(),
            c(0x0000000F).to_array()
        );
    }

    /// 钉住：字号与几何常量（§4.3 / §4.5 / §3.x）。
    #[test]
    fn fonts_and_geometry_match_spec() {
        assert_eq!(
            (
                FONT_QUERY,
                FONT_FILENAME,
                FONT_TABLE,
                FONT_STATUS,
                FONT_EMPTY_TITLE,
                EMPTY_TEXT_GAP
            ),
            (14.0, 13.0, 12.0, 11.0, 14.0, 4.0)
        );

        assert_eq!(TITLEBAR_H, 32.0);
        assert_eq!(TITLEBAR_PAD_X, 20.0);
        assert_eq!(TRAFFIC_LIGHTS_RESERVE, 80.0);
        assert_eq!((GEAR_SIZE, GEAR_ICON, GEAR_RADIUS), (28.0, 16.0, 7));

        assert_eq!(SEARCH_ROW_H, 52.0);
        assert_eq!(SEARCH_BOX_H, 36.0);
        assert_eq!(SEARCH_BOX_RADIUS, 8);
        assert_eq!(SEARCH_BOX_PAD, (0.0, 14.0));
        assert_eq!(SEARCH_BOX_GAP, 10.0);
        assert_eq!(SEARCH_ICON, 16.0);

        assert_eq!(HEADER_H, 32.0);
        assert_eq!(ROW_H, 30.0);
        assert_eq!((CONTENT_PAD_X, COLUMN_GAP), (16.0, 16.0));
        assert_eq!((ROW_ICON, ICON_NAME_GAP), (16.0, 8.0));
        assert_eq!((COL_SIZE_W, COL_MTIME_W, COL_PATH_W), (96.0, 160.0, 300.0));
        assert_eq!((SORT_ARROW, SORT_ARROW_GAP), (12.0, 6.0));

        assert_eq!(STATUS_H, 28.0);
        assert_eq!(STATUS_GAP, 16.0);
        assert_eq!((GAP_BADGE_RADIUS, GAP_BADGE_GAP), (6, 6.0));
        assert_eq!(GAP_BADGE_PAD, (3.0, 10.0));
        assert_eq!(GAP_ICON, 12.0);

        assert_eq!((WINDOW_RADIUS, WINDOW_STROKE), (12, 1.0));

        // §3.2 高度推导：32 + 52 + 1 + 32 + 1 + 480 + 1 + 28 = 627。
        let fixed =
            TITLEBAR_H + SEARCH_ROW_H + DIVIDER_H + HEADER_H + DIVIDER_H + DIVIDER_H + STATUS_H;
        assert_eq!(fixed + 16.0 * ROW_H, 627.0);
    }
}
