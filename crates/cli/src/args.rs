//! 命令行参数解析:子命令语法、数据目录解析、排序键换算。
//! 纯逻辑模块,不读环境也不碰文件系统(值由 main 注入)。

use std::ffi::OsString;
use std::path::PathBuf;

pub const USAGE: &str = "\
macEverything — macOS 全盘文件名即时搜索

用法:
  macEverything [--data-dir <PATH>] start
  macEverything [--data-dir <PATH>] stop
  macEverything [--data-dir <PATH>] query [--json] [--sort <name|size|mtime|path>] [--desc] [--include-hidden] [--] <关键词>
  macEverything [--data-dir <PATH>] status [--json]
  macEverything [--data-dir <PATH>] exclude add|remove|list|reset [路径]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Mtime,
    Path,
}

impl SortKey {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Size => "size",
            Self::Mtime => "mtime",
            Self::Path => "path",
        }
    }
    fn natural_desc(self) -> bool {
        matches!(self, Self::Mtime)
    }
    /// 绝对 desc → 协议的“反转自然方向”位。
    pub fn protocol_desc(self, absolute_desc: bool) -> bool {
        self.natural_desc() != absolute_desc
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Start,
    Stop,
    Query {
        json: bool,
        sort: Option<SortKey>,
        desc: bool,
        include_hidden: bool,
        query: String,
    },
    Status {
        json: bool,
    },
    Exclude {
        action: String,
        path: Option<String>,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub data_dir: Option<PathBuf>,
    pub command: Command,
}

#[derive(Debug, PartialEq, Eq)]
pub enum UsageError {
    MissingCommand,
    UnknownCommand(String),
    MissingDataDir,
    DuplicateDataDir,
    DataDirRelative,
    MissingSortValue,
    InvalidSort(String),
    DuplicateSort,
    DescWithoutSort,
    InvalidExcludeAction(String),
    MissingExcludePath,
    MissingQuery,
    UnexpectedArg(String),
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCommand => f.write_str("缺少子命令"),
            Self::UnknownCommand(c) => write!(f, "未知子命令: {c}"),
            Self::MissingDataDir => f.write_str("--data-dir 缺少路径"),
            Self::DuplicateDataDir => f.write_str("--data-dir 重复"),
            Self::DataDirRelative => f.write_str("--data-dir 必须是绝对路径"),
            Self::MissingSortValue => f.write_str("--sort 缺少值"),
            Self::InvalidSort(v) => write!(f, "非法排序键: {v}(可选 name|size|mtime|path)"),
            Self::DuplicateSort => f.write_str("--sort 重复"),
            Self::DescWithoutSort => f.write_str("--desc 必须与 --sort 同时使用"),
            Self::InvalidExcludeAction(a) => {
                write!(f, "未知排除操作: {a}(可选 add|remove|list|reset)")
            }
            Self::MissingExcludePath => f.write_str("exclude add/remove 缺少路径"),
            Self::MissingQuery => f.write_str("缺少查询关键词"),
            Self::UnexpectedArg(a) => write!(f, "意外参数: {a}"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum DataDirError {
    EnvEmpty,
    EnvRelative,
    HomeUnset,
}

impl std::fmt::Display for DataDirError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EnvEmpty => f.write_str("MCE_DATA_DIR 为空"),
            Self::EnvRelative => f.write_str("MCE_DATA_DIR 必须是绝对路径"),
            Self::HomeUnset => f.write_str("HOME 未设置,无法确定默认数据目录"),
        }
    }
}

