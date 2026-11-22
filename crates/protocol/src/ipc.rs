//! Frontend ↔ `animed` NDJSON IPC 完整编解码实现（遵循 `.agents/contracts/ipc-protocol.md`）。

use std::fmt::{self, Write as _};

use crate::json::{JsonValue, write_json_f32, write_json_string};
use crate::{Candidate, CandidateKind, CommitAction, EngineConfig, KeyInput, PreeditState};

/// NDJSON 单行载荷硬上限（64 KiB，含换行前文本，遵守 `ipc-protocol.md` §1）。
pub const MAX_IPC_LINE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    LineTooLarge { len: usize, max: usize },
    ContainsUnescapedNewline,
    InvalidJson(String),
    MissingField(&'static str),
    InvalidField { field: &'static str, detail: String },
    UnknownMessageType(String),
}

impl ProtocolError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::LineTooLarge { .. } => "line_too_large",
            Self::ContainsUnescapedNewline => "unescaped_newline",
            Self::InvalidJson(_) => "invalid_json",
            Self::MissingField(_) => "missing_field",
            Self::InvalidField { .. } => "invalid_field",
            Self::UnknownMessageType(_) => "unknown_message_type",
        }
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LineTooLarge { len, max } => {
                write!(f, "IPC line length {len} exceeds {max} byte limit")
            }
            Self::ContainsUnescapedNewline => {
                write!(f, "IPC line contains unescaped newline or carriage return")
            }
            Self::InvalidJson(msg) => write!(f, "invalid JSON payload: {msg}"),
            Self::MissingField(field) => write!(f, "missing required field '{field}'"),
            Self::InvalidField { field, detail } => {
                write!(f, "invalid field '{field}': {detail}")
            }
            Self::UnknownMessageType(ty) => write!(f, "unknown IPC message type '{ty}'"),
        }
    }
}

impl std::error::Error for ProtocolError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcRequest {
    Hello {
        client: String,
        protocol_version: u32,
        session_id: Option<String>,
    },
    Key {
        seq: u64,
        key: KeyInput,
        mods: u32,
    },
    Select {
        seq: u64,
        index: usize,
    },
    Context {
        before: String,
        after: String,
    },
    Privacy {
        enabled: bool,
    },
    Reset {
        seq: u64,
    },
    Config(EngineConfig),
    Ping {
        id: Option<u64>,
    },
    Shutdown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IpcResponse {
    HelloAck {
        protocol_version: u32,
        version: String,
        capabilities: Vec<String>,
    },
    State(PreeditState),
    Commit(CommitAction),
    Pong {
        version: String,
        id: Option<u64>,
    },
    Error {
        seq: Option<u64>,
        code: String,
        message: String,
    },
}

fn prepare_ndjson_line(line: &str) -> Result<&str, ProtocolError> {
    if line.len() > MAX_IPC_LINE_BYTES {
        return Err(ProtocolError::LineTooLarge {
            len: line.len(),
            max: MAX_IPC_LINE_BYTES,
        });
    }
    let stripped = line
        .strip_suffix("\r\n")
        .or_else(|| line.strip_suffix('\n'))
        .unwrap_or(line);
    if stripped.contains('\n') || stripped.contains('\r') {
        return Err(ProtocolError::ContainsUnescapedNewline);
    }
    Ok(stripped)
}

fn parse_root_object(line: &str) -> Result<JsonValue, ProtocolError> {
    let stripped = prepare_ndjson_line(line)?;
    let val = JsonValue::parse(stripped).map_err(ProtocolError::InvalidJson)?;
    if !val.is_object() {
        return Err(ProtocolError::InvalidJson(
            "IPC message root must be a JSON object".to_string(),
        ));
    }
    Ok(val)
}

fn req_str<'a>(obj: &'a JsonValue, field: &'static str) -> Result<&'a str, ProtocolError> {
    let val = obj.get(field).ok_or(ProtocolError::MissingField(field))?;
    val.as_str().ok_or_else(|| ProtocolError::InvalidField {
        field,
        detail: "expected string".to_string(),
    })
}

fn opt_str<'a>(obj: &'a JsonValue, field: &'static str) -> Result<Option<&'a str>, ProtocolError> {
    match obj.get(field) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(v) => v.as_str().map(Some).ok_or_else(|| ProtocolError::InvalidField {
            field,
            detail: "expected string or null".to_string(),
        }),
    }
}

