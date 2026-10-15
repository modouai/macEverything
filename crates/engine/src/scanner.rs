//! 扫描器:目录遍历 → Index。Task 9 串行定语义,Task 10 并行化(结果必须一致)。
//!
//! 语义基线(ADR-6):
//! - 排除按路径前缀命中,整棵子树剪枝(目录既不入索引也不入队);
//! - 符号链接一律不入索引、不跟随(防环 + 不跨根);
//! - `read_dir` 的 PermissionDenied 单独归类 `gaps.permission`,其余 IO 错误计数
//!   `gaps.io_errors` 不中断——扫描对不可读子树降级为「缺口」,不整体失败。
use crate::entry::NO_PARENT;
use crate::index::Index;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

/// 扫描缺口:权限类逐条留痕(目录 + errno 文案),其余 IO 错误只计数。
#[derive(Default, Debug)]
pub struct ScanGaps {
    pub permission: Vec<(PathBuf, String)>, // (目录, errno 文案)
    pub io_errors: u32,
}

/// Task 10 并行化的参数载体;本任务未消费(测试直接调 `scan_roots` 三参形式)。
pub struct ScanOptions<'a> {
    pub roots: &'a [PathBuf],
    pub excludes: &'a [PathBuf],
    pub threads: usize, // Task 10 使用;1 = 串行
}

fn is_excluded(p: &Path, excludes: &[PathBuf]) -> bool {
    excludes.iter().any(|x| p.starts_with(x))
}

/// 串行 BFS 扫描:`threads` 本任务收下但忽略,恒串行(Task 10 实现并行)。
/// 根目录条目名取路径尾段,parent = NO_PARENT。
pub fn scan_roots(roots: &[PathBuf], excludes: &[PathBuf], threads: usize) -> (Index, ScanGaps) {
    let mut index = Index::new();
    let mut gaps = ScanGaps::default();
    let mut queue: VecDeque<(PathBuf, u32)> = VecDeque::new();
    for root in roots {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.to_string_lossy().into_owned());
        let idx = index.insert(&name, NO_PARENT, true);
        queue.push_back((root.clone(), idx));
    }
    while let Some((dir, parent_idx)) = queue.pop_front() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                gaps.permission.push((dir.clone(), e.to_string()));
                continue;
            }
            Err(_) => {
                gaps.io_errors += 1;
                continue;
            }
        };
        for entry in rd.flatten() {
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => {
                    gaps.io_errors += 1;
                    continue;
                }
            };
            if ft.is_symlink() {
                continue;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if ft.is_dir() {
                if is_excluded(&path, excludes) {
                    continue;
                }
                let idx = index.insert(&name, parent_idx, true);
                queue.push_back((path, idx));
            } else {
                index.insert(&name, parent_idx, false);
            }
        }
    }
    let _ = threads;
    (index, gaps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 受控目录树(Task 10 并行化复用同一 fixture):
    /// ```text
    /// <tmp>/
    /// ├── src/main.rs
    /// ├── node_modules/pkg/i.js   ← 排除目标
    /// ├── README.md
    /// ├── .git/config
    /// └── link.lnk → <tmp>/src    ← 符号链接
    /// ```
    fn fixture() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        fs::create_dir_all(r.join("src")).unwrap();
        fs::write(r.join("src/main.rs"), "").unwrap();
        fs::create_dir_all(r.join("node_modules/pkg")).unwrap();
        fs::write(r.join("node_modules/pkg/i.js"), "").unwrap();
        fs::write(r.join("README.md"), "").unwrap();
        fs::create_dir(r.join(".git")).unwrap();
        fs::write(r.join(".git/config"), "").unwrap();
        t
    }

    /// 深度 1(根的直接子项)条目名。根自身不参与——plan 语义:「断言只查顶层子项,
    /// 根名不参与」;NO_PARENT 只命中根,须再下探一层。
    fn top_level_names(index: &Index) -> Vec<String> {
        (0..index.entries.len() as u32)
            .filter(|i| {
                let p = index.entries[*i as usize].parent;
                p != NO_PARENT && index.entries[p as usize].parent == NO_PARENT
            })
            .map(|i| String::from_utf8_lossy(index.display_name_of(i)).into_owned())
            .collect()
    }

    #[test]
    fn scan_respects_excludes_and_links() {
        let t = fixture();
        let link = t.path().join("link.lnk");
        std::os::unix::fs::symlink(t.path().join("src"), &link).unwrap();
        let (index, gaps) = scan_roots(
            &[t.path().to_path_buf()],
            &[t.path().join("node_modules")],
            1,
        );
        assert_eq!(gaps.permission.len(), 0);
        assert_eq!(gaps.io_errors, 0);
        let names = top_level_names(&index);
        assert!(
            names.contains(&"src".to_string())
                && names.contains(&"README.md".to_string())
                && names.contains(&".git".to_string())
        );
        // 排除整棵子树
        assert!(!names.contains(&"node_modules".to_string()));
        // 符号链接不入索引
        assert!(!names.contains(&"link.lnk".to_string()));
        // 精确计数:根 + src + main.rs + README.md + .git + config,
        // node_modules(排除)与 link.lnk(链接)都不产生条目——Task 10 并行结果的一致性基线
        assert_eq!(index.entry_count(), 6);
    }

    #[cfg(unix)]
    #[test]
    fn permission_denied_dir_goes_to_gaps() {
        use std::os::unix::fs::PermissionsExt;
        let t = fixture();
        let locked = t.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        // root/越权环境读 000 目录不报错,该钉在此类环境无意义,跳过不硬造红灯
        if fs::read_dir(&locked).is_ok() {
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
            eprintln!("跳过:当前进程可越权读 000 目录(root?),权限缺口钉不适用");
            return;
        }
        let (index, gaps) = scan_roots(&[t.path().to_path_buf()], &[], 1);
        assert_eq!(gaps.permission.len(), 1, "只读目录应单独归类 permission");
        assert_eq!(gaps.permission[0].0, locked, "缺口路径即该只读目录");
        assert!(!gaps.permission[0].1.is_empty(), "errno 文案非空");
        assert_eq!(gaps.io_errors, 0, "权限缺口不重复计入 io_errors");
        // 目录本身入索引(其父目录清单读得到它),子树因不可读而不可达
        let locked_idx = (0..index.entries.len() as u32)
            .find(|i| {
                index.entries[*i as usize].parent != NO_PARENT
                    && String::from_utf8_lossy(index.display_name_of(*i)).into_owned() == "locked"
            })
            .expect("locked 目录应入索引");
        assert!(index.entries[locked_idx as usize].is_dir());
        assert!(index.children_of(locked_idx).is_empty());
        // 恢复权限,保 TempDir 清理(临时目录卸载不因 000 而残留)
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    }
}
