//! 落盘:布局与内存同构(ADR-3),mmap 零反序列化加载的前提;
//! last_event_id 供停机重放(ADR-5);写 = 临时文件 + fsync + rename 原子替换。
//! 读侧:load_mmap 产出 SharedView(mmap 只读视图,立即可查),materialize 物化为可变 Index。
use std::io::Write;
use std::path::Path;

use crate::arena::ArenaBuilder;
use crate::entry::Entry;
use crate::index::{Index, IndexView};

pub const MAGIC: &[u8; 4] = b"MCEI";
pub const FORMAT_VERSION: u32 = 1;

#[derive(Default, Clone, Debug)]
pub struct IndexMeta {
    pub built_at: u64,
    pub last_event_id: u64,
    pub scan_roots: Vec<Vec<u8>>,
    pub excluded: Vec<Vec<u8>>,
}

fn put_u32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn put_u64(v: &mut Vec<u8>, x: u64) {
    v.extend_from_slice(&x.to_le_bytes());
}

/// 布局(与内存同构,读侧 mmap 零反序列化,Task 12 消费):
/// header: magic[4] | u32 version | u32 n_entries | u32 tombstone_count |
///         u32 n_path_offsets | u64 built_at | u64 last_event_id |
///         u32 n_roots + 逐个 (u32 len + bytes) |
///         u32 n_excluded + 逐个 (u32 len + bytes) |
///         u64 display_arena_len | u64 match_arena_len | u64 path_pool_len
/// body(顺序):display_arena | match_arena | path_pool |
///         path_offsets 逐对 (u32, u32)(与条目号全量对齐,含 (0,0) 占位)|
///         [4B 对齐填充] |
///         entries 逐条 24 字节(五 u32 LE + flags u8 + 3 字节 0 填充)
/// 对齐规则:entries 段起始向上对齐到 4B,写侧补零;读侧同规则
/// entries_off = align_up(path_offsets 段末, 4)(mmap 裸转型 *const Entry 的对齐前提)。
/// 取舍:rename 原子替换保证崩溃不损坏——断电最坏整体回滚 rename,旧文件复活,数据不损坏;
/// 「断电必达」需 F_FULLFSYNC + 目录 fsync,暂不做(product 边界外)。
pub fn write_atomic(index: &Index, meta: &IndexMeta, path: &Path) -> std::io::Result<()> {
    debug_assert_eq!(
        index.path_offsets.len(),
        index.entries.len(),
        "path_offsets 与条目号全量对齐是落盘布局不变量"
    );
    let mut buf: Vec<u8> = Vec::with_capacity(1 << 20);
    buf.extend_from_slice(MAGIC);
    put_u32(&mut buf, FORMAT_VERSION);
    put_u32(&mut buf, index.entries.len() as u32);
    put_u32(&mut buf, index.tombstone_count());
    // n_path_offsets 存条目总数(非有真值的目录段数):读侧按 n×8 定位 entries 段起点
    put_u32(&mut buf, index.path_offsets.len() as u32);
    put_u64(&mut buf, meta.built_at);
    put_u64(&mut buf, meta.last_event_id);
    put_u32(&mut buf, meta.scan_roots.len() as u32);
    for r in &meta.scan_roots {
        put_u32(&mut buf, r.len() as u32);
        buf.extend_from_slice(r);
    }
    put_u32(&mut buf, meta.excluded.len() as u32);
    for e in &meta.excluded {
        put_u32(&mut buf, e.len() as u32);
        buf.extend_from_slice(e);
    }
    put_u64(&mut buf, index.display_arena.data.len() as u64);
    put_u64(&mut buf, index.match_arena.data.len() as u64);
    put_u64(&mut buf, index.path_pool.data.len() as u64);
    buf.extend_from_slice(&index.display_arena.data);
    buf.extend_from_slice(&index.match_arena.data);
    buf.extend_from_slice(&index.path_pool.data);
    for (s, l) in &index.path_offsets {
        put_u32(&mut buf, *s);
        put_u32(&mut buf, *l);
    }
    // entries 段起始向上对齐到 4B(mmap 裸转型 *const Entry 的对齐前提);
    // 读侧同规则:entries_off = align_up(path_offsets 段末, 4)
    while !buf.len().is_multiple_of(4) {
        buf.push(0);
    }
    // 逐字段 LE 写,禁裸内存拷贝:repr(C) 的 3 字节填充内容未定义,
    // 裸拷贝会把未初始化垃圾写进文件(不可复现、且污染同构布局假设)
    for e in &index.entries {
        buf.extend_from_slice(&e.display_start.to_le_bytes());
        buf.extend_from_slice(&e.display_len.to_le_bytes());
        buf.extend_from_slice(&e.match_start.to_le_bytes());
        buf.extend_from_slice(&e.match_len.to_le_bytes());
        buf.extend_from_slice(&e.parent.to_le_bytes());
        buf.push(e.flags);
        buf.extend_from_slice(&[0u8; 3]); // 对齐填充
    }

    let tmp = tmp_path_for(path)?;
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&buf)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 临时文件名 = 目标文件名整体加 .tmp 后缀,恒与目标不同。
/// 禁 with_extension:目标名本身以 .tmp 结尾时退化为同名(tmp == path),
/// 「写临时 + rename 替换」塌缩成原地截断写,原子性消失。
pub(crate) fn tmp_path_for(path: &Path) -> std::io::Result<std::path::PathBuf> {
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "路径无文件名,无法构造临时文件",
        )
    })?;
    // pid + 进程内序号:防并发写同一目标时两方共用 tmp 名互相踩踏
    // (daemon 的 stop 落盘与消费线程退出落盘曾实测交错损坏索引文件)。
    static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(path.with_file_name(format!(
        "{}.{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        seq
    )))
}