fn req_bool(obj: &JsonValue, field: &'static str) -> Result<bool, ProtocolError> {
    let val = obj.get(field).ok_or(ProtocolError::MissingField(field))?;
    val.as_bool().ok_or_else(|| ProtocolError::InvalidField {
        field,
        detail: "expected boolean".to_string(),
    })
}

fn req_u64(obj: &JsonValue, field: &'static str) -> Result<u64, ProtocolError> {
    let val = obj.get(field).ok_or(ProtocolError::MissingField(field))?;
    val.as_u64().ok_or_else(|| ProtocolError::InvalidField {
        field,
        detail: "expected non-negative u64 integer".to_string(),
    })
}

fn opt_u64(obj: &JsonValue, field: &'static str) -> Result<Option<u64>, ProtocolError> {
    match obj.get(field) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| ProtocolError::InvalidField {
            field,
            detail: "expected non-negative u64 integer or null".to_string(),
        }),
    }
}

fn req_u32(obj: &JsonValue, field: &'static str) -> Result<u32, ProtocolError> {
    let val = obj.get(field).ok_or(ProtocolError::MissingField(field))?;
    val.as_u32().ok_or_else(|| ProtocolError::InvalidField {
        field,
        detail: "expected non-negative u32 integer".to_string(),
    })
}

fn opt_u32(obj: &JsonValue, field: &'static str) -> Result<Option<u32>, ProtocolError> {
    match obj.get(field) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(v) => v.as_u32().map(Some).ok_or_else(|| ProtocolError::InvalidField {
            field,
            detail: "expected non-negative u32 integer or null".to_string(),
        }),
    }
}

fn req_usize(obj: &JsonValue, field: &'static str) -> Result<usize, ProtocolError> {
    let val = obj.get(field).ok_or(ProtocolError::MissingField(field))?;
    val.as_usize().ok_or_else(|| ProtocolError::InvalidField {
        field,
        detail: "expected non-negative integer".to_string(),
    })
}

fn req_f32(obj: &JsonValue, field: &'static str) -> Result<f32, ProtocolError> {
    let val = obj.get(field).ok_or(ProtocolError::MissingField(field))?;
    val.as_f32().ok_or_else(|| ProtocolError::InvalidField {
        field,
        detail: "expected finite float number".to_string(),
    })
}

fn decode_key_input(obj: &JsonValue) -> Result<KeyInput, ProtocolError> {
    let key_name = req_str(obj, "key")?;
    match key_name {
        "char" => {
            let text = req_str(obj, "text")?;
            let mut chars = text.chars();
            let Some(ch) = chars.next() else {
                return Err(ProtocolError::InvalidField {
                    field: "text",
                    detail: "key 'char' requires non-empty 1-char text".to_string(),
                });
            };
            if chars.next().is_some() {
                return Err(ProtocolError::InvalidField {
                    field: "text",
                    detail: "key 'char' expects exactly 1 character".to_string(),
                });
            }
            Ok(KeyInput::Char(ch))
        }
        "space" => Ok(KeyInput::Space),
        "backspace" => Ok(KeyInput::Backspace),
        "enter" => Ok(KeyInput::Enter),
        "escape" => Ok(KeyInput::Escape),
        "digit" => {
            let text = req_str(obj, "text")?;
            let bytes = text.as_bytes();
            if bytes.len() != 1 || !bytes[0].is_ascii_digit() {
                return Err(ProtocolError::InvalidField {
                    field: "text",
                    detail: "key 'digit' requires single ASCII digit '0'..'9'".to_string(),
                });
            }
            Ok(KeyInput::Digit(bytes[0] - b'0'))
        }
        other => Err(ProtocolError::InvalidField {
            field: "key",
            detail: format!("unsupported key event '{other}'"),
        }),
    }
}

