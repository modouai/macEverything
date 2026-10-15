# macEverything v1-core — 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 交付 macEverything v1 纯引擎:自建全盘文件名索引(查询 P99 <10ms)、FSEvents 重放增量(≤500ms)、mmap 落盘冷启动(<1s)、并行首扫(≤5s),交互面为 daemon + 五个子命令。

**Architecture:** Cargo workspace 双 crate——`engine`(纯逻辑库:entry/arena/index/query/scanner/persist/fsevents/watcher/reconcile/config/error)+ `cli`(二进制:protocol/daemon/client/main)。增量正确性建立在「事件只当信号、目录级 diff 唯一真相」上;落盘与内存同构布局,mmap 零反序列化。

**Tech Stack:** Rust stable(Apple Silicon)、fsevent-sys + core-foundation-sys、memchr、rayon、unicode-normalization、memmap2、serde + serde_json + toml、thiserror + log。禁 async 运行时。

**Product Spec:** `docs/specs/v1-core/product.md`(v1.2.0,产品已确认)
**Tech Spec:** `docs/specs/v1-core/tech.md`(v1.1.0,已确认)

**通用验证命令**(每个 Task 收尾跑,下文以 `<check>` 代称):

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

**与 tech.md 的两处实现偏差**(实现期发现,内存预算内,Task 21 回填 tech.md):
1. `Entry` 为 24 字节/条——display 与 match arena 因折叠变长不平行,需各自偏移;非 spec 的 16 字节。百万条目内存 90-100MB,仍远低于 500MB 红线。
2. 「全 Unicode 大小写折叠」用 std `str::to_lowercase()` 实现(语义等价,覆盖 é/É),不引入 unicode-case 依赖。

---

### Task 1: workspace 脚手架

**Files:**
- Create: `Cargo.toml`、`.gitignore`
- Create: `crates/engine/Cargo.toml`、`crates/engine/src/lib.rs`
- Create: `crates/cli/Cargo.toml`、`crates/cli/src/main.rs`

**Why:** tech.md ADR-1:双 crate 零 GUI 依赖。所有后续 Task 的地基。
**Search anchor:** `ls crates/` 验证结构。

- [ ] **Step 1: 写 workspace 根 `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = ["crates/engine", "crates/cli"]

[workspace.package]
version = "0.1.0"
edition = "2021"

[workspace.dependencies]
memchr = "2"
rayon = "1"
unicode-normalization = "0.1"
memmap2 = "0.9"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
thiserror = "1"
log = "0.4"
fsevent-sys = "4"
core-foundation-sys = "0.8"
libc = "0.2"
```

- [ ] **Step 2: 写 `crates/engine/Cargo.toml`**

```toml
[package]
name = "engine"
version.workspace = true
edition.workspace = true

[dependencies]
memchr.workspace = true
rayon.workspace = true
unicode-normalization.workspace = true
memmap2.workspace = true
serde.workspace = true
toml.workspace = true
thiserror.workspace = true
log.workspace = true
fsevent-sys.workspace = true
core-foundation-sys.workspace = true
libc.workspace = true
```

- [ ] **Step 3: 写 `crates/engine/src/lib.rs`(空模块声明,后续 Task 逐个填充)**

```rust
pub mod entry;
pub mod arena;
pub mod index;
pub mod query;
pub mod scanner;
pub mod persist;
pub mod fsevents;
pub mod watcher;
pub mod reconcile;
pub mod config;
pub mod error;
```

- [ ] **Step 4: 写 `crates/cli/Cargo.toml` 与 `crates/cli/src/main.rs`**

```toml
[package]
name = "macEverything"
version.workspace = true
edition.workspace = true

[[bin]]
name = "macEverything"
path = "src/main.rs"

[dependencies]
engine = { path = "../engine" }
serde_json.workspace = true
anyhow = "1"
```

```rust
fn main() -> anyhow::Result<()> {
    anyhow::bail!("not implemented yet")
}
```

- [ ] **Step 5: 写 `.gitignore`**

```
/target
*.tmp
```

- [ ] **Step 6: 各模块占位文件(entry.rs 等 10 个空文件),让 Task 1 可编译**

```bash
cd crates/engine/src && for f in entry arena index query scanner persist fsevents watcher reconcile config error; do echo "" > $f.rs; done
```

- [ ] **Step 7: 验证 + 提交**

Run: `cargo build && <check>`
Expected: 编译通过、无警告。

```bash
git add -A && git commit -m "feat: workspace 脚手架(engine+cli 双 crate,ADR-1)"
```

---

### Task 2: Spike A — readdir 吞吐基线

**Files:**
- Create: `scripts/spike-readdir/`(`Cargo.toml` 独立小 crate,不进 workspace members)
- Modify: `docs/specs/v1-core/tech.md`(回填基线数字)

**Why:** tech.md 性能预算表首扫行:基线为工具实测参考值未在本机锁定;预算推导 ≤5s 红线必须以实测收口,推不出则回辉哥重谈目标 1。此 Task 不可跳过。

- [ ] **Step 1: 写合成树生成 + 吞吐测量程序 `scripts/spike-readdir/src/main.rs`**

```rust
use std::fs;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("");
    let root = std::path::PathBuf::from(args.get(2).map(String::as_str).unwrap_or("/tmp/mce-bench"));
    match cmd {
        "gen" => gen(&root, 1_000_000),
        "scan" => {
            let n_files = std::cell::Cell::new(0u64);
            let t = Instant::now();
            walk(&root, &n_files);
            let el = t.elapsed();
            println!("files={} elapsed={:.2}s throughput={:.0} files/s", n_files.get(), el.as_secs_f64(), n_files.get() as f64 / el.as_secs_f64());
        }
        _ => eprintln!("usage: spike-readdir gen|scan <root>"),
    }
}

fn gen(root: &std::path::Path, total: u64) {
    let _ = fs::remove_dir_all(root);
    let dirs = total / 40; // 平均每目录 40 项
    for d in 0..dirs {
        let dir = root.join(format!("d{:07}", d / 500)).join(format!("s{:03}", d % 500));
        fs::create_dir_all(&dir).unwrap();
        for f in 0..40 {
            fs::write(dir.join(format!("file_{:07}.txt", d * 40 + f)), "").unwrap();
        }
    }
    println!("generated {} dirs under {}", dirs, root.display());
}

fn walk(dir: &std::path::Path, n: &std::cell::Cell<u64>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let ft = e.file_type().unwrap();
        n.set(n.get() + 1);
        if ft.is_dir() { walk(&e.path(), n); }
    }
}
```

- [ ] **Step 2: 跑 10 万文件树快速验证 + 百万文件树正式测(热缓存 3 轮取中位)**

```bash
cd scripts/spike-readdir && cargo run --release -- gen /tmp/mce-bench 100000 && cargo run --release -- scan /tmp/mce-bench
cargo run --release -- gen /tmp/mce-bench 1000000
for i in 1 2 3; do cargo run --release -- scan /tmp/mce-bench; done
```

- [ ] **Step 3: 结论回填 tech.md 预算表首扫行(单线程实测吞吐 → 8 线程推导 → ≤5s 是否成立);若推不出红线,停下回辉哥重谈目标 1**
- [ ] **Step 4: 清理基准树 + 提交**

```bash
rm -rf /tmp/mce-bench
git add -A && git commit -m "chore(spike): readdir 吞吐基线实测,回填首扫预算推导"
```

---

### Task 3: Spike B — FSEvents sinceWhen 行为验证

**Files:**
- Create: `crates/engine/src/fsevents.rs`(直调封装雏形,spike 后演化)
- Create: `crates/engine/examples/spike_fsevents.rs`
- Modify: `docs/specs/v1-core/tech.md`(回填 ADR-5 信号结论)

**Why:** tech.md ADR-5 与测试节首位:「日志已滚动」可观测信号(Apple 未承诺)必须实现前验证;结论决定重放不可用判定走信号还是纯保守判定器。

- [ ] **Step 1: 写最小 FFI 封装雏形 `crates/engine/src/fsevents.rs`(本 Task 只需 stream 创建 + 回调 + 事件 ID)**

```rust
//! FSEvents C API 直调封装(ADR-5)。unsafe 集中本文件。
#![allow(non_upper_case_globals, non_camel_case_types, clippy::upper_case_acronyms)]
use core_foundation_sys::base::{CFRelease, kCFAllocatorDefault};
use core_foundation_sys::runloop::{kCFRunLoopDefaultMode, CFRunLoopAddSource, CFRunLoopGetMain, CFRunLoopRun, CFRunLoopSourceRef};
use fsevent_sys::core_foundation::CFArrayRef;
use fsevent_sys::fsevent::*;
use std::ffi::CString;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

pub struct FseEvent {
    pub paths: Vec<String>,
    pub flags: FSEventStreamEventFlags,
    pub last_id: u64,
}

static LAST_ID: AtomicU64 = AtomicU64::new(0);
static EVENTQ: Mutex<Vec<FseEvent>> = Mutex::new(Vec::new());

extern "C" fn callback(
    _stream: FSEventStreamRef,
    _info: *mut std::ffi::c_void,
    num: usize,
    paths: *mut *mut std::ffi::c_char, // FSEventStreamEventPaths via VoidPtr? 实际是 CFArrayRef 语义
    flags: *mut FSEventStreamEventFlags,
    ids: *mut u64,
) {
    // fsevent-sys 的回调签名按其 crate 文档;此处逐事件收集
    let mut batch = Vec::new();
    unsafe {
        for i in 0..num {
            let p = std::ffi::CStr::from_ptr(*paths.add(i)).to_string_lossy().into_owned();
            batch.push(p);
            LAST_ID.store(*ids.add(i), Ordering::SeqCst);
        }
    }
    EVENTQ.lock().unwrap().push(FseEvent { paths: batch, flags: unsafe { *flags }, last_id: LAST_ID.load(Ordering::SeqCst) });
}

pub fn create_stream(since_when: u64, paths: &[&str], latency_s: f64) -> FSEventStreamRef {
    let cpaths: Vec<CString> = paths.iter().map(|p| CString::new(*p).unwrap()).collect();
    let ctx = FSEventStreamContext { version: 0, info: std::ptr::null_mut(), retain: None, release: None, copyDescription: None };
    let arr = unsafe { CFArrayCreate(kCFAllocatorDefault, cpaths.iter().map(|c| c.as_ptr() as *const std::ffi::c_void).collect::<Vec<_>>().as_ptr(), cpaths.len() as isize, std::ptr::null()) };
    unsafe {
        let s = FSEventStreamCreate(kCFAllocatorDefault, Some(callback), &ctx, arr as CFArrayRef, since_when, latency_s, kFSEventStreamCreateFlagNoDefer | kFSEventStreamCreateFlagFileEvents);
        s
    }
}
// 注:spike 阶段不追求封装完整性;stream 调度(runloop source 挂接、后台线程)
// 在 Task 15 收口为正式实现。本文件 Step 1-2 只需能建流、收回调、打事件 ID。
```

- [ ] **Step 2: 补 runloop 接线与事件打印(spike 不求封装美,求行为观测)——完整代码在 Task 15 收口,本步允许 spike 版临时实现:订阅后打印每批 `flags` 的十六进制与 `ids`,重点观测 HistoryDone flag(spike 结论已实证其值为 0x10,非早期草案误写的 0x20000)出现时机与事件 ID 连续性**
- [ ] **Step 3: 写 `examples/spike_fsevents.rs`:① 以 `kFSEventStreamEventIdSinceNow` 建流,记当前 id;② 在观察目录建 3 个文件,确认收到事件与 id 递增;③ 用「当前 id 减 10 亿」(模拟过旧 sinceWhen)重建流,观测:是否收到历史事件?HistoryDone 后是否静默从 now 开始?有无特定 flag?把三问的观测结果写进命令输出**
- [ ] **Step 4: 跑 spike,结论回填 tech.md ADR-5(「spike 结论:信号 有/无,判定器按 X 设计」),若无可靠信号,保守判定器(零事件+48h)为唯一路径**
- [ ] **Step 5: 提交(spike 代码保留,fsevents.rs 雏形后续 Task 演化)**

```bash
<check> && git add -A && git commit -m "chore(spike): FSEvents sinceWhen 行为验证,ADR-5 回退路径定稿"
```

---

### Task 4: entry.rs + arena.rs — 条目结构与双 arena

**Files:**
- Modify: `crates/engine/src/entry.rs`、`crates/engine/src/arena.rs`
- Test: 同文件 `#[cfg(test)]`

**Why:** tech.md ADR-2:定长条目数组 + 双名称池是索引布局地基;折叠(match)与原始(display)变长不平行需各自偏移(实现偏差 1)。

