//! 设置窗口设计 token（`floatbar-visual/settings-visual.md` §2.2 / §2.4 / §2.5）。
//!
//! Task 2 一次建全表，Task 4/5 接入消费（标题栏与分组 / 控件 / 目录列表）。
//! 数值真源 = 规格表；接入前由本文件单测逐项钉住，防手滑改错。
//!
//! 规则同悬浮条 / 完整窗口 token：绘制代码只读这里，禁散落色值/字号/几何值
//! （visual.md §6.6「视图层不写条件分支」）。
//!
//! 字号 token（`*_FONT`）均为**基准点**（base pt）：Task 3 的全局字号倍率落地后，
//! 绘制时必须经 `font::size(base)` 换算，勿直接使用常量；几何尺寸不随字号缩放。
//!
//! 窗口圆角 / 内描边 / 交通灯交互预留沿用完整结果窗口的 `window_tokens`
//! （单点定义），本文件只 re-export 供设置窗口消费。

// token 表按规格一次建全、消费方分 Task 接入；测试逐项钉住数值。
// 仅「规格对账」、应用永不读取的参考值已逐条 `#[allow(dead_code)]`；其余 token
// 在 Task 4/5 接入后即被消费，届时可移除本模块级 allow。
#![allow(dead_code)]

use eframe::egui;

// 设置窗口与完整结果窗口共用同一套窗口圆角/描边/交通灯交互预留（单点定义）。
pub use super::window_tokens::{TRAFFIC_LIGHTS_RESERVE, WINDOW_RADIUS, WINDOW_STROKE};

/// `#RRGGBBAA` → `Color32`。
fn c(hex: u32) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(
        ((hex >> 24) & 0xFF) as u8,
        ((hex >> 16) & 0xFF) as u8,
        ((hex >> 8) & 0xFF) as u8,
        (hex & 0xFF) as u8,
    )
}

/// 设置窗口全套颜色（settings-visual.md §2.5 逐行 + 错误提示色）。
#[derive(Debug, Clone, Copy)]
pub struct SettingsPalette {
    /// 窗口底（标题栏与内容同底）。
    pub window_fill: egui::Color32,
    /// 1px 内描边（内容层整窗绘制；圆角裁剪由原生窗口负责）。
    pub window_stroke: egui::Color32,
    /// 规格对账专用：原生窗口阴影由系统提供，应用不读取。
    #[allow(dead_code)]
    pub window_shadow: egui::Color32,
    /// 标题「设置」/ 行标签（§2.5「标题 / 行标签」）。
    pub text_primary: egui::Color32,
    /// 分组标题 / 辅助说明 / 次要图标（§2.5 同色）。
    pub text_secondary: egui::Color32,
    /// 分组卡片底。
    pub card_fill: egui::Color32,
    /// 行间分隔线（1px）。
    pub divider: egui::Color32,
    /// 控件底（分段容器 / 下拉字段等未选中态的共用底色）。
    pub control_fill: egui::Color32,
    /// 控件文字。
    pub control_text: egui::Color32,
    /// 字号分段：选中段底。
    pub segment_selected_fill: egui::Color32,
    /// 字号分段：选中段文字。
    pub segment_selected_text: egui::Color32,
    /// 字号分段：未选中段文字（未选中段无独立底色，透出控件底）。
    pub segment_unselected_text: egui::Color32,
    /// 开关开态（系统蓝；后续若跟随 macOS 强调色则由系统提供）。
    pub switch_on: egui::Color32,
    /// 开关钮（两主题均为白色）。
    pub switch_knob: egui::Color32,
    /// 开关钮 1px 柔和投影（§2.4 表外补充：黑色 20%，两主题同值）。
    pub switch_knob_shadow: egui::Color32,
    /// 键帽底。
    pub keycap_fill: egui::Color32,
    /// 键帽文字。
    pub keycap_text: egui::Color32,
    /// 「＋ 添加目录…」按钮底。
    pub add_button_fill: egui::Color32,
    /// 「＋ 添加目录…」按钮文字。
    pub add_button_text: egui::Color32,
    /// 排除目录路径（§2.5 单列；与主文字同值）。
    pub dir_path: egui::Color32,
    /// 移除图标 ×。
    pub remove_icon: egui::Color32,
    /// 错误提示（settings-visual.md §2.5 表外：与完整窗口
    /// `FullWindowPalette::error_text` 同源——同一错误红在同类底上取同值）。
    pub error_text: egui::Color32,
}

