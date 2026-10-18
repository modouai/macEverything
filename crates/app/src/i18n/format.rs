//! 带参数文案（按当前语言渲染；中英共用同一组参数）。

use super::{lang, Lang};

/// "{n} 项" / "{n} item(s)"。
pub fn items(n: u32) -> String {
    match lang() {
        Lang::ZhHans => format!("{n} 项"),
        Lang::En if n == 1 => "1 item".to_owned(),
        Lang::En => format!("{n} items"),
    }
}

/// "N 个目录未收录" / "N folder(s) not indexed"。
pub fn gaps(n: u32) -> String {
    match lang() {
        Lang::ZhHans => format!("{n} 个目录未收录"),
        Lang::En if n == 1 => "1 folder not indexed".to_owned(),
        Lang::En => format!("{n} folders not indexed"),
    }
}

/// "展示前 N 条" / "Showing first N"。
pub fn showing_first(n: usize) -> String {
    match lang() {
        Lang::ZhHans => format!("展示前 {n} 条"),
        Lang::En => format!("Showing first {n}"),
    }
}

/// 状态栏计数段：`N 项 · 匹配 X ms · 读取 Y ms[ · 展示前 K 条]`。
pub fn counts(total: u32, shown: usize, match_ms: &str, read_ms: &str) -> String {
    let mut out = match lang() {
        Lang::ZhHans => format!("{} · 匹配 {match_ms} ms · 读取 {read_ms} ms", items(total)),
        Lang::En => format!("{} · Match {match_ms} ms · Read {read_ms} ms", items(total)),
    };
    if (shown as u32) < total {
        out.push_str(" · ");
        out.push_str(&showing_first(shown));
    }
    out
}

/// 缺口面板标题。
pub fn gaps_header(n: usize) -> String {
    match lang() {
        Lang::ZhHans => format!("权限缺口({n} 个目录,未收录):"),
        Lang::En if n == 1 => "Permission Gaps (1 folder not indexed):".to_owned(),
        Lang::En => format!("Permission Gaps ({n} folders not indexed):"),
    }
}

/// 索引构建条目数。
pub fn index_building_count(n: u64) -> String {
    let n = n.to_string();
    super::fill(s().index_building_count, &[("n", n.as_str())])
}

fn s() -> &'static super::Strings {
    super::s()
}
