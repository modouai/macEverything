//! 导出索引全部活条目路径(每行一条,display 形态):对拍脚本的索引侧数据源。
//! 用法:cargo run --release -p engine --example export_paths <index.bin>
//! 输出到 stdout;路径含换行的文件会破坏行协议(罕见,对拍脚本会作为差异报出)。
use engine::index::IndexView;
use engine::persist;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("用法: export_paths <index.bin>");
    let view = persist::load_mmap(std::path::Path::new(&path)).expect("加载索引失败");
    let mut out = String::new();
    // 分段枚举(tech.md §4.3):段内下标 + 段起始号 = 条目号
    for (start, seg) in view.entry_segments() {
        for (j, e) in seg.iter().enumerate() {
            if !e.is_live() {
                continue;
            }
            out.push_str(&view.full_path_dyn(start + j as u32));
            out.push('\n');
        }
    }
    print!("{out}");
}
