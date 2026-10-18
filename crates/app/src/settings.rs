//! v2.1 设置持久化：数据目录 `settings.toml`。
//!
//! 字号与两个全局快捷键入此文件；开机自启的真源是 SMAppService，排除目录的
//! 真源是引擎配置，均不写入本文件（design §三）。
//!
//! 读取：文件缺失 → 默认值；损坏/读取失败 → 默认值 + `log::warn`，且**不改写**
//! 原文件（坏文件留待人工或后续版本修复）。写入：同目录 tmp + rename 原子替换，
//! 失败以 `anyhow::Result` 交给调用方（设置页只留痕，不中断）。

use std::path::{Path, PathBuf};

use anyhow::Context;
use global_hotkey::hotkey::HotKey;
use serde::{Deserialize, Serialize};

use crate::i18n::LanguageSetting;

/// 文件名（模块私有；路径一律经 [`path`] 取得，防调用方自行拼接）。
const FILE_NAME: &str = "settings.toml";

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
    pub language: LanguageSetting,
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
///
/// 写入/替换失败会清理临时文件，并返回带路径上下文的错误（调用方留痕即可，
/// 不中断设置流程）。
pub fn save(data_dir: &Path, f: &SettingsFile) -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("创建设置目录失败({})", data_dir.display()))?;
    let p = path(data_dir);
    let tmp = data_dir.join(format!("{FILE_NAME}.tmp"));
    let text = toml::to_string_pretty(f).context("序列化设置失败")?;
    if let Err(e) = std::fs::write(&tmp, text) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("写入设置临时文件失败({})", tmp.display()));
    }
    if let Err(e) = std::fs::rename(&tmp, &p) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("替换设置文件失败({})", p.display()));
    }
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
        assert_eq!(FILE_NAME, "settings.toml");
        assert_eq!(p, dir.path().join("settings.toml"), "路径口径");
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
            language: LanguageSetting::System,
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

        // serde kebab-case（settings.toml 的存储口径）。
        let text = toml::to_string(&SettingsFile {
            font_size: FontSize::Large,
            ..Default::default()
        })
        .unwrap();
        assert!(text.contains("font_size = \"large\""), "{text}");
    }

    /// 钉住 global-hotkey 0.6.4 的规范序列化文本（大小写/修饰键顺序固定）。
    /// 输入侧解析兼容 `Alt+Space` / `Alt+Shift+F` 等写法，保存后统一为规范形式。
    #[test]
    fn hotkey_serialization_is_canonical() {
        let f = SettingsFile {
            font_size: FontSize::Standard,
            language: LanguageSetting::System,
            palette_hotkey: Some("Alt+Space".parse().unwrap()),
            window_hotkey: Some("Alt+Shift+F".parse().unwrap()),
        };
        let text = toml::to_string(&f).unwrap();
        assert!(text.contains("palette_hotkey = \"alt+Space\""), "{text}");
        assert!(
            text.contains("window_hotkey = \"shift+alt+KeyF\""),
            "{text}"
        );
        assert_eq!(
            toml::from_str::<SettingsFile>(&text).unwrap(),
            f,
            "规范文本可回读"
        );
    }

    /// 部分文件与未知键：缺字段取默认，未知键忽略（前向兼容）。
    #[test]
    fn partial_and_unknown_keys_load_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            path(dir.path()),
            "font_size = \"large\"\nfuture_key = { nested = true }\n",
        )
        .unwrap();
        let loaded = load(dir.path());
        assert_eq!(loaded.font_size, FontSize::Large);
        assert!(loaded.palette_hotkey.is_none());
        assert!(loaded.window_hotkey.is_none());
    }

    /// 保存成功不残留 tmp；rename 失败（目标被目录占用）返回 Err 且清理 tmp。
    #[test]
    fn save_cleans_tmp_and_reports_rename_failure() {
        let dir = tempfile::tempdir().unwrap();
        let f = SettingsFile::default();
        let tmp = dir.path().join("settings.toml.tmp");

        save(dir.path(), &f).unwrap();
        assert!(!tmp.exists(), "成功保存后不得残留临时文件");

        let target = path(dir.path());
        std::fs::remove_file(&target).unwrap();
        std::fs::create_dir(&target).unwrap();
        let err = save(dir.path(), &f).unwrap_err();
        let message = format!("{err:#}");
        assert!(
            message.contains("settings.toml"),
            "错误须带路径上下文: {message}"
        );
        assert!(!tmp.exists(), "失败后必须清理临时文件");
    }

    #[test]
    fn language_defaults_and_unknown_value_falls_back_to_system() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        std::fs::write(&p, "font_size = \"large\"\nlanguage = \"klingon\"\n").unwrap();
        let f = load(dir.path());
        assert_eq!(f.language, LanguageSetting::System);
        assert_eq!(f.font_size, FontSize::Large, "未知语言不得拖垮其它字段");
    }

    /// 非字符串语言值（数字/布尔）也按跟随系统处理，且不重置同文件的其它字段。
    #[test]
    fn non_string_language_values_fall_back_without_resetting_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());

        std::fs::write(&p, "font_size = \"large\"\nlanguage = 1\n").unwrap();
        let f = load(dir.path());
        assert_eq!(f.language, LanguageSetting::System);
        assert_eq!(f.font_size, FontSize::Large, "数字语言不得重置整个文件");

        std::fs::write(&p, "font_size = \"large\"\nlanguage = true\n").unwrap();
        let f = load(dir.path());
        assert_eq!(f.language, LanguageSetting::System);
        assert_eq!(f.font_size, FontSize::Large, "布尔语言不得重置整个文件");
    }

    #[test]
    fn language_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let f = SettingsFile {
            language: LanguageSetting::En,
            ..SettingsFile::default()
        };
        save(dir.path(), &f).unwrap();
        assert_eq!(load(dir.path()).language, LanguageSetting::En);
    }
}