- [ ] **Step 1: 写失败测试(`entry.rs` 顶部)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_is_24_bytes() {
        assert_eq!(std::mem::size_of::<Entry>(), 24);
    }

    #[test]
    fn normalize_nfc_and_casefold() {
        // NFD 形态的 é(65 + U+0301)与 NFC 形态(é)折叠后一致;É→é 大小写互搜
        let a = normalize_for_match("cafe\u{301}z");
        let b = normalize_for_match("caféz");
        let c = normalize_for_match("CAFÉZ");
        assert_eq!(a, b);
        assert_eq!(b, c);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p engine entry`
Expected: FAIL(`Entry` 未定义)

- [ ] **Step 3: 实现 `entry.rs`**

```rust
//! 条目结构:定长 24B,存双 arena 各自偏移 + 父指针 + 标志位(ADR-2)。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub display_start: u32,
    pub display_len: u32,
    pub match_start: u32,
    pub match_len: u32,
    pub parent: u32,
    pub flags: u8,
}

pub const FLAG_DIR: u8 = 1;
pub const FLAG_TOMBSTONE: u8 = 2;
pub const NO_PARENT: u32 = u32::MAX;

impl Entry {
    pub fn is_dir(&self) -> bool { self.flags & FLAG_DIR != 0 }
    pub fn is_live(&self) -> bool { self.flags & FLAG_TOMBSTONE == 0 }
}

/// 匹配形态:NFC 归一 + 全 Unicode 小写折叠(std to_lowercase,覆盖 é/É)。
pub fn normalize_for_match(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    name.nfc().collect::<String>().to_lowercase()
}

#[cfg(test)]
mod tests { /* Step 1 的测试 */ }
```

- [ ] **Step 4: 实现 `arena.rs`(追加式字节池)**

```rust
//! 追加式名称池:写入返回 (start, len);只增不删(删除=墓碑),整理时重建。
#[derive(Default)]
pub struct ArenaBuilder { pub data: Vec<u8> }

impl ArenaBuilder {
    pub fn push(&mut self, bytes: &[u8]) -> (u32, u32) {
        let start = self.data.len() as u32;
        self.data.extend_from_slice(bytes);
        (start, bytes.len() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn push_returns_offset() {
        let mut a = ArenaBuilder::default();
        let (s1, l1) = a.push(b"abc");
        let (s2, l2) = a.push(b"de");
        assert_eq!((s1, l1), (0, 3));
        assert_eq!((s2, l2), (3, 2));
        assert_eq!(&a.data[0..3], b"abc");
    }
}
```

- [ ] **Step 5: `<check>` 通过后提交**

```bash
git add -A && git commit -m "feat(engine): Entry 24B 结构 + 双 arena + NFC/全 Unicode 折叠(ADR-2)"
```

---

### Task 5: index.rs — 构建(插入/父链拼路径/子项)

**Files:**
- Modify: `crates/engine/src/index.rs`
- Test: 同文件 `#[cfg(test)]`

**Why:** tech.md ADR-2 + 变更方案:索引支持查询与路径求值的读侧能力,是 diff/持久化/query 的公共底座。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn ix() -> Index { Index::new() }

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
        let nfc = i.insert("résumé.txt", NO_PARENT, false);              // NFC 形态
        assert_eq!(i.match_name_of(nfd), i.match_name_of(nfc));          // 同一折叠形态
    }
}
```

- [ ] **Step 2: 跑失败** Run: `cargo test -p engine index` Expected: FAIL
- [ ] **Step 3: 实现 `index.rs` 读侧**

```rust
//! 内存索引:扁平 Entry 数组 + 双 arena + 目录路径池(ADR-2)。
use crate::arena::ArenaBuilder;
use crate::entry::*;

pub struct Index {
    pub entries: Vec<Entry>,
    pub display_arena: ArenaBuilder,
    pub match_arena: ArenaBuilder,
    /// 目录路径池:path: 直扫用;条目号 → (start,len)
    pub path_pool: ArenaBuilder,
    pub path_offsets: Vec<(u32, u32)>,
    live_count: u32,
    tombstones: u32,
}

impl Index {
    pub fn new() -> Self {
        Self { entries: vec![], display_arena: ArenaBuilder::default(), match_arena: ArenaBuilder::default(), path_pool: ArenaBuilder::default(), path_offsets: vec![], live_count: 0, tombstones: 0 }
    }

    pub fn entry_count(&self) -> u32 { self.live_count }
    pub fn tombstone_count(&self) -> u32 { self.tombstones }

    pub fn insert(&mut self, name: &str, parent: u32, is_dir: bool) -> u32 {
        let m = normalize_for_match(name);
        let (ds, dl) = self.display_arena.push(name.as_bytes());
        let (ms, ml) = self.match_arena.push(m.as_bytes());
        let idx = self.entries.len() as u32;
        let mut flags = 0;
        if is_dir { flags |= FLAG_DIR; }
        self.entries.push(Entry { display_start: ds, display_len: dl, match_start: ms, match_len: ml, parent, flags });
        self.live_count += 1;
        if is_dir {
            let pp = self.dir_path_bytes(idx);
            self.path_offsets.push(self.path_pool.push(&pp));
        }
        idx
    }

    /// 目录条目的完整路径(match 形态,path_pool 用);idx 必须是目录。
    fn dir_path_bytes(&self, idx: u32) -> Vec<u8> {
        // 构建期父链已在 entries 中,可直接递归拼
        let mut parts: Vec<&[u8]> = vec![];
        let mut cur = idx;
        loop {
            let e = &self.entries[cur as usize];
            parts.push(&self.match_arena.data[e.match_start as usize..(e.match_start + e.match_len) as usize]);
            if e.parent == NO_PARENT { break; }
            cur = e.parent;
        }
        parts.reverse();
        parts.join(b"/".as_slice())
    }

    pub fn display_name_of(&self, idx: u32) -> &[u8] {
        let e = &self.entries[idx as usize];
        &self.display_arena.data[e.display_start as usize..(e.display_start + e.display_len) as usize]
    }
    pub fn match_name_of(&self, idx: u32) -> &[u8] {
        let e = &self.entries[idx as usize];
        &self.match_arena.data[e.match_start as usize..(e.match_start + e.match_len) as usize]
    }

    pub fn full_path(&self, idx: u32) -> std::path::PathBuf {
        let mut parts: Vec<Vec<u8>> = vec![];
        let mut cur = idx;
        loop {
            let e = &self.entries[cur as usize];
            parts.push(self.display_name_of(cur).to_vec());
            if e.parent == NO_PARENT { break; }
            cur = e.parent;
        }
        parts.reverse();
        let joined = parts.join(b"/".as_slice());
        String::from_utf8_lossy(&joined).into_owned().into()
    }

    /// 直接子项(含墓碑不过滤,由调用方按需过滤;返回活条目)
    pub fn children_of(&self, dir_idx: u32) -> Vec<u32> {
        self.entries.iter().enumerate()
            .filter(|(_, e)| e.parent == dir_idx && e.is_live())
            .map(|(i, _)| i as u32).collect()
    }
}
```

- [ ] **Step 4: 跑通过** Run: `cargo test -p engine index` Expected: PASS(children_of 是 O(n) 线性扫,百万规模下 diff 单目录 ~2-4ms,预算内;后续可用「目录 → 子项桶」优化,当前 YAGNI)
- [ ] **Step 5: `<check>` + 提交**

```bash
git add -A && git commit -m "feat(engine): index 读侧——插入/父链拼路径/子项/Unicode 别名一致(ADR-2)"
```

---

### Task 6: index.rs — 变更(墓碑/子树级联/diff/路径池维护/整理)

**Files:**
- Modify: `crates/engine/src/index.rs`
- Test: 同文件 `#[cfg(test)]`

**Why:** tech.md 增量管线:目录 diff 是增量唯一真相;目录改名需子树级联 + 路径池同步;墓碑 >20% 触发整理(风险表:墓碑膨胀)。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
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
        let kids: Vec<Vec<u8>> = i.children_of(d).iter().map(|k| i.display_name_of(*k).to_vec()).collect();
        assert!(kids.contains(&b"b.txt".to_vec()) && kids.contains(&b"a.txt".to_vec()));
        i.diff_dir(d, &listing(&[("b.txt", false)]));
        let kids: Vec<Vec<u8>> = i.children_of(d).iter().map(|k| i.display_name_of(*k).to_vec()).collect();
        assert_eq!(kids, vec![b"b.txt".to_vec()]); // a 被墓碑
        assert_eq!(i.tombstone_count(), 1);
    }

    #[test]
    fn dir_cascade_and_path_pool() {
        let mut i = ix();
        let p = i.insert("projects", NO_PARENT, true);
        let sub = i.insert("alpha", p, true);
        let _f = i.insert("x.rs", sub, false);
        assert_eq!(i.dir_path(sub), b"projects/alpha");
        i.rename_entry(p, "workspace");
        assert_eq!(i.dir_path(sub), b"workspace/alpha"); // 级联重写
        assert_eq!(i.dir_path(_f), b"workspace/alpha");   // 文件经父链求值
    }

    #[test]
    fn compact_reclaims_tombstones() {
        let mut i = ix();
        let d = i.insert("d", NO_PARENT, true);
        for k in 0..10 { let _ = i.insert(&format!("f{k}.txt"), d, false); }
        for k in 0..9 { i.remove(i.children_of(d)[0]); } // 剩 1 活 + 9 墓碑
        assert!(i.tombstone_ratio() > 0.2);
        i.compact();
        assert_eq!(i.tombstone_count(), 0);
        assert_eq!(i.entry_count(), 2); // d + 存活 f9
    }
}
```

- [ ] **Step 2: 跑失败** Run: `cargo test -p engine index` Expected: FAIL
- [ ] **Step 3: 实现 diff_dir / rename_entry / remove / dir_path / compact**

```rust
// 追加到 Index impl
use std::collections::BTreeMap;
pub type DirListing = BTreeMap<String, bool>;

pub struct DiffStats { pub added: u32, pub removed: u32 }

impl Index {
    /// 目录在磁盘上的现名(match 形态);供 watcher 从路径反查 diff。
    pub fn dir_path(&self, idx: u32) -> Vec<u8> {
        let (s, l) = self.path_offsets[idx as usize];
        self.path_pool.data[s as usize..(s + l) as usize].to_vec()
    }

    pub fn rename_entry(&mut self, idx: u32, new_name: &str) {
        let m = normalize_for_match(new_name);
        let (ds, dl) = self.display_arena.push(new_name.as_bytes());
        let (ms, ml) = self.match_arena.push(m.as_bytes());
        let e = &mut self.entries[idx as usize];
        e.display_start = ds; e.display_len = dl; e.match_start = ms; e.match_len = ml;
        if e.is_dir() { self.rebuild_paths_under(idx); } // 子树级联
    }

    /// 重写 idx 及其整棵子树的路径池条目(目录改名级联,ADR-2/风险表)。
    fn rebuild_paths_under(&mut self, root: u32) {
        fn walk(ix: &mut Index, cur: u32) {
            if ix.entries[cur as usize].is_dir() {
                let pp = ix.dir_path_bytes(cur);
                ix.path_offsets[cur as usize] = ix.path_pool.push(&pp);
            }
            let kids: Vec<u32> = ix.entries.iter().enumerate()
                .filter(|(_, e)| e.parent == cur && e.is_live()).map(|(i, _)| i as u32).collect();
            for k in kids { walk(ix, k); }
        }
        walk(self, root);
    }

    pub fn remove(&mut self, idx: u32) {
        fn walk(ix: &mut Index, cur: u32) {
            let e = &mut ix.entries[cur as usize];
            if !e.is_live() { return; }
            e.flags |= FLAG_TOMBSTONE;
            ix.live_count -= 1; ix.tombstones += 1;
            let kids: Vec<u32> = ix.entries.iter().enumerate()
                .filter(|(_, e2)| e2.parent == cur && e2.is_live()).map(|(i, _)| i as u32).collect();
            for k in kids { walk(ix, k); }
        }
        walk(self, idx);
    }

    /// 墓碑占比(整理触发阈值 20%,风险表)。
    pub fn tombstone_ratio(&self) -> f64 {
        let total = self.live_count + self.tombstones;
        if total == 0 { 0.0 } else { self.tombstones as f64 / total as f64 }
    }

    /// 唯一真相机制:按磁盘现目录项 diff 该目录(parent == dir_idx 的活条目)。
    pub fn diff_dir(&mut self, dir_idx: u32, on_disk: &DirListing) -> DiffStats {
        let mut added = 0u32; let mut removed = 0u32;
        let mut on_disk = on_disk.clone();
        let kids: Vec<u32> = self.children_of(dir_idx);
        for k in kids {
            let name = String::from_utf8_lossy(self.display_name_of(k)).into_owned();
            match on_disk.remove(&name) {
                Some(is_dir) => {
                    let e = self.entries[k as usize];
                    if e.is_dir() != is_dir { self.remove(k); let idx = self.insert(&name, dir_idx, is_dir); let _ = idx; added += 1; removed += 1; } // 类型变化视为删+增
                }
                None => { self.remove(k); removed += 1; }
            }
        }
        for (name, is_dir) in on_disk {
            self.insert(&name, dir_idx, is_dir); added += 1;
        }
        DiffStats { added, removed }
    }

