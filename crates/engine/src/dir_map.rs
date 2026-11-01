//! dir_map:「绝对目录路径 → 条目号」映射的 intern 化存储(memory-footprint §4.4)。
//!
//! 旧实现 `HashMap<PathBuf, u32>`:每键占一份 hashbrown 槽内 PathBuf(24B)+ 一块
//! 独立路径堆分配,20 万目录实测 ~40MB。改为两级结构:
//!
//! - `buckets: HashMap<u64, Bucket>`:键 = 路径原始字节的 FNV-1a 64(手写,不引
//!   新依赖),值 = 池槽桶。桶 SmallVec 风格:单元素内联(`One`)零堆分配,碰撞时
//!   才升级堆向量(`Many`)。
//! - `pool: Vec<(PathBuf, u32)>`:键路径字节池(保真原始字节)+ 对位值(目录条目
//!   号)。键的**字节语义与旧 map 完全一致**(历史形态:根 canonical 路径 + 逐段
//!   display 名拼接;`/tmp` 类符号链接由 daemon 侧 canonicalize 对齐),仅存储
//!   形态从散装 PathBuf 改为集中池化。
//!
//! 哈希碰撞在 64 位 FNV-1a 下极罕见(20 万键 ~10⁻⁹)但必须正确:桶内以池中字节
//! 全比对消解,同桶兄弟互不串扰(单测钉)。摘除腾出的池槽经 `free` 复用,键反复
//! 增删不使池单调增长;daemon 在 compact/重扫后 `rebuild_dir_map` 整体重建,池随
//! 之回收。
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// daemon 侧「绝对目录路径 → 条目号」映射(intern 化,内存形态见模块注释)。
#[derive(Default)]
pub struct DirIndexMap {
    /// FNV-1a 64(键路径原始字节)→ 池槽桶(常态单元素)。
    buckets: HashMap<u64, Bucket>,
    /// 池槽 = (键路径原始字节, 对位值 = 目录条目号)。死槽为空路径哨兵:键必为
    /// 非空绝对路径,空路径即空闲,`keys` 据此跳过(摘除即清字节,不滞留)。
    pool: Vec<(PathBuf, u32)>,
    /// 摘除后待复用的池槽。
    free: Vec<u32>,
}

/// 哈希桶:SmallVec 风格——单元素内联零堆分配,碰撞才升级堆向量。
enum Bucket {
    One(u32),
    Many(Vec<u32>),
}

impl Bucket {
    fn slots(&self) -> &[u32] {
        match self {
            Bucket::One(s) => std::slice::from_ref(s),
            Bucket::Many(v) => v,
        }
    }

    fn push(&mut self, s: u32) {
        match self {
            Bucket::One(prev) => *self = Bucket::Many(vec![*prev, s]),
            Bucket::Many(v) => v.push(s),
        }
    }
}

impl DirIndexMap {
    /// 查键:hash 定桶,桶内以池中字节全比对消解碰撞。
    pub fn get(&self, key: &Path) -> Option<u32> {
        let bytes = key.as_os_str().as_encoded_bytes();
        let h = fnv1a64(bytes);
        self.slot_of(h, bytes).map(|s| self.pool[s as usize].1)
    }

    pub fn contains_key(&self, key: &Path) -> bool {
        self.get(key).is_some()
    }

    /// 插入/更新:同键覆盖值(池槽与键字节不动);新键入池。
    pub fn insert(&mut self, key: &Path, idx: u32) {
        let bytes = key.as_os_str().as_encoded_bytes();
        // 空键会与死槽哨兵(空路径)混淆,keys() 会漏掉它——键必为非空绝对路径。
        debug_assert!(!bytes.is_empty(), "dir_map 键不得为空路径");
        let h = fnv1a64(bytes);
        // 先查既键(共享借用),后动桶与池,避免可变借用交叠。
        if let Some(s) = self.slot_of(h, bytes) {
            self.pool[s as usize].1 = idx;
            return;
        }
        let s = self.alloc_slot(key, idx);
        match self.buckets.get_mut(&h) {
            Some(bucket) => bucket.push(s),
            None => {
                self.buckets.insert(h, Bucket::One(s));
            }
        }
    }

    /// 摘键,返回旧值。幂等:键不在返回 None。
    pub fn remove(&mut self, key: &Path) -> Option<u32> {
        let bytes = key.as_os_str().as_encoded_bytes();
        let h = fnv1a64(bytes);
        let s = self.slot_of(h, bytes)?;
        self.bucket_evict(h, s);
        let v = self.pool[s as usize].1;
        self.pool[s as usize] = (PathBuf::new(), 0); // 清字节释放路径堆块;空路径即死槽
        self.free.push(s);
        Some(v)
    }

    /// 按值谓词保留:键保留当且仅当 `f(值)` 为真。摘除路径(scanner::remove_prefix)
    /// 的清键方法——受害者条目墓碑后,连同历史滞留的「条目已死、键仍在」陈旧键
    /// 一并清扫。O(池长),仅低频摘除路径调用。
    pub fn retain(&mut self, mut f: impl FnMut(u32) -> bool) {
        // 先按原字节重算 hash 从桶中摘槽(清字节后就无从定位桶了),再清池槽。
        let mut dead: Vec<(u64, u32)> = Vec::new();
        for (s, (k, v)) in self.pool.iter().enumerate() {
            if !k.as_os_str().is_empty() && !f(*v) {
                dead.push((fnv1a64(k.as_os_str().as_encoded_bytes()), s as u32));
            }
        }
        for (h, s) in dead {
            self.bucket_evict(h, s);
            self.pool[s as usize] = (PathBuf::new(), 0);
            self.free.push(s);
        }
    }