pub fn dark() -> SettingsPalette {
    SettingsPalette {
        window_fill: c(0x1A1D22FF),
        window_stroke: c(0xFFFFFF1F),
        window_shadow: c(0x000000B3),
        text_primary: c(0xE4E7EBFF),
        text_secondary: c(0x8B929AFF),
        card_fill: c(0xFFFFFF0A),
        divider: c(0xFFFFFF14),
        control_fill: c(0xFFFFFF14),
        control_text: c(0xF2F4F7FF),
        segment_selected_fill: c(0xFFFFFF29),
        segment_selected_text: c(0xFFFFFFFF),
        segment_unselected_text: c(0x8B929AFF),
        switch_on: c(0x0A84FFFF),
        switch_knob: c(0xFFFFFFFF),
        switch_knob_shadow: c(0x00000033),
        keycap_fill: c(0xFFFFFF1A),
        keycap_text: c(0xC6CBD1FF),
        add_button_fill: c(0xFFFFFF1A),
        add_button_text: c(0xC6CBD1FF),
        dir_path: c(0xE4E7EBFF),
        remove_icon: c(0x8B929AFF),
        error_text: c(0xF87171FF),
    }
}

pub fn light() -> SettingsPalette {
    SettingsPalette {
        window_fill: c(0xFFFFFFFF),
        window_stroke: c(0x0F172A1F),
        window_shadow: c(0x1E293B38),
        text_primary: c(0x1F242BFF),
        text_secondary: c(0x6B7280FF),
        card_fill: c(0x00000005),
        divider: c(0x00000012),
        control_fill: c(0x0000000A),
        control_text: c(0x1A1D22FF),
        segment_selected_fill: c(0xFFFFFFFF),
        segment_selected_text: c(0x1F242BFF),
        segment_unselected_text: c(0x6B7280FF),
        switch_on: c(0x007AFFFF),
        switch_knob: c(0xFFFFFFFF),
        switch_knob_shadow: c(0x00000033),
        keycap_fill: c(0x0000000D),
        keycap_text: c(0x4B5563FF),
        add_button_fill: c(0x0000000D),
        add_button_text: c(0x4B5563FF),
        dir_path: c(0x1F242BFF),
        remove_icon: c(0x6B7280FF),
        error_text: c(0xB91C1CFF),
    }
}

/// 按主题取整表（每帧一次调用，按值返回即可）。
pub fn palette(is_dark: bool) -> SettingsPalette {
    if is_dark {
        dark()
    } else {
        light()
    }
}

// —— 窗口 / 标题栏（§2.1 / §2.2）——
// 设置窗口尺寸由本文件单点拥有（`ui/settings.rs` 从这里引用）。
pub const SETTINGS_W: f32 = 620.0;
pub const SETTINGS_H: f32 = 600.0;

/// 设置窗口专用自绘标题栏高（44；勿与完整结果窗口的 32 混用）。
pub const TITLEBAR_H: f32 = 44.0;
/// 标题栏左右内边距（§2.1；规格对账专用：标题居中、交通灯由原生绘制）。
#[allow(dead_code)]
pub const TITLEBAR_PAD_X: f32 = 20.0;
/// 标题「设置」字号（§2.1：13pt）。
pub const TITLE_FONT: f32 = 13.0;
/// 交通灯单枚直径（§2.2；规格对账专用：原生交通灯不由应用绘制）。
#[allow(dead_code)]
pub const TRAFFIC_LIGHT_SIZE: f32 = 12.0;
/// 交通灯间距（§2.2；规格对账专用）。
#[allow(dead_code)]
pub const TRAFFIC_LIGHT_GAP: f32 = 8.0;
/// 标题右侧弹性占位（§2.2「Right Spacer (52)」；规格对账专用：
/// 与交通灯组等宽以保持标题居中，由灯组尺寸推导）。
#[allow(dead_code)]
pub const TITLEBAR_RIGHT_SPACER: f32 = TRAFFIC_LIGHT_SIZE * 3.0 + TRAFFIC_LIGHT_GAP * 2.0;

