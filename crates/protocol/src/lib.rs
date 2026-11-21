//! 平台无关领域类型与 IPC 契约模型（遵守 .agents/contracts/core-api.md 与 ipc-protocol.md）。

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
}
