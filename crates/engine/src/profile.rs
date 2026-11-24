//! 分模式打分 / 召回参数（`ModeProfile`）：单词模式与整句模式的瓶颈不同（单词模式受排序限制，
//! 整句模式受召回限制），因此两种模式各持一份可独立调参的参数集，由 `EngineConfig.space_commits` 选择。
//!
//! # 两种模式
//! - **单词模式**（`space_commits = true`）：空格上屏首选，预编辑通常只有一个 token；
//!   但无空格连打（`iwanttogo`）仍会在单 token 内切分并走词格，所以连打 / 词格参数在本模式下同样生效。
//! - **整句模式**（`space_commits = false`）：空格留在预编辑区，多 token 由 L2 词格整句解码。
//!
//! # 如何按模式调参
//! 1. 编译期：只改 [`WORD_PROFILE`] 或 [`SENTENCE_PROFILE`] 自己的字段，其余继承 [`BASELINE_PROFILE`]：
//!    ```ignore
//!    pub const WORD_PROFILE: ModeProfile = ModeProfile {
//!        confusables: ConfusableProfile { adjacent_key_single_token_min_penalty: 3.5, ..BASELINE_PROFILE.confusables },
//!        ..BASELINE_PROFILE
//!    };
//!    ```
//! 2. 运行期（网格搜索，不重编译）：`EngineSession::set_profiles` / `profiles_mut`，或
//!    `anime-eval --profile word.space_edits.penalty=0.9`（键名见 [`ModeProfile::field_names`]）。
//!
//! 默认两份 profile 与 [`BASELINE_PROFILE`] 完全相同，且所有偏移 / 缩放默认值均为恒等变换（`+0.0`、`×1.0`），
//! 保证与引入 profile 之前逐行一致。
//!
//! # 不在 profile 中的参数
//! L1 内部权重（`EXACT_BONUS = 6.5`、`EDIT_LAMBDA`、`COMPLETION_*`，`crates/lexicon`）与 L2 词格内部权重
//! （`LEX_LAMBDA` / `LM_LAMBDA` / `MAX_CANDS_PER_SLOT = 10`，`crates/lm-ngram`）不归引擎所有；profile 用引擎侧
//! 等价旋钮覆盖：`lexicon.*_offset`（按匹配类型的加性偏移）、`l2.context_weight`（单 token 路径 PMI 加分缩放）、
//! `lattice.lex_scale`（词格词法分缩放，等价于改 LEX_LAMBDA:LM_LAMBDA 比值）。

/// L1 查询与按匹配类型的分数偏移。偏移在引擎取回 L1 结果后施加（缓存的是 L1 原始结果）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LexiconProfile {
    /// L1 查询返回条数；`None` 时取 `EngineConfig.max_candidates`。
    pub query_limit: Option<usize>,
    /// L1 模糊搜索最大编辑代价（`QueryOptions::default().max_edit_cost`）。
    pub max_edit_cost: f32,
    /// 精确词偏移（叠加在 L1 的 `EXACT_BONUS = 6.5` 之上，易混词锚点换算会同步扣除）。
    pub exact_offset: f32,
    /// 前缀补全偏移（L1 补全惩罚 `COMPLETION_BASE_PENALTY` 等的引擎侧对冲）。
    pub completion_offset: f32,
    /// 拼写纠错偏移。
    pub typo_offset: f32,
    /// 读音纠错偏移。
    pub phonetic_offset: f32,
    /// L1 模糊召回额外穷举全部单次编辑（`QueryOptions::exhaustive_single_edit`）：词首附近的漏字 /
    /// 错键不再因束宽剪枝而召回不到（`bateria -> bacteria`）。
    pub exhaustive_single_edit: bool,
}

impl LexiconProfile {
    pub(crate) fn has_offsets(&self) -> bool {
        self.exact_offset != 0.0
            || self.completion_offset != 0.0
            || self.typo_offset != 0.0
            || self.phonetic_offset != 0.0
    }
}

/// 单 token 路径的 L2 上下文加分。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContextLmProfile {
    /// `score_in_context`（与词频无关的 PMI 提升，L2 内已乘 `CONTEXT_LIFT_WEIGHT = 1.4`）的额外缩放。
    pub context_weight: f32,
}