pub fn decode_request_line(line: &str) -> Result<IpcRequest, ProtocolError> {
    let obj = parse_root_object(line)?;
    let msg_type = req_str(&obj, "type")?;
    match msg_type {
        "hello" => Ok(IpcRequest::Hello {
            client: req_str(&obj, "client")?.to_string(),
            protocol_version: req_u32(&obj, "protocol_version")?,
            session_id: opt_str(&obj, "session_id")?.map(str::to_string),
        }),
        "key" => Ok(IpcRequest::Key {
            seq: req_u64(&obj, "seq")?,
            key: decode_key_input(&obj)?,
            mods: opt_u32(&obj, "mods")?.unwrap_or(0),
        }),
        "select" => Ok(IpcRequest::Select {
            seq: req_u64(&obj, "seq")?,
            index: req_usize(&obj, "index")?,
        }),
        "context" => Ok(IpcRequest::Context {
            before: req_str(&obj, "before")?.to_string(),
            after: opt_str(&obj, "after")?.unwrap_or("").to_string(),
        }),
        "privacy" => Ok(IpcRequest::Privacy {
            enabled: req_bool(&obj, "enabled")?,
        }),
        "reset" => Ok(IpcRequest::Reset {
            seq: req_u64(&obj, "seq")?,
        }),
        "config" => Ok(IpcRequest::Config(EngineConfig {
            space_commits: req_bool(&obj, "space_commits")?,
            smart_spacing: req_bool(&obj, "smart_spacing")?,
            max_candidates: req_usize(&obj, "max_candidates")?,
            enable_phonetic: req_bool(&obj, "enable_phonetic")?,
            enable_neural: req_bool(&obj, "enable_neural")?,
        })),
        "ping" => Ok(IpcRequest::Ping {
            id: opt_u64(&obj, "id")?,
        }),
        "shutdown" => Ok(IpcRequest::Shutdown),
        other => Err(ProtocolError::UnknownMessageType(other.to_string())),
    }
}

fn decode_candidate(val: &JsonValue) -> Result<Candidate, ProtocolError> {
    if !val.is_object() {
        return Err(ProtocolError::InvalidField {
            field: "candidates",
            detail: "each candidate must be a JSON object".to_string(),
        });
    }
    let text = req_str(val, "text")?.to_string();
    let kind_str = req_str(val, "kind")?;
    let kind = CandidateKind::from_str_name(kind_str).ok_or_else(|| ProtocolError::InvalidField {
        field: "kind",
        detail: format!("unknown CandidateKind '{kind_str}'"),
    })?;
    let comment = opt_str(val, "comment")?.map(str::to_string);
    let score = req_f32(val, "score")?;
    let consumed_chars = req_usize(val, "consumed_chars")?;
    let trailing_space = req_bool(val, "trailing_space")?;
    Ok(Candidate {
        text,
        kind,
        comment,
        score,
        consumed_chars,
        trailing_space,
    })
}

fn decode_candidates(obj: &JsonValue) -> Result<Vec<Candidate>, ProtocolError> {
    let val = obj
        .get("candidates")
        .ok_or(ProtocolError::MissingField("candidates"))?;
    let arr = val.as_array().ok_or_else(|| ProtocolError::InvalidField {
        field: "candidates",
        detail: "expected JSON array".to_string(),
    })?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        out.push(decode_candidate(item)?);
    }
    Ok(out)
}