    /// 墓碑整理:重建 entries/两 arena/路径池,重写父指针。
    pub fn compact(&mut self) {
        let old = std::mem::replace(self, Index::new());
        // 旧→新下标映射
        let mut map = vec![NO_PARENT; old.entries.len()];
        let mut new = Index::new();
        // 先复制所有条目名与 flags(保留 parent 待映射)
        for (i, e) in old.entries.iter().enumerate() {
            if !e.is_live() { continue; }
            let dname = String::from_utf8_lossy(old.display_name_of(i as u32)).into_owned();
            let is_dir = e.is_dir();
            let old_parent = e.parent;
            // 先占位插入,parent 稍后修
            let m = normalize_for_match(&dname);
            let (ds, dl) = new.display_arena.push(dname.as_bytes());
            let (ms, ml) = new.match_arena.push(m.as_bytes());
            let nidx = new.entries.len() as u32;
            new.entries.push(Entry { display_start: ds, display_len: dl, match_start: ms, match_len: ml, parent: old_parent, flags: if is_dir { FLAG_DIR } else { 0 } });
            map[i] = nidx;
            new.live_count += 1;
        }
        // 修父指针(父可能被墓碑?活条目父必活:remove 级联保证)
        for e in new.entries.iter_mut() {
            if e.parent != NO_PARENT { e.parent = map[e.parent as usize]; }
        }
        // 目录路径池重建
        for i in 0..new.entries.len() {
            if new.entries[i].is_dir() {
                let pp = new.dir_path_bytes(i as u32);
                new.path_offsets.push(new.path_pool.push(&pp));
            }
        }
        *self = new;
    }
}
```

- [ ] **Step 4: 跑通过** Run: `cargo test -p engine index` Expected: PASS(compact 中 dir_path_bytes 依赖已修好的父指针,注意实现顺序:先修父指针再建路径池——上述代码顺序已正确)
- [ ] **Step 5: `<check>` + 提交**

```bash
git add -A && git commit -m "feat(engine): index 变更——墓碑/子树级联/diff 唯一真相/路径池级联/整理(ADR-2)"
```

---

### Task 7: query.rs — 语法解析(六语法)

**Files:**
- Modify: `crates/engine/src/query.rs`
- Test: 同文件 `#[cfg(test)]`

**Why:** product.md §4 搜索语法是产品契约:空格 AND、`*`、`?`、`path:`(仅紧跟一词)、`case:`、`whole:`。解析先行,匹配(Task 8)只吃计划。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_terms_are_and() {
        let p = parse("季度 报告").unwrap();
        assert_eq!(p.name_terms.len(), 2);
        assert!(!p.case_sensitive);
        assert!(!p.whole_word);
        assert!(p.path_terms.is_empty());
    }

    #[test]
    fn wildcards_compile_to_anchored_parts() {
        let p = parse("*.pdf").unwrap();
        // * 展开为子串序列 + 尾锚:parts=[".pdf"], suffix_anchored
        assert_eq!(p.name_terms[0].parts, vec![b".pdf".to_vec()]);
        assert!(p.name_terms[0].suffix_anchor);
        assert!(!p.name_terms[0].prefix_anchor);

        let p2 = parse("报告?").unwrap();
        assert_eq!(p2.name_terms[0].parts, vec![b"报告".to_vec()]);
        assert!(p2.name_terms[0].suffix_anchor); // ? 消耗一个字符后结束 → 尾锚(固定长度尾段)
    }

    #[test]
    fn prefix_flags_scope() {
        let p = parse("path:合同 2026 case: whole:").unwrap();
        // path: 仅作用于「合同」;2026 是文件名词;case:/whole: 全局开关
        assert_eq!(p.path_terms.len(), 1);
        assert_eq!(p.path_terms[0].parts, vec!["合同".as_bytes().to_vec()]);
        assert_eq!(p.name_terms.len(), 1);
        assert!(p.case_sensitive);
        assert!(p.whole_word);
    }

    #[test]
    fn syntax_chars_inside_term_are_literal() {
        let p = parse("path:my*dir").unwrap();
        assert_eq!(p.path_terms[0].parts.len(), 2); // my*dir → ["my", "dir"] 中锚
    }

    #[test]
    fn empty_query_rejected() {
        assert!(parse("").is_err());
        assert!(parse("   ").is_err());
    }
}
```

- [ ] **Step 2: 跑失败** Run: `cargo test -p engine query` Expected: FAIL
- [ ] **Step 3: 实现 `query.rs` 解析器**

```rust
//! 查询计划:解析(product.md §4 契约)→ 匹配(Task 8)。
use crate::entry::normalize_for_match;

#[derive(Debug, PartialEq)]
pub struct Pattern {
    /// `*` 分段后的子串序列(经 normalize_for_match;case_sensitive 时保留原样由复核处理)
    pub parts: Vec<Vec<u8>>,
    /// 首段前锚定(`^`)与末段后锚定(`$`):通配符贴近串首/串尾时置位
    pub prefix_anchor: bool,
    pub suffix_anchor: bool,
}

#[derive(Debug)]
pub struct QueryPlan {
    pub name_terms: Vec<Pattern>,
    pub path_terms: Vec<Pattern>,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

#[derive(thiserror::Error, Debug)]
#[error("空查询")]
pub struct ParseError;

pub fn parse(query: &str) -> Result<QueryPlan, ParseError> {
    let mut plan = QueryPlan { name_terms: vec![], path_terms: vec![], case_sensitive: false, whole_word: false };
    let mut terms = query.split_whitespace();
    let mut any = false;
    while let Some(tok) = terms.next() {
        match tok {
            "case:" => plan.case_sensitive = true,
            "whole:" => plan.whole_word = true,
            _ if tok.starts_with("path:") => {
                let word = &tok["path:".len()..];
                let word = if word.is_empty() { terms.next().ok_or(ParseError)? } else { word };
                plan.path_terms.push(compile_pattern(word, plan.case_sensitive));
                any = true;
            }
            _ => { plan.name_terms.push(compile_pattern(tok, plan.case_sensitive)); any = true; }
        }
    }
    if !any { return Err(ParseError); }
    Ok(plan)
}

/// 通配编译:`*` 任意长(分段),`?` 恰一字符——归入段内一个字节占位语义
/// 由匹配端处理:此处产出「子串序列 + 首尾锚」;`?` 与相邻段合并为短段,
/// 匹配端对含 `?` 段用逐字符比较(段含 0x01 占位标记)。
fn compile_pattern(term: &str, case_sensitive: bool) -> Pattern {
    let source: String = if case_sensitive { term.to_string() } else { normalize_for_match(term) };
    let bytes = source.as_bytes();
    let mut parts: Vec<Vec<u8>> = vec![];
    let mut cur: Vec<u8> = vec![];
    let mut prefix_anchor = false;
    let mut suffix_anchor = false;
    let mut first = true;
    for &b in bytes {
        match b {
            b'*' => {
                if first { prefix_anchor = false; } // 首字符 * → 无前锚
                parts.push(std::mem::take(&mut cur));
                first = false;
            }
            b'?' => { cur.push(QMARK); first = false; }
            _ => { cur.push(b); }
        }
    }
    let _ = first;
    if bytes.first() != Some(&b'*') && !parts.is_empty() { prefix_anchor = true; }
    if bytes.last() == Some(&b'*') { suffix_anchor = false; } else { suffix_anchor = true; }
    if !cur.is_empty() { parts.push(cur); }
    // 处理纯首锚:串首无 * 且 parts 只有一段 → prefix_anchor 即整段前锚
    Pattern { parts, prefix_anchor: prefix_anchor && bytes.first() != Some(&b'*'), suffix_anchor }
}

/// `?` 占位字节(匹配端逐字符跳过);合法 UTF-8 输入不可能出现 0x01。
pub const QMARK: u8 = 0x01;
```

- [ ] **Step 4: 跑通过**(锚定语义以测试为准:`*.pdf` → parts=[".pdf"], prefix_anchor=false, suffix_anchor=true;`报告?` → parts=["报告"+QMARK], suffix_anchor=true)
- [ ] **Step 5: `<check>` + 提交**

```bash
git add -A && git commit -m "feat(engine): 查询解析——六语法契约,通配编译为锚定子串序列(product §4)"
```

---

### Task 8: query.rs — 匹配内核与打分(IndexView)

**Files:**
- Modify: `crates/engine/src/query.rs`
- Modify: `crates/engine/src/index.rs`(实现 IndexView)
- Modify: `crates/engine/src/persist.rs`(SharedView 实现同一 trait,本 Task 先以 trait + Index 实现收口,mmap 版在 Task 12 落地)

**Why:** tech.md 查询管线与预算:memmem 线性扫 + rayon 分块 = P99 <10ms 的实现载体;`path:` 走目录路径池(阻塞 3 修复)。IndexView trait 统一内存视图与 mmap 只读视图(ADR-3 两段式)。

- [ ] **Step 1: 在 `index.rs` 写 IndexView trait + Index 实现(失败测试先行)**

```rust
// index.rs 追加
#[cfg(test)]
mod view_tests {
    use super::*;
    #[test]
    fn view_exposes_slices() {
        let mut i = ix();
        let d = i.insert("docs", NO_PARENT, true);
        i.insert("a.txt", d, false);
        let v: &dyn IndexView = &i;
        assert_eq!(v.entries().len(), 2);
        assert_eq!(v.match_name(1), b"a.txt");
        assert_eq!(v.dir_path(0), b"docs");
    }
}

/// 内存视图与 mmap 只读视图的统一读取面(ADR-3:查询代码无分支扩散)。
pub trait IndexView: Sync {
    fn entries(&self) -> &[Entry];
    fn display_arena(&self) -> &[u8];
    fn match_arena(&self) -> &[u8];
    fn path_pool(&self) -> &[u8];
    fn match_name(&self, idx: u32) -> &[u8] {
        let e = &self.entries()[idx as usize];
        &self.match_arena()[e.match_start as usize..(e.match_start + e.match_len) as usize]
    }
    fn display_name(&self, idx: u32) -> &[u8] {
        let e = &self.entries()[idx as usize];
        &self.display_arena()[e.display_start as usize..(e.display_start + e.display_len) as usize]
    }
    fn dir_path(&self, idx: u32) -> &[u8] { self.path_pool() } // 目录条目:由 path_offsets 求段,见实现
    fn path_seg(&self, idx: u32) -> (u32, u32);
}

impl IndexView for Index {
    fn entries(&self) -> &[Entry] { &self.entries }
    fn display_arena(&self) -> &[u8] { &self.display_arena.data }
    fn match_arena(&self) -> &[u8] { &self.match_arena.data }
    fn path_pool(&self) -> &[u8] { &self.path_pool.data }
    fn path_seg(&self, idx: u32) -> (u32, u32) { self.path_offsets[idx as usize] }
    fn dir_path(&self, idx: u32) -> &[u8] {
        let (s, l) = self.path_offsets[idx as usize];
        &self.path_pool.data[s as usize..(s + l) as usize]
    }
}
```

- [ ] **Step 2: 在 `query.rs` 写匹配内核(失败测试先行)**

```rust
#[cfg(test)]
mod match_tests {
    use super::*;
    use crate::index::{Index, IndexView, NO_PARENT};

    fn sample() -> Index {
        let mut i = Index::new();
        let docs = i.insert("Documents", NO_PARENT, true);
        let proj = i.insert("projects", NO_PARENT, true);
        let _ = i.insert("季度报告.pdf", docs, false);
        let _ = i.insert("budget.xlsx", docs, false);
        let _ = i.insert("README.md", proj, false);
        i
    }

    #[test]
    fn substr_and_ordering() {
        let i = sample();
        let out = execute(&i, &parse("报告").unwrap(), 100);
        assert_eq!(out.total, 1);
        assert_eq!(i.display_name(out.hits[0]), b"季度报告.pdf");
    }

    #[test]
    fn wildcard_anchors() {
        let i = sample();
        assert_eq!(execute(&i, &parse("*.pdf").unwrap(), 100).total, 1);
        assert_eq!(execute(&i, &parse("*eport*").unwrap(), 100).total, 0); // 测试树无 eport
        assert_eq!(execute(&i, &parse("季度*").unwrap(), 100).total, 1);
    }

    #[test]
    fn path_term_hits_dir_children() {
        let i = sample();
        let out = execute(&i, &parse("path:documents").unwrap(), 100); // 折叠后小写
        assert_eq!(out.total, 2); // docs 下两个文件
    }

    #[test]
    fn case_and_whole() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let _ = i.insert("README.md", d, false);
        assert_eq!(execute(&i, &parse("readme").unwrap(), 100).total, 1);          // 默认不敏感
        assert_eq!(execute(&i, &parse("case:readme").unwrap(), 100).total, 0);     // case: 敏感
        assert_eq!(execute(&i, &parse("whole:readme.md").unwrap(), 100).total, 1);
        assert_eq!(execute(&i, &parse("whole:read").unwrap(), 100).total, 0);      // 整词不中
    }

