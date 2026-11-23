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
- Status: [DONE — 待评审] `ood_academic_v1` 已生成并复核；arXiv 训练侧文本已供 task-07 L3 训练集使用。
- Changed Files:
  - `scripts/data_pipeline/fetch_arxiv_oai.py`（新增）：OAI-PMH 有限抓取（`set=cs`、`from=2025-01-01`、上限 65,000 条、3.5 s / 请求、503 Retry-After、断点续传），逐页 SHA256 清单 `arxiv-cs.dat`。
  - `scripts/data_pipeline/build_arxiv_text.py`（新增）：只保留主分类 cs.*，清洗 LaTeX 公式 / 命令 / URL（公式改为子句断点），不输出标题；按 arXiv ID CRC32 留出 10%。
  - `scripts/data_pipeline/build_eval_v3.py`：新增 `ood_academic_v1`（与 ood_dev_v1 同法，seed + 4）；`dev_clauses(keep_source_case=True)` 丢弃原文大小写与词典词形冲突的子句（`Nielsen` / `DL` 否则会被期望答案改写成小写）。
  - `scripts/data_pipeline/test_pipeline.py`（33 tests）、`data/MANIFEST.yaml`（`arxiv-cs`，CC0-1.0）、`eval/benchmarks/ood_academic_v1.tsv`（新增，1030 行）、`eval/review/negative_review.tsv`（追加 144 条裁决，6 条剔除）、`ROADMAP.md`。
- Verification Output：
  - 抓取：50 页 / 65,000 条记录，原始 39 MB；主分类 cs.* 54,585 篇 → 训练侧 49,160 篇、留出 5,425 篇（非主分类 cs 10,407 篇丢弃）。
  - 留出论文切出 26,505 条常见词子句 + 8,908 条新词 / 术语子句；`ood_academic_v1`：8 类单错误各 60、`noise:multi` 100、`negative:clean` 300、`negative:novel` 150。
  - 复核 4 轮收敛（剔除残片、源句缺撇号 `VRs potential` / `users perceptions`、用词错误 `interested information`、公式残留变量 `n`）；既有冻结评测集（noise_v2 / ood_v1 / ood_tm2_v1 / ood_dev_v1）重新生成后逐字节不变。
  - `make check && make test && make eval && make verify-infra && verify-boundaries`：全部通过（BOUNDARY OK task-14，8 个文件零越界）。
  - **基线（去泄漏 L2 sha256 3fcbf1b9，top1，95% CI）**：

    | 指标 | L1+L2 | L1+L2 Top-5 | L1+L2+通用 SmolLM2-360M |
    |---|---|---|---|
    | TOTAL | **53.7%（50.6–56.7）** | 69.0% | 58.6%（55.6–61.6） |
    | negative:clean | 88.0 | 97.0 | 90.7 |
    | **negative:novel** | **32.0** | 34.7 | 34.0 |
    | noise:multi | 20.0 | 37.0 | 25.0 |
    | realword_edit | 43.3 | 81.7 | 58.3 |
    | space_missing / space_shift | 35.0 / 48.3 | 63.3 / 86.7 | 45.0 / 65.0 |
    | leading_drop / boundary_gemination | 53.3 / 45.0 | 65.0 / 71.7 | 55.0 / 53.3 |
    | contraction | 1.7 | 16.7 | 1.7 |

    L3 推理 p50 76 ms；按键延迟 p50 / p99 / max = 0.38 / 3.3 / 15.6 ms（契约内）。
  - **结论**：学术文本是所有评测集中最弱的领域（低于 ood_dev 62.9%）。干净句中 12% 被误改，几乎全是术语与缩写（`LLMs -> Me`、`GPUs -> Cups`、`Cholesky -> Closely`、`HuggingFace -> Hygienics`、`interpretability -> interoperability`、`datasets -> data sets`），新词句仅 32% 原样保留。通用 L3 只带来 +4.9pp，Top-5 上限 69.0% 说明召回同样不足 → L1 可挂载学术词库（缩写 / 术语 / 复数缩写）与 task-07 纠错 L3 都是必要的。
  - **已知偏差**：`noise:contraction` 在学术文本中几乎全是所有格（`models ability -> model's ability`），单复数所有格（`model's` / `models'`）本身有歧义，该子项仅作参考；含连字符的子句（`state-of-the-art`）被 `ALPHA_WORD_RE` 整句排除，连字符复合词尚未覆盖。