/// 易混真词、简写展开与闭合 token 真词近邻（`confusables.rs`）的先验分差。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConfusableProfile {
    /// 每个 token 查询的易混词条数。
    pub query_limit: usize,
    /// L1 精确词基准分中的 `EXACT_BONUS`，用于从锚点还原原词对数词频；须与 `crates/lexicon` 一致。
    pub lexicon_exact_bonus: f32,
    /// 简写（u -> you、pls -> please）相对锚点的分差。
    pub shorthand_penalty: f32,
    /// 易混词比原词常见时的顺风惩罚：`clamp(Δlog_freq, 0, surplus_clamp) × surplus_weight`。
    pub surplus_weight: f32,
    pub surplus_clamp: f32,
    /// 易混词比原词罕见时的惩罚减免：`clamp(Δlog_freq, 0, deficit_clamp) × deficit_weight`，下限 `min_penalty`。
    pub deficit_weight: f32,
    pub deficit_clamp: f32,
    pub min_penalty: f32,
    /// 单 token + 上文时单次 QWERTY 直接邻键替换真词对（fast/vast）的最低惩罚（task-16）：
    /// 单 token 路径的 L2 是 PMI 提升，要求至少这么大的上下文优势才允许翻盘。
    pub adjacent_key_single_token_min_penalty: f32,
    /// 闭合真词一次编辑近邻（he -> the）的分差：`base + cost_weight × edit_cost + surplus_weight × surplus`。
    pub realword_neighbor_base_penalty: f32,
    pub realword_neighbor_cost_weight: f32,
    pub realword_neighbor_surplus_weight: f32,
    /// 原词过于罕见（Zipf < 3）时不展开真词近邻。
    pub realword_neighbor_min_raw_log_freq: f32,
    /// 非词 token 近邻不得超过槽内最佳分减去该余量。
    pub nonword_neighbor_cap_margin: f32,
    /// 每个闭合 token 最多展开的近邻数。
    pub max_closed_neighbors: usize,
    /// 向 L1 请求的闭合 token 近邻条数。
    pub closed_neighbor_query_limit: usize,
}

/// 整句空格出错的备选切分（多打 / 错位 / 漏打空格）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpaceEditProfile {
    /// 每处空格编辑的信道代价（解码分单位，约等于 LM_LAMBDA × 2 个 log10）。
    pub penalty: f32,
    /// 每次按键最多额外解码的备选切分数，守住同步路径延迟预算。
    pub max_alternatives: usize,
    /// 漏打空格两段切分每个 token 最多保留的切法数。
    pub max_two_way_splits: usize,
}

/// 无空格连打（`iwanttogo`、`thankyou`）的切分门槛与连打候选分。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RunonProfile {
    /// 两段切分的词频门槛（log10 概率）：常规部件 Zipf >= 4。
    pub part_min_log_freq: f32,
    /// <= 2 字母部件的门槛（Zipf >= 6：a、i、to、it、me 级别）。
    pub short_part_min_log_freq: f32,
    /// 收录但低于该词频（Zipf < 3）的粘连形（thankyou、alot）也允许尝试拆分。
    pub rare_exact_log_freq: f32,
    /// 存在编辑代价 <= 该值的拼写纠错时，纯小写 token 不强制拆分。
    pub block_typo_edit_cost: f32,
    /// 短于该长度且 L1 有任何召回的 token 不强制拆分。
    pub min_forced_len: usize,
    /// 未闭合 token 存在词频 >= 该值的前缀补全时不强制拆分。
    pub block_completion_log_freq: f32,
    /// 单 token 路径强制拆分的连打候选分：`max(best_single + forced_lead, forced_floor)`，无单词候选时取 `forced_floor`。
    pub forced_lead: f32,
    pub forced_floor: f32,
    /// 单 token 路径非强制拆分的连打候选分：`best_single - optional_trail`，无单词候选时取 `optional_fallback`。
    pub optional_trail: f32,
    pub optional_fallback: f32,
}

