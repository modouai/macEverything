//! 设置页顶部标签栏：枚举、几何、命中与自绘图标。
//!
//! 只负责标签栏本身；页面绘制在 `settings_view`，状态编排在 `settings`。
//!
//! Task 2/4 完成绘制与页面分发接线前，本模块尚无生产消费方：枚举与 `impl`
//! 上的 `#[allow(dead_code)]` 是临时接线标记，必须在接线后删除；后续新增
//! allow 也须窄作用域，不得恢复整模块 allow（`settings_tokens` 已按此收敛）。

use crate::i18n;

#[allow(dead_code)] // Task 3/4 接线前暂未消费。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SettingsTab {
    #[default]
    General,
    Index,
    Advanced,
    Experimental,
    About,
}

#[allow(dead_code)] // Task 2/4 接线前暂未消费。
impl SettingsTab {
    pub const ALL: [Self; 5] = [
        Self::General,
        Self::Index,
        Self::Advanced,
        Self::Experimental,
        Self::About,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::General => i18n::s().settings_tab_general,
            Self::Index => i18n::s().settings_tab_index,
            Self::Advanced => i18n::s().settings_tab_advanced,
            Self::Experimental => i18n::s().settings_tab_experimental,
            Self::About => i18n::s().settings_tab_about,
        }
    }

    pub fn scroll_id(self) -> &'static str {
        match self {
            Self::General => "settings-page-general",
            Self::Index => "settings-page-index",
            Self::Advanced => "settings-page-advanced",
            Self::Experimental => "settings-page-experimental",
            Self::About => "settings-page-about",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    #[test]
    fn order_is_general_index_advanced_experimental_about() {
        assert_eq!(
            SettingsTab::ALL,
            [
                SettingsTab::General,
                SettingsTab::Index,
                SettingsTab::Advanced,
                SettingsTab::Experimental,
                SettingsTab::About,
            ]
        );
    }

    #[test]
    fn default_is_general() {
        assert_eq!(SettingsTab::default(), SettingsTab::General);
    }

    #[test]
    fn labels_cover_both_languages() {
        for (lang, expected) in [
            (Lang::ZhHans, ["通用", "索引", "高级", "实验", "关于"]),
            (
                Lang::En,
                ["General", "Index", "Advanced", "Experimental", "About"],
            ),
        ] {
            crate::i18n::with_lang(lang, || {
                let labels: Vec<&str> = SettingsTab::ALL.iter().map(|tab| tab.label()).collect();
                assert_eq!(labels, expected);
            });
        }
    }

    #[test]
    fn scroll_ids_are_unique() {
        let ids: Vec<&str> = SettingsTab::ALL.iter().map(|tab| tab.scroll_id()).collect();
        assert!(ids.iter().all(|id| id.starts_with("settings-page-")));
        let mut dedup = ids.clone();
        dedup.sort_unstable();
        dedup.dedup();
        assert_eq!(dedup.len(), ids.len());
    }
}
