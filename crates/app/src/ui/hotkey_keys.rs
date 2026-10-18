//! 快捷键键位映射、校验与展示（设置 · 快捷键 Task H1）。
//!
//! 录制层拿到的是一次 egui 按键事件（`egui::Key` + 当时修饰态），落盘/注册层用
//! `global_hotkey::hotkey::HotKey`；本模块是两层之间唯一的翻译点：
//! [`from_egui`] 白名单映射 + 必须含修饰键，[`validate`] 在注册/持久化前复核
//! （同一张白名单 + 修饰键），[`display_keys`] 只负责设置页键帽文案（macOS 符号 +
//! 可读键名），与 `HotKey` 的 serde 规范串（如 `alt+Space`）彼此独立。
//!
//! 支持面刻意收窄：A–Z、0–9、F1–F12、Space/Enter/Tab、方向键、Home/End/
//! PageUp/PageDown 与主键区常见标点。媒体键、浏览器键等一律拒绝（需求未列出的
//! 键不做「顺手支持」）。白名单是 [`SUPPORTED_KEYS`] 唯一真源：录制映射与
//! `settings.toml` 校验共用，手工编辑写入的 Escape 等越界键会在注册前被拒。

use std::fmt;

use eframe::egui;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};

use crate::i18n;

/// 快捷键校验错误；`text()` / `Display` 按当前语言输出可直接展示的文案。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyError {
    /// 未按任何修饰键。
    NeedsModifier,
    /// 键位不在白名单内。
    Unsupported,
}

impl HotkeyError {
    pub fn text(self) -> &'static str {
        match self {
            Self::NeedsModifier => i18n::s().hotkey_needs_modifier,
            Self::Unsupported => i18n::s().hotkey_unsupported,
        }
    }
}

impl fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

impl std::error::Error for HotkeyError {}

/// 未按任何修饰键。
const NO_MODIFIER: HotkeyError = HotkeyError::NeedsModifier;
/// 键位不在白名单内。
const UNSUPPORTED_KEY: HotkeyError = HotkeyError::Unsupported;

/// 把一次 egui 按键事件翻译成全局热键。
///
/// `key` 必须是**非修饰键**的实际按键，修饰键只从 `mods` 读取。白名单外的键
/// （纯修饰键、F13+、媒体键、未列入的标点等）→「不支持该键」；白名单键但
/// 未按任何修饰键 →「需包含修饰键」。
pub fn from_egui(key: egui::Key, mods: egui::Modifiers) -> Result<HotKey, HotkeyError> {
    let code = code_from_egui(key)?;

    let mut hotkey_mods = Modifiers::empty();
    // egui 在 macOS 上 command/mac_cmd 同时为真；分别判断后按位或入 SUPER，
    // 天然不会重复（bitflags insert 幂等）。
    if mods.command || mods.mac_cmd {
        hotkey_mods.insert(Modifiers::SUPER);
    }
    if mods.ctrl {
        hotkey_mods.insert(Modifiers::CONTROL);
    }
    if mods.alt {
        hotkey_mods.insert(Modifiers::ALT);
    }
    if mods.shift {
        hotkey_mods.insert(Modifiers::SHIFT);
    }

    if hotkey_mods.is_empty() {
        return Err(NO_MODIFIER);
    }

    Ok(HotKey::new(Some(hotkey_mods), code))
}

/// 注册/持久化前复核：必须至少含一个修饰键，且键位在 [`from_egui`] 的同一白名单
/// （[`SUPPORTED_KEYS`]）内。手工编辑 `settings.toml` 写入的 Escape/媒体键等会在
/// 注册前被拒，由调用方回落默认键并提示。
pub fn validate(hotkey: HotKey) -> Result<(), HotkeyError> {
    if hotkey.mods.is_empty() {
        return Err(NO_MODIFIER);
    }
    if !is_supported_code(hotkey.key) {
        return Err(UNSUPPORTED_KEY);
    }
    Ok(())
}

/// 键帽展示：macOS 符号按 ⌃⌥⇧⌘ 顺序 + 可读键名。
pub fn display_keys(hotkey: HotKey) -> Vec<String> {
    let mut keys = Vec::with_capacity(5);
    if hotkey.mods.contains(Modifiers::CONTROL) {
        keys.push("⌃".to_owned());
    }
    if hotkey.mods.contains(Modifiers::ALT) {
        keys.push("⌥".to_owned());
    }
    if hotkey.mods.contains(Modifiers::SHIFT) {
        keys.push("⇧".to_owned());
    }
    if hotkey.mods.contains(Modifiers::SUPER) {
        keys.push("⌘".to_owned());
    }
    keys.push(code_label(hotkey.key));
    keys
}

