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
    cancel: "取消" / "Cancel",
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
    // 悬浮条
    search_placeholder: "搜索文件…" / "Search files…",
    open_in_full_window: "在完整窗口中打开（⌥↵）" / "Open in Full Window (⌥↵)",
    empty_start: "输入关键词开始搜索" / "Type to search",
    empty_start_sub: "可搜文件名，也可搜路径" / "Search file names or paths",
    empty_no_match: "无匹配项" / "No matches",
    empty_no_match_sub: "换个关键词试试" / "Try a different keyword",
    // 角标
    badge_building: "建索引中 {n}" / "Building index {n}",
    badge_reconciling: "补齐中" / "Reconciling",
    badge_rescanning: "重扫中" / "Rescanning",
    badge_offline_index: "离线索引" / "Offline index",
    badge_engine_not_running: "引擎未运行" / "Engine not running",
    badge_building_index: "索引构建中" / "Building index",
    badge_query_failed: "查询失败" / "Query failed",
    // 热键提示（持久）
    hotkey_unavailable: "快捷键不可用,可从托盘唤起" / "Shortcut unavailable; use the menu bar instead",
    hotkey_unavailable_reason: "可从托盘唤起" / "use the menu bar instead",
    window_hotkey_unavailable_reason: "可从托盘打开" / "open it from the menu bar",
    hotkey_resume_failed: "快捷键恢复失败,请重新录制" / "Failed to restore shortcut; please record it again",
    hotkey_fallback: "{default} 不可用,已改用 {name}" / "{default} unavailable; using {name} instead",
    hotkey_saved_palette_default: "保存的唤起快捷键 {custom} 不可用,已回落默认 {name}" / "Saved Quick Search shortcut {custom} is unavailable; reverted to default {name}",
    hotkey_saved_palette_other: "保存的唤起快捷键 {custom} 不可用,已改用 {name}" / "Saved Quick Search shortcut {custom} is unavailable; using {name} instead",
    hotkey_saved_palette_unavailable: "保存的唤起快捷键 {custom} 不可用,{reason}" / "Saved Quick Search shortcut {custom} is unavailable; {reason}",
    hotkey_saved_window_conflict_default: "保存的完整窗口快捷键 {custom} 与唤起键冲突,已回落默认 {name}" / "Full Window shortcut {custom} conflicts with Quick Search; reverted to default {name}",
    hotkey_saved_window_default: "保存的完整窗口快捷键 {custom} 不可用,已回落默认 {name}" / "Full Window shortcut {custom} is unavailable; reverted to default {name}",
    hotkey_saved_window_conflict_unavailable: "保存的完整窗口快捷键 {custom} 与唤起键冲突,{reason}" / "Full Window shortcut {custom} conflicts with Quick Search; {reason}",
    hotkey_saved_window_unavailable: "保存的完整窗口快捷键 {custom} 不可用,{reason}" / "Full Window shortcut {custom} is unavailable; {reason}",
    // 列头 / 搜索
    col_name: "名称" / "Name",
    col_size: "大小" / "Size",
    col_mtime: "修改时间" / "Date Modified",
    col_path: "路径" / "Path",
    settings_tooltip: "设置" / "Settings",
    // 状态栏
    status_sorting: "排序中…" / "Sorting…",
    engine_outdated_sort: "引擎版本过旧,排序未生效" / "Engine is outdated; sorting is unavailable",
    restarting: "重启中…" / "Restarting…",
    restart_engine: "重启引擎" / "Restart Engine",
    status_engine_not_ready: "引擎未就绪" / "Engine not ready",
    status_query_failed: "查询失败" / "Query failed",
    // 缺口面板
    gaps_more: "… 其余 {n} 个" / "… {n} more",
    // 索引状态
    index_ready: "就绪" / "Ready",
    index_reconciling: "索引补齐中(结果可能不全)" / "Reconciling index (results may be incomplete)",
    index_degraded: "索引重扫中(结果可能不全)" / "Rescanning index (results may be incomplete)",
    index_offline_since: "离线索引 · 生成于 {time}" / "Offline index · Generated {time}",
    index_building: "索引构建中" / "Building index",
    engine_starting: "引擎启动中" / "Engine starting",
    offline_index: "离线索引" / "Offline index",
    engine_not_running: "引擎未运行" / "Engine not running",
    // 空态
    empty_engine_starting: "启动引擎中" / "Starting engine",
    empty_wait: "请稍候…" / "Please wait…",
    empty_warming: "索引构建中" / "Building index",
    empty_warming_sub: "部分结果可能暂不可见" / "Some results may be temporarily unavailable",
    empty_search_failed: "搜索失败" / "Search failed",
    empty_search_failed_sub: "请重试或检查引擎状态" / "Try again or check the engine status",
    empty_searching: "搜索中…" / "Searching…",
    // 托盘
    tray_open_search: "打开搜索" / "Open Quick Search",
    tray_open_window: "打开完整窗口" / "Open Full Window",
    tray_autostart: "开机自启" / "Launch at Login",
    tray_settings: "设置…" / "Settings…",
    tray_quit: "退出" / "Quit",
    tray_state_building: "引擎:构建中" / "Engine: Building",
    tray_state_ready: "引擎:就绪" / "Engine: Ready",
    tray_state_reconciling: "引擎:补齐中" / "Engine: Reconciling",
    tray_state_degraded: "引擎:重扫中" / "Engine: Rescanning",
    tray_state_offline: "引擎:未运行(离线索引)" / "Engine: Not Running (Offline Index)",
    tray_state_not_running: "引擎:未运行" / "Engine: Not Running",
    // 右键菜单
    menu_open: "打开" / "Open",
    menu_open_in_finder: "在访达中打开" / "Open in Finder",
    menu_show_in_finder: "在访达中显示" / "Show in Finder",
    menu_copy_path: "拷贝路径" / "Copy Path",
    menu_locate_failed: "无法定位所在目录" / "Could not locate the containing folder",
    menu_file_missing: "文件不存在,索引可能待更新" / "File not found; the index may be out of date",
    menu_open_failed: "打开失败" / "Failed to open",
    menu_copy_failed: "拷贝路径失败" / "Failed to copy path",
    // 引擎拉起（用户可见）
    engine_not_found: "未找到引擎可执行文件(macEverything),已搜索以下路径:" / "Could not find the macEverything engine. Searched:",
    engine_not_found_hint: "请将 macEverything 放到 App 同目录,或加入 PATH 后重试。" / "Place macEverything next to the app or add it to PATH, then try again.",
    engine_locate_app_failed: "定位 App 可执行文件失败" / "Could not locate the app executable",
    engine_start_failed: "启动引擎失败" / "Failed to start the engine",
    engine_start_timeout: "引擎进程已拉起,但未能连接引擎 socket" / "The engine process started but did not accept connections",
    engine_restart_failed: "引擎重启失败" / "Failed to restart the engine",
    // 热键校验/运行时错误（一次性）
    hotkey_needs_modifier: "需包含修饰键" / "A modifier key is required",
    hotkey_unsupported: "不支持该键" / "Unsupported key",
    hotkey_register_failed: "注册失败(可能被系统或其他应用占用)" / "Registration failed (it may be in use by the system or another app)",
    hotkey_reset_failed: "恢复默认快捷键失败(可能被系统或其他应用占用)" / "Failed to restore the default shortcut (it may be in use by the system or another app)",
    hotkey_global_unavailable: "全局热键不可用" / "Global shortcuts unavailable",
    hotkey_restore_other_failed: "恢复另一快捷键 {name} 失败(可能被占用)" / "Failed to restore the other shortcut {name} (it may be in use)",
    hotkey_all_candidates_unavailable: "候选快捷键均不可用" / "No shortcut candidates are available",
    hotkey_duplicate: "该快捷键已用于另一个功能" / "This shortcut is already used for another feature",
    hotkey_saved_but_save_failed: "快捷键已生效,但设置保存失败" / "Shortcut applied, but settings could not be saved",
    hotkey_reset_but_save_failed: "快捷键已恢复默认,但设置保存失败" / "Default shortcut restored, but settings could not be saved",
    // 悬浮条打开失败（应用自产）
    palette_file_missing: "文件不存在,索引可能待更新" / "File not found; the index may be out of date",
    palette_open_failed: "打开失败" / "Failed to open",
    palette_action_enter: "进入" / "Open Folder",
    palette_action_open: "打开" / "Open",
    palette_footer_hide: "隐藏" / "Hide",
    palette_footer_full_window: "完整窗口" / "Full Window",
    palette_footer_finder: "访达" / "Finder",
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
