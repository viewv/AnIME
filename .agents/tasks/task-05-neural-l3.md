---
task_id: "task-05-neural-l3"
worktree: "../wt-anime-neural-l3"
allowed_paths:
  - "crates/lm-neural/**"
  - "crates/engine/**"
  - "crates/animed/**"
  - "crates/eval/**"
  - "data/MANIFEST.yaml"
  - "scripts/data_pipeline/**"
  - "Makefile"
forbidden_paths:
  - ".agents/contracts/**"
  - "frontends/**"
  - "crates/protocol/**"
  - "crates/lexicon/**"
  - "crates/lm-ngram/**"
read_only_context:
  - ".agents/contracts/core-api.md"
  - ".agents/contracts/ipc-protocol.md"
  - ".agents/contracts/data-format.md"
  - ".agents/contracts/eval-schema.md"
acceptance_cmd: "cargo test --workspace && ./scripts/verify-boundaries.sh .agents/tasks/task-05-neural-l3.md"
---

## Objective (目标)
为 `crates/lm-neural` 实现基于 `llama.cpp` 的端侧 L3 神经语言模型推理后端（Vulkan GPU 优先，支持 CPU 自动回退，全仓保持 `unsafe_code = "forbid"`），接入前缀 KV-Cache 复用重排与 `✦` 续写预测，拉取并登记 100% 开源（Apache-2.0）GGUF 基座模型（`SmolLM2-360M` 与 `SmolLM2-135M`）至 `data/MANIFEST.yaml`，完成 `animed` / `eval` 集成与本机 AMD Radeon 780M Vulkan GPU vs CPU 延迟和 7 类准确率实测。

## Constraints (局部硬约束)
- 仅改动 `allowed_paths` 内文件；必须跨界或改动公共契约时，立即暂停并在 `Blocked` 栏申请。
- `crates/lm-neural` 与 `crates/engine` 严禁引入 Linux/Fcitx5/Wayland/macOS 平台专有依赖；默认不带特性的 `cargo test --workspace` 在离线无 `.gguf` 环境下必须稳定通过。
- 严格遵守 100% 开源洁净数据原则与 `.agents/contracts/data-format.md`，不得向 Git 提交 `.gguf` 二进制文件。
- 无 `.gguf` 模型或 Vulkan 设备不可用时自动优雅降级，绝不阻断输入法正常打字与 L1+L2 同步响应。

## Blocked (必须跨界或改契约时立即暂停：写出明确原因 + 申请修改的文件路径；无阻塞留 None)
- None

## Handoff / Evidence (由执行 Agent 填写后退出)
- Status: [DONE]
- Changed Files:
  - `crates/lm-neural/{Cargo.toml,src/lib.rs,src/config.rs,src/prompt.rs,src/llama_backend.rs}`
  - `crates/engine/src/lib.rs`
  - `crates/animed/{Cargo.toml,src/main.rs,src/paths.rs,src/l3_scheduler.rs,src/server.rs}`
  - `crates/eval/{Cargo.toml,src/cli.rs,src/main.rs,src/metrics.rs,src/report.rs}`
  - `scripts/data_pipeline/fetch_models.py`, `data/MANIFEST.yaml`, `Makefile`, `.agents/tasks/task-05-neural-l3.md`
- Verification Output:
  - Offline workspace (`make check && make test && make eval && make verify-infra`): all exit 0 with 0 warnings (`engine`: 10 unit + 1 integration, `lexicon`: 8 + 2, `lm-neural`: 5, `lm-ngram`: 5, `protocol`: 4, `animed`: 4, `eval`: 7; `INFRA OK`).
  - Vulkan build (`make engine-vulkan && cargo build --release -p anime-eval --features vulkan`): both exit 0 with 0 warnings after user installed `spirv-headers`.
  - Feature `--features llama` integration test (`cargo test --release -p anime-lm-neural --features llama -- --ignored --nocapture`): `llama_smollm2_135m_rerank_and_continuation_smoke ... ok` in `0.21s`.
  - Release Vulkan stdio E2E (`target/release/animed --stdio --model data/compiled/models/SmolLM2-360M.Q4_K_M.gguf --l3-debounce-ms 0`): `backend=llama-vulkan, gpu_offload=true, device=Vulkan0(Vulkan), warmup=910ms`, `hello_ack` advertises `"l3-neural"`, `seq=4 revision=0 final_l3=false` -> `revision=1 final_l3=true` with reranked scores and `✦` continuations `['for your', 'for the']`; nonexistent `--model` path gracefully warns & falls back to `NoopNeuralModel` (exit 0).
  - Real 1400-case CPU vs Vulkan GPU benchmark (`eval/benchmarks/core_v1.tsv` + compiled `lexicon.animelex.bin` & `ngram.animengr.bin`):
    - Baseline L1+L2: `TOTAL top1=72.5% top3=89.9% top5=93.3% strict1=70.8% p50=941us p99=28625us`; `context_realword top1=65.0%`; `sentence top1=74.5%`.
    - L1+L2+L3 CPU (`SmolLM2-135M-Q4_K_M.gguf`, `gpu-layers=0`): `TOTAL top1=75.6% top3=90.0% top5=93.4% strict1=73.9%`; `context_realword top1=85.0%`; `sentence top1=76.5%`; `L3 calls=400, warmup=115ms, p50=33986us, p99=62349us, mean=35241us`.
    - L1+L2+L3 Vulkan GPU (`SmolLM2-135M-Q4_K_M.gguf`, `gpu-layers=999`, `Vulkan0(Vulkan)`): `TOTAL top1=75.7% top3=90.0% top5=93.4% strict1=73.9%`; `context_realword top1=84.5%`; `sentence top1=77.5%`; `L3 calls=400, warmup=163ms, p50=32363us, p99=178091us, mean=39393us`.
    - L1+L2+L3 CPU (`SmolLM2-360M.Q4_K_M.gguf`, `gpu-layers=0`): `TOTAL top1=76.5% top3=90.3% top5=93.4% strict1=74.7%`; `context_realword top1=86.0%`; `sentence top1=81.5% (top3=91.0%, strict1=70.5%)`; `L3 calls=400, warmup=82ms, p50=94784us, p99=158084us, mean=95132us`.
    - L1+L2+L3 Vulkan GPU (`SmolLM2-360M.Q4_K_M.gguf`, `gpu-layers=999`, `Vulkan0(Vulkan)`): `TOTAL top1=76.5% top3=90.3% top5=93.4% strict1=74.6%`; `context_realword top1=86.0% (+21.0pp)`; `sentence top1=81.5% (+7.0pp, top3=91.0%, strict1=70.0%)`; `L3 calls=400, warmup=341ms, p50=43685us (2.17x vs CPU), p99=90399us, mean=47906us (1.99x vs CPU)`.
  - Live Page-1 continuation slot placement (`crates/engine/src/lib.rs`, `apply_neural_rerank` places up to 2 `✦` continuations at slots 2–3 right after Top-1): verified via `--stdio` (`1. thank you for`, `2. ✦ thank you for the`, `3. ✦ thank you for your`) and live desktop Fcitx5 session over `/run/user/1000/anime/engined.sock`.
  - Boundary guard (`./scripts/verify-boundaries.sh .agents/tasks/task-05-neural-l3.md`): `BOUNDARY OK`.
