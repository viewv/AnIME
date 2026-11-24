//! 紧凑前缀树（Flat Arena Trie）、A* 最优前缀补全与 QWERTY 加权模糊束搜索。

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::keyboard::{
    APOSTROPHE_SKIP_COST, TRANSPOSITION_COST, deletion_cost, insertion_cost, substitution_cost,
};

pub const NO_WORD: u32 = u32::MAX;
const DEFAULT_BEAM_WIDTH: usize = 64;
const MAX_COMPLETION_BEAM_NODES: usize = 16;
const MAX_FUZZY_COMPLETION_COST: f32 = 1.45;
const BEAM_EDIT_LAMBDA: f32 = 3.2;
const MIN_NON_APOSTROPHE_EDIT: f32 = 0.39;

#[derive(Debug, Clone, Copy)]
pub struct TrieNode {
    pub edge_start: u32,
    pub edge_len: u16,
    pub word_id: u32,
    pub max_subtree_log_freq: f32,
}

#[derive(Debug, Clone)]
pub struct CompactTrie {
    nodes: Vec<TrieNode>,
    edges: Vec<(u8, u32)>,
    word_log_freqs: Vec<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrieFuzzyHit {
    pub word_id: u32,
    pub edit_cost: f32,
    pub extra_chars: usize,
}

struct BuildNode {
    edges: Vec<(u8, u32)>,
    word_id: u32,
    max_subtree_log_freq: f32,
}

#[derive(Debug, Clone, Copy)]
struct OrdF32(f32);

impl PartialEq for OrdF32 {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for OrdF32 {}

impl PartialOrd for OrdF32 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrdF32 {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeapKind {
    Node { node_id: u32, extra_chars: usize },
    Terminal { word_id: u32, extra_chars: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HeapItem {
    priority: OrdF32,
    kind: HeapKind,
}

impl PartialOrd for HeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapItem {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority.cmp(&other.priority)
    }
}

#[derive(Debug, Clone, Copy)]
struct BeamEntry {
    cost: f32,
    del_count: u8,
    last_ch: u8,
}

impl CompactTrie {
    /// 从 `(lowercase_key, word_id, log_freq)` 列表构建紧凑平坦前缀树，并沿树底向上传播子树最高词频上界。
    pub fn build<'a, I>(entries: I, word_log_freqs: Vec<f32>) -> Self
    where
        I: IntoIterator<Item = (&'a str, u32, f32)>,
    {
        let mut build_nodes = vec![BuildNode {
            edges: Vec::new(),
            word_id: NO_WORD,
            max_subtree_log_freq: f32::NEG_INFINITY,
        }];

        for (key, word_id, log_freq) in entries {
            if key.is_empty() {
                continue;
            }
            let mut cur = 0u32;
            if log_freq > build_nodes[0].max_subtree_log_freq {
                build_nodes[0].max_subtree_log_freq = log_freq;
            }
            for b in key.bytes() {
                let next_id = if let Some(&(_, cid)) =
                    build_nodes[cur as usize].edges.iter().find(|&&(eb, _)| eb == b)
                {
                    cid
                } else {
                    let nid = build_nodes.len() as u32;
                    build_nodes.push(BuildNode {
                        edges: Vec::new(),
                        word_id: NO_WORD,
                        max_subtree_log_freq: log_freq,
                    });
                    build_nodes[cur as usize].edges.push((b, nid));
                    nid
                };
                if log_freq > build_nodes[next_id as usize].max_subtree_log_freq {
                    build_nodes[next_id as usize].max_subtree_log_freq = log_freq;
                }
                cur = next_id;
            }
            let term = &mut build_nodes[cur as usize];
            if term.word_id == NO_WORD
                || log_freq
                    > word_log_freqs
                        .get(term.word_id as usize)
                        .copied()
                        .unwrap_or(f32::NEG_INFINITY)
            {
                term.word_id = word_id;
            }
        }

        let mut nodes = Vec::with_capacity(build_nodes.len());
        let mut edges = Vec::new();
        for mut bn in build_nodes {
            bn.edges.sort_unstable_by_key(|&(b, _)| b);
            let edge_start = edges.len() as u32;
            let edge_len = u16::try_from(bn.edges.len()).unwrap_or(u16::MAX);
            edges.extend_from_slice(&bn.edges[..usize::from(edge_len)]);
            nodes.push(TrieNode {
                edge_start,
                edge_len,
                word_id: bn.word_id,
                max_subtree_log_freq: bn.max_subtree_log_freq,
            });
        }

        Self {
            nodes,
            edges,
            word_log_freqs,
        }
    }

    #[inline]
    fn node_edges(&self, node_id: u32) -> &[(u8, u32)] {
        let node = &self.nodes[node_id as usize];
        let start = node.edge_start as usize;
        let end = start + usize::from(node.edge_len);
        &self.edges[start..end]
    }

    #[inline]
    pub fn child(&self, node_id: u32, byte: u8) -> Option<u32> {
        let slice = self.node_edges(node_id);
        if slice.len() <= 8 {
            slice.iter().find(|&&(b, _)| b == byte).map(|&(_, id)| id)
        } else {
            slice
                .binary_search_by_key(&byte, |&(b, _)| b)
                .ok()
                .map(|idx| slice[idx].1)
        }
    }

    /// O(|prefix|) 精确查找路径所在节点。
    pub fn find_node(&self, prefix_lower: &str) -> Option<u32> {
        let mut cur = 0u32;
        for b in prefix_lower.bytes() {
            cur = self.child(cur, b)?;
        }
        Some(cur)
    }

    #[inline]
    pub fn node_word_id(&self, node_id: u32) -> Option<u32> {
        let wid = self.nodes[node_id as usize].word_id;
        (wid != NO_WORD).then_some(wid)
    }

    /// A* 最优优先子树补全：按 `max_subtree_log_freq` 精确弹出子树中词频最高的 `limit` 个严格延伸词。
    pub fn complete_prefix(&self, start_node: u32, limit: usize) -> Vec<(u32, usize)> {
        if limit == 0 {
            return Vec::new();
        }
        let mut heap = BinaryHeap::new();
        for &(_, child_id) in self.node_edges(start_node) {
            let freq = self.nodes[child_id as usize].max_subtree_log_freq;
            heap.push(HeapItem {
                priority: OrdF32(freq),
                kind: HeapKind::Node {
                    node_id: child_id,
                    extra_chars: 1,
                },
            });
        }

        let mut out = Vec::with_capacity(limit);
        let max_pops = limit.saturating_mul(64).max(128);
        let mut pops = 0usize;

        while let Some(item) = heap.pop() {
            pops += 1;
            match item.kind {
                HeapKind::Terminal {
                    word_id,
                    extra_chars,
                } => {
                    out.push((word_id, extra_chars));
                    if out.len() >= limit {
                        break;
                    }
                }
                HeapKind::Node {
                    node_id,
                    extra_chars,
                } => {
                    let node = &self.nodes[node_id as usize];
                    if node.word_id != NO_WORD {
                        let wf = self
                            .word_log_freqs
                            .get(node.word_id as usize)
                            .copied()
                            .unwrap_or(node.max_subtree_log_freq);
                        heap.push(HeapItem {
                            priority: OrdF32(wf),
                            kind: HeapKind::Terminal {
                                word_id: node.word_id,
                                extra_chars,
                            },
                        });
                    }
                    for &(_, child_id) in self.node_edges(node_id) {
                        let cf = self.nodes[child_id as usize].max_subtree_log_freq;
                        heap.push(HeapItem {
                            priority: OrdF32(cf),
                            kind: HeapKind::Node {
                                node_id: child_id,
                                extra_chars: extra_chars + 1,
                            },
                        });
                    }
                }
            }
            if pops >= max_pops && out.len() >= limit {
                break;
            }
        }
        out
    }

    pub fn fuzzy_search(
        &self,
        query_lower: &str,
        max_edit_cost: f32,
        completion_per_node: usize,
    ) -> (Vec<TrieFuzzyHit>, Vec<TrieFuzzyHit>) {
        self.fuzzy_search_with_width(
            query_lower,
            max_edit_cost,
            completion_per_node,
            DEFAULT_BEAM_WIDTH,
        )
    }

    /// QWERTY 物理代价加权模糊 Trie 束搜索：
    /// 使用连续 Vec 按节点 ID 归并去重（避免每步分配 HashMap），并用 A* 子树词频上界剪枝。
    pub fn fuzzy_search_with_width(
        &self,
        query_lower: &str,
        max_edit_cost: f32,
        completion_per_node: usize,
        beam_width: usize,
    ) -> (Vec<TrieFuzzyHit>, Vec<TrieFuzzyHit>) {
        let q = query_lower.as_bytes();
        let n = q.len();
        if n == 0 || beam_width == 0 {
            return (Vec::new(), Vec::new());
        }

        // 短词严格收紧编辑容忍度，避免如 "it"/"to"/"an" 被过度泛化
        let eff_max_cost = match n {
            1 => 0.0,
            2 => max_edit_cost.min(0.65),
            3 => max_edit_cost.min(1.25),
            _ => max_edit_cost,
        };

        let mut layers: Vec<Vec<(u32, BeamEntry)>> = (0..=n)
            .map(|_| Vec::with_capacity(beam_width * 2))
            .collect();
        layers[0].push((
            0,
            BeamEntry {
                cost: 0.0,
                del_count: 0,
                last_ch: 0,
            },
        ));

        let mut active: Vec<(u32, BeamEntry)> = Vec::with_capacity(beam_width * 2);

        for i in 0..=n {
            // 1. 先按 node_id 归并当前层重复路径，仅保留最小代价状态
            let mut layer = std::mem::take(&mut layers[i]);
            dedup_beam_layer(&mut layer);

            // 2. 在尚未到达串尾时，做单次轻量撇号跳过与漏打扩展（串尾延伸交由 complete_prefix 完成，避免冗余）
            if eff_max_cost > 0.0 && i < n {
                if layer.len() > beam_width * 2 {
                    prune_beam_by_astar(&self.nodes, &mut layer, beam_width * 2);
                }
                let mut additions = Vec::new();
                for &(node_id, st) in &layer {
                    let rem = eff_max_cost - st.cost;
                    if rem < APOSTROPHE_SKIP_COST {
                        continue;
                    }
                    let prev_ch = (st.last_ch != 0).then_some(st.last_ch);
                    let allow_del = st.del_count < 2
                        && rem >= MIN_NON_APOSTROPHE_EDIT
                        && (i > 0 || (n >= 5 && st.del_count == 0));
                    for &(ch, child_id) in self.node_edges(node_id) {
                        if ch == b'\'' {
                            let nc = st.cost + APOSTROPHE_SKIP_COST;
                            if nc <= eff_max_cost {
                                additions.push((
                                    child_id,
                                    BeamEntry {
                                        cost: nc,
                                        del_count: st.del_count,
                                        last_ch: b'\'',
                                    },
                                ));
                            }
                            continue;
                        }
                        if allow_del {
                            let dc = deletion_cost(ch, prev_ch);
                            let nc = st.cost + dc;
                            if nc <= eff_max_cost {
                                additions.push((
                                    child_id,
                                    BeamEntry {
                                        cost: nc,
                                        del_count: st.del_count + 1,
                                        last_ch: ch,
                                    },
                                ));
                            }
                        }
                    }
                }
                if !additions.is_empty() {
                    layer.extend(additions);
                    dedup_beam_layer(&mut layer);
                }
            }

            // 3. 按 A* 可容许上界截断束宽
            if layer.len() > beam_width {
                prune_beam_by_astar(&self.nodes, &mut layer, beam_width);
            } else {
                sort_beam_by_astar(&self.nodes, &mut layer);
            }

            active = layer;
            if i == n {
                break;
            }

            let cur_q = q[i];
            let prev_q = if i > 0 { Some(q[i - 1]) } else { None };
            let next_q = q.get(i + 1).copied();

            let (ahead1, ahead2) = layers.split_at_mut(i + 2);
            let next_layer = &mut ahead1[i + 1];

            for &(node_id, st) in &active {
                // 精确字符匹配 (0 代价)
                if let Some(child_id) = self.child(node_id, cur_q) {
                    next_layer.push((
                        child_id,
                        BeamEntry {
                            cost: st.cost,
                            del_count: st.del_count,
                            last_ch: cur_q,
                        },
                    ));
                }

                // 剩余编辑预算低于最小非撇号操作代价时，跳过所有换位/插入/替换边遍历
                if eff_max_cost - st.cost < MIN_NON_APOSTROPHE_EDIT {
                    continue;
                }

                // 相邻两字符换位（如 teh->the, recieve->receive）
                if let Some(nq) = next_q {
                    if nq != cur_q {
                        let nc = st.cost + TRANSPOSITION_COST;
                        if nc <= eff_max_cost {
                            if let Some(c1) = self.child(node_id, nq) {
                                if let Some(c2) = self.child(c1, cur_q) {
                                    ahead2[0].push((
                                        c2,
                                        BeamEntry {
                                            cost: nc,
                                            del_count: st.del_count,
                                            last_ch: cur_q,
                                        },
                                    ));
                                }
                            }
                        }
                    }
                }

                // 多敲键跳过（消耗输入 cur_q，Trie 不前进）
                let ins_cost = insertion_cost(cur_q, prev_q, next_q);
                if st.cost + ins_cost <= eff_max_cost {
                    next_layer.push((
                        node_id,
                        BeamEntry {
                            cost: st.cost + ins_cost,
                            del_count: st.del_count,
                            last_ch: st.last_ch,
                        },
                    ));
                }

                // QWERTY 加权替换
                for &(ch, child_id) in self.node_edges(node_id) {
                    if ch == cur_q || ch == b'\'' {
                        continue;
                    }
                    let sc = substitution_cost(cur_q, ch);
                    if st.cost + sc <= eff_max_cost {
                        next_layer.push((
                            child_id,
                            BeamEntry {
                                cost: st.cost + sc,
                                del_count: st.del_count,
                                last_ch: ch,
                            },
                        ));
                    }
                }
            }
        }

        let mut terminal_hits = Vec::new();
        let mut completion_hits = Vec::new();
        let mut comp_expanded = 0usize;

        for &(node_id, st) in &active {
            if let Some(word_id) = self.node_word_id(node_id) {
                terminal_hits.push(TrieFuzzyHit {
                    word_id,
                    edit_cost: st.cost,
                    extra_chars: 0,
                });
            }
            // 仅对精确前缀（cost == 0）或高优低代价纠错节点（cost <= 1.25 且前 MAX_COMPLETION_BEAM_NODES 个）做子树补全，
            // 避免无关高编辑代价枝节污染前缀结果并消除多余 BinaryHeap 开销。
            let allow_comp = completion_per_node > 0
                && (st.cost == 0.0
                    || (n >= 4
                        && st.cost <= MAX_FUZZY_COMPLETION_COST
                        && comp_expanded < MAX_COMPLETION_BEAM_NODES));
            if allow_comp {
                comp_expanded += 1;
                let comp_limit = if st.cost == 0.0 {
                    completion_per_node.max(8)
                } else {
                    completion_per_node
                };
                for (cid, extra_chars) in self.complete_prefix(node_id, comp_limit) {
                    completion_hits.push(TrieFuzzyHit {
                        word_id: cid,
                        edit_cost: st.cost,
                        extra_chars,
                    });
                }
            }
        }

        (terminal_hits, completion_hits)
    }

    /// 穷举单次编辑（漏打 / 多打 / QWERTY 替换 / 相邻换位）的确定性 Trie 遍历，返回 `(终点节点, 代价)`。
    ///
    /// Why：束搜索按"子树最高词频 - λ·代价"剪枝，打分里不含尚未消费的输入，所以词首附近的漏字
    /// （`bateria` 中 `ba_teria` 漏 c、`paer` 漏 p）在第 2–3 层就被一批零代价 / 低代价的高频前缀挤出束宽，
    /// 后面的字母即使全部吻合也救不回来；同样的漏字在词尾（`bacteia`）却能命中。单次编辑的候选空间只有
    /// O(26·n)，沿精确前缀路径逐位分叉、只走真实存在的子边，开销是常数级的微秒量级，因此直接穷举，
    /// 让"只差一次编辑"的词不再取决于束宽。终点节点既可以是完整词（纠错），也可以是前缀（错拼后补全）。
    pub fn single_edit_nodes(&self, query_lower: &str, allow_leading_omission: bool) -> Vec<(u32, f32)> {
        let q = query_lower.as_bytes();
        let n = q.len();
        let mut out: Vec<(u32, f32)> = Vec::new();
        if n == 0 {
            return out;
        }
        let walk = |mut node: u32, rest: &[u8]| -> Option<u32> {
            for &b in rest {
                node = self.child(node, b)?;
            }
            Some(node)
        };
        let mut node = 0u32;
        for p in 0..=n {
            let prev_target = (p > 0).then(|| q[p - 1]);
            // 漏打：目标词在第 p 位多一个字母 ch（漏打首字母只对长词放开，与束搜索一致）
            if p < n && (p > 0 || allow_leading_omission) {
                for &(ch, child) in self.node_edges(node) {
                    if ch == b'\'' {
                        continue;
                    }
                    if let Some(end) = walk(child, &q[p..]) {
                        out.push((end, deletion_cost(ch, prev_target)));
                    }
                }
            }
            if p == n {
                break;
            }
            // 多打：跳过输入第 p 个字符
            if n >= 2 {
                if let Some(end) = walk(node, &q[p + 1..]) {
                    out.push((end, insertion_cost(q[p], prev_target, q.get(p + 1).copied())));
                }
            }
            // QWERTY 加权替换
            for &(ch, child) in self.node_edges(node) {
                if ch == q[p] || ch == b'\'' {
                    continue;
                }
                if let Some(end) = walk(child, &q[p + 1..]) {
                    out.push((end, substitution_cost(q[p], ch)));
                }
            }
            // 相邻换位
            if p + 1 < n && q[p] != q[p + 1] {
                if let Some(end) = walk(node, &[q[p + 1], q[p]]).and_then(|m| walk(m, &q[p + 2..])) {
                    out.push((end, TRANSPOSITION_COST));
                }
            }
            match self.child(node, q[p]) {
                Some(next) => node = next,
                None => break,
            }
        }
        out.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.total_cmp(&b.1)));
        out.dedup_by(|b, a| a.0 == b.0);
        out
    }

