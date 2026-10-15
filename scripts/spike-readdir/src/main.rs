use std::fs;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("");
    let root = PathBuf::from(args.get(2).map(String::as_str).unwrap_or("/tmp/mce-bench"));
    match cmd {
        "gen" => gen(&root, args.get(3).map(|s| s.parse().unwrap()).unwrap_or(1_000_000)),
        "scan" => {
            let n = std::cell::Cell::new(0u64);
            let t = Instant::now();
            walk(&root, &n);
            let el = t.elapsed();
            println!(
                "files={} elapsed={:.2}s throughput={:.0} files/s",
                n.get(),
                el.as_secs_f64(),
                n.get() as f64 / el.as_secs_f64()
            );
        }
        _ => eprintln!("usage: spike-readdir gen|scan <root> [total]"),
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
        if ft.is_dir() {
            walk(&e.path(), n);
        }
    }
}
