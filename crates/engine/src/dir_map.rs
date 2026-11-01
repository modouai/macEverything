//! dir_map:「绝对目录路径 → 条目号」映射的紧凑存储(memory-footprint §4.4)。
//!
//! 三代形态演进:
//! 1. `HashMap<PathBuf, u32>`:每键一份槽内 PathBuf + 独立路径堆分配,20 万目录
//!    实测 ~40MB。
//! 2. `HashMap<u64, Bucket>` + `Vec<(PathBuf, u32)>`:路径仍逐键堆分配,
//!    HashMap 值 `Bucket` 含 Vec 分支(24B+tag),20 万键仍占 ~40MB。
//! 3. 当前:开放寻址表 + 路径字节 arena + 定长槽(12B/目录)。路径字节集中在一
//!    个 Vec,槽只存 (start, len, value);表项 `u32` 指向槽。20 万目录约
//!    「17MB arena + 2.4MB 槽 + 1MB 表」,并把 15 万+ 次小对象分配到 3 个大块。
//!
//! 键的**字节语义不变**(根 canonical 路径 + 逐段 display 名拼接;`/tmp` 类符号
//! 链接由 daemon 侧 canonicalize 对齐)。哈希 FNV-1a 64 手写,不引新依赖;碰撞
//! 以线性探测 + arena 字节全比对消解(单测以稠密插入钉死)。
//!
//! 摘除只清槽位并记垃圾字节,路径 arena 在垃圾过半时整体压实;daemon 在
//! compact/重扫后 `rebuild_dir_map` 整体重建,arena 随之回收。
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// 空表项。
const EMPTY: u32 = 0;
/// 摘除墓碑(探测链上继续前进,插入可复用)。
const TOMBSTONE: u32 = u32::MAX;
/// 垃圾字节超过此值且过半才整体压实(避免频繁重建)。
const COMPACT_MIN_GARBAGE: usize = 4 * 1024 * 1024;

/// 池槽(12B):路径在 arena 中的区间 + 目录条目号。
#[derive(Clone, Copy)]
struct Slot {
    start: u32,
    len: u32,
    value: u32,
}

/// daemon 侧「绝对目录路径 → 条目号」映射(内存形态见模块注释)。
pub struct DirIndexMap {
    /// 开放寻址表:`0` 空、`u32::MAX` 墓碑、其余 = 槽号+1。
    table: Vec<u32>,
    /// 定长槽;`len == 0` 表示已摘除(空路径不是合法键)。
    slots: Vec<Slot>,
    /// 路径字节 arena(保真原始字节)。
    bytes: Vec<u8>,
    /// 摘除后待复用的槽。
    free: Vec<u32>,
    /// 活键数。
    len: usize,
    /// 墓碑数(触发扩容/重哈希)。
    tombstones: usize,
    /// arena 中已无槽引用的字节数。
    garbage: usize,
}

impl Default for DirIndexMap {
    fn default() -> Self {
        Self::with_capacity(0, 0)
    }
}

impl DirIndexMap {
    /// 按预期目录数与路径总字节预留容量(顺序建图时用,避免逐级倍增)。
    pub fn with_capacity(dirs: usize, path_bytes: usize) -> Self {
        let mut cap = 16usize;
        while cap < (dirs + 1) * 2 {
            cap <<= 1;
        }
        Self {
            table: vec![EMPTY; cap],
            slots: Vec::with_capacity(dirs),
            bytes: Vec::with_capacity(path_bytes),
            free: Vec::new(),
            len: 0,
            tombstones: 0,
            garbage: 0,
        }
    }

    /// 查键:hash 定起点,线性探测至空位,槽内 arena 字节全比对。
    pub fn get(&self, key: &Path) -> Option<u32> {
        let bytes = key.as_os_str().as_encoded_bytes();
        self.find(fnv1a64(bytes), bytes)
            .map(|(_, s)| self.slots[s as usize].value)
    }

    pub fn contains_key(&self, key: &Path) -> bool {
        self.get(key).is_some()
    }

