//! v2.1 设置持久化：数据目录 `settings.toml`。
//!
//! 字号与两个全局快捷键入此文件；开机自启的真源是 SMAppService，排除目录的
//! 真源是引擎配置，均不写入本文件（design §三）。
//!
//! 读取：文件缺失 → 默认值；损坏/读取失败 → 默认值 + `log::warn`，且**不改写**
//! 原文件（坏文件留待人工或后续版本修复）。写入：同目录 tmp + rename 原子替换，
//! 失败以 `anyhow::Result` 交给调用方（设置页只留痕，不中断）。

// 本模块由 Task 3（启动读取字号）/ Task 5（设置页写入）接入；接入前避免
// dead_code 告警。接入后移除本 allow（届时全部 API 均应有消费者）。
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use global_hotkey::hotkey::HotKey;
use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "settings.toml";

/// 全局字号倍率档（design §四：0.9 / 1.0 / 1.15；几何尺寸不变）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum FontSize {
    Small,
    #[default]
    Standard,
    Large,
}

impl FontSize {
    /// 文本级全局倍率（基准字号 × scale；几何不缩放）。
    pub fn scale(self) -> f32 {
        match self {
            Self::Small => 0.9,
            Self::Standard => 1.0,
            Self::Large => 1.15,
        }
    }

    /// 分段控件显示文案。
    pub fn label(self) -> &'static str {
        match self {
            Self::Small => "小",
            Self::Standard => "标准",
            Self::Large => "大",
        }
    }
}

/// `settings.toml` 的完整内容（未知键忽略，缺字段取默认——前向兼容）。
///
/// 两个快捷键 `None` = 沿用内置默认（⌥Space 降级链 / ⌥⇧F），不写死到文件，
/// 以便以后调整默认键时老用户跟随新默认。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SettingsFile {
    #[serde(default)]
    pub font_size: FontSize,
    #[serde(default)]
    pub palette_hotkey: Option<HotKey>,
    #[serde(default)]
    pub window_hotkey: Option<HotKey>,
}

/// 设置文件路径（`data_dir/settings.toml`）。
pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

/// 读取设置：无文件 / 读取失败 / 解析失败一律默认值；后两者留痕。
///
/// 不写回文件——损坏内容原样保留（与 `window_state::load` 的「只读」纪律一致）。
pub fn load(data_dir: &Path) -> SettingsFile {
    let p = path(data_dir);
    match std::fs::read_to_string(&p) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
            log::warn!("设置解析失败({}),用默认值: {e}", p.display());
            SettingsFile::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SettingsFile::default(),
        Err(e) => {
            log::warn!("设置读取失败({}),用默认值: {e}", p.display());
            SettingsFile::default()
        }
    }
}

/// 原子保存：建目录 → 同目录 `settings.toml.tmp` → rename 替换。
pub fn save(data_dir: &Path, f: &SettingsFile) -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let p = path(data_dir);
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string_pretty(f)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use global_hotkey::hotkey::{Code, Modifiers};

    #[test]
    fn defaults_roundtrip_and_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        assert_eq!(p, dir.path().join(FILE_NAME));
        assert_eq!(load(dir.path()), SettingsFile::default(), "无文件 = 默认");

        let f = SettingsFile::default();
        assert_eq!(f.font_size, FontSize::Standard);
        assert!(f.palette_hotkey.is_none());
        assert!(f.window_hotkey.is_none());
        save(dir.path(), &f).unwrap();
        assert_eq!(load(dir.path()), f);

        // 非默认值 + 两个热键一并往返（HotKey 以字符串序列化）。
        let custom = SettingsFile {
            font_size: FontSize::Large,
            palette_hotkey: Some(HotKey::new(Some(Modifiers::ALT), Code::Space)),
            window_hotkey: Some(HotKey::new(
                Some(Modifiers::ALT | Modifiers::SHIFT),
                Code::KeyF,
            )),
        };
        save(dir.path(), &custom).unwrap();
        assert_eq!(load(dir.path()), custom);

        // 损坏文件 → 默认值，且不覆盖原文件（留待用户/后续版本修复）。
        std::fs::write(&p, "font_size = [").unwrap();
        assert_eq!(load(dir.path()), SettingsFile::default());
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "font_size = [",
            "load 不得改写损坏文件"
        );
    }

    #[test]
    fn font_size_scales() {
        assert_eq!(FontSize::Small.scale(), 0.9);
        assert_eq!(FontSize::Standard.scale(), 1.0);
        assert_eq!(FontSize::Large.scale(), 1.15);
        assert_eq!(FontSize::Small.label(), "小");
        assert_eq!(FontSize::Standard.label(), "标准");
        assert_eq!(FontSize::Large.label(), "大");

        // serde kebab-case（settings.toml 的存储口径）。
        let text = toml::to_string(&SettingsFile {
            font_size: FontSize::Large,
            ..Default::default()
        })
        .unwrap();
        assert!(text.contains("font_size = \"large\""), "{text}");
    }
}
