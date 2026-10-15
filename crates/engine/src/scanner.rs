//! 扫描器:目录遍历 → Index。Task 9 串行定语义,Task 10 并行化(结果必须一致)。
//!
//! 语义基线(ADR-6):
//! - 排除按路径前缀命中,整棵子树剪枝(目录既不入索引也不入队);
//! - 符号链接一律不入索引、不跟随(防环 + 不跨根);
//! - `read_dir` 的 PermissionDenied 单独归类 `gaps.permission`,其余 IO 错误计数
//!   `gaps.io_errors` 不中断——扫描对不可读子树降级为「缺口」,不整体失败;
//!   `io_errors` 含 `read_dir` 打开与迭代中途两类失败;
//! - 非 UTF-8 文件名按 `to_string_lossy`(U+FFFD 替换)收录;条目数不丢,名称字节
//!   与磁盘非严格一致;
//! - **调用方契约:roots 之间不得互相重叠(含相等)**——重叠根会重复收录同一物理
//!   子树且并行臂聚合键互相覆盖,与串行臂行为分歧;debug 构建断言拒绝(见
//!   `assert_disjoint_roots`)。
//!
//! 两条路径(串行/并行)的分类语义由 `classify_entry` 单点保证。
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

fn is_excluded(p: &Path, excludes: &[PathBuf]) -> bool {
    excludes.iter().any(|x| p.starts_with(x))
}

/// 调用方契约(debug 期执法):roots 两两不得前缀重叠(含相等——相等根同样
/// 重复收录且并行臂聚合键互相覆盖)。
fn assert_disjoint_roots(roots: &[PathBuf]) {
    debug_assert!(
        roots.iter().enumerate().all(|(i, a)| roots
            .iter()
            .enumerate()
            .all(|(j, b)| i == j || !b.starts_with(a))),
        "roots 不得互相重叠:重叠根会重复收录同一物理子树,且并行臂与串行臂行为分歧"
    );
}

/// 扫描入口:`threads <= 1` 走串行 BFS;`> 1` 走并行遍历(ADR-6)。
/// 两条路径的分类语义由 `classify_entry` 单点保证;验收 = 路径集合与串行一致。
/// 根目录条目名取路径尾段,parent = NO_PARENT。
pub fn scan_roots(roots: &[PathBuf], excludes: &[PathBuf], threads: usize) -> (Index, ScanGaps) {
    assert_disjoint_roots(roots);
    if threads <= 1 {
        scan_serial(roots, excludes)
    } else {
        scan_parallel(roots, excludes, threads)
    }
}

/// 目录项分类(两臂共用的语义核心,副作用留调用方):
/// symlink → `Skip`;排除目录 → `Skip`(整棵剪枝);IO 失败 → `Err`(调用方计 io_errors)。
fn classify_entry(entry: std::fs::DirEntry, excludes: &[PathBuf]) -> Result<Step, ()> {
    let ft = entry.file_type().map_err(|_| ())?;
    if ft.is_symlink() {
        return Ok(Step::Skip);
    }
    let path = entry.path();
    let name = entry.file_name().to_string_lossy().into_owned();
    if ft.is_dir() {
        if is_excluded(&path, excludes) {
            return Ok(Step::Skip);
        }
        Ok(Step::Dir { path, name })
    } else {
        Ok(Step::File { name })
    }
}

enum Step {
    Dir { path: PathBuf, name: String },
    File { name: String },
    Skip,
}

/// 串行 BFS 扫描(语义基线本体)。
fn scan_serial(roots: &[PathBuf], excludes: &[PathBuf]) -> (Index, ScanGaps) {
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
        for entry in rd {
            // ReadDir 迭代中途 readdir 失败产出 Err 项:计数不中断(flatten 会静默丢弃)
            let entry = match entry {
                Ok(e) => e,
                Err(_) => {
                    gaps.io_errors += 1;
                    continue;
                }
            };
            match classify_entry(entry, excludes) {
                Ok(Step::Dir { path, name }) => {
                    let idx = index.insert(&name, parent_idx, true);
                    queue.push_back((path, idx));
                }
                Ok(Step::File { name }) => {
                    index.insert(&name, parent_idx, false);
                }
                Ok(Step::Skip) => {}
                Err(()) => gaps.io_errors += 1,
            }
        }
    }
    (index, gaps)
}

/// 聚合记录:目录条目携带自身原始路径(`raw`,dir_map 键——目录名非 UTF-8 时
/// lossy 名重建的键会与弹队原始路径漂移,macOS 不可达但 Linux ext4 可触发)。
enum Rec {
    Dir {
        parent: PathBuf,
        name: String,
        raw: PathBuf,
    },
    File {
        parent: PathBuf,
        name: String,
    },
}