// —— 读侧:mmap 零反序列化加载(ADR-3) ——

/// 编译期钉:Entry 定长 24B 是 mmap 裸转型 *const Entry 的前提(布局同构契约),
/// Entry 布局改动时此处先红,防线前移到编译期。
const _: () = assert!(std::mem::size_of::<Entry>() == 24, "Entry 必须 24 字节");
const _: () = assert!(
    std::mem::align_of::<Entry>() == 4,
    "Entry 对齐必须 4(mmap 裸转型对齐前提)"
);

#[derive(thiserror::Error, Debug)]
pub enum LoadError {
    #[error("magic 不符")]
    BadMagic,
    #[error("版本不识别: {0}")]
    BadVersion(u32),
    #[error("文件过短或布局损坏")]
    Truncated,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// mmap 只读视图:五原语直切 mmap 区段,零反序列化。冷启动两段式第一步——
/// load_mmap 后立即可查(query::execute 直消费 &dyn IndexView,零分支扩散);
/// 第二步 materialize() 物化为可变 Index。
///
/// 一致性信任级别:load 侧对 magic/version/段长做 need 全覆盖校验;
/// 条目内 arena 偏移与 path_seg 的 idx 不逐一校验——越界属文件损坏或调用方 bug,
/// 表现为切片 panic(与内存 Index 同一信任级别),不做运行时防御。
pub struct SharedView {
    mmap: memmap2::Mmap,
    pub meta: IndexMeta,
    entries_off: usize,
    display_off: usize,
    match_off: usize,
    path_pool_off: usize,
    path_offsets_off: usize,
    n_entries: usize,
}

impl IndexView for SharedView {
    fn entries(&self) -> &[Entry] {
        // SAFETY:三前提在 load_mmap 已逐一成立——
        // 1) 对齐:mmap 基址页对齐,entries_off 经 align_up(…, 4) 且 Entry 对齐值 = 4(repr(C) 全 u32);
        // 2) 边界:need(entries_off, n_entries*24) 校验通过,切片不越出 mmap;
        // 3) 位型合法:Entry 全字段 u32/u8,任意位型皆合法值(repr(C) 无失效位型)。
        // 生命周期:返回切片借用 &self,与 mmap 同寿,不出借圈。
        unsafe {
            std::slice::from_raw_parts(
                self.mmap[self.entries_off..].as_ptr() as *const Entry,
                self.n_entries,
            )
        }
    }
    fn display_arena(&self) -> &[u8] {
        &self.mmap[self.display_off..self.match_off]
    }
    fn match_arena(&self) -> &[u8] {
        &self.mmap[self.match_off..self.path_pool_off]
    }
    fn path_pool(&self) -> &[u8] {
        &self.mmap[self.path_pool_off..self.path_offsets_off]
    }
    fn path_seg(&self, idx: u32) -> (u32, u32) {
        let off = self.path_offsets_off + idx as usize * 8;
        (
            u32::from_le_bytes(self.mmap[off..off + 4].try_into().unwrap()),
            u32::from_le_bytes(self.mmap[off + 4..off + 8].try_into().unwrap()),
        )
    }
}

impl SharedView {
    /// 冷启动两段式第二步:从 mmap 物化为可变 Index(零逻辑重建——直接拷字节)。
    /// 条目 1:1 拷贝(条目号不变、墓碑保留),父指针无需重映射;
    /// live 目录路径池经父链重建(与 compact 同法,复用 dir_path_bytes 唯一实现),
    /// 墓碑目录的陈旧池段不重建(置 (0,0) 占位)——查询只读活目录段,行为无差。
    pub fn materialize(&self) -> Index {
        let entries = self.entries();
        let display = self.display_arena();
        let match_arena = self.match_arena();
        let mut new_entries: Vec<Entry> = Vec::with_capacity(entries.len());
        let mut display_out = ArenaBuilder::default();
        let mut match_out = ArenaBuilder::default();
        let mut live = 0u32;
        let mut tombstones = 0u32;
        for e in entries {
            let d = &display[e.display_start as usize..(e.display_start + e.display_len) as usize];
            let m = &match_arena[e.match_start as usize..(e.match_start + e.match_len) as usize];
            let (ds, dl) = display_out.push(d);
            let (ms, ml) = match_out.push(m);
            new_entries.push(Entry {
                display_start: ds,
                display_len: dl,
                match_start: ms,
                match_len: ml,
                parent: e.parent,
                flags: e.flags,
            });
            if e.is_live() {
                live += 1;
            } else {
                tombstones += 1;
            }
        }
        let mut i = Index::from_parts(
            new_entries,
            display_out,
            match_out,
            ArenaBuilder::default(),
            vec![(0, 0); entries.len()],
            live,
            tombstones,
        );
        // 顺序约束与 compact 一致:先拷好父链再建路径池(dir_path_bytes 依赖父链)
        for n in 0..i.entries.len() {
            if i.entries[n].is_dir() && i.entries[n].is_live() {
                let pp = i.dir_path_bytes(n as u32);
                i.path_offsets[n] = i.path_pool.push(&pp);
            }
        }
        i
    }
}

/// mmap 零拷贝加载:File::open → Mmap::map → header 逐字段读取(全部 need 边界校验)。
/// 段切界与写侧同规则:entries_off = align_up(path_offsets 段末, 4)。
pub fn load_mmap(path: &Path) -> Result<Box<SharedView>, LoadError> {
    let file = std::fs::File::open(path)?;
    // 0 字节文件必须早拒:Mmap::map 对空文件报 Io,而 truncated_prefix 语义期望 Truncated
    if file.metadata()?.len() == 0 {
        return Err(LoadError::Truncated);
    }
    // SAFETY:文件只读打开;写侧走「临时文件 + rename」原子替换,映射期间 inode 不会被原地改写
    let mmap = unsafe { memmap2::Mmap::map(&file)? };
    let buf = &mmap[..];

    let mut cur = 0usize;
    need(buf, cur, 4)?;
    if &buf[0..4] != MAGIC {
        return Err(LoadError::BadMagic);
    }
    cur = 4;
    let version = rd_u32(buf, &mut cur)?;
    if version != FORMAT_VERSION {
        return Err(LoadError::BadVersion(version));
    }
    let n_entries = rd_u32(buf, &mut cur)?;
    let _tombstone_count = rd_u32(buf, &mut cur)?; // header 计数,读出即弃(读取面以条目 flags 为准)
    let n_path_offsets = rd_u32(buf, &mut cur)?;
    let built_at = rd_u64(buf, &mut cur)?;
    let last_event_id = rd_u64(buf, &mut cur)?;

    let mut scan_roots = Vec::new();
    let n_roots = rd_u32(buf, &mut cur)?;
    for _ in 0..n_roots {
        scan_roots.push(rd_bytes(buf, &mut cur)?);
    }
    let mut excluded = Vec::new();
    let n_excluded = rd_u32(buf, &mut cur)?;
    for _ in 0..n_excluded {
        excluded.push(rd_bytes(buf, &mut cur)?);
    }

    let display_len = rd_u64(buf, &mut cur)?;
    let match_len = rd_u64(buf, &mut cur)?;
    let pool_len = rd_u64(buf, &mut cur)?;

    let display_off = cur;
    need(buf, display_off, display_len)?;
    let match_off = display_off + display_len as usize;
    need(buf, match_off, match_len)?;
    let path_pool_off = match_off + match_len as usize;
    need(buf, path_pool_off, pool_len)?;
    let path_offsets_off = path_pool_off + pool_len as usize;

    // path_offsets 与条目号全量对齐是布局不变量(写侧同源落盘),违反即布局损坏
    if n_path_offsets != n_entries {
        return Err(LoadError::Truncated);
    }
    let offsets_bytes = n_path_offsets as u64 * 8;
    need(buf, path_offsets_off, offsets_bytes)?;
    let offsets_end = path_offsets_off + offsets_bytes as usize;

    // 与写侧同规则:entries 段起始向上对齐 4B(写侧补零,读侧同公式切界)
    let entries_off = offsets_end.next_multiple_of(4);
    need(buf, entries_off, n_entries as u64 * 24)?;

    Ok(Box::new(SharedView {
        mmap,
        meta: IndexMeta {
            built_at,
            last_event_id,
            scan_roots,
            excluded,
        },
        entries_off,
        display_off,
        match_off,
        path_pool_off,
        path_offsets_off,
        n_entries: n_entries as usize,
    }))
}

/// 边界检查:off..off+len 必须是 buf 内的合法区间;0 长度请求在界内时接受,
/// off 越过文件尾(含 off == len 处的 0 长度)拒绝。
/// 全 u64 域比较长度,无溢出面;len/offset 来自文件,通过后才允许切片。
fn need(buf: &[u8], off: usize, len: u64) -> Result<(), LoadError> {
    if off > buf.len() || len > (buf.len() - off) as u64 {
        return Err(LoadError::Truncated);
    }
    Ok(())
}

/// 读取助手自检边界:每读必查,header 中段截断返回 Truncated 而非切片 panic。
fn rd_u32(buf: &[u8], cur: &mut usize) -> Result<u32, LoadError> {
    need(buf, *cur, 4)?;
    let v = u32::from_le_bytes(buf[*cur..*cur + 4].try_into().unwrap());
    *cur += 4;
    Ok(v)
}

fn rd_u64(buf: &[u8], cur: &mut usize) -> Result<u64, LoadError> {
    need(buf, *cur, 8)?;
    let v = u64::from_le_bytes(buf[*cur..*cur + 8].try_into().unwrap());
    *cur += 8;
    Ok(v)
}

/// 读「u32 长度 + 字节串」段(roots/excluded 逐个);长度越界返回 Truncated。
fn rd_bytes(buf: &[u8], cur: &mut usize) -> Result<Vec<u8>, LoadError> {
    let len = rd_u32(buf, cur)? as u64;
    need(buf, *cur, len)?;
    let v = buf[*cur..*cur + len as usize].to_vec();
    *cur += len as usize;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_header_magic_version() {
        let mut i = Index::new();
        let d = i.insert("docs", crate::entry::NO_PARENT, true);
        i.insert("a.txt", d, false);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.bin");
        let meta = IndexMeta {
            built_at: 1700000000,
            last_event_id: 42,
            scan_roots: vec![b"/Users/t".to_vec()],
            excluded: vec![],
        };
        write_atomic(&i, &meta, &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..4], MAGIC);
        assert_eq!(
            u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            FORMAT_VERSION
        );
        // last_event_id 落位:header 固定前段 20B + built_at(20..28)+ last_event_id(28..36)
        assert_eq!(u64::from_le_bytes(bytes[28..36].try_into().unwrap()), 42);
    }