    /// 全部活键(池序,不保证键序;调用方仅做抽样/过滤,不依赖序)。
    pub fn keys(&self) -> impl Iterator<Item = &Path> {
        self.pool
            .iter()
            .map(|(k, _)| k.as_path())
            .filter(|k| !k.as_os_str().is_empty())
    }

    pub fn is_empty(&self) -> bool {
        self.pool.len() == self.free.len()
    }

    /// 桶内字节比对定位槽(碰撞消解唯一入口:同桶兄弟以池中原始字节区分)。
    fn slot_of(&self, h: u64, bytes: &[u8]) -> Option<u32> {
        self.buckets
            .get(&h)?
            .slots()
            .iter()
            .copied()
            .find(|&s| self.bytes_at(s) == bytes)
    }

    fn bytes_at(&self, slot: u32) -> &[u8] {
        self.pool[slot as usize].0.as_os_str().as_encoded_bytes()
    }

    /// 从桶中摘槽;桶空则整桶删除。
    fn bucket_evict(&mut self, h: u64, s: u32) {
        let empty = match self.buckets.get_mut(&h) {
            // One 桶的单槽必是 s(slot_of 在本桶命中),违约即桶内定位已坏,响亮暴露。
            Some(Bucket::One(x)) => {
                debug_assert_eq!(*x, s, "One 桶单槽应为待摘槽");
                true
            }
            Some(Bucket::Many(slots)) => {
                let pos = slots.iter().position(|&x| x == s).expect("待摘槽必在桶内");
                slots.swap_remove(pos); // 桶内顺序无语义
                slots.is_empty()
            }
            None => panic!("待摘槽必在桶内"),
        };
        if empty {
            self.buckets.remove(&h);
        }
    }

    fn alloc_slot(&mut self, key: &Path, idx: u32) -> u32 {
        match self.free.pop() {
            Some(s) => {
                self.pool[s as usize] = (key.to_path_buf(), idx);
                s
            }
            None => {
                self.pool.push((key.to_path_buf(), idx));
                (self.pool.len() - 1) as u32
            }
        }
    }
}

/// FNV-1a 64(手写,不引新依赖):键路径原始字节 → 桶键。
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

    /// 碰撞消解钉:同桶两键必须以池中字节各自命中,互不串扰。
    /// 构造:正常插入两键后,把 a 的 hash 桶手工改为「b 的槽在前、a 的槽在后」
    /// (模拟真实碰撞里另一键先入桶)。突变「桶内只取首槽不比字节」→ get(a)
    /// 返回 b 的值,本测红。
    #[test]
    fn colliding_keys_resolved_by_pool_bytes() {
        let mut m = DirIndexMap::default();
        let a = Path::new("/tmp/x/a");
        let b = Path::new("/tmp/x/b");
        m.insert(a, 1);
        m.insert(b, 2);
        let bytes_a = a.as_os_str().as_encoded_bytes();
        let bytes_b = b.as_os_str().as_encoded_bytes();
        let slot_of_bytes = |m: &DirIndexMap, bytes: &[u8]| {
            (0..m.pool.len() as u32)
                .find(|&s| m.bytes_at(s) == bytes)
                .expect("键应在池中")
        };
        let (slot_a, slot_b) = (slot_of_bytes(&m, bytes_a), slot_of_bytes(&m, bytes_b));
        let ha = fnv1a64(bytes_a);
        m.buckets.insert(ha, Bucket::Many(vec![slot_b, slot_a]));

        assert_eq!(m.get(a), Some(1), "桶内字节比对:跳过兄弟槽命中自身值");
        assert_eq!(m.get(b), Some(2), "同桶兄弟互不串扰");
        // 碰撞桶内的摘除:只摘 a,b 幸存
        assert_eq!(m.remove(a), Some(1));
        assert_eq!(m.get(a), None);
        assert_eq!(m.get(b), Some(2), "摘除不得误伤同桶兄弟");
    }

    /// 池槽复用钉:摘除腾出的槽被新键复用,池不随增删单调增长;复用后旧键消失
    /// (槽字节已覆盖,不复活)、新键可查。
    #[test]
    fn removed_slot_is_reused_and_old_key_vanishes() {
        let mut m = DirIndexMap::default();
        let a = Path::new("/tmp/x/a");
        m.insert(a, 1);
        assert_eq!(m.remove(a), Some(1));
        let c = Path::new("/tmp/x/c");
        m.insert(c, 3);
        assert_eq!(m.pool.len(), 1, "复用摘除槽,池不长高");
        assert_eq!(m.get(a), None, "旧键不复活(槽字节已覆盖)");
        assert_eq!(m.get(c), Some(3));
        assert_eq!(m.keys().collect::<Vec<_>>(), vec![c]);
    }

    /// retain 钉:按值谓词清扫——不满足谓词的键整键消失(桶与池同步),满足的
    /// 连同同桶兄弟一起幸存;清扫腾出的槽可复用。
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
        assert_eq!(m.pool.len(), 3, "retain 不缩池,槽回 freelist");
        m.insert(Path::new("/tmp/x/d"), 4);
        assert_eq!(m.pool.len(), 3, "新键复用被清槽");
        assert_eq!(m.get(Path::new("/tmp/x/d")), Some(4));
    }
}