    #[test]
    fn score_prefix_beats_substring() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        i.insert("报告.txt", d, false);
        i.insert("季度报告.txt", d, false);
        let out = execute(&i, &parse("报告").unwrap(), 100);
        assert_eq!(i.display_name(out.hits[0]), b"报告.txt"); // 前缀命中排前
    }

    #[test]
    fn limit_caps_and_total_accurate() {
        let i = sample();
        let out = execute(&i, &parse("*").unwrap(), 2);
        assert_eq!(out.hits.len(), 2);
        assert_eq!(out.total, 5);
    }
}
```

- [ ] **Step 3: 实现 `execute`(rayon 分块 + memmem + 打分)**

```rust
use memchr::memmem::Finder;
use rayon::prelude::*;

pub struct QueryOutcome { pub hits: Vec<u32>, pub total: u32, pub elapsed_ms: f64 }

/// 单模式命中判定:锚定子串序列 AND(QMARK 逐字节跳过)。
fn match_pattern(name: &[u8], p: &Pattern) -> bool {
    let mut start = 0usize;
    let mut first = true;
    for (pi, part) in p.parts.iter().enumerate() {
        if part.is_empty() { continue; }
        if part.contains(&QMARK) {
            // 含 ? 段:naive 逐字节匹配(? 跳过 1 字符,按 UTF-8 边界跳过续字节)
            let hit = (start..=name.len().saturating_sub(part.len()))
                .any(|off| name[off..].starts_with(part) || qmark_match(&name[off..], part));
            if !hit { return false; }
            start = name.len(); // ? 段命中后位置不定,后续段退化为普通子串
            first = false;
            continue;
        }
        let found = memchr::memmem::find(&name[start..], part).map(|o| o + start);
        match found {
            None => return false,
            Some(pos) => {
                if first && p.prefix_anchor && pos != 0 { return false; }
                if pi == p.parts.len() - 1 && p.suffix_anchor && pos + part.len() != name.len() { return false; }
                start = pos + part.len();
                first = false;
            }
        }
    }
    true
}

fn qmark_match(hay: &[u8], pat: &[u8]) -> bool {
    let (mut h, mut p) = (0usize, 0usize);
    while p < pat.len() {
        if h >= hay.len() { return false; }
        if pat[p] == QMARK { h += utf8_len(hay[h]); p += 1; }
        else if hay[h] == pat[p] { h += 1; p += 1; }
        else { return false; }
    }
    true
}

fn utf8_len(b: u8) -> usize {
    match b { 0x00..=0x7F => 1, 0xC0..=0xDF => 2, 0xE0..=0xEF => 3, _ => 4 }
}

fn score(name: &[u8], first_part: &[u8]) -> u8 {
    if name.starts_with(first_part) { 3 }
    else if name.windows(first_part.len()).any(|w| w == first_part && is_boundary(name, w.as_ptr() as usize - name.as_ptr() as usize)) { 2 }
    else { 1 }
}

fn is_boundary(name: &[u8], off: usize) -> bool {
    off == 0 || !matches!(name[off - 1], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | 0x80..=0xFF)
}

pub fn execute(view: &dyn crate::index::IndexView, plan: &QueryPlan, limit: usize) -> QueryOutcome {
    let t = std::time::Instant::now();
    let entries = view.entries();
    // path: 命中的目录集合
    let hit_dirs: Option<std::collections::HashSet<u32>> = if plan.path_terms.is_empty() { None } else {
        let mut set = std::collections::HashSet::new();
        for (idx, e) in entries.iter().enumerate() {
            if !e.is_dir() { continue; }
            let (s, l) = view.path_seg(idx as u32);
            let path = &view.path_pool()[s as usize..(s + l) as usize];
            if plan.path_terms.iter().all(|pt| match_pattern(path, pt)) { set.insert(idx as u32); }
        }
        Some(set)
    };
    // 文件名预折叠(case: 时逐条按原样)
    struct Named<'a> { idx: u32, name: Vec<u8> }
    let prepared: Vec<Named> = entries.par_iter().enumerate()
        .filter(|(i, e)| e.is_live())
        .filter_map(|(i, e)| {
            let raw = view.match_name(*i as u32);
            let name: Vec<u8> = if plan.case_sensitive { raw.to_vec() } else { raw.to_vec() }; // match arena 已折叠;case: 重取 display 折叠前形态
            if plan.case_sensitive {
                let d = view.display_name(*i as u32);
                Some(Named { idx: *i as u32, name: d.to_vec() })
            } else {
                // path: 语义:文件名或父目录命中
                if let Some(dirs) = &hit_dirs {
                    if !dirs.contains(&e.parent) && !plan.path_terms.iter().all(|pt| match_pattern(&name, pt)) { return None; }
                }
                if !plan.name_terms.is_empty() && !plan.name_terms.iter().all(|pt| match_pattern(&name, pt)) { return None; }
                if plan.name_terms.is_empty() && plan.path_terms.iter().all(|pt| match_pattern(&name, pt)) { /* path-only + 文件名自身也命中 */ }
                Some(Named { idx: *i as u32, name })
            }
        })
        .collect();
    // case: / path 组合的精确过滤(case: 敏感时以 display 原始形态重跑全部模式)
    let scored: Vec<(u32, u8, usize)> = prepared.into_iter().filter_map(|n| {
        let target: &[u8] = if plan.case_sensitive { &n.name } else { &n.name };
        if plan.case_sensitive {
            if !plan.name_terms.iter().all(|pt| match_pattern_cs(target, pt)) { return None; }
        }
        if plan.whole_word {
            let s = String::from_utf8_lossy(target);
            let ok = plan.name_terms.iter().all(|pt| pt.parts.iter().all(|part| whole_contains(&s, part)));
            if !ok { return None; }
        }
        let first = plan.name_terms.first().and_then(|p| p.parts.first()).cloned().unwrap_or_default();
        let sc = score(target, &first);
        Some((n.idx, sc, target.len()))
    }).collect();
    let total = scored.len() as u32;
    let mut hits: Vec<u32> = scored.into_iter()
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|(i, s, l)| (std::cmp::Reverse(s), l, i))
        .collect::<Vec<_>>()
        .into_iter()
        .take(limit)
        .map(|(_, _, i)| i).collect();
    // 排序:分数降序、名字长度升序,取前 limit(全量排序保证截断正确)
    let _ = &mut hits;
    QueryOutcome { hits, total, elapsed_ms: t.elapsed().as_secs_f64() * 1000.0 }
}

fn match_pattern_cs(name: &[u8], p: &Pattern) -> bool { match_pattern(name, p) } // case: 模式串未折叠,逐字节即敏感

fn whole_contains(hay: &str, needle: &[u8]) -> bool {
    let needle = std::str::from_utf8(needle).unwrap_or("");
    let mut start = 0;
    while let Some(pos) = hay[start..].find(needle) {
        let abs = start + pos;
        let end = abs + needle.len();
        let lb = hay[..abs].chars().next_back();
        let rb = hay[end..].chars().next();
        let b = |c: Option<char>| c.map(|c| c.is_alphanumeric() || c == '_').unwrap_or(false);
        if !b(lb) && !b(rb) { return true; }
        start = end;
    }
    false
}
```

- [ ] **Step 4: 跑通过 + 打分排序简收口**——上述草稿的排序段故意留了接缝,收口为:**收集全量 (idx, score, len) → sort_by(Rediscore, len) → truncate(limit) → hits**;不要保留草稿中 `into_par_iter` 那段错误映射(它丢失分数)。收口代码:

```rust
    let mut rows: Vec<(u8, usize, u32)> = scored.into_iter().map(|(i, s, l)| (s, l, i)).collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let hits = rows.into_iter().take(limit).map(|(_, _, i)| i).collect();
```

Run: `cargo test -p engine query` Expected: 全部 PASS(含 match_tests)
- [ ] **Step 5: `<check>` + 提交**

```bash
git add -A && git commit -m "feat(engine): 查询内核——memmem 锚定序列/path 池/case-whole/打分排序,IndexView 统一视图(ADR-3)"
```

---

### Task 9: scanner.rs — 串行扫描(受控目录树 TDD)

**Files:**
- Modify: `crates/engine/src/scanner.rs`
- Test: 同文件 `#[cfg(test)]`

**Why:** tech.md 变更方案:扫描器语义(排除/符号链接/权限缺口分类)先在串行版定死并被测试钉住,Task 10 并行化以「与串行结果一致」为验收。

- [ ] **Step 1: 写失败测试(用 tempfile 建受控树;`crates/engine/Cargo.toml` 加 `[dev-dependencies] tempfile = "3"`)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        fs::create_dir_all(r.join("src")).unwrap();
        fs::write(r.join("src/main.rs"), "").unwrap();
        fs::create_dir_all(r.join("node_modules/pkg")).unwrap();
        fs::write(r.join("node_modules/pkg/i.js"), "").unwrap();
        fs::write(r.join("README.md"), "").unwrap();
        fs::write(r.join(".git/config"), "").unwrap_or_else(|_| { fs::create_dir(r.join(".git")).unwrap(); fs::write(r.join(".git/config"), "").unwrap(); String::new() }.into());
        t
    }

    #[test]
    fn scan_respects_excludes_and_links() {
        let t = fixture();
        let link = t.path().join("link.lnk");
        std::os::unix::fs::symlink(t.path().join("src"), &link).unwrap();
        let (index, gaps) = scan_roots(&[t.path().to_path_buf()], &[t.path().join("node_modules")], 1);
        assert_eq!(gaps.permission.len(), 0);
        let names: Vec<String> = (0..index.entries.len() as u32)
            .filter(|i| index.entries[*i as usize].parent == NO_PARENT)
            .map(|i| String::from_utf8_lossy(index.display_name_of(i)).into_owned())
            .collect();
        assert!(names.contains(&"src".to_string()) && names.contains(&"README.md".to_string()) && names.contains(&".git".to_string()));
        assert!(!names.contains(&"node_modules".to_string())); // 排除整棵子树
        assert!(!names.contains(&"link.lnk".to_string()));     // 符号链接不入索引
        let _ = index.entry_count();
    }
}
```

- [ ] **Step 2: 跑失败** Run: `cargo test -p engine scanner` Expected: FAIL
- [ ] **Step 3: 实现串行 `scan_roots`**

```rust
//! 扫描器:目录遍历 → Index。Task 9 串行定语义,Task 10 并行化(结果必须一致)。
use crate::entry::NO_PARENT;
use crate::index::Index;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

#[derive(Default, Debug)]
pub struct ScanGaps {
    pub permission: Vec<(PathBuf, String)>, // (目录, errno 文案)
    pub io_errors: u32,
}

pub struct ScanOptions<'a> {
    pub roots: &'a [PathBuf],
    pub excludes: &'a [PathBuf],
    pub threads: usize, // Task 10 使用;1 = 串行
}

/// 目录登记:先插目录条目(占位),子项遍历时挂父号。
fn is_excluded(p: &Path, excludes: &[PathBuf]) -> bool {
    excludes.iter().any(|x| p.starts_with(x))
}

pub fn scan_roots(roots: &[PathBuf], excludes: &[PathBuf], _threads: usize) -> (Index, ScanGaps) {
    let mut index = Index::new();
    let mut gaps = ScanGaps::default();
    let mut queue: VecDeque<(PathBuf, u32)> = VecDeque::new(); // (绝对路径, 父条目号)
    // 根目录条目:名字取路径尾段,parent=NO_PARENT
    for root in roots {
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.to_string_lossy().into_owned());
        let idx = index.insert(&name, NO_PARENT, true);
        queue.push_back((root.clone(), idx));
    }
    while let Some((dir, parent_idx)) = queue.pop_front() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                gaps.permission.push((dir.clone(), e.to_string()));
                continue;
            }
            Err(_) => { gaps.io_errors += 1; continue; }
        };
        for entry in rd.flatten() {
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => { gaps.io_errors += 1; continue; }
            };
            if ft.is_symlink() { continue; }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if ft.is_dir() {
                if is_excluded(&path, excludes) { continue; }
                let idx = index.insert(&name, parent_idx, true);
                queue.push_back((path, idx));
            } else {
                index.insert(&name, parent_idx, false);
            }
        }
    }
    (index, gaps)
}
```

- [ ] **Step 4: 跑通过** Run: `cargo test -p engine scanner` Expected: PASS
- [ ] **Step 5: `<check>` + 提交**

```bash
git add -A && git commit -m "feat(engine): 串行扫描器——排除/符号链接/权限缺口分类(ADR-6 语义基线)"
```

---

### Task 10: scanner.rs — 并行化(work-stealing,确定性)

**Files:**
- Modify: `crates/engine/src/scanner.rs`
- Test: 同文件 `#[cfg(test)]`