    // 确定性双钉:同一索引写两次字节级一致(逐字段 LE 写无 padding 垃圾、无随机源的回归钉)
    #[test]
    fn write_is_deterministic_byte_for_byte() {
        let mut i = Index::new();
        let d = i.insert("docs", crate::entry::NO_PARENT, true);
        i.insert("a.txt", d, false);
        let dir = tempfile::tempdir().unwrap();
        let meta = IndexMeta {
            built_at: 1,
            last_event_id: 2,
            scan_roots: vec![],
            excluded: vec![],
        };
        let p1 = dir.path().join("a.bin");
        let p2 = dir.path().join("b.bin");
        write_atomic(&i, &meta, &p1).unwrap();
        write_atomic(&i, &meta, &p2).unwrap();
        assert_eq!(std::fs::read(&p1).unwrap(), std::fs::read(&p2).unwrap());
    }

    #[test]
    fn write_replaces_atomically_no_tmp_left() {
        let i = Index::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.bin");
        let meta = IndexMeta::default();
        write_atomic(&i, &meta, &path).unwrap();
        write_atomic(&i, &meta, &path).unwrap(); // 二次覆盖
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(left.len(), 1); // 无 .tmp 残留
    }

    // 临时文件名钉:目标名本身以 .tmp 结尾时,临时文件不得与目标同名
    // (回退成 with_extension 即红:with_extension("out.tmp") == "out.tmp" 同名塌缩)
    #[test]
    fn tmp_name_never_collides_with_tmp_suffixed_target() {
        let target = Path::new("/data/out.tmp");
        assert_ne!(tmp_path_for(target).unwrap(), target);
        assert_ne!(
            tmp_path_for(Path::new("/data/index.bin")).unwrap(),
            Path::new("/data/index.bin")
        );
    }
}

