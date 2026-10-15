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
///         entries 逐条 24 字节(五 u32 LE + flags u8 + 3 字节 0 填充)
pub fn write_atomic(index: &Index, meta: &IndexMeta, path: &Path) -> std::io::Result<()> {
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

    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&buf)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_magic_version_roundtrip() {
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
}