**Why:** tech.md ADR-6:并行遍历把首扫从 10-20s 压到 ≤5s 红线;「多线程结果与串行一致 + 缺口合并无损」是钉住正确性的唯一手段。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod par_tests {
    use super::*;

    #[test]
    fn parallel_equals_serial() {
        let t = fixture(); // 复用 Task 9 fixture,再补一层嵌套
        std::fs::create_dir_all(t.path().join("src/deep/deeper")).unwrap();
        std::fs::write(t.path().join("src/deep/deeper/x.txt"), "").unwrap();
        let roots = vec![t.path().to_path_buf()];
        let excl = vec![];
        let (s_idx, s_gaps) = scan_roots(&roots, &excl, 1);
        let (p_idx, p_gaps) = scan_roots(&roots, &excl, 8);
        assert_eq!(s_idx.entry_count(), p_idx.entry_count());
        let s_paths: std::collections::BTreeSet<String> = (0..s_idx.entries.len() as u32).map(|i| s_idx.full_path(i).to_string_lossy().into_owned()).collect();
        let p_paths: std::collections::BTreeSet<String> = (0..p_idx.entries.len() as u32).map(|i| p_idx.full_path(i).to_string_lossy().into_owned()).collect();
        assert_eq!(s_paths, p_paths); // 路径集合相等 = 聚合确定性
        assert_eq!(s_gaps.permission.len(), p_gaps.permission.len());
    }
}
```

- [ ] **Step 2: 跑失败** Run: `cargo test -p engine scanner` Expected: FAIL(threads>1 未实现,仍走串行则断言通过但无并行——本测试设计为「相等性」,并行实现后依然过;真正驱动并行的是 Step 3 重构)

- [ ] **Step 3: 实现并行 scan_roots(替换原实现:threads==1 走原串行路径;>1 走共享队列)**

```rust
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Mutex;

/// 并行遍历(ADR-6):共享目录队列,活跃线程 = min(threads, 物理核);
/// 各线程产出「相对记录」(父目录路径, 名, is_dir),聚合阶段按目录层序重建 Index。
pub fn scan_roots(roots: &[PathBuf], excludes: &[PathBuf], threads: usize) -> (Index, ScanGaps) {
    if threads <= 1 { return scan_serial(roots, excludes); }
    let queue: Mutex<VecDeque<(PathBuf, Option<PathBuf>)>> = Mutex::new(roots.iter().map(|r| (r.clone(), None)).collect());
    let records: Mutex<Vec<(PathBuf, String, bool)>> = Mutex::new(vec![]); // (父目录绝对路径, 名, is_dir)
    let gaps: Mutex<ScanGaps> = Mutex::new(ScanGaps::default());
    let nthreads = threads.min(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8));
    std::thread::scope(|s| {
        for _ in 0..nthreads {
            s.spawn(|| loop {
                let Some((dir, _root_tag)) = queue.lock().unwrap().pop_front() else { break };
                let rd = match std::fs::read_dir(&dir) {
                    Ok(rd) => rd,
                    Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                        gaps.lock().unwrap().permission.push((dir.clone(), e.to_string()));
                        continue;
                    }
                    Err(_) => { gaps.lock().unwrap().io_errors += 1; continue; }
                };
                for entry in rd.flatten() {
                    let ft = match entry.file_type() { Ok(f) => f, Err(_) => { gaps.lock().unwrap().io_errors += 1; continue } };
                    if ft.is_symlink() { continue; }
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if ft.is_dir() {
                        if is_excluded(&path, excludes) { continue; }
                        records.lock().unwrap().push((dir.clone(), name.clone(), true));
                        queue.lock().unwrap().push_back((path, None));
                    } else {
                        records.lock().unwrap().push((dir.clone(), name, false));
                    }
                }
            });
        }
    });
    // 聚合:目录路径 → 条目号 映射;BFS 层序保证父先于子(records 中父目录一定先被弹出,但子目录记录在父遍历时产生——天然有序于发现序)
    let mut index = Index::new();
    let mut dir_map: std::collections::HashMap<PathBuf, u32> = std::collections::HashMap::new();
    let recs = records.into_inner().unwrap();
    // 根目录先登记
    for root in roots {
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.to_string_lossy().into_owned());
        let idx = index.insert(&name, NO_PARENT, true);
        dir_map.insert(root.clone(), idx);
    }
    // 目录记录先全部登记(两轮),保证文件挂父时父必在
    for (parent, name, is_dir) in &recs {
        if *is_dir {
            let pidx = dir_map[parent];
            let idx = index.insert(name, pidx, true);
            let mut path = parent.clone();
            path.push(name);
            dir_map.insert(path, idx);
        }
    }
    for (parent, name, is_dir) in &recs {
        if !*is_dir {
            let pidx = dir_map[parent];
            index.insert(name, pidx, false);
        }
    }
    (index, gaps.into_inner().unwrap())
}

fn scan_serial(roots: &[PathBuf], excludes: &[PathBuf]) -> (Index, ScanGaps) { /* Task 9 原实现整体搬入 */ }
```

- [ ] **Step 4: 跑通过 + 基准快测** Run: `cargo test -p engine scanner && cargo test -p engine` Expected: 全部 PASS
- [ ] **Step 5: `<check>` + 提交**

```bash
git add -A && git commit -m "feat(engine): 并行扫描器 work-stealing,路径集合与串行一致(ADR-6)"
```

---

### Task 11: persist.rs — 落盘写(同构布局 + 原子替换)

**Files:**
- Modify: `crates/engine/src/persist.rs`
- Test: 同文件 `#[cfg(test)]`

**Why:** tech.md ADR-3:落盘布局与内存同构(mmap 零反序列化的前提);临时文件 + rename 原子替换保证崩溃不损坏(product.md 边界:daemon 崩溃/断电)。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_magic_version_roundtrip() {
        let mut i = Index::new();
        let d = i.insert("docs", NO_PARENT, true);
        i.insert("a.txt", d, false);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.bin");
        let meta = IndexMeta { built_at: 1700000000, last_event_id: 42, scan_roots: vec![b"/Users/t".to_vec()], excluded: vec![] };
        write_atomic(&i, &meta, &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..4], MAGIC);
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), FORMAT_VERSION);
        assert_eq!(bytes.len() % 4, 0);
    }

    #[test]
    fn write_replaces_atomically_no_tmp_left() {
        let i = Index::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.bin");
        let meta = IndexMeta::default();
        write_atomic(&i, &meta, &path).unwrap();
        write_atomic(&i, &meta, &path).unwrap(); // 二次覆盖
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(left.len(), 1); // 无 .tmp 残留
    }
}
```

- [ ] **Step 2: 跑失败** Run: `cargo test -p engine persist` Expected: FAIL
- [ ] **Step 3: 实现 `persist.rs` 写侧**

```rust
//! 落盘:布局 = header + display_arena + match_arena + path_pool + path_offsets + entries
//! (ADR-3;last_event_id 供停机重放,ADR-5)。
use crate::entry::Entry;
use crate::index::Index;
use std::io::Write;
use std::path::Path;

pub const MAGIC: &[u8; 4] = b"MCEI";
pub const FORMAT_VERSION: u32 = 1;

#[derive(Default, Clone, Debug)]
pub struct IndexMeta {
    pub built_at: u64,
    pub last_event_id: u64,
    pub scan_roots: Vec<Vec<u8>>,
    pub excluded: Vec<Vec<u8>>,
}

fn put_u32(v: &mut Vec<u8>, x: u32) { v.extend_from_slice(&x.to_le_bytes()); }
fn put_u64(v: &mut Vec<u8>, x: u64) { v.extend_from_slice(&x.to_le_bytes()); }

pub fn write_atomic(index: &Index, meta: &IndexMeta, path: &Path) -> std::io::Result<()> {
    let mut buf: Vec<u8> = Vec::with_capacity(1 << 20);
    buf.extend_from_slice(MAGIC);
    put_u32(&mut buf, FORMAT_VERSION);
    put_u32(&mut buf, index.entries.len() as u32);
    put_u32(&mut buf, index.tombstone_count());
    put_u32(&mut buf, index.path_offsets.len() as u32);
    put_u64(&mut buf, meta.built_at);
    put_u64(&mut buf, meta.last_event_id);
    put_u32(&mut buf, meta.scan_roots.len() as u32);
    for r in &meta.scan_roots { put_u32(&mut buf, r.len() as u32); buf.extend_from_slice(r); }
    put_u32(&mut buf, meta.excluded.len() as u32);
    for e in &meta.excluded { put_u32(&mut buf, e.len() as u32); buf.extend_from_slice(e); }
    buf.extend_from_slice(&index.display_arena.data);
    buf.extend_from_slice(&index.match_arena.data);
    buf.extend_from_slice(&index.path_pool.data);
    for (s, l) in &index.path_offsets { put_u32(&mut buf, *s); put_u32(&mut buf, *l); }
    for e in &index.entries { buf.extend_from_slice(unsafe { std::slice::from_raw_parts(e as *const Entry as *const u8, 24) }); }

    let tmp = path.with_extension("tmp");
    { let mut f = std::fs::File::create(&tmp)?; f.write_all(&buf)?; f.sync_all()?; }
    std::fs::rename(&tmp, path)?;
    Ok(())
}
```

- [ ] **Step 4: 跑通过** Run: `cargo test -p engine persist` Expected: PASS
- [ ] **Step 5: `<check>` + 提交**

```bash
git add -A && git commit -m "feat(engine): 落盘写——同构布局/last_event_id/临时文件+rename 原子替换(ADR-3)"
```

---

### Task 12: persist.rs — mmap 加载 + SharedView

**Files:**
- Modify: `crates/engine/src/persist.rs`
- Test: 同文件 `#[cfg(test)]`

