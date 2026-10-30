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
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

/// 扫描缺口:权限类逐条留痕(目录 + errno 文案),其余 IO 错误只计数。
#[derive(Default, Debug)]
pub struct ScanGaps {
    pub permission: Vec<(PathBuf, String)>, // (目录, errno 文案)
    pub io_errors: u32,
}

/// 排除判定:绝对路径排除按前缀命中(整棵子树剪枝);相对名排除(如
/// `node_modules`、`.git`)按「路径任一组件等于该名」匹配——语义见
/// `reconcile::expand_excludes` 契约(product §1 默认排除即名字式)。
/// 首扫(scanner)、增量(watcher::apply_dir_diff)与 daemon 补链共用此判定,
/// 三条路径语义一致。
pub fn is_excluded(p: &Path, excludes: &[PathBuf]) -> bool {
    excludes.iter().any(|x| {
        if x.is_absolute() {
            p.starts_with(x)
        } else {
            p.components().any(|c| c.as_os_str() == x.as_os_str())
        }
    })
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
        // 根条目名 = 完整绝对路径:full_path 沿父链拼接后即为可直接使用的绝对路径
        // (`/Users/x/Docs/a.txt`);若只存尾段名,结果路径会丢前导目录。
        let name = root.to_string_lossy().into_owned();
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

/// 聚合记录:目录表引用 + 名称(峰值瘦身,ADR-6 增补)。
///
/// 旧设计逐条拷贝完整父路径(`parent: PathBuf` + Dir 的 `raw: PathBuf`),百万
/// 级扫描的 `Vec<Rec>` 是全量重扫峰值内存的主项;改为 `dir_seq` 引用目录表后,
/// 路径串仅在表内存一份,记录体 32B/条。pub:daemon 首扫流式聚合复用(分片可见)。
pub enum Rec {
    /// 目录条目。`dir_seq` 指向**自身**在目录表中的位置(入队时刻登记,与产出
    /// 记录同 shard 同原子时刻),聚合时 raw = table[dir_seq] 直接取用——完整
    /// 原始字节,目录名非 UTF-8 时键不漂移(旧 raw 存在的理由,逐字保留)。
    Dir { dir_seq: u32, name: String },
    /// 文件条目。`dir_seq` 指向**父目录**。
    File { dir_seq: u32, name: String },
}

/// 目录路径表:并行遍历侧按 shard 本地登记(每目录恰一次,入队时刻),聚合侧
/// 合并为连续存储。`dir_seq` 全局编码 = `shard << SEQ_SHARD_BITS | local`,
/// 单 shard 目录数上限 `1 << SEQ_SHARD_BITS`(越界 panic——2^20 目录/shard,
/// 远超现实规模)。
pub struct DirTable {
    paths: Vec<PathBuf>,
    offsets: Vec<usize>, // 每 shard 在 paths 中的起点
}

/// dir_seq 编码的 shard 位宽(见 DirTable)。
const SEQ_SHARD_BITS: u32 = 12;

impl DirTable {
    fn encode(shard: usize, local: usize) -> u32 {
        assert!(
            local < (1usize << (32 - SEQ_SHARD_BITS)),
            "单 shard 目录数超出 dir_seq 编码位宽"
        );
        ((shard as u32) << (32 - SEQ_SHARD_BITS)) | local as u32
    }

    fn shard_of(seq: u32) -> usize {
        (seq >> (32 - SEQ_SHARD_BITS)) as usize
    }

    #[must_use]
    pub fn get(&self, seq: u32) -> &Path {
        let shard = Self::shard_of(seq);
        let local = (seq & ((1 << (32 - SEQ_SHARD_BITS)) - 1)) as usize;
        &self.paths[self.offsets[shard] + local]
    }
}

/// 并行遍历(ADR-6):共享目录队列(work-sharing:纯 FIFO 消费,空队即退),
/// 活跃线程 = min(threads, 可用逻辑核)。各线程把分类结果写入**线程本地分片**
/// (无逐条全局锁),scope 结束一次性上收;记录按路径深度稳定排序后才返回,
/// 保证聚合侧「父记录先于子记录」。注意:条目号/arena 布局随线程交织,不做
/// 字节级稳定——此为 ADR-6「分片按队列顺序聚合」的已登记偏差(见 plan 偏差 #3)。
///
/// 目录表登记规则(峰值瘦身):队列元素携带目录的 dir_seq;目录**入队时刻**
/// 由产出其记录的 shard 登记路径(每目录恰一次),该 Dir 记录同 shard 引用同一
/// seq——记录与登记天然同 shard,无需跨线程同步;子记录引用父 seq(可能跨
/// shard),聚合侧经 DirTable 编码定位。返回 (records, table, gaps)。
pub fn collect_records(
    roots: &[PathBuf],
    excludes: &[PathBuf],
    threads: usize,
) -> (Vec<Rec>, DirTable, ScanGaps) {
    use std::sync::Mutex;

    let nthreads = threads.min(
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8),
    );
    let mut shard_dirs: Vec<Vec<PathBuf>> = (0..nthreads).map(|_| Vec::new()).collect();
    let mut shards: Vec<Vec<Rec>> = (0..nthreads).map(|_| Vec::new()).collect();

    // 根目录预先登记在 shard 0(shard 尚未 spawn,主线程独占无锁)。
    let mut queue: VecDeque<(PathBuf, u32)> = VecDeque::new();
    for root in roots {
        let seq = DirTable::encode(0, shard_dirs[0].len());
        shard_dirs[0].push(root.clone());
        queue.push_back((root.clone(), seq));
    }
    let queue: Mutex<VecDeque<(PathBuf, u32)>> = Mutex::new(queue);
    let gaps: Mutex<ScanGaps> = Mutex::new(ScanGaps::default());
    // 闭包为 move(需捕获各自 shard 的可变借用);queue/gaps 以共享引用捕获
    //(&Mutex 为 Copy,每线程一份引用副本,不 move 本体);目录表按迭代器切分
    // 不相交借用(索引式 &mut shard_dirs[sid] 无法通过借用检查——spawn 借用
    // 存活至 scope 结束)。
    let queue = &queue;
    let gaps_ref = &gaps;
    let mut dirs_iter = shard_dirs.iter_mut();

    std::thread::scope(|s| {
        for (sid, shard) in shards.iter_mut().enumerate() {
            let dirs = dirs_iter.next().unwrap();
            s.spawn(move || loop {
                let Some((dir, seq)) = queue.lock().unwrap().pop_front() else {
                    break;
                };
                let rd = match std::fs::read_dir(&dir) {
                    Ok(rd) => rd,
                    Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                        gaps_ref
                            .lock()
                            .unwrap()
                            .permission
                            .push((dir, e.to_string()));
                        continue;
                    }
                    Err(_) => {
                        gaps_ref.lock().unwrap().io_errors += 1;
                        continue;
                    }
                };
                for entry in rd {
                    let entry = match entry {
                        Ok(e) => e,
                        Err(_) => {
                            gaps_ref.lock().unwrap().io_errors += 1;
                            continue;
                        }
                    };
                    match classify_entry(entry, excludes) {
                        Ok(Step::Dir { path, name }) => {
                            // 子目录:登记自身路径取得 seq(与 Dir 记录同 shard),
                            // 入队供后续处理其子树。
                            let child_seq = DirTable::encode(sid, dirs.len());
                            dirs.push(path.clone());
                            shard.push(Rec::Dir {
                                dir_seq: child_seq,
                                name,
                            });
                            queue.lock().unwrap().push_back((path, child_seq));
                        }
                        Ok(Step::File { name }) => {
                            shard.push(Rec::File { dir_seq: seq, name });
                        }
                        Ok(Step::Skip) => {}
                        Err(()) => {
                            gaps_ref.lock().unwrap().io_errors += 1;
                        }
                    }
                }
            });
        }
    });

    // 合并各 shard 目录表为连续存储(编码不变,offsets 定位)。
    let mut paths = Vec::new();
    let mut offsets = Vec::with_capacity(nthreads);
    for d in &shard_dirs {
        offsets.push(paths.len());
        paths.extend(d.iter().cloned());
    }
    let table = DirTable { paths, offsets };

    let mut recs: Vec<Rec> = shards.into_iter().flatten().collect();
    // 分片交织破坏 shard 内「父先于子」的因果序:子目录记录可能落在序号更小的
    // shard,flatten 后先于父被消费,dir_map 查父即 miss panic(全量套件实测
    // flake)。按**条目自身在树中的深度**稳定排序恢复拓扑序:目录的自身深度 =
    // 其表内路径段数;文件的自身深度 = 父路径段数 + 1——同深度的目录记录必先于
    // 其直接子文件(单轮聚合含跨类型父子,「文件用父深度」会让两者同深度,
    // 反序时子先于父即 miss)。深度相同者互不为祖先,顺序无关。
    // sort_by_cached_key:key 为整路径 components 遍历,cached 版每记录只算一次
    // (sort_by_key 会按比较次数重复解析路径,百万记录下聚合段多付千万次遍历)
    recs.sort_by_cached_key(|r| match r {
        Rec::Dir { dir_seq, .. } => table.get(*dir_seq).components().count(),
        Rec::File { dir_seq, .. } => table.get(*dir_seq).components().count() + 1,
    });
    (recs, table, Mutex::into_inner(gaps).unwrap())
}

