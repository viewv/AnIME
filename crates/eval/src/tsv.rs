//! TSV 评测集解析器，严格遵循 .agents/contracts/eval-schema.md 的 5 列格式、7 类枚举、
//! `||` 多参考答案与 `notes` 分组标签约定。

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    Exact,
    Prefix,
    TypoSimple,
    TypoPrefix,
    TypoPhonetic,
    ContextRealword,
    Sentence,
}

impl Category {
    #[cfg(test)]
    pub const ALL: [Self; 7] = [
        Self::Exact,
        Self::Prefix,
        Self::TypoSimple,
        Self::TypoPrefix,
        Self::TypoPhonetic,
        Self::ContextRealword,
        Self::Sentence,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Prefix => "prefix",
            Self::TypoSimple => "typo_simple",
            Self::TypoPrefix => "typo_prefix",
            Self::TypoPhonetic => "typo_phonetic",
            Self::ContextRealword => "context_realword",
            Self::Sentence => "sentence",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "exact" => Some(Self::Exact),
            "prefix" => Some(Self::Prefix),
            "typo_simple" => Some(Self::TypoSimple),
            "typo_prefix" => Some(Self::TypoPrefix),
            "typo_phonetic" => Some(Self::TypoPhonetic),
            "context_realword" => Some(Self::ContextRealword),
            "sentence" => Some(Self::Sentence),
            _ => None,
        }
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalRow {
    pub category: Category,
    pub context: String,
    pub input: String,
    /// `expected` 列原文（多参考时保留 `||` 分隔，供报告展示）。
    pub expected: String,
    /// 切分后的可接受参考答案，至少 1 个；任一命中即算命中。
    pub references: Vec<String>,
    pub notes: String,
    /// `notes` 首 token 形如 `<namespace>:<tag>` 时的分组标签。
    pub tag: Option<String>,
}

/// 按 `||` 切分多参考答案；任一参考为空视为格式错误，防止 `a ||` 这类笔误静默放宽匹配。
pub fn split_references(raw: &str) -> Result<Vec<String>, String> {
    let refs: Vec<String> = raw.split("||").map(|r| r.trim().to_string()).collect();
    if refs.iter().any(|r| r.is_empty()) {
        return Err(format!("empty reference in expected column '{raw}'"));
    }
    Ok(refs)
}

/// 提取 `notes` 开头的 `<namespace>:<tag>` 标签（`[a-z_]+:[a-z_]+`），否则返回 `None`。
pub fn parse_tag(notes: &str) -> Option<String> {
    let head = notes.split_whitespace().next()?;
    let (ns, tag) = head.split_once(':')?;
    let valid = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
    (valid(ns) && valid(tag)).then(|| head.to_string())
}

pub fn parse_tsv(content: &str) -> Result<Vec<EvalRow>, String> {
    if content.starts_with('\u{feff}') {
        return Err("eval TSV must be UTF-8 without BOM".to_string());
    }

    let mut rows = Vec::new();
    for (line_no, raw) in content.lines().enumerate() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = raw.split('\t').collect();
        if parts.len() != 5 {
            return Err(format!(
                "line {}: expected 5 tab-separated columns (category, context, input, expected, notes), got {}",
                line_no + 1,
                parts.len()
            ));
        }

        let category = Category::parse(parts[0]).ok_or_else(|| {
            format!(
                "line {}: unknown category '{}' (allowed: exact, prefix, typo_simple, typo_prefix, typo_phonetic, context_realword, sentence)",
                line_no + 1,
                parts[0].trim()
            )
        })?;

        let input = parts[2];
        if input.is_empty() {
            return Err(format!("line {}: input column must not be empty", line_no + 1));
        }

        let expected = parts[3].trim();
        if expected.is_empty() {
            return Err(format!(
                "line {}: expected column must not be empty",
                line_no + 1
            ));
        }

        let references =
            split_references(expected).map_err(|e| format!("line {}: {e}", line_no + 1))?;
        let notes = parts[4].trim().to_string();
        let tag = parse_tag(&notes);

        rows.push(EvalRow {
            category,
            context: parts[1].to_string(),
            input: input.to_string(),
            expected: expected.to_string(),
            references,
            notes,
            tag,
        });
    }
    Ok(rows)
}
