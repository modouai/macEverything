//! Overlay:目录级增量层(tech.md §4.2,memory-footprint S3 核心)。
//!
//! daemon 常驻 = Base(mmap 只读)+ Overlay(小内存增量)。目录被 diff 时,
//! 该目录的直接子项全集**只**存于 overlay index(anchor 的实际子项)——不另维护
//! children 映射(同一信息的两份表示会在二次 diff 时漂移);下次 diff 同目录时,
//! 旧子项集由 `Index::children_of(anchor)` 动态派生(名称从 arena 读取),与磁盘
//! listing 比对产出 `DiffStats`。
//!
//! 查询侧屏蔽判定是**纯条目号链**(tech.md §4.2 `valid(e)`):仅对 Base 命中条目,
//! 沿父链上溯,最近被 diff 的祖先层以 overlay listing 为准——子项在 `anchors`
//! (新 listing 中存活且为目录)则该子树转由 Base 裁定,否则屏蔽。不做名称逐段
//! 校验:overlay 子项名是 display 形态、Base 拼路径用 match 形态,大小写/Unicode
//! 归一差异会造成漏屏蔽;结构化判定从根上排除该类缺陷。
//!
//! 结构备忘(规格三字段外的派生账本,均与本模块写侧同点维护):
//! - `anchor_base`:anchors 的反向桥(overlay anchor 条目号 → base 目录条目号),
//!   拼路径/合并物化 O(1) 切空间用。单一真相仍是 overlay index 的子项集,本表
//!   只在 `anchors` 同一写点增删(`link_anchor`/`unlink_anchor`)。
//! - `dir_ids`:纯 overlay 目录(冷启动后新建、Base 中无对应条目)的绝对路径寻址
//!   面,daemon 的 diff 入口据它区分 Base/Overlay 两个空间。
//! - overlay index 的路径池:目录条目入 overlay 时**立即写合成完整路径**
//!   (match 形态,`rewrite_dir_path`),而非 `Index::insert` 默认的 overlay 内
//!   局部链——`Index::insert`/`rename_entry` 产出的池段对桥接 anchor 是错的
//!   (上溯在 NO_PARENT 桥断掉),path: 查询按池段取路径会漏 Overlay 目录。
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::arena::ArenaBuilder;
use crate::entry::{Entry, FLAG_TOMBSTONE, NO_PARENT};
use crate::index::{DiffStats, Index, IndexView};

/// 已 diff 目录的 Base 条目号集合(子项全集以 overlay index 中 anchor 的实际子项为准)。
pub struct Overlay {
    overlaid: HashSet<u32>,
    /// base_dir_idx → overlay 内对应目录条目号(拼接 overlay 条目父链 / 屏蔽判定用)。
    anchors: HashMap<u32, u32>,
    /// 反向桥:overlay anchor 条目号 → base_dir_idx(派生账本,与 anchors 同点维护)。
    anchor_base: HashMap<u32, u32>,
    /// 纯 overlay 目录的绝对路径 → overlay 条目号(新建目录的 diff 寻址面)。
    dir_ids: HashMap<PathBuf, u32>,
    /// Overlay 自身的小型 Index(parent 在 overlay 条目号空间内;桥接 anchor 的
    /// parent = NO_PARENT 占位,真实父经 `anchor_base` 切回 base 空间)。
    index: Index,
}

/// diff 目标所在空间:Base 已收录目录,或纯 overlay 目录(冷启动后新建)。
#[derive(Debug, Clone, Copy)]
pub enum DiffTarget {
    /// Base 条目号。
    Base(u32),
    /// Overlay 条目号。
    OverlayDir(u32),
}

impl Overlay {
    pub fn new() -> Self {
        Self {
            overlaid: HashSet::new(),
            anchors: HashMap::new(),
            anchor_base: HashMap::new(),
            dir_ids: HashMap::new(),
            index: Index::new(),
        }
    }

    /// 是否完全无增量(compact/整理只在无 Overlay 时安全:任何账本非空都可能
    /// 锚定 Base 条目号,整理重排会使其失效)。
    pub fn is_empty(&self) -> bool {
        self.overlaid.is_empty()
            && self.anchors.is_empty()
            && self.anchor_base.is_empty()
            && self.dir_ids.is_empty()
            && self.index.entries.is_empty()
    }

    /// 合成视图入口:T3 查询/status 的唯一读取面(Base 段 + Overlay 段稠密拼接)。
    pub fn composite_view<'a>(&'a self, base: &'a dyn IndexView) -> CompositeView<'a> {
        CompositeView::new(base, self)
    }

    /// 纯 overlay 目录寻址(带死键自愈:对应条目已墓碑 → 摘键返回 None)。
    pub fn overlay_dir_id(&mut self, p: &Path) -> Option<u32> {
        let o = *self.dir_ids.get(p)?;
        if self
            .index
            .entries
            .get(o as usize)
            .is_some_and(|e| e.is_live())
        {
            Some(o)
        } else {
            self.dir_ids.remove(p);
            None
        }
    }

    /// 摘除纯 overlay 目录键(diff 目标已死时的对账;幂等)。
    pub fn remove_dir_key(&mut self, p: &Path) {
        self.dir_ids.remove(p);
    }

    /// 被抑制的 base 背书 anchor 数(base 目录活 + anchor 条目活):status 近似
    /// 计数的扣减项——anchor 是桥不是增量条目,查询侧同样不可见(单源 flush 合并
    /// 时它也不会成为独立条目)。O(diffed 目录数),不枚举被屏蔽的 Base 条目。
    pub fn suppressed_anchor_count(&self, base: &dyn IndexView) -> u32 {
        self.anchor_base
            .iter()
            .filter(|(&o, &b)| {
                base.entry(b).is_live()
                    && self
                        .index
                        .entries
                        .get(o as usize)
                        .is_some_and(|e| e.is_live())
            })
            .count() as u32
    }

