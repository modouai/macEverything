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

// —— T3 合成视图对拍(memory-footprint tech.md §4.6 风险表:查询双源合并零差异)——

use engine::index::IndexView;
use engine::overlay::{DiffTarget, Overlay};

/// 按完整路径定位 base 条目号。
fn find_id(index: &engine::index::Index, p: &Path) -> u32 {
    (0..index.entries.len() as u32)
        .find(|&i| index.full_path(i) == p)
        .unwrap_or_else(|| panic!("base 应含 {}", p.display()))
}

/// 合成视图全量命中路径集(含根,与 ref_set 同口径)。
/// overlay 对拍的排除集(与 fixture 扫描同参)。
fn fixture_excludes() -> Vec<PathBuf> {
    vec![PathBuf::from("node_modules")]
}

fn overlay_hits(base: &engine::index::Index, ov: &Overlay) -> BTreeSet<String> {
    let view = ov.composite_view(base);
    let out = engine::query::execute(&view, &engine::query::parse("*").unwrap(), usize::MAX);
    let set: BTreeSet<String> = out
        .hits
        .iter()
        .map(|&h| view.full_path_composite(h))
        .collect();
    assert_eq!(set.len(), out.total as usize, "命中路径不得互相折叠");
    set
}

/// 参考集:独立递归枚举 + 根。
fn ref_set(root: &Path, excludes: &[PathBuf]) -> BTreeSet<String> {
    let mut r = BTreeSet::new();
    ref_walk(root, excludes, &mut r);
    r.insert(root.to_string_lossy().into_owned());
    r
}

/// 受控树:
/// ```text
/// root/
/// ├── src/{a.rs, lib/deep.txt}
/// ├── docs/b.md
/// ├── node_modules/x.js   ← 排除目标
/// └── README.md
/// ```
fn overlay_fixture() -> (tempfile::TempDir, engine::index::Index) {
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    fs::create_dir_all(r.join("src/lib/deeper")).unwrap();
    fs::create_dir_all(r.join("node_modules")).unwrap();
    fs::create_dir_all(r.join("docs")).unwrap();
    fs::write(r.join("src/a.rs"), "").unwrap();
    fs::write(r.join("src/lib/deep.txt"), "").unwrap();
    fs::write(r.join("src/lib/deeper/bottom.txt"), "").unwrap();
    fs::write(r.join("node_modules/x.js"), "").unwrap();
    fs::write(r.join("docs/b.md"), "").unwrap();
    fs::write(r.join("README.md"), "").unwrap();
    let (index, gaps) =
        engine::scanner::scan_roots(&[r.to_path_buf()], &[PathBuf::from("node_modules")], 1);
    assert_eq!(gaps.permission.len(), 0);
    (t, index)
}

/// 目录 diff 后立即查询:旧路径为空/新路径可见,对拍零差异。
#[test]
fn parity_overlay_diff_then_query_zero_diff() {
    let (t, index) = overlay_fixture();
    let root = t.path();
    // 磁盘漂移:删两个文件、添两个文件(一层根 + 一层 src)
    fs::remove_file(root.join("src/a.rs")).unwrap();
    fs::write(root.join("src/new.rs"), "").unwrap();
    fs::remove_file(root.join("README.md")).unwrap();
    fs::write(root.join("top.txt"), "").unwrap();

    let mut ov = Overlay::new();
    ov.apply_diff(
        &index,
        DiffTarget::Base(find_id(&index, root)),
        root,
        &[PathBuf::from("node_modules")],
    )
    .unwrap();
    let src = root.join("src");
    ov.apply_diff(
        &index,
        DiffTarget::Base(find_id(&index, &src)),
        &src,
        &[PathBuf::from("node_modules")],
    )
    .unwrap();

    // 立即查询专项:旧路径为空、新路径可见(不用重启/重扫)
    let view = ov.composite_view(&index);
    let probe =
        |q: &str| engine::query::execute(&view, &engine::query::parse(q).unwrap(), 10).total;
    assert_eq!(probe("whole:a.rs"), 0, "已删文件必须被屏蔽");
    assert_eq!(probe("whole:new.rs"), 1, "新文件必须可见");
    assert_eq!(probe("whole:README.md"), 0, "已删文件必须被屏蔽");
    assert_eq!(probe("whole:top.txt"), 1, "新文件必须可见");

    assert_eq!(
        overlay_hits(&index, &ov),
        ref_set(root, &fixture_excludes()),
        "对拍零差异"
    );
}

