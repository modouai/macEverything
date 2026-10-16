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
    let entries = view.entries();
    let mut out = String::new();
    for i in 0..entries.len() as u32 {
        if !entries[i as usize].is_live() {
            continue;
        }
        out.push_str(&view.full_path_dyn(i));
        out.push('\n');
    }
    print!("{out}");
}