    /// 插入/更新:同键覆盖值(槽与路径字节不动);新键入池。
    pub fn insert(&mut self, key: &Path, idx: u32) {
        let _ = self.insert_get_slot(key, idx);
    }

    /// 同 [`insert`](Self::insert),额外返回槽号(顺序建图时子目录直接引用
    /// 父路径,免二次查表与临时 PathBuf)。
    pub fn insert_get_slot(&mut self, key: &Path, idx: u32) -> u32 {
        let bytes = key.as_os_str().as_encoded_bytes();
        // 空键会与死槽哨兵(len == 0)混淆——键必为非空绝对路径。
        debug_assert!(!bytes.is_empty(), "dir_map 键不得为空路径");
        let h = fnv1a64(bytes);
        if let Some((_, s)) = self.find(h, bytes) {
            self.slots[s as usize].value = idx;
            return s;
        }
        self.ensure_insert_capacity();
        let start = self.bytes.len() as u32;
        self.bytes.extend_from_slice(bytes);
        let slot = match self.free.pop() {
            Some(s) => {
                self.slots[s as usize] = Slot {
                    start,
                    len: bytes.len() as u32,
                    value: idx,
                };
                s
            }
            None => {
                self.slots.push(Slot {
                    start,
                    len: bytes.len() as u32,
                    value: idx,
                });
                (self.slots.len() - 1) as u32
            }
        };
        let i = self.probe_insert_slot(h);
        self.table[i] = slot + 1;
        self.len += 1;
        slot
    }

    /// 池槽对应的键路径(顺序建图拼接父路径用);槽必须为活槽。
    pub fn path_at(&self, slot: u32) -> &Path {
        let s = self.slots[slot as usize];
        Path::new(OsStr::from_bytes(
            &self.bytes[s.start as usize..(s.start + s.len) as usize],
        ))
    }

    /// 摘键,返回旧值。幂等:键不在返回 None。
    pub fn remove(&mut self, key: &Path) -> Option<u32> {
        let bytes = key.as_os_str().as_encoded_bytes();
        let (ti, s) = self.find(fnv1a64(bytes), bytes)?;
        let v = self.slots[s as usize].value;
        self.evict(ti, s);
        Some(v)
    }

    /// 按值谓词保留:键保留当且仅当 `f(值)` 为真。
    /// 摘除路径(scanner::remove_prefix)的清键方法——低频调用。
    pub fn retain(&mut self, mut f: impl FnMut(u32) -> bool) {
        let mut removed = 0usize;
        for s in 0..self.slots.len() {
            let sl = self.slots[s];
            if sl.len != 0 && !f(sl.value) {
                self.slots[s].len = 0;
                self.garbage += sl.len as usize;
                self.free.push(s as u32);
                self.len -= 1;
                removed += 1;
            }
        }
        if removed > 0 {
            self.tombstones = 0;
            self.rebuild_table();
            self.maybe_compact();
        }
    }

    /// 摘除全部「路径以 `prefix` 为前缀」的键(含 prefix 自身),返回摘除数。
    pub fn remove_prefix(&mut self, prefix: &Path) -> u32 {
        let mut victims: Vec<u32> = Vec::new();
        for s in 0..self.slots.len() {
            let sl = self.slots[s];
            if sl.len == 0 {
                continue;
            }
            let p = Path::new(OsStr::from_bytes(
                &self.bytes[sl.start as usize..(sl.start + sl.len) as usize],
            ));
            if p.starts_with(prefix) {
                victims.push(s as u32);
            }
        }
        let n = victims.len() as u32;
        for s in victims {
            let sl = self.slots[s as usize];
            self.slots[s as usize].len = 0;
            self.garbage += sl.len as usize;
            self.free.push(s);
            self.len -= 1;
        }
        if n > 0 {
            self.tombstones = 0;
            self.rebuild_table();
            self.maybe_compact();
        }
        n
    }

