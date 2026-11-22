//! 三层可扩展词典组合器（L1 Base + 外部扩展词典 + 运行时动态热词层）。
//!
//! 解决静态编译词典对现代术语（如 `kubernetes`、`wayland`）、领域专名及用户现学热词无法热插拔的问题：
//! - **Layer 1（Base 只读底座）**：主 `ANIMELEX` v1 紧凑词典 + 内置缩写/大小写兜底；
//! - **Layer 2（外部扩展层）**：按文件名确定性加载外部 `.animelex.bin`、`.tsv` 或纯文本 `.txt` 词表；
//! - **Layer 3（动态热词层）**：线程安全 `RwLock` 托管的运行时增量词汇（可由 Task 03 的 `user.db` 启动预热并在选词后实时写入），
//!   惰性重建小型 `Lexicon` 索引以直接复用完整 `CompactTrie` 前缀补全、QWERTY 模糊纠错与 Double Metaphone 能力。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::format::{DEFAULT_EXTENSION_ZIPF, LexiconError};
use crate::{Lexicon, LexiconCandidate, LexiconMatchKind, LexiconProvider, QueryOptions};

/// 新学用户词的默认基础 Zipf 词频，确保在不劫持高频常见词的前提下作为合法领域词稳定召回。
pub const DEFAULT_DYNAMIC_BASE_ZIPF: f32 = 4.2;

/// 用户高频词从 SQLite `user_words.freq` 映射到的 Zipf 上限。
pub const DEFAULT_DYNAMIC_MAX_ZIPF: f32 = 7.2;