    /// 目录 diff 写侧(tech.md §4.2):磁盘 listing 到手 → 确保 anchor → 与旧子项集
    /// (`children_of(anchor)` 派生)比对增删 → `overlaid.insert`。
    ///
    /// 失败语义与 `watcher::apply_dir_diff` 一致:listing 读取任一失败一律上抛
    /// 不应用(部分 listing 会把「读不到」误判为「已删除」)。
    pub fn apply_diff(
        &mut self,
        base: &dyn IndexView,
        target: DiffTarget,
        abs_dir: &Path,
        excludes: &[PathBuf],
    ) -> std::io::Result<DiffStats> {
        let listing = crate::watcher::read_dir_listing(abs_dir, excludes)?;
        // 2. 确保 anchor:Base 目录复用既有 anchor(父层 diff 建的桥,父子链已正确);
        //    无则新建(parent = 最近已锚定祖先的 anchor,无则 NO_PARENT 桥接占位)。
        let (anchor, b0) = match target {
            DiffTarget::Base(b) => (self.ensure_anchor(base, b), Some(b)),
            DiffTarget::OverlayDir(o) => (o, None),
        };
        // 3. 旧子项集动态派生(单一真相 = overlay index);名称从 display arena 读取,
        //    与 listing 同为磁盘 display 形态,可比对。
        let mut old: HashMap<String, u32> = self
            .index
            .children_of(anchor)
            .into_iter()
            .map(|k| {
                (
                    String::from_utf8_lossy(self.index.display_name_of(k)).into_owned(),
                    k,
                )
            })
            .collect();
        // Base 直接子项名 → 条目号(仅为目录子项建 anchors 桥;O(n) 全表扫与既有
        // diff_dir 的 children_of 同代价,预算内)。Overlay 目标无 Base 子项可桥。
        let base_children: HashMap<String, u32> = match b0 {
            Some(b) => base
                .children_of(b)
                .into_iter()
                .map(|k| {
                    (
                        String::from_utf8_lossy(base.display_name(k)).into_owned(),
                        k,
                    )
                })
                .collect(),
            None => HashMap::new(),
        };

        let mut added = 0u32;
        let mut removed = 0u32;
        // 统计口径的旧集:overlay anchor 已有子项优先(二次 diff 起 = 唯一真相);
        // 首 diff(overlay 空)取 **base** 子项名集——对外语义与 diff_dir 时代一致
        // (只计真实漂移),抽样对拍依赖「无漂移返回 0」,不得把首 diff 全记 added。
        let prior: HashMap<String, bool> = if old.is_empty() {
            match b0 {
                Some(_) => base_children
                    .iter()
                    .map(|(n, k)| (n.clone(), base.entry(*k).is_dir()))
                    .collect(),
                None => HashMap::new(),
            }
        } else {
            old.iter()
                .map(|(n, k)| (n.clone(), self.index.entries[*k as usize].is_dir()))
                .collect()
        };
        for (name, is_dir) in &listing {
            match prior.get(name) {
                Some(d) if *d == *is_dir => {}
                Some(_) => added += 1, // 类型翻转 = 删 + 增
                None => added += 1,
            }
            if prior.get(name).is_some_and(|d| *d != *is_dir) {
                removed += 1;
            }
            let existing = old.remove(name);
            let existing = match existing {
                // 存活且类型一致:overlay 条目原位保留(目录子项的桥在下方续接)
                Some(k) if self.index.entries[k as usize].is_dir() == *is_dir => Some(k),
                // 类型翻转:旧条目墓碑 + 桥同步摘除,按下方分支重建;
                // 计数已按 prior 集完成,此处只落写侧
                other => {
                    if let Some(k) = other {
                        self.unlink_anchor(k);
                        self.index.remove(k);
                    }
                    None
                }
            };
            let entry = match existing {
                Some(k) => k,
                None if !*is_dir => self.index.insert(name, anchor, false),
                None => {
                    // 目录新增前的**桥复用**:同名 base 子目录已有活桥(子目录
                    // 先于父被 diff 时,其 anchor 是顶层桥接占位、不在本 anchor
                    // 的子项集里)——复用并 re-parent 到当前 anchor,而非新建。
                    // 新建 + link 覆盖会让旧桥链变孤儿:其下真实 overlay 子树
                    // 仍活仍命中(同路径重复结果行),re-diff 每轮复制一份
                    // (无界增长),删除方向 sweep 按 anchors 键摘不到旧桥
                    // (anchor_base 残留),物化时幽灵根腐化落盘。
                    let reused = base_children
                        .get(name)
                        .copied()
                        .filter(|&b| base.entry(b).is_live() && base.entry(b).is_dir())
                        .and_then(|b| self.anchors.get(&b).copied())
                        .filter(|&o| {
                            self.index
                                .entries
                                .get(o as usize)
                                .is_some_and(|e| e.is_live())
                        });
                    match reused {
                        Some(o) => {
                            // 池段免重写:合成路径 = 链上最高桥的 base 段 + 中间段,
                            // 挂接点变化恒等互换(Index::reparent 注)
                            self.index.reparent(o, anchor);
                            o
                        }
                        None => {
                            let k = self.index.insert(name, anchor, true);
                            self.rewrite_dir_path(base, k);
                            k
                        }
                    }
                }
            };
            if *is_dir {
                // 目录子项寻址面:base 已收录目录走 dir_map(resolve_target 先查
                // dir_map,dir_ids 键不会被消费,插了只是死内存);只给无 base 对应
                // 的纯 overlay 目录入 dir_ids。
                // 桥接只连「base 中确为目录」的子项:file→dir 翻转时 base 同名条目
                // 是陈旧**文件**,桥连它会让陈旧文件条目 valid(掩不住,is_dir 永久
                // 错)而新 overlay 目录被抑制,且无自愈路径——翻转场景 base 文件走
                // 「不在 anchors → 屏蔽」,由 overlay 目录条目服务。
                match base_children.get(name) {
                    Some(&b) if base.entry(b).is_live() && base.entry(b).is_dir() => {
                        self.link_anchor(b, entry);
                    }
                    _ => {
                        self.dir_ids.insert(abs_dir.join(name), entry);
                    }
                }
            }
        }
        // 清单级删除:墓碑制级联(Index::remove);被删目录子项的 anchors 桥同步
        // 摘除(否则 Base 侧陈旧子树被误判为有效)。removed 计入 prior 缺席项。
        for (name, k) in old {
            self.unlink_anchor(k);
            self.index.remove(k);
            self.remove_dir_key(&abs_dir.join(&name));
            // 深层 anchor 兜底:该子树内此前自身被 diff 过的目录,其 anchor 不在
            // 本 anchor 直接子项的 unlink 射程内(级联只墓碑 overlay 条目,
            // anchors/overlaid/dir_ids 键残留会让 base 深层子树泄漏命中)——
            // 按被删子项路径前缀扫账本(低频删除路径的写侧路径比对)。
            self.purge_prefix(base, &abs_dir.join(&name));
        }
        // base 子目录从 listing 消失**或降级为文件**的删除方向:本 anchor 的
        // overlay 旧子项集(old)对「消失」看不见——子树此前自身被 diff 过时其
        // anchor 是独立桥(顶层占位),父首 diff 时 old 为空更是整棵不可见;
        // 对「降级」(rm -rf d1 && touch d1)则名字仍在 listing,旧条件会放过,
        // 残留深层桥让 base 陈旧子树复活(评审 C1 变体 A)。仅扫 base 目录子项
        // (文件子项无子树,天然无账本);文件路径 purge 天然空转。
        for (name, &b) in &base_children {
            if base.entry(b).is_dir() && listing.get(name) != Some(&true) {
                self.purge_prefix(base, &abs_dir.join(name));
            }
        }
        removed += prior
            .keys()
            .filter(|n| !listing.contains_key(n.as_str()))
            .count() as u32;
        // 4. Base 中 D 的直接子项从此作废(查询侧屏蔽判定依据 overlaid 键集)。
        if let Some(b) = b0 {
            self.overlaid.insert(b);
        }
        Ok(DiffStats { added, removed })
    }

    /// 登记 base↔overlay anchor 对。同 base 目录已有**异条目**旧桥(历史覆盖
    /// 遗留的孤儿)时清扫:旧桥级联墓碑(其 overlay 子树是上一次 listing 的
    /// 陈旧真相,可见性由新桥条目 / base 子树兜底,不丢可见数据),anchor_base
    /// 键同步清除——残留会让旧桥链逃过删除方向 sweep 并在物化时成为幽灵根。
    fn link_anchor(&mut self, b: u32, o: u32) {
        if let Some(&prev) = self.anchors.get(&b) {
            if prev != o {
                self.anchor_base.remove(&prev);
                self.index.remove(prev); // 级联墓碑旧桥链
            }
        }
        self.anchors.insert(b, o);
        self.anchor_base.insert(o, b);
    }

    /// 摘除 overlay 条目(若为 anchor)的双向桥。条目墓碑后桥残留会让查询侧把
    /// Base 陈旧子树误判为有效(`anchors.contains(e)` 恒真),必须同点清理。
    fn unlink_anchor(&mut self, o: u32) {
        if let Some(b) = self.anchor_base.remove(&o) {
            self.anchors.remove(&b);
        }
    }

    /// 按 base 目录号摘除双向桥(purge 前缀扫的收口入口;返回被摘的 overlay
    /// 条目号供级联墓碑)。与 [`Overlay::unlink_anchor`] 同一套账本纪律,
    /// 只是定位方向相反。
    fn unlink_anchor_by_base(&mut self, b: u32) -> Option<u32> {
        let a = self.anchors.remove(&b)?;
        self.anchor_base.remove(&a);
        Some(a)
    }

    /// 确保 Base 目录的 anchor 条目存在,返回其 overlay 条目号。
    /// 已有(父层 diff 建的桥,或自身上次 diff)→ 复用;无则新建:parent 指向
    /// overlay 内最近的已锚定祖先 anchor,D 尚无 overlay 祖先时 parent = 桥接
    /// 占位(NO_PARENT),旁表 `anchors`/`anchor_base` 记 base↔overlay 对。
    fn ensure_anchor(&mut self, base: &dyn IndexView, b0: u32) -> u32 {
        if let Some(&a) = self.anchors.get(&b0) {
            return a;
        }
        let mut parent_anchor = NO_PARENT;
        let mut cur = base.entry(b0).parent;
        while cur != NO_PARENT {
            if let Some(&a) = self.anchors.get(&cur) {
                parent_anchor = a;
                break;
            }
            cur = base.entry(cur).parent;
        }
        let name = String::from_utf8_lossy(base.display_name(b0)).into_owned();
        let a = self.index.insert(&name, parent_anchor, true);
        // 先建桥再写路径:rewrite_dir_path 上溯到自身(桥接 anchor)时要查 anchor_base
        self.link_anchor(b0, a);
        self.rewrite_dir_path(base, a);
        a
    }