/// overlay 嵌套两层:root → src → src/lib 逐层 diff,深层 overlay 条目
/// 路径两段拼接正确,对拍零差异。
#[test]
fn parity_overlay_nested_two_levels_zero_diff() {
    let (t, index) = overlay_fixture();
    let root = t.path();
    fs::write(root.join("src/lib/new_deep.txt"), "").unwrap();
    fs::remove_file(root.join("src/lib/deep.txt")).unwrap();
    fs::write(root.join("src/nested.txt"), "").unwrap();

    let mut ov = Overlay::new();
    let src = root.join("src");
    let lib = root.join("src/lib");
    for dir in [root.to_path_buf(), src.clone(), lib.clone()] {
        ov.apply_diff(
            &index,
            DiffTarget::Base(find_id(&index, &dir)),
            &dir,
            &[PathBuf::from("node_modules")],
        )
        .unwrap();
    }
    let view = ov.composite_view(&index);
    let probe =
        |q: &str| engine::query::execute(&view, &engine::query::parse(q).unwrap(), 10).total;
    assert_eq!(
        probe("whole:new_deep.txt"),
        1,
        "两层 overlay 深层新文件可见"
    );
    assert_eq!(probe("whole:deep.txt"), 0, "深层已删文件被屏蔽");
    // 深层 overlay 条目路径经两段拼接完整复原
    let out = engine::query::execute(&view, &engine::query::parse("new_deep").unwrap(), 10);
    assert_eq!(
        view.full_path_composite(out.hits[0]),
        lib.join("new_deep.txt").to_string_lossy()
    );
    assert_eq!(
        overlay_hits(&index, &ov),
        ref_set(root, &fixture_excludes()),
        "对拍零差异"
    );
}

/// 子目录 diff 后父未再 diff:子树 overlay 裁定、父层 Base 陈旧不可见;
/// 父随后 diff 时子树仍有效(「有 anchor 不在 overlaid → 终止有效」钉,
/// 突变破坏该终止条件时本测必红)。
#[test]
fn parity_overlay_subdir_diff_then_parent() {
    let (t, index) = overlay_fixture();
    let root = t.path();
    fs::write(root.join("src/lib/added.txt"), "").unwrap();

    let mut ov = Overlay::new();
    let lib = root.join("src/lib");
    // 子目录先 diff(父从未 diff:桥接占位)
    ov.apply_diff(
        &index,
        DiffTarget::Base(find_id(&index, &lib)),
        &lib,
        &[PathBuf::from("node_modules")],
    )
    .unwrap();
    {
        let view = ov.composite_view(&index);
        let probe =
            |q: &str| engine::query::execute(&view, &engine::query::parse(q).unwrap(), 10).total;
        assert_eq!(probe("whole:deep.txt"), 1, "子树 overlay 裁定:旧文件可见");
        assert_eq!(probe("whole:added.txt"), 1, "子树 overlay 裁定:新文件可见");
    }
    // 父层新文件:父未 diff → 不可见
    fs::write(root.join("src/fresh.txt"), "").unwrap();
    {
        let view = ov.composite_view(&index);
        let probe =
            |q: &str| engine::query::execute(&view, &engine::query::parse(q).unwrap(), 10).total;
        assert_eq!(probe("whole:fresh.txt"), 0, "父未 diff,父层新文件不可见");
    }
    // 父随后 diff:lib 幸存且为目录 → 桥续接,子树仍有效
    let src = root.join("src");
    ov.apply_diff(
        &index,
        DiffTarget::Base(find_id(&index, &src)),
        &src,
        &[PathBuf::from("node_modules")],
    )
    .unwrap();
    let view = ov.composite_view(&index);
    let probe =
        |q: &str| engine::query::execute(&view, &engine::query::parse(q).unwrap(), 10).total;
    assert_eq!(probe("whole:added.txt"), 1, "父 diff 后子树仍有效");
    assert_eq!(probe("whole:fresh.txt"), 1, "父 diff 后父层新文件可见");
    assert_eq!(
        overlay_hits(&index, &ov),
        ref_set(root, &fixture_excludes()),
        "对拍零差异"
    );
}