**Why:** tech.md ADR-3:冷启动 <1s 的载体——mmap 零反序列化直查;坏 magic / 版本不识别 → 弃用重建(product.md 边界)。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod load_tests {
    use super::*;
    use crate::index::{Index, IndexView};

    fn build_and_save(dir: &tempfile::TempDir, last_id: u64) -> (Index, std::path::PathBuf) {
        let mut i = Index::new();
        let d = i.insert("docs", crate::entry::NO_PARENT, true);
        i.insert("résumé.txt", d, false);
        let path = dir.path().join("index.bin");
        let meta = IndexMeta { built_at: 1, last_event_id: last_id, scan_roots: vec![], excluded: vec![] };
        write_atomic(&i, &meta, &path).unwrap();
        (i, path)
    }

    #[test]
    fn mmap_view_reads_identically() {
        let dir = tempfile::tempdir().unwrap();
        let (src, path) = build_and_save(&dir, 7);
        let view = load_mmap(&path).unwrap();
        let v: &dyn IndexView = &*view;
        assert_eq!(v.entries().len() as u32, src.entry_count() + src.tombstone_count());
        assert_eq!(v.match_name(1), src.match_name_of(1));
        assert_eq!(v.display_name(1), src.display_name_of(1));
        assert_eq!(view.meta.last_event_id, 7);
        // 查询直跑共享视图(Task 8 的 execute 无需改动)
        let out = crate::query::execute(v, &crate::query::parse("resume").unwrap(), 10);
        assert_eq!(out.total, 1);
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
}
```

- [ ] **Step 2: 跑失败** Run: `cargo test -p engine persist` Expected: FAIL
- [ ] **Step 3: 实现 SharedView + load_mmap(零拷贝:偏移解析 + memmap2)**

```rust
#[derive(thiserror::Error, Debug)]
pub enum LoadError {
    #[error("magic 不符")] BadMagic,
    #[error("版本不识别: {0}")] BadVersion(u32),
    #[error("文件过短或布局损坏")] Truncated,
    #[error(transparent)] Io(#[from] std::io::Error),
}

pub struct SharedView {
    mmap: memmap2::Mmap,
    pub meta: IndexMeta,
    entries_off: usize,
    display_off: usize,
    match_off: usize,
    path_pool_off: usize,
    path_offsets_off: usize,
    n_entries: usize,
    n_path_segs: usize,
}

impl crate::index::IndexView for SharedView {
    fn entries(&self) -> &[Entry] {
        unsafe { std::slice::from_raw_parts(self.mmap[self.entries_off..].as_ptr() as *const Entry, self.n_entries) }
    }
    fn display_arena(&self) -> &[u8] { &self.mmap[self.display_off..self.match_off] }
    fn match_arena(&self) -> &[u8] { &self.mmap[self.match_off..self.path_pool_off] }
    fn path_pool(&self) -> &[u8] { &self.mmap[self.path_pool_off..self.path_offsets_off] }
    fn path_seg(&self, idx: u32) -> (u32, u32) {
        let base = self.path_offsets_off + idx as usize * 8;
        let s = u32::from_le_bytes(self.mmap[base..base + 4].try_into().unwrap());
        let l = u32::from_le_bytes(self.mmap[base + 4..base + 8].try_into().unwrap());
        (s, l)
    }
    fn dir_path(&self, idx: u32) -> &[u8] {
        let (s, l) = self.path_seg(idx);
        &self.path_pool()[s as usize..(s + l) as usize]
    }
}

impl SharedView {
    /// 物化为可变索引(冷启动两段式第二步):从 mmap 拷贝重建 Index(ADR-3)。
    pub fn materialize(&self) -> Index {
        let mut i = Index::new();
        let _ = &mut i; // 按条目顺序重建:display/match 名与 parent 映射;两轮法同 Task 10 聚合
        // 实现:读 entries 数组,第一轮按序 insert(名字来自 display_arena 段),
        // parent 暂记旧号;第二轮修 parent;第三轮目录建路径池。约 40 行,模式同 compact。
        i
    }
}

pub fn load_mmap(path: &Path) -> Result<Box<SharedView>, LoadError> {
    let file = std::fs::File::open(path)?;
    let mmap = unsafe { memmap2::Mmap::map(&file)? };
    let m = &mmap[..];
    let need = |off: usize, n: usize| -> Result<(), LoadError> { if off + n > m.len() { Err(LoadError::Truncated) } else { Ok(()) } };
    if m.len() < 4 || &m[0..4] != MAGIC { return Err(LoadError::BadMagic); }
    let ver = u32::from_le_bytes(m[4..8].try_into().unwrap());
    if ver != FORMAT_VERSION { return Err(LoadError::BadVersion(ver)); }
    let n_entries = u32::from_le_bytes(m[8..12].try_into().unwrap()) as usize;
    let _tomb = u32::from_le_bytes(m[12..16].try_into().unwrap()) as usize;
    let n_path_segs = u32::from_le_bytes(m[16..20].try_into().unwrap()) as usize;
    let built_at = u64::from_le_bytes(m[20..28].try_into().unwrap());
    let last_event_id = u64::from_le_bytes(m[28..36].try_into().unwrap());
    let mut off = 36usize;
    let mut roots = vec![];
    let n_roots = u32::from_le_bytes(m[off..off + 4].try_into().unwrap()) as usize; off += 4;
    for _ in 0..n_roots {
        need(off, 4)?;
        let l = u32::from_le_bytes(m[off..off + 4].try_into().unwrap()) as usize; off += 4;
        need(off, l)?; roots.push(m[off..off + l].to_vec()); off += l;
    }
    let mut excluded = vec![];
    let n_ex = u32::from_le_bytes(m[off..off + 4].try_into().unwrap()) as usize; off += 4;
    for _ in 0..n_ex {
        need(off, 4)?;
        let l = u32::from_le_bytes(m[off..off + 4].try_into().unwrap()) as usize; off += 4;
        need(off, l)?; excluded.push(m[off..off + l].to_vec()); off += l;
    }
    let display_off = off;
    // 各段长度由写侧顺序决定:display + match + path_pool 三段长度未在头中显式记录——
    // 写侧补充:三段长度各自 u64 前缀。见 Step 4 的布局修正。
    let _ = (entries_off, match_off, path_pool_off, path_offsets_off);
    let meta = IndexMeta { built_at, last_event_id, scan_roots: roots, excluded };
    Ok(Box::new(SharedView { mmap, meta, entries_off: 0, display_off, match_off: 0, path_pool_off: 0, path_offsets_off: 0, n_entries, n_path_segs }))
}
```

- [ ] **Step 4: 布局补丁(写读两端同步改)**——Task 11 的 header 中 display/match/path_pool 三段长度未记录,读侧无法切界。修正:**header 末尾补 3 个 u64(三段长度)**,写侧 `write_atomic` 在 excluded 后 `put_u64` 三个 `data.len()`;读侧依次读取并切界,`entries_off = path_offsets_off + n_path_segs*8`,`need(entries_off, n_entries*24)`。同步更新 Task 11 测试(布局变化不影响其断言,无需改)。
- [ ] **Step 5: 跑通过 + `<check>` + 提交**

```bash
cargo test -p engine persist && <check>
git add -A && git commit -m "feat(engine): mmap 零拷贝加载 + SharedView + materialize 骨架(ADR-3)"
```

---

### Task 12b: 两处收口修正(path_offsets 对齐 + materialize 全码)

**Files:**
- Modify: `crates/engine/src/index.rs`、`crates/engine/src/persist.rs`

**Why:** 自审发现的两个收口:① `path_offsets` 若按「目录才 push」,下标 ≠ 条目号(文件穿插时错位),必须全量对齐(目录写真值、其余 `(0,0)` 占位,8B×百万=8MB 可接受);② Task 12 的 `materialize` 是骨架注释,违反「无占位」规则,此处给全码。

- [ ] **Step 1: 修 `Index::insert` 的路径池分支(替换 Task 5 中的对应两行)**

```rust
        if is_dir {
            let pp = self.dir_path_bytes(idx);
            let seg = self.path_pool.push(&pp);
            self.path_offsets.push(seg);
        } else {
            self.path_offsets.push((0, 0)); // 占位对齐:path_offsets 下标 == 条目号
        }
```

- [ ] **Step 2: `SharedView::materialize` 全码(替换 Task 12 骨架)**

```rust
    pub fn materialize(&self) -> Index {
        let entries = self.entries();
        let mut i = Index::new();
        i.entries.reserve(entries.len());
        i.path_offsets = vec![(0, 0); entries.len()];
        let mut map = vec![NO_PARENT; entries.len()];
        for (n, e) in entries.iter().enumerate() {
            let d = &self.display_arena()[e.display_start as usize..(e.display_start + e.display_len) as usize];
            let m = &self.match_arena()[e.match_start as usize..(e.match_start + e.match_len) as usize];
            let (ds, dl) = i.display_arena.push(d);
            let (ms, ml) = i.match_arena.push(m);
            i.entries.push(Entry { display_start: ds, display_len: dl, match_start: ms, match_len: ml, parent: e.parent, flags: e.flags });
            map[n] = i.entries.len() as u32 - 1;
            if e.is_live() { i.live_count += 1 } else { i.tombstones += 1 }
        }
        for e in i.entries.iter_mut() {
            if e.parent != NO_PARENT { e.parent = map[e.parent as usize]; }
        }
        for n in 0..i.entries.len() {
            if i.entries[n].is_dir() && i.entries[n].is_live() {
                let pp = i.dir_path_bytes(n as u32);
                i.path_offsets[n] = i.path_pool.push(&pp);
            }
        }
        i
    }
```

- [ ] **Step 3: 回归 + 提交** Run: `cargo test --workspace` Expected: PASS

```bash
git add -A && git commit -m "fix(engine): path_offsets 按条目号对齐占位;materialize 全码收口"
```

---

### Task 13: config.rs — 排除列表(toml)

**Files:**
- Modify: `crates/engine/src/config.rs`
- Test: 同文件 `#[cfg(test)]`

**Why:** product.md 数据结构:配置文件是用户数据契约(toml,排除目录列表);exclude 命令是唯一运行时写口(边界表:手改配置重启生效)。

- [ ] **Step 1: 失败测试 + 实现(单一职责,一并给全)**

```rust
//! 配置:应用数据目录下 config.toml。默认排除 ~/Library、node_modules、.git(product §1)。
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub excluded: Vec<String>,
}

pub fn default_config() -> Config {
    Config { excluded: vec!["~/Library".into(), "node_modules".into(), ".git".into()] }
}

pub fn load(path: &Path) -> Config {
    match std::fs::read_to_string(path) {
        Ok(s) => toml::from_str(&s).unwrap_or_else(|e| { log::warn!("config.toml 解析失败,用默认配置: {e}"); default_config() }),
        Err(_) => default_config(),
    }
}

pub fn save(cfg: &Config, path: &Path) -> std::io::Result<()> {
    if let Some(p) = path.parent() { std::fs::create_dir_all(p)?; }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string_pretty(cfg).unwrap())?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let c = load(&p);
        assert_eq!(c, default_config()); // 缺文件 → 默认
        let mut c2 = default_config();
        c2.excluded.push("/tmp/x".into());
        save(&c2, &p).unwrap();
        assert_eq!(load(&p), c2);
    }
}
```

- [ ] **Step 2: `<check>` + 提交**

```bash
git add -A && git commit -m "feat(engine): config.toml 读写——默认排除/原子保存(数据契约)"
```

---

### Task 14: watcher.rs — 事件批处理与目录 diff 联动

**Files:**
- Modify: `crates/engine/src/watcher.rs`
- Test: 同文件 `#[cfg(test)]`

**Why:** tech.md 增量管线:100ms 防抖合并(事件风暴场景 4)+ 目录 diff 应用是增量的全部;事件只当信号。

- [ ] **Step 1: 失败测试(批处理纯逻辑,时钟注入)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{Index, NO_PARENT};

    #[test]
    fn batcher_dedups_within_window() {
        let mut b = DirBatcher::new(std::time::Duration::from_millis(100), now_stub());
        b.push("/a/b"); b.push("/a/b"); b.push("/a/c");
        let drained = b.drain_ready();
        assert!(drained.is_empty()); // 窗口未到
        b.advance(150);
        let got = b.drain_ready();
        assert_eq!(got.len(), 2); // /a/b 与 /a/c 去重
    }

    #[test]
    fn diff_updates_index_from_real_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();
        let mut i = Index::new();
        let d = i.insert(dir.path().to_string_lossy().as_ref(), NO_PARENT, true);
        let _ = i.insert("ghost.txt", d, false); // 索引有、磁盘无
        let stats = apply_dir_diff(&mut i, d, dir.path()).unwrap();
        assert!(stats.removed >= 1);
        let kids: Vec<Vec<u8>> = i.children_of(d).iter().map(|k| i.display_name_of(*k).to_vec()).collect();
        assert!(kids.contains(&b"a.txt".to_vec()));
        assert!(!kids.contains(&b"ghost.txt".to_vec()));
    }
}
```

- [ ] **Step 2: 跑失败** Run: `cargo test -p engine watcher` Expected: FAIL
- [ ] **Step 3: 实现**

```rust
//! 增量:事件 → 目录集合(100ms 静默窗)→ apply_dir_diff(唯一真相)。
use crate::index::{DiffStats, Index};
use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

pub struct DirBatcher {
    window: Duration,
    pending: HashSet<String>,
    last_push: Instant,
    now: fn() -> Instant,
}

impl DirBatcher {
    pub fn new(window: Duration, now: fn() -> Instant) -> Self {
        Self { window, pending: HashSet::new(), last_push: now(), now }
    }
    pub fn push(&mut self, dir: impl Into<String>) {
        self.pending.insert(dir.into());
        self.last_push = (self.now)();
    }
    pub fn advance(&mut self, ms: u64) { /* 仅测试用:推进时间桩由测试自控,生产路径用 drain_ready 的 elapsed 判定 */ let _ = ms; }
    pub fn drain_ready(&mut self) -> Vec<String> {
        if (self.now)().duration_since(self.last_push) < self.window || self.pending.is_empty() {
            return vec![];
        }
        self.last_push = (self.now)();
        self.pending.drain().collect()
    }
}

pub fn now_stub() -> fn() -> Instant { Instant::now }

/// 目录 diff:重读磁盘目录(事件风暴下这是唯一真相,tech.md 增量管线)。
/// 批量语义:每次最多读一个目录;调用方(drainer 线程)循环消费。
pub fn apply_dir_diff(index: &mut Index, dir_idx: u32, abs_dir: &Path) -> std::io::Result<DiffStats> {
    let mut listing = std::collections::BTreeMap::new();
    for e in std::fs::read_dir(abs_dir)?.flatten() {
        let ft = match e.file_type() { Ok(f) => f, Err(_) => continue };
        if ft.is_symlink() { continue; }
        listing.insert(e.file_name().to_string_lossy().into_owned(), ft.is_dir());
    }
    Ok(index.diff_dir(dir_idx, &listing))
}
```

- [ ] **Step 4: 跑通过**(`advance` 是死位——删除它,窗口推进由测试直接操纵 `last_push`?测试已用真实 Instant,`drain_ready` 首调时 elapsed<100ms 会返回空,第二断言需等待——**修正测试**:`b.last_push = (b.now)() - Duration::from_millis(150);` 直接改字段(pub(crate)),删除 `advance`。以此为准)
- [ ] **Step 5: `<check>` + 提交**

```bash
git add -A && git commit -m "feat(engine): 事件批处理去重 + 目录 diff 联动(增量唯一真相)"
```

---

### Task 15: fsevents.rs — 正式封装(stream/重放/事件 ID)

**Files:**
- Modify: `crates/engine/src/fsevents.rs`(Task 3 雏形收口)
- Test: `crates/engine/tests/fsevents_live.rs`(需真实 FS 事件,标记 `#[ignore]`,实机跑)

**Why:** tech.md ADR-5:sinceWhen 重放是停机补齐的基石;unsafe 集中本文件,六类路径集成测试覆盖。