/// 将 Task 03 `user_words.freq`（累计确认次数）对数平滑映射为 Zipf 词频。
///
/// 原因注释：使用 `log2(freq)` 平滑增长，使第 1 次学会的新词具备基础可召回权重（4.2），
/// 多次确认后稳步提升，同时封顶 7.2 以防单个人名/热词过度压制超高频功能词（如 `the`/`to`）。
#[inline]
pub fn user_freq_to_zipf(freq: u32) -> f32 {
    let f = (freq.max(1)) as f32;
    (DEFAULT_DYNAMIC_BASE_ZIPF + 0.65 * f.log2()).min(DEFAULT_DYNAMIC_MAX_ZIPF)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExtensionFormat {
    Animelex,
    Wordlist,
}

fn classify_extension_format(path: &Path) -> Option<ExtensionFormat> {
    let lower = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if lower.ends_with(".animelex.bin") || lower.ends_with(".bin") {
        Some(ExtensionFormat::Animelex)
    } else if lower.ends_with(".tsv") || lower.ends_with(".txt") {
        Some(ExtensionFormat::Wordlist)
    } else {
        None
    }
}

#[inline]
fn sanitize_bias(bias: f32) -> f32 {
    if bias.is_finite() { bias } else { 0.0 }
}

/// Layer 2 外部只读扩展词典条目。
#[derive(Debug, Clone)]
pub struct ExtensionLayer {
    name: String,
    lexicon: Lexicon,
    score_bias: f32,
}

impl ExtensionLayer {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn lexicon(&self) -> &Lexicon {
        &self.lexicon
    }

    pub fn score_bias(&self) -> f32 {
        self.score_bias
    }
}

#[derive(Debug, Clone)]
struct DynamicEntry {
    word: String,
    freq: u32,
    zipf: f32,
}

#[derive(Debug, Default)]
struct DynamicState {
    entries: HashMap<String, DynamicEntry>,
    cached: Option<Lexicon>,
    dirty: bool,
}

/// 三层统一实现 [`LexiconProvider`] 的可扩展 L1 词典引擎。
#[derive(Debug)]
pub struct LayeredLexicon {
    base: Lexicon,
    extensions: Vec<ExtensionLayer>,
    dynamic: RwLock<DynamicState>,
    dynamic_score_bias: f32,
}

impl LayeredLexicon {
    pub fn new(base: Lexicon) -> Self {
        Self {
            base,
            extensions: Vec::new(),
            dynamic: RwLock::new(DynamicState::default()),
            dynamic_score_bias: 0.0,
        }
    }

    pub fn from_animelex_file(path: &Path) -> Result<Self, LexiconError> {
        Ok(Self::new(Lexicon::from_animelex_file(path)?))
    }

    pub fn with_dynamic_score_bias(mut self, bias: f32) -> Self {
        self.dynamic_score_bias = sanitize_bias(bias);
        self
    }

    pub fn set_dynamic_score_bias(&mut self, bias: f32) {
        self.dynamic_score_bias = sanitize_bias(bias);
    }

    pub fn base(&self) -> &Lexicon {
        &self.base
    }

    pub fn extensions(&self) -> &[ExtensionLayer] {
        &self.extensions
    }

    fn insert_or_replace_extension(
        &mut self,
        name: String,
        lexicon: Lexicon,
        score_bias: f32,
    ) -> usize {
        let layer = ExtensionLayer {
            name,
            lexicon,
            score_bias: sanitize_bias(score_bias),
        };
        if let Some(pos) = self.extensions.iter().position(|e| e.name == layer.name) {
            self.extensions[pos] = layer;
            pos
        } else {
            self.extensions.push(layer);
            self.extensions.len() - 1
        }
    }

    /// 挂载（或按同名覆盖）一个外部只读扩展词典。
    pub fn add_extension(&mut self, name: impl Into<String>, lexicon: Lexicon, score_bias: f32) {
        let _ = self.insert_or_replace_extension(name.into(), lexicon, score_bias);
    }

    /// 按名称移除外部扩展词典；若存在并返回 `true`。
    pub fn remove_extension(&mut self, name: &str) -> bool {
        let before = self.extensions.len();
        self.extensions.retain(|e| e.name != name);
        self.extensions.len() != before
    }

    /// 从单个外部文件加载并挂载扩展词典：
    /// 支持 `.animelex.bin`/`.bin`（二进制）与 `.tsv`/`.txt`（纯文本或 TSV 词表）。
    pub fn load_extension_file(
        &mut self,
        path: &Path,
        score_bias: f32,
    ) -> Result<&ExtensionLayer, LexiconError> {
        let format = classify_extension_format(path).ok_or_else(|| {
            LexiconError::UnsupportedFormat(format!(
                "expected .animelex.bin/.bin/.tsv/.txt extension for '{}'",
                path.display()
            ))
        })?;
        let lex = match format {
            ExtensionFormat::Animelex => Lexicon::from_animelex_file(path)?,
            ExtensionFormat::Wordlist => Lexicon::from_wordlist_file(path, DEFAULT_EXTENSION_ZIPF)?,
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let idx = self.insert_or_replace_extension(name, lex, score_bias);
        Ok(&self.extensions[idx])
    }

    /// 按文件名字典序确定性加载目录中的所有受支持扩展词典，跳过隐藏文件与子目录；
    /// 若目录不存在则安全返回 `Ok(0)`（零平台硬编码，调用方负责解析 OS 路径）。
    pub fn load_extension_dir(
        &mut self,
        dir: &Path,
        score_bias: f32,
    ) -> Result<usize, LexiconError> {
        if !dir.exists() {
            return Ok(0);
        }

        let mut candidates: Vec<(String, PathBuf)> = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(fname_os) = path.file_name() else {
                continue;
            };
            let fname = fname_os.to_string_lossy().into_owned();
            if fname.starts_with('.') || classify_extension_format(&path).is_none() {
                continue;
            }
            candidates.push((fname, path));
        }

        candidates.sort_by(|a, b| a.0.cmp(&b.0));
        let mut loaded = 0usize;
        for (_, path) in candidates {
            self.load_extension_file(&path, score_bias)?;
            loaded += 1;
        }
        Ok(loaded)
    }

    fn apply_learn_locked(
        base: &Lexicon,
        entries: &mut HashMap<String, DynamicEntry>,
        word: &str,
        freq_increment: u32,
    ) -> bool {
        let trimmed = word.trim();
        if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) || freq_increment == 0 {
            return false;
        }
        let key = trimmed.to_ascii_lowercase();
        let has_internal_upper = trimmed.chars().skip(1).any(|c| c.is_ascii_uppercase());
        // 原因注释：用户在句首上屏普通词（如 "Now" / "So" / "The"）时会被句首自动大写规则转为首字母大写并记入 user.db；
        // 若主词典已收录该词为纯小写普通词汇且无词内大写（非 TypeScript/iPhone 等），动态层必须以纯小写归档，
        // 避免将普通词误标记为专有名词导致句中敲 "now" 恒变成大写 "Now"。
        let canonical = if base.has_lowercase_canonical(&key) && !has_internal_upper {
            key.clone()
        } else {
            trimmed.to_string()
        };
        let canonical_has_upper = canonical.bytes().any(|b| b.is_ascii_uppercase());
        match entries.get_mut(&key) {
            Some(entry) => {
                entry.freq = entry.freq.saturating_add(freq_increment);
                entry.zipf = entry.zipf.max(user_freq_to_zipf(entry.freq));
                // 仅当原词为纯小写而新词确实属于带大小写特征的专名/混写词时升级规范词形
                if !entry.word.bytes().any(|b| b.is_ascii_uppercase()) && canonical_has_upper {
                    entry.word = canonical;
                }
            }
            None => {
                entries.insert(
                    key,
                    DynamicEntry {
                        word: canonical,
                        freq: freq_increment,
                        zipf: user_freq_to_zipf(freq_increment),
                    },
                );
            }
        }
        true
    }

    /// 增量学习用户新词或常用词（如 `user.db` 在确认上屏后实时同步），线程安全且不锁死只读快路径。
    pub fn learn_word(&self, word: &str, freq_increment: u32) -> bool {
        let trimmed = word.trim();
        if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) || freq_increment == 0 {
            return false;
        }
        let mut guard = self.dynamic.write().unwrap_or_else(|e| e.into_inner());
        let changed =
            Self::apply_learn_locked(&self.base, &mut guard.entries, trimmed, freq_increment);
        if changed {
            guard.dirty = true;
        }
        changed
    }

    /// 直接写入或覆写动态热词及其显式 `zipf` 权重。
    pub fn upsert_dynamic_word(&self, word: &str, zipf: f32) -> bool {
        let trimmed = word.trim();
        if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) || !zipf.is_finite() {
            return false;
        }
        let clamped_zipf = zipf.clamp(0.0, 9.0);
        let key = trimmed.to_ascii_lowercase();
        let mut guard = self.dynamic.write().unwrap_or_else(|e| e.into_inner());
        match guard.entries.get_mut(&key) {
            Some(entry) => {
                entry.word = trimmed.to_string();
                entry.zipf = clamped_zipf;
                entry.freq = entry.freq.max(1);
            }
            None => {
                guard.entries.insert(
                    key,
                    DynamicEntry {
                        word: trimmed.to_string(),
                        freq: 1,
                        zipf: clamped_zipf,
                    },
                );
            }
        }
        guard.dirty = true;
        true
    }

    /// 启动期单次加锁批量预热用户词频表（供 Task 03 从 SQLite `user_words` 一次性导入）。
    pub fn preload_dynamic_words<I, S>(&self, entries: I) -> usize
    where
        I: IntoIterator<Item = (S, u32)>,
        S: Into<String>,
    {
        let mut guard = self.dynamic.write().unwrap_or_else(|e| e.into_inner());
        let mut accepted = 0usize;
        for (word, freq) in entries {
            let owned = word.into();
            if Self::apply_learn_locked(&self.base, &mut guard.entries, &owned, freq) {
                accepted += 1;
            }
        }
        if accepted > 0 {
            guard.dirty = true;
        }
        accepted
    }

    /// 从 Layer 3 动态热词层移除指定词条（大小写不敏感）。
    pub fn remove_dynamic_word(&self, word: &str) -> bool {
        let key = word.trim().to_ascii_lowercase();
        if key.is_empty() {
            return false;
        }
        let mut guard = self.dynamic.write().unwrap_or_else(|e| e.into_inner());
        let removed = guard.entries.remove(&key).is_some();
        if removed {
            if guard.entries.is_empty() {
                guard.cached = None;
                guard.dirty = false;
            } else {
                guard.dirty = true;
            }
        }
        removed
    }

    /// 清空全部动态热词及缓存索引。
    pub fn clear_dynamic_words(&self) {
        let mut guard = self.dynamic.write().unwrap_or_else(|e| e.into_inner());
        guard.entries.clear();
        guard.cached = None;
        guard.dirty = false;
    }

    pub fn dynamic_word_count(&self) -> usize {
        self.dynamic
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .len()
    }

    /// 导出当前动态热词快照（按 `zipf` 降序、词形字典序升序排列）。
    pub fn dynamic_words(&self) -> Vec<(String, f32)> {
        let guard = self.dynamic.read().unwrap_or_else(|e| e.into_inner());
        let mut list: Vec<(String, f32)> = guard
            .entries
            .values()
            .map(|e| (e.word.clone(), e.zipf))
            .collect();
        list.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        list
    }

    /// 返回三层合计词条容量（按层统计之和，未跨层去重）。
    pub fn total_word_count(&self) -> usize {
        self.base.len()
            + self
                .extensions
                .iter()
                .map(|ext| ext.lexicon.len())
                .sum::<usize>()
            + self.dynamic_word_count()
    }

    fn has_exact_lower(&self, lower: &str) -> bool {
        if self.base.contains_exact_lower(lower)
            || self
                .extensions
                .iter()
                .any(|ext| ext.lexicon.contains_exact_lower(lower))
        {
            return true;
        }
        self.dynamic
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .contains_key(lower)
    }

    fn query_dynamic(
        &self,
        raw: &str,
        opts: &QueryOptions,
        suppress_heavy_fuzzy: bool,
    ) -> Vec<LexiconCandidate> {
        {
            let guard = self.dynamic.read().unwrap_or_else(|e| e.into_inner());
            if guard.entries.is_empty() {
                return Vec::new();
            }
            if !guard.dirty {
                if let Some(lex) = guard.cached.as_ref() {
                    return lex.query_layer(raw, opts, false, suppress_heavy_fuzzy);
                }
            }
        }

        // 读锁必须在上一作用域显式释放后才可升级为写锁，防止同线程 RwLock 死锁
        let mut guard = self.dynamic.write().unwrap_or_else(|e| e.into_inner());
        if guard.entries.is_empty() {
            guard.cached = None;
            guard.dirty = false;
            return Vec::new();
        }
        if guard.dirty || guard.cached.is_none() {
            let rebuilt = Lexicon::from_word_zipfs(
                guard.entries.values().map(|e| (e.word.as_str(), e.zipf)),
                Vec::new(),
            );
            guard.cached = Some(rebuilt);
            guard.dirty = false;
        }
        guard
            .cached
            .as_ref()
            .map(|lex| lex.query_layer(raw, opts, false, suppress_heavy_fuzzy))
            .unwrap_or_default()
    }

    fn merge_candidate(
        best: &mut HashMap<String, LexiconCandidate>,
        cand: LexiconCandidate,
        prefer_incoming_on_tie: bool,
    ) {
        let key = cand.word.to_ascii_lowercase();
        match best.get_mut(&key) {
            Some(existing) => {
                let replace = if (cand.score - existing.score).abs() <= 1e-5 {
                    match (
                        cand.kind == LexiconMatchKind::Exact,
                        existing.kind == LexiconMatchKind::Exact,
                    ) {
                        (true, false) => true,
                        (false, true) => false,
                        _ => prefer_incoming_on_tie,
                    }
                } else {
                    cand.score > existing.score
                };
                if replace {
                    *existing = cand;
                }
            }
            None => {
                best.insert(key, cand);
            }
        }
    }
}

