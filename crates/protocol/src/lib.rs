//! daemon 消息契约(tech.md 契约表):4 字节小端长度 + JSON;
//! 每消息含 `v` 协议版本字段;长度上限 1 MB;exclude 路径须绝对(扫描根内校验
//! 由 daemon 持根列表执行)。
use serde::{Deserialize, Serialize};

/// 单条消息(含 4 字节长度头)的上限。
pub const MAX_MSG: usize = 1_000_000;

/// 当前协议版本;daemon 拒绝异版本请求(新旧二进制混跑防护)。
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "msg", rename_all = "snake_case")]
pub enum Request {
    Query {
        v: u32,
        query: String,
        limit: u32,
        /// 列排序键(name/size/mtime/path);缺省 = relevance。纯新增可选字段
        /// 不 bump v:旧 daemon 忽略未知字段,旧客户端缺省不发送此键。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sort: Option<String>,
        /// 方向:desc = **反转该排序键的自然方向**,不是绝对升/降(Task 19b 定案)。
        /// 自然方向:name/size/path 升序、mtime 降序(engine::query::execute_sorted);
        /// None/false = 自然方向,true = 反转(如 mtime + desc=true 即最旧在前)。
        /// 引擎在全量命中排序后按方向取前 K——截断在方向之后,total > K 时取的是
        /// 真最大/最小 K 条(修复「升序前 K 条反转」误差)。同 sort:纯新增可选
        /// 字段不 bump v,缺省不发送。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        desc: Option<bool>,
    },
    Status {
        v: u32,
    },
    Stop {
        v: u32,
    },
    ExcludeAdd {
        v: u32,
        path: String,
    },
    ExcludeRemove {
        v: u32,
        path: String,
    },
    ExcludeList {
        v: u32,
    },
    /// 恢复内置默认排除项(`~/Library`、`node_modules`、`.git`、`~/.Trash`)。
    /// 协议版本不变:旧 daemon 解码未知变体失败并拒绝请求,设置页按错误展示;
    /// 使用新请求须以新 CLI 重启引擎(不自动重启)。
    ExcludeReset {
        v: u32,
    },
}