pub fn decode_response_line(line: &str) -> Result<IpcResponse, ProtocolError> {
    let obj = parse_root_object(line)?;
    let msg_type = req_str(&obj, "type")?;
    match msg_type {
        "hello_ack" => {
            let caps_val = obj
                .get("capabilities")
                .ok_or(ProtocolError::MissingField("capabilities"))?;
            let caps_arr = caps_val.as_array().ok_or_else(|| ProtocolError::InvalidField {
                field: "capabilities",
                detail: "expected array of strings".to_string(),
            })?;
            let mut capabilities = Vec::with_capacity(caps_arr.len());
            for item in caps_arr {
                let s = item.as_str().ok_or_else(|| ProtocolError::InvalidField {
                    field: "capabilities",
                    detail: "each capability must be a string".to_string(),
                })?;
                capabilities.push(s.to_string());
            }
            Ok(IpcResponse::HelloAck {
                protocol_version: req_u32(&obj, "protocol_version")?,
                version: req_str(&obj, "version")?.to_string(),
                capabilities,
            })
        }
        "state" => Ok(IpcResponse::State(PreeditState {
            seq: req_u64(&obj, "seq")?,
            revision: req_u32(&obj, "revision")?,
            preedit: req_str(&obj, "preedit")?.to_string(),
            cursor: req_usize(&obj, "cursor")?,
            candidates: decode_candidates(&obj)?,
            passthrough: req_bool(&obj, "passthrough")?,
            final_l3: req_bool(&obj, "final_l3")?,
        })),
        "commit" => Ok(IpcResponse::Commit(CommitAction {
            seq: req_u64(&obj, "seq")?,
            text: req_str(&obj, "text")?.to_string(),
            delete_before: req_usize(&obj, "delete_before")?,
            remaining_preedit: req_str(&obj, "remaining_preedit")?.to_string(),
            candidates: decode_candidates(&obj)?,
        })),
        "pong" => Ok(IpcResponse::Pong {
            version: req_str(&obj, "version")?.to_string(),
            id: opt_u64(&obj, "id")?,
        }),
        "error" => Ok(IpcResponse::Error {
            seq: opt_u64(&obj, "seq")?,
            code: req_str(&obj, "code")?.to_string(),
            message: req_str(&obj, "message")?.to_string(),
        }),
        other => Err(ProtocolError::UnknownMessageType(other.to_string())),
    }
}

fn ensure_line_fits(s: String) -> Result<String, ProtocolError> {
    if s.len() > MAX_IPC_LINE_BYTES {
        Err(ProtocolError::LineTooLarge {
            len: s.len(),
            max: MAX_IPC_LINE_BYTES,
        })
    } else {
        Ok(s)
    }
}