#[cfg(test)]
mod load_tests {
    use super::*;

    fn build_and_save(dir: &tempfile::TempDir, last_id: u64) -> (Index, std::path::PathBuf) {
        let mut i = Index::new();
        let d = i.insert("docs", crate::entry::NO_PARENT, true);
        i.insert("résumé.txt", d, false);
        // 1 字节名子目录 + 1 字节名文件(résumé.txt 为 12B UTF-8,é 各占 2B):
        // display/match 两池各 18B、路径池 10B → 池末偏移 114、offsets_end = 114 + 4×8 = 146,
        // 146 ≡ 2 (mod 4) 非 4 对齐——钉住 entries 段 align_up 切界
        // (删读侧 next_multiple_of(4) 或写侧补零,此 fixture 下必红)
        i.insert("x", d, true);
        i.insert("y", d, false);
        let path = dir.path().join("index.bin");
        let meta = IndexMeta {
            built_at: 1,
            last_event_id: last_id,
            scan_roots: vec![],
            excluded: vec![],
        };
        write_atomic(&i, &meta, &path).unwrap();
        (i, path)
    }

    #[test]
    fn mmap_view_reads_identically() {
        let dir = tempfile::tempdir().unwrap();
        let (src, path) = build_and_save(&dir, 7);
        let view = load_mmap(&path).unwrap();
        let v: &dyn IndexView = &*view;
        assert_eq!(
            v.entries().len() as u32,
            src.entry_count() + src.tombstone_count()
        );
        assert_eq!(v.match_name(1), src.match_name_of(1));
        assert_eq!(v.display_name(1), src.display_name_of(1));
        assert_eq!(view.meta.last_event_id, 7);
        // scan_roots/excluded 段往返钉(build_and_save 传空,此处自建非空)
        let path2 = dir.path().join("with-roots.bin");
        let meta2 = IndexMeta {
            built_at: 2,
            last_event_id: 9,
            scan_roots: vec![b"/Users/t".to_vec()],
            excluded: vec![b"~/Library".to_vec()],
        };
        write_atomic(&src, &meta2, &path2).unwrap();
        let view2 = load_mmap(&path2).unwrap();
        assert_eq!(view2.meta.scan_roots, vec![b"/Users/t".to_vec()]);
        assert_eq!(view2.meta.excluded, vec![b"~/Library".to_vec()]);
        // 查询直跑共享视图(Task 8 的 execute 零改动消费)。
        // 词用 résumé:normalize_for_match 只做 NFC+小写折叠不去变音符,"resume" 必不命中
        let out = crate::query::execute(v, &crate::query::parse("résumé").unwrap(), 10);
        assert_eq!(out.total, 1);
        // path: 查询走 mmap 视图的 path_seg + path_pool 切段(docs 的子项:résumé.txt + x + y)
        let path_out = crate::query::execute(v, &crate::query::parse("path:docs").unwrap(), 10);
        assert_eq!(path_out.total, 3);
    }