    /// 目录条目的合成完整路径(match 形态)写入 overlay 路径池(条目号对齐):
    /// 沿 overlay 父链上溯至桥接 anchor,切 base 目录号续 base 链,两段拼接。
    /// 桥接 anchor 自身名 = base 目录名(base 链已含),不入段。父链在建条目时
    /// 已定型,路径只需写一次(条目不改名,改名=删+增)。
    fn rewrite_dir_path(&mut self, base: &dyn IndexView, o: u32) {
        let mut parts: Vec<Vec<u8>> = Vec::new();
        let mut cur = o;
        let base_dir;
        loop {
            let e = &self.index.entries[cur as usize];
            if e.parent == NO_PARENT {
                // 桥接 anchor:必有 base 背书(写侧同点登记);缺失即账本腐化,响亮暴露
                base_dir = self.anchor_base[&cur];
                break;
            }
            parts.push(self.index.match_name_of(cur).to_vec());
            cur = e.parent;
        }
        parts.reverse();
        let mut bytes = base.dir_path(base_dir).to_vec();
        for p in parts {
            bytes.push(b'/');
            bytes.extend_from_slice(&p);
        }
        let seg = self.index.path_pool.push(&bytes);
        self.index.path_offsets[o as usize] = seg;
    }

    /// 摘除以 `prefix` 为路径前缀的子树所波及的 Overlay 账本(`scanner::remove_prefix`
    /// 的同步收尾):受影响 base 目录(overlaid ∪ anchors 键)按 Base 完整路径前缀
    /// 命中——anchor 整链墓碑(级联覆盖其 overlay 子树),键集摘除;dir_ids 同前缀
    /// 清键。写侧路径比对(低频摘除路径),与查询侧零字符串判定互不影响。
    pub fn purge_prefix(&mut self, base: &dyn IndexView, prefix: &Path) {
        let victims: Vec<u32> = self
            .overlaid
            .iter()
            .copied()
            .chain(self.anchors.keys().copied())
            .collect::<HashSet<_>>()
            .into_iter()
            .filter(|&b| {
                let p = base.full_path_dyn(b);
                Path::new(&p).starts_with(prefix)
            })
            .collect();
        for b in victims {
            if let Some(a) = self.unlink_anchor_by_base(b) {
                self.index.remove(a); // 级联:anchor 下的 overlay 子树一并墓碑
            }
            self.overlaid.remove(&b);
        }
        let p = prefix.to_path_buf();
        self.dir_ids.retain(|k, _| !k.starts_with(&p));
    }
}

impl Default for Overlay {
    fn default() -> Self {
        Self::new()
    }
}

/// 双源合成视图(tech.md §4.2 查询侧):Base 全段 + Overlay 全段,**稠密拼接**
/// (T2 `entry_segments` 契约:有序无缝覆盖 0..N−1)。Overlay 条目在合成空间中
/// = `base_count + overlay_idx`;屏蔽判定只针对 Base 命中(Overlay 命中天然有效,
/// 仅抑制「base 背书 anchor」的双份),parent 链上溯在两空间各自进行、经
/// `anchors` 桥接。
pub struct CompositeView<'a> {
    base: &'a dyn IndexView,
    /// Base 空间条目槽位数(含墓碑):两空间的分界 = Overlay 段起始号。
    base_count: u32,
    overlay: &'a Overlay,
    /// Overlay 是否携带增量(flush/重扫后的常态为空):空时 entry_valid 顶部
    /// 短路,Base 命中零屏蔽判定开销。
    overlay_active: bool,
}

impl<'a> CompositeView<'a> {
    pub fn new(base: &'a dyn IndexView, overlay: &'a Overlay) -> Self {
        // 分界取 Base 段枚举的实际末位(段契约:首段起 0、按序稠密)
        let base_count = base
            .entry_segments()
            .last()
            .map(|&(s, seg)| s + seg.len() as u32)
            .unwrap_or(0);
        Self {
            base,
            base_count,
            overlay,
            overlay_active: !overlay.is_empty(),
        }
    }

    fn overlay_entry(&self, idx: u32) -> &Entry {
        &self.overlay.index.entries[(idx - self.base_count) as usize]
    }
}

impl IndexView for CompositeView<'_> {
    fn entry_segments(&self) -> Vec<(u32, &[Entry])> {
        let mut segs = self.base.entry_segments();
        segs.push((self.base_count, self.overlay.index.entries.as_slice()));
        segs
    }

    fn entry(&self, idx: u32) -> &Entry {
        if idx < self.base_count {
            self.base.entry(idx)
        } else {
            self.overlay_entry(idx)
        }
    }

    /// 父条目号(合成空间):Overlay 条目的父存的是 overlay 空间号,必须重映射
    /// (base_count + 父)——query 侧 dir_hits 按合成号直下标,错空间号会命中
    /// 无关槽位(评审缺陷:path 查询对 overlay 新增文件假阴性/对无关条目假阳性)。
    fn parent_idx(&self, idx: u32) -> u32 {
        if idx < self.base_count {
            self.base.entry(idx).parent
        } else {
            let p = self.overlay_entry(idx).parent;
            if p == NO_PARENT {
                NO_PARENT
            } else {
                self.base_count + p
            }
        }
    }

    fn match_name(&self, idx: u32) -> &[u8] {
        if idx < self.base_count {
            self.base.match_name(idx)
        } else {
            self.overlay.index.match_name(idx - self.base_count)
        }
    }

    fn display_name(&self, idx: u32) -> &[u8] {
        if idx < self.base_count {
            self.base.display_name(idx)
        } else {
            self.overlay.index.display_name(idx - self.base_count)
        }
    }

    /// 目录完整路径(match 形态)。Base 条目走 Base 路径池;Overlay 目录读写侧
    /// 预写的合成路径段(`rewrite_dir_path`,两段拼接的唯一持久形态)。
    fn dir_path(&self, idx: u32) -> &[u8] {
        if idx < self.base_count {
            self.base.dir_path(idx)
        } else {
            <Index as IndexView>::dir_path(&self.overlay.index, idx - self.base_count)
        }
    }

    /// 拼路径唯一入口(tech.md §4.2 `full_path_composite`):trait 默认实现按
    /// 合成空间直读 `entry(cur).parent`,而 overlay 父链存的是 **overlay 空间**
    /// 条目号——默认实现会在桥接 anchor 处把它当 base 条目号跳进无关子树
    /// (实机冒烟实测:overlay 新文件拼出「…/index.bin/新文件」错路径)。
    /// 必须整体改道 [`CompositeView::full_path_composite`] 的两段拼接。
    fn full_path_dyn(&self, idx: u32) -> String {
        self.full_path_composite(idx)
    }

    /// 条目有效性(tech.md §4.2)。
    /// - Base 命中:`valid(e)` 条目号链上溯——最近被 diff 的祖先层以 listing 为准;
    ///   全链无 overlay → 有效。零字符串比较,与拼路径同一条父链。
    /// - Overlay 命中:天然有效,唯「base 背书的 anchor」抑制——该目录条目在 Base
    ///   侧同名额本就是权威命中,不抑制会双份(子树已转 Base 裁定,anchor 自身
    ///   的名字也属 Base 裁定范围)。
    fn entry_valid(&self, idx: u32) -> bool {
        if !self.overlay_active {
            return true; // 无增量(flush/重扫后常态):全量有效,零判定开销
        }
        if idx < self.base_count {
            let mut cur = idx;
            loop {
                let p = self.base.entry(cur).parent;
                if p == NO_PARENT {
                    return true; // 终止条件 1:到根,全链无 overlay → 有效
                }
                if self.overlay.overlaid.contains(&p) {
                    // 终止条件 2:最近祖先被 diff,直接子项层以 listing 为准。
                    // 加固(C1 纵深防御):键存在但锚定条目已墓碑的残键一律按
                    // 无效处理——退化为保守屏蔽,而非泄漏陈旧子树(变体 B:
                    // 深层 anchors 键在级联墓碑后若未及摘除,contains_key 会
                    // 让 base 陈旧目录复活)。
                    return self.overlay.anchors.get(&cur).is_some_and(|&o| {
                        self.overlay
                            .index
                            .entries
                            .get(o as usize)
                            .is_some_and(|e| e.is_live())
                    });
                }
                cur = p; // p 未被 diff:p 的存活性由其父链裁定,递归上溯
            }
        }
        match self.overlay.anchor_base.get(&(idx - self.base_count)) {
            None => true,
            Some(&b) => !self.base.entry(b).is_live(),
        }
    }
}

impl CompositeView<'_> {
    /// Overlay 条目拼路径(display 原始形态):沿 overlay parent 链上溯至桥接
    /// anchor 根,切换到 base 目录号继续 base 链——两段拼接,`full_path_composite`
    /// 单一入口(tech.md §4.2;daemon 结果行/stat 路径共用)。桥接 anchor 自身名
    /// = base 目录名(base 链已含),不入段。
    pub fn full_path_composite(&self, idx: u32) -> String {
        if idx < self.base_count {
            return self.base.full_path_dyn(idx);
        }
        let mut parts: Vec<Vec<u8>> = Vec::new();
        let mut cur = idx - self.base_count;
        let base_dir;
        loop {
            let e = &self.overlay.index.entries[cur as usize];
            if e.parent == NO_PARENT {
                // 桥接 anchor:必有 base 背书(写侧同点登记);缺失即账本腐化,响亮暴露
                base_dir = self.overlay.anchor_base[&cur];
                break;
            }
            parts.push(self.overlay.index.display_name_of(cur).to_vec());
            cur = e.parent;
        }
        parts.reverse();
        let mut out = self.base.full_path_dyn(base_dir).into_bytes();
        for p in parts {
            out.push(b'/');
            out.extend_from_slice(&p);
        }
        String::from_utf8_lossy(&out).into_owned()
    }
}

