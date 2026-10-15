//! 兜底全量重扫:重放不可用时的最终一致性闸门(ADR-5 回退;状态 degraded)。
use crate::index::Index;
use crate::scanner::ScanGaps;
use std::path::{Path, PathBuf};

/// 保守判定:重放零事件且停机超阈值 → 一律重扫(禁止静默遗漏的最后一道闸)。
///
/// 背景:Task 3 spike 实证「过旧 sinceWhen 无可观测信号」——事件日志滚动后
/// 重放静默从 now 开始,调用方无从分辨「无变更」与「历史已丢」。因此不能赌
/// 信号:零事件 + 停机时长超过阈值(默认 48h,配置项)即视为重放不可信。
pub fn needs_full_rescan(replayed_events: u64, downtime_secs: u64, threshold_secs: u64) -> bool {
    replayed_events == 0 && downtime_secs > threshold_secs
}

/// 全量重扫:复用并行扫描器产出全新 Index;调用方(daemon)应用后原子替换并重落盘。
pub fn full_rescan(roots: &[PathBuf], excludes: &[PathBuf], threads: usize) -> (Index, ScanGaps) {
    crate::scanner::scan_roots(roots, excludes, threads)
}

/// 重放不可用判定的统一入口(信号路径已由 spike 排除,见 ADR-5):
/// 收到丢弃类标记、监听器失效、或保守判定命中,任一即重扫。
#[must_use]
pub fn should_degrade(
    dropped_flag_seen: bool,
    stream_failed: bool,
    replayed_events: u64,
    downtime_secs: u64,
    threshold_secs: u64,
) -> bool {
    dropped_flag_seen
        || stream_failed
        || needs_full_rescan(replayed_events, downtime_secs, threshold_secs)
}

/// 展开排除列表为 scanner 可用形式(`~`/`~/x` 前缀展开为 $HOME;其余原样)。
///
/// 语义分工(contract):绝对路径按前缀命中的整棵子树排除;相对名(如
/// `node_modules`、`.git`)按「路径任一组件等于该名」匹配(scanner::is_excluded)。
#[must_use]
pub fn expand_excludes(excludes: &[String]) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    excludes
        .iter()
        .map(|e| expand_one(e, home.as_deref()))
        .collect()
}

fn expand_one(e: &str, home: Option<&Path>) -> PathBuf {
    if let Some(h) = home {
        if e == "~" {
            return h.to_path_buf();
        }
        if let Some(rest) = e.strip_prefix("~/") {
            return h.join(rest);
        }
    }
    PathBuf::from(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conservative_judgement() {
        let day = 48 * 3600;
        assert!(
            needs_full_rescan(0, 49 * 3600, day),
            "零事件 + 超阈值 → 重扫"
        );
        assert!(!needs_full_rescan(0, 3600, day), "零事件 + 未超 → 重放可信");
        assert!(!needs_full_rescan(15, 100 * 3600, day), "有事件 → 不重扫");
        assert!(
            !needs_full_rescan(0, day, day),
            "恰等于阈值不触发(严格大于)"
        );
    }

    #[test]
    fn degrade_triggers() {
        let day = 48 * 3600;
        assert!(should_degrade(true, false, 10, 0, day), "丢弃标记 → 降级");
        assert!(should_degrade(false, true, 10, 0, day), "监听失效 → 降级");
        assert!(
            should_degrade(false, false, 0, 49 * 3600, day),
            "保守判定 → 降级"
        );
        assert!(
            !should_degrade(false, false, 10, 60, day),
            "健康增量 → 不降级"
        );
    }

    #[test]
    fn expand_excludes_expands_home_and_keeps_relative() {
        let out = expand_excludes(&[
            "~/Library".into(),
            "node_modules".into(),
            "/abs/path".into(),
        ]);
        let home = std::env::var_os("HOME").expect("测试环境须有 HOME");
        assert_eq!(out[0], PathBuf::from(home).join("Library"));
        assert_eq!(out[1], PathBuf::from("node_modules"));
        assert_eq!(out[2], PathBuf::from("/abs/path"));
    }

    #[test]
    fn full_rescan_delegates_to_scanner() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("a.txt"), "").unwrap();
        let (idx, gaps) = full_rescan(&[t.path().to_path_buf()], &[], 4);
        assert_eq!(idx.entry_count(), 2); // 根 + a.txt
        assert_eq!(gaps.permission.len(), 0);
    }
}
