---
task_id: "task-14-academic-eval"
worktree: "../wt-anime-academic"
allowed_paths:
  - "eval/**"
  - "scripts/data_pipeline/**"
  - "data/MANIFEST.yaml"
  - ".agents/tasks/task-14-academic-eval.md"
  - "ROADMAP.md"
forbidden_paths:
  - ".agents/contracts/**"
  - "crates/**"
  - "frontends/**"
read_only_context:
  - ".agents/contracts/eval-schema.md"
  - ".agents/contracts/data-format.md"
acceptance_cmd: "make check && make test && make eval && make verify-infra && ./scripts/verify-boundaries.sh .agents/tasks/task-14-academic-eval.md"
---

## Objective (目标)
为学术论文写作（以 CS 为主）建立数据源与评测：从 arXiv 官方 OAI-PMH 抓取 CS 论文标题 + 摘要（元数据 CC0），**按论文留出**构建评测集 `ood_academic_v1`，其余论文作为 L3 纠错模型（task-07）与后续 L2 混合的训练文本。

## Background
- 用户以开发者身份使用 AnIME，并会撰写 CS 学术论文；学术文本新词密度高（transformer、ablation、BERT），且有大量固定句式（we propose / to the best of our knowledge）。
- 用户 2026-11-23 批准仅下载 arXiv 元数据（不含全文 / PDF），聚焦 CS 领域。
- arXiv Terms of Use：描述性元数据（标题、摘要、作者、分类）按 CC0 1.0 使用；OAI-PMH 限速每 3 秒 1 次请求、单连接；不得程序化抓取全量语料 → 只抓取 CS set 的有限子集。

## 核心子任务
1. **抓取**（`scripts/data_pipeline/fetch_arxiv_oai.py`）：`https://oaipmh.arxiv.org/oai`，`metadataPrefix=arXiv`、`set=cs`、`from` 固定日期，请求间隔 >= 3 s，遵守 503 Retry-After，记录上限（默认 100k 篇），原始 XML 页 gzip 存入 `data/raw/arxiv-cs/`，可断点续传。
2. **文本**（`scripts/data_pipeline/build_arxiv_text.py`）：解析标题 + 摘要，清洗 LaTeX（`$...$` 公式、`\cmd{}`、`~`、引用）与换行；按 arXiv ID 的 CRC32 确定性留出约 10% 论文为评测专用，训练文本输出到 `data/compiled/arxiv_cs/{train,heldout}.jsonl`。
3. **评测集 `eval/benchmarks/ood_academic_v1.tsv`**（`build_eval_v3.py`，与 ood_dev_v1 同一方法）：留出论文的摘要句取 4–14 词子句；单错误 8 类各 >= 60、`noise:multi` >= 100、`negative:clean` >= 300、`negative:novel` >= 150；噪声只作用于词典词；`source:arxiv` 标签。
4. **复核**：沿用 task-10 / task-13 分歧裁决，追加到 `eval/review/negative_review.tsv`，迭代至零未复核分歧。
5. **MANIFEST** 登记 `arxiv-cs-oai`（CC0 元数据、抓取参数、页数、SHA256 清单）。
6. **基线**：L1+L2 与 L1+L2+L3（通用 SmolLM2-360M）在 `ood_academic_v1` 上的指标写入 Evidence。

## 与其他任务的关系
- task-07 的 `build_l3_sft.py` 读取 `data/compiled/arxiv_cs/train.jsonl`（仅非留出论文）作为新来源，并对 `ood_academic_v1` 去泄漏；L2 混合（task-11 配置 + arXiv）与 L1 可挂载学术词库另开任务。

## Constraints (局部硬约束)
- 仅改动 `allowed_paths` 内文件；必须跨界或改动公共契约时，必须立即暂停，并在 `Blocked` 栏写出明确原因与需改动的具体文件路径以申请权限。
- 只抓元数据（标题 / 摘要），不下载全文；原始数据与编译文本不入 Git。
- 既有冻结评测集不得改动；`source:<name>` 为契约已有可选标签，不改契约。

## Blocked (必须跨界或改契约时立即暂停：写出明确原因 + 申请修改的文件路径；无阻塞留 None)
- None

## Handoff / Evidence (由执行 Agent 填写后退出)
- Status: [PENDING]
- Changed Files:
- Verification Output:
