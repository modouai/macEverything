//! CLI JSON 输出构造:query/status/error 对象与 `schema_version`。
//! 只做数据整形,不读环境、不打印;stdout 的「单个 JSON 对象」由 main 保证。

use client::{QueryOutcome, StatusOutcome};
use serde_json::{json, Value};
use std::path::Path;

/// JSON 契约版本,独立于协议版本;字段只增不删,变更须升版本。
pub const SCHEMA_VERSION: u32 = 1;

/// query 结果对象:字段口径与 `protocol::HitEntry` 一致(size/mtime 读不到为 null)。
pub fn query_json(out: &QueryOutcome) -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "total": out.total,
        "elapsed_ms": out.elapsed_ms,
        "stat_ms": out.stat_ms,
        "sort_echo": out.sort_echo,
        "desc_echo": out.desc_echo,
        "offline_built_at": out.offline_built_at,
        "entries": out.entries.iter().map(|e| json!({
            "name": e.name,
            "path": e.path,
            "size": e.size,
            "mtime": e.mtime,
            "is_dir": e.is_dir,
        })).collect::<Vec<_>>(),
    })
}

/// status 对象:任何状态字段集保持一致;无索引时 `index_file` 取数据目录下的
/// 预期路径(daemon 不可能回报该状态)。
pub fn status_json(outcome: &StatusOutcome, data_dir: &Path) -> Value {
    match outcome {
        StatusOutcome::Reported(r) => json!({
            "schema_version": SCHEMA_VERSION,
            "state": r.state,
            "entry_count": r.entry_count,
            "gaps": r.gaps.iter().map(|g| json!({
                "path": g.path,
                "reason": g.reason,
            })).collect::<Vec<_>>(),
            "io_error_count": r.io_error_count,
            "built_at": r.built_at,
            "index_file": r.index_file,
            "offline": r.offline,
        }),
        StatusOutcome::NoIndex => no_index_json(data_dir),
    }
}

/// 无落盘索引:固定字段集,gaps/计数为 0,offline=true。
pub fn no_index_json(data_dir: impl AsRef<Path>) -> Value {
    let index_file = data_dir.as_ref().join("index.bin");
    json!({
        "schema_version": SCHEMA_VERSION,
        "state": "no_index",
        "entry_count": 0,
        "gaps": [],
        "io_error_count": 0,
        "built_at": 0,
        "index_file": index_file.to_string_lossy(),
        "offline": true,
    })
}

/// 失败对象:`code` = daemon 错误码;连接/离线路径失败为 null。
pub fn error_json(code: Option<&str>, message: &str) -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "error": { "code": code, "message": message },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use client::{QueryOutcome, StatusOutcome, StatusReport};
    use protocol::HitEntry;

    #[test]
    fn query_shape_has_all_keys() {
        let out = QueryOutcome {
            entries: vec![HitEntry {
                name: "a".into(),
                path: "/a".into(),
                size: None,
                mtime: None,
                is_dir: false,
            }],
            total: 1,
            elapsed_ms: 1.0,
            stat_ms: 0.0,
            sort_echo: "relevance".into(),
            desc_echo: None,
            offline_built_at: None,
        };
        let v = query_json(&out);
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["total"], 1);
        assert!(v["entries"][0]["size"].is_null());
        assert!(v["entries"][0]["mtime"].is_null());
        assert!(v["desc_echo"].is_null());
        // 缺键在 Value 索引下同样得到 null:必须显式钉住字段集。
        let keys: std::collections::BTreeSet<&str> =
            v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            std::collections::BTreeSet::from([
                "schema_version",
                "total",
                "elapsed_ms",
                "stat_ms",
                "sort_echo",
                "desc_echo",
                "offline_built_at",
                "entries",
            ])
        );
        let entry: std::collections::BTreeSet<&str> = v["entries"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            entry,
            std::collections::BTreeSet::from(["name", "path", "size", "mtime", "is_dir"])
        );
    }

    #[test]
    fn no_index_shape_is_stable() {
        let v = no_index_json("/tmp/mce");
        assert_eq!(v["state"], "no_index");
        assert_eq!(v["entry_count"], 0);
        assert_eq!(v["offline"], true);
        assert_eq!(v["gaps"], serde_json::json!([]));
        assert_eq!(v["index_file"], "/tmp/mce/index.bin");
    }

    /// 在线状态:字段集固定,gaps 逐项映射,offline=false。
    #[test]
    fn status_reported_maps_fields() {
        let outcome = StatusOutcome::Reported(StatusReport {
            state: "ready".into(),
            entry_count: 42,
            gaps: vec![protocol::Gap {
                path: "/x".into(),
                reason: "permission".into(),
            }],
            io_error_count: 3,
            built_at: 123,
            index_file: "/d/index.bin".into(),
            offline: false,
        });
        let v = status_json(&outcome, std::path::Path::new("/d"));
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["state"], "ready");
        assert_eq!(v["entry_count"], 42);
        assert_eq!(v["gaps"][0]["path"], "/x");
        assert_eq!(v["gaps"][0]["reason"], "permission");
        assert_eq!(v["io_error_count"], 3);
        assert_eq!(v["built_at"], 123);
        assert_eq!(v["index_file"], "/d/index.bin");
        assert_eq!(v["offline"], false);
    }

    /// NoIndex 走同一字段集,`index_file` 取数据目录下的预期路径(而非 daemon 回报值)。
    #[test]
    fn status_no_index_derives_index_file_from_data_dir() {
        let v = status_json(&StatusOutcome::NoIndex, std::path::Path::new("/d"));
        assert_eq!(v["state"], "no_index");
        assert_eq!(v["index_file"], "/d/index.bin");
        assert_eq!(v["offline"], true);
    }

    #[test]
    fn error_shape_is_stable() {
        let v = error_json(Some("warming"), "索引尚未就绪");
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["error"]["code"], "warming");
        assert_eq!(v["error"]["message"], "索引尚未就绪");

        let v = error_json(None, "offline");
        assert!(v["error"]["code"].is_null());
    }
}
