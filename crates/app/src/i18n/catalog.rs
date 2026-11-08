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
    settings_tab_general: "通用" / "General",
    settings_tab_index: "索引" / "Index",
    settings_tab_advanced: "高级" / "Advanced",
    settings_tab_experimental: "实验" / "Experimental",
    settings_tab_about: "关于" / "About",
    group_shortcuts: "快捷键" / "Shortcuts",
    group_appearance_startup: "外观与启动" / "Appearance & Startup",
    row_font_size: "字号" / "Text Size",
    row_autostart: "开机自启" / "Launch at Login",
    row_language: "语言" / "Language",
    row_palette_hotkey: "唤起快捷键" / "Quick Search Shortcut",
    row_window_hotkey: "完整窗口快捷键" / "Full Window Shortcut",
    row_excluded: "排除目录" / "Excluded Folders",
    group_volumes: "外接磁盘" / "External Disks",
    row_volumes_auto: "自动索引外接磁盘" / "Index External Disks Automatically",
    volumes_none: "未挂载本地外接磁盘" / "No local external disks mounted",
    volumes_unmounted: "未挂载" / "Not mounted",
    volumes_loading: "加载中…" / "Loading…",
    volumes_worker_exited: "卷偏好工作线程异常退出" / "Volume preferences worker exited unexpectedly",
    group_search: "搜索" / "Search",
    row_include_hidden: "包含隐藏项" / "Include Hidden Items",
    hidden_desc: "名称以 . 开头的文件和文件夹。关闭后不进入搜索结果。"
        / "Files and folders whose names start with a dot. Turn off to exclude them from search results.",
    hidden_loading: "加载中…" / "Loading…",
    hidden_worker_exited: "隐藏项偏好工作线程异常退出" / "Hidden items preferences worker exited unexpectedly",
    // 命令行工具安装器(v2.5)
    row_cli_tool: "命令行工具" / "Command-Line Tool",
    cli_install_button: "安装到 PATH" / "Install to PATH",
    cli_uninstall_button: "卸载" / "Uninstall",
    cli_repair_button: "修复为当前 App" / "Repair for This App",
    cli_retry_button: "重试" / "Retry",
    cli_checking: "检查中…" / "Checking…",
    cli_state_absent: "未安装" / "Not installed",
    cli_state_ours: "已安装，指向本 App" / "Installed for this app",
    cli_state_shadowed: "另有指向本 App 的链接（被遮蔽）" / "Another link to this app is shadowed",
    cli_state_other_app: "指向另一个 macEverything，不覆盖" / "Points to another macEverything; not overwritten",
    cli_state_foreign: "其他来源，不管理" / "Managed by another source",
    cli_state_broken: "链接失效" / "Link is broken",
    cli_state_broken_volume: "目标卷未挂载" / "Target volume is not mounted",
    cli_state_skipped: "PATH 前部的同名条目会被终端跳过" / "Earlier same-name entries are skipped by the terminal",
    cli_state_unreadable: "无法读取：{err}" / "Cannot read: {err}",
    cli_unavailable_dev: "开发构建不提供安装" / "Not available in a development build",
    cli_unavailable_moved: "先移到 Applications 并重启 App" / "Move to Applications and relaunch the app",
    cli_unavailable_bundled: "App 内置命令行缺失或损坏" / "The bundled command-line tool is missing or damaged",
    cli_unknown_path: "无法读取登录 shell 的 PATH" / "Couldn't read the login shell PATH",
    cli_worker_start_failed: "命令行工具检查线程异常退出" / "The command-line tool worker exited unexpectedly",
    cli_hint_add_path: "PATH 中没有可用目录；先把 {dir} 加入 PATH（勿由 App 修改 shell 配置），然后重试安装" / "No usable directory in PATH; add {dir} to PATH (the app won't edit your shell config), then retry installation",
    cli_fda_hint: "首次在终端运行可能需要完全磁盘访问权限" / "The first terminal run may need Full Disk Access",
    // 关于 / 更新
    row_version: "版本" / "Version",
    row_website_link: "官网" / "Website",
    row_auto_check: "启动时自动检查更新" / "Check for Updates at Launch",
    // 保存面板目录助手(设置行 + 授权引导 + 托盘待授权项)
    row_panel_assist: "目录助手" / "Directory Assist",
    row_panel_assist_desc: "在其他应用保存或打开文件时，浮出目录搜索并跳转。"
        / "Show directory search over save/open panels and jump to the selected directory.",
    panel_assist_unauthorized: "当前未授权，点击授权。" / "Not authorized — click to authorize.",
    panel_assist_enable_title: "开启保存面板目录助手？" / "Enable Save-Panel Directory Assist?",
    panel_assist_enable_message: "该功能目前为实验功能，请谨慎开启。开启后需要「设备控制和数据访问」权限，用于读取保存/打开面板的结构与路径，你可以在系统设置 → 隐私与安全性 → 设备控制和数据访问中随时关闭。"
        / "This is an experimental feature. It requires \"Device Control and Data Access\" permission to read the save/open panel structure and paths. You can disable it anytime in System Settings → Privacy & Security → Device Control and Data Access.",
    panel_assist_enable_cancel: "取消" / "Cancel",
    panel_assist_enable_confirm: "启用" / "Enable",
    tray_panel_assist_auth: "保存面板助手待授权…" / "Save-Panel Assist Needs Permission…",
    // 保存面板目录助手浮层
    overlay_hint: "输入目录名…" / "Type a directory name…",
    overlay_input_hint: "输入文字搜索，选择目录回车后自动跳转" / "Type to search; select a folder and press Return to jump",
    overlay_clipboard_hint: "路径已复制，按 ⌘⇧G 后 ⌘V 粘贴" / "Path copied — press ⌘⇧G then ⌘V",
    overlay_key_select: "选择" / "Select",
    overlay_key_jump: "跳转" / "Jump",
    overlay_key_close: "关闭" / "Close",
    update_open_failed: "无法打开下载页" / "Couldn't open the download page",
    update_status_idle: "尚未检查" / "Not checked yet",
    update_status_checking: "正在检查…" / "Checking…",
    update_status_up_to_date: "已是最新版本" / "You're up to date",
    update_status_failed: "检查失败，请稍后重试" / "Couldn't check for updates. Try again later.",
    update_action_check: "检查更新" / "Check for Updates",
    update_action_download: "前往下载" / "Download",
    tray_check_update: "检查更新…" / "Check for Updates…",
    tray_checking: "检查中…" / "Checking…",
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
    recording_hint: "按 Esc 或点击别处取消,Backspace 恢复默认" / "Press Esc or click elsewhere to cancel recording; Backspace restores the default",
    recording_hint_hover: "按 Esc 或点击别处取消录制,Backspace 恢复默认" / "Press Esc or click elsewhere to cancel recording; Backspace restores the default",
    keycap_hover_record: "点击录制快捷键" / "Click to record shortcut",
    keycap_hover_rerecord: "点击改为录制此快捷键" / "Click to record a new shortcut",
    // 提示 / 错误
    autostart_approval_note: "已注册,待系统设置「登录项」允许" / "Registered. Allow it in System Settings > Login Items.",
    reset_confirm_title: "恢复默认排除目录？" / "Restore default excluded folders?",
    reset_confirm_message: "将清空自定义排除项，恢复 ~/Library、node_modules、.git、~/.Trash。此操作会触发一次索引重扫。" / "This clears custom exclusions and restores ~/Library, node_modules, .git, and ~/.Trash. This will trigger a full index rescan.",
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
    menu_show_in_finder: "在访达中显示" / "Show in Finder",
    menu_quick_look: "快速查看" / "Quick Look",
    menu_copy_file: "拷贝文件" / "Copy File",
    menu_copy_path: "拷贝路径" / "Copy Path",
    menu_copy_name: "拷贝文件名" / "Copy File Name",
    menu_file_missing: "文件不存在,索引可能待更新" / "File not found; the index may be out of date",
    menu_open_failed: "打开失败" / "Failed to open",
    menu_copy_failed: "拷贝路径失败" / "Failed to copy path",
    quick_look_failed: "无法预览该项目" / "Could not preview this item",
    copy_file_failed: "拷贝文件失败" / "Failed to copy file",
    copy_name_failed: "拷贝文件名失败" / "Failed to copy file name",
    // 引擎拉起（用户可见）
    engine_not_found: "未找到引擎可执行文件(macEverything),已搜索以下路径:" / "Could not find the macEverything engine. Searched:",
    engine_not_found_hint: "请将 macEverything 放到 App 同目录,或加入 PATH 后重试。" / "Place macEverything next to the app or add it to PATH, then try again.",
    engine_locate_app_failed: "定位 App 可执行文件失败" / "Could not locate the app executable",
    engine_start_failed: "启动引擎失败" / "Failed to start the engine",
    engine_start_timeout: "引擎进程已拉起,但未能连接引擎 socket" / "The engine process started but did not accept connections",
    engine_start_timeout_hint: "请查看 {log} 了解启动失败原因" / "See {log} for details",
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
    palette_action_enter: "进入" / "Enter",
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