/// 并行遍历(ADR-6):共享目录队列(work-sharing:纯 FIFO 消费,空队即退),
/// 活跃线程 = min(threads, 可用逻辑核)。各线程把分类结果写入**线程本地分片**
/// (无逐条全局锁),scope 结束一次性上收;单线程两轮聚合保证「父条目号先于子 +
/// 路径集合确定」。注意:条目号/arena 布局随线程交织,不做字节级稳定——此为
/// ADR-6「分片按队列顺序聚合」的已登记偏差(见 plan 实现偏差 #3)。
fn scan_parallel(roots: &[PathBuf], excludes: &[PathBuf], threads: usize) -> (Index, ScanGaps) {
    use std::sync::Mutex;

    let queue: Mutex<VecDeque<PathBuf>> = Mutex::new(roots.iter().cloned().collect());
    let gaps: Mutex<ScanGaps> = Mutex::new(ScanGaps::default());
    let nthreads = threads.min(
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8),
    );
    let mut shards: Vec<Vec<Rec>> = (0..nthreads).map(|_| Vec::new()).collect();

    std::thread::scope(|s| {
        for shard in shards.iter_mut() {
            s.spawn(|| loop {
                let Some(dir) = queue.lock().unwrap().pop_front() else {
                    break;
                };
                let rd = match std::fs::read_dir(&dir) {
                    Ok(rd) => rd,
                    Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                        gaps.lock().unwrap().permission.push((dir, e.to_string()));
                        continue;
                    }
                    Err(_) => {
                        gaps.lock().unwrap().io_errors += 1;
                        continue;
                    }
                };
                for entry in rd {
                    let entry = match entry {
                        Ok(e) => e,
                        Err(_) => {
                            gaps.lock().unwrap().io_errors += 1;
                            continue;
                        }
                    };
                    match classify_entry(entry, excludes) {
                        Ok(Step::Dir { path, name }) => {
                            shard.push(Rec::Dir {
                                parent: dir.clone(),
                                name,
                                raw: path.clone(),
                            });
                            queue.lock().unwrap().push_back(path);
                        }
                        Ok(Step::File { name }) => {
                            shard.push(Rec::File {
                                parent: dir.clone(),
                                name,
                            });
                        }
                        Ok(Step::Skip) => {}
                        Err(()) => {
                            gaps.lock().unwrap().io_errors += 1;
                        }
                    }
                }
            });
        }
    });

    // 聚合(单线程):根先登记;记录经深度拓扑排序(见下),父目录必先于子处理;
    // 第一轮全部目录、第二轮全部文件为冗余兜底。
    let mut index = Index::new();
    let mut dir_map: std::collections::HashMap<PathBuf, u32> = std::collections::HashMap::new();
    for root in roots {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.to_string_lossy().into_owned());
        let idx = index.insert(&name, NO_PARENT, true);
        dir_map.insert(root.clone(), idx);
    }
    let mut recs: Vec<Rec> = shards.into_iter().flatten().collect();
    // 分片交织破坏 shard 内「父先于子」的因果序:子目录记录可能落在序号更小的
    // shard,flatten 后先于父被消费,dir_map 查父即 miss panic(全量套件实测
    // flake)。按路径深度稳定排序恢复拓扑序——父路径段数恒少于子,深度相同者
    // 互不为祖先、顺序无关。
    // sort_by_cached_key:key 为整路径 components 遍历,cached 版每记录只算一次
    // (sort_by_key 会按比较次数重复解析路径,百万记录下聚合段多付千万次遍历)
    recs.sort_by_cached_key(|r| match r {
        Rec::Dir { raw, .. } => raw.components().count(),
        Rec::File { parent, .. } => parent.components().count(),
    });
    for rec in &recs {
        if let Rec::Dir { parent, name, raw } = rec {
            let pidx = dir_map[parent];
            let idx = index.insert(name, pidx, true);
            dir_map.insert(raw.clone(), idx);
        }
    }
    for rec in &recs {
        if let Rec::File { parent, name } = rec {
            let pidx = dir_map[parent];
            index.insert(name, pidx, false);
        }
    }
    (index, gaps.into_inner().unwrap())
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
        // io_errors 含两类失败:read_dir 打开失败(fixture 不可控,权限钉单列)与
        // 迭代中途 readdir 失败(受控树难造,错误注入测试不可行,依赖代码审查——
        // review 时须确认迭代循环对 Err 项显式计数,勿回退 flatten 静默丢弃)
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

    /// Task 10:并行遍历结果必须与串行一致(ADR-6 验收)——
    /// 条目数、全路径集合、权限缺口数、IO 错误数四项全等。
    #[test]
    fn parallel_equals_serial() {
        let t = fixture();
        // 补一层嵌套:并行聚合的两轮法须正确处理隔代祖孙
        fs::create_dir_all(t.path().join("src/deep/deeper")).unwrap();
        fs::write(t.path().join("src/deep/deeper/x.txt"), "").unwrap();
        let roots = vec![t.path().to_path_buf()];
        let excl = vec![t.path().join("node_modules")];
        let (s_idx, s_gaps) = scan_roots(&roots, &excl, 1);
        let (p_idx, p_gaps) = scan_roots(&roots, &excl, 8);
        assert_eq!(s_idx.entry_count(), p_idx.entry_count(), "条目数一致");
        let s_paths: std::collections::BTreeSet<String> = (0..s_idx.entries.len() as u32)
            .map(|i| s_idx.full_path(i).to_string_lossy().into_owned())
            .collect();
        let p_paths: std::collections::BTreeSet<String> = (0..p_idx.entries.len() as u32)
            .map(|i| p_idx.full_path(i).to_string_lossy().into_owned())
            .collect();
        assert_eq!(s_paths, p_paths, "全路径集合一致 = 聚合确定性");
        assert_eq!(
            s_gaps.permission.len(),
            p_gaps.permission.len(),
            "权限缺口数一致"
        );
        assert_eq!(s_gaps.io_errors, p_gaps.io_errors, "IO 错误计数一致");
    }

    /// 多根 parity:两独立根,聚合时根登记顺序与 records 交织不得破坏结果。
    #[test]
    fn multi_root_parity() {
        let t1 = fixture();
        let t2 = tempfile::tempdir().unwrap();
        fs::create_dir_all(t2.path().join("nested")).unwrap();
        fs::write(t2.path().join("nested/other.txt"), "").unwrap();
        fs::write(t2.path().join("top.txt"), "").unwrap();
        let roots = vec![t1.path().to_path_buf(), t2.path().to_path_buf()];
        let (s_idx, _) = scan_roots(&roots, &[], 1);
        let (p_idx, _) = scan_roots(&roots, &[], 8);
        assert_eq!(s_idx.entry_count(), p_idx.entry_count());
        let s_paths: std::collections::BTreeSet<String> = (0..s_idx.entries.len() as u32)
            .map(|i| s_idx.full_path(i).to_string_lossy().into_owned())
            .collect();
        let p_paths: std::collections::BTreeSet<String> = (0..p_idx.entries.len() as u32)
            .map(|i| p_idx.full_path(i).to_string_lossy().into_owned())
            .collect();
        assert_eq!(s_paths, p_paths);
        // 两根各含自身子树(full_path 以根条目名(tempdir 尾段)开头,用 ends_with 判)
        assert!(s_paths.iter().any(|p| p.ends_with("nested/other.txt")));
        assert!(s_paths.iter().any(|p| p.ends_with("top.txt")));
    }

    /// 重叠根为调用方契约违规:debug 构建断言拒绝(模块头「调用方契约」)。
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "roots 不得互相重叠")]
    fn overlapping_roots_rejected_in_debug() {
        let t = fixture();
        scan_roots(&[t.path().to_path_buf(), t.path().join("src")], &[], 8);
    }

    /// 相等根同样违规(曾因 a == b 短路豁免,突变实证)。
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "roots 不得互相重叠")]
    fn equal_roots_rejected_in_debug() {
        let t = fixture();
        let root = t.path().to_path_buf();
        scan_roots(&[root.clone(), root], &[], 8);
    }

    /// 分片交织拓扑序压力钉:跨 shard 时子目录记录可先于父被消费(深度排序
    /// 修复前,全量套件 9 轮 1 次 dir_map miss panic)。深层嵌套 × 重复采样,
    /// 不 panic 即过;顺带钉条目数稳定。
    #[test]
    fn parallel_aggregation_topological_order_stress() {
        for round in 0..32 {
            let t = fixture();
            fs::create_dir_all(t.path().join("a/b/c/d/e")).unwrap();
            fs::write(t.path().join("a/b/c/d/e/x.txt"), "").unwrap();
            fs::create_dir_all(t.path().join("p/q/r")).unwrap();
            let roots = vec![t.path().to_path_buf()];
            let (idx, _) = scan_roots(&roots, &[], 8);
            // fixture 6(根/src/main.rs/README.md/.git/config)+ node_modules/pkg/i.js 3
            // + a/b/c/d/e/x 6 + p/q/r 3(无排除,全部入索引)
            assert_eq!(idx.entry_count(), 18, "round {round}");
        }
    }

    /// 并行臂权限分支钉:threads=8 下只读目录同样单独归类 permission
    /// (防「错误分类只在串行臂被钉住」的回归盲区;迭代中 IO 错误受控树
    /// 不可造,仍依赖代码审查——见 scan 测试内注释)。
    #[cfg(unix)]
    #[test]
    fn parallel_permission_denied_goes_to_gaps() {
        use std::os::unix::fs::PermissionsExt;
        let t = fixture();
        let locked = t.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_dir(&locked).is_ok() {
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
            return; // root/越权环境无意义,同串行钉
        }
        let (index, gaps) = scan_roots(&[t.path().to_path_buf()], &[], 8);
        assert_eq!(gaps.permission.len(), 1, "并行臂权限缺口单独归类");
        assert_eq!(gaps.permission[0].0, locked);
        assert_eq!(gaps.io_errors, 0);
        assert!(index.entry_count() > 0);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    }
}
