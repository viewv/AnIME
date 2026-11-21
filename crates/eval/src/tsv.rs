//! TSV 评测集解析器，严格遵循 .agents/contracts/eval-schema.md 的 5 列格式与 7 类枚举。

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
    pub expected: String,
    pub notes: String,
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

        rows.push(EvalRow {
            category,
            context: parts[1].to_string(),
            input: input.to_string(),
            expected: expected.to_string(),
            notes: parts[4].trim().to_string(),
        });
    }
    Ok(rows)
}
