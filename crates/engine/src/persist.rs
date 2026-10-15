//! 落盘:布局与内存同构(ADR-3),mmap 零反序列化加载的前提;
//! last_event_id 供停机重放(ADR-5);写 = 临时文件 + fsync + rename 原子替换。
use std::io::Write;
use std::path::Path;

use crate::index::Index;

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
fn tmp_path_for(path: &Path) -> std::io::Result<std::path::PathBuf> {
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "路径无文件名,无法构造临时文件",
        )
    })?;
    Ok(path.with_file_name(format!("{}.tmp", name.to_string_lossy())))
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
