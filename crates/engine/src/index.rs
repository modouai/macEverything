//! 内存索引:扁平 Entry 数组 + 双 arena + 目录路径池(ADR-2)。
use std::collections::BTreeMap;

use crate::arena::ArenaBuilder;
use crate::entry::*;

/// 目录现项清单:名称 -> 是否目录(watcher/reconcile 从磁盘读取后传入)。
pub type DirListing = BTreeMap<String, bool>;

/// 一次 diff_dir 的增删统计。removed 计清单级条目数:目录被删时其子树的级联
/// 墓碑不逐条计入(计数语义为目录项数,与磁盘清单增量一一对应)。
#[derive(Debug, Clone, Copy)]
pub struct DiffStats {
    pub added: u32,
    pub removed: u32,
}

pub struct Index {
    pub entries: Vec<Entry>,
    pub display_arena: ArenaBuilder,
    pub match_arena: ArenaBuilder,
    /// 目录路径池:path: 直扫用;按条目号对齐(目录真值/其余 (0,0) 占位)
    pub path_pool: ArenaBuilder,
    pub path_offsets: Vec<(u32, u32)>,
    live_count: u32,
    tombstones: u32,
    /// rename/级联重写产生的 arena 孤儿字节(双名称池 + 路径池被替换的旧段)。
    /// 整理触发条件之一(纯 rename 零墓碑,单靠墓碑占比永不触发)。
    orphan_pool_bytes: u64,
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
            orphan_pool_bytes: 0,
        }
    }

    /// crate 内装配构造:persist::SharedView::materialize 从 mmap 逐字节拷出的字段直接组装,
    /// 绕过逐条 insert(条目号 1:1 保持)。live/tombstone 计数由调用方统计;
    /// 孤儿字节恒 0(全量重建,与 compact 同理)。仅限同构数据源,不对 crate 外暴露。
    pub(crate) fn from_parts(
        entries: Vec<Entry>,
        display_arena: ArenaBuilder,
        match_arena: ArenaBuilder,
        path_pool: ArenaBuilder,
        path_offsets: Vec<(u32, u32)>,
        live_count: u32,
        tombstones: u32,
    ) -> Self {
        Self {
            entries,
            display_arena,
            match_arena,
            path_pool,
            path_offsets,
            live_count,
            tombstones,
            orphan_pool_bytes: 0,
        }
    }

    pub fn entry_count(&self) -> u32 {
        self.live_count
    }
    pub fn tombstone_count(&self) -> u32 {
        self.tombstones
    }

    /// 插入条目,返回条目号。不变量:`parent` 必须是 NO_PARENT 或已存在条目号
    /// (严格小于返回值);违反将破坏父链上溯的终止性(debug 断言在测试期暴露)。
    /// path_offsets 与条目号全量对齐:目录 push 真值段,非目录 push (0,0) 占位(Task 12b)。
    pub fn insert(&mut self, name: &str, parent: u32, is_dir: bool) -> u32 {
        let idx = self.entries.len() as u32;
        debug_assert!(
            parent == NO_PARENT || parent < idx,
            "parent 必须先于子条目存在"
        );
        let m = normalize_for_match(name);
        let (ds, dl) = self.display_arena.push(name.as_bytes());
        let (ms, ml) = self.match_arena.push(m.as_bytes());
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
    /// pub(crate):persist::SharedView::materialize 重建路径池复用同一父链拼接(唯此一份)。
    pub(crate) fn dir_path_bytes(&self, idx: u32) -> Vec<u8> {
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

    /// 条目的完整路径(display 原始形态,展示用)。委托 trait 默认实现(父链拼接唯此一份)。
    pub fn full_path(&self, idx: u32) -> std::path::PathBuf {
        self.full_path_dyn(idx).into()
    }

    /// 条目完整路径的**原始字节**形态(display 名直接组字节,不经 lossy)。
    /// 扫描摘除(scanner::remove_prefix)的 victim 匹配用它与调用方前缀(磁盘
    /// 原始字节)比对——`full_path` 走 `from_utf8_lossy`,非 UTF-8 名称会被替换成
    /// U+FFFD 而与真实字节错位(dir_map 键保真的前提是全链路原始字节;APFS 拒绝
    /// 非 UTF-8 名,此差异实际只在 Linux 侧可达)。pub(crate):父链拼接第三形态
    /// 与 `dir_path_bytes`(match 形态)/`full_path`(lossy display 形态)同处
    /// 一文件,防「拼接唯此一份」约定漂移。
    pub(crate) fn full_path_raw_bytes(&self, idx: u32) -> std::path::PathBuf {
        use std::os::unix::ffi::OsStringExt;
        let mut parts: Vec<&[u8]> = Vec::new();
        let mut cur = idx;
        loop {
            let e = &self.entries[cur as usize];
            parts.push(self.display_name_of(cur));
            if e.parent == NO_PARENT {
                break;
            }
            cur = e.parent;
        }
        parts.reverse();
        let mut bytes: Vec<u8> = Vec::new();
        for (i, p) in parts.iter().enumerate() {
            if i > 0 {
                bytes.push(b'/');
            }
            bytes.extend_from_slice(p);
        }
        std::path::PathBuf::from(std::ffi::OsString::from_vec(bytes))
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
    /// 委托 trait 默认实现(池段切取唯此一份),按 owned 供既有调用方。
    pub fn dir_path(&self, idx: u32) -> Vec<u8> {
        IndexView::dir_path(self, idx).to_vec()
    }

    // —— Task 6:变更语义 ——

    /// 改名。双 arena 追加新段(旧段成孤儿,计入 orphan_pool_bytes,整理时回收);
    /// 目录触发子树路径池级联重写,文件不进路径池(Task 12b)。
    pub fn rename_entry(&mut self, idx: u32, new_name: &str) {
        let m = normalize_for_match(new_name);
        let (ds, dl) = self.display_arena.push(new_name.as_bytes());
        let (ms, ml) = self.match_arena.push(m.as_bytes());
        let e = &mut self.entries[idx as usize];
        self.orphan_pool_bytes += (e.display_len + e.match_len) as u64;
        e.display_start = ds;
        e.display_len = dl;
        e.match_start = ms;
        e.match_len = ml;
        if e.is_dir() {
            self.rebuild_paths_under(idx); // 子树级联
        }
    }

    /// 重挂父指针(Overlay 桥复用,tech.md §4.2 写侧:子目录先于父被 diff 时,
    /// 其 anchor 是顶层桥接占位;父层首 diff 复用该桥并挂到真实父 anchor 下,
    /// 而非新建覆盖——覆盖会让旧桥链成孤儿,见 overlay.rs)。
    /// **不做路径池级联**:overlay 目录的池段是「合成完整路径」(overlay.rs
    /// `rewrite_dir_path` 两段拼接 = 链上最高桥的 base 目录路径 + 中间段),把
    /// 链中段的桥挂到更长链后,最高桥的 base 段与补入的中间段恒等互换
    /// (两侧同为 match 形态父链拼接),既有池段值不变;查询侧路径按链现算,
    /// 自动正确。调用方保证:new_parent 为活条目且不在 idx 子树内(当前调用
    /// 形态:占位桥 parent 从 NO_PARENT 变为真实父 anchor,结构性无环)。
    pub fn reparent(&mut self, idx: u32, new_parent: u32) {
        debug_assert_ne!(idx, new_parent, "reparent 不得自挂");
        debug_assert!(
            self.entries
                .get(new_parent as usize)
                .is_some_and(|e| e.is_live()),
            "reparent 目标父必须为活条目"
        );
        self.entries[idx as usize].parent = new_parent;
    }

    /// 一趟 O(n) 构建 parent -> 活子项邻接表。子树遍历(rebuild/remove)查表取子项,
    /// 总代价 O(n + S),替代逐层全量扫(后者在顶层目录改名时是 O(n×S),击穿写锁红线)。
    fn children_table(&self) -> Vec<Vec<u32>> {
        let mut table: Vec<Vec<u32>> = (0..self.entries.len()).map(|_| Vec::new()).collect();
        for (i, e) in self.entries.iter().enumerate() {
            if e.is_live() && e.parent != NO_PARENT {
                table[e.parent as usize].push(i as u32);
            }
        }
        table
    }

    /// 重写 root 及其整棵子树的目录路径池条目(目录改名级联,ADR-2 风险表)。
    /// 文件条目跳过(占位 (0,0) 不变);被替换的旧路径段计入 orphan_pool_bytes。
    fn rebuild_paths_under(&mut self, root: u32) {
        let mut table = self.children_table();
        fn walk(ix: &mut Index, table: &mut Vec<Vec<u32>>, cur: u32) {
            if ix.entries[cur as usize].is_dir() {
                let (_, old_len) = ix.path_offsets[cur as usize];
                ix.orphan_pool_bytes += old_len as u64;
                let pp = ix.dir_path_bytes(cur);
                ix.path_offsets[cur as usize] = ix.path_pool.push(&pp);
            }
            let kids = std::mem::take(&mut table[cur as usize]);
            for k in kids {
                walk(ix, table, k);
            }
        }
        walk(self, &mut table, root);
    }

    /// 删除(墓碑制,不 swap-remove:保条目号与父指针稳定)。
    /// 子树级联:活子项一并打墓碑,保证「活条目的父必活」不变量。
    /// 无活子项(最常见的文件删除)走廉价探测,免 O(n) 邻接表构建。
    pub fn remove(&mut self, idx: u32) {
        if !self.entries[idx as usize].is_live() {
            return;
        }
        let has_children = self.entries.iter().any(|e| e.parent == idx && e.is_live());
        if !has_children {
            self.tombstone_one(idx);
            return;
        }
        self.remove_many(&[idx]);
    }

    /// 批量删除:整批共用一次构建的邻接表。单条 remove 各自建表是 O(n),
    /// 目录 diff 删除风暴下 N 条 = N×O(n);批量后总代价 O(n + ΣS)。
    /// 同一子树内重复 idx 由 walk 的墓碑短路自然去重。
    fn remove_many(&mut self, idxs: &[u32]) {
        let mut table = self.children_table();
        for &idx in idxs {
            self.walk_remove(&mut table, idx);
        }
    }

    fn tombstone_one(&mut self, idx: u32) {
        let e = &mut self.entries[idx as usize];
        if !e.is_live() {
            return;
        }
        e.flags |= FLAG_TOMBSTONE;
        self.live_count -= 1;
        self.tombstones += 1;
    }

    fn walk_remove(&mut self, table: &mut [Vec<u32>], cur: u32) {
        if !self.entries[cur as usize].is_live() {
            return;
        }
        self.tombstone_one(cur);
        let kids = std::mem::take(&mut table[cur as usize]);
        for k in kids {
            self.walk_remove(table, k);
        }
    }

    /// 墓碑占比(整理触发条件之一,阈值 20%,ADR-2 风险表)。
    pub fn tombstone_ratio(&self) -> f64 {
        let total = self.live_count + self.tombstones;
        if total == 0 {
            0.0
        } else {
            self.tombstones as f64 / total as f64
        }
    }

    /// 整理判定:墓碑占比超 20%,或 rename/级联重写累计的 arena 孤儿字节超 32MB
    /// (纯 rename 零墓碑,单靠占比永不触发;孤儿字节同样占内存,一并进触发条件)。
    pub fn needs_compaction(&self) -> bool {
        const ORPHAN_POOL_LIMIT: u64 = 32 * 1024 * 1024;
        self.tombstone_ratio() > 0.2 || self.orphan_pool_bytes > ORPHAN_POOL_LIMIT
    }

    /// 唯一真相机制:按磁盘现目录项 diff 该目录(parent == dir_idx 的活条目)。
    /// 索引中有而磁盘无 → 墓碑;磁盘有而索引无 → 插入;类型不符 → 删+增。
    pub fn diff_dir(&mut self, dir_idx: u32, on_disk: &DirListing) -> DiffStats {
        debug_assert!(
            self.entries[dir_idx as usize].is_live(),
            "对墓碑目录执行 diff"
        );
        let mut added = 0u32;
        let mut removed = 0u32;
        let mut on_disk = on_disk.clone();
        let mut to_remove: Vec<u32> = Vec::new();
        let mut to_reinsert: Vec<(String, bool)> = Vec::new();
        let kids: Vec<u32> = self.children_of(dir_idx);
        for k in kids {
            let name = String::from_utf8_lossy(self.display_name_of(k)).into_owned();
            match on_disk.remove(&name) {
                Some(is_dir) => {
                    if self.entries[k as usize].is_dir() != is_dir {
                        // 类型变化视为删+增
                        to_remove.push(k);
                        to_reinsert.push((name, is_dir));
                        added += 1;
                        removed += 1;
                    }
                }
                None => {
                    to_remove.push(k);
                    removed += 1;
                }
            }
        }
        // 批量墓碑:删除风暴(解压后清理、批量删除)共用一次邻接表构建
        self.remove_many(&to_remove);
        for (name, is_dir) in to_reinsert {
            let _ = self.insert(&name, dir_idx, is_dir);
        }
        for (name, is_dir) in on_disk {
            let _ = self.insert(&name, dir_idx, is_dir);
            added += 1;
        }
        DiffStats { added, removed }
    }

    /// 墓碑整理:重建 entries/双 arena/路径池,重写父指针。
    /// 顺序约束:先修父指针再建路径池(dir_path_bytes 依赖已修好的父链)。
    pub fn compact(&mut self) {
        let old = std::mem::take(self);
        let mut map = vec![NO_PARENT; old.entries.len()];
        let mut new = Index::new();
        for (i, e) in old.entries.iter().enumerate() {
            if !e.is_live() {
                continue;
            }
            let dname = String::from_utf8_lossy(old.display_name_of(i as u32)).into_owned();
            let old_parent = e.parent;
            let m = normalize_for_match(&dname);
            let (ds, dl) = new.display_arena.push(dname.as_bytes());
            let (ms, ml) = new.match_arena.push(m.as_bytes());
            new.entries.push(Entry {
                display_start: ds,
                display_len: dl,
                match_start: ms,
                match_len: ml,
                parent: old_parent,
                flags: e.flags & !FLAG_TOMBSTONE, // 保留未来 flag 位,只剥墓碑
            });
            map[i] = new.entries.len() as u32 - 1;
            new.live_count += 1;
        }
        for e in new.entries.iter_mut() {
            if e.parent != NO_PARENT {
                // remove 的子树级联保证「活条目的父必活」,此处映射必命中活条目;
                // 违反即索引静默腐化,release 崩溃优于腐化。
                let mapped = map[e.parent as usize];
                assert!(
                    mapped != NO_PARENT,
                    "compact 前存在父为墓碑的活条目,违反 remove 级联不变量"
                );
                e.parent = mapped;
            }
        }
        // path_offsets 按条目号对齐(目录真值/其余占位)——先 resize 占位再填目录
        new.path_offsets = vec![(0, 0); new.entries.len()];
        for i in 0..new.entries.len() {
            if new.entries[i].is_dir() {
                let pp = new.dir_path_bytes(i as u32);
                new.path_offsets[i] = new.path_pool.push(&pp);
            }
        }
        new.orphan_pool_bytes = 0; // 全量重建,孤儿字节清零
        *self = new;
    }
}

impl Default for Index {
    fn default() -> Self {
        Self::new()
    }
}

/// 内存视图与 mmap 只读视图的统一读取面(ADR-3:查询代码无分支扩散)。
/// 查询端只依赖本 trait:S3 的双源合成视图(Base + Overlay 各自连续,tech.md §4.2)
/// 实现同一组方法后自动获得查询能力。
///
/// 接口契约(tech.md §4.3 接口改造):**条目遍历是分段枚举,不存在「单一连续
/// 切片」假设**——单源 Index 恒一段;双源各产一段。随机访问仅限低频点
/// (结果行组装/排序键),全量扫描必须走分段枚举(每条目一次虚调用是热路径红线)。
///
/// T3 收窄(T4 前定形):名称/路径读取只暴露**语义方法**(match_name/display_name/
/// dir_path/full_path_dyn),不再暴露裸 arena/池段访问——双源合成视图没有单一
/// arena,按空间分发只能在语义层做;裸访问器会绕过分发读到错空间的字节。
/// 匹配形态名是必答题的三个原语之一(match 折叠段/display 原始段/目录路径段),
/// 各实现按自己的存储直接切取。
pub trait IndexView: Sync {
    /// 连续条目段枚举(扫描原语):(段起始条目号, 连续 Entry 切片)序列。
    /// 前置契约:**各段不重叠,按条目号升序稠密覆盖 0..N−1**(段 i 末 + 1 == 段
    /// i+1 起;查询侧 dir_hits/父链按全局条目号直接下标,稀疏或空洞编号未定义,
    /// 合成视图必须按序拼接)。段内条目号 = 段起始号 + 段内下标;调用方不得假设
    /// 段数为 1。返回 Vec 而非迭代器是 dyn 兼容约束(泛型/RPITIT 方法非对象
    /// 安全);每查询入口一次的小分配,不在逐条目路径上。
    fn entry_segments(&self) -> Vec<(u32, &[Entry])>;

    /// 随机访问:条目号 → 条目。越界行为与内存切片一致(panic,同一信任级别)。
    fn entry(&self, idx: u32) -> &Entry;

    /// 条目的父条目号,**全局合成空间号**(T3 评审 2026-10-31):单源视图与
    /// `entry(idx).parent` 一致;双源合成视图必须把 overlay 空间父号重映射进
    /// 合成空间(base_count + 父)——查询侧 path 词的 dir_hits 预过滤按合成号
    /// 直下标取「父是否命中目录」,直接读 `entry(idx).parent` 会在 Overlay 命中
    /// 上拿到错空间号、命中无关槽位(path 查询随机丢失/误收,持续整个增量窗口)。
    /// NO_PARENT 原样返回(调用方以 `!= NO_PARENT` 短路)。
    fn parent_idx(&self, idx: u32) -> u32 {
        self.entry(idx).parent
    }

    /// 匹配形态名(match 折叠段;case: 查询在查询端另取 display 归一)。
    fn match_name(&self, idx: u32) -> &[u8];

    /// 展示形态名(原始字节,未归一)。
    fn display_name(&self, idx: u32) -> &[u8];

    /// 目录条目的完整路径(match 形态);非目录条目返回空切片。
    fn dir_path(&self, idx: u32) -> &[u8];

    /// 条目有效性(tech.md §4.2 屏蔽判定挂点)。单源视图恒有效(零开销默认);
    /// 合成视图覆写:Base 命中走 `valid(e)` 条目号链屏蔽,Overlay 命中抑制
    /// 「base 背书的 anchor」双份命中。查询侧只对**命中候选**调用,不在全量扫描
    /// 热路径上。
    fn entry_valid(&self, _idx: u32) -> bool {
        true
    }

    /// 直接子项(活条目)。默认实现 O(n) 全表扫——与 `Index::children_of` 同已知
    /// 设计(单目录 diff 2-4ms 预算内);合成视图对 Overlay 空间不覆写(写侧
    /// 直接用 `Overlay::index` 的固有能力)。
    fn children_of(&self, dir_idx: u32) -> Vec<u32> {
        let mut out = Vec::new();
        for &(start, seg) in &self.entry_segments() {
            for (j, e) in seg.iter().enumerate() {
                if e.parent == dir_idx && e.is_live() {
                    out.push(start + j as u32);
                }
            }
        }
        out
    }

    /// 沿 parent 链拼 display 完整路径(Task 18 daemon 结果输出用)。
    fn full_path_dyn(&self, idx: u32) -> String {
        let mut parts: Vec<&[u8]> = Vec::new();
        let mut cur = idx;
        loop {
            let e = self.entry(cur);
            parts.push(self.display_name(cur));
            if e.parent == NO_PARENT {
                break;
            }
            cur = e.parent;
        }
        parts.reverse();
        let joined = parts.join(b"/".as_slice());
        String::from_utf8_lossy(&joined).into_owned()
    }
}

impl IndexView for Index {
    fn entry_segments(&self) -> Vec<(u32, &[Entry])> {
        vec![(0, self.entries.as_slice())]
    }
    fn entry(&self, idx: u32) -> &Entry {
        &self.entries[idx as usize]
    }
    fn match_name(&self, idx: u32) -> &[u8] {
        Index::match_name_of(self, idx)
    }
    fn display_name(&self, idx: u32) -> &[u8] {
        Index::display_name_of(self, idx)
    }
    fn dir_path(&self, idx: u32) -> &[u8] {
        let (s, l) = self.path_offsets[idx as usize];
        &self.path_pool.data[s as usize..(s + l) as usize]
    }
    fn children_of(&self, dir_idx: u32) -> Vec<u32> {
        Index::children_of(self, dir_idx) // 固有能力(邻接直查),不用默认 O(n) 全表扫
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

    // —— Task 6:变更语义 ——
    use std::collections::BTreeMap;

    fn listing(files: &[(&str, bool)]) -> BTreeMap<String, bool> {
        files.iter().map(|(n, d)| (n.to_string(), *d)).collect()
    }

    #[test]
    fn diff_applies_add_remove_rename() {
        let mut i = ix();
        let d = i.insert("docs", NO_PARENT, true);
        let _a = i.insert("a.txt", d, false);
        i.diff_dir(d, &listing(&[("b.txt", false), ("a.txt", false)]));
        // b 新增,a 保留
        let kids: Vec<Vec<u8>> = i
            .children_of(d)
            .iter()
            .map(|k| i.display_name_of(*k).to_vec())
            .collect();
        assert!(kids.contains(&b"b.txt".to_vec()) && kids.contains(&b"a.txt".to_vec()));
        i.diff_dir(d, &listing(&[("b.txt", false)]));
        let kids: Vec<Vec<u8>> = i
            .children_of(d)
            .iter()
            .map(|k| i.display_name_of(*k).to_vec())
            .collect();
        assert_eq!(kids, vec![b"b.txt".to_vec()]); // a 被墓碑
        assert_eq!(i.tombstone_count(), 1);
    }

    #[test]
    fn dir_cascade_and_path_pool() {
        let mut i = ix();
        let p = i.insert("projects", NO_PARENT, true);
        let sub = i.insert("alpha", p, true);
        let _f = i.insert("x.rs", sub, false);
        assert_eq!(i.dir_path(sub), b"projects/alpha".to_vec());
        i.rename_entry(p, "workspace");
        assert_eq!(i.dir_path(p), b"workspace".to_vec());
        assert_eq!(i.dir_path(sub), b"workspace/alpha".to_vec()); // 目录:路径池级联重写
                                                                  // 文件不进路径池(Task 12b 占位设计);其路径经父链求值:
        assert_eq!(i.full_path(_f).to_string_lossy(), "workspace/alpha/x.rs");
    }

    #[test]
    fn compact_reclaims_tombstones() {
        let mut i = ix();
        let d = i.insert("d", NO_PARENT, true);
        for k in 0..10 {
            let _ = i.insert(&format!("f{k}.txt"), d, false);
        }
        for _ in 0..9 {
            i.remove(i.children_of(d)[0]);
        } // 剩 1 活 + 9 墓碑
        assert!(i.tombstone_ratio() > 0.2);
        i.compact();
        assert_eq!(i.tombstone_count(), 0);
        assert_eq!(i.entry_count(), 2); // d + 存活 f9
                                        // compact 后读侧语义不破:父链重映射 + 路径池重建仍一致
        let kids = i.children_of(d);
        assert_eq!(kids.len(), 1);
        assert_eq!(i.full_path(kids[0]).to_string_lossy(), "d/f9.txt");
        assert_eq!(i.dir_path(d), b"d".to_vec());
    }

    // —— quality review 钉 ——

    // I1 位移钉:compact 后存活条目前移,父指针必须重映射到新条目号
    #[test]
    fn compact_remaps_shifted_entries() {
        let mut i = ix();
        let gone = i.insert("gone", NO_PARENT, true); // 0
        let d = i.insert("d", NO_PARENT, true); // 1
        i.insert("f.txt", d, false); // 2
        i.remove(gone);
        i.compact();
        assert_eq!(i.entries.len(), 2);
        assert!(i.entries[0].is_dir(), "d 位移到 0 号");
        assert_eq!(i.entries[1].parent, 0, "f.parent 重映射为 d 的新号");
        assert_eq!(i.full_path(1).to_string_lossy(), "d/f.txt");
        assert_eq!(i.children_of(0), vec![1u32]);
        assert_eq!(i.dir_path(0), b"d".to_vec());
    }

    // I2 级联钉:remove 顶层目录,子树(含嵌套两层)全部打墓碑,整理后彻底回收
    #[test]
    fn remove_cascades_subtree() {
        let mut i = ix();
        let root = i.insert("root", NO_PARENT, true); // 0
        let mid = i.insert("mid", root, true); // 1
        let leaf_f = i.insert("leaf.txt", mid, false); // 2
        let leaf_d = i.insert("leafdir", mid, true); // 3
        let deep = i.insert("deep.txt", leaf_d, false); // 4
        assert_eq!(i.entry_count(), 5);
        i.remove(root);
        for idx in [root, mid, leaf_f, leaf_d, deep] {
            assert!(!i.entries[idx as usize].is_live(), "条目 {idx} 应为墓碑");
        }
        assert_eq!(i.entry_count(), 0);
        assert_eq!(i.tombstone_count(), 5);
        i.compact();
        assert!(i.entries.is_empty(), "整理后墓碑条目全部回收");
        assert_eq!(i.entry_count(), 0);
    }

    // I3 池膨胀钉:纯 rename 零墓碑,孤儿 arena 字节达阈值即触发整理判定
    #[test]
    fn needs_compaction_triggers_on_rename_pool_growth() {
        let mut i = ix();
        let p = i.insert("projects", NO_PARENT, true);
        for k in 0..50 {
            i.insert(&format!("sub{k:02}"), p, true);
        }
        assert!(!i.needs_compaction());
        for _ in 0..40_000 {
            i.rename_entry(p, "workspace-x");
        }
        assert_eq!(i.tombstone_count(), 0, "纯 rename 不产生墓碑");
        assert!(i.needs_compaction(), "孤儿池字节超 32MB 阈值");
        i.compact();
        assert!(!i.needs_compaction(), "整理后孤儿字节清零");
    }

    // M3 类型变化钉:file↔dir 翻转按删+增处理,旧条目墓碑、新条目入位
    #[test]
    fn diff_dir_handles_type_flip() {
        let mut i = ix();
        let d = i.insert("docs", NO_PARENT, true);
        let old = i.insert("item", d, false); // 索引里是文件
        let stats = i.diff_dir(d, &listing(&[("item", true)])); // 磁盘上是目录
        assert_eq!((stats.added, stats.removed), (1, 1));
        assert!(!i.entries[old as usize].is_live(), "旧条目被墓碑");
        let kids = i.children_of(d);
        assert_eq!(kids.len(), 1);
        assert_ne!(kids[0], old, "新条目重新入位");
        assert!(i.entries[kids[0] as usize].is_dir(), "新条目为目录");
        assert_eq!(i.tombstone_count(), 1);
        assert_eq!(i.entry_count(), 2); // docs + 新 item
    }

    // M-r1 双池记账钉:rename 的 display+match 两段孤儿都计入
    // orphan_pool_bytes(文件改名不涉路径池,隔离验证;删记账行此测必红)
    #[test]
    fn rename_entry_counts_display_and_match_orphans() {
        let long = "x".repeat(1024);
        let mut i = ix();
        let f = i.insert("file", NO_PARENT, false);
        for _ in 0..17_000 {
            i.rename_entry(f, &long); // ~2048B × 17k ≈ 34.8MB > 32MB 阈值
        }
        assert!(i.needs_compaction());
    }

    // 批量删除钉:diff_dir 一次删 99/101 条(删除风暴形态)结果与逐条一致,
    // 计数联动且触发整理判定(daemon 接线 needs_compaction 的前提)。
    #[test]
    fn diff_dir_bulk_removal_and_compaction_trigger() {
        let mut i = ix();
        let d = i.insert("docs", NO_PARENT, true);
        for k in 0..100 {
            i.insert(&format!("f{k:03}.txt"), d, false);
        }
        let stats = i.diff_dir(d, &listing(&[("f000.txt", false)]));
        assert_eq!((stats.added, stats.removed), (0, 99));
        assert_eq!(i.entry_count(), 2); // d + f000
        assert_eq!(i.tombstone_count(), 99);
        assert!(i.needs_compaction(), "墓碑占比 99/101 > 20%");
        i.compact();
        assert_eq!(i.entry_count(), 2);
        assert_eq!(i.tombstone_count(), 0);
        assert_eq!(
            i.dir_path(d),
            b"docs".to_vec(),
            "整理后路径池按新条目号重建"
        );
    }

    // 批量删除中的类型翻转钉:同一批里既有删除又有 file↔dir 翻转,顺序语义不变。
    #[test]
    fn diff_dir_bulk_mixed_removal_and_type_flip() {
        let mut i = ix();
        let d = i.insert("docs", NO_PARENT, true);
        i.insert("gone.txt", d, false);
        let flip = i.insert("item", d, false); // 磁盘上是目录
        let stats = i.diff_dir(d, &listing(&[("item", true)]));
        assert_eq!((stats.added, stats.removed), (1, 2));
        assert!(!i.entries[flip as usize].is_live(), "旧条目墓碑");
        let kids = i.children_of(d);
        assert_eq!(kids.len(), 1);
        assert!(i.entries[kids[0] as usize].is_dir(), "翻转后为目录");
    }

    // —— Task 8:IndexView 统一读取面(ADR-3);T2 改分段枚举(tech.md §4.3) ——

    #[test]
    fn view_exposes_segments() {
        let mut i = ix();
        let d = i.insert("docs", NO_PARENT, true);
        i.insert("a.txt", d, false);
        let v: &dyn IndexView = &i;
        // 单源 Index 恒一段,段起始号 0,段长 = 条目总数
        let segs = v.entry_segments();
        assert_eq!(segs.len(), 1, "单源 Index 恰一个连续段");
        assert_eq!(segs[0].0, 0);
        assert_eq!(segs[0].1.len(), 2);
        assert!(v.entry(1).is_live());
        assert_eq!(v.match_name(1), b"a.txt");
        assert_eq!(v.display_name(1), b"a.txt");
        assert_eq!(v.dir_path(0), b"docs");
        assert_eq!(v.dir_path(0), b"docs", "目录条目路径段有真值");
        assert_eq!(v.dir_path(1), b"", "文件条目占位段为空");
        assert_eq!(v.full_path_dyn(1), "docs/a.txt");
    }
}
