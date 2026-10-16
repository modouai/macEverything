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
}

impl Request {
    pub fn version(&self) -> u32 {
        match self {
            Request::Query { v, .. }
            | Request::Status { v }
            | Request::Stop { v }
            | Request::ExcludeAdd { v, .. }
            | Request::ExcludeRemove { v, .. }
            | Request::ExcludeList { v } => *v,
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

    /// sort 编码→解码往返:请求与响应两侧的排序字段经长度头协议无损保留。
    #[test]
    fn sort_field_roundtrip() {
        let r = Request::Query {
            v: PROTOCOL_VERSION,
            query: "a".into(),
            limit: 10,
            sort: Some("size".into()),
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

    /// sort=None(CLI/App 不传)编码不含 sort 键:v1 线上形态逐字节保持,
    /// 混跑期旧 daemon 收到的消息与旧客户端完全同形。
    #[test]
    fn none_sort_omits_field_on_wire() {
        let r = Request::Query {
            v: PROTOCOL_VERSION,
            query: "a".into(),
            limit: 10,
            sort: None,
        };
        let body = serde_json::to_string(&r).unwrap();
        assert!(!body.contains("sort"), "None 不应序列化 sort 键:{body}");
    }

    #[test]
    fn reject_relative_path() {
        assert!(validate::exclude_path("relative/x").is_err());
        assert!(validate::exclude_path("~/Library").is_err(), "~ 非绝对");
        assert!(validate::exclude_path("/abs/x").is_ok());
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
        ];
        for c in cases {
            assert_eq!(c.version(), PROTOCOL_VERSION);
        }
    }
}