    /// `single_edit_nodes` 的终点按"完整词命中 + 前缀补全"拆分为两组命中，与 `fuzzy_search` 返回形状相同。
    /// 补全只对 A* 上界最高的前 `MAX_COMPLETION_BEAM_NODES` 个前缀节点展开，且代价不超过模糊补全上限。
    pub fn single_edit_search(
        &self,
        query_lower: &str,
        max_edit_cost: f32,
        completion_per_node: usize,
        allow_leading_omission: bool,
    ) -> (Vec<TrieFuzzyHit>, Vec<TrieFuzzyHit>) {
        let mut ends = self.single_edit_nodes(query_lower, allow_leading_omission);
        ends.retain(|&(_, c)| c <= max_edit_cost);
        let mut terminal = Vec::new();
        for &(node_id, cost) in &ends {
            if let Some(word_id) = self.node_word_id(node_id) {
                terminal.push(TrieFuzzyHit {
                    word_id,
                    edit_cost: cost,
                    extra_chars: 0,
                });
            }
        }
        let mut completion = Vec::new();
        if completion_per_node > 0 && query_lower.len() >= 4 {
            ends.retain(|&(_, c)| c <= MAX_FUZZY_COMPLETION_COST);
            ends.sort_unstable_by(|&(ia, ca), &(ib, cb)| {
                let sa = self.nodes[ia as usize].max_subtree_log_freq - BEAM_EDIT_LAMBDA * ca;
                let sb = self.nodes[ib as usize].max_subtree_log_freq - BEAM_EDIT_LAMBDA * cb;
                sb.total_cmp(&sa)
            });
            for &(node_id, cost) in ends.iter().take(MAX_COMPLETION_BEAM_NODES) {
                for (word_id, extra_chars) in self.complete_prefix(node_id, completion_per_node) {
                    completion.push(TrieFuzzyHit {
                        word_id,
                        edit_cost: cost,
                        extra_chars,
                    });
                }
            }
        }
        (terminal, completion)
    }
}

fn dedup_beam_layer(layer: &mut Vec<(u32, BeamEntry)>) {
    if layer.len() <= 1 {
        return;
    }
    layer.sort_unstable_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cost.total_cmp(&b.1.cost))
            .then_with(|| a.1.del_count.cmp(&b.1.del_count))
    });
    layer.dedup_by(|b, a| a.0 == b.0);
}

fn sort_beam_by_astar(nodes: &[TrieNode], layer: &mut [(u32, BeamEntry)]) {
    layer.sort_unstable_by(|&(ida, sta), &(idb, stb)| {
        let sa = nodes[ida as usize].max_subtree_log_freq - BEAM_EDIT_LAMBDA * sta.cost;
        let sb = nodes[idb as usize].max_subtree_log_freq - BEAM_EDIT_LAMBDA * stb.cost;
        sb.total_cmp(&sa)
    });
}

fn prune_beam_by_astar(nodes: &[TrieNode], layer: &mut Vec<(u32, BeamEntry)>, beam_width: usize) {
    if layer.len() <= beam_width {
        return;
    }
    layer.select_nth_unstable_by(beam_width, |&(ida, sta), &(idb, stb)| {
        let sa = nodes[ida as usize].max_subtree_log_freq - BEAM_EDIT_LAMBDA * sta.cost;
        let sb = nodes[idb as usize].max_subtree_log_freq - BEAM_EDIT_LAMBDA * stb.cost;
        sb.total_cmp(&sa)
    });
    layer.truncate(beam_width);
    sort_beam_by_astar(nodes, layer);
}