    #[test]
    fn bad_magic_and_version_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.bin");
        std::fs::write(&p, b"XXXXgarbage-which-is-long-enough").unwrap();
        assert!(matches!(load_mmap(&p), Err(LoadError::BadMagic)));
        let (_, path) = build_and_save(&dir, 1);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[4..8].copy_from_slice(&99u32.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();
        assert!(matches!(load_mmap(&path), Err(LoadError::BadVersion(99))));
    }

    // materialize 全码的行为钉:物化索引与源索引路径全集等价、可变操作可用
    #[test]
    fn materialize_produces_equivalent_index() {
        let dir = tempfile::tempdir().unwrap();
        let (src, path) = build_and_save(&dir, 3);
        let view = load_mmap(&path).unwrap();
        let mut m = view.materialize();
        assert_eq!(m.entry_count(), src.entry_count());
        let a: std::collections::BTreeSet<String> = (0..m.entries.len() as u32)
            .map(|i| m.full_path(i).to_string_lossy().into_owned())
            .collect();
        let b: std::collections::BTreeSet<String> = (0..src.entries.len() as u32)
            .map(|i| src.full_path(i).to_string_lossy().into_owned())
            .collect();
        assert_eq!(a, b);
        // 路径池重建钉:目录段经条目号可取且与源一致(删此重建代码此行必红)
        assert_eq!(m.dir_path(0), src.dir_path(0));
        // 物化后可变操作真实可用:remove 走墓碑制,计数联动(children_of 只读,不算实证)
        let before = m.entry_count();
        m.remove(m.children_of(0)[0]);
        assert_eq!(m.entry_count(), before - 1);
        assert_eq!(m.tombstone_count(), 1);
    }

