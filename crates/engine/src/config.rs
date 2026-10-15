//! 配置:应用数据目录下 config.toml。
//! v1 契约:仅 `excluded` 一字段(排除目录列表),无版本字段——单字段无演进面,
//! 未来加字段靠 serde default 兜底旧文件读取,破坏性格式变更才走版本化迁移。
//! 默认排除 ~/Library、node_modules、.git(product §1)。

use serde::{Deserialize, Serialize};
use std::path::Path;

/// 应用配置。
/// 双默认语义:`Default` derive 未实现(空 excluded 误用即漏扫 ~/Library),
/// 唯一默认入口是 [`default_config()`](三条基线排除)。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Config {
    /// 排除目录列表(条目语义由扫描侧解释)。
    #[serde(default)]
    pub excluded: Vec<String>,
}

/// 默认配置(product §1)。
pub fn default_config() -> Config {
    Config {
        excluded: vec!["~/Library".into(), "node_modules".into(), ".git".into()],
    }
}

/// 读配置。缺文件 → 默认;解析失败 → 告警后用默认,不崩溃、不动用户文件(留痕不静默)。
pub fn load(path: &Path) -> Config {
    match std::fs::read_to_string(path) {
        Ok(s) => toml::from_str(&s).unwrap_or_else(|e| {
            log::warn!("{} 解析失败,用默认配置: {e}", path.display());
            default_config()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => default_config(),
        Err(e) => {
            log::warn!("{} 读取失败,用默认配置: {e}", path.display());
            default_config()
        }
    }
}

/// 原子保存:先建父目录,写 `<目标>.tmp` 后 rename 替换,不留半写文件
/// (tmp 命名复用 persist::tmp_path_for)。
/// 取舍:不做 fsync——最坏断电配置损坏,load 的降级路径已兜住(回默认三条)。
pub fn save(cfg: &Config, path: &Path) -> std::io::Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = crate::persist::tmp_path_for(path)?;
    // Vec<String> 结构序列化不会失败,unwrap 安全
    std::fs::write(&tmp, toml::to_string_pretty(cfg).unwrap())?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let c = load(&p);
        assert_eq!(c, default_config()); // 缺文件 → 默认
        let mut c2 = default_config();
        c2.excluded.push("/tmp/x".into());
        save(&c2, &p).unwrap();
        assert_eq!(load(&p), c2);
    }

    #[test]
    fn corrupt_file_falls_back_to_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "excluded = [").unwrap(); // 非法 toml
        assert_eq!(load(&p), default_config());
    }

    #[test]
    fn forward_compat_unknown_field_and_missing_excluded() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "future_field = 1\n").unwrap();
        let c = load(&p);
        // 未知字段容忍;excluded 缺失走 serde default 得空列表。
        // 若解析失败降级,这里会是 default_config 的 3 条,断言即红。
        assert!(c.excluded.is_empty());
    }

    #[test]
    fn save_creates_missing_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a/b/config.toml");
        save(&default_config(), &p).unwrap();
        assert_eq!(load(&p), default_config());
    }
}
