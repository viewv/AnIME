---
task_id: "task-15-verbatim-candidate"
worktree: "../wt-anime-verbatim"
allowed_paths:
  - "crates/engine/src/**"
  - "crates/engine/tests/**"
  - ".agents/tasks/task-15-verbatim-candidate.md"
  - "ROADMAP.md"
forbidden_paths:
  - ".agents/contracts/**"
  - "crates/protocol/**"
  - "crates/lexicon/**"
  - "crates/lm-ngram/**"
  - "crates/lm-neural/**"
  - "crates/animed/**"
  - "crates/eval/**"
  - "eval/**"
  - "scripts/**"
  - "data/MANIFEST.yaml"
  - "frontends/**"
read_only_context:
  - ".agents/contracts/core-api.md"
  - ".agents/contracts/eval-schema.md"
acceptance_cmd: "make check && make test && make eval && make verify-infra && ./scripts/verify-boundaries.sh .agents/tasks/task-15-verbatim-candidate.md"
---

## Objective (目标)
整句模式下，`crates/engine` 始终把用户原样输入（raw preedit，保留用户自己的大小写）作为一条**非 Raw** 的整句候选放进 L3 重排窗口（`max_rerank_candidates`，默认 6）并展示给用户，让 L3（尤其 task-07 纠错模型）能在"照抄"与"纠错"之间选择；不强制置顶。并用评测证据决定 L1+L2-only 模式下是否在特定条件（如含词典外 token）下额外提升它。

## Background
- task-07 中期检查点表明：L3 无法提升 `negative:novel`（含 ChatGPT / UBI / roleplay / supersymmetry / HVLP 等新词的干净句），因为原样输入不在引擎候选中 —— L1 先把 `ChatGPT` 改成 `Chapter`，L3 只能在错误候选之间挑选。
- 现有 Raw 兜底候选（`score -999`、位于末位）会被 animed 调度器与 anime-eval 在构造 L3 候选列表时**按 `CandidateKind::Raw` 过滤掉**，且位于 Top-5 之外；这两处分别属于 `crates/animed` 与 `crates/eval`，本任务不改动它们，因此原样候选必须以 `CandidateKind::Sentence`（comment `verbatim`）出现。
- 用户以开发者身份使用 AnIME，优先现代 / 技术 / 学术文本；文学评测集（ood_v1）仅作参考。

## 核心子任务
1. 在 `recompute_candidates` 的整句分支，构造原样候选（句首按现有规则首字母大写，其余保留用户大小写），去重后插入到整句候选的固定窗口位置（不在 top-1，且位于前 `max_rerank_candidates` 条整句候选之内）；分数取相邻候选之间，使 L3 重排的加性分数能把它提到前面。
2. `apply_neural_rerank` 之后原样候选仍然保留（不被当作 Raw 丢弃）。
3. 以评测证据决定 L1+L2-only 下的提升规则（候选条件：含词典外 token 且大小写显式表达意图，如 `ChatGPT` / `UBI`）。
4. 单测：原样候选存在、位于窗口内、不在 top-1、经过 L3 重排后保留；L3 偏好原样时能被提到 top-1。
5. 评测：`anime-eval --group-by-tag`（去泄漏 L2 `~/.local/share/anime/ngram.animengr.bin`、词典 `data/compiled/lexicon.animelex.bin`）在 `ood_dev_v1` / `ood_academic_v1` / `ood_tm2_v1` / `noise_v2` / `core_v2` 上，分别跑 L1+L2、L1+L2+通用 SmolLM2-360M，以及（若存在）纠错 L3 模型；前后对比表 + Wilson 95% CI；单键延迟契约（p50 <= 3 ms、p99 <= 10 ms、max <= 16 ms）不得破坏。

## Constraints (局部硬约束)
- 仅改动 `allowed_paths` 内文件；必须跨界或改动公共契约时，必须立即暂停，并在 `Blocked` 栏写出明确原因与需改动的具体文件路径以申请权限。
- 核心 crate（`protocol` / `lexicon` / `lm-ngram` / `lm-neural` / `engine`）严禁引入 Linux/Fcitx5/macOS 平台专有依赖。
- 不得重新安装或重启 live 守护进程（`~/.local/bin/animed`，systemd --user）除非用户明确同意。
- 噪声标签回退不得超出 95% CI；冻结评测集不得改动。

## Blocked (必须跨界或改契约时立即暂停：写出明确原因 + 申请修改的文件路径；无阻塞留 None)
- None

## Handoff / Evidence (由执行 Agent 填写后退出)
- Status: [PENDING]
- Changed Files:
- Verification Output:
