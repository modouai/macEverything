//! 查询 micro-bench:load_mmap 索引 → 200 次随机关键词 → 引擎侧耗时分布。
//! 用法:cargo run --release -p engine --example bench_query <index.bin> [总文件数]
use engine::index::IndexView;
use engine::persist;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("用法: bench_query <index.bin> [total]");
    let total: u32 = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);
    let view = persist::load_mmap(std::path::Path::new(path)).expect("加载索引失败");
    println!("条目 {}", view.entries().len());

    let mut times = Vec::with_capacity(200);
    let mut seed: u64 = 0xABCD;
    for _ in 0..200 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let n = ((seed >> 33) as u32) % total;
        let q = format!("file_{n:07}");
        let plan = engine::query::parse(&q).unwrap();
        let out = engine::query::execute(view.as_ref(), &plan, 1000);
        let _ = out.total;
        times.push(out.elapsed_ms);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = times.len();
    println!(
        "中位 {:.2} ms · P90 {:.2} · P99 {:.2} · 最大 {:.2}",
        times[n / 2],
        times[n * 9 / 10],
        times[n * 99 / 100],
        times[n - 1]
    );
    // 分布直方(>5ms 的尾部样本计数)
    let over: Vec<f64> = times.iter().copied().filter(|t| *t > 5.0).collect();
    println!(
        ">5ms 样本 {} 个: {:?}",
        over.len(),
        &over[..over.len().min(10)]
    );
}