    // live/tombstone 计数拆分钉:物化把计数全记进 live 时此测必红
    #[test]
    fn materialize_preserves_tombstone_counts() {
        let dir = tempfile::tempdir().unwrap();
        let mut i = Index::new();
        let d = i.insert("docs", crate::entry::NO_PARENT, true);
        let gone = i.insert("gone.txt", d, false);
        i.insert("kept.txt", d, false);
        i.remove(gone);
        let path = dir.path().join("tomb.bin");
        write_atomic(&i, &IndexMeta::default(), &path).unwrap();
        let m = load_mmap(&path).unwrap().materialize();
        assert_eq!(m.entry_count(), i.entry_count());
        assert_eq!(m.tombstone_count(), i.tombstone_count());
        assert_eq!(m.entries.len(), i.entries.len(), "条目 1:1 拷贝,墓碑保留");
        // 穿插墓碑后父链与路径池仍一致
        assert_eq!(m.dir_path(0), b"docs".to_vec());
        assert_eq!(m.full_path(2).to_string_lossy(), "docs/kept.txt");
    }

    // 截断钉:合法文件的任意前缀都必须返回 Truncated,绝不 panic
    // (读助手每读必查的全覆盖证明;删任一读点的边界检查,对应前缀长度即 panic)
    #[test]
    fn truncated_prefix_never_panics() {
        let dir = tempfile::tempdir().unwrap();
        let (_, path) = build_and_save(&dir, 1);
        let full = std::fs::read(&path).unwrap();
        for len in 0..full.len() {
            let p = dir.path().join("cut.bin");
            std::fs::write(&p, &full[..len]).unwrap();
            assert!(
                matches!(load_mmap(&p), Err(LoadError::Truncated)),
                "前缀长度 {len} 应返回 Truncated"
            );
        }
    }
}
