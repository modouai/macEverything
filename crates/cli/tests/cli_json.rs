//! 进程级契约:退出码、`--json` 时 stdout 只能是单个 JSON 对象、用法错误只写 stderr。
use serde_json::Value;
use std::collections::BTreeSet;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_macEverything")
}

/// 隔离环境:HOME 与数据目录都在临时目录,绝不碰真实索引。
fn isolated() -> (tempfile::TempDir, Command) {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Command::new(bin());
    c.env("HOME", dir.path())
        .env("MCE_DATA_DIR", dir.path().join("data"));
    (dir, c)
}

#[test]
fn no_args_is_usage_error_with_empty_stdout() {
    let out = Command::new(bin()).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(!out.stderr.is_empty());
}

#[test]
fn status_json_no_index_is_one_object() {
    let (_d, mut c) = isolated();
    let out = c.args(["status", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["state"], "no_index");
    assert_eq!(v["entry_count"], 0);
    assert_eq!(v["offline"], true);
}

/// 无索引状态字段集固定;`index_file` 指向目标数据目录下的预期路径。
#[test]
fn status_json_no_index_has_exact_key_set() {
    let (d, mut c) = isolated();
    let out = c.args(["status", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let keys: BTreeSet<String> = v.as_object().unwrap().keys().cloned().collect();
    let expected: BTreeSet<String> = [
        "schema_version",
        "state",
        "entry_count",
        "gaps",
        "io_error_count",
        "built_at",
        "index_file",
        "offline",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(keys, expected);
    let index = d.path().join("data").join("index.bin");
    assert_eq!(v["index_file"], serde_json::json!(index.to_string_lossy()));
}

#[test]
fn query_json_without_index_is_coded_null_error() {
    let (_d, mut c) = isolated();
    let out = c.args(["query", "--json", "x"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema_version"], 1);
    assert!(v["error"]["code"].is_null());
    assert!(!v["error"]["message"].as_str().unwrap().is_empty());
}

#[test]
fn desc_without_sort_exits_2_with_empty_stdout() {
    let (_d, mut c) = isolated();
    let out = c.args(["query", "--json", "--desc", "x"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}

/// 用法错误先于数据目录解析:即使带 `--json` 也不输出 JSON 对象。
#[test]
fn usage_error_with_json_leaves_stdout_empty() {
    let (_d, mut c) = isolated();
    let out = c.args(["status", "--json", "--bogus"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(!out.stderr.is_empty());
}

/// `--data-dir` 相对路径是用法错误(退出码 2),不得进入运行期。
#[test]
fn relative_data_dir_flag_exits_2_with_empty_stdout() {
    let (_d, mut c) = isolated();
    let out = c
        .args(["--data-dir", "rel", "status", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(!out.stderr.is_empty());
}

/// `MCE_DATA_DIR` 相对路径是运行期错误:JSON 模式下仍输出错误对象到 stdout。
#[test]
fn relative_env_data_dir_is_runtime_error_json() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(bin())
        .env("HOME", dir.path())
        .env("MCE_DATA_DIR", "rel")
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema_version"], 1);
    assert!(v["error"]["code"].is_null());
    assert!(v["error"]["message"].as_str().unwrap().contains("绝对路径"));
}

/// 人类模式同一错误只写 stderr,stdout 保持空。
#[test]
fn relative_env_data_dir_human_error_goes_to_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(bin())
        .env("HOME", dir.path())
        .env("MCE_DATA_DIR", "rel")
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(!out.stderr.is_empty());
}