/// 聚合前登记根条目(键为原始路径形态,与 `apply_records` 的 dir_map 同规则)。
pub fn seed_roots(index: &mut Index, dir_map: &mut HashMap<PathBuf, u32>, roots: &[PathBuf]) {
    for root in roots {
        // 根条目名 = 完整绝对路径(与串行臂一致,full_path 可直接使用)
        let name = root.to_string_lossy().into_owned();
        let idx = index.insert(&name, NO_PARENT, true);
        dir_map.insert(root.clone(), idx);
    }
}

/// 把记录切片聚合进 index。前置:dir_map 已含根(`seed_roots`);切片内父记录
/// 先于子记录(`collect_records` 的深度排序保证,跨批亦然——深度单调不减)。
/// 可分批调用,批间索引处于一致可查询状态(首扫分片可见的实现载体)。
///
/// Dir 聚合:raw = table[dir_seq](自身原始路径)直接作 dir_map 键——目录名
/// 非 UTF-8 时键保真(不做父路径 + lossy 名拼接,那会键漂移);父条目号查
/// `raw.parent()`(PathBuf 零拷贝切片,拓扑序保证父已在 map)。
pub fn apply_records(
    index: &mut Index,
    dir_map: &mut HashMap<PathBuf, u32>,
    recs: &[Rec],
    table: &DirTable,
) {
    for rec in recs {
        match rec {
            Rec::Dir { dir_seq, name } => {
                let raw = table.get(*dir_seq);
                let pidx = dir_map[raw.parent().expect("目录记录的表内路径必有父段")];
                let idx = index.insert(name, pidx, true);
                dir_map.insert(raw.to_path_buf(), idx);
            }
            Rec::File { dir_seq, name } => {
                let pidx = dir_map[table.get(*dir_seq)];
                index.insert(name, pidx, false);
            }
        }
    }
}