impl Request {
    pub fn version(&self) -> u32 {
        match self {
            Request::Query { v, .. }
            | Request::Status { v }
            | Request::Stop { v }
            | Request::ExcludeAdd { v, .. }
            | Request::ExcludeRemove { v, .. }
            | Request::ExcludeList { v }
            | Request::ExcludeReset { v } => *v,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "msg", rename_all = "snake_case")]
pub enum Response {
    Query {
        entries: Vec<HitEntry>,
        total: u32,
        elapsed_ms: f64,
        /// 结果行 size/mtime 的按需读取耗时(product 契约:不计入 elapsed_ms,
        /// 单独计时随输出给出)。缺省 0:容忍跨版本响应。
        #[serde(default)]
        stat_ms: f64,
        /// 实际生效的排序键回显(relevance/name/size/mtime/path)。空串 = 旧
        /// daemon 缺省(实际按 relevance);调用方比对请求键与回显,不一致即
        /// 提示「引擎版本过旧,排序未生效」,防静默错排序。
        #[serde(default)]
        sort_echo: String,
    },
    Status {
        state: String,
        entry_count: u32,
        gaps: Vec<Gap>,
        io_error_count: u32,
        built_at: u64,
        index_file: String,
    },
    Stop {
        ok: bool,
        final_entry_count: u32,
    },
    Exclude {
        ok: bool,
        excluded: Vec<String>,
        /// 失败原因(ok=false 时给出可操作提示);成功时缺省不序列化。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Error {
        code: String,
        message: String,
    },
}

/// 查询结果行(大小/修改时间按需读取,None = 未取)。
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct HitEntry {
    pub name: String,
    pub path: String,
    pub size: Option<u64>,
    pub mtime: Option<u64>,
    /// 是否目录(动作提示「进入」/「打开」分流)。纯新增可选字段不 bump 协议
    /// 版本:旧响应缺该键时缺省 false,按文件处理。
    #[serde(default)]
    pub is_dir: bool,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct Gap {
    pub path: String,
    pub reason: String,
}

/// 编码:4 字节小端长度 + JSON 体。
pub fn encode<T: Serialize>(msg: &T) -> serde_json::Result<Vec<u8>> {
    let body = serde_json::to_vec(msg)?;
    let mut out = (body.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(&body);
    Ok(out)
}

/// 解码消息体(不含长度头)。
pub fn decode_request(buf: &[u8]) -> serde_json::Result<Request> {
    serde_json::from_slice(buf)
}

pub fn decode_response(buf: &[u8]) -> serde_json::Result<Response> {
    serde_json::from_slice(buf)
}

/// 路径校验:exclude 目标必须是绝对路径(扫描根内校验由 daemon 执行)。
pub mod validate {
    pub fn exclude_path(p: &str) -> Result<String, String> {
        if !p.starts_with('/') {
            return Err("必须是绝对路径".into());
        }
        Ok(p.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let r = Request::Query {
            v: PROTOCOL_VERSION,
            query: "a b".into(),
            limit: 100,
            sort: None,
            desc: None,
        };
        let bytes = encode(&r).unwrap();
        assert_eq!(
            u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize,
            bytes.len() - 4
        );
        assert_eq!(decode_request(&bytes[4..]).unwrap(), r);
    }

    #[test]
    fn response_roundtrip() {
        let r = Response::Query {
            entries: vec![HitEntry {
                name: "a.txt".into(),
                path: "/x/a.txt".into(),
                size: Some(12),
                mtime: None,
                is_dir: false,
            }],
            total: 1,
            elapsed_ms: 0.8,
            stat_ms: 1.2,
            sort_echo: "relevance".into(),
        };
        let bytes = encode(&r).unwrap();
        assert_eq!(decode_response(&bytes[4..]).unwrap(), r);
    }

    /// 向后兼容钉:旧客户端(无 sort)/旧 daemon(无 sort_echo)的消息必须可解码,
    /// 缺省语义 = relevance(空回显让调用方识别「请求键未生效」,防静默错排序)。
    #[test]
    fn query_sort_field_backward_compatible() {
        let old_req = br#"{"msg":"query","v":1,"query":"a","limit":10}"#;
        let r: Request = serde_json::from_slice(old_req).unwrap();
        assert!(matches!(r, Request::Query { sort: None, .. }));
        let old_resp = br#"{"msg":"query","entries":[],"total":0,"elapsed_ms":0.0,"stat_ms":0.0}"#;
        let p: Response = serde_json::from_slice(old_resp).unwrap();
        assert!(matches!(p, Response::Query { sort_echo, .. } if sort_echo.is_empty()));
    }

    /// is_dir 向后兼容钉:旧响应(无该键)必须可解码且缺省为文件(false);
    /// 新响应经线上编码往返保留目录真值(true)。
    #[test]
    fn is_dir_field_backward_compatible() {
        let old_resp = br#"{"msg":"query","entries":[{"name":"a.txt","path":"/x/a.txt","size":12,"mtime":null}],"total":1,"elapsed_ms":0.0,"stat_ms":0.0,"sort_echo":"relevance"}"#;
        match serde_json::from_slice::<Response>(old_resp).unwrap() {
            Response::Query { entries, .. } => {
                assert_eq!(entries.len(), 1);
                assert!(!entries[0].is_dir, "旧 JSON 缺 is_dir 应缺省为文件");
            }
            other => panic!("意外响应: {other:?}"),
        }

        let new_resp = Response::Query {
            entries: vec![HitEntry {
                name: "Documents".into(),
                path: "/x/Documents".into(),
                size: None,
                mtime: None,
                is_dir: true,
            }],
            total: 1,
            elapsed_ms: 0.0,
            stat_ms: 0.0,
            sort_echo: "relevance".into(),
        };
        let bytes = encode(&new_resp).unwrap();
        assert_eq!(decode_response(&bytes[4..]).unwrap(), new_resp);
    }

    /// desc 向后兼容钉:旧 JSON(无 desc;v1 形态与 8b 的 sort-only 形态)必须可
    /// 解码且 desc=None,缺省语义 = 引擎自然方向(等值 false)。
    #[test]
    fn desc_field_backward_compatible() {
        let old = br#"{"msg":"query","v":1,"query":"a","limit":10}"#;
        let r: Request = serde_json::from_slice(old).unwrap();
        assert!(matches!(r, Request::Query { desc: None, .. }));
        // 8b 客户端形态:带 sort、无 desc——19b daemon 收到的消息与 8b 完全同形
        let old_8b = br#"{"msg":"query","v":1,"query":"a","limit":10,"sort":"size"}"#;
        let r8b: Request = serde_json::from_slice(old_8b).unwrap();
        assert!(matches!(
            r8b,
            Request::Query { sort: Some(ref s), desc: None, .. } if s == "size"
        ));
    }

    /// desc 编码→解码往返:线上形态为 JSON bool(不是字符串),经长度头协议无损保留。
    #[test]
    fn desc_field_roundtrip() {
        let r = Request::Query {
            v: PROTOCOL_VERSION,
            query: "a".into(),
            limit: 10,
            sort: Some("size".into()),
            desc: Some(true),
        };
        let b = encode(&r).unwrap();
        assert_eq!(decode_request(&b[4..]).unwrap(), r);
        let body = serde_json::to_string(&r).unwrap();
        assert!(
            body.contains(r#""desc":true"#),
            "desc 线上为 JSON bool:{body}"
        );
    }

    /// sort 编码→解码往返:请求与响应两侧的排序字段经长度头协议无损保留。
    #[test]
    fn sort_field_roundtrip() {
        let r = Request::Query {
            v: PROTOCOL_VERSION,
            query: "a".into(),
            limit: 10,
            sort: Some("size".into()),
            desc: None,
        };
        let b = encode(&r).unwrap();
        assert_eq!(decode_request(&b[4..]).unwrap(), r);
        let p = Response::Query {
            entries: vec![],
            total: 0,
            elapsed_ms: 0.0,
            stat_ms: 0.0,
            sort_echo: "size".into(),
        };
        let b = encode(&p).unwrap();
        assert_eq!(decode_response(&b[4..]).unwrap(), p);
    }

    /// sort/desc=None(CLI/App 不传)编码不含两键:v1 线上形态逐字节保持,
    /// 混跑期旧 daemon 收到的消息与旧客户端完全同形。
    #[test]
    fn none_sort_omits_field_on_wire() {
        let r = Request::Query {
            v: PROTOCOL_VERSION,
            query: "a".into(),
            limit: 10,
            sort: None,
            desc: None,
        };
        let body = serde_json::to_string(&r).unwrap();
        assert!(!body.contains("sort"), "None 不应序列化 sort 键:{body}");
        assert!(!body.contains("desc"), "None 不应序列化 desc 键:{body}");
    }

    #[test]
    fn reject_relative_path() {
        assert!(validate::exclude_path("relative/x").is_err());
        assert!(validate::exclude_path("~/Library").is_err(), "~ 非绝对");
        assert!(validate::exclude_path("/abs/x").is_ok());
    }

    /// E5:恢复默认是纯版本化无参请求;线上 tag 固定为 `exclude_reset`,往返无损。
    /// 旧 daemon 不认识该 tag → 解码失败并拒绝(协议版本号刻意不 bump)。
    #[test]
    fn exclude_reset_roundtrip_and_version() {
        let r = Request::ExcludeReset {
            v: PROTOCOL_VERSION,
        };
        let bytes = encode(&r).unwrap();
        assert_eq!(decode_request(&bytes[4..]).unwrap(), r);
        assert_eq!(r.version(), PROTOCOL_VERSION);
        let body = serde_json::to_string(&r).unwrap();
        assert!(
            body.contains(r#""msg":"exclude_reset""#),
            "线上 tag 应为 exclude_reset:{body}"
        );
        assert!(!body.contains("path"), "恢复默认不接受路径/参数:{body}");
    }

    #[test]
    fn version_field_present_on_all_variants() {
        // 契约:每请求都携带版本字段(serde tag 保证变体可辨)
        let cases = [
            Request::Status {
                v: PROTOCOL_VERSION,
            },
            Request::Stop {
                v: PROTOCOL_VERSION,
            },
            Request::ExcludeList {
                v: PROTOCOL_VERSION,
            },
            Request::ExcludeReset {
                v: PROTOCOL_VERSION,
            },
        ];
        for c in cases {
            assert_eq!(c.version(), PROTOCOL_VERSION);
        }
    }
}
