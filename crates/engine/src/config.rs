//! 配置:应用数据目录下 config.toml。
//! v1 契约:仅 `excluded` 一字段(排除目录列表),无版本字段——单字段无演进面,
//! 未来加字段靠 serde default 兜底旧文件读取,破坏性格式变更才走版本化迁移。
//! 默认排除 ~/Library、node_modules、.git、~/.Trash(product §1)。
//! `~/.Trash` 用绝对形态(非相对名 `.Trash`):只排除回收站本身,且它受 TCC
//! 保护,不排除会成为 status 里永久的权限缺口噪音。

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// 应用配置。
/// 双默认语义:`Default` derive 未实现(空 excluded 误用即漏扫 ~/Library),
/// 唯一默认入口是 [`default_config()`](四条基线排除)。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Config {
    /// 排除目录列表(条目语义由扫描侧解释)。
    #[serde(default)]
    pub excluded: Vec<String>,
    /// 外接磁盘自动索引主开关(external-volumes spec;默认开)。
    #[serde(default = "default_true")]
    pub auto_index_volumes: bool,
    /// 不自动索引的卷目录名列表(如 ["BackupU"];按 /Volumes/<名> 的名键控)。
    #[serde(default)]
    pub volumes_excluded: Vec<String>,
}

fn default_true() -> bool {
    true
}

/// 默认配置(product §1)。
pub fn default_config() -> Config {
    Config {
        excluded: vec![
            "~/Library".into(),
            "node_modules".into(),
            ".git".into(),
            "~/.Trash".into(),
            // 卷元数据目录(external-volumes):每个 APFS/HFS+ 卷自动生成,
            // 名字式排除对任意卷内同名生效
            ".fseventsd".into(),
            ".Trashes".into(),
            ".Spotlight-V100".into(),
            ".DocumentRevisions-V100".into(),
        ],
        auto_index_volumes: true,
        volumes_excluded: vec![],
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
/// 取舍:不做 fsync——最坏断电配置损坏,load 的降级路径已兜住(回默认四条)。
pub fn save(cfg: &Config, path: &Path) -> std::io::Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = crate::persist::tmp_path_for(path)?;
    // 0600:配置与索引/日志同目录同源(排除清单等用户数据),不应对同组可读
    // (umask 只兜 create 初始位,显式 mode 防放宽)。tmp 名唯一,create+truncate 安全。
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    // Vec<String> 结构序列化不会失败,unwrap 安全
    f.write_all(toml::to_string_pretty(cfg).unwrap().as_bytes())?;
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

    /// 默认排除四条:`~/.Trash` 用绝对形态只命中回收站,不用相对名 `.Trash`
    /// 以免误伤任意层级的同名目录;它受 TCC 保护,不排除会在 status 里留下
    /// 永久「Operation not permitted」缺口噪音。
    #[test]
    fn default_config_excludes_home_trash() {
        assert_eq!(
            default_config().excluded,
            vec![
                "~/Library",
                "node_modules",
                ".git",
                "~/.Trash",
                // 卷元数据目录(external-volumes):名字式,对任意卷内同名生效
                ".fseventsd",
                ".Trashes",
                ".Spotlight-V100",
                ".DocumentRevisions-V100",
            ]
        );
        assert!(default_config().auto_index_volumes);
        assert!(default_config().volumes_excluded.is_empty());
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
        // 若解析失败降级,这里会是 default_config 的 4 条,断言即红。
        assert!(c.excluded.is_empty());
    }

    #[test]
    fn save_creates_missing_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a/b/config.toml");
        save(&default_config(), &p).unwrap();
        assert_eq!(load(&p), default_config());
    }

    /// config.toml 0600:排除清单等用户数据与索引/日志同源,不应对同组可读。
    /// 突变「std::fs::write 默认 0644」此测必红。
    #[test]
    fn saved_config_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        save(&default_config(), &p).unwrap();
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
