//! 增量:事件 → 目录集合(100ms 静默窗)→ apply_dir_diff(唯一真相)。
use crate::index::{DiffStats, Index};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 事件批处理器:push 登记受影响目录,合批窗口到期后 drain 出批量集合。
/// 语义:事件风暴(解压、拉代码)下同一窗口内的 N 条事件合并为一次 diff
/// (tech.md 增量管线);时钟经函数指针注入,测试可直接操纵。
///
/// 窗口自**首个** pending 事件起算,而非末个:持续事件流(如解压 2 万文件,
/// 事件间隔恒小于窗口)不得无限推迟 drain——否则索引在整个风暴期间停更,
/// 与「增量 ≤500ms」红线冲突。窗口到期即 drain,无论风暴是否仍在继续。
pub struct DirBatcher {
    window: Duration,
    pending: HashSet<String>,
    /// 当前窗口起点(首个 pending 事件的时刻);无 pending 时为 None。
    first_push: Option<Instant>,
    now: fn() -> Instant,
}

impl DirBatcher {
    pub fn new(window: Duration, now: fn() -> Instant) -> Self {
        Self {
            window,
            pending: HashSet::new(),
            first_push: None,
            now,
        }
    }

    pub fn push(&mut self, dir: impl Into<String>) {
        self.pending.insert(dir.into());
        if self.first_push.is_none() {
            self.first_push = Some((self.now)());
        }
    }

    /// 合批窗口到期后取出全部待处理目录(去重);未到期返回空。
    pub fn drain_ready(&mut self) -> Vec<String> {
        let Some(first) = self.first_push else {
            return vec![];
        };
        if (self.now)().duration_since(first) < self.window {
            return vec![];
        }
        self.first_push = None;
        self.pending.drain().collect()
    }

    /// 无视窗口强制取出全部待处理目录:重放追平(HistoryDone)前与停止落盘前
    /// 必须先把窗口内 pending 事件应用完,否则「就绪」/落盘水位会早于索引追上
    /// (实机重放抽验与 stop 丢失链路均曾因此暴露)。
    pub fn drain_all(&mut self) -> Vec<String> {
        self.first_push = None;
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
                                     // 直接回拨窗口起点绕过真实时钟,制造窗口期满
        b.first_push = Some((b.now)() - std::time::Duration::from_millis(150));
        let got = b.drain_ready();
        assert_eq!(got.len(), 2); // /a/b 与 /a/c 去重
    }

    /// 窗口自首个事件起算钉:持续事件流下新事件不得刷新窗口起点,
    /// 否则风暴期间 drain 永不发生(索引停更贯穿整个风暴,实测丢失链路之一)。
    /// 突变「push 内改写 first_push / drain 判 last_push」时此测必红。
    #[test]
    fn window_counts_from_first_push_not_last() {
        let mut b = DirBatcher::new(std::time::Duration::from_millis(100), now_stub());
        b.push("/a");
        b.first_push = Some((b.now)() - std::time::Duration::from_millis(110)); // 窗口已到期
        b.push("/b"); // 持续到达的新事件
        let got = b.drain_ready();
        assert_eq!(got.len(), 2, "窗口到期即 drain,不等风暴结束");
    }

    /// drain_ready 后窗口重置:下一批重新从首事件起算,不被上一批残留影响。
    #[test]
    fn window_resets_after_drain() {
        let mut b = DirBatcher::new(std::time::Duration::from_millis(100), now_stub());
        b.first_push = Some((b.now)() - std::time::Duration::from_millis(150));
        b.push("/a");
        assert_eq!(b.drain_ready().len(), 1);
        b.push("/b");
        assert!(b.drain_ready().is_empty(), "新窗口未到期");
    }

    /// drain_all 无视窗口(重放追平与停止落盘前强制排空)。
    #[test]
    fn drain_all_ignores_window_and_clears() {
        let mut b = DirBatcher::new(std::time::Duration::from_secs(5000), now_stub());
        b.push("/x");
        assert!(b.drain_ready().is_empty());
        assert_eq!(b.drain_all(), vec!["/x".to_string()]);
        assert!(b.drain_ready().is_empty(), "排空后为空");
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
