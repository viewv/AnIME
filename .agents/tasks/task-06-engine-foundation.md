---
task_id: "task-06-engine-foundation"
worktree: "../wt-anime-engine-foundation"
allowed_paths:
  - "crates/lm-neural/**"
  - "crates/engine/**"
  - "crates/lexicon/**"
  - ".agents/tasks/task-06-engine-foundation.md"
forbidden_paths:
  - ".agents/contracts/**"
  - "frontends/**"
  - "crates/protocol/**"
  - "crates/lm-ngram/**"
  - "crates/animed/**"
  - "crates/eval/**"
  - "data/**"
  - "scripts/**"
read_only_context:
  - ".agents/contracts/core-api.md"
  - ".agents/contracts/ipc-protocol.md"
  - ".agents/contracts/data-format.md"
  - ".agents/contracts/eval-schema.md"
acceptance_cmd: "cargo test --workspace && ./scripts/verify-boundaries.sh .agents/tasks/task-06-engine-foundation.md"
---

## Objective (目标)
针对 1.7B 教师模型对比评测暴露出的四项底座短板完成工程修补（不改动契约或前端，不依赖外部模型微调）：
1. **消除 L3 子词长度归一化偏差**：`crates/lm-neural` 多 token 候选改用真实联合对数概率 `sum` 替代 `sum / l^0.8`，消除 `'s` / `'ll` / `'t` / `al` 等高条件概率词缀因分母拉长引起的畸高误纠。
2. **句内共享词前缀 KV 折叠**：`split_prefix_and_candidates` 自动将候选句共同词边界前缀折叠进 `shared_prefix`，在保持打分数学等价（常数前缀分差在均值中心化后精确抵消）的同时大幅降低多词候选 GPU decode 延迟。
3. **补全易混真词与多词槽过滤排序**：在 `crates/engine` 中合并/补全常见真词混淆组（`where/were/we're/wear`、`an/and`、`of/off`、`lets/let's`、`whose/who's`），对非句尾精确命中词跳过无效前缀补全，并在 `expand_slot_confusables` 后重排序保证高优易混词不落出 Viterbi 前 10 槽。
4. **提升 L1 `typo_prefix` 模糊前缀子树召回**：温和放宽 `CompactTrie` 模糊前缀补全节点上限与代价窗口，守住 `< 1.5 ms` p50 门禁。

## Constraints (局部硬约束)
- 仅改动 `allowed_paths` 内文件，保持工作区 `unsafe_code = "forbid"`，零平台专有依赖，默认 `make test` 全绿且 0 warnings。
- 实测比较修复前后 Baseline / `135M` / `360M` / `1.7B` 在 `eval/benchmarks/core_v1.tsv` 上的 7 类准确率与 Vulkan 延迟。

## Blocked (必须跨界或改契约时立即暂停：写出明确原因 + 申请修改的文件路径；无阻塞留 None)
- None

## Handoff / Evidence (由执行 Agent 填写后退出)
- Status: [DONE]
- Changed Files:
  - `crates/lm-neural/src/llama_backend.rs`
  - `crates/lm-neural/src/prompt.rs`
  - `crates/engine/src/confusables.rs`
  - `crates/engine/src/lib.rs`
  - `crates/lexicon/src/trie.rs`
  - `crates/lexicon/src/lexicon.rs`
  - `.agents/tasks/task-06-engine-foundation.md`
- Verification Output:
  - Offline workspace (`make check && make test && make eval && make verify-infra`): all exit 0 with 0 warnings (`engine`: 11 unit + 1 integration, `lexicon`: 8 + 2, `lm-neural`: 5, `lm-ngram`: 5, `protocol`: 4, `animed`: 4, `eval`: 7; `INFRA OK`).
  - Feature `--features llama` integration test (`cargo test --release -p anime-lm-neural --features llama -- --ignored --nocapture`): `llama_smollm2_135m_rerank_and_continuation_smoke ... ok` in `0.16s`.
  - Vulkan release rebuild (`make engine-vulkan && cargo build --release -p anime-eval --features vulkan`): exit 0 with 0 warnings.
  - Before vs After on `eval/benchmarks/core_v1.tsv` (1400 rows, compiled `lexicon.animelex.bin` + `ngram.animengr.bin`, Radeon 780M `Vulkan0(Vulkan)`):
    - Baseline L1+L2: `context_realword top1=65.0%->87.0% (+22.0pp), top3=91.5%->100.0%, top5=91.5%->100.0%`; `typo_prefix top5=79.5%->81.5%`; `sentence sync p50=8824us->2835us`; `TOTAL top1=72.5%->75.6% (+3.1pp), top3=89.9%->91.3%, top5=93.3%->94.6%, p99=28625us->6709us`.
    - `SmolLM2-135M` Vulkan: `context_realword top1=84.5%->92.5% (+8.0pp), top3=100.0%`; `sentence top1=77.5%->77.5%, top3=89.0%->90.0%`; `TOTAL top1=75.7%->76.9%, top5=93.4%->94.8%`; `L3 p50=33472us, p99=178091us->60316us, mean=39393us->33177us`.
    - `SmolLM2-360M` Vulkan: `context_realword top1=86.0%->91.5% (+5.5pp), top3=100.0%`; `sentence top1=81.5%->80.5%, top3=91.0%, strict1=70.0%`; `TOTAL top1=76.5%->77.1%, top3=90.3%->91.5%, top5=93.4%->94.8%`; `L3 warmup=341ms->223ms, p50=49493us, p99=90399us->72115us, mean=47906us->47713us`.
    - `SmolLM2-1.7B` Vulkan: `context_realword top1=83.0%->91.0% (+8.0pp), top3=100.0%`; `sentence top1=71.0%->78.5% (+7.5pp), strict1=61.0%->68.0%, top3=89.5%->91.0%`; `TOTAL top1=74.6%->76.8% (+2.2pp)`; `L3 warmup=1658ms->652ms, p50=124279us, mean=118394us`.
  - Boundary guard (`./scripts/verify-boundaries.sh .agents/tasks/task-06-engine-foundation.md`): `BOUNDARY OK`.