/// 词格槽位与解码。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatticeProfile {
    /// 引擎侧每槽候选上限（排序后截断）；`None` 时不截断，由 L2 的 `MAX_CANDS_PER_SLOT = 10` 把关。
    pub max_slot_candidates: Option<usize>,
    /// 读音纠错保底窗口：槽内候选 >= 该数且最佳读音纠错不在前该数位时，替换第该数位。0 关闭。
    pub phonetic_reserve_window: usize,
    /// 闭合错拼 token 的前缀补全最多比原 token 长几个字符。
    pub closed_completion_max_extra_chars: usize,
    /// 词格 n-best 条数；`None` 时取 `EngineConfig.max_candidates`。
    pub nbest: Option<usize>,
    /// 送入词格前对槽位词法分的缩放（等价于改 L2 的 LEX_LAMBDA:LM_LAMBDA 比值）。
    pub lex_scale: f32,
}

/// 整句原样候选（task-15）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VerbatimProfile {
    pub enabled: bool,
    /// 默认位置（0 基）：紧跟 top-1 之后，处于 L3 重排窗口内。
    pub slot: usize,
    /// 领先或垫底时与相邻候选的分差：只决定次序，远小于 L3 加性分数（±4.5 量级）。
    pub lead_margin: f32,
    /// 判定"已知驼峰词错拼"的近邻编辑代价上限。
    pub near_edit_cost: f32,
}

/// 多词预编辑中首词的部分选词候选。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PartialProfile {
    /// 首词 L1 候选取前几条。
    pub first_word_hits: usize,
    /// 相对 L1 分的惩罚。
    pub penalty: f32,
}

/// 是否请求异步 L3 重排（animed 按 `final_l3 == false` 调度，离线评测按 `wants_l3_rerank`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct L3Profile {
    pub enabled: bool,
    /// 预编辑至少有几个空格分隔的 token 才请求。
    pub min_tokens: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModeProfile {
    pub lexicon: LexiconProfile,
    pub l2: ContextLmProfile,
    pub confusables: ConfusableProfile,
    pub space_edits: SpaceEditProfile,
    pub runon: RunonProfile,
    pub lattice: LatticeProfile,
    pub verbatim: VerbatimProfile,
    pub partial: PartialProfile,
    pub l3: L3Profile,
}

/// 引入 profile 之前两种模式共用的参数（逐值搬迁，行为中立的基准）。
pub const BASELINE_PROFILE: ModeProfile = ModeProfile {
    lexicon: LexiconProfile {
        query_limit: None,
        max_edit_cost: 2.5,
        exact_offset: 0.0,
        completion_offset: 0.0,
        typo_offset: 0.0,
        phonetic_offset: 0.0,
        exhaustive_single_edit: false,
    },
    l2: ContextLmProfile { context_weight: 1.0 },
    confusables: ConfusableProfile {
        query_limit: 6,
        lexicon_exact_bonus: 6.5,
        shorthand_penalty: 0.8,
        surplus_weight: 0.35,
        surplus_clamp: 2.0,
        deficit_weight: 0.22,
        deficit_clamp: 1.8,
        min_penalty: 0.88,
        adjacent_key_single_token_min_penalty: 3.0,
        realword_neighbor_base_penalty: 2.8,
        realword_neighbor_cost_weight: 1.0,
        realword_neighbor_surplus_weight: 0.35,
        realword_neighbor_min_raw_log_freq: -6.0,
        nonword_neighbor_cap_margin: 0.2,
        max_closed_neighbors: 6,
        closed_neighbor_query_limit: 12,
    },
    space_edits: SpaceEditProfile {
        penalty: 0.75,
        max_alternatives: 8,
        max_two_way_splits: 2,
    },
    runon: RunonProfile {
        part_min_log_freq: -5.0,
        short_part_min_log_freq: -3.0,
        rare_exact_log_freq: -6.0,
        block_typo_edit_cost: 1.25,
        min_forced_len: 7,
        block_completion_log_freq: -5.5,
        forced_lead: 1.5,
        forced_floor: 0.5,
        optional_trail: 1.5,
        optional_fallback: -2.0,
    },
    lattice: LatticeProfile {
        max_slot_candidates: None,
        phonetic_reserve_window: 10,
        closed_completion_max_extra_chars: 2,
        nbest: None,
        lex_scale: 1.0,
    },
    verbatim: VerbatimProfile {
        enabled: true,
        slot: 1,
        lead_margin: 0.01,
        near_edit_cost: 1.25,
    },
    partial: PartialProfile {
        first_word_hits: 2,
        penalty: 1.0,
    },
    l3: L3Profile {
        enabled: true,
        min_tokens: 1,
    },
};