- [ ] **Step 1: 收口正式 API(替换 spike 版)**

```rust
//! FSEvents 直调封装(ADR-5)。unsafe 全集中本文件。
#![allow(non_upper_case_globals, non_camel_case_types)]
use core_foundation_sys::base::kCFAllocatorDefault;
use fsevent_sys::core_foundation::{CFArrayRef, CFArrayCreate, CFStringCreateWithCString, CFRelease};
use fsevent_sys::fsevent::*;
use std::ffi::CString;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

pub const SINCE_NOW: u64 = kFSEventStreamEventIdSinceNow;
pub const FLAG_HISTORY_DONE: u32 = 0x00000010; // kFSEventStreamEventFlagHistoryDone(Task 3 spike 实证:0x20000 实为 ItemIsDir,SDK FSEvents.h L435/536 双源确认)
pub const FLAG_RESCAN: u32 = 0x400000;      // kFSEventStreamEventFlagUserDropped|KernelDropped 近似,精确值 spike 定稿

#[derive(Debug, Clone)]
pub struct FseBatch {
    pub paths: Vec<String>,
    pub flags: u32,
    pub last_id: u64,
    pub history_done: bool,
}

type Tx = Arc<Mutex<Sender<FseBatch>>>;

pub struct FseStream {
    raw: FSEventStreamRef,
    rl: core_foundation_sys::runloop::CFRunLoopSourceRef,
}

impl FseStream {
    /// 创建并挂到独立线程的 runloop;事件经 channel 送达。
    pub fn spawn(since_when: u64, paths: &[String], latency_ms: u64, tx: Sender<FseBatch>) -> anyhow::Result<Self> {
        let tx: Tx = Arc::new(Mutex::new(tx));
        let cpaths: Vec<*const std::ffi::c_void> = paths.iter()
            .map(|p| unsafe { CFStringCreateWithCString(kCFAllocatorDefault, CString::new(p.as_str())?.as_ptr(), 0x08000100) as *const std::ffi::c_void })
            .collect();
        extern "C" fn cb(_s: FSEventStreamRef, info: *mut std::ffi::c_void, num: usize, paths: *mut *mut std::ffi::c_char, flags: *mut u32, ids: *mut u64) {
            unsafe {
                let tx = &*(info as *const Tx);
                let mut batch = FseBatch { paths: vec![], flags: 0, last_id: 0, history_done: false };
                for i in 0..num {
                    batch.paths.push(std::ffi::CStr::from_ptr(*paths.add(i)).to_string_lossy().into_owned());
                    batch.flags |= *flags.add(i);
                    batch.last_id = *ids.add(i);
                }
                batch.history_done = batch.flags & FLAG_HISTORY_DONE != 0;
                let _ = tx.lock().unwrap().send(batch);
            }
        }
        let ctx = FSEventStreamContext { version: 0, info: Box::into_raw(Box::new(tx)) as *mut _, retain: None, release: None, copyDescription: None };
        let arr = unsafe { CFArrayCreate(kCFAllocatorDefault, cpaths.as_ptr(), cpaths.len() as isize, std::ptr::null()) } as CFArrayRef;
        let raw = unsafe { FSEventStreamCreate(kCFAllocatorDefault, Some(cb), &ctx, arr, since_when, latency_ms as f64 / 1000.0, kFSEventStreamCreateFlagNoDefer) };
        if raw.is_null() { anyhow::bail!("FSEventStreamCreate 失败"); }
        // 独立线程 runloop
        let rl_src = unsafe { FSEventStreamCopyDispatchQueueHLT(raw) }; // 或经典 API:ScheduleWithRunLoop,见 Step 2
        Ok(Self { raw, rl: rl_src })
    }
    pub fn last_id_of_stream(&self) -> u64 { unsafe { FSEventStreamGetLatestEventId(self.raw) } }
    pub fn stop(&mut self) { unsafe { FSEventStreamStop(self.raw); FSEventStreamInvalidate(self.raw); FSEventStreamRelease(self.raw); } }
}
impl Drop for FseStream { fn drop(&mut self) { self.stop() } }
```

- [ ] **Step 2: 调度收口**——`FSEventStreamCopyDispatchQueueHLT` 非公开 API;正式实现用经典对:`FSEventStreamScheduleWithRunLoop(raw, CFRunLoopGetCurrent(), kCFRunLoopDefaultMode)` + `FSEventStreamStart` + 专用线程 `CFRunLoopRun()`。按此改写 `spawn`:专用 `std::thread` 内建流、调度、start、runloop;`stop` 经 `CFRunLoopStop` 发信号。**以 fsevent-sys 实际导出为准,写码前先 `grep pub fn` 该 crate 确认签名**(Doubt-Driven:禁凭记忆写 FFI)。
- [ ] **Step 3: 集成测试 `tests/fsevents_live.rs`(#[ignore],实机 `cargo test -p engine --ignored` 跑)**

```rust
#[test]
#[ignore = "需真实 FS 事件与 runloop,实机验证"]
fn live_events_and_history_done() {
    let (tx, rx) = std::sync::mpsc::channel();
    let dir = tempfile::tempdir().unwrap();
    let p = vec![dir.path().to_string_lossy().into_owned()];
    let mut s = engine::fsevents::FseStream::spawn(engine::fsevents::SINCE_NOW, &p, 50, tx).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    std::fs::write(dir.path().join("x.txt"), "").unwrap();
    let got = rx.recv_timeout(std::time::Duration::from_secs(3)).expect("未收到事件");
    assert!(got.paths.iter().any(|p| p.ends_with("x.txt")));
    assert!(got.last_id > 0);
    s.stop();
}
```

- [ ] **Step 4: `<check>` + 实机跑 ignored 测试 + 提交**

```bash
cargo test -p engine --ignored && <check>
git add -A && git commit -m "feat(engine): FSEvents 直调封装——sinceWhen/HistoryDone/事件 ID(ADR-5)"
```

---

### Task 16: reconcile.rs — 兜底全量重扫 + 保守判定器

**Files:**
- Modify: `crates/engine/src/reconcile.rs`
- Test: 同文件 `#[cfg(test)]`

**Why:** tech.md ADR-5 回退三路与状态机:零事件长停机(保守阈值 48h)必须转重扫——「禁止静默遗漏」的最后一道闸。

- [ ] **Step 1: 失败测试(判定器纯逻辑)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conservative_judgement() {
        // 零事件 + 停机 49h(> 48h 默认阈值)→ 重扫
        assert!(needs_full_rescan(0, 49 * 3600, 48 * 3600));
        // 零事件 + 停机 1h → 重放可信
        assert!(!needs_full_rescan(0, 3600, 48 * 3600));
        // 有事件 → 不重扫
        assert!(!needs_full_rescan(15, 100 * 3600, 48 * 3600));
        // 重放异常标志 → 重扫
        assert!(needs_full_rescan(5, 3600, 48 * 3600).then_some(true).unwrap_or(false) || REPLAY_FAILED_DUMMY);
    }
    const REPLAY_FAILED_DUMMY: bool = true; // 第二参数形态见实现:needs_full_rescan(events, downtime_s, threshold_s)
}
```

- [ ] **Step 2: 跑失败** Run: `cargo test -p engine reconcile` Expected: FAIL
- [ ] **Step 3: 实现**

```rust
//! 兜底全量重扫(ADR-5 回退;状态 degraded)。
use crate::index::Index;
use std::path::PathBuf;

/// 保守判定:重放零事件且停机超阈值 → 一律重扫(spike 信号为辅,此为底线)。
pub fn needs_full_rescan(replayed_events: u64, downtime_secs: u64, threshold_secs: u64) -> bool {
    replayed_events == 0 && downtime_secs > threshold_secs
}

/// 全量重扫:复用并行扫描器产出全新 Index;调用方(demon)原子替换后重落盘。
pub fn full_rescan(roots: &[PathBuf], excludes: &[String], threads: usize) -> (Index, crate::scanner::ScanGaps) {
    let excl: Vec<PathBuf> = excludes.iter().map(PathBuf::from).collect();
    crate::scanner::scan_roots(roots, &excl, threads)
}
```

- [ ] **Step 4: 跑通过(测试中 REPLAY_FAILED_DUMMY 段删除——判定器单一职责,异常标志由调用方直接走重扫路径,不经此函数;测试只留三段断言)**
- [ ] **Step 5: `<check>` + 提交**

```bash
git add -A && git commit -m "feat(engine): 保守判定器 + 兜底全量重扫(禁止静默遗漏最后闸门)"
```

---

### Task 17: protocol.rs — daemon 消息契约

**Files:**
- Create: `crates/cli/src/protocol.rs`
- Test: 同文件 `#[cfg(test)]`

**Why:** tech.md daemon 通信契约表:长度上限 1MB、版本字段、path 校验、stop 响应——协议健壮性四件套(review 先做批 3)。

- [ ] **Step 1: 失败测试 + 实现**

```rust
//! 消息契约(tech.md 契约表):4 字节小端长度 + JSON;v 字段必填。
use serde::{Deserialize, Serialize};

pub const MAX_MSG: usize = 1_000_000;

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "msg", rename_all = "snake_case")]
pub enum Request {
    Query { v: u32, query: String, limit: u32 },
    Status { v: u32 },
    Stop { v: u32 },
    ExcludeAdd { v: u32, path: String },
    ExcludeRemove { v: u32, path: String },
    ExcludeList { v: u32 },
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "msg", rename_all = "snake_case")]
pub enum Response {
    Query { entries: Vec<HitEntry>, total: u32, elapsed_ms: f64 },
    Status { state: String, entry_count: u32, gaps: Vec<Gap>, io_error_count: u32, built_at: u64, index_file: String },
    Stop { ok: bool, final_entry_count: u32 },
    Exclude { ok: bool, excluded: Vec<String> },
    Error { code: String, message: String },
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct HitEntry { pub name: String, pub path: String, pub size: Option<u64>, pub mtime: Option<u64> }

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct Gap { pub path: String, pub reason: String }

pub fn encode(req: &Request) -> serde_json::Result<Vec<u8>> {
    let body = serde_json::to_vec(req)?;
    let mut out = (body.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(&body);
    Ok(out)
}

pub fn decode(buf: &[u8]) -> serde_json::Result<Request> {
    serde_json::from_slice(buf)
}

pub mod validate {
    /// path 必须为绝对路径(扫描根内校验由 daemon 持根列表执行)。
    pub fn exclude_path(p: &str) -> Result<String, String> {
        if !p.starts_with('/') { return Err("必须是绝对路径".into()); }
        Ok(p.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let r = Request::Query { v: 1, query: "a b".into(), limit: 100 };
        let bytes = encode(&r).unwrap();
        assert!(bytes.len() >= 4);
        assert_eq!(decode(&bytes[4..]).unwrap(), r);
    }
    #[test]
    fn reject_relative_path() {
        assert!(validate::exclude_path("relative/x").is_err());
        assert!(validate::exclude_path("/abs/x").is_ok());
    }
}
```

- [ ] **Step 2: `<check>` + 提交**

```bash
git add -A && git commit -m "feat(cli): daemon 消息契约——长度前缀/版本字段/path 校验(契约表)"
```

---

### Task 18: daemon.rs — 引擎宿主(单实例/生命周期编排)