    /// 按函数重映射全部值;返回 None 的键整键摘除。
    /// 合并 flush 换基的唯一用法:值 = 旧 Base 条目号,`f` = 旧号→新号表,
    /// 未留存(NO_PARENT)即摘键。键字节不变,不触发重哈希,仅重建探测表。
    pub fn remap(&mut self, mut f: impl FnMut(u32) -> Option<u32>) {
        let mut removed = 0usize;
        for s in 0..self.slots.len() {
            let sl = self.slots[s];
            if sl.len == 0 {
                continue;
            }
            match f(sl.value) {
                Some(nv) => self.slots[s].value = nv,
                None => {
                    self.slots[s].len = 0;
                    self.garbage += sl.len as usize;
                    self.free.push(s as u32);
                    self.len -= 1;
                    removed += 1;
                }
            }
        }
        if removed > 0 {
            self.tombstones = 0;
            self.rebuild_table();
            self.maybe_compact();
        }
    }

    /// 全部活键(不保证键序;调用方仅做抽样/过滤,不依赖序)。
    pub fn keys(&self) -> impl Iterator<Item = &Path> {
        self.slots.iter().filter(|s| s.len != 0).map(move |s| {
            Path::new(OsStr::from_bytes(
                &self.bytes[s.start as usize..(s.start + s.len) as usize],
            ))
        })
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 探测定位 (表项下标, 槽号)。槽内 arena 字节全比对消解碰撞。
    fn find(&self, h: u64, bytes: &[u8]) -> Option<(usize, u32)> {
        let mask = self.table.len() - 1;
        let mut i = (h as usize) & mask;
        loop {
            let e = self.table[i];
            if e == EMPTY {
                return None;
            }
            if e != TOMBSTONE {
                let s = e - 1;
                let sl = self.slots[s as usize];
                if &self.bytes[sl.start as usize..(sl.start + sl.len) as usize] == bytes {
                    return Some((i, s));
                }
            }
            i = (i + 1) & mask;
        }
    }

    /// 插入前的容量检查:活键+墓碑超 3/4 载入即原地重哈希(表容量翻倍)。
    fn ensure_insert_capacity(&mut self) {
        if (self.len + self.tombstones + 1) * 4 < self.table.len() * 3 {
            return;
        }
        let old = std::mem::take(&mut self.table);
        self.table = vec![EMPTY; old.len() * 2];
        self.tombstones = 0;
        for s in 0..self.slots.len() {
            let sl = self.slots[s];
            if sl.len == 0 {
                continue;
            }
            let h = fnv1a64(&self.bytes[sl.start as usize..(sl.start + sl.len) as usize]);
            let i = self.probe_insert_slot(h);
            self.table[i] = s as u32 + 1;
        }
    }

    /// 从 `h` 起找到可写表项(优先首个墓碑,否则首个空位)。调用前保证存在空位。
    fn probe_insert_slot(&self, h: u64) -> usize {
        let mask = self.table.len() - 1;
        let mut i = (h as usize) & mask;
        let mut tomb = None;
        loop {
            let e = self.table[i];
            if e == EMPTY {
                return tomb.unwrap_or(i);
            }
            if e == TOMBSTONE && tomb.is_none() {
                tomb = Some(i);
            }
            i = (i + 1) & mask;
        }
    }

    /// 摘除一个槽:表项改墓碑、arena 字节计入垃圾、槽入 freelist。
    fn evict(&mut self, table_idx: usize, slot: u32) {
        self.table[table_idx] = TOMBSTONE;
        self.tombstones += 1;
        let len = self.slots[slot as usize].len as usize;
        self.slots[slot as usize].len = 0;
        self.garbage += len;
        self.free.push(slot);
        self.len -= 1;
        self.maybe_compact();
    }

    /// 清空表并按活槽重建探测链(不分配;remap/retain 摘除后收口)。
    fn rebuild_table(&mut self) {
        self.table.fill(EMPTY);
        for s in 0..self.slots.len() {
            let sl = self.slots[s];
            if sl.len == 0 {
                continue;
            }
            let h = fnv1a64(&self.bytes[sl.start as usize..(sl.start + sl.len) as usize]);
            let i = self.probe_insert_slot(h);
            self.table[i] = s as u32 + 1;
        }
    }

    /// 垃圾字节过半且够大时整体压实(重建 slots/arena;低频删除路径才触发)。
    fn maybe_compact(&mut self) {
        if self.garbage < COMPACT_MIN_GARBAGE || self.garbage * 2 <= self.bytes.len() {
            return;
        }
        let mut new = DirIndexMap::with_capacity(self.len, self.bytes.len() - self.garbage);
        for s in 0..self.slots.len() {
            let sl = self.slots[s];
            if sl.len == 0 {
                continue;
            }
            let bytes = &self.bytes[sl.start as usize..(sl.start + sl.len) as usize];
            let h = fnv1a64(bytes);
            let start = new.bytes.len() as u32;
            new.bytes.extend_from_slice(bytes);
            let ns = new.slots.len() as u32;
            new.slots.push(Slot {
                start,
                len: sl.len,
                value: sl.value,
            });
            let i = new.probe_insert_slot(h);
            new.table[i] = ns + 1;
            new.len += 1;
        }
        *self = new;
    }
}

/// FNV-1a 64(手写,不引新依赖):键路径原始字节 → 表起点。
/// 已知向量(FNV 官方):空串 = offset basis;`a` = 0xaf63dc4c8601ec8c。
fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(PRIME);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FNV-1a 64 已知向量钉:空串 = offset basis,"a" = 0xaf63dc4c8601ec8c。
    /// 突变(初值/素数/异或与乘次序颠倒)必红。
    #[test]
    fn fnv1a64_known_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x85944171f73967e8);
    }

    /// 与旧 `HashMap<PathBuf, u32>` 的语义等价钉:插入可查、同键覆盖、remove 返回
    /// 旧值且键消失、重复摘除幂等、兄弟键不受误伤。
    #[test]
    fn insert_get_replace_remove_roundtrip() {
        let mut m = DirIndexMap::default();
        let a = Path::new("/tmp/x/a");
        let b = Path::new("/tmp/x/b");
        assert_eq!(m.get(a), None);
        assert!(m.is_empty());
        m.insert(a, 1);
        m.insert(b, 2);
        assert_eq!(m.get(a), Some(1));
        assert_eq!(m.get(b), Some(2));
        assert!(m.contains_key(b));
        m.insert(a, 9);
        assert_eq!(m.get(a), Some(9), "同键覆盖旧值");
        assert_eq!(m.keys().count(), 2, "覆盖不新增键");
        assert_eq!(m.remove(a), Some(9));
        assert_eq!(m.get(a), None);
        assert_eq!(m.remove(a), None, "重复摘除幂等");
        assert_eq!(m.get(b), Some(2), "兄弟键不受误伤");
        assert_eq!(m.remove(b), Some(2));
        assert!(m.is_empty());
    }

    /// 稠密插入钉:千级键必然产生探测链碰撞(载入 3/4 内线性探测),每个键
    /// 都必须按 arena 字节命中自身值、不被邻槽串扰。突变「探测只取首个非空
    /// 槽不比字节」/「墓碑处停止探测」本测红。
    #[test]
    fn dense_insert_resolves_collisions_without_crosstalk() {
        let mut m = DirIndexMap::default();
        for i in 0..2000u32 {
            m.insert(Path::new(&format!("/tmp/dir-{i:04}")), i);
        }
        for i in 0..2000u32 {
            assert_eq!(
                m.get(Path::new(&format!("/tmp/dir-{i:04}"))),
                Some(i),
                "键 {i} 必须命中自身值"
            );
        }
        assert_eq!(m.keys().count(), 2000);
        // 摘除中间一段后其余键仍可命中(墓碑不截断探测链)
        for i in 1000..1500u32 {
            assert_eq!(m.remove(Path::new(&format!("/tmp/dir-{i:04}"))), Some(i));
        }
        for i in (0..1000).chain(1500..2000u32) {
            assert_eq!(m.get(Path::new(&format!("/tmp/dir-{i:04}"))), Some(i));
        }
        for i in 1000..1500u32 {
            assert_eq!(m.get(Path::new(&format!("/tmp/dir-{i:04}"))), None);
        }
    }

    /// 槽复用钉:摘除腾出的槽被新键复用,槽数组不随增删单调增长;复用后旧键
    /// 消失(槽字节已覆盖,不复活)、新键可查。
    #[test]
    fn removed_slot_is_reused_and_old_key_vanishes() {
        let mut m = DirIndexMap::default();
        let a = Path::new("/tmp/x/a");
        m.insert(a, 1);
        assert_eq!(m.remove(a), Some(1));
        let c = Path::new("/tmp/x/c");
        m.insert(c, 3);
        assert_eq!(m.slots.len(), 1, "复用摘除槽,槽数组不长高");
        assert_eq!(m.get(a), None, "旧键不复活(槽已改写)");
        assert_eq!(m.get(c), Some(3));
        assert_eq!(m.keys().collect::<Vec<_>>(), vec![c]);
    }

    /// remap 钉:值经映射函数改写,返回 None 的键整键摘除且槽可复用;
    /// 键字节与命中关系不变。
    #[test]
    fn remap_rewrites_values_and_drops_unmapped_keys() {
        let mut m = DirIndexMap::default();
        let a = Path::new("/tmp/x/a");
        let b = Path::new("/tmp/x/b");
        let c = Path::new("/tmp/x/c");
        m.insert(a, 1);
        m.insert(b, 2);
        m.insert(c, 3);
        m.remap(|v| match v {
            2 => None,
            v => Some(v * 10),
        });
        assert_eq!(m.get(a), Some(10));
        assert_eq!(m.get(b), None, "未映射键必须摘除");
        assert_eq!(m.get(c), Some(30));
        assert_eq!(m.keys().count(), 2);
        m.insert(Path::new("/tmp/x/d"), 4);
        assert_eq!(m.slots.len(), 3, "remap 摘除的槽可复用");
        assert_eq!(m.get(Path::new("/tmp/x/d")), Some(4));
    }

    /// retain 钉:按值谓词清扫——不满足谓词的键整键消失,满足的连同邻槽
    /// 兄弟一起幸存;清扫腾出的槽可复用。
    #[test]
    fn retain_keeps_only_predicated_values() {
        let mut m = DirIndexMap::default();
        let a = Path::new("/tmp/x/a");
        let b = Path::new("/tmp/x/b");
        let c = Path::new("/tmp/x/c");
        m.insert(a, 1);
        m.insert(b, 2);
        m.insert(c, 3);
        m.retain(|v| v != 2);
        assert_eq!(m.get(b), None, "谓词外键清除");
        assert_eq!(m.get(a), Some(1));
        assert_eq!(m.get(c), Some(3));
        assert_eq!(m.keys().count(), 2);
        assert_eq!(m.slots.len(), 3, "retain 不缩槽数组,槽回 freelist");
        m.insert(Path::new("/tmp/x/d"), 4);
        assert_eq!(m.slots.len(), 3, "新键复用被清槽");
        assert_eq!(m.get(Path::new("/tmp/x/d")), Some(4));
    }

    /// remove_prefix 钉:仅前缀组件匹配的键被摘除(同名前缀不误伤),
    /// 其余键命中不变。
    #[test]
    fn remove_prefix_matches_path_components_only() {
        let mut m = DirIndexMap::default();
        m.insert(Path::new("/tmp/x/a"), 1);
        m.insert(Path::new("/tmp/x/ab"), 2);
        m.insert(Path::new("/tmp/y"), 3);
        assert_eq!(m.remove_prefix(Path::new("/tmp/x")), 2);
        assert_eq!(m.get(Path::new("/tmp/x/a")), None);
        assert_eq!(m.get(Path::new("/tmp/x/ab")), None);
        assert_eq!(m.get(Path::new("/tmp/y")), Some(3));
    }
}