pub fn encode_request_line(req: &IpcRequest) -> Result<String, ProtocolError> {
    let mut out = String::with_capacity(128);
    match req {
        IpcRequest::Hello {
            client,
            protocol_version,
            session_id,
        } => {
            out.push_str(r#"{"type":"hello","client":"#);
            write_json_string(&mut out, client);
            let _ = write!(out, r#","protocol_version":{protocol_version}"#);
            if let Some(sid) = session_id {
                out.push_str(r#","session_id":"#);
                write_json_string(&mut out, sid);
            }
            out.push('}');
        }
        IpcRequest::Key { seq, key, mods } => {
            let _ = write!(out, r#"{{"type":"key","seq":{seq},"key":"#);
            match key {
                KeyInput::Char(ch) => {
                    out.push_str(r#""char","text":"#);
                    let mut buf = [0u8; 4];
                    write_json_string(&mut out, ch.encode_utf8(&mut buf));
                }
                KeyInput::Space => out.push_str(r#""space""#),
                KeyInput::Backspace => out.push_str(r#""backspace""#),
                KeyInput::Enter => out.push_str(r#""enter""#),
                KeyInput::Escape => out.push_str(r#""escape""#),
                KeyInput::Digit(d) => {
                    out.push_str(r#""digit","text":"#);
                    let digit_ch = char::from(b'0' + (*d).min(9));
                    let mut buf = [0u8; 4];
                    write_json_string(&mut out, digit_ch.encode_utf8(&mut buf));
                }
            }
            if *mods != 0 {
                let _ = write!(out, r#","mods":{mods}"#);
            }
            out.push('}');
        }
        IpcRequest::Select { seq, index } => {
            let _ = write!(out, r#"{{"type":"select","seq":{seq},"index":{index}}}"#);
        }
        IpcRequest::Context { before, after } => {
            out.push_str(r#"{"type":"context","before":"#);
            write_json_string(&mut out, before);
            out.push_str(r#","after":"#);
            write_json_string(&mut out, after);
            out.push('}');
        }
        IpcRequest::Privacy { enabled } => {
            let _ = write!(out, r#"{{"type":"privacy","enabled":{enabled}}}"#);
        }
        IpcRequest::Reset { seq } => {
            let _ = write!(out, r#"{{"type":"reset","seq":{seq}}}"#);
        }
        IpcRequest::Config(cfg) => {
            let _ = write!(
                out,
                r#"{{"type":"config","space_commits":{},"smart_spacing":{},"max_candidates":{},"enable_phonetic":{},"enable_neural":{}}}"#,
                cfg.space_commits,
                cfg.smart_spacing,
                cfg.max_candidates,
                cfg.enable_phonetic,
                cfg.enable_neural
            );
        }
        IpcRequest::Ping { id } => {
            if let Some(v) = id {
                let _ = write!(out, r#"{{"type":"ping","id":{v}}}"#);
            } else {
                out.push_str(r#"{"type":"ping"}"#);
            }
        }
        IpcRequest::Shutdown => out.push_str(r#"{"type":"shutdown"}"#),
    }
    ensure_line_fits(out)
}

fn write_candidates(out: &mut String, candidates: &[Candidate]) {
    out.push('[');
    for (idx, cand) in candidates.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str(r#"{"text":"#);
        write_json_string(out, &cand.text);
        out.push_str(r#","kind":"#);
        write_json_string(out, cand.kind.as_str());
        if let Some(comment) = &cand.comment {
            out.push_str(r#","comment":"#);
            write_json_string(out, comment);
        }
        out.push_str(r#","score":"#);
        write_json_f32(out, cand.score);
        let _ = write!(
            out,
            r#","consumed_chars":{},"trailing_space":{}}}"#,
            cand.consumed_chars, cand.trailing_space
        );
    }
    out.push(']');
}

fn serialize_state_line(st: &PreeditState, cands: &[Candidate]) -> String {
    let mut out = String::with_capacity(256);
    let _ = write!(
        out,
        r#"{{"type":"state","seq":{},"revision":{},"preedit":"#,
        st.seq, st.revision
    );
    write_json_string(&mut out, &st.preedit);
    let _ = write!(out, r#","cursor":{},"candidates":"#, st.cursor);
    write_candidates(&mut out, cands);
    let _ = write!(
        out,
        r#","passthrough":{},"final_l3":{}}}"#,
        st.passthrough, st.final_l3
    );
    out
}

fn serialize_commit_line(cm: &CommitAction, cands: &[Candidate]) -> String {
    let mut out = String::with_capacity(256);
    let _ = write!(out, r#"{{"type":"commit","seq":{},"text":"#, cm.seq);
    write_json_string(&mut out, &cm.text);
    let _ = write!(
        out,
        r#","delete_before":{},"remaining_preedit":"#,
        cm.delete_before
    );
    write_json_string(&mut out, &cm.remaining_preedit);
    out.push_str(r#","candidates":"#);
    write_candidates(&mut out, cands);
    out.push('}');
    out
}

fn shrink_candidates_to_fit<F>(original: &[Candidate], serialize: F) -> Result<String, ProtocolError>
where
    F: Fn(&[Candidate]) -> String,
{
    let full = serialize(original);
    if full.len() <= MAX_IPC_LINE_BYTES {
        return Ok(full);
    }

    // 超 64 KiB 时先剥离候选注释，再从尾部裁剪次要候选并尽量保留末尾 Raw 兜底项
    let mut trimmed: Vec<Candidate> = original
        .iter()
        .cloned()
        .map(|mut c| {
            c.comment = None;
            c
        })
        .collect();
    let no_comment = serialize(&trimmed);
    if no_comment.len() <= MAX_IPC_LINE_BYTES {
        return Ok(no_comment);
    }

    while !trimmed.is_empty() {
        let len = trimmed.len();
        if len > 1 && trimmed[len - 1].kind == CandidateKind::Raw {
            trimmed.remove(len - 2);
        } else {
            trimmed.pop();
        }
        let attempt = serialize(&trimmed);
        if attempt.len() <= MAX_IPC_LINE_BYTES {
            return Ok(attempt);
        }
    }

    Err(ProtocolError::LineTooLarge {
        len: full.len(),
        max: MAX_IPC_LINE_BYTES,
    })
}

pub fn encode_response_line(resp: &IpcResponse) -> Result<String, ProtocolError> {
    match resp {
        IpcResponse::HelloAck {
            protocol_version,
            version,
            capabilities,
        } => {
            let mut out = String::with_capacity(128);
            let _ = write!(out, r#"{{"type":"hello_ack","protocol_version":{protocol_version},"version":"#);
            write_json_string(&mut out, version);
            out.push_str(r#","capabilities":["#);
            for (idx, cap) in capabilities.iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                write_json_string(&mut out, cap);
            }
            out.push_str("]}");
            ensure_line_fits(out)
        }
        IpcResponse::State(st) => {
            shrink_candidates_to_fit(&st.candidates, |cands| serialize_state_line(st, cands))
        }
        IpcResponse::Commit(cm) => {
            shrink_candidates_to_fit(&cm.candidates, |cands| serialize_commit_line(cm, cands))
        }
        IpcResponse::Pong { version, id } => {
            // 当 id 为 None 时不输出 "id" 字段，严格保持 Phase 0 兼容响应格式
            let mut out = String::with_capacity(64);
            out.push_str(r#"{"type":"pong","version":"#);
            write_json_string(&mut out, version);
            if let Some(v) = id {
                let _ = write!(out, r#","id":{v}"#);
            }
            out.push('}');
            ensure_line_fits(out)
        }
        IpcResponse::Error { seq, code, message } => {
            let mut out = String::with_capacity(128);
            out.push_str(r#"{"type":"error""#);
            if let Some(s) = seq {
                let _ = write!(out, r#","seq":{s}"#);
            }
            out.push_str(r#","code":"#);
            write_json_string(&mut out, code);
            out.push_str(r#","message":"#);
            write_json_string(&mut out, message);
            out.push('}');
            ensure_line_fits(out)
        }
    }
}

/// 当请求校验或 JSON 解析失败时尽力提取 `"seq"`，便于客户端精准定位过期/错误包。
pub fn extract_seq_best_effort(line: &str) -> Option<u64> {
    if line.len() > MAX_IPC_LINE_BYTES {
        return None;
    }
    let stripped = line
        .strip_suffix("\r\n")
        .or_else(|| line.strip_suffix('\n'))
        .unwrap_or(line);
    if let Ok(val) = JsonValue::parse(stripped) {
        if let Some(seq) = val.get("seq").and_then(JsonValue::as_u64) {
            return Some(seq);
        }
    }
    let key_idx = stripped.find(r#""seq""#)?;
    let after_key = &stripped[key_idx + 5..];
    let colon_idx = after_key.find(':')?;
    let digits_start = after_key[colon_idx + 1..].trim_start();
    let digits_len = digits_start
        .bytes()
        .take_while(|b| b.is_ascii_digit())
        .count();
    if digits_len == 0 {
        return None;
    }
    digits_start[..digits_len].parse::<u64>().ok()
}
