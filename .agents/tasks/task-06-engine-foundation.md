---
task_id: "task-06-engine-foundation"
worktree: "../wt-anime-engine-foundation"
allowed_paths:
  - "crates/lm-neural/**"
  - "crates/engine/**"
  - "crates/lexicon/**"
  - "crates/animed/**"
  - "crates/eval/**"
  - "data/MANIFEST.yaml"
  - "scripts/data_pipeline/**"
  - ".agents/tasks/task-06-engine-foundation.md"
forbidden_paths:
  - ".agents/contracts/**"
  - "frontends/**"
  - "crates/protocol/**"
  - "crates/lm-ngram/**"
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
5. **Plan A 数据驱动易混真词发现（移除手写列表）**：引入开源 `BSD-2-Clause` **CMUdict** (`cmusphinx/cmudict`) 写入 `ANIMELEX` `phonetic` 字段，在 `crates/lexicon` 实现 `RealWordConfusableIndex`（CMUdict 完全同音、音节/清浊近音、自动撇号缩写、运行时 QWERTY 低代价邻键搜索 + 无 CMUdict 兜底），在 `crates/engine` 删除 40 组手写 `CONFUSABLE_GROUPS`，改用 `LexiconProvider::contextual_confusables` 与通用形态/频率排序。

## Constraints (局部硬约束)
- 仅改动 `allowed_paths` 内文件，保持工作区 `unsafe_code = "forbid"`，零平台专有依赖，默认 `make test` 全绿且 0 warnings。
- 实测比较修复前后 Baseline / `135M` / `360M` / `1.7B` 在 `eval/benchmarks/core_v1.tsv` 上的 7 类准确率与 Vulkan 延迟，并补充 OOD（分布外）易混真词泛化测试。

## Blocked (必须跨界或改契约时立即暂停：写出明确原因 + 申请修改的文件路径；无阻塞留 None)
- None

## Handoff / Evidence (由执行 Agent 填写后退出)
- Status: [DONE]
- Changed Files:
  - `crates/lm-neural/src/llama_backend.rs`
  - `crates/lm-neural/src/prompt.rs`
  - `crates/engine/src/confusables.rs`
  - `crates/engine/src/lib.rs`
  - `crates/lexicon/src/confusable_index.rs`
  - `crates/lexicon/src/layered.rs`
  - `crates/lexicon/src/lexicon.rs`
  - `crates/lexicon/src/lib.rs`
  - `crates/lexicon/src/trie.rs`
  - `crates/animed/Cargo.toml`
  - `crates/animed/src/main.rs`
  - `crates/animed/src/server.rs`
  - `crates/eval/src/main.rs`
  - `data/MANIFEST.yaml`
  - `scripts/data_pipeline/build_lexicon.py`
  - `.agents/tasks/task-06-engine-foundation.md`
- Verification Output:
  - Data pipeline & provenance (`python3 scripts/data_pipeline/fetch_sources.py --verify-only` & `build_lexicon.py --cmudict data/raw/cmudict/cmudict.dict`): `MANIFEST OK: 10 个数据源全部通过开源协议与格式校验`; rebuilt `data/compiled/lexicon.animelex.bin` (`120000 words, 57117 misspellings, 56318 entries populated with CMUdict ARPAbet phonetics`).
  - Offline workspace (`make check && make test && make eval && make verify-infra`): all exit 0 with 0 warnings (`engine`: 11 unit + 1 integration, `lexicon`: 11 unit + 2 integration, `lm-neural`: 5, `lm-ngram`: 5, `protocol`: 4, `animed`: 4, `eval`: 7; `INFRA OK`).
  - Release L1 latency gate (`cargo test --release -p anime-lexicon --test real_lexicon_eval -- --nocapture`): 120K CMUdict `ANIMELEX` loaded in `294 ms`; `core_v1` `p50=382 us, p99=538 us` (Layered `p50=399 us, p99=574 us`; `eval_large_open` `p50=364 us, p99=494 us` < `1000 us` gate).
  - Vulkan release rebuild (`make engine-vulkan && cargo build --release -p anime-eval --features vulkan`): exit 0 with 0 warnings.
  - Data-Driven Plan A (0 hand-crafted confusable groups) on `eval/benchmarks/core_v1.tsv` (1400 rows, Radeon 780M `Vulkan0(Vulkan)`):
    - Baseline L1+L2: `context_realword top1=85.5%, top3=99.5%, top5=100.0%`; `sentence top1=72.0%, top3=89.5%, top5=91.0%, p50=2517us`; `typo_prefix top5=81.5%`; `TOTAL top1=75.1%, top3=91.2%, top5=94.6%, p50=792us, p99=6529us`.
    - `SmolLM2-135M` Vulkan: `context_realword top1=91.0%, top3=99.5%, top5=99.5%`; `sentence top1=74.5%, top3=90.0%, top5=91.0%`; `TOTAL top1=76.2%, top3=91.3%, top5=94.6%`; `L3 warmup=161ms, p50=28336us, p99=51725us, mean=29036us`.
    - `SmolLM2-360M` Vulkan: `context_realword top1=89.5%, top3=99.5%, top5=99.5%`; `sentence top1=77.0%, top3=90.5%, top5=92.5%, strict1=70.0%`; `TOTAL top1=76.4%, top3=91.4%, top5=94.8%, strict1=75.0%`; `L3 warmup=357ms, p50=42825us, p99=63167us, mean=42041us`.
    - `SmolLM2-1.7B` Vulkan: `context_realword top1=89.0%, top3=99.5%, top5=99.5%`; `sentence top1=80.0%, strict1=70.5%, top3=90.5%, top5=92.0%`; `TOTAL top1=76.7%, top3=91.4%, top5=94.7%`; `L3 warmup=2224ms, p50=109723us, p99=152638us, mean=105440us`.
  - OOD Generalization Benchmark (25 OOD real-word confusable cases absent from deleted 40-group list + 5 negative control sentences):
    - Baseline L1+L2: `context_realword top1=48.0%, top3=100.0%, top5=100.0%`; `negative_control top1=60.0%, top3=100.0%`; `TOTAL top1=50.0%, top3=100.0%`.
    - `SmolLM2-360M` Vulkan: `context_realword top1=84.0% (+36.0pp), top3=100.0%, top5=100.0%` (all 4 remaining misses at `ci_rank=1`, i.e. 100.0% Top-2 recall); `negative_control top1=80.0% (4/5), top3=100.0%`; `TOTAL top1=83.3%, top3=100.0%, top5=100.0%`.
  - Boundary guard (`./scripts/verify-boundaries.sh .agents/tasks/task-06-engine-foundation.md`): `BOUNDARY OK`.