/// 支持键位唯一真源：egui 录制映射与 `settings.toml` 校验共用同一张表。
///
/// 增删键位只改这里——[`from_egui`] 与 [`validate`] 自动保持一致。
const SUPPORTED_KEYS: &[(egui::Key, Code)] = &[
    (egui::Key::A, Code::KeyA),
    (egui::Key::B, Code::KeyB),
    (egui::Key::C, Code::KeyC),
    (egui::Key::D, Code::KeyD),
    (egui::Key::E, Code::KeyE),
    (egui::Key::F, Code::KeyF),
    (egui::Key::G, Code::KeyG),
    (egui::Key::H, Code::KeyH),
    (egui::Key::I, Code::KeyI),
    (egui::Key::J, Code::KeyJ),
    (egui::Key::K, Code::KeyK),
    (egui::Key::L, Code::KeyL),
    (egui::Key::M, Code::KeyM),
    (egui::Key::N, Code::KeyN),
    (egui::Key::O, Code::KeyO),
    (egui::Key::P, Code::KeyP),
    (egui::Key::Q, Code::KeyQ),
    (egui::Key::R, Code::KeyR),
    (egui::Key::S, Code::KeyS),
    (egui::Key::T, Code::KeyT),
    (egui::Key::U, Code::KeyU),
    (egui::Key::V, Code::KeyV),
    (egui::Key::W, Code::KeyW),
    (egui::Key::X, Code::KeyX),
    (egui::Key::Y, Code::KeyY),
    (egui::Key::Z, Code::KeyZ),
    (egui::Key::Num0, Code::Digit0),
    (egui::Key::Num1, Code::Digit1),
    (egui::Key::Num2, Code::Digit2),
    (egui::Key::Num3, Code::Digit3),
    (egui::Key::Num4, Code::Digit4),
    (egui::Key::Num5, Code::Digit5),
    (egui::Key::Num6, Code::Digit6),
    (egui::Key::Num7, Code::Digit7),
    (egui::Key::Num8, Code::Digit8),
    (egui::Key::Num9, Code::Digit9),
    (egui::Key::F1, Code::F1),
    (egui::Key::F2, Code::F2),
    (egui::Key::F3, Code::F3),
    (egui::Key::F4, Code::F4),
    (egui::Key::F5, Code::F5),
    (egui::Key::F6, Code::F6),
    (egui::Key::F7, Code::F7),
    (egui::Key::F8, Code::F8),
    (egui::Key::F9, Code::F9),
    (egui::Key::F10, Code::F10),
    (egui::Key::F11, Code::F11),
    (egui::Key::F12, Code::F12),
    (egui::Key::Space, Code::Space),
    (egui::Key::Enter, Code::Enter),
    (egui::Key::Tab, Code::Tab),
    (egui::Key::ArrowDown, Code::ArrowDown),
    (egui::Key::ArrowLeft, Code::ArrowLeft),
    (egui::Key::ArrowRight, Code::ArrowRight),
    (egui::Key::ArrowUp, Code::ArrowUp),
    (egui::Key::Home, Code::Home),
    (egui::Key::End, Code::End),
    (egui::Key::PageUp, Code::PageUp),
    (egui::Key::PageDown, Code::PageDown),
    (egui::Key::Minus, Code::Minus),
    (egui::Key::Equals, Code::Equal),
    (egui::Key::OpenBracket, Code::BracketLeft),
    (egui::Key::CloseBracket, Code::BracketRight),
    (egui::Key::Backslash, Code::Backslash),
    (egui::Key::Semicolon, Code::Semicolon),
    (egui::Key::Quote, Code::Quote),
    (egui::Key::Comma, Code::Comma),
    (egui::Key::Period, Code::Period),
    (egui::Key::Slash, Code::Slash),
    (egui::Key::Backtick, Code::Backquote),
];