/// 数据目录优先级:显式 flag > MCE_DATA_DIR > HOME 默认路径。
/// 空/相对的环境变量是错误而非回退——静默换目录会让客户端连错 daemon。
pub fn resolve_data_dir(
    flag: Option<PathBuf>,
    env: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, DataDirError> {
    if let Some(p) = flag {
        return Ok(p);
    }
    if let Some(v) = env {
        if v.to_string_lossy().trim().is_empty() {
            return Err(DataDirError::EnvEmpty);
        }
        let p = PathBuf::from(v);
        if !p.is_absolute() {
            return Err(DataDirError::EnvRelative);
        }
        return Ok(p);
    }
    let home = home.ok_or(DataDirError::HomeUnset)?;
    Ok(PathBuf::from(home).join("Library/Application Support/macEverything"))
}

fn parse_sort(v: &str) -> Result<SortKey, UsageError> {
    match v {
        "name" => Ok(SortKey::Name),
        "size" => Ok(SortKey::Size),
        "mtime" => Ok(SortKey::Mtime),
        "path" => Ok(SortKey::Path),
        other => Err(UsageError::InvalidSort(other.into())),
    }
}

/// 解析 `std::env::args().skip(1)` 形态的参数(不含 argv[0])。
/// `--data-dir` 只允许出现在子命令之前;子命令后出现按意外参数报错。
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Args, UsageError> {
    let mut it = args.into_iter().peekable();
    let mut data_dir = None;

    while let Some(a) = it.peek().cloned() {
        let value = if a == "--data-dir" {
            it.next();
            Some(it.next().ok_or(UsageError::MissingDataDir)?)
        } else if let Some(v) = a.strip_prefix("--data-dir=") {
            let v = v.to_string();
            it.next();
            Some(v)
        } else {
            None
        };
        if let Some(v) = value {
            if data_dir.is_some() {
                return Err(UsageError::DuplicateDataDir);
            }
            let p = PathBuf::from(v);
            if !p.is_absolute() {
                return Err(UsageError::DataDirRelative);
            }
            data_dir = Some(p);
        } else {
            break;
        }
    }

    let cmd = it.next().ok_or(UsageError::MissingCommand)?;
    let command = match cmd.as_str() {
        "start" => {
            no_extra(&mut it)?;
            Command::Start
        }
        "stop" => {
            no_extra(&mut it)?;
            Command::Stop
        }
        "status" => {
            let mut json = false;
            for a in it {
                if a == "--json" {
                    json = true;
                } else {
                    return Err(UsageError::UnexpectedArg(a));
                }
            }
            Command::Status { json }
        }
        "query" => parse_query(it)?,
        "exclude" => {
            let action = it.next().unwrap_or_else(|| "list".into());
            let path = it.next();
            let extra = it.next();
            // exclude 没有选项:以 '-' 开头的 token 不能当 action/path,
            // 否则 `exclude --json` 会被当成未知操作、`exclude list --json` 会静默忽略。
            if action.starts_with('-') {
                return Err(UsageError::UnexpectedArg(action));
            }
            match action.as_str() {
                "list" | "reset" => {
                    if let Some(p) = path.as_deref() {
                        return Err(UsageError::UnexpectedArg(p.into()));
                    }
                }
                "add" | "remove" => {
                    let Some(p) = path.as_deref() else {
                        return Err(UsageError::MissingExcludePath);
                    };
                    if p.starts_with('-') {
                        return Err(UsageError::UnexpectedArg(p.into()));
                    }
                }
                _ => return Err(UsageError::InvalidExcludeAction(action)),
            }
            if let Some(extra) = extra {
                return Err(UsageError::UnexpectedArg(extra));
            }
            Command::Exclude { action, path }
        }
        other => return Err(UsageError::UnknownCommand(other.into())),
    };
    Ok(Args { data_dir, command })
}

fn no_extra<I: Iterator<Item = String>>(it: &mut I) -> Result<(), UsageError> {
    if let Some(a) = it.next() {
        Err(UsageError::UnexpectedArg(a))
    } else {
        Ok(())
    }
}

/// query 语法:关键词是第一个非选项参数,其余单词以空格拼接;
/// 关键词以 `-` 开头时用 `--` 分隔。选项必须出现在关键词之前。
fn parse_query<I: Iterator<Item = String>>(mut it: I) -> Result<Command, UsageError> {
    let mut json = false;
    let mut sort = None;
    let mut desc = false;
    let mut include_hidden = false;
    let mut text = Vec::new();
    let mut literal = false;

    while let Some(a) = it.next() {
        if literal {
            text.push(a);
            continue;
        }
        if a == "--" {
            literal = true;
            continue;
        }
        if !text.is_empty() {
            // 选项必须出现在关键词之前:关键词开始后不再解析选项。
            // 以 '-' 开头的关键词须用 `--` 分隔,否则按意外参数报错。
            if a.starts_with('-') {
                return Err(UsageError::UnexpectedArg(a));
            }
            text.push(a);
            continue;
        }
        match a.as_str() {
            "--json" => json = true,
            "--desc" => desc = true,
            "--include-hidden" => include_hidden = true,
            "--sort" => {
                if sort.is_some() {
                    return Err(UsageError::DuplicateSort);
                }
                let v = it.next().ok_or(UsageError::MissingSortValue)?;
                sort = Some(parse_sort(&v)?);
            }
            other => {
                if let Some(v) = other.strip_prefix("--sort=") {
                    if sort.is_some() {
                        return Err(UsageError::DuplicateSort);
                    }
                    sort = Some(parse_sort(v)?);
                } else if other.starts_with('-') {
                    return Err(UsageError::UnexpectedArg(other.into()));
                } else {
                    text.push(a);
                }
            }
        }
    }

    if desc && sort.is_none() {
        return Err(UsageError::DescWithoutSort);
    }
    let query = text.join(" ");
    if query.trim().is_empty() {
        return Err(UsageError::MissingQuery);
    }
    Ok(Command::Query {
        json,
        sort,
        desc,
        include_hidden,
        query,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn data_dir_flag_wins_over_env() {
        let got = resolve_data_dir(
            Some(PathBuf::from("/tmp/flag")),
            Some(std::ffi::OsString::from("/tmp/env")),
            Some(std::ffi::OsString::from("/Users/u")),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/tmp/flag"));
    }

    #[test]
    fn env_relative_is_error() {
        assert_eq!(
            resolve_data_dir(None, Some("rel".into()), Some("/Users/u".into())),
            Err(DataDirError::EnvRelative)
        );
    }

    #[test]
    fn query_parses_flags_and_text() {
        let got = parse(a(&[
            "query", "--json", "--sort", "mtime", "--desc", "quarter", "report",
        ]))
        .unwrap();
        assert_eq!(
            got.command,
            Command::Query {
                json: true,
                sort: Some(SortKey::Mtime),
                desc: true,
                include_hidden: false,
                query: "quarter report".into()
            }
        );
    }

    /// --include-hidden 解析为 true；缺省 false；可与排序选项共存。
    #[test]
    fn query_include_hidden_flag() {
        let got = parse(a(&["query", "--include-hidden", "x"])).unwrap();
        assert_eq!(
            got.command,
            Command::Query {
                json: false,
                sort: None,
                desc: false,
                include_hidden: true,
                query: "x".into(),
            }
        );
        let got = parse(a(&["query", "x"])).unwrap();
        match got.command {
            Command::Query { include_hidden, .. } => assert!(!include_hidden),
            other => panic!("意外命令: {other:?}"),
        }
        let both = parse(a(&[
            "query",
            "--include-hidden",
            "--sort",
            "size",
            "--desc",
            "x",
        ]))
        .unwrap();
        assert_eq!(
            both.command,
            Command::Query {
                json: false,
                sort: Some(SortKey::Size),
                desc: true,
                include_hidden: true,
                query: "x".into(),
            }
        );
        assert!(parse(a(&["query", "--include-hidden", "--include-hidden", "x"])).is_ok());
        assert_eq!(
            parse(a(&["query", "x", "--include-hidden"])),
            Err(UsageError::UnexpectedArg("--include-hidden".into()))
        );
        assert!(USAGE.contains("[--include-hidden]"));
    }

    #[test]
    fn desc_without_sort_is_usage_error() {
        assert_eq!(
            parse(a(&["query", "--desc", "x"])),
            Err(UsageError::DescWithoutSort)
        );
    }

    #[test]
    fn data_dir_must_precede_command() {
        assert_eq!(
            parse(a(&["query", "--data-dir", "/tmp/x", "a"])),
            Err(UsageError::UnexpectedArg("--data-dir".into()))
        );
    }

    #[test]
    fn sort_translation_is_absolute() {
        assert!(SortKey::Name.protocol_desc(true));
        assert!(!SortKey::Name.protocol_desc(false));
        assert!(SortKey::Size.protocol_desc(true));
        assert!(!SortKey::Size.protocol_desc(false));
        assert!(!SortKey::Mtime.protocol_desc(true)); // 绝对降序 = 反转 mtime 自然降序
        assert!(SortKey::Mtime.protocol_desc(false));
        assert!(SortKey::Path.protocol_desc(true));
        assert!(!SortKey::Path.protocol_desc(false));
    }

    #[test]
    fn parse_sort_maps_name_and_path() {
        let name = parse(a(&["query", "--sort", "name", "x"])).unwrap();
        assert_eq!(
            name.command,
            Command::Query {
                json: false,
                sort: Some(SortKey::Name),
                desc: false,
                include_hidden: false,
                query: "x".into()
            }
        );
        let path = parse(a(&["query", "--sort=path", "x"])).unwrap();
        assert_eq!(
            path.command,
            Command::Query {
                json: false,
                sort: Some(SortKey::Path),
                desc: false,
                include_hidden: false,
                query: "x".into()
            }
        );
    }

    // ---- 以下为补充边界用例 ----

    #[test]
    fn data_dir_eq_form_parses() {
        let got = parse(a(&["--data-dir=/tmp/data", "status", "--json"])).unwrap();
        assert_eq!(got.data_dir, Some(PathBuf::from("/tmp/data")));
        assert_eq!(got.command, Command::Status { json: true });
    }

    #[test]
    fn duplicate_data_dir_is_error() {
        assert_eq!(
            parse(a(&["--data-dir", "/tmp/a", "--data-dir=/tmp/b", "status"])),
            Err(UsageError::DuplicateDataDir)
        );
    }

    #[test]
    fn missing_data_dir_value_is_error() {
        assert_eq!(parse(a(&["--data-dir"])), Err(UsageError::MissingDataDir));
        assert_eq!(
            parse(a(&["--data-dir", "start"])),
            Err(UsageError::DataDirRelative),
            "--data-dir 后紧跟的值即使像子命令也当路径(相对路径被拒)"
        );
    }

    #[test]
    fn relative_data_dir_flag_is_error() {
        assert_eq!(
            parse(a(&["--data-dir", "rel", "status"])),
            Err(UsageError::DataDirRelative)
        );
    }

    #[test]
    fn sort_eq_form_parses() {
        let got = parse(a(&["query", "--sort=size", "x"])).unwrap();
        assert_eq!(
            got.command,
            Command::Query {
                json: false,
                sort: Some(SortKey::Size),
                desc: false,
                include_hidden: false,
                query: "x".into()
            }
        );
    }

    #[test]
    fn duplicate_sort_is_error() {
        assert_eq!(
            parse(a(&["query", "--sort", "name", "--sort=size", "x"])),
            Err(UsageError::DuplicateSort)
        );
    }

    #[test]
    fn missing_sort_value_is_error() {
        assert_eq!(
            parse(a(&["query", "--sort"])),
            Err(UsageError::MissingSortValue)
        );
    }

    #[test]
    fn invalid_sort_is_error() {
        assert_eq!(
            parse(a(&["query", "--sort", "date", "x"])),
            Err(UsageError::InvalidSort("date".into()))
        );
    }

    #[test]
    fn double_dash_allows_leading_hyphen_query() {
        let got = parse(a(&["query", "--json", "--", "-foo", "--sort"])).unwrap();
        assert_eq!(
            got.command,
            Command::Query {
                json: true,
                sort: None,
                desc: false,
                include_hidden: false,
                query: "-foo --sort".into()
            }
        );
    }

    #[test]
    fn options_after_first_keyword_are_rejected() {
        assert_eq!(
            parse(a(&["query", "foo", "--json"])),
            Err(UsageError::UnexpectedArg("--json".into()))
        );
    }

    #[test]
    fn empty_query_is_usage_error() {
        assert_eq!(parse(a(&["query"])), Err(UsageError::MissingQuery));
        assert_eq!(parse(a(&["query", "   "])), Err(UsageError::MissingQuery));
    }

    #[test]
    fn env_empty_is_error() {
        assert_eq!(
            resolve_data_dir(None, Some("  ".into()), Some("/Users/u".into())),
            Err(DataDirError::EnvEmpty)
        );
    }

    #[test]
    fn flag_wins_over_broken_env() {
        let got =
            resolve_data_dir(Some(PathBuf::from("/tmp/flag")), Some("rel".into()), None).unwrap();
        assert_eq!(got, PathBuf::from("/tmp/flag"));
    }

    #[test]
    fn env_wins_over_home() {
        let got = resolve_data_dir(None, Some("/tmp/env".into()), Some("/Users/u".into())).unwrap();
        assert_eq!(got, PathBuf::from("/tmp/env"));
    }

    #[test]
    fn home_default_when_no_flag_or_env() {
        let got = resolve_data_dir(None, None, Some("/Users/u".into())).unwrap();
        assert_eq!(
            got,
            PathBuf::from("/Users/u/Library/Application Support/macEverything")
        );
    }

    #[test]
    fn home_unset_is_error() {
        assert_eq!(
            resolve_data_dir(None, None, None),
            Err(DataDirError::HomeUnset)
        );
    }

    #[test]
    fn status_rejects_non_json_args() {
        assert_eq!(
            parse(a(&["status", "--sort", "name"])),
            Err(UsageError::UnexpectedArg("--sort".into()))
        );
    }

    #[test]
    fn start_and_stop_reject_extra_args() {
        assert_eq!(
            parse(a(&["start", "--json"])),
            Err(UsageError::UnexpectedArg("--json".into()))
        );
        assert_eq!(
            parse(a(&["stop", "x"])),
            Err(UsageError::UnexpectedArg("x".into()))
        );
    }

    #[test]
    fn exclude_defaults_to_list() {
        assert_eq!(
            parse(a(&["exclude"])).unwrap().command,
            Command::Exclude {
                action: "list".into(),
                path: None
            }
        );
        assert_eq!(
            parse(a(&["exclude", "add", "/tmp/x"])).unwrap().command,
            Command::Exclude {
                action: "add".into(),
                path: Some("/tmp/x".into())
            }
        );
    }

    #[test]
    fn exclude_rejects_flag_like_tokens() {
        assert_eq!(
            parse(a(&["exclude", "--json"])),
            Err(UsageError::UnexpectedArg("--json".into()))
        );
        assert_eq!(
            parse(a(&["exclude", "list", "--json"])),
            Err(UsageError::UnexpectedArg("--json".into()))
        );
        assert_eq!(
            parse(a(&["exclude", "add", "--json"])),
            Err(UsageError::UnexpectedArg("--json".into()))
        );
    }

    #[test]
    fn exclude_validates_action_arity() {
        assert_eq!(
            parse(a(&["exclude", "list", "/tmp/x"])),
            Err(UsageError::UnexpectedArg("/tmp/x".into()))
        );
        assert_eq!(
            parse(a(&["exclude", "reset", "/tmp/x"])),
            Err(UsageError::UnexpectedArg("/tmp/x".into()))
        );
        assert_eq!(
            parse(a(&["exclude", "add"])),
            Err(UsageError::MissingExcludePath)
        );
        assert_eq!(
            parse(a(&["exclude", "remove"])),
            Err(UsageError::MissingExcludePath)
        );
        assert_eq!(
            parse(a(&["exclude", "bogus"])),
            Err(UsageError::InvalidExcludeAction("bogus".into()))
        );
        assert_eq!(
            parse(a(&["exclude", "add", "/tmp/x", "extra"])),
            Err(UsageError::UnexpectedArg("extra".into()))
        );
    }

    #[test]
    fn exclude_accepts_valid_forms() {
        assert_eq!(
            parse(a(&["exclude", "reset"])).unwrap().command,
            Command::Exclude {
                action: "reset".into(),
                path: None
            }
        );
        assert_eq!(
            parse(a(&["exclude", "remove", "/tmp/x"])).unwrap().command,
            Command::Exclude {
                action: "remove".into(),
                path: Some("/tmp/x".into())
            }
        );
    }

    #[test]
    fn unknown_command_is_error() {
        assert_eq!(
            parse(a(&["frobnicate"])),
            Err(UsageError::UnknownCommand("frobnicate".into()))
        );
        assert_eq!(parse(Vec::new()), Err(UsageError::MissingCommand));
    }
}
