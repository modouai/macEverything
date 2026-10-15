//! 增量:事件 → 目录集合(100ms 静默窗)→ apply_dir_diff(唯一真相)。
use crate::index::{DiffStats, Index};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 事件批处理器:push 登记受影响目录,静默窗期满后 drain 出批量集合。
/// 语义:事件风暴(解压、拉代码)下 100ms 内的 N 条事件合并为一次 diff
/// (tech.md 增量管线);时钟经函数指针注入,测试可直接操纵。
pub struct DirBatcher {
    window: Duration,
    pending: HashSet<String>,
    last_push: Instant,
    now: fn() -> Instant,
}

impl DirBatcher {
    pub fn new(window: Duration, now: fn() -> Instant) -> Self {
        Self {
            window,
            pending: HashSet::new(),
            last_push: now(),
            now,
        }
    }

    pub fn push(&mut self, dir: impl Into<String>) {
        self.pending.insert(dir.into());
        self.last_push = (self.now)();
    }

    /// 窗口静默期满后取出全部待处理目录(去重);未期满返回空。
    pub fn drain_ready(&mut self) -> Vec<String> {
        if (self.now)().duration_since(self.last_push) < self.window || self.pending.is_empty() {
            return vec![];
        }
        self.last_push = (self.now)();
        self.pending.drain().collect()
    }
}

pub fn now_stub() -> fn() -> Instant {
    Instant::now
}

/// 目录 diff:重读磁盘目录与索引比对(事件只当信号、目录 diff 才是真相,tech.md)。
///
/// 失败语义:打开或遍历中途任一失败一律上抛,不应用。理由——diff 的输入是
/// 「完整真相」,部分读取的 listing 会把「读不到」的条目误判为「已删除」打墓碑。
/// 调用方(增量 worker)记录错误并等待后续事件重试;目录已被删的竞态由父目录
/// 的 diff 兜底(remove 级联)。
///
/// 排除:与首扫共用 `scanner::is_excluded`——排除目录不入 listing,防增量把
/// 首扫排除掉的子树(如 ~/Library)重新拉回索引(两条路径语义一致)。
pub fn apply_dir_diff(
    index: &mut Index,
    dir_idx: u32,
    abs_dir: &Path,
    excludes: &[PathBuf],
) -> std::io::Result<DiffStats> {
    let rd = std::fs::read_dir(abs_dir)?;
    let mut listing = std::collections::BTreeMap::new();
    for e in rd {
        let e = e?;
        let ft = e.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        let is_dir = ft.is_dir();
        if is_dir && crate::scanner::is_excluded(&e.path(), excludes) {
            continue;
        }
        listing.insert(e.file_name().to_string_lossy().into_owned(), is_dir);
    }
    Ok(index.diff_dir(dir_idx, &listing))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::NO_PARENT;
    use crate::index::Index;

    #[test]
    fn batcher_dedups_within_window() {
        let mut b = DirBatcher::new(std::time::Duration::from_millis(100), now_stub());
        b.push("/a/b");
        b.push("/a/b");
        b.push("/a/c");
        let drained = b.drain_ready();
        assert!(drained.is_empty()); // 窗口未到
                                     // push 会刷新 last_push,直接回拨字段绕过真实时钟,制造静默期满
        b.last_push = (b.now)() - std::time::Duration::from_millis(150);
        let got = b.drain_ready();
        assert_eq!(got.len(), 2); // /a/b 与 /a/c 去重
    }

    #[test]
    fn diff_updates_index_from_real_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();
        let mut i = Index::new();
        let d = i.insert(dir.path().to_string_lossy().as_ref(), NO_PARENT, true);
        let _ = i.insert("ghost.txt", d, false); // 索引有、磁盘无
        let stats = apply_dir_diff(&mut i, d, dir.path(), &[]).unwrap();
        assert!(stats.removed >= 1);
        let kids: Vec<Vec<u8>> = i
            .children_of(d)
            .iter()
            .map(|k| i.display_name_of(*k).to_vec())
            .collect();
        assert!(kids.contains(&b"a.txt".to_vec()));
        assert!(!kids.contains(&b"ghost.txt".to_vec()));
    }

    /// 增量排除钉:磁盘上出现排除目录(如 ~/Library)时,diff 不得把它拉进索引
    /// (回归钉——曾在实机重放路径暴露:首扫排除的 Library 被增量重新收录)。
    #[test]
    fn diff_respects_excludes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Library")).unwrap();
        std::fs::write(dir.path().join("keep.txt"), "").unwrap();
        let mut i = Index::new();
        let d = i.insert(dir.path().to_string_lossy().as_ref(), NO_PARENT, true);
        let excludes = vec![dir.path().join("Library")];
        apply_dir_diff(&mut i, d, dir.path(), &excludes).unwrap();
        let kids: Vec<Vec<u8>> = i
            .children_of(d)
            .iter()
            .map(|k| i.display_name_of(*k).to_vec())
            .collect();
        assert!(
            !kids.contains(&b"Library".to_vec()),
            "排除目录不得经增量入索引:{kids:?}"
        );
        assert!(kids.contains(&b"keep.txt".to_vec()));
    }
}
