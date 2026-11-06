//! 悬浮条设计 token（`floatbar-visual/visual.md` §四；数值真源 = 五份导出稿）。
//!
//! 规则：绘制代码只读这里，**禁散落色值/字号**（§6.6「实现上把 §4.1 做成 token，
//! 视图层不写条件分支」）。深/浅两套整表由 `palette(dark)` 取。

use eframe::egui;

/// `#RRGGBBAA` → `Color32`(入参 u64;**字面量必须恰 8 位**:前 6 位 RGB、后 2 位 alpha)。
///
/// 超长字面量会被这里的低位截取**静默**改色——曾两次写出 10 位(多打 FF):
/// 「半透明黑」→ 不透明近黑(浅色页脚黑条)、「半透明白」→ 不透明黄(深色页脚
/// 黄条),实机实证 2026-11-06。`debug_assert` 在开发构建直接 panic,钉住测试兜底。
fn c(hex: u64) -> egui::Color32 {
    debug_assert!(hex <= 0xFFFF_FFFF, "色值字面量超 8 位(RRGGBBAA): {hex:#X}");
    egui::Color32::from_rgba_unmultiplied(
        ((hex >> 24) & 0xFF) as u8,
        ((hex >> 16) & 0xFF) as u8,
        ((hex >> 8) & 0xFF) as u8,
        (hex & 0xFF) as u8,
    )
}

/// 悬浮条全套 token（§4.1 逐行）。
#[derive(Clone, Copy)]
pub struct Palette {
    pub panel_fill: egui::Color32,
    pub panel_stroke: egui::Color32,
    pub search_icon: egui::Color32,
    pub query_text: egui::Color32,
    pub btn_fill: egui::Color32,
    pub btn_fill_hover: egui::Color32,
    pub btn_icon: egui::Color32,
    pub divider: egui::Color32,
    pub row_selected: egui::Color32,
    pub file_name: egui::Color32,
    pub file_name_selected: egui::Color32,
    pub dir_text: egui::Color32,
    pub action_hint: egui::Color32,
    pub footer_fill: egui::Color32,
    pub footer_label: egui::Color32,
    pub keycap_fill: egui::Color32,
    pub keycap_text: egui::Color32,
    /// 状态角标（§2.6）：中性 / 警示 / 错误三档强调度的底与字。
    /// 中性档与键帽同值，但有意分列（可独立调整，当前同值）。
    pub badge_neutral_fill: egui::Color32,
    pub badge_neutral_text: egui::Color32,
    pub badge_warning_fill: egui::Color32,
    pub badge_warning_text: egui::Color32,
    pub badge_error_fill: egui::Color32,
    pub badge_error_text: egui::Color32,
}

pub fn dark() -> Palette {
    Palette {
        panel_fill: c(0x191C2199),
        panel_stroke: c(0xFFFFFF29),
        search_icon: c(0x8B929AFF),
        query_text: c(0xF2F4F7FF),
        btn_fill: c(0xFFFFFF1F),
        btn_fill_hover: c(0xFFFFFF2E),
        btn_icon: c(0xE8EAEDFF),
        divider: c(0xFFFFFF14),
        row_selected: c(0xFFFFFF1F),
        file_name: c(0xE4E7EBFF),
        file_name_selected: c(0xFFFFFFFF),
        dir_text: c(0xA2AAB4FF),
        action_hint: c(0xC6CBD1FF),
        footer_fill: c(0xFFFFFF0F),
        footer_label: c(0x8B929AFF),
        keycap_fill: c(0xFFFFFF1A),
        keycap_text: c(0xC6CBD1FF),
        badge_neutral_fill: c(0xFFFFFF1A),
        badge_neutral_text: c(0xC6CBD1FF),
        badge_warning_fill: c(0xF59E0B29),
        badge_warning_text: c(0xF0B357FF),
        badge_error_fill: c(0xEF444429),
        badge_error_text: c(0xF87171FF),
    }
}

