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
    /// 搜索结果是否包含隐藏项（名称以 `.` 开头）。
    /// None = 旧配置缺字段，等待 [`load_migrating`] 迁移。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_hidden: Option<bool>,
}

impl Config {
    /// 生效值；未迁移的纯内存路径将 None 视为不包含。
    pub fn include_hidden_effective(&self) -> bool {
        self.include_hidden.unwrap_or(false)
    }
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
        include_hidden: None,
    }
}

/// [`load_detailed`] 的结果:区分「可解析」「缺文件」「失败」,供迁移入口
/// 判断写回是否安全(失败时不得用默认值覆盖用户文件)。
enum Loaded {
    Ok(Config),
    Missing,
    Failed,
}

/// 详细读取:解析/读取失败只告警并归为 [`Loaded::Failed`],不改动用户文件。
fn load_detailed(path: &Path) -> Loaded {
    match std::fs::read_to_string(path) {
        Ok(s) => match toml::from_str(&s) {
            Ok(c) => Loaded::Ok(c),
            Err(e) => {
                log::warn!("{} 解析失败,用默认配置: {e}", path.display());
                Loaded::Failed
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Loaded::Missing,
        Err(e) => {
            log::warn!("{} 读取失败,用默认配置: {e}", path.display());
            Loaded::Failed
        }
    }
}

/// 读配置。缺文件 → 默认;解析失败 → 告警后用默认,不崩溃、不动用户文件(留痕不静默)。
pub fn load(path: &Path) -> Config {
    match load_detailed(path) {
        Loaded::Ok(c) => c,
        Loaded::Missing | Loaded::Failed => default_config(),
    }
}

/// 读配置并完成旧配置迁移。
/// `index_present` = 数据目录已有落盘索引（升级信号）：
/// 字段缺失时写入 `Some(index_present)` 并原子落盘；写失败只告警，内存仍按迁移值。
///
/// 仅对「可解析」与「缺文件」迁移；解析/读取失败时不动磁盘，只在内存里按
/// `index_present` 给出生效值（升级用户保持旧行为：包含隐藏项），
/// 避免用默认值覆盖用户的损坏/不可读文件（与 [`load`] 的「不动用户文件」契约一致）。
/// 迁移无跨进程锁：多进程并发写回为 last-writer-wins。
pub fn load_migrating(path: &Path, index_present: bool) -> Config {
    let mut cfg = match load_detailed(path) {
        Loaded::Ok(c) => c,
        Loaded::Missing => default_config(),
        Loaded::Failed => {
            // 损坏/不可读：文件保持原样，内存按升级信号决定是否包含隐藏项。
            let mut cfg = default_config();
            cfg.include_hidden = Some(index_present);
            return cfg;
        }
    };
    if cfg.include_hidden.is_none() {
        cfg.include_hidden = Some(index_present);
        if let Err(e) = save(&cfg, path) {
            log::warn!(
                "{} 隐藏项偏好迁移写盘失败（内存仍生效）: {e}",
                path.display()
            );
        }
    }
    cfg
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

    /// 旧配置（缺字段）+ 已有落盘索引 = 升级用户：迁移为包含并写回。
    #[test]
    fn load_migrating_legacy_config_with_index_includes_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "excluded = [\"node_modules\"]\n").unwrap();
        let c = load_migrating(&p, true);
        assert_eq!(c.include_hidden, Some(true));
        assert_eq!(c.excluded, vec!["node_modules"], "迁移须保留其它设置");
        assert_eq!(load(&p).include_hidden, Some(true), "迁移结果须写回");
    }

    /// 旧配置 + 无落盘索引 = 新装：迁移为不包含并写回。
    #[test]
    fn load_migrating_fresh_install_excludes_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let c = load_migrating(&p, false);
        assert_eq!(c.include_hidden, Some(false));
        assert_eq!(load(&p).include_hidden, Some(false));
    }

    /// 显式值不被迁移覆盖（用户已关掉→升级后仍关）。
    #[test]
    fn load_migrating_keeps_explicit_value() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, "include_hidden = false\n").unwrap();
        assert_eq!(load_migrating(&p, true).include_hidden, Some(false));
        assert_eq!(load(&p).include_hidden, Some(false));
    }

    /// 纯读入口不产生副作用：缺文件时不创建配置。
    #[test]
    fn load_does_not_write_config() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let c = load(&p);
        assert_eq!(c.include_hidden, None);
        assert!(!p.exists(), "load 不得写盘");
    }

    /// 损坏配置不得被迁移覆盖：迁移只针对可解析/缺文件，失败时原样保留用户文件；
    /// 内存仍按升级信号给出生效值（升级用户保持旧行为：包含隐藏项）。
    #[test]
    fn load_migrating_does_not_overwrite_corrupt_config() {
        let corrupt = "excluded = [";

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        std::fs::write(&p, corrupt).unwrap();
        let c = load_migrating(&p, true);
        assert_eq!(c.include_hidden, Some(true), "升级用户内存值保持旧行为");
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            corrupt,
            "不得覆盖损坏配置"
        );

        let dir2 = tempfile::tempdir().unwrap();
        let p2 = dir2.path().join("config.toml");
        std::fs::write(&p2, corrupt).unwrap();
        let c2 = load_migrating(&p2, false);
        assert_eq!(c2.include_hidden, Some(false), "新装用户内存值不包含");
        assert_eq!(
            std::fs::read_to_string(&p2).unwrap(),
            corrupt,
            "不得覆盖损坏配置"
        );
    }

    /// 写盘失败不应中断迁移：内存仍按迁移值生效，只告警。
    #[cfg(unix)]
    #[test]
    fn load_migrating_survives_write_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        // 目录只读 → save 的 tmp 创建失败。
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        // 探测：若进程/文件系统忽略目录权限（如 root），本测试无意义，提前恢复并返回。
        let probe = dir.path().join("probe");
        let writable = std::fs::write(&probe, "x").is_ok();
        if writable {
            std::fs::remove_file(&probe).ok();
        }
        if writable {
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            return;
        }
        let c = load_migrating(&p, true);
        // 恢复目录权限，保证 tempdir 可清理。
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(c.include_hidden, Some(true));
    }
}