impl LexiconProvider for LayeredLexicon {
    fn query(&self, raw: &str, opts: &QueryOptions) -> Vec<LexiconCandidate> {
        let trimmed = raw.trim();
        if trimmed.is_empty() || opts.limit == 0 {
            return Vec::new();
        }

        let has_dynamic = !self
            .dynamic
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .is_empty();
        // 零额外开销快路径：当未挂载扩展且尚无动态热词时直接透传主词典
        if self.extensions.is_empty() && !has_dynamic {
            return self.base.query(raw, opts);
        }

        let needle = trimmed.to_ascii_lowercase();
        // 原因注释：只要任一扩展或热词层已精确命中该词（如 kubernetes / Antigravity），
        // 主层必须切入轻量快路径，否则会对已合法新词耗费 ~400 us 执行无意义重度纠错与语音桶扫描。
        let any_layer_exact = self.has_exact_lower(&needle);
        let mut best: HashMap<String, LexiconCandidate> = HashMap::with_capacity(opts.limit * 4);

        for cand in self.base.query_layer(raw, opts, true, any_layer_exact) {
            Self::merge_candidate(&mut best, cand, false);
        }

        for ext in &self.extensions {
            for mut cand in ext.lexicon.query_layer(raw, opts, false, any_layer_exact) {
                cand.score += ext.score_bias;
                Self::merge_candidate(&mut best, cand, true);
            }
        }

        if has_dynamic {
            for mut cand in self.query_dynamic(raw, opts, any_layer_exact) {
                cand.score += self.dynamic_score_bias;
                Self::merge_candidate(&mut best, cand, true);
            }
        }

        let mut out: Vec<LexiconCandidate> = best.into_values().collect();
        out.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.edit_cost.total_cmp(&b.edit_cost))
                .then_with(|| a.word.cmp(&b.word))
        });
        out.truncate(opts.limit);
        out
    }

    fn contextual_confusables(&self, raw: &str, limit: usize) -> Vec<crate::ConfusableCandidate> {
        self.base.contextual_confusables(raw, limit)
    }

    fn closed_token_neighbors(&self, raw: &str, limit: usize) -> Vec<LexiconCandidate> {
        self.base.closed_token_neighbors(raw, limit)
    }

    fn segment_runon(&self, raw: &str) -> Option<Vec<String>> {
        let trimmed = raw.trim();
        if self.has_exact_lower(&trimmed.to_ascii_lowercase()) {
            return None;
        }
        self.base.segment_runon(raw)
    }
}