/// egui 逻辑键 → keyboard-types 物理键位（白名单查表）。
fn code_from_egui(key: egui::Key) -> Result<Code, HotkeyError> {
    SUPPORTED_KEYS
        .iter()
        .find_map(|&(egui_key, code)| (egui_key == key).then_some(code))
        .ok_or(UNSUPPORTED_KEY)
}

/// 物理键位是否在录制白名单内（`settings.toml` 手工编辑校验用）。
fn is_supported_code(code: Code) -> bool {
    SUPPORTED_KEYS
        .iter()
        .any(|&(_, supported)| supported == code)
}

/// `Code` → 键帽可读名。白名单外的键（持久化文件陈旧/手工编辑）回落到
/// keyboard-types 的规范名，保证展示不丢键。
fn code_label(code: Code) -> String {
    let label = match code {
        Code::KeyA => "A",
        Code::KeyB => "B",
        Code::KeyC => "C",
        Code::KeyD => "D",
        Code::KeyE => "E",
        Code::KeyF => "F",
        Code::KeyG => "G",
        Code::KeyH => "H",
        Code::KeyI => "I",
        Code::KeyJ => "J",
        Code::KeyK => "K",
        Code::KeyL => "L",
        Code::KeyM => "M",
        Code::KeyN => "N",
        Code::KeyO => "O",
        Code::KeyP => "P",
        Code::KeyQ => "Q",
        Code::KeyR => "R",
        Code::KeyS => "S",
        Code::KeyT => "T",
        Code::KeyU => "U",
        Code::KeyV => "V",
        Code::KeyW => "W",
        Code::KeyX => "X",
        Code::KeyY => "Y",
        Code::KeyZ => "Z",
        Code::Digit0 => "0",
        Code::Digit1 => "1",
        Code::Digit2 => "2",
        Code::Digit3 => "3",
        Code::Digit4 => "4",
        Code::Digit5 => "5",
        Code::Digit6 => "6",
        Code::Digit7 => "7",
        Code::Digit8 => "8",
        Code::Digit9 => "9",
        Code::F1 => "F1",
        Code::F2 => "F2",
        Code::F3 => "F3",
        Code::F4 => "F4",
        Code::F5 => "F5",
        Code::F6 => "F6",
        Code::F7 => "F7",
        Code::F8 => "F8",
        Code::F9 => "F9",
        Code::F10 => "F10",
        Code::F11 => "F11",
        Code::F12 => "F12",
        Code::Space => "Space",
        Code::Enter => "Enter",
        Code::Tab => "Tab",
        Code::Home => "Home",
        Code::End => "End",
        Code::PageUp => "PageUp",
        Code::PageDown => "PageDown",
        Code::ArrowLeft => "←",
        Code::ArrowRight => "→",
        Code::ArrowUp => "↑",
        Code::ArrowDown => "↓",
        Code::Minus => "-",
        Code::Equal => "=",
        Code::BracketLeft => "[",
        Code::BracketRight => "]",
        Code::Backslash => "\\",
        Code::Semicolon => ";",
        Code::Quote => "'",
        Code::Comma => ",",
        Code::Period => ".",
        Code::Slash => "/",
        Code::Backquote => "`",
        _ => return code.to_string(),
    };
    label.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A–Z / 0–9 / F1–F12 全部映射到 keyboard-types 的 `Code`。
    #[test]
    fn letters_digits_and_function_keys_map_to_codes() {
        let cases = [
            (egui::Key::A, Code::KeyA),
            (egui::Key::B, Code::KeyB),
            (egui::Key::C, Code::KeyC),
            (egui::Key::D, Code::KeyD),
            (egui::Key::E, Code::KeyE),
            (egui::Key::F, Code::KeyF),
            (egui::Key::G, Code::KeyG),
            (egui::Key::H, Code::KeyH),
            (egui::Key::I, Code::KeyI),
            (egui::Key::J, Code::KeyJ),
            (egui::Key::K, Code::KeyK),
            (egui::Key::L, Code::KeyL),
            (egui::Key::M, Code::KeyM),
            (egui::Key::N, Code::KeyN),
            (egui::Key::O, Code::KeyO),
            (egui::Key::P, Code::KeyP),
            (egui::Key::Q, Code::KeyQ),
            (egui::Key::R, Code::KeyR),
            (egui::Key::S, Code::KeyS),
            (egui::Key::T, Code::KeyT),
            (egui::Key::U, Code::KeyU),
            (egui::Key::V, Code::KeyV),
            (egui::Key::W, Code::KeyW),
            (egui::Key::X, Code::KeyX),
            (egui::Key::Y, Code::KeyY),
            (egui::Key::Z, Code::KeyZ),
            (egui::Key::Num0, Code::Digit0),
            (egui::Key::Num1, Code::Digit1),
            (egui::Key::Num2, Code::Digit2),
            (egui::Key::Num3, Code::Digit3),
            (egui::Key::Num4, Code::Digit4),
            (egui::Key::Num5, Code::Digit5),
            (egui::Key::Num6, Code::Digit6),
            (egui::Key::Num7, Code::Digit7),
            (egui::Key::Num8, Code::Digit8),
            (egui::Key::Num9, Code::Digit9),
            (egui::Key::F1, Code::F1),
            (egui::Key::F2, Code::F2),
            (egui::Key::F3, Code::F3),
            (egui::Key::F4, Code::F4),
            (egui::Key::F5, Code::F5),
            (egui::Key::F6, Code::F6),
            (egui::Key::F7, Code::F7),
            (egui::Key::F8, Code::F8),
            (egui::Key::F9, Code::F9),
            (egui::Key::F10, Code::F10),
            (egui::Key::F11, Code::F11),
            (egui::Key::F12, Code::F12),
        ];

        for (key, code) in cases {
            let hotkey = from_egui(key, egui::Modifiers::ALT)
                .unwrap_or_else(|err| panic!("{key:?} 应在白名单内: {err:?}"));
            assert_eq!(hotkey.key, code, "{key:?} 应映射到 {code:?}");
        }
    }

    /// 控制/导航键与主键区常见标点映射。
    #[test]
    fn navigation_and_punctuation_keys_map_to_codes() {
        let cases = [
            (egui::Key::Space, Code::Space),
            (egui::Key::Enter, Code::Enter),
            (egui::Key::Tab, Code::Tab),
            (egui::Key::ArrowDown, Code::ArrowDown),
            (egui::Key::ArrowLeft, Code::ArrowLeft),
            (egui::Key::ArrowRight, Code::ArrowRight),
            (egui::Key::ArrowUp, Code::ArrowUp),
            (egui::Key::Home, Code::Home),
            (egui::Key::End, Code::End),
            (egui::Key::PageUp, Code::PageUp),
            (egui::Key::PageDown, Code::PageDown),
            (egui::Key::Minus, Code::Minus),
            (egui::Key::Equals, Code::Equal),
            (egui::Key::OpenBracket, Code::BracketLeft),
            (egui::Key::CloseBracket, Code::BracketRight),
            (egui::Key::Backslash, Code::Backslash),
            (egui::Key::Semicolon, Code::Semicolon),
            (egui::Key::Quote, Code::Quote),
            (egui::Key::Comma, Code::Comma),
            (egui::Key::Period, Code::Period),
            (egui::Key::Slash, Code::Slash),
            (egui::Key::Backtick, Code::Backquote),
        ];

        for (key, code) in cases {
            let hotkey = from_egui(key, egui::Modifiers::ALT)
                .unwrap_or_else(|err| panic!("{key:?} 应在白名单内: {err:?}"));
            assert_eq!(hotkey.key, code, "{key:?} 应映射到 {code:?}");
        }
    }

    /// command（含 mac_cmd）/ ctrl / alt / shift 映射到 global-hotkey 修饰位；
    /// command 与 mac_cmd 同为真时 SUPER 只出现一次。
    #[test]
    fn egui_modifiers_map_to_global_hotkey_modifiers() {
        let cmd = from_egui(egui::Key::Space, egui::Modifiers::COMMAND).unwrap();
        assert_eq!(cmd.mods, Modifiers::SUPER, "command 应映射为 SUPER");

        let mac_cmd = from_egui(egui::Key::Space, egui::Modifiers::MAC_CMD).unwrap();
        assert_eq!(mac_cmd.mods, Modifiers::SUPER, "mac_cmd 应映射为 SUPER");

        let both = from_egui(
            egui::Key::Space,
            egui::Modifiers::MAC_CMD | egui::Modifiers::COMMAND,
        )
        .unwrap();
        assert_eq!(both.mods, Modifiers::SUPER);
        assert_eq!(
            both.mods.bits(),
            Modifiers::SUPER.bits(),
            "command + mac_cmd 不得重复叠加 SUPER"
        );

        let ctrl = from_egui(egui::Key::Space, egui::Modifiers::CTRL).unwrap();
        assert_eq!(ctrl.mods, Modifiers::CONTROL);

        let alt = from_egui(egui::Key::Space, egui::Modifiers::ALT).unwrap();
        assert_eq!(alt.mods, Modifiers::ALT);

        let shift = from_egui(egui::Key::Space, egui::Modifiers::SHIFT).unwrap();
        assert_eq!(shift.mods, Modifiers::SHIFT);

        let all = from_egui(
            egui::Key::F,
            egui::Modifiers::COMMAND
                | egui::Modifiers::CTRL
                | egui::Modifiers::ALT
                | egui::Modifiers::SHIFT,
        )
        .unwrap();
        assert_eq!(
            all.mods,
            Modifiers::SUPER | Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT
        );
    }

    /// 没有任何修饰键 → 「需包含修饰键」。
    #[test]
    fn rejects_missing_modifier() {
        for key in [
            egui::Key::A,
            egui::Key::Num0,
            egui::Key::Space,
            egui::Key::F5,
        ] {
            let err = from_egui(key, egui::Modifiers::NONE).unwrap_err();
            assert_eq!(err, NO_MODIFIER, "{key:?} 无修饰键必须被拒");
            assert_eq!(err.text(), "需包含修饰键");
        }
    }

    /// 纯修饰键不是可录制的按键；白名单外的键（F13+/媒体键/未列入标点）→
    /// 「不支持该键」。
    #[test]
    fn rejects_pure_modifier_and_unsupported_keys() {
        for key in [
            egui::Key::ShiftLeft,
            egui::Key::ShiftRight,
            egui::Key::ControlLeft,
            egui::Key::AltRight,
            egui::Key::SuperLeft,
        ] {
            assert_eq!(
                from_egui(key, egui::Modifiers::SHIFT).unwrap_err(),
                UNSUPPORTED_KEY,
                "纯修饰键 {key:?} 不是按键事件"
            );
        }

        for key in [
            egui::Key::F13,
            egui::Key::Escape,
            egui::Key::MediaPlayPause,
            egui::Key::Questionmark,
            egui::Key::Plus,
        ] {
            let err = from_egui(key, egui::Modifiers::ALT).unwrap_err();
            assert_eq!(err, UNSUPPORTED_KEY, "{key:?} 不在白名单内");
            assert_eq!(err.text(), "不支持该键");
        }
    }

    /// 展示用 macOS 符号：⌃⌥⇧⌘ 固定顺序（Apple 习惯），键名可读。
    #[test]
    fn display_uses_macos_symbols_in_apple_order() {
        let alt_space = HotKey::new(Some(Modifiers::ALT), Code::Space);
        assert_eq!(display_keys(alt_space), ["⌥", "Space"]);

        let alt_shift_f = HotKey::new(Some(Modifiers::ALT | Modifiers::SHIFT), Code::KeyF);
        assert_eq!(display_keys(alt_shift_f), ["⌥", "⇧", "F"]);

        let cmd_shift_f = HotKey::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyF);
        assert_eq!(display_keys(cmd_shift_f), ["⇧", "⌘", "F"]);

        let all = HotKey::new(
            Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT | Modifiers::SUPER),
            Code::KeyF,
        );
        assert_eq!(display_keys(all), ["⌃", "⌥", "⇧", "⌘", "F"]);
    }

    /// 每个白名单键位都有可读展示名（字母/数字/F 键/导航/标点）。
    #[test]
    fn display_labels_for_supported_keys() {
        let cases = [
            (Code::KeyA, "A"),
            (Code::KeyZ, "Z"),
            (Code::Digit0, "0"),
            (Code::Digit9, "9"),
            (Code::F1, "F1"),
            (Code::F12, "F12"),
            (Code::Space, "Space"),
            (Code::Enter, "Enter"),
            (Code::Tab, "Tab"),
            (Code::Home, "Home"),
            (Code::End, "End"),
            (Code::PageUp, "PageUp"),
            (Code::PageDown, "PageDown"),
            (Code::ArrowLeft, "←"),
            (Code::ArrowRight, "→"),
            (Code::ArrowUp, "↑"),
            (Code::ArrowDown, "↓"),
            (Code::Minus, "-"),
            (Code::Equal, "="),
            (Code::BracketLeft, "["),
            (Code::BracketRight, "]"),
            (Code::Backslash, "\\"),
            (Code::Semicolon, ";"),
            (Code::Quote, "'"),
            (Code::Comma, ","),
            (Code::Period, "."),
            (Code::Slash, "/"),
            (Code::Backquote, "`"),
        ];

        for (code, label) in cases {
            let hotkey = HotKey::new(Some(Modifiers::ALT), code);
            assert_eq!(display_keys(hotkey), ["⌥", label], "{code:?} 展示名不符");
        }
    }

    /// from_egui → display_keys 联动：⌥Space / ⌥⇧F 与产品文案一致。
    #[test]
    fn recorded_hotkey_displays_like_product_copy() {
        let palette = from_egui(egui::Key::Space, egui::Modifiers::ALT).unwrap();
        assert_eq!(display_keys(palette), ["⌥", "Space"]);

        let window =
            from_egui(egui::Key::F, egui::Modifiers::ALT | egui::Modifiers::SHIFT).unwrap();
        assert_eq!(display_keys(window), ["⌥", "⇧", "F"]);
    }

    /// validate：无修饰键 → 「需包含修饰键」；白名单内带修饰键 → Ok。
    #[test]
    fn validate_requires_at_least_one_modifier() {
        for hotkey in [
            HotKey::new(None, Code::Space),
            HotKey::new(Some(Modifiers::empty()), Code::KeyA),
        ] {
            assert_eq!(validate(hotkey).unwrap_err(), NO_MODIFIER);
        }

        assert!(validate(HotKey::new(Some(Modifiers::ALT), Code::Space)).is_ok());
        assert!(validate(HotKey::new(Some(Modifiers::SUPER), Code::KeyF)).is_ok());
    }

    /// 手工编辑 settings.toml 写入白名单外的键（如 Escape）：
    /// 校验拒绝、提示「不支持该键」；展示层仍回落 canonical 名，不 panic 不丢键。
    #[test]
    fn validate_rejects_unsupported_persisted_code_with_canonical_display() {
        let escape = "alt+Escape".parse::<HotKey>().unwrap();
        let err = validate(escape).unwrap_err();
        assert_eq!(err, UNSUPPORTED_KEY);
        assert_eq!(err.to_string(), "不支持该键");
        assert_eq!(err.text(), i18n::s().hotkey_unsupported);
        assert_eq!(display_keys(escape), ["⌥", "Escape"], "展示回落规范名");

        // 白名单内键位对照：同样带修饰键 → Ok。
        assert!(validate("alt+Space".parse::<HotKey>().unwrap()).is_ok());
    }

    /// 每种 from_egui 映射出的键位都能通过 validate（两处共用 SUPPORTED_KEYS）。
    #[test]
    fn validate_accepts_every_recordable_key() {
        for &(key, _) in SUPPORTED_KEYS {
            let hotkey = from_egui(key, egui::Modifiers::ALT)
                .unwrap_or_else(|err| panic!("{key:?} 应在白名单内: {err}"));
            assert!(validate(hotkey).is_ok(), "{key:?} 映射结果须可注册");
        }
    }

    /// HotkeyError 实现 Display + std::error::Error（H3 内联错误可直接 to_string），
    /// 文案随当前语言切换。
    #[test]
    fn hotkey_error_displays_user_facing_text() {
        fn assert_error(err: &dyn std::error::Error) -> String {
            err.to_string()
        }

        i18n::with_lang(i18n::Lang::ZhHans, || {
            assert_eq!(assert_error(&NO_MODIFIER), "需包含修饰键");
            assert_eq!(assert_error(&UNSUPPORTED_KEY), "不支持该键");
            assert_eq!(NO_MODIFIER.text(), "需包含修饰键");
            assert_eq!(UNSUPPORTED_KEY.text(), "不支持该键");
        });

        i18n::with_lang(i18n::Lang::En, || {
            assert_eq!(assert_error(&NO_MODIFIER), "A modifier key is required");
            assert_eq!(assert_error(&UNSUPPORTED_KEY), "Unsupported key");
            assert_eq!(NO_MODIFIER.text(), "A modifier key is required");
            assert_eq!(UNSUPPORTED_KEY.text(), "Unsupported key");
        });
    }
}