/// 单词模式（`space_commits = true`）。单词模式调参轨道只改这里。
///
/// 单词模式每次只排一个 token，且空格直接上屏首选，所以召回缺口与排序错误都会直接变成错误上屏
/// （task-18，`eval/benchmarks/word_ctx_v1.tsv`）。
pub const WORD_PROFILE: ModeProfile = ModeProfile {
    lexicon: LexiconProfile {
        exhaustive_single_edit: true,
        ..BASELINE_PROFILE.lexicon
    },
    ..BASELINE_PROFILE
};

/// 整句模式（`space_commits = false`）。整句召回轨道只改这里。
pub const SENTENCE_PROFILE: ModeProfile = BASELINE_PROFILE;

/// 两种模式各一份 profile。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModeProfiles {
    pub word: ModeProfile,
    pub sentence: ModeProfile,
}

impl Default for ModeProfiles {
    fn default() -> Self {
        Self {
            word: WORD_PROFILE,
            sentence: SENTENCE_PROFILE,
        }
    }
}

impl ModeProfiles {
    /// `space_commits` 为真（空格上屏首选）选单词模式，否则选整句模式。
    pub fn for_space_commits(&self, space_commits: bool) -> &ModeProfile {
        if space_commits { &self.word } else { &self.sentence }
    }

    /// 解析 `<word|sentence|both>.<group>.<field>=<value>` 覆盖（评测 / 网格搜索用）。
    pub fn apply_override(&mut self, spec: &str) -> Result<(), String> {
        let (key, value) = spec
            .split_once('=')
            .ok_or_else(|| format!("profile override '{spec}' must look like word.group.field=value"))?;
        let (mode, field) = key
            .trim()
            .split_once('.')
            .ok_or_else(|| format!("profile override key '{key}' must start with word., sentence. or both."))?;
        match mode {
            "word" => self.word.set_field(field, value.trim()),
            "sentence" => self.sentence.set_field(field, value.trim()),
            "both" => {
                self.word.set_field(field, value.trim())?;
                self.sentence.set_field(field, value.trim())
            }
            other => Err(format!("unknown profile mode '{other}' (expected word, sentence or both)")),
        }
    }
}

trait FieldValue: Sized {
    fn parse_field(raw: &str) -> Result<Self, String>;
    fn render_field(&self) -> String;
}

impl FieldValue for f32 {
    fn parse_field(raw: &str) -> Result<Self, String> {
        raw.parse().map_err(|_| format!("expected a float, got '{raw}'"))
    }
    fn render_field(&self) -> String {
        format!("{self:?}")
    }
}

impl FieldValue for usize {
    fn parse_field(raw: &str) -> Result<Self, String> {
        raw.parse().map_err(|_| format!("expected an integer, got '{raw}'"))
    }
    fn render_field(&self) -> String {
        self.to_string()
    }
}

impl FieldValue for bool {
    fn parse_field(raw: &str) -> Result<Self, String> {
        raw.parse().map_err(|_| format!("expected true or false, got '{raw}'"))
    }
    fn render_field(&self) -> String {
        self.to_string()
    }
}

impl FieldValue for Option<usize> {
    fn parse_field(raw: &str) -> Result<Self, String> {
        if raw.eq_ignore_ascii_case("none") {
            Ok(None)
        } else {
            usize::parse_field(raw).map(Some)
        }
    }
    fn render_field(&self) -> String {
        self.map_or_else(|| "none".to_string(), |v| v.to_string())
    }
}

