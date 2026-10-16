//! 对拍:索引条目集 == 独立实现的递归枚举集(同根/同排除/同符号链接策略)。
//! product.md「准」轴验收的机械化:与 find 对拍零差异。
//! 参考实现刻意独立复刻语义(不调 scanner 内部),两实现互证。
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// 排除语义复刻(与 scanner 契约一致:绝对路径前缀 + 相对名组件匹配)。
fn ref_is_excluded(p: &Path, excludes: &[PathBuf]) -> bool {
    excludes.iter().any(|x| {
        if x.is_absolute() {
            p.starts_with(x)
        } else {
            p.components().any(|c| c.as_os_str() == x.as_os_str())
        }
    })
}

/// 参考递归枚举:符号链接不跟、排除目录整棵剪枝;产出绝对路径集合。
fn ref_walk(root: &Path, excludes: &[PathBuf], out: &mut BTreeSet<String>) {
    let Ok(rd) = fs::read_dir(root) else { return };
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        let p = e.path();
        if ft.is_dir() && ref_is_excluded(&p, excludes) {
            continue;
        }
        out.insert(p.to_string_lossy().into_owned());
        if ft.is_dir() {
            ref_walk(&p, excludes, out);
        }
    }
}

/// 确定性伪随机树生成(LCG,免 rand 依赖):深度 3-5、约 1200 条目、
/// 每层混入 node_modules(名字式排除目标)与一个符号链接。
fn build_tree(root: &Path) -> u64 {
    let mut seed: u64 = 0x5EED;
    let mut next = move || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) as u32
    };
    let mut count = 0u64;
    fn rec(dir: &Path, depth: u32, next: &mut impl FnMut() -> u32, count: &mut u64) {
        if depth == 0 {
            return;
        }
        let dirs = (next() % 4) + 1;
        for d in 0..dirs {
            let name = if next().is_multiple_of(7) {
                "node_modules".to_string()
            } else {
                format!("d{depth}_{d}")
            };
            let sub = dir.join(&name);
            fs::create_dir_all(&sub).unwrap();
            for f in 0..((next() % 6) + 2) {
                fs::write(sub.join(format!("f{f}.txt")), "").unwrap();
                *count += 1;
            }
            *count += 1;
            rec(&sub, depth - 1, next, count);
        }
        // 每层一个符号链接(递归一层的自指,不跟随防环)
        #[cfg(unix)]
        {
            let link = dir.join("loop.lnk");
            let _ = std::os::unix::fs::symlink(dir, &link);
        }
    }
    rec(root, 4, &mut next, &mut count);
    count
}

#[test]
fn parity_zero_diff() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path();
    build_tree(root);
    // 排除:名字式(node_modules)+ 绝对式(一条深度 1 目录)
    let excludes = vec![PathBuf::from("node_modules"), root.join("d4_0")];

    // 索引侧
    let (index, gaps) = engine::scanner::scan_roots(&[root.to_path_buf()], &excludes, 4);
    assert_eq!(gaps.permission.len(), 0, "受控树无权限缺口");
    let indexed: BTreeSet<String> = (0..index.entries.len() as u32)
        .filter(|i| index.entries[*i as usize].parent != engine::entry::NO_PARENT)
        .map(|i| index.full_path(i).to_string_lossy().into_owned())
        .collect();

    // 参考侧
    let mut reference: BTreeSet<String> = BTreeSet::new();
    ref_walk(root, &excludes, &mut reference);

    assert!(!reference.is_empty(), "参考集不应为空(树生成失败?)");
    let only_index: Vec<_> = indexed.difference(&reference).take(5).collect();
    let only_ref: Vec<_> = reference.difference(&indexed).take(5).collect();
    assert!(
        indexed == reference,
        "对拍零差异被破坏\n索引独有: {only_index:?}\n参考独有: {only_ref:?}"
    );
}

/// 并行臂同样对拍零差异(串行/并行语义一致 + 与参考一致,三方闭合)。
#[test]
fn parity_parallel_arm() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path();
    build_tree(root);
    let excludes = vec![PathBuf::from("node_modules")];
    let (s_idx, _) = engine::scanner::scan_roots(&[root.to_path_buf()], &excludes, 1);
    let (p_idx, _) = engine::scanner::scan_roots(&[root.to_path_buf()], &excludes, 8);
    let s: BTreeSet<String> = (0..s_idx.entries.len() as u32)
        .map(|i| s_idx.full_path(i).to_string_lossy().into_owned())
        .collect();
    let p: BTreeSet<String> = (0..p_idx.entries.len() as u32)
        .map(|i| p_idx.full_path(i).to_string_lossy().into_owned())
        .collect();
    let mut reference: BTreeSet<String> = BTreeSet::new();
    ref_walk(root, &excludes, &mut reference);
    let ref_with_root: BTreeSet<String> = {
        let mut r = reference.clone();
        r.insert(root.to_string_lossy().into_owned());
        r
    };
    assert_eq!(s, p, "串行/并行路径集合一致");
    assert_eq!(s, ref_with_root, "索引(含根)与参考(加根)一致");
}