/// 清除以 `prefix` 为路径前缀的整棵子树(卷卸载/停用路径,external-volumes §4)。
///
/// 取 dir_map 中前缀匹配(含前缀自身)的条目,**浅者先**调 `Index::remove`——
/// 其子树级联墓碑(「活条目的父必活」不变量)保证后代无遗漏,深键重复删除
/// 对墓碑短路,天然幂等;dir_map 同步清掉前缀下全部键。返回清除的活条目数
/// (含目录)。无匹配键返回 0(从未索引/重复清除,幂等)。
pub fn remove_prefix(index: &mut Index, dir_map: &mut HashMap<PathBuf, u32>, prefix: &Path) -> u32 {
    let before = index.entry_count();
    let mut victims: Vec<u32> = dir_map
        .iter()
        .filter(|(p, _)| p.starts_with(prefix))
        .map(|(_, &idx)| idx)
        .collect();
    victims.sort_unstable_by_key(|&idx| index.full_path(idx).components().count());
    for idx in victims {
        index.remove(idx);
    }
    dir_map.retain(|p, _| !p.starts_with(prefix));
    before.saturating_sub(index.entry_count())
}

/// 并行扫描入口(一次性):遍历 → 深度排序 → 聚合。
fn scan_parallel(roots: &[PathBuf], excludes: &[PathBuf], threads: usize) -> (Index, ScanGaps) {
    let (recs, table, gaps) = collect_records(roots, excludes, threads);
    let mut index = Index::new();
    let mut dir_map: std::collections::HashMap<PathBuf, u32> = std::collections::HashMap::new();
    seed_roots(&mut index, &mut dir_map, roots);
    apply_records(&mut index, &mut dir_map, &recs, &table);
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

    /// 卷卸载清除(external-volumes §4):前缀清除只动卷子树,
    /// HOME 侧条目幸存;重复调用幂等返回 0。
    #[test]
    fn remove_prefix_purges_volume_subtree_and_map() {
        let mut index = Index::new();
        let mut dir_map: HashMap<PathBuf, u32> = HashMap::new();
        let home = PathBuf::from("/Users/t");
        let usb = PathBuf::from("/Volumes/U");
        seed_roots(&mut index, &mut dir_map, &[home.clone(), usb.clone()]);
        let home_idx = dir_map[&home];
        let usb_idx = dir_map[&usb];
        let note = index.insert("note.txt", home_idx, false);
        // U 盘侧:docs 目录(入 map)+ 两个文件 + 顶层散文件
        let docs = index.insert("docs", usb_idx, true);
        dir_map.insert(usb.join("docs"), docs);
        index.insert("a.pdf", docs, false);
        index.insert("b.txt", docs, false);
        index.insert("loose.dat", usb_idx, false);
        assert_eq!(
            index.entry_count(),
            7,
            "两根 + note + docs + 2 文件 + loose"
        );

        let removed = remove_prefix(&mut index, &mut dir_map, &usb);
        assert_eq!(removed, 5, "U 根 + docs + a/b + loose,级联墓碑");
        assert_eq!(index.entry_count(), 2, "只剩两根条目");
        assert!(!dir_map.contains_key(&usb), "卷根键已清");
        assert!(!dir_map.contains_key(&usb.join("docs")), "子目录键已清");
        assert!(dir_map.contains_key(&home), "HOME 键不受影响");
        assert_eq!(
            index.full_path(note),
            home.join("note.txt"),
            "幸存条目路径可复原(未误伤)"
        );
        // 幂等:再清一遍无匹配、无副作用
        assert_eq!(remove_prefix(&mut index, &mut dir_map, &usb), 0);
        assert_eq!(index.entry_count(), 2);
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

    /// 流式聚合等价钉(首扫分片可见的载体):分批 apply_records 的最终索引
    /// 必须与一次性 scan_roots 完全一致(条目数 + 全路径集合)。
    /// 突变「apply_records 依赖切片全局排序而非深度单调」时此测必红。
    #[test]
    fn staged_apply_equals_one_shot_scan() {
        let t = fixture();
        fs::create_dir_all(t.path().join("a/b/c/d")).unwrap();
        fs::write(t.path().join("a/b/c/d/deep.txt"), "").unwrap();
        let roots = vec![t.path().to_path_buf()];
        let excl = vec![t.path().join("node_modules")];

        let (one_shot, _) = scan_roots(&roots, &excl, 4);

        let (recs, table, _) = collect_records(&roots, &excl, 4);
        let mut staged = Index::new();
        let mut dir_map = HashMap::new();
        seed_roots(&mut staged, &mut dir_map, &roots);
        // 逐条聚合(最严切分):任何「子记录先于父记录」的排序缺陷都会在此暴露
        for chunk in recs.chunks(1) {
            apply_records(&mut staged, &mut dir_map, chunk, &table);
        }

        assert_eq!(staged.entry_count(), one_shot.entry_count());
        let paths = |i: &Index| -> std::collections::BTreeSet<String> {
            (0..i.entries.len() as u32)
                .map(|k| i.full_path(k).to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(paths(&staged), paths(&one_shot), "分批聚合与一次性结果一致");
    }

    /// 名字式排除:相对名(如 node_modules)匹配路径任一组件——任意层级的
    /// node_modules 都整棵剪枝,不限于扫描根直属(与绝对路径前缀式并存)。
    #[test]
    fn name_style_excludes_match_any_component() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        fs::create_dir_all(r.join("src/node_modules/x")).unwrap();
        fs::write(r.join("src/node_modules/x/i.js"), "").unwrap();
        fs::write(r.join("src/keep.rs"), "").unwrap();
        let excludes = vec![PathBuf::from("node_modules")];
        let (index, _) = scan_roots(&[r.to_path_buf()], &excludes, 4);
        let paths: Vec<String> = (0..index.entries.len() as u32)
            .map(|i| index.full_path(i).to_string_lossy().into_owned())
            .collect();
        assert!(
            !paths.iter().any(|p| p.contains("node_modules")),
            "任意层级名字式排除:{paths:?}"
        );
        assert!(paths.iter().any(|p| p.ends_with("keep.rs")));
        // 绝对路径前缀式排除并存(fixture 的 node_modules 在根下,同样被剪)
        let abs = vec![r.join("src")];
        let (idx2, _) = scan_roots(&[r.to_path_buf()], &abs, 1);
        let paths2: Vec<String> = (0..idx2.entries.len() as u32)
            .map(|i| idx2.full_path(i).to_string_lossy().into_owned())
            .collect();
        assert!(!paths2.iter().any(|p| p.contains("/src")));
    }

    /// 非 UTF-8 目录名 dir_map 键保真钉(tech.md §3):聚合的键 = 目录表原始
    /// 字节,不因 lossy 名称漂移。macOS APFS 拒绝非法 UTF-8 字节名(EILSEQ,
    /// 场景仅 Linux ext4 可达),故用合成 DirTable + Rec 直测聚合侧——正是
    /// 本改动触及的范围。突变「Dir 聚合改父路径 + lossy 名拼接」→ 字节比对必红。
    #[test]
    fn non_utf8_dir_name_key_stays_raw() {
        use std::os::unix::ffi::OsStringExt;
        let t = tempfile::tempdir().unwrap();
        let bad = std::ffi::OsString::from_vec(vec![b'b', b'a', b'd', 0xFF]);
        let table = DirTable {
            paths: vec![t.path().to_path_buf(), t.path().join(&bad)],
            offsets: vec![0],
        };
        let recs = vec![Rec::Dir {
            dir_seq: DirTable::encode(0, 1),
            name: bad.to_string_lossy().into_owned(),
        }];

        let mut index = Index::new();
        let mut dir_map = HashMap::new();
        seed_roots(&mut index, &mut dir_map, &[t.path().to_path_buf()]);
        apply_records(&mut index, &mut dir_map, &recs, &table);

        let key = dir_map
            .keys()
            .find(|k| k.file_name() == Some(&bad))
            .expect("dir_map 应含原始字节形态的非 UTF-8 键(lossy 拼接会产出 U+FFFD 漂移键)");
        assert_eq!(
            key.as_os_str().as_encoded_bytes().last(),
            Some(&0xFF),
            "键保留原始字节,未被 lossy 替换"
        );
    }
}