/// 字段表只写一次，同时生成按名设置、按名列出与键名清单，避免新增字段时漏改其中之一。
macro_rules! profile_fields {
    ($($group:ident . $field:ident),* $(,)?) => {
        impl ModeProfile {
            /// 可覆盖的字段键名（`group.field`）。
            pub fn field_names() -> &'static [&'static str] {
                &[$(concat!(stringify!($group), ".", stringify!($field))),*]
            }

            /// 按 `group.field` 设置一个字段。
            pub fn set_field(&mut self, key: &str, value: &str) -> Result<(), String> {
                match key {
                    $(concat!(stringify!($group), ".", stringify!($field)) => {
                        self.$group.$field = FieldValue::parse_field(value)
                            .map_err(|e| format!("{key}: {e}"))?;
                        Ok(())
                    })*
                    _ => Err(format!("unknown profile field '{key}'")),
                }
            }

            /// 全部字段的 `(group.field, value)`，供报告记录实际生效参数。
            pub fn fields(&self) -> Vec<(&'static str, String)> {
                vec![$((concat!(stringify!($group), ".", stringify!($field)), self.$group.$field.render_field())),*]
            }
        }
    };
}

profile_fields!(
    lexicon.query_limit,
    lexicon.max_edit_cost,
    lexicon.exact_offset,
    lexicon.completion_offset,
    lexicon.typo_offset,
    lexicon.phonetic_offset,
    lexicon.exhaustive_single_edit,
    l2.context_weight,
    confusables.query_limit,
    confusables.lexicon_exact_bonus,
    confusables.shorthand_penalty,
    confusables.surplus_weight,
    confusables.surplus_clamp,
    confusables.deficit_weight,
    confusables.deficit_clamp,
    confusables.min_penalty,
    confusables.adjacent_key_single_token_min_penalty,
    confusables.realword_neighbor_base_penalty,
    confusables.realword_neighbor_cost_weight,
    confusables.realword_neighbor_surplus_weight,
    confusables.realword_neighbor_min_raw_log_freq,
    confusables.nonword_neighbor_cap_margin,
    confusables.max_closed_neighbors,
    confusables.closed_neighbor_query_limit,
    space_edits.penalty,
    space_edits.max_alternatives,
    space_edits.max_two_way_splits,
    runon.part_min_log_freq,
    runon.short_part_min_log_freq,
    runon.rare_exact_log_freq,
    runon.block_typo_edit_cost,
    runon.min_forced_len,
    runon.block_completion_log_freq,
    runon.forced_lead,
    runon.forced_floor,
    runon.optional_trail,
    runon.optional_fallback,
    lattice.max_slot_candidates,
    lattice.phonetic_reserve_window,
    lattice.closed_completion_max_extra_chars,
    lattice.nbest,
    lattice.lex_scale,
    verbatim.enabled,
    verbatim.slot,
    verbatim.lead_margin,
    verbatim.near_edit_cost,
    partial.first_word_hits,
    partial.penalty,
    l3.enabled,
    l3.min_tokens,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentence_mode_stays_baseline() {
        let p = ModeProfiles::default();
        assert_eq!(p.word, WORD_PROFILE);
        assert_eq!(p.sentence, BASELINE_PROFILE);
        assert_eq!(*p.for_space_commits(true), WORD_PROFILE);
        assert_eq!(*p.for_space_commits(false), SENTENCE_PROFILE);
        assert!(!BASELINE_PROFILE.lexicon.has_offsets());
    }

    #[test]
    fn overrides_touch_only_the_named_mode() {
        let mut p = ModeProfiles::default();
        p.apply_override("word.space_edits.penalty=0.9").expect("float");
        p.apply_override("sentence.lattice.nbest=12").expect("option");
        p.apply_override("both.l3.enabled=false").expect("bool");
        assert_eq!(p.word.space_edits.penalty, 0.9);
        assert_eq!(p.sentence.space_edits.penalty, 0.75);
        assert_eq!(p.sentence.lattice.nbest, Some(12));
        assert_eq!(p.word.lattice.nbest, None);
        assert!(!p.word.l3.enabled && !p.sentence.l3.enabled);
        assert!(p.apply_override("word.nope.x=1").is_err());
        assert!(p.apply_override("phrase.l3.enabled=true").is_err());
        assert!(p.apply_override("word.l3.min_tokens=abc").is_err());
        assert_eq!(ModeProfile::field_names().len(), BASELINE_PROFILE.fields().len());
        for key in ModeProfile::field_names() {
            let mut q = BASELINE_PROFILE;
            let (_, v) = BASELINE_PROFILE.fields().into_iter().find(|(k, _)| k == key).expect("listed");
            q.set_field(key, &v).expect("round-trips");
            assert_eq!(q, BASELINE_PROFILE, "{key}");
        }
    }
}