// —— 内容 / 分组 / 卡片（§2.1 / §2.3）——
/// 内容区内边距：上下 / 左右（§2.1「左右 24，上下 20」）。
pub const CONTENT_PAD: (f32, f32) = (20.0, 24.0);
/// 分组间距。
pub const GROUP_GAP: f32 = 20.0;
/// 分组标题字号（§2.3：12pt）。
pub const GROUP_HEADER_FONT: f32 = 12.0;
/// 分组标题与卡片间距（§2.3：8）。
pub const GROUP_HEADER_GAP: f32 = 8.0;

pub const CARD_RADIUS: u8 = 10;
/// 常规行高（§2.3）。
pub const ROW_H: f32 = 44.0;
/// 排除目录分组头（标题 + 添加按钮）行高。
pub const EXCLUDE_HEADER_H: f32 = 52.0;
/// 排除目录行高。
pub const EXCLUDE_ROW_H: f32 = 40.0;
/// 行内左右内边距（§2.3：16）。
pub const ROW_PAD_X: f32 = 16.0;
/// 行间分隔线高（§2.3：1px）。
pub const DIVIDER_H: f32 = 1.0;
/// 行标签字号（§2.3：13pt）。
pub const LABEL_FONT: f32 = 13.0;
/// 标签与右侧控件间距（§2.3：16）。
pub const LABEL_CONTROL_GAP: f32 = 16.0;
/// 辅助说明字号（§2.3：11pt）。
pub const SUPPORT_FONT: f32 = 11.0;
/// 辅助说明与标签间距（§2.3：2）。
pub const LABEL_SUPPORT_GAP: f32 = 2.0;

// —— 字号分段（§2.4）——
pub const SEG_W: f32 = 180.0;
pub const SEG_H: f32 = 28.0;
pub const SEG_RADIUS: u8 = 7;
/// 分段容器内边距。
pub const SEG_PAD: f32 = 2.0;
/// 段间距。
pub const SEG_GAP: f32 = 2.0;
pub const SEG_ITEM_RADIUS: u8 = 6;
pub const SEG_FONT: f32 = 12.0;

// —— 开关（§2.4；命名与色板 `switch_*` 统一）——
/// 开关尺寸（宽, 高）。
pub const SWITCH_SIZE: (f32, f32) = (38.0, 22.0);
pub const SWITCH_RADIUS: u8 = 11;
/// 开关钮直径（§2.4：18×18）。
pub const SWITCH_KNOB: f32 = 18.0;
/// 开关钮投影扩边（§2.4「1px 柔和投影」）。
pub const SWITCH_KNOB_SHADOW_SIZE: f32 = 1.0;

// —— 下拉 / 键帽字段（§2.4）——
/// 语言下拉与快捷键字段尺寸（宽, 高）。
pub const FIELD: (f32, f32) = (152.0, 28.0);
pub const FIELD_RADIUS: u8 = 7;
/// 字段文字字号（§2.4：13pt）。
pub const FIELD_FONT: f32 = 13.0;
/// 下拉 chevron-down 图标。
pub const FIELD_CHEVRON: f32 = 14.0;

/// 键帽高（宽随内容）。
pub const KEYCAP_H: f32 = 20.0;
pub const KEYCAP_RADIUS: u8 = 4;
pub const KEYCAP_FONT: f32 = 11.0;

/// 「＋ 添加目录…」按钮高（宽随内容）。
pub const ADD_BUTTON_H: f32 = 28.0;
pub const ADD_BUTTON_RADIUS: u8 = 7;
pub const ADD_BUTTON_FONT: f32 = 12.0;