/// EPERM 负缓存目录混合树:不可读目录保留语义(条目在 Base、子树空、不进
/// overlay 可见集),与参考枚举(读不到即剪)零差异;可读环境自动退化为
/// 全索引树,对拍同样成立(两实现同语义,互证不失效)。
#[test]
fn parity_overlay_eperm_mixed_tree() {
    use std::os::unix::fs::PermissionsExt;
    let (t, index) = overlay_fixture();
    let root = t.path();
    let locked = root.join("src/locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("secret.txt"), "").unwrap();
    // 扫描在锁定目录创建之后?不——fixture 已扫。此处按「锁定目录在扫描后出现,
    // 随后父目录 diff」编排:listing 收录 locked(父可读),其子树因 diff 失败
    // (负缓存短路)保持空——daemon 语义的引擎级等价物。
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(&locked).is_ok() {
        // 越权环境(root):读 000 目录不报错,两侧同语义实读,对拍不失效;恢复权限保清理
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let mut ov = Overlay::new();
    let src = root.join("src");
    ov.apply_diff(
        &index,
        DiffTarget::Base(find_id(&index, &src)),
        &src,
        &[PathBuf::from("node_modules")],
    )
    .unwrap();
    // locked 目录可读性恢复前的子树空:diff locked 会失败(EPERM 形态),
    // 负缓存语义 = 不 diff、条目保留。此处只验证父层 diff 后的零差异。
    let view = ov.composite_view(&index);
    let probe =
        |q: &str| engine::query::execute(&view, &engine::query::parse(q).unwrap(), 10).total;
    assert_eq!(
        probe("whole:locked"),
        1,
        "EPERM 目录条目保留(父 listing 收录)"
    );
    assert_eq!(
        overlay_hits(&index, &ov),
        ref_set(root, &fixture_excludes()),
        "对拍零差异(锁定目录子树两侧同为空或同为实读)"
    );
    let _ = fs::set_permissions(&locked, fs::Permissions::from_mode(0o755));
}

/// 类型翻转 + 子树删除(评审缺陷 1/2 的对拍形态):file→dir 翻转不桥连 base
/// 陈旧文件;整树删除后父层 diff 摘净深层 anchor(各自 diff 过的独立桥)。
/// 对拍零差异 + 物化产物同集(突变「link 守卫去 is_dir」「删除方向去 purge 扫」
/// 时对应断言必红)。
#[test]
fn parity_overlay_type_flip_and_subtree_removal_zero_diff() {
    let (t, index) = overlay_fixture();
    let root = t.path();
    // 形态一:file→dir 翻转(src/a.rs)
    fs::remove_file(root.join("src/a.rs")).unwrap();
    fs::create_dir(root.join("src/a.rs")).unwrap();
    fs::write(root.join("src/a.rs/i.txt"), "").unwrap();
    // 形态二:整树删除已 diff 过的子树(src/lib);lib 内深层目录(deeper,
    // 首扫收录)也各自 diff——独立桥的深层 anchor 摘除必须经 purge 扫,unlink 够不着
    fs::write(root.join("src/lib/new_deep.txt"), "").unwrap();

    let mut ov = Overlay::new();
    let src = root.join("src");
    let lib = root.join("src/lib");
    ov.apply_diff(
        &index,
        DiffTarget::Base(find_id(&index, root)),
        root,
        &fixture_excludes(),
    )
    .unwrap();
    ov.apply_diff(
        &index,
        DiffTarget::Base(find_id(&index, &src)),
        &src,
        &fixture_excludes(),
    )
    .unwrap();
    // 深层各自 diff(翻转目录 a.rs 经 overlay 寻址 diff)
    ov.apply_diff(
        &index,
        DiffTarget::Base(find_id(&index, &lib)),
        &lib,
        &fixture_excludes(),
    )
    .unwrap();
    let deeper = root.join("src/lib/deeper");
    ov.apply_diff(
        &index,
        DiffTarget::Base(find_id(&index, &deeper)),
        &deeper,
        &fixture_excludes(),
    )
    .unwrap();
    let a_rs = ov
        .overlay_dir_id(&src.join("a.rs"))
        .expect("翻转目录应入 overlay 寻址面");
    ov.apply_diff(
        &index,
        DiffTarget::OverlayDir(a_rs),
        &src.join("a.rs"),
        &fixture_excludes(),
    )
    .unwrap();

    // 翻转钉:src/a.rs 唯一命中且为目录(base 陈旧文件不得复活)
    {
        let view = ov.composite_view(&index);
        let out = engine::query::execute(&view, &engine::query::parse("whole:a.rs").unwrap(), 10);
        assert_eq!(out.total, 1, "翻转后恰一份");
        assert!(view.entry(out.hits[0]).is_dir(), "命中必须是目录");
    }

    // 形态二落地:整树删除 src/lib,再 diff src → 深层桥摘净
    fs::remove_dir_all(&lib).unwrap();
    ov.apply_diff(
        &index,
        DiffTarget::Base(find_id(&index, &src)),
        &src,
        &fixture_excludes(),
    )
    .unwrap();
    {
        let view = ov.composite_view(&index);
        let probe =
            |q: &str| engine::query::execute(&view, &engine::query::parse(q).unwrap(), 10).total;
        assert_eq!(probe("whole:deep.txt"), 0, "已删子树深层文件不得泄漏");
        assert_eq!(probe("whole:bottom"), 0, "已删深层独立桥目录内容不得泄漏");
        assert_eq!(probe("whole:new_deep.txt"), 0, "已删子树深层增量不得泄漏");
    }

    assert_eq!(
        overlay_hits(&index, &ov),
        ref_set(root, &fixture_excludes()),
        "对拍零差异"
    );
    // 物化产物与合成视图同集(墓碑桥/幽灵根不落盘)
    let merged = engine::overlay::materialize_composite(&index, &ov);
    let merged_paths: BTreeSet<String> = (0..merged.entries.len() as u32)
        .filter(|&i| merged.entries[i as usize].is_live())
        .map(|i| merged.full_path(i).to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        merged_paths,
        overlay_hits(&index, &ov),
        "物化与合成视图零差异"
    );
}

/// 子先父后 + re-diff(评审第四形态「桥覆盖孤儿」对拍):子目录先 diff(桥为
/// 顶层占位)→ 父层首 diff 必须复用并 re-parent,不得新建覆盖;多轮 re-diff
/// 恒一份,整树删除后零泄漏。突变「复用/reparent 去掉」时:命中数塌缩/重复
/// (link 覆盖清扫旧桥 → 数据丢失)与 hit_paths 折叠断言必红。
#[test]
fn parity_overlay_child_first_then_parent_zero_diff() {
    let (t, index) = overlay_fixture();
    let root = t.path();
    fs::write(root.join("src/lib/new_deep.txt"), "").unwrap();

    let mut ov = Overlay::new();
    let src = root.join("src");
    let lib = root.join("src/lib");
    // 子先:lib 直接 diff(父 src、祖 root 均从未 diff,桥为顶层占位)
    ov.apply_diff(
        &index,
        DiffTarget::Base(find_id(&index, &lib)),
        &lib,
        &fixture_excludes(),
    )
    .unwrap();
    // 祖首:复用 lib 桥并挂到 root 桥下
    ov.apply_diff(
        &index,
        DiffTarget::Base(find_id(&index, root)),
        root,
        &fixture_excludes(),
    )
    .unwrap();
    // 父首:lib 桥从 root 桥下 re-parent 到 src 桥下(链中段挂接)
    ov.apply_diff(
        &index,
        DiffTarget::Base(find_id(&index, &src)),
        &src,
        &fixture_excludes(),
    )
    .unwrap();
    // re-diff 轮次:恒一份、路径不折叠
    for _ in 0..2 {
        ov.apply_diff(
            &index,
            DiffTarget::Base(find_id(&index, &lib)),
            &lib,
            &fixture_excludes(),
        )
        .unwrap();
        ov.apply_diff(
            &index,
            DiffTarget::Base(find_id(&index, root)),
            root,
            &fixture_excludes(),
        )
        .unwrap();
    }
    let view = ov.composite_view(&index);
    let probe =
        |q: &str| engine::query::execute(&view, &engine::query::parse(q).unwrap(), 10).total;
    assert_eq!(probe("whole:deep.txt"), 1, "re-diff 后旧文件恰一份");
    assert_eq!(probe("whole:new_deep.txt"), 1, "re-diff 后增量恰一份");
    assert_eq!(
        overlay_hits(&index, &ov),
        ref_set(root, &fixture_excludes()),
        "对拍零差异(重复行会在此处经 total 折叠断言暴露)"
    );

    // 整树删除 + 父层 diff:零泄漏、物化无幽灵根
    fs::remove_dir_all(&lib).unwrap();
    ov.apply_diff(
        &index,
        DiffTarget::Base(find_id(&index, &src)),
        &src,
        &fixture_excludes(),
    )
    .unwrap();
    let view = ov.composite_view(&index);
    let probe =
        |q: &str| engine::query::execute(&view, &engine::query::parse(q).unwrap(), 10).total;
    assert_eq!(probe("whole:deep.txt"), 0, "已删路径不得泄漏");
    assert_eq!(probe("whole:new_deep.txt"), 0, "已删增量不得泄漏");
    assert_eq!(
        overlay_hits(&index, &ov),
        ref_set(root, &fixture_excludes()),
        "删除后对拍零差异"
    );
    let merged = engine::overlay::materialize_composite(&index, &ov);
    let merged_paths: BTreeSet<String> = (0..merged.entries.len() as u32)
        .filter(|&i| merged.entries[i as usize].is_live())
        .map(|i| merged.full_path(i).to_string_lossy().into_owned())
        .collect();
    assert_eq!(merged_paths, overlay_hits(&index, &ov), "物化无幽灵根");
}
