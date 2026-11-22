//! 平台无关领域类型与 IPC 契约模型（遵守 .agents/contracts/core-api.md 与 ipc-protocol.md）。

pub mod ipc;
mod json;

pub use crate::ipc::{
    IpcRequest, IpcResponse, MAX_IPC_LINE_BYTES, ProtocolError, decode_request_line,
    decode_response_line, encode_request_line, encode_response_line, extract_seq_best_effort,
};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    Exact,
    Completion,
    TypoCorrection,
    PhoneticCorrection,
    Sentence,
    Continuation,
    Raw,
}

impl CandidateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "Exact",
            Self::Completion => "Completion",
            Self::TypoCorrection => "TypoCorrection",
            Self::PhoneticCorrection => "PhoneticCorrection",
            Self::Sentence => "Sentence",
            Self::Continuation => "Continuation",
            Self::Raw => "Raw",
        }
    }

    pub fn from_str_name(raw: &str) -> Option<Self> {
        match raw {
            "Exact" => Some(Self::Exact),
            "Completion" => Some(Self::Completion),
            "TypoCorrection" => Some(Self::TypoCorrection),
            "PhoneticCorrection" => Some(Self::PhoneticCorrection),
            "Sentence" => Some(Self::Sentence),
            "Continuation" => Some(Self::Continuation),
            "Raw" => Some(Self::Raw),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub text: String,
    pub kind: CandidateKind,
    pub comment: Option<String>,
    pub score: f32,
    pub consumed_chars: usize,
    pub trailing_space: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyInput {
    Char(char),
    Space,
    Backspace,
    Enter,
    Escape,
    Digit(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineConfig {
    pub space_commits: bool,
    pub smart_spacing: bool,
    pub max_candidates: usize,
    pub enable_phonetic: bool,
    pub enable_neural: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            // 拼音式英文默认在预编辑区保留词间空格，按数字 1-9 上屏。
            space_commits: false,
            smart_spacing: true,
            max_candidates: 9,
            enable_phonetic: true,
            enable_neural: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextText {
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreeditState {
    pub seq: u64,
    pub revision: u32,
    pub preedit: String,
    pub cursor: usize,
    pub candidates: Vec<Candidate>,
    pub passthrough: bool,
    pub final_l3: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommitAction {
    pub seq: u64,
    pub text: String,
    pub delete_before: usize,
    pub remaining_preedit: String,
    pub candidates: Vec<Candidate>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_matches_contract() {
        let cfg = EngineConfig::default();
        assert_eq!(PROTOCOL_VERSION, 1);
        assert!(!cfg.space_commits);
        assert!(cfg.smart_spacing);
        assert_eq!(cfg.max_candidates, 9);
        assert!(cfg.enable_phonetic);
        assert!(cfg.enable_neural);
    }

    #[test]
    fn candidate_kind_wire_names_match_contract() {
        assert_eq!(CandidateKind::Exact.as_str(), "Exact");
        assert_eq!(CandidateKind::Completion.as_str(), "Completion");
        assert_eq!(CandidateKind::TypoCorrection.as_str(), "TypoCorrection");
        assert_eq!(
            CandidateKind::PhoneticCorrection.as_str(),
            "PhoneticCorrection"
        );
        assert_eq!(CandidateKind::Sentence.as_str(), "Sentence");
        assert_eq!(CandidateKind::Continuation.as_str(), "Continuation");
        assert_eq!(CandidateKind::Raw.as_str(), "Raw");
    }

    #[test]
    fn ipc_requests_and_responses_roundtrip_all_contract_variants() {
        let high_seq = (1u64 << 58) + 777;
        let requests = vec![
            IpcRequest::Hello {
                client: "fcitx5-anime".into(),
                protocol_version: PROTOCOL_VERSION,
                session_id: Some("wayland-1".into()),
            },
            IpcRequest::Key {
                seq: high_seq,
                key: KeyInput::Char('q'),
                mods: 2,
            },
            IpcRequest::Key {
                seq: 2,
                key: KeyInput::Space,
                mods: 0,
            },
            IpcRequest::Key {
                seq: 3,
                key: KeyInput::Backspace,
                mods: 0,
            },
            IpcRequest::Key {
                seq: 4,
                key: KeyInput::Enter,
                mods: 0,
            },
            IpcRequest::Key {
                seq: 5,
                key: KeyInput::Escape,
                mods: 0,
            },
            IpcRequest::Key {
                seq: 6,
                key: KeyInput::Digit(9),
                mods: 0,
            },
            IpcRequest::Select { seq: 7, index: 0 },
            IpcRequest::Context {
                before: "Line1\nThank you for \"your\" \t".into(),
                after: "\\end".into(),
            },
            IpcRequest::Privacy { enabled: true },
            IpcRequest::Reset { seq: 8 },
            IpcRequest::Config(EngineConfig {
                space_commits: true,
                smart_spacing: false,
                max_candidates: 7,
                enable_phonetic: true,
                enable_neural: false,
            }),
            IpcRequest::Ping { id: None },
            IpcRequest::Ping { id: Some(99) },
            IpcRequest::Shutdown,
        ];

        for req in requests {
            let wire = encode_request_line(&req).expect("encode request");
            assert!(!wire.contains('\n') && !wire.contains('\r'));
            let decoded = decode_request_line(&wire).expect("decode request");
            assert_eq!(decoded, req);
        }

        // Phase 0 pong 精确格式（不输出 "id"）
        let phase0_pong = IpcResponse::Pong {
            version: "0.1.0".into(),
            id: None,
        };
        assert_eq!(
            encode_response_line(&phase0_pong).expect("encode pong"),
            r#"{"type":"pong","version":"0.1.0"}"#
        );

        let responses = vec![
            IpcResponse::HelloAck {
                protocol_version: PROTOCOL_VERSION,
                version: "0.1.0".into(),
                capabilities: vec!["l1-fuzzy".into(), "l2-ngram".into(), "stdio".into()],
            },
            IpcResponse::State(PreeditState {
                seq: high_seq,
                revision: 1,
                preedit: "appie".into(),
                cursor: 5,
                candidates: vec![
                    Candidate {
                        text: "apple".into(),
                        kind: CandidateKind::TypoCorrection,
                        comment: Some("typo".into()),
                        score: -1.25,
                        consumed_chars: 5,
                        trailing_space: true,
                    },
                    Candidate {
                        text: "appie".into(),
                        kind: CandidateKind::Raw,
                        comment: None,
                        score: -999.0,
                        consumed_chars: 5,
                        trailing_space: false,
                    },
                ],
                passthrough: false,
                final_l3: true,
            }),
            IpcResponse::Commit(CommitAction {
                seq: 43,
                text: "apple ".into(),
                delete_before: 1,
                remaining_preedit: "pie".into(),
                candidates: vec![],
            }),
            phase0_pong,
            IpcResponse::Pong {
                version: "0.1.0".into(),
                id: Some(99),
            },
            IpcResponse::Error {
                seq: Some(12),
                code: "invalid_field".into(),
                message: "bad\nmsg".into(),
            },
            IpcResponse::Error {
                seq: None,
                code: "invalid_json".into(),
                message: "eof".into(),
            },
        ];

        for resp in responses {
            let wire = encode_response_line(&resp).expect("encode response");
            assert!(!wire.contains('\n') && !wire.contains('\r'));
            let decoded = decode_response_line(&wire).expect("decode response");
            assert_eq!(decoded, resp);
        }
    }

    #[test]
    fn ipc_validates_unicode_limits_errors_and_best_effort_seq() {
        // UTF-16 代理对与控制转义解码
        let req = decode_request_line(
            r#"{"type":"context","before":"Hi \uD83D\uDE00\n\t\\","after":""}"#,
        )
        .expect("decode surrogate");
        assert_eq!(
            req,
            IpcRequest::Context {
                before: "Hi 😀\n\t\\".into(),
                after: "".into(),
            }
        );

        // context 的 after 可选，ping 的 id 可选
        assert_eq!(
            decode_request_line(r#"{"type":"context","before":"go "}"#).unwrap(),
            IpcRequest::Context {
                before: "go ".into(),
                after: "".into(),
            }
        );

        // 超过 64 KiB、未转义换行、深层嵌套、缺失字段与非法字段拒绝
        let huge = format!(r#"{{"type":"ping","pad":"{}"}}"#, "a".repeat(MAX_IPC_LINE_BYTES));
        assert!(matches!(
            decode_request_line(&huge),
            Err(ProtocolError::LineTooLarge { .. })
        ));
        assert!(matches!(
            decode_request_line("{\"type\":\"ping\",\n\"id\":1}"),
            Err(ProtocolError::ContainsUnescapedNewline)
        ));
        let deep = format!("{}1{}", "[".repeat(40), "]".repeat(40));
        assert!(matches!(
            decode_request_line(&deep),
            Err(ProtocolError::InvalidJson(_))
        ));
        assert!(matches!(
            decode_request_line(r#"{"type":"key","seq":1,"key":"char","text":"ab"}"#),
            Err(ProtocolError::InvalidField { field: "text", .. })
        ));
        assert!(matches!(
            decode_request_line(r#"{"type":"key","seq":1,"key":"digit","text":"x"}"#),
            Err(ProtocolError::InvalidField { field: "text", .. })
        ));
        assert!(matches!(
            decode_request_line(r#"{"type":"wat"}"#),
            Err(ProtocolError::UnknownMessageType(_))
        ));

        assert_eq!(
            extract_seq_best_effort(r#"{"type":"key","seq":42,"bad"#),
            Some(42)
        );

        // 超长候选列表自动裁剪至 <= 64 KiB
        let big_cands: Vec<Candidate> = (0..1000)
            .map(|i| Candidate {
                text: format!("word_{i}_{}", "x".repeat(60)),
                kind: CandidateKind::Completion,
                comment: Some("long-comment".into()),
                score: -(i as f32),
                consumed_chars: 5,
                trailing_space: true,
            })
            .collect();
        let line = encode_response_line(&IpcResponse::State(PreeditState {
            seq: 1,
            revision: 0,
            preedit: "word".into(),
            cursor: 4,
            candidates: big_cands,
            passthrough: false,
            final_l3: true,
        }))
        .expect("shrinks to fit 64 KiB");
        assert!(line.len() <= MAX_IPC_LINE_BYTES);
    }
}