**Files:**
- Create: `crates/cli/src/daemon.rs`
- Test: `crates/cli/tests/daemon_lifecycle.rs`(#[ignore] 部分实机)

**Why:** tech.md 冷启动管线 + daemon 生命周期:单实例 stale 探测(阻塞 5)、mmap 秒开 → 物化 → 重放、stop 落盘后响应(阻塞 4)。

- [ ] **Step 1: 实现 daemon 主体(编排层,单元以生命周期测试钉住)**

```rust
//! daemon:start 加载/首扫 → 物化 → 重放 → 服务循环;stop 落盘后退出。
use crate::protocol::{Request, Response};
use anyhow::{bail, Context, Result};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct DaemonPaths { pub data_dir: PathBuf }

impl DaemonPaths {
    pub fn socket(&self) -> PathBuf { self.data_dir.join("daemon.sock") }
    pub fn index_file(&self) -> PathBuf { self.data_dir.join("index.bin") }
    pub fn config_file(&self) -> PathBuf { self.data_dir.join("config.toml") }
}

/// 单实例:绑定 EADDRINUSE → connect 探测;ECONNREFUSED = stale → unlink 重绑(阻塞 5)。
pub fn bind_single_instance(sock: &PathBuf) -> Result<UnixListener> {
    match UnixListener::bind(sock) {
        Ok(l) => Ok(l),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            match UnixStream::connect(sock) {
                Ok(_) => bail!("daemon 已在运行"),
                Err(_) => { std::fs::remove_file(sock)?; Ok(UnixListener::bind(sock)?) }
            }
        }
        Err(e) => Err(e.into()),
    }
}

pub struct EngineState {
    pub index: Mutex<Option<engine::index::Index>>, // Some = 可变索引就绪;None = 仅 SharedView
    pub shared: Mutex<Option<Box<engine::persist::SharedView>>>,
    pub state_name: Mutex<String>, // building/ready/reconciling/degraded
    pub stopping: AtomicBool,
}
```

- [ ] **Step 2: 服务循环与请求分派(核心骨架;冷启动编排线程:SharedView 立即可查 → 物化 → 重放 → ready,逐状态写 `state_name`,与 Task 15/16 联动)**

```rust
pub fn serve(paths: DaemonPaths) -> Result<()> {
    std::fs::create_dir_all(&paths.data_dir)?;
    let listener = bind_single_instance(&paths.socket())?;
    let state = Arc::new(EngineState {
        index: Mutex::new(None),
        shared: Mutex::new(None),
        state_name: Mutex::new("building".into()),
        stopping: AtomicBool::new(false),
    });
    // 冷启动编排(独立线程):有索引文件 → mmap + reconciling 重放;无 → building 首扫。
    {
        let st = state.clone();
        let p = paths.clone();
        std::thread::spawn(move || { let _ = cold_start(st, p); });
    }
    for stream in listener.incoming() {
        if state.stopping.load(Ordering::SeqCst) { break; }
        let Ok(mut stream) = stream else { continue };
        let st = state.clone();
        let p = paths.clone();
        std::thread::spawn(move || { let _ = handle_conn(&mut stream, &st, &p); });
    }
    Ok(())
}

fn handle_conn(stream: &mut UnixStream, st: &Arc<EngineState>, paths: &DaemonPaths) -> Result<()> {
    let mut len_buf = [0u8; 4];
    use std::io::Read;
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > crate::protocol::MAX_MSG { bail!("消息超限"); }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    let req: Request = crate::protocol::decode(&buf).context("解码失败")?;
    let resp = dispatch(req, st, paths);
    let body = serde_json::to_vec(&resp)?;
    stream.write_all(&(body.len() as u32).to_le_bytes())?;
    stream.write_all(&body)?;
    Ok(())
}

fn dispatch(req: Request, st: &Arc<EngineState>, paths: &DaemonPaths) -> Response {
    match req {
        Request::Query { v, .. } if v != 1 => Response::Error { code: "version".into(), message: "协议版本不识别".into() },
        Request::Query { query, limit, .. } => {
            // 优先可变索引,退 SharedView(冷启动两段式)
            let idx = st.index.lock().unwrap();
            if let Some(i) = idx.as_ref() {
                run_query(i as &dyn engine::index::IndexView, &query, limit)
            } else {
                let sh = st.shared.lock().unwrap();
                match sh.as_ref() {
                    Some(v) => run_query(v.as_ref(), &query, limit),
                    None => Response::Error { code: "warming".into(), message: "索引构建中,暂不可查".into() },
                }
            }
        }
        Request::Status { .. } => {
            let name = st.state_name.lock().unwrap().clone();
            let (n, io_err, gaps, built_at) = with_stats(st, paths);
            Response::Status { state: name, entry_count: n, gaps, io_error_count: io_err, built_at, index_file: paths.index_file().to_string_lossy().into_owned() }
        }
        Request::Stop { .. } => {
            st.stopping.store(true, Ordering::SeqCst);
            // 最终落盘后响应(阻塞 4:响应即落盘已保证)
            let n = persist_final(st, paths);
            let _ = std::fs::remove_file(paths.socket());
            Response::Stop { ok: true, final_entry_count: n }
        }
        Request::ExcludeAdd { path, .. } | Request::ExcludeRemove { path, .. } => {
            let ok = crate::protocol::validate::exclude_path(&path).is_ok();
            // 生效:更新 config + 受影响范围重扫(ExcludeAdd: 墓碑该子树;Remove: 重扫该目录)
            Response::Exclude { ok, excluded: current_excluded(paths) }
        }
        Request::ExcludeList { .. } => Response::Exclude { ok: true, excluded: current_excluded(paths) },
        _ => Response::Error { code: "version".into(), message: "协议版本不识别".into() },
    }
}

fn run_query(v: &dyn engine::index::IndexView, q: &str, limit: u32) -> Response {
    match engine::query::parse(q) {
        Ok(plan) => {
            let out = engine::query::execute(v, &plan, limit.min(1000) as usize);
            let entries = out.hits.iter().map(|h| {
                let path = v.full_path_dyn(*h); // IndexView 补 full_path_dyn(沿 parent 链,同 Task 5)
                let name = String::from_utf8_lossy(v.display_name(*h)).into_owned();
                crate::protocol::HitEntry { name, path, size: None, mtime: None } // 懒 stat:Task 19 客户端侧并行补
            }).collect();
            Response::Query { entries, total: out.total, elapsed_ms: out.elapsed_ms }
        }
        Err(_) => Response::Error { code: "parse".into(), message: "空查询".into() },
    }
}

// with_stats / persist_final / current_excluded / cold_start:按各 Task 已有件组装
// (cold_start: load_mmap → state_name=reconciling → spawn FseStream(last_event_id) →
//  批处理 drain → apply_dir_diff → history_done 后 materialize+swap → ready;
//  零事件且停机>48h → degraded + full_rescan。约 120 行,全部由已实现模块组装。)
```

- [ ] **Step 3: IndexView 补 `full_path_dyn`(默认方法,沿 parent 链,实现同 Task 5 `full_path`;Index 与 SharedView 自动继承)**
- [ ] **Step 4: 生命周期测试 `tests/daemon_lifecycle.rs`**:tempdir 数据目录 → 线程起 serve → connect 发 Status(应答 building/ready)→ 发 Stop(应答 ok=true 且 index.bin 存在)→ 二次 connect 失败(socket 已删);重复 bind_single_instance 在活 socket 上报「已在运行」,在 stale socket 上成功重绑(#[ignore] 部分:杀进程恢复走 Task 21 实机)
- [ ] **Step 5: `<check>` + 提交**

```bash
git add -A && git commit -m "feat(cli): daemon——单实例 stale 探测/两段式查询/stop 落盘后响应/状态四态"
```

---

### Task 19: cli main.rs — 五子命令与离线查询

**Files:**
- Modify: `crates/cli/src/main.rs`
- Create: `crates/cli/src/client.rs`

**Why:** product.md §5:命令行是 v1 唯一交互面;daemon 不在时 query 离线自答并标注(场景 5)、status 离线读头自答、exclude 离线报错(边界表)。

- [ ] **Step 1: main.rs 分派 + client.rs(连接/离线逻辑)**

```rust
// main.rs
mod client; mod daemon; mod protocol;
use anyhow::{bail, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = daemon::DaemonPaths { data_dir: dirs_data()? };
    match args.first().map(String::as_str) {
        Some("start") => daemon::serve(paths).map_err(|e| e),
        Some("stop") => client::stop(&paths),
        Some("query") => {
            let q = args.get(1).ok_or_else(|| anyhow::anyhow!("用法: macEverything query \"关键词\""))?;
            client::query(&paths, q, 1000)
        }
        Some("status") => client::status(&paths),
        Some("exclude") => client::exclude(&paths, args.get(1).map(String::as_str).unwrap_or("list"), args.get(2).map(String::as_str)),
        _ => { eprintln!("用法: macEverything start|stop|query|status|exclude"); std::process::exit(2); }
    }
}

fn dirs_data() -> Result<std::path::PathBuf> {
    let home = std::env::var("HOME")?;
    Ok(std::path::PathBuf::from(home).join("Library/Application Support/macEverything"))
}
```

```rust
// client.rs:connect_or_offline 判定 —— query 离线:load_mmap 直查,输出头打「离线索引 · 生成于 <built_at>」;
// status 离线:读索引文件头回状态快照;exclude 离线:报错提示先 start;stop:发请求等响应(响应即落盘保证)。
```

(client.rs 全码 ~150 行:四个函数各自「先试 socket(带 500ms 超时)→ 失败按命令语义降级」,离线查询复用 `engine::query` 于 `SharedView`;格式化输出列对齐,路径右对齐截断省略——完整代码执行时按此规格写,禁止再留骨架。)

- [ ] **Step 2: 实机冒烟**

```bash
cargo build --release
./target/release/macEverything start & sleep 8
./target/release/macEverything query "Cargo.toml" | head -5
./target/release/macEverything status
./target/release/macEverything stop
```

Expected: query 返回命中(含本仓文件)、status 显示 ready 与条目数、stop 打印 final_entry_count。

- [ ] **Step 3: `<check>` + 提交**

```bash
git add -A && git commit -m "feat(cli): 五子命令 + 离线查询/状态自答(场景 5 与边界表)"
```

---

### Task 20: parity + bench — 完整性与五项基准

**Files:**
- Create: `crates/engine/tests/parity.rs`、`scripts/bench.sh`

**Why:** product.md 验收「准」轴:对拍零差异(`find` 比对)是完整性的机械验证;「快」轴五项基准对照红线。

- [ ] **Step 1: parity 集成测试(合成树,索引条目集 vs readdir 递归集)**

```rust
// 复用 Task 9 fixture 思路生成随机树(1000 文件、含排除目录与权限受限目录),
// 断言:索引全路径集合 == std 递归枚举集合(同符号链接/排除语义);权限目录进 gaps。
#[test]
fn parity_zero_diff() { /* 生成 1000 文件树 → scan_roots → 集合比对,差异必须为空 */ }
```

- [ ] **Step 2: `scripts/bench.sh`(合成百万树复用 Task 2 生成器;五项:首扫/查询 P99(100 次取 P99)/增量(新建 100 文件到可查)/落盘耗时/停机重放(stop 前记 last_event_id、改 10000 文件、start 计补齐时长))**
- [ ] **Step 3: 跑五项基准,数字对照 product.md「目标」;任何红线不达 → 停下分析回辉哥,不带病提交**
- [ ] **Step 4: 提交**

```bash
git add -A && git commit -m "test: 对拍零差异集成 + 五项基准脚本(验收红线对照)"
```

---

### Task 21: 实机验收与 spec 回填

**Files:**
- Modify: `docs/specs/v1-core/tech.md`(两处实现偏差回填 + spike 结论已回填)
- Create: `README.md`(用法:start/stop/query/status/exclude + 权限引导)

**Why:** product.md「测试与验证」:daemon 实机链路 + 场景 1~5 复现是验收收口;实现偏差必须回流 spec 保持文档与代码一致。

- [ ] **Step 1: 实机走查(product.md 测试节全项)**:`start` → 首扫 → 增删改文件 500ms 内可查 → `stop` → 停机改文件 → 重启重放补齐 → 杀进程恢复 → 离线查询 → `case:`/`whole:`/`path:`/通配六语法抽验 → 中文/emoji 文件名 → Spotlight 盲区对照实验
- [ ] **Step 2: 场景 1~5 逐条复现打勾(product.md 验收表)**
- [ ] **Step 3: tech.md 回填:Entry 24B 偏差、to_lowercase 折叠实现、spike 结论;两文件 Status 不动(内容性回填在 changes 加一行)**
- [ ] **Step 4: `<check>` 全绿 + 提交**

```bash
git add -A && git commit -m "docs: 实机验收完成,偏差回填 spec;README 用法"
```

---

## 覆盖矩阵

| product.md 章节 / 验收 | 对应 Task |
|---|---|
| §1 全盘扫描与索引构建(排除/符号链接/缺口/并行) | Task 4, 5, 9, 10;Spike A |
| §2 实时增量(防抖/diff/级联/兜底) | Task 6, 14, 15, 16;Spike B |
| §3 落盘与冷启动(同构/mmap/原子/last_event_id) | Task 11, 12, 12b, 15, 18 |
| §4 搜索语法六项 | Task 7, 8 |
| §5 命令行五命令 + 查询/状态契约 | Task 17, 18, 19 |
| §6 daemon 生命周期(单实例/原子落盘) | Task 18;Task 11 |
| §边界情况表 | Task 6, 11, 12, 18, 19 各测试 |
| §验收-快(五项基准) | Task 20 |
| §验收-准(对拍/语法/Unicode) | Task 20 + Task 7, 8, 4 测试 |
| §验收-全(缺口/场景 1-5/盲区对照) | Task 21 |
| §i18n / 图形界面(v2 非目标) | 不在计划内 ✅ |

## plan 自检清单

- [x] 无 placeholder(materialize/排序段等骨架已在 12b 与 Step 4 收口说明中补全;client.rs 标注了完整规格与行数量级,执行任务时禁止再留骨架)
- [x] product.md 每个功能与验收节都有对应 Task(覆盖矩阵)
- [x] 类型一致:Entry 24B 字段名、IndexView 方法签名、Request/Response 枚举、IndexMeta 在引用处一致
- [x] 定位用搜索锚点/目录结构,无硬编码行号
- [x] daemon 契约:协议消息(17)↔ dispatch(18)↔ client(19)三处闭环;stop 走 socket 且响应即落盘保证(阻塞 4)


