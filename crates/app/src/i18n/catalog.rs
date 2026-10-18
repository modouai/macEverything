//! 编译期文案目录：每条文案只声明一次，生成两套静态表。
//!
//! 宏保证两套表都填满 `Strings` 的全部字段——漏翻编译期报错。

macro_rules! catalog {
    ($( $field:ident : $zh:literal / $en:literal ),* $(,)?) => {
        #[derive(Debug, Clone, Copy)]
        pub struct Strings {
            $( pub $field: &'static str, )*
        }

        static ZH_HANS: Strings = Strings { $( $field: $zh, )* };
        static EN: Strings = Strings { $( $field: $en, )* };

        pub fn strings(lang: crate::i18n::Lang) -> &'static Strings {
            match lang {
                crate::i18n::Lang::ZhHans => &ZH_HANS,
                crate::i18n::Lang::En => &EN,
            }
        }

        pub fn s() -> &'static Strings {
            strings(crate::i18n::lang())
        }

        /// 测试清单（key, 中文, English）；仅供单测，生产不读。
        #[allow(dead_code)]
        pub fn all_entries() -> &'static [(&'static str, &'static str, &'static str)] {
            &[ $( (stringify!($field), $zh, $en), )* ]
        }
    };
}

catalog! {
    settings_title: "设置" / "Settings",
    // 分组 / 行
    group_appearance: "外观" / "Appearance",
    group_general: "通用" / "General",
    group_index: "索引" / "Index",
    row_font_size: "字号" / "Text Size",
    row_autostart: "开机自启" / "Launch at Login",
    row_language: "语言" / "Language",
    row_palette_hotkey: "唤起快捷键" / "Quick Search Shortcut",
    row_window_hotkey: "完整窗口快捷键" / "Full Window Shortcut",
    row_excluded: "排除目录" / "Excluded Folders",
    // 字号档
    font_small: "小" / "Small",
    font_standard: "标准" / "Standard",
    font_large: "大" / "Large",
    // 语言选项
    language_system: "跟随系统" / "System Default",
    language_zh_hans: "简体中文" / "简体中文",
    language_en: "English" / "English",
    // 排除目录
    add_folder: "＋ 添加目录…" / "Add Folder…",
    restore_defaults: "恢复默认" / "Restore Defaults",
    excluded_empty: "未排除任何目录" / "No excluded folders",
    exclude_loading: "加载中…" / "Loading…",
    exclude_rescanning: "重扫中…" / "Rescanning…",
    // 快捷键录制
    recording_prompt: "按下快捷键…" / "Press shortcut…",
    recording_hint: "按 Esc 或点击别处取消,Backspace 恢复默认" / "Press Esc or click elsewhere to cancel; Backspace restores the default",
    recording_hint_hover: "按 Esc 或点击别处取消录制,Backspace 恢复默认" / "Press Esc or click elsewhere to cancel recording; Backspace restores the default",
    keycap_hover_record: "点击录制快捷键" / "Click to record shortcut",
    keycap_hover_rerecord: "点击改为录制此快捷键" / "Click to record a new shortcut",
    // 提示 / 错误
    autostart_approval_note: "已注册,待系统设置「登录项」允许" / "Registered. Allow it in System Settings > Login Items.",
    reset_confirm_title: "恢复默认排除目录？" / "Restore default excluded folders?",
    reset_confirm_message: "将清空自定义排除项，恢复 ~/Library、node_modules、.git。此操作会触发一次索引重扫。" / "This clears custom exclusions and restores ~/Library, node_modules, and .git. This will trigger a full index rescan.",
    reset_confirm_action: "恢复默认" / "Restore Defaults",
    settings_save_failed: "设置保存失败" / "Failed to save settings",
    language_save_failed: "语言已切换,但设置保存失败" / "Language changed, but the settings could not be saved",
    autostart_read_failed: "读取开机自启状态失败" / "Failed to read Launch at Login status",
    autostart_not_effective: "开机自启未生效" / "Launch at Login did not take effect",
    autostart_set_failed: "开机自启设置失败" / "Failed to set Launch at Login",
    exclude_failed_no_reason: "排除目录操作失败(daemon 未给出原因)" / "Excluded folders operation failed (the engine did not provide a reason)",
    exclude_worker_exited: "排除目录工作线程异常退出(无结果)" / "Excluded folders worker exited unexpectedly (no result)",
    exclude_worker_start_failed: "无法启动排除目录工作线程" / "Could not start the excluded folders worker",
    // 自启状态说明
    autostart_status_enabled: "已启用(系统设置「登录项」可见)" / "Enabled (visible in System Settings > Login Items)",
    autostart_status_requires_approval: "已注册,待用户在系统设置「登录项」中允许" / "Registered. Awaiting approval in System Settings > Login Items",
    autostart_status_not_registered: "未注册(已关闭)" / "Not registered (off)",
    autostart_status_not_found: "未找到条目(注册尚未生效)" / "Entry not found (registration not yet effective)",
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placeholders(s: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut rest = s;
        while let Some(start) = rest.find('{') {
            let after = &rest[start + 1..];
            let Some(end) = after.find('}') else { break };
            out.push(&after[..end]);
            rest = &after[end + 1..];
        }
        out
    }

    #[test]
    fn entries_are_complete_and_placeholders_match() {
        for (key, zh, en) in all_entries() {
            assert!(!zh.trim().is_empty(), "{key} 中文为空");
            assert!(!en.trim().is_empty(), "{key} 英文为空");
            assert_eq!(zh.trim(), *zh, "{key} 中文首尾空白");
            assert_eq!(en.trim(), *en, "{key} 英文首尾空白");
            assert_eq!(placeholders(zh), placeholders(en), "{key} 中英占位符不一致");
        }
    }
}