pub fn light() -> Palette {
    Palette {
        panel_fill: c(0xFFFFFF99),
        panel_stroke: c(0x0F172A1F),
        search_icon: c(0x6B7280FF),
        query_text: c(0x1A1D22FF),
        btn_fill: c(0x0000000F),
        btn_fill_hover: c(0x0000001A),
        btn_icon: c(0x1F242BFF),
        divider: c(0x00000012),
        row_selected: c(0x5B6FD91F),
        file_name: c(0x1F242BFF),
        file_name_selected: c(0x14181EFF),
        dir_text: c(0x5F6774FF),
        action_hint: c(0x4B5563FF),
        footer_fill: c(0x00000005),
        footer_label: c(0x5F6774FF),
        keycap_fill: c(0x0000000D),
        keycap_text: c(0x4B5563FF),
        badge_neutral_fill: c(0x0000000D),
        badge_neutral_text: c(0x4B5563FF),
        badge_warning_fill: c(0xF59E0B1F),
        badge_warning_text: c(0xB45309FF),
        badge_error_fill: c(0xEF44441F),
        badge_error_text: c(0xB91C1CFF),
    }
}

/// 按主题取整表（每帧一次调用，按值返回即可）。
pub fn palette(is_dark: bool) -> Palette {
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
pub const FONT_QUERY: f32 = 21.0;
pub const FONT_FILENAME: f32 = 15.0;
pub const FONT_DIR: f32 = 12.0;
pub const FONT_HINT: f32 = 12.0; // 动作提示 / 页脚标签 / 结果计数
pub const FONT_KEYCAP: f32 = 11.0; // 键帽 / 状态角标
/// 悬浮条空态主文案（§2.7：14pt 基准）。
pub const FONT_EMPTY_TITLE: f32 = 14.0;

// —— 几何（§2.2/§2.3/§2.4/§2.5）——
pub const PANEL_W: f32 = 700.0;
pub const PANEL_RADIUS: u8 = 12;
pub const SEARCH_H: f32 = 64.0;
pub const SEARCH_PAD_X: f32 = 20.0; // 搜索行左右内边距
pub const SEARCH_GAP: f32 = 14.0; // 搜索行子元素间距
pub const SEARCH_ICON: f32 = 20.0;
pub const BTN_SIZE: f32 = 28.0; // 打开完整窗口按钮
pub const BTN_RADIUS: u8 = 7;
pub const DIVIDER_H: f32 = 1.0;
pub const ROW_H: f32 = 56.0;
/// 结果区左右内缩（行宽 = 面板宽 − 2×此值；最大宽下即 §2.4 的 684）。
pub const LIST_SIDE_PAD: f32 = 8.0;
/// 行内「文件名 ↔ 所在目录」纵向间距（§2.4「文本列纵向，间距 3」）。
pub const ROW_TEXT_GAP_Y: f32 = 3.0;
pub const ROW_GAP: f32 = 2.0;
pub const ROW_RADIUS: u8 = 8;
pub const ROW_PAD_X: f32 = 12.0;
pub const ROW_INNER_GAP: f32 = 12.0;
pub const ROW_ICON: f32 = 32.0; // 系统真实图标（裸放，无块底）
pub const LIST_PAD_V: f32 = 8.0; // 结果区上下内边距
pub const FOOTER_H: f32 = 38.0;
pub const FOOTER_PAD_X: f32 = 16.0;
pub const FOOTER_GAP: f32 = 16.0;
pub const KEYCAP_RADIUS: u8 = 4;
pub const KEYCAP_PAD: (f32, f32) = (3.0, 6.0); // 上下 / 左右
pub const KEYCAP_GAP: f32 = 6.0; // 提示组内部（标签与键帽）
pub const EMPTY_H: f32 = 72.0; // 空态区（§2.7；纵向居中，主副间距 4）
pub const EMPTY_GAP: f32 = 4.0;
/// 空态两行文案块高（主 14pt + 间距 + 副 12pt 的行盒之和，实测 ≈38）——用于在
/// `EMPTY_H` 内纵向居中。
pub const EMPTY_TEXT_H: f32 = 38.0;
/// 选中行动作提示预留宽（「打开」12pt 两字 + 与文件名列的余量，实测 ≈40）。
pub const ACTION_HINT_W: f32 = 40.0;
/// 键帽高（11pt 文本 + 上 3 下 3 内边距 + 描边余量 = §2.5 的 19）。
pub const KEYCAP_H: f32 = 19.0;

// —— 形象几何（§2.9；Task 9 已落地：窗口高 = 面板高 + `MASCOT_STRIP`）——
/// 形象画布宽（§2.9）。
pub const MASCOT_W: f32 = 118.0;
/// 形象画布高（§2.9）。
pub const MASCOT_H: f32 = 100.0;
/// 形象下缘压过面板顶边的深度（§2.9）。
pub const MASCOT_OVERLAP: f32 = 34.0;
/// 形象右缘距面板右缘（§2.9）。
pub const MASCOT_RIGHT_GAP: f32 = 60.0;
/// 角标（或输入框）与形象左缘的最小净空（§2.9「避让」）。
pub const MASCOT_BADGE_GAP: f32 = 12.0;
/// 画布顶部到窗口顶的余量（旋转/呼吸时头顶不碰窗口边）。
///
/// 旋转支点在画布底边中点：±3° 摇摆时顶角相对画布顶最多上探
/// `59·sin3° − 100·(1−cos3°) ≈ 2.95px`；呼吸最高再上抬 1.5px，4px 留量足够。
pub const MASCOT_HEADROOM: f32 = 4.0;
/// 形象露出面板顶边的条高 = 画布高 − 压顶量 + 头顶余量（窗口高 = 面板高 + 本值）。
pub const MASCOT_STRIP: f32 = MASCOT_H - MASCOT_OVERLAP + MASCOT_HEADROOM;
/// 搜索行右端「按钮后游标 → 角标右缘」的预留宽（不含 item_spacing 14）；
/// 按钮左缘到角标右缘的实际距离 = `SEARCH_GAP + 本值` = 142。
///
/// 推导（§2.9）：`面板右缘 − SEARCH_PAD_X − BTN_SIZE − SEARCH_GAP − 本值`
/// 必须等于 `形象左缘 − MASCOT_BADGE_GAP`。无角标时该净空同样保留，
/// 输入框右缘不进入形象区域。
pub const MASCOT_CLEAR_W: f32 =
    MASCOT_RIGHT_GAP + MASCOT_W + MASCOT_BADGE_GAP - (SEARCH_PAD_X + BTN_SIZE) - SEARCH_GAP;

/// 编译期钉住形象露出条高（窗口预留直接读 `MASCOT_STRIP`）。
const _: () = assert!(MASCOT_STRIP == 70.0);

/// 面板高（不含形象条；窗口高 = 本值 + `MASCOT_STRIP`，Task 9）。
pub const fn panel_h(list_h: f32) -> f32 {
    SEARCH_H + DIVIDER_H + list_h + DIVIDER_H + FOOTER_H
}

/// 结果区高（n 行为 0 时取空态区高；§2.2 高度推导）。
pub const fn list_h(rows: usize) -> f32 {
    if rows == 0 {
        EMPTY_H
    } else {
        rows as f32 * ROW_H + (rows as f32 - 1.0) * ROW_GAP + LIST_PAD_V * 2.0
    }
}

/// 保存面板目录助手浮层 token（`overlay-visual/prototype.html` v1 定版；
/// 不透明材质，色值取自 §4.2「完整结果窗口颜色」+ 原型补充的占位/空态/文件夹三档）。
///
/// 规则同 §4.1：绘制代码只读这里，禁散落色值。
#[derive(Clone, Copy)]
pub struct Overlay {
    /// 面板底（**不透明**;无毛玻璃,内存优先——2026-11-05 用户约束）。
    pub bg: egui::Color32,
    /// 面板描边。
    pub border: egui::Color32,
    pub divider: egui::Color32,
    pub search_icon: egui::Color32,
    /// 查询文本。
    pub query: egui::Color32,
    /// 选中行底。
    pub selected_bg: egui::Color32,
    /// 目录名（主文本）。
    pub name: egui::Color32,
    /// 目录名（选中行,加粗）。
    pub name_selected: egui::Color32,
    /// 完整路径（次文本）。
    pub path: egui::Color32,
    /// 「↵ 跳转」动作提示字。
    pub action_text: egui::Color32,
    /// 「↵ 跳转」动作提示底。
    pub action_bg: egui::Color32,
    /// 页脚底（§4.2 状态栏底同源）。
    pub footer_bg: egui::Color32,
    pub footer_text: egui::Color32,
    pub keycap_bg: egui::Color32,
    pub keycap_text: egui::Color32,
    /// 空态文本。
    pub empty_text: egui::Color32,
    /// 文件夹 glyph 前身/后标签（自绘矢量;内存优先不用 NSWorkspace 图标纹理）。
    pub folder_front: egui::Color32,
    pub folder_back: egui::Color32,
}

pub fn overlay(is_dark: bool) -> Overlay {
    if is_dark {
        Overlay {
            bg: c(0x1A1D22FF),
            border: c(0xFFFFFF1F),
            divider: c(0xFFFFFF14),
            search_icon: c(0x8B929AFF),
            query: c(0xF2F4F7FF),
            selected_bg: c(0xFFFFFF1F),
            name: c(0xE4E7EBFF),
            name_selected: c(0xFFFFFFFF),
            path: c(0x8B929AFF),
            action_text: c(0xC6CBD1FF),
            // 半透明白三件套曾误写 10 位字面量(多打 FF),c() 解码成不透明黄色
            // (深色页脚黄条实证 2026-11-06);与浅色三件套同病同修。
            action_bg: c(0xFFFFFF1A),
            footer_bg: c(0xFFFFFF0A),
            footer_text: c(0x8B929AFF),
            keycap_bg: c(0xFFFFFF1A),
            keycap_text: c(0xC6CBD1FF),
            empty_text: c(0x6D757EFF),
            folder_front: c(0x4E9BE8FF),
            folder_back: c(0x3D87D6FF),
        }
    } else {
        Overlay {
            bg: c(0xFFFFFFFF),
            border: c(0x0F172A1F),
            divider: c(0x00000012),
            search_icon: c(0x6B7280FF),
            query: c(0x1A1D22FF),
            selected_bg: c(0x5B6FD91F),
            name: c(0x1F242BFF),
            name_selected: c(0x14181EFF),
            path: c(0x5F6774FF),
            action_text: c(0x4B5563FF),
            // 半透明黑三件套曾误写 10 位十六进制字面量(多打 FF),c() 解码成
            // **不透明近黑**——浮层页脚整条呈黑色(实证 2026-11-06)。钉住测试防再犯。
            action_bg: c(0x0000000F),
            footer_bg: c(0x00000005),
            footer_text: c(0x5F6774FF),
            keycap_bg: c(0x0000000D),
            keycap_text: c(0x4B5563FF),
            empty_text: c(0x9AA2ADFF),
            folder_front: c(0x5CA6EEFF),
            folder_back: c(0x4A94E4FF),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 钉住：深/浅两套 token 与 `visual.md` §4.1 表逐项相等（防手滑改错色值）。
    #[test]
    fn tokens_match_spec_table() {
        let rows: [(egui::Color32, u32, u32, &str); 23] = [
            (dark().panel_fill, 0x191C2199, 0xFFFFFF99, "面板底"),
            (dark().panel_stroke, 0xFFFFFF29, 0x0F172A1F, "面板描边"),
            (dark().search_icon, 0x8B929AFF, 0x6B7280FF, "搜索图标"),
            (dark().query_text, 0xF2F4F7FF, 0x1A1D22FF, "查询文本"),
            (dark().btn_fill, 0xFFFFFF1F, 0x0000000F, "按钮底(静止)"),
            (
                dark().btn_fill_hover,
                0xFFFFFF2E,
                0x0000001A,
                "按钮底(悬停)",
            ),
            (dark().btn_icon, 0xE8EAEDFF, 0x1F242BFF, "按钮图标"),
            (dark().divider, 0xFFFFFF14, 0x00000012, "分隔线"),
            (dark().row_selected, 0xFFFFFF1F, 0x5B6FD91F, "选中行底"),
            (dark().file_name, 0xE4E7EBFF, 0x1F242BFF, "文件名"),
            (
                dark().file_name_selected,
                0xFFFFFFFF,
                0x14181EFF,
                "文件名(选中)",
            ),
            (dark().dir_text, 0xA2AAB4FF, 0x5F6774FF, "所在目录"),
            (dark().action_hint, 0xC6CBD1FF, 0x4B5563FF, "动作提示"),
            (dark().footer_fill, 0xFFFFFF0F, 0x00000005, "页脚底"),
            (dark().footer_label, 0x8B929AFF, 0x5F6774FF, "页脚标签"),
            (dark().keycap_fill, 0xFFFFFF1A, 0x0000000D, "键帽底"),
            (dark().keycap_text, 0xC6CBD1FF, 0x4B5563FF, "键帽文字"),
            (
                dark().badge_neutral_fill,
                0xFFFFFF1A,
                0x0000000D,
                "角标底(中性)",
            ),
            (
                dark().badge_neutral_text,
                0xC6CBD1FF,
                0x4B5563FF,
                "角标文字(中性)",
            ),
            (
                dark().badge_warning_fill,
                0xF59E0B29,
                0xF59E0B1F,
                "角标底(警示)",
            ),
            (
                dark().badge_warning_text,
                0xF0B357FF,
                0xB45309FF,
                "角标文字(警示)",
            ),
            (
                dark().badge_error_fill,
                0xEF444429,
                0xEF44441F,
                "角标底(错误)",
            ),
            (
                dark().badge_error_text,
                0xF87171FF,
                0xB91C1CFF,
                "角标文字(错误)",
            ),
        ];
        let light: [egui::Color32; 23] = [
            light().panel_fill,
            light().panel_stroke,
            light().search_icon,
            light().query_text,
            light().btn_fill,
            light().btn_fill_hover,
            light().btn_icon,
            light().divider,
            light().row_selected,
            light().file_name,
            light().file_name_selected,
            light().dir_text,
            light().action_hint,
            light().footer_fill,
            light().footer_label,
            light().keycap_fill,
            light().keycap_text,
            light().badge_neutral_fill,
            light().badge_neutral_text,
            light().badge_warning_fill,
            light().badge_warning_text,
            light().badge_error_fill,
            light().badge_error_text,
        ];
        for (i, (dark_val, dark_hex, light_hex, name)) in rows.iter().enumerate() {
            assert_eq!(
                dark_val.to_array(),
                c(u64::from(*dark_hex)).to_array(),
                "{name} 深色值不符 §4.1"
            );
            assert_eq!(
                light[i].to_array(),
                c(u64::from(*light_hex)).to_array(),
                "{name} 浅色值不符 §4.1"
            );
        }
    }

    /// 钉住：浮层 token 与 `overlay-visual/prototype.html` v1 定版表逐项相等。
    /// 回归背景：浅色半透明黑三件套曾误写 10 位字面量 → c() 解码成不透明近黑
    /// （页脚黑条，实机取证 2026-11-06）；整表钉死。
    #[test]
    fn overlay_tokens_match_spec_table() {
        let dark = overlay(true);
        let light = overlay(false);
        let light_vals: [egui::Color32; 18] = [
            light.bg,
            light.border,
            light.divider,
            light.search_icon,
            light.query,
            light.selected_bg,
            light.name,
            light.name_selected,
            light.path,
            light.action_text,
            light.action_bg,
            light.footer_bg,
            light.footer_text,
            light.keycap_bg,
            light.keycap_text,
            light.empty_text,
            light.folder_front,
            light.folder_back,
        ];
        let rows: [(egui::Color32, u64, u64, &str); 18] = [
            (dark.bg, 0x1A1D22FF, 0xFFFFFFFF, "面板底"),
            (dark.border, 0xFFFFFF1F, 0x0F172A1F, "面板描边"),
            (dark.divider, 0xFFFFFF14, 0x00000012, "分隔线"),
            (dark.search_icon, 0x8B929AFF, 0x6B7280FF, "搜索图标"),
            (dark.query, 0xF2F4F7FF, 0x1A1D22FF, "查询文本"),
            (dark.selected_bg, 0xFFFFFF1F, 0x5B6FD91F, "选中行底"),
            (dark.name, 0xE4E7EBFF, 0x1F242BFF, "目录名"),
            (dark.name_selected, 0xFFFFFFFF, 0x14181EFF, "目录名(选中)"),
            (dark.path, 0x8B929AFF, 0x5F6774FF, "完整路径"),
            (dark.action_text, 0xC6CBD1FF, 0x4B5563FF, "动作提示字"),
            (dark.action_bg, 0xFFFFFF1A, 0x0000000F, "动作提示底"),
            (dark.footer_bg, 0xFFFFFF0A, 0x00000005, "页脚底"),
            (dark.footer_text, 0x8B929AFF, 0x5F6774FF, "页脚文字"),
            (dark.keycap_bg, 0xFFFFFF1A, 0x0000000D, "键帽底"),
            (dark.keycap_text, 0xC6CBD1FF, 0x4B5563FF, "键帽文字"),
            (dark.empty_text, 0x6D757EFF, 0x9AA2ADFF, "空态文本"),
            (dark.folder_front, 0x4E9BE8FF, 0x5CA6EEFF, "文件夹前身"),
            (dark.folder_back, 0x3D87D6FF, 0x4A94E4FF, "文件夹后标签"),
        ];
        for (i, (val, dark_hex, light_hex, name)) in rows.iter().enumerate() {
            assert_eq!(
                val.to_array(),
                c(*dark_hex).to_array(),
                "{name} 深色值不符浮层定版表"
            );
            assert_eq!(
                light_vals[i].to_array(),
                c(*light_hex).to_array(),
                "{name} 浅色值不符浮层定版表"
            );
        }
    }

    /// 钉住：面板高推导（§2.2：6 行 → 466；空态 176）。
    #[test]
    fn panel_height_derivation() {
        assert_eq!(list_h(0), EMPTY_H);
        assert_eq!(list_h(1), ROW_H + LIST_PAD_V * 2.0);
        assert_eq!(list_h(6), 6.0 * ROW_H + 5.0 * ROW_GAP + LIST_PAD_V * 2.0);
        assert_eq!(panel_h(list_h(0)), 176.0);
        assert_eq!(panel_h(list_h(6)), 466.0);
    }

    /// 钉住：形象净空推导（§2.9）——内容宽 = 面板宽时，角标右缘与形象左缘恰 12。
    #[test]
    fn mascot_clear_w_leaves_twelve_px_gap() {
        let mascot_left = PANEL_W - MASCOT_RIGHT_GAP - MASCOT_W;
        assert_eq!(MASCOT_CLEAR_W, 128.0, "净空宽应等于 §2.9 推导值");
        // Task 11 起搜索行内容宽 = panel_w（描边画在内侧，不再从内容宽里扣 2）。
        let badge_right = PANEL_W - SEARCH_PAD_X - BTN_SIZE - SEARCH_GAP - MASCOT_CLEAR_W;
        assert_eq!(
            badge_right,
            PANEL_W - (MASCOT_RIGHT_GAP + MASCOT_W + MASCOT_BADGE_GAP),
            "角标右缘须按「面板右缘 − 形象占位 − 净空」定位"
        );
        assert_eq!(
            mascot_left - badge_right,
            MASCOT_BADGE_GAP,
            "内容宽 = 面板宽时净空应为 12"
        );
        assert!(badge_right < mascot_left, "角标不得进入形象区域");
        // 形象条 = 画布 100 − 压顶 34 + 头顶余量 4。
        assert_eq!(MASCOT_HEADROOM, 4.0);
        assert_eq!(MASCOT_STRIP, 70.0);
        assert_eq!(MASCOT_STRIP, MASCOT_H - MASCOT_OVERLAP + MASCOT_HEADROOM);
    }
}
