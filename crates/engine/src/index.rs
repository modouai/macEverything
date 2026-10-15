//! 内存索引:扁平 Entry 数组 + 双 arena + 目录路径池(ADR-2)。
use crate::arena::ArenaBuilder;
use crate::entry::*;

pub struct Index {
    pub entries: Vec<Entry>,
    pub display_arena: ArenaBuilder,
    pub match_arena: ArenaBuilder,
    /// 目录路径池:path: 直扫用;按条目号对齐(目录真值/其余 (0,0) 占位)
    pub path_pool: ArenaBuilder,
    pub path_offsets: Vec<(u32, u32)>,
    live_count: u32,
    tombstones: u32,
}

impl Index {
    pub fn new() -> Self {
        Self {
            entries: vec![],
            display_arena: ArenaBuilder::default(),
            match_arena: ArenaBuilder::default(),
            path_pool: ArenaBuilder::default(),
            path_offsets: vec![],
            live_count: 0,
            tombstones: 0,
        }
    }

    pub fn entry_count(&self) -> u32 {
        self.live_count
    }
    pub fn tombstone_count(&self) -> u32 {
        self.tombstones
    }

    /// 插入条目,返回条目号。path_offsets 与条目号全量对齐:
    /// 目录 push 真值段,非目录 push (0,0) 占位(Task 12b)。
    pub fn insert(&mut self, name: &str, parent: u32, is_dir: bool) -> u32 {
        let m = normalize_for_match(name);
        let (ds, dl) = self.display_arena.push(name.as_bytes());
        let (ms, ml) = self.match_arena.push(m.as_bytes());
        let idx = self.entries.len() as u32;
        let mut flags = 0;
        if is_dir {
            flags |= FLAG_DIR;
        }
        self.entries.push(Entry {
            display_start: ds,
            display_len: dl,
            match_start: ms,
            match_len: ml,
            parent,
            flags,
        });
        self.live_count += 1;
        if is_dir {
            let pp = self.dir_path_bytes(idx);
            let seg = self.path_pool.push(&pp);
            self.path_offsets.push(seg);
        } else {
            self.path_offsets.push((0, 0)); // 占位对齐:path_offsets 下标 == 条目号
        }
        idx
    }

    /// 目录条目的完整路径(match 形态,path_pool 用);idx 必须是目录。
    fn dir_path_bytes(&self, idx: u32) -> Vec<u8> {
        let mut parts: Vec<&[u8]> = vec![];
        let mut cur = idx;
        loop {
            let e = &self.entries[cur as usize];
            parts.push(
                &self.match_arena.data
                    [e.match_start as usize..(e.match_start + e.match_len) as usize],
            );
            if e.parent == NO_PARENT {
                break;
            }
            cur = e.parent;
        }
        parts.reverse();
        parts.join(b"/".as_slice())
    }

    pub fn display_name_of(&self, idx: u32) -> &[u8] {
        let e = &self.entries[idx as usize];
        &self.display_arena.data
            [e.display_start as usize..(e.display_start + e.display_len) as usize]
    }

    pub fn match_name_of(&self, idx: u32) -> &[u8] {
        let e = &self.entries[idx as usize];
        &self.match_arena.data[e.match_start as usize..(e.match_start + e.match_len) as usize]
    }

    /// 条目的完整路径(display 原始形态,展示用)。
    pub fn full_path(&self, idx: u32) -> std::path::PathBuf {
        let mut parts: Vec<Vec<u8>> = vec![];
        let mut cur = idx;
        loop {
            let e = &self.entries[cur as usize];
            parts.push(self.display_name_of(cur).to_vec());
            if e.parent == NO_PARENT {
                break;
            }
            cur = e.parent;
        }
        parts.reverse();
        let joined = parts.join(b"/".as_slice());
        String::from_utf8_lossy(&joined).into_owned().into()
    }

    /// 直接子项(活条目)。O(n) 线性扫是已知设计(百万规模单目录 diff ~2-4ms,预算内)。
    pub fn children_of(&self, dir_idx: u32) -> Vec<u32> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.parent == dir_idx && e.is_live())
            .map(|(i, _)| i as u32)
            .collect()
    }

    /// 目录在磁盘上的现名(match 形态);供 watcher/diff 反查。
    pub fn dir_path(&self, idx: u32) -> Vec<u8> {
        let (s, l) = self.path_offsets[idx as usize];
        self.path_pool.data[s as usize..(s + l) as usize].to_vec()
    }
}

impl Default for Index {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ix() -> Index {
        Index::new()
    }

    #[test]
    fn insert_and_full_path() {
        let mut i = ix();
        let docs = i.insert("Documents", NO_PARENT, true);
        let q1 = i.insert("季度报告.pdf", docs, false);
        let q2 = i.insert("budget.xlsx", docs, false);
        assert_eq!(i.entry_count(), 3);
        assert_eq!(i.full_path(q1).to_string_lossy(), "Documents/季度报告.pdf");
        assert_eq!(i.full_path(q2).to_string_lossy(), "Documents/budget.xlsx");
    }

    #[test]
    fn children_of_lists_live_entries() {
        let mut i = ix();
        let d = i.insert("d", NO_PARENT, true);
        let a = i.insert("a", d, false);
        let _ = i.insert("b", d, false);
        let mut kids = i.children_of(d);
        kids.sort();
        assert_eq!(kids, vec![a, a + 1]);
    }

    #[test]
    fn unicode_aliases_match_identically() {
        let mut i = ix();
        let nfd = i.insert("re\u{301}sume\u{301}.txt", NO_PARENT, false); // NFD 形态
        let nfc = i.insert("résumé.txt", NO_PARENT, false); // NFC 形态
        assert_eq!(i.match_name_of(nfd), i.match_name_of(nfc)); // 同一折叠形态
    }

    // Task 12b 修正的钉子:非目录条目也有 path_offsets 占位,下标==条目号
    #[test]
    fn path_offsets_align_with_entry_index() {
        let mut i = ix();
        let d = i.insert("docs", NO_PARENT, true);
        i.insert("f.txt", d, false); // 文件穿插
        let sub = i.insert("sub", d, true);
        assert_eq!(i.path_offsets.len(), 3);
        assert!(i.path_offsets[d as usize].1 > 0); // 目录有真值
        assert_eq!(i.path_offsets[1], (0, 0)); // 文件占位
        assert_eq!(i.dir_path(sub), b"docs/sub".to_vec()); // 目录路径池按条目号可取
    }
}