/// 物化中间缓冲:顺序追加条目 + 双 arena 拷贝 + live/墓碑计数。
struct EntrySink {
    entries: Vec<Entry>,
    display: ArenaBuilder,
    match_arena: ArenaBuilder,
    live: u32,
    tombstones: u32,
}

impl EntrySink {
    fn push(&mut self, d: &[u8], m: &[u8], parent: u32, flags: u8) {
        let (ds, dl) = self.display.push(d);
        let (ms, ml) = self.match_arena.push(m);
        self.entries.push(Entry {
            display_start: ds,
            display_len: dl,
            match_start: ms,
            match_len: ml,
            parent,
            flags,
        });
        if flags & FLAG_TOMBSTONE == 0 {
            self.live += 1;
        } else {
            self.tombstones += 1;
        }
    }
}

/// 合成空间一次性物化为独立 Index(「Base + Overlay 顺序遍历」,tech.md §4.3):
/// 产物 = **合成视图的可见集**。Base 段跳过墓碑与被屏蔽条目(valid(e) 判定,
/// 条目号经映射表重排——被屏蔽条目落盘会在重启后失去 Overlay 屏蔽面而复活);
/// 「活条目的父必活」+ 屏蔽判定沿子树闭合,保证留存条目的父必已留存。
/// Overlay 段中,**anchor 是桥不是节点**——它背书的 base 目录条目已是权威本体,
/// 物化时整条略去,其子项改挂到 base 目录号下;纯 overlay 条目按序追加,overlay
/// 父为 anchor → 切 base 号,为纯 overlay 目录 → 用已映射的合成号。路径池按活
/// 目录经父链重建(与 materialize/compact 同法,复用 `dir_path_bytes` 唯一实现)。
/// T3 flush 的过渡写形态(T4 改条目流直写);落盘条目号为全新空间,内存侧
/// base/overlay/dir_map 不受影响(索引文件只被下次冷启动消费,全量重建映射)。
pub fn materialize_composite(base: &dyn IndexView, overlay: &Overlay) -> Index {
    let base_count: u32 = base
        .entry_segments()
        .iter()
        .map(|&(_, seg)| seg.len() as u32)
        .sum();
    let mut sink = EntrySink {
        entries: Vec::with_capacity(base_count as usize + overlay.index.entries.len()),
        display: ArenaBuilder::default(),
        match_arena: ArenaBuilder::default(),
        live: 0,
        tombstones: 0,
    };
    let view = CompositeView::new(base, overlay);
    // base 条目号 → 物化条目号(墓碑/被屏蔽条目不占号;NO_PARENT = 未留存)
    let mut base_map: Vec<u32> = vec![NO_PARENT; base_count as usize];
    for &(start, seg) in &base.entry_segments() {
        for (j, e) in seg.iter().enumerate() {
            let idx = start + j as u32;
            if !e.is_live() || !view.entry_valid(idx) {
                continue;
            }
            let parent = if e.parent == NO_PARENT {
                NO_PARENT
            } else {
                let mapped = base_map[e.parent as usize];
                debug_assert_ne!(
                    mapped, NO_PARENT,
                    "留存条目的父必已留存(级联墓碑 + 屏蔽沿子树闭合)"
                );
                mapped
            };
            let d = base.display_name(idx).to_vec();
            let m = base.match_name(idx).to_vec();
            sink.push(&d, &m, parent, e.flags);
            base_map[idx as usize] = sink.entries.len() as u32 - 1;
        }
    }
    // overlay 条目号 → 物化条目号(anchor 略去不占号;纯 overlay 条目按插入序追加,
    // 其父纯 overlay 目录必先插入——目录先于子项创建,单趟映射成立)。
    let mut merged_of: HashMap<u32, u32> = HashMap::new();
    for (o, e) in overlay.index.entries.iter().enumerate() {
        let o = o as u32;
        if !e.is_live() || overlay.anchor_base.contains_key(&o) {
            // 墓碑与 anchor 桥均不入物化:墓碑是死条目(摘除删除方向会留下
            // 「曾为桥接 anchor」的墓碑——parent = NO_PARENT,若按非 anchor
            // 路径走会触发幽灵根 panic,评审缺陷 2 实测);anchor 的 base 目录
            // 已在本体,物化树不再收录。
            continue;
        }
        let parent = if e.parent == NO_PARENT {
            panic!("非 anchor 条目的 parent 必指向 overlay 内条目")
        } else if let Some(&b) = overlay.anchor_base.get(&e.parent) {
            let mapped = base_map[b as usize];
            debug_assert_ne!(mapped, NO_PARENT, "anchor 的 base 目录必须已留存");
            mapped // 父为 anchor → 挂其 base 目录
        } else {
            merged_of[&e.parent] // 父为纯 overlay 目录 → 用已映射合成号
        };
        let d = overlay.index.display_name_of(o).to_vec();
        let m = overlay.index.match_name_of(o).to_vec();
        sink.push(&d, &m, parent, e.flags);
        merged_of.insert(o, sink.entries.len() as u32 - 1);
    }
    let EntrySink {
        entries,
        display,
        match_arena,
        live,
        tombstones,
    } = sink;
    let n_slots = entries.len();
    let mut out = Index::from_parts(
        entries,
        display,
        match_arena,
        ArenaBuilder::default(),
        vec![(0, 0); n_slots],
        live,
        tombstones,
    );
    // 路径池重建:先拷好父链再建池(dir_path_bytes 依赖父链,与 materialize 同序)
    for n in 0..out.entries.len() {
        if out.entries[n].is_dir() && out.entries[n].is_live() {
            let pp = out.dir_path_bytes(n as u32);
            out.path_offsets[n] = out.path_pool.push(&pp);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query;

    /// 受控树扫描(真实磁盘夹具:父链/路径池与首扫产物同构)。
    fn scan(root: &Path) -> Index {
        crate::scanner::scan_roots(&[root.to_path_buf()], &[], 1).0
    }

    /// 按完整路径定位 base 条目号(测试夹具用,O(n) 可接受)。
    fn id_of(base: &Index, p: &Path) -> u32 {
        (0..base.entries.len() as u32)
            .find(|&i| base.full_path(i) == p)
            .unwrap_or_else(|| panic!("base 应含 {}", p.display()))
    }

    /// 合成视图查询命中路径全集(含根,与 ref_walk 集合同口径)。
    fn hit_paths(view: &CompositeView) -> std::collections::BTreeSet<String> {
        let out = query::execute(view, &query::parse("*").unwrap(), usize::MAX);
        let set: std::collections::BTreeSet<String> = out
            .hits
            .iter()
            .map(|&h| view.full_path_composite(h))
            .collect();
        // 根条目(parent == NO_PARENT)不在父链上,full_path_composite 即其名;
        // execute 的 * 全量命中必含根,集合口径与 ref_walk ∪ {root} 对齐。
        assert_eq!(set.len(), out.total as usize, "命中路径不得互相折叠");
        set
    }

    /// 参考递归枚举(独立实现,与 parity.rs 同语义;symlink 不跟、排除剪枝)。
    fn ref_walk(root: &Path, out: &mut std::collections::BTreeSet<String>) {
        let Ok(rd) = std::fs::read_dir(root) else {
            return;
        };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            out.insert(e.path().to_string_lossy().into_owned());
            if ft.is_dir() {
                ref_walk(&e.path(), out);
            }
        }
    }

    fn set(paths: &[&str]) -> std::collections::BTreeSet<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

    fn total(view: &CompositeView, q: &str) -> u32 {
        query::execute(view, &query::parse(q).unwrap(), usize::MAX).total
    }

    /// 目录 diff 后立即查询:旧路径为空、新路径可见、幸存文件恰一份
    /// (mutation M1:去掉 `anchors.contains(e)` 终止 → 旧路径命中回归,必红)。
    #[test]
    fn diff_masks_deleted_and_serves_new() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::write(root.join("a.txt"), "").unwrap();
        std::fs::write(root.join("gone.txt"), "").unwrap();
        let base = scan(root);
        let root_id = id_of(&base, root);

        // 磁盘漂移后 diff:gone.txt 删除、new.txt 新增
        std::fs::remove_file(root.join("gone.txt")).unwrap();
        std::fs::write(root.join("new.txt"), "").unwrap();
        let mut ov = Overlay::new();
        let stats = ov
            .apply_diff(&base, DiffTarget::Base(root_id), root, &[])
            .unwrap();
        // 增删对外口径 = 真实漂移(与 diff_dir 时代一致):首 diff 的统计旧集取
        // base 子项名集(overlay 层旧集为空只作写入起点),gone.txt 删除、new.txt 新增。
        assert_eq!((stats.added, stats.removed), (1, 1), "清单级增删计数");

        let view = ov.composite_view(&base);
        assert_eq!(total(&view, "gone"), 0, "已删路径必须被屏蔽");
        assert_eq!(total(&view, "new.txt"), 1, "新路径必须可见");
        assert_eq!(total(&view, "a.txt"), 1, "幸存文件恰一份(base 副本已屏蔽)");
        let out = query::execute(&view, &query::parse("new").unwrap(), 10);
        assert_eq!(
            view.full_path_composite(out.hits[0]),
            root.join("new.txt").to_string_lossy()
        );
    }

    /// 目录子项幸存:base 子树继续裁定且恰一份(mutation M4:去掉合成视图对
    /// 「base 背书 anchor」的抑制 → 目录双份命中,必红)。
    #[test]
    fn surviving_dir_child_single_hit_and_subtree_served() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::create_dir(root.join("keepdir")).unwrap();
        std::fs::write(root.join("keepdir/deep.txt"), "").unwrap();
        std::fs::write(root.join("f.txt"), "").unwrap();
        let base = scan(root);
        let root_id = id_of(&base, root);

        std::fs::write(root.join("x.txt"), "").unwrap();
        std::fs::remove_file(root.join("f.txt")).unwrap();
        let mut ov = Overlay::new();
        ov.apply_diff(&base, DiffTarget::Base(root_id), root, &[])
            .unwrap();
        let view = ov.composite_view(&base);

        assert_eq!(total(&view, "keepdir"), 1, "幸存目录恰一份(不双份)");
        assert_eq!(total(&view, "deep"), 1, "幸存目录子树由 base 裁定,仍可见");
        // path: 词:x.txt 挂在 root(非命中目录)下不入集,deep.txt 为 keepdir 唯一子项
        assert_eq!(total(&view, "path:keepdir"), 1, "deep.txt");
    }

    /// overlay 嵌套两层:路径两段拼接、全量对拍与独立枚举零差异。
    #[test]
    fn nested_two_level_overlay_matches_reference() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::create_dir_all(root.join("d1/d2")).unwrap();
        std::fs::write(root.join("d1/d2/leaf.txt"), "").unwrap();
        let base = scan(root);
        let root_id = id_of(&base, root);

        // 三层各自漂移:root 加文件、d1 加文件、d2 加文件 + 删 leaf
        std::fs::write(root.join("n0.txt"), "").unwrap();
        std::fs::write(root.join("d1/n1.txt"), "").unwrap();
        std::fs::write(root.join("d1/d2/n2.txt"), "").unwrap();
        std::fs::remove_file(root.join("d1/d2/leaf.txt")).unwrap();

        let mut ov = Overlay::new();
        ov.apply_diff(&base, DiffTarget::Base(root_id), root, &[])
            .unwrap();
        let d1_id = id_of(&base, &root.join("d1"));
        ov.apply_diff(&base, DiffTarget::Base(d1_id), &root.join("d1"), &[])
            .unwrap();
        let d2_id = id_of(&base, &root.join("d1/d2"));
        ov.apply_diff(&base, DiffTarget::Base(d2_id), &root.join("d1/d2"), &[])
            .unwrap();

        let view = ov.composite_view(&base);
        // 独立参考枚举(现磁盘态)+ 根,与合成视图命中集零差异
        let mut reference = std::collections::BTreeSet::new();
        ref_walk(root, &mut reference);
        reference.insert(root.to_string_lossy().into_owned());
        assert_eq!(hit_paths(&view), reference, "嵌套两层 overlay 对拍零差异");

        // 两段拼接:overlay 深层新文件的完整路径正确
        assert_eq!(total(&view, "n2"), 1);
        let out = query::execute(&view, &query::parse("n2").unwrap(), 10);
        assert_eq!(
            view.full_path_composite(out.hits[0]),
            root.join("d1/d2/n2.txt").to_string_lossy()
        );
        // path: 词命中 overlay 目录预写路径段(rewrite_dir_path 的钉;
        // 突变「不写 overlay 路径池」→ path:d1 漏掉 n1/n2,必红)。
        // 命中集口径:路径含 d1 的目录(base d1/d2、overlay d1/d2)的全部子项
        // ——d2(base 目录,父层 listing 幸存)、n1、n2;leaf 已删被屏蔽。
        let out_d1 = query::execute(&view, &query::parse("path:d1").unwrap(), 10);
        let d1_set: std::collections::BTreeSet<String> = out_d1
            .hits
            .iter()
            .map(|&h| view.full_path_composite(h))
            .collect();
        assert_eq!(
            d1_set,
            set(&[
                &root.join("d1/d2").to_string_lossy(),
                &root.join("d1/n1.txt").to_string_lossy(),
                &root.join("d1/d2/n2.txt").to_string_lossy(),
            ]),
            "path:d1 命中集(base 幸存目录 + 两层 overlay 新文件)"
        );
    }

    /// 子目录 diff 后父未再 diff(终止条件 1/2 的组合):
    /// 子先 diff(桥接 anchor)→ 子树 overlay 裁定;父仍 Base → 父层新文件不可见;
    /// 父随后 diff → 子树仍裁定有效(mutation M2:破坏「有 anchor 不在 overlaid
    /// → 终止有效」,如把未 diff 的锚定目录一并屏蔽 → 本测后半必红)。
    #[test]
    fn subdir_diff_then_parent_diff_keeps_subtree() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::create_dir_all(root.join("parent/child")).unwrap();
        std::fs::write(root.join("parent/child/c.txt"), "").unwrap();
        std::fs::write(root.join("parent/p.txt"), "").unwrap();
        let base = scan(root);
        let parent_id = id_of(&base, &root.join("parent"));

        // 子目录先 diff:父从未 diff,child 走桥接占位
        std::fs::write(root.join("parent/child/new.txt"), "").unwrap();
        let mut ov = Overlay::new();
        ov.apply_diff(
            &base,
            DiffTarget::Base(id_of(&base, &root.join("parent/child"))),
            &root.join("parent/child"),
            &[],
        )
        .unwrap();
        {
            let view = ov.composite_view(&base);
            assert_eq!(total(&view, "c.txt"), 1, "子树 overlay 裁定:旧文件仍可见");
            assert_eq!(total(&view, "new"), 1, "子树 overlay 裁定:新文件可见");
        }

        // 父层新文件:父未 diff → Base 陈旧,不可见(预期语义)
        std::fs::write(root.join("parent/newp.txt"), "").unwrap();
        {
            let view = ov.composite_view(&base);
            assert_eq!(total(&view, "newp"), 0, "父未 diff,父层新文件不可见");
        }

        // 父随后 diff:child 在父 listing 中幸存且为目录 → 桥续接,子树仍有效
        ov.apply_diff(
            &base,
            DiffTarget::Base(parent_id),
            &root.join("parent"),
            &[],
        )
        .unwrap();
        let view = ov.composite_view(&base);
        // whole: 精确到整词:纯子串 "new" 会同时误中 newp.txt,"p.txt" 会误中 newp.txt
        assert_eq!(
            total(&view, "whole:c.txt"),
            1,
            "父 diff 后子树仍以父层 listing 裁定"
        );
        assert_eq!(
            total(&view, "whole:new.txt"),
            1,
            "child 内 overlay 增量不受父 diff 影响"
        );
        assert_eq!(
            total(&view, "whole:newp.txt"),
            1,
            "父 diff 后父层新文件可见"
        );
        assert_eq!(total(&view, "whole:p.txt"), 1, "父层幸存文件恰一份");
    }

    /// 大小写/Unicode 名称形态:结构化条目号判定不受形态影响。
    /// 突变「屏蔽判定改走名称校验(match 折叠形态)」→ 同折叠名的替换文件被误判
    /// 为「仍在 listing」→ base 陈旧条目不屏蔽,双份命中,本测必红。
    #[test]
    fn masking_is_structural_not_name_based() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::write(root.join("OLD.TXT"), "").unwrap();
        std::fs::write(root.join("cafe\u{301}.md"), "").unwrap(); // NFD 形态
        let base = scan(root);
        let root_id = id_of(&base, root);

        // 删除后以「同折叠名、异形态」重建:OLD.TXT → old.txt,NFD café → NFC café
        std::fs::remove_file(root.join("OLD.TXT")).unwrap();
        std::fs::write(root.join("old.txt"), "").unwrap();
        std::fs::remove_file(root.join("cafe\u{301}.md")).unwrap();
        std::fs::write(root.join("caf\u{e9}.md"), "").unwrap(); // NFC 形态

        let mut ov = Overlay::new();
        ov.apply_diff(&base, DiffTarget::Base(root_id), root, &[])
            .unwrap();
        let view = ov.composite_view(&base);
        // whole: 精确整词:扫描根路径(…/folders/…)含子串 "old",会污染子串计数
        assert_eq!(
            total(&view, "whole:old.txt"),
            1,
            "形态替换后恰一份(结构化屏蔽)"
        );
        assert_eq!(
            total(&view, "whole:caf\u{e9}.md"),
            1,
            "NFD→NFC 替换后恰一份"
        );
        assert_eq!(
            total(&view, "case:old.txt"),
            1,
            "base 侧 OLD.TXT 被屏蔽,唯一命中为 overlay 新名"
        );
    }

    /// 子项消失时的 anchors 桥清理:base 目录在 listing 中被删除后,其 base 子树
    /// 必须随父层 listing 作废(桥残留 = 陈旧子树「有效」的静默数据错误)。
    #[test]
    fn removed_dir_child_bridge_is_unlinked() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/deep.txt"), "").unwrap();
        let base = scan(root);
        let root_id = id_of(&base, root);

        // 第一轮:root diff,sub 幸存 → 桥建立,子树可见
        std::fs::write(root.join("x.txt"), "").unwrap();
        let mut ov = Overlay::new();
        ov.apply_diff(&base, DiffTarget::Base(root_id), root, &[])
            .unwrap();
        {
            let view = ov.composite_view(&base);
            assert_eq!(total(&view, "deep"), 1);
        }

        // 第二轮:sub 从磁盘消失;root 再 diff → 桥必须摘除
        std::fs::remove_dir_all(root.join("sub")).unwrap();
        ov.apply_diff(&base, DiffTarget::Base(root_id), root, &[])
            .unwrap();
        let view = ov.composite_view(&base);
        assert_eq!(total(&view, "deep"), 0, "已删目录子树必须随父 listing 作废");
        assert!(
            !ov.anchors.contains_key(&id_of(&base, &root.join("sub"))),
            "被删目录子项的 anchors 键必须随父层 diff 摘除"
        );
    }

    /// file→dir 类型翻转:base 陈旧**文件**条目不得被桥连(缺陷 1 回归钉)。
    /// 突变「link 守卫去掉 is_dir 检查」→ 陈旧文件条目 valid(is_dir=false)而
    /// overlay 目录被抑制,本测 is_dir 断言必红。
    #[test]
    fn type_flip_file_to_dir_never_bridges_base_file() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::write(root.join("thing"), "").unwrap();
        let base = scan(root);
        let root_id = id_of(&base, root);

        // 磁盘:同名文件翻转为目录
        std::fs::remove_file(root.join("thing")).unwrap();
        std::fs::create_dir(root.join("thing")).unwrap();
        std::fs::write(root.join("thing/inner.txt"), "").unwrap();
        let mut ov = Overlay::new();
        ov.apply_diff(&base, DiffTarget::Base(root_id), root, &[])
            .unwrap();
        {
            let view = ov.composite_view(&base);
            let out = query::execute(&view, &query::parse("whole:thing").unwrap(), 10);
            assert_eq!(out.total, 1, "翻转后恰一份(陈旧 base 文件被屏蔽)");
            assert!(
                view.entry(out.hits[0]).is_dir(),
                "唯一命中必须是 overlay 目录条目,不得是 base 陈旧文件"
            );
            assert_eq!(total(&view, "whole:inner"), 0, "thing 未 diff,子内容不可见");
        }

        // diff 翻转后的目录(纯 overlay 寻址)→ 子内容可见;物化无 panic
        let thing = ov
            .overlay_dir_id(&root.join("thing"))
            .expect("thing 应入寻址面");
        ov.apply_diff(
            &base,
            DiffTarget::OverlayDir(thing),
            &root.join("thing"),
            &[],
        )
        .unwrap();
        {
            let view = ov.composite_view(&base);
            assert_eq!(total(&view, "whole:inner"), 1, "翻转目录 diff 后子内容可见");
        }
        let merged = materialize_composite(&base, &ov);
        let paths: std::collections::BTreeSet<String> = (0..merged.entries.len() as u32)
            .filter(|&i| merged.entries[i as usize].is_live())
            .map(|i| merged.full_path(i).to_string_lossy().into_owned())
            .collect();
        let mut reference = std::collections::BTreeSet::new();
        ref_walk(root, &mut reference);
        reference.insert(root.to_string_lossy().into_owned());
        assert_eq!(paths, reference, "翻转形态物化对拍零差异");
    }

    /// 子树删除方向 A:子目录们各自 diff 过(独立桥),整树删除后**首次** diff
    /// 未 diff 过的父——父的 overlay 旧子项集为空,unlink 够不着深层桥
    /// (缺陷 2 回归钉;突变「去掉 base 子项消失方向的 purge 扫」此测必红)。
    #[test]
    fn first_parent_diff_sweeps_anchors_of_removed_diffed_subtree() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::create_dir_all(root.join("d1/d2")).unwrap();
        std::fs::write(root.join("d1/d2/leaf.txt"), "").unwrap();
        std::fs::write(root.join("keep.txt"), "").unwrap();
        let base = scan(root);
        let root_id = id_of(&base, root);

        // d1、d2 各自 diff(独立桥:parent 首 diff 尚未发生)
        let mut ov = Overlay::new();
        ov.apply_diff(
            &base,
            DiffTarget::Base(id_of(&base, &root.join("d1"))),
            &root.join("d1"),
            &[],
        )
        .unwrap();
        ov.apply_diff(
            &base,
            DiffTarget::Base(id_of(&base, &root.join("d1/d2"))),
            &root.join("d1/d2"),
            &[],
        )
        .unwrap();

        // 整树删除,再首 diff root
        std::fs::remove_dir_all(root.join("d1")).unwrap();
        ov.apply_diff(&base, DiffTarget::Base(root_id), root, &[])
            .unwrap();
        let view = ov.composite_view(&base);
        assert_eq!(total(&view, "whole:leaf"), 0, "深层文件不得泄漏命中");
        assert_eq!(total(&view, "whole:d2"), 0, "深层目录不得泄漏命中");
        assert_eq!(total(&view, "whole:d1"), 0, "被删子树根不得泄漏命中");
        assert_eq!(total(&view, "keep"), 1, "删除范围外不受误伤");
        // 物化不得因「anchor 的 base 目录未留存」断言 panic,产物对拍零差异
        let merged = materialize_composite(&base, &ov);
        let paths: std::collections::BTreeSet<String> = (0..merged.entries.len() as u32)
            .filter(|&i| merged.entries[i as usize].is_live())
            .map(|i| merged.full_path(i).to_string_lossy().into_owned())
            .collect();
        assert_eq!(paths, hit_paths(&view), "物化与合成视图零差异");
    }

    /// 子树删除方向 B:父先 diff(子桥挂父 anchor 下)、子树随后被 diff 过,
    /// 整树删除后再 diff 父——unlink 摘掉直接桥,但被 diff 过的深层目录桥是
    /// 独立账本键,残留即泄漏(缺陷 2 回归钉;突变「去掉删除方向 purge 扫」
    /// 经 anchors[d2] 残留使 base 深层子树复活,本测必红)。
    #[test]
    fn rediff_after_subtree_removal_sweeps_deep_anchors() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::create_dir_all(root.join("d1/d2")).unwrap();
        std::fs::write(root.join("d1/d2/leaf.txt"), "").unwrap();
        std::fs::write(root.join("keep.txt"), "").unwrap();
        let base = scan(root);
        let root_id = id_of(&base, root);

        let mut ov = Overlay::new();
        ov.apply_diff(&base, DiffTarget::Base(root_id), root, &[])
            .unwrap();
        ov.apply_diff(
            &base,
            DiffTarget::Base(id_of(&base, &root.join("d1"))),
            &root.join("d1"),
            &[],
        )
        .unwrap();
        ov.apply_diff(
            &base,
            DiffTarget::Base(id_of(&base, &root.join("d1/d2"))),
            &root.join("d1/d2"),
            &[],
        )
        .unwrap();

        std::fs::remove_dir_all(root.join("d1")).unwrap();
        ov.apply_diff(&base, DiffTarget::Base(root_id), root, &[])
            .unwrap();
        let view = ov.composite_view(&base);
        assert_eq!(total(&view, "whole:leaf"), 0);
        assert_eq!(total(&view, "whole:d2"), 0, "深层桥残留会让 base d2 复活");
        assert_eq!(total(&view, "whole:d1"), 0);
        assert_eq!(total(&view, "keep"), 1, "删除范围外不受误伤");
        let merged = materialize_composite(&base, &ov);
        let paths: std::collections::BTreeSet<String> = (0..merged.entries.len() as u32)
            .filter(|&i| merged.entries[i as usize].is_live())
            .map(|i| merged.full_path(i).to_string_lossy().into_owned())
            .collect();
        assert_eq!(paths, hit_paths(&view), "物化与合成视图零差异");
    }

    /// 桥覆盖孤儿(评审第四形态)回归钉:子目录先于父被 diff(桥是顶层占位),
    /// 父层首 diff 必须复用并 re-parent 该桥,而非新建覆盖。
    /// 突变「复用/reparent 去掉(改回新建覆盖)」:
    /// - re-diff 轮后 whole:f.txt 命中 2(同路径重复结果行,用户可见)→ 红;
    /// - 孤儿态 + 整树删除后已删路径仍命中(泄漏 1≠0)+ 物化幽灵根 → 红。
    #[test]
    fn child_first_bridge_is_reused_not_duplicated_on_parent_diff() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::create_dir_all(root.join("c")).unwrap();
        std::fs::write(root.join("c/f.txt"), "").unwrap();
        std::fs::write(root.join("keep.txt"), "").unwrap();
        let base = scan(root);
        let root_id = id_of(&base, root);
        let c = root.join("c");

        let mut ov = Overlay::new();
        // 子先:c 是 base 目录(dir_map 寻址),事件直中 c 时父可从未 diff
        // (P0 承认的事件丢失现实)——此时 c 的桥是顶层占位
        ov.apply_diff(&base, DiffTarget::Base(id_of(&base, &c)), &c, &[])
            .unwrap();
        // 父首:必须复用占位桥并挂正,不得新建覆盖
        ov.apply_diff(&base, DiffTarget::Base(root_id), root, &[])
            .unwrap();
        {
            let view = ov.composite_view(&base);
            assert_eq!(total(&view, "whole:f.txt"), 1, "父首 diff 后同路径恰一份");
        }
        // re-diff 轮次(评审序列 y→root→c→y→c):无界增长复制钉
        for _ in 0..2 {
            ov.apply_diff(&base, DiffTarget::Base(id_of(&base, &c)), &c, &[])
                .unwrap();
            ov.apply_diff(&base, DiffTarget::Base(root_id), root, &[])
                .unwrap();
        }
        {
            let view = ov.composite_view(&base);
            assert_eq!(total(&view, "whole:f.txt"), 1, "re-diff 不得复制条目");
            let mut reference = std::collections::BTreeSet::new();
            ref_walk(root, &mut reference);
            reference.insert(root.to_string_lossy().into_owned());
            assert_eq!(hit_paths(&view), reference, "对拍零差异");
        }

        // 整树删除后再 diff 父:已删路径清零、物化无幽灵根
        std::fs::remove_dir_all(&c).unwrap();
        ov.apply_diff(&base, DiffTarget::Base(root_id), root, &[])
            .unwrap();
        {
            let view = ov.composite_view(&base);
            assert_eq!(total(&view, "whole:f.txt"), 0, "已删路径不得泄漏命中");
            assert_eq!(total(&view, "keep"), 1, "删除范围外不受误伤");
            let merged = materialize_composite(&base, &ov);
            let paths: std::collections::BTreeSet<String> = (0..merged.entries.len() as u32)
                .filter(|&i| merged.entries[i as usize].is_live())
                .map(|i| merged.full_path(i).to_string_lossy().into_owned())
                .collect();
            assert_eq!(paths, hit_paths(&view), "物化与合成视图零差异(无幽灵根)");
        }
    }

    /// path: 查询的父号空间重映射(评审终评缺陷回归钉):Overlay 条目的 parent
    /// 是 overlay 空间号,dir_hits 按合成号对齐——突变「parent_idx 去掉重映射」时
    /// dir_hits 直下标命中无关槽位:overlay 新文件假阴性(path:w1=0)、无关目录
    /// 假阳性(path:w2 误中 w1 下文件),两断言皆红。
    #[test]
    fn path_query_remaps_overlay_parent_ids() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::create_dir_all(root.join("w1")).unwrap();
        std::fs::create_dir_all(root.join("w2")).unwrap();
        std::fs::write(root.join("w1/a.txt"), "").unwrap();
        std::fs::write(root.join("w2/b.txt"), "").unwrap();
        let base = scan(root);
        let root_id = id_of(&base, root);

        // root diff → w1/w2 桥建立(base 副本转屏蔽);w1 下新增文件只进 overlay
        let mut ov = Overlay::new();
        ov.apply_diff(&base, DiffTarget::Base(root_id), root, &[])
            .unwrap();
        std::fs::write(root.join("w1/new.txt"), "").unwrap();
        ov.apply_diff(
            &base,
            DiffTarget::Base(id_of(&base, &root.join("w1"))),
            &root.join("w1"),
            &[],
        )
        .unwrap();
        let view = ov.composite_view(&base);
        // 假阴性方向:overlay 子项的父必须重映射进合成空间,path:w1 命中 w1 的
        // 全部子项(overlay 存活副本 a.txt + overlay 新文件 new.txt)
        let w1_hits = |view: &CompositeView| {
            let out = query::execute(view, &query::parse("path:w1").unwrap(), 10);
            let set: std::collections::BTreeSet<String> = out
                .hits
                .iter()
                .map(|&h| view.full_path_composite(h))
                .collect();
            assert_eq!(set.len(), out.total as usize, "命中路径不得折叠");
            set
        };
        assert_eq!(
            w1_hits(&view),
            set(&[
                &root.join("w1/a.txt").to_string_lossy(),
                &root.join("w1/new.txt").to_string_lossy()
            ]),
            "path:w1 必须命中 overlay 新文件(父号重映射)"
        );
        // 假阳性方向:path:w2 不得命中 w1 下文件(错空间号会读到无关槽位)
        assert_eq!(total(&view, "path:w2"), 1, "path:w2 只命中 b.txt");
        assert_eq!(total(&view, "whole:new.txt"), 1, "name 查询对照(恒正常)");
    }

    /// dir→file 类型翻转泄漏(评审 C1)变体 A:父从未 diff,d1/d2 先各自 diff,
    /// 整树删除后原地降级为同名文件,父层首 diff——删除方向 sweep 必须覆盖
    /// 「listing 有名但降级为文件」(父层 old 为空,翻转分支根本不进)。
    /// 突变「sweep 条件去掉降级判定」→ 陈旧 base 子树复活:d1 与新文件双份
    /// (whole:d1=2)、深层 d2 泄漏(whole:d2=1),本测断言红。
    #[test]
    fn dir_to_file_flip_first_parent_diff_sweeps_demoted_subtree() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::create_dir_all(root.join("d1/d2")).unwrap();
        std::fs::write(root.join("d1/d2/leaf.txt"), "").unwrap();
        std::fs::write(root.join("keep.txt"), "").unwrap();
        let base = scan(root);
        let root_id = id_of(&base, root);
        let d1 = root.join("d1");

        let mut ov = Overlay::new();
        ov.apply_diff(&base, DiffTarget::Base(id_of(&base, &d1)), &d1, &[])
            .unwrap();
        ov.apply_diff(
            &base,
            DiffTarget::Base(id_of(&base, &d1.join("d2"))),
            &d1.join("d2"),
            &[],
        )
        .unwrap();

        // rm -rf d1 && touch d1:目录降级为同名文件
        std::fs::remove_dir_all(&d1).unwrap();
        std::fs::write(&d1, "").unwrap();
        ov.apply_diff(&base, DiffTarget::Base(root_id), root, &[])
            .unwrap();
        let view = ov.composite_view(&base);
        assert_eq!(total(&view, "whole:leaf"), 0, "已删深层文件不得泄漏");
        assert_eq!(total(&view, "whole:d2"), 0, "已删深层目录不得泄漏");
        assert_eq!(
            total(&view, "whole:d1"),
            1,
            "同名文件恰一份(不得与 base 陈旧目录条目双份)"
        );
        assert_eq!(total(&view, "keep"), 1, "删除范围外不受误伤");
        let merged = materialize_composite(&base, &ov);
        let paths: std::collections::BTreeSet<String> = (0..merged.entries.len() as u32)
            .filter(|&i| merged.entries[i as usize].is_live())
            .map(|i| merged.full_path(i).to_string_lossy().into_owned())
            .collect();
        assert_eq!(paths, hit_paths(&view), "物化与合成视图零差异(无幽灵根)");
    }

    /// dir→file 类型翻转泄漏(评审 C1)变体 B:父先 diff(翻转分支生效,unlink
    /// 清直接桥并级联墓碑深层 overlay 条目),但深层 anchors 键残留会让
    /// contains_key 把 base 陈旧 d2 判有效。sweep 条件修复后本测由 purge 扫
    /// 兜住;entry_valid 的 is_live 加固为纵深防御(残键漏网退化为保守屏蔽)。
    #[test]
    fn dir_to_file_flip_rediff_sweeps_deep_anchor_keys() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::create_dir_all(root.join("d1/d2")).unwrap();
        std::fs::write(root.join("d1/d2/leaf.txt"), "").unwrap();
        std::fs::write(root.join("keep.txt"), "").unwrap();
        let base = scan(root);
        let root_id = id_of(&base, root);
        let d1 = root.join("d1");

        let mut ov = Overlay::new();
        ov.apply_diff(&base, DiffTarget::Base(root_id), root, &[])
            .unwrap();
        ov.apply_diff(&base, DiffTarget::Base(id_of(&base, &d1)), &d1, &[])
            .unwrap();
        ov.apply_diff(
            &base,
            DiffTarget::Base(id_of(&base, &d1.join("d2"))),
            &d1.join("d2"),
            &[],
        )
        .unwrap();

        // rm -rf d1 && touch d1;父层再 diff(翻转分支:unlink o1 + 级联)
        std::fs::remove_dir_all(&d1).unwrap();
        std::fs::write(&d1, "").unwrap();
        ov.apply_diff(&base, DiffTarget::Base(root_id), root, &[])
            .unwrap();
        let view = ov.composite_view(&base);
        assert_eq!(total(&view, "whole:leaf"), 0);
        assert_eq!(
            total(&view, "whole:d2"),
            0,
            "深层 anchors 残键不得让 base 陈旧 d2 复活"
        );
        assert_eq!(total(&view, "whole:d1"), 1, "同名文件恰一份");
        assert_eq!(total(&view, "keep"), 1, "删除范围外不受误伤");
        let merged = materialize_composite(&base, &ov);
        let paths: std::collections::BTreeSet<String> = (0..merged.entries.len() as u32)
            .filter(|&i| merged.entries[i as usize].is_live())
            .map(|i| merged.full_path(i).to_string_lossy().into_owned())
            .collect();
        assert_eq!(paths, hit_paths(&view), "物化与合成视图零差异(无幽灵根)");
    }

    /// purge_prefix:摘除子树波及的 Overlay 账本同步作废
    /// (mutation:remove_prefix 不调 purge → 陈旧 overlay 条目复活,必红)。
    #[test]
    fn purge_prefix_clears_accounts() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::create_dir_all(root.join("zone/inner")).unwrap();
        std::fs::write(root.join("zone/z.txt"), "").unwrap();
        std::fs::write(root.join("zone/inner/i.txt"), "").unwrap();
        std::fs::write(root.join("keep.txt"), "").unwrap();
        let base = scan(root);
        let root_id = id_of(&base, root);
        let zone_id = id_of(&base, &root.join("zone"));

        let mut ov = Overlay::new();
        std::fs::write(root.join("zone/newz.txt"), "").unwrap();
        ov.apply_diff(&base, DiffTarget::Base(root_id), root, &[])
            .unwrap();
        std::fs::write(root.join("zone/inner/newi.txt"), "").unwrap();
        ov.apply_diff(&base, DiffTarget::Base(zone_id), &root.join("zone"), &[])
            .unwrap();
        {
            let view = ov.composite_view(&base);
            assert_eq!(total(&view, "newz"), 1);
        }

        // zone 子树整体摘除(卷卸载/ENOENT 形态):overlay 账本随 purge 作废
        ov.purge_prefix(&base, &root.join("zone"));
        let view = ov.composite_view(&base);
        assert_eq!(total(&view, "newz"), 0, "摘除子树的 overlay 条目不得复活");
        assert_eq!(total(&view, "newi"), 0, "深层 overlay 增量级联作废");
        assert_eq!(total(&view, "keep"), 1, "摘除范围外不受误伤");
    }

    /// 合成视图物化:Base + Overlay 合并为完整树,路径集与独立枚举零差异,
    /// 且落盘→mmap 载入后可查(T4 合并写的产物形态预演)。
    #[test]
    fn materialize_composite_roundtrip() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::create_dir_all(root.join("d")).unwrap();
        std::fs::write(root.join("d/old.txt"), "").unwrap();
        let base = scan(root);
        let root_id = id_of(&base, root);
        std::fs::remove_file(root.join("d/old.txt")).unwrap();
        std::fs::write(root.join("d/new.txt"), "").unwrap();
        let mut ov = Overlay::new();
        ov.apply_diff(&base, DiffTarget::Base(root_id), root, &[])
            .unwrap();
        // d 自身也 diff:old.txt 屏蔽、new.txt 入 overlay
        ov.apply_diff(
            &base,
            DiffTarget::Base(id_of(&base, &root.join("d"))),
            &root.join("d"),
            &[],
        )
        .unwrap();
        let view = ov.composite_view(&base);

        let merged = materialize_composite(&base, &ov);
        let merged_paths: std::collections::BTreeSet<String> = (0..merged.entries.len() as u32)
            .filter(|&i| merged.entries[i as usize].is_live())
            .map(|i| merged.full_path(i).to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            merged_paths,
            hit_paths(&view),
            "物化产物与合成视图路径集零差异"
        );

        // 落盘 → mmap → 查询:合成树对既有持久化链路透明
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("merged.bin");
        crate::persist::write_atomic(
            &crate::persist::EntryLayout::of(&merged),
            merged.entries.iter(),
            &crate::persist::IndexMeta::default(),
            &path,
        )
        .unwrap();
        let shared = crate::persist::load_mmap(&path).unwrap();
        let out = query::execute(shared.as_ref(), &query::parse("whole:new.txt").unwrap(), 10);
        assert_eq!(out.total, 1);
        let out_old = query::execute(shared.as_ref(), &query::parse("whole:old.txt").unwrap(), 10);
        assert_eq!(out_old.total, 0, "已删 old.txt 随合并物化作废");
    }

    /// diff 目标为纯 overlay 目录(冷启动后新建):寻址、嵌套与屏蔽全链路。
    #[test]
    fn pure_overlay_dir_diffs() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::write(root.join("a.txt"), "").unwrap();
        let base = scan(root);
        let root_id = id_of(&base, root);

        // 冷启动后新建目录树 /brand/new
        std::fs::create_dir_all(root.join("brand/new")).unwrap();
        std::fs::write(root.join("brand/new/f.txt"), "").unwrap();
        let mut ov = Overlay::new();
        // 逐级 diff:root → brand(新,纯 overlay)→ new(纯 overlay)
        ov.apply_diff(&base, DiffTarget::Base(root_id), root, &[])
            .unwrap();
        let brand = ov
            .overlay_dir_id(&root.join("brand"))
            .expect("brand 应入寻址面");
        ov.apply_diff(
            &base,
            DiffTarget::OverlayDir(brand),
            &root.join("brand"),
            &[],
        )
        .unwrap();
        let newd = ov
            .overlay_dir_id(&root.join("brand/new"))
            .expect("new 应入寻址面");
        ov.apply_diff(
            &base,
            DiffTarget::OverlayDir(newd),
            &root.join("brand/new"),
            &[],
        )
        .unwrap();

        let view = ov.composite_view(&base);
        let mut reference = std::collections::BTreeSet::new();
        ref_walk(root, &mut reference);
        reference.insert(root.to_string_lossy().into_owned());
        assert_eq!(hit_paths(&view), reference, "纯 overlay 子树对拍零差异");
        let out = query::execute(&view, &query::parse("f.txt").unwrap(), 10);
        assert_eq!(
            view.full_path_composite(out.hits[0]),
            root.join("brand/new/f.txt").to_string_lossy()
        );
        // 回归钉(实机冒烟暴露):trait 侧 full_path_dyn 是 daemon 结果行/stat 的
        // 实际消费点,必须与 full_path_composite 同一入口——默认实现按合成空间
        // 直读父号,会在桥接 anchor 处跳进 base 无关条目(突变「去掉 full_path_dyn
        // 改道」时此断言必红)。
        assert_eq!(
            IndexView::full_path_dyn(&view, out.hits[0]),
            root.join("brand/new/f.txt").to_string_lossy()
        );
    }
}