// —— 图标（§2.4）——
/// 排除目录行的文件夹图标。
pub const FOLDER_ICON: f32 = 16.0;
/// 排除目录行的移除 ×。
pub const REMOVE_ICON: f32 = 14.0;

#[cfg(test)]
mod tests {
    use super::*;

    /// 钉住：深/浅两套颜色与 §2.5 表逐项相等。
    #[test]
    fn settings_palette_matches_spec_table() {
        let rows: [(egui::Color32, egui::Color32, u32, u32, &str); 20] = [
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
                dark().text_primary,
                light().text_primary,
                0xE4E7EBFF,
                0x1F242BFF,
                "标题/行标签",
            ),
            (
                dark().text_secondary,
                light().text_secondary,
                0x8B929AFF,
                0x6B7280FF,
                "分组标题/辅助说明/次要图标",
            ),
            (
                dark().card_fill,
                light().card_fill,
                0xFFFFFF0A,
                0x00000005,
                "分组卡片底",
            ),
            (
                dark().divider,
                light().divider,
                0xFFFFFF14,
                0x00000012,
                "分隔线",
            ),
            (
                dark().control_fill,
                light().control_fill,
                0xFFFFFF14,
                0x0000000A,
                "控件底",
            ),
            (
                dark().control_text,
                light().control_text,
                0xF2F4F7FF,
                0x1A1D22FF,
                "控件文字",
            ),
            (
                dark().segment_selected_fill,
                light().segment_selected_fill,
                0xFFFFFF29,
                0xFFFFFFFF,
                "选中段底",
            ),
            (
                dark().segment_selected_text,
                light().segment_selected_text,
                0xFFFFFFFF,
                0x1F242BFF,
                "选中段文字",
            ),
            (
                dark().segment_unselected_text,
                light().segment_unselected_text,
                0x8B929AFF,
                0x6B7280FF,
                "未选中段文字",
            ),
            (
                dark().switch_on,
                light().switch_on,
                0x0A84FFFF,
                0x007AFFFF,
                "开关开态",
            ),
            (
                dark().switch_knob,
                light().switch_knob,
                0xFFFFFFFF,
                0xFFFFFFFF,
                "开关钮",
            ),
            (
                dark().keycap_fill,
                light().keycap_fill,
                0xFFFFFF1A,
                0x0000000D,
                "键帽底",
            ),
            (
                dark().keycap_text,
                light().keycap_text,
                0xC6CBD1FF,
                0x4B5563FF,
                "键帽文字",
            ),
            (
                dark().add_button_fill,
                light().add_button_fill,
                0xFFFFFF1A,
                0x0000000D,
                "添加目录按钮底",
            ),
            (
                dark().add_button_text,
                light().add_button_text,
                0xC6CBD1FF,
                0x4B5563FF,
                "添加目录按钮文字",
            ),
            (
                dark().dir_path,
                light().dir_path,
                0xE4E7EBFF,
                0x1F242BFF,
                "目录路径",
            ),
            (
                dark().remove_icon,
                light().remove_icon,
                0x8B929AFF,
                0x6B7280FF,
                "移除图标",
            ),
        ];
        for (dark_val, light_val, dark_hex, light_hex, name) in rows {
            assert_eq!(
                dark_val.to_array(),
                c(dark_hex).to_array(),
                "{name} 深色值不符 §2.5"
            );
            assert_eq!(
                light_val.to_array(),
                c(light_hex).to_array(),
                "{name} 浅色值不符 §2.5"
            );
        }
    }

    /// 钉住：错误提示色与完整窗口同源（直接比对，防两处漂移）。
    #[test]
    fn error_text_matches_full_window() {
        use crate::ui::window_tokens;
        assert_eq!(
            dark().error_text.to_array(),
            window_tokens::dark().error_text.to_array()
        );
        assert_eq!(
            light().error_text.to_array(),
            window_tokens::light().error_text.to_array()
        );
    }

    /// 钉住：开关钮投影色（§2.4 表外补充：黑色 20%，两主题同值）。
    #[test]
    fn switch_knob_shadow_is_documented_value() {
        assert_eq!(
            dark().switch_knob_shadow.to_array(),
            c(0x00000033).to_array()
        );
        assert_eq!(
            light().switch_knob_shadow.to_array(),
            c(0x00000033).to_array()
        );
    }

    /// `c()` 转换本身不是自证：用已知不透明 RGBA 十六进制校验字节序
    /// （`Color32` 存储预乘 alpha，半透明值不适合直接比对原始字节）。
    #[test]
    fn color_helper_preserves_opaque_rgba_hex() {
        assert_eq!(c(0x112233FF).to_array(), [0x11, 0x22, 0x33, 0xFF]);
    }

    /// 钉住：§2.2 / §2.4 的几何与字号。
    #[test]
    fn geometry_matches_spec() {
        assert_eq!(
            (SETTINGS_W, SETTINGS_H, WINDOW_RADIUS, WINDOW_STROKE),
            (620.0, 600.0, 12, 1.0)
        );

        assert_eq!((TITLEBAR_H, TITLEBAR_PAD_X, TITLE_FONT), (44.0, 20.0, 13.0));
        assert_eq!(
            (TRAFFIC_LIGHT_SIZE, TRAFFIC_LIGHT_GAP, TITLEBAR_RIGHT_SPACER),
            (12.0, 8.0, 52.0)
        );
        // 拖拽预留为原生交通灯命中区实现值（与完整窗口 token 同值），非规格表数值。
        assert_eq!(TRAFFIC_LIGHTS_RESERVE, 80.0);

        assert_eq!(CONTENT_PAD, (20.0, 24.0));
        assert_eq!(
            (GROUP_GAP, GROUP_HEADER_FONT, GROUP_HEADER_GAP),
            (20.0, 12.0, 8.0)
        );

        assert_eq!(
            (
                CARD_RADIUS,
                ROW_H,
                EXCLUDE_HEADER_H,
                EXCLUDE_ROW_H,
                ROW_PAD_X,
                DIVIDER_H
            ),
            (10, 44.0, 52.0, 40.0, 16.0, 1.0)
        );
        assert_eq!(
            (
                LABEL_FONT,
                LABEL_CONTROL_GAP,
                SUPPORT_FONT,
                LABEL_SUPPORT_GAP
            ),
            (13.0, 16.0, 11.0, 2.0)
        );

        assert_eq!(
            (
                SEG_W,
                SEG_H,
                SEG_RADIUS,
                SEG_PAD,
                SEG_GAP,
                SEG_ITEM_RADIUS,
                SEG_FONT
            ),
            (180.0, 28.0, 7, 2.0, 2.0, 6, 12.0)
        );
        assert_eq!(
            (
                SWITCH_SIZE,
                SWITCH_RADIUS,
                SWITCH_KNOB,
                SWITCH_KNOB_SHADOW_SIZE
            ),
            ((38.0, 22.0), 11, 18.0, 1.0)
        );
        assert_eq!(
            (FIELD, FIELD_RADIUS, FIELD_FONT, FIELD_CHEVRON),
            ((152.0, 28.0), 7, 13.0, 14.0)
        );
        assert_eq!((KEYCAP_H, KEYCAP_RADIUS, KEYCAP_FONT), (20.0, 4, 11.0));
        assert_eq!(
            (ADD_BUTTON_H, ADD_BUTTON_RADIUS, ADD_BUTTON_FONT),
            (28.0, 7, 12.0)
        );
        assert_eq!((FOLDER_ICON, REMOVE_ICON), (16.0, 14.0));
    }

    /// `palette(is_dark)` 按主题取表。
    #[test]
    fn palette_selects_theme() {
        assert_eq!(
            palette(true).window_fill.to_array(),
            dark().window_fill.to_array()
        );
        assert_eq!(
            palette(false).window_fill.to_array(),
            light().window_fill.to_array()
        );
    }
}
