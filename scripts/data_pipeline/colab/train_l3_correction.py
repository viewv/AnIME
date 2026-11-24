#!/usr/bin/env python3
"""AnIME 专用纠错 L3 模型训练（Colab / Kaggle GPU 上运行，task-07 P2）。

输入：`build_l3_sft.py` 生成的 `train.jsonl` / `val.jsonl`（每行 `{"prompt", "target", "meta"}`）。
训练：`HuggingFaceTB/SmolLM2-360M` 全参数监督微调，只对 target token 计算损失。
导出：`--export` 时把最终权重转换为 GGUF 并量化为 Q4_K_M / Q8_0（需要 git、cmake）。

分词口径（必须与 `crates/lm-neural` 推理一致）：prompt 与 target **分别**分词后拼接、不加任何特殊 token——
llama.cpp 后端同样是先编码模板前缀、再单独编码每个候选后缀并逐 token 打分。

断线续训：checkpoint 写入 `--out-dir`（建议放在 Google Drive），重复运行同一命令会自动从最近 checkpoint 恢复。
"""

from __future__ import annotations

import argparse
import json
import os
import random
import subprocess
import sys
from collections import Counter, defaultdict
from pathlib import Path


def log(msg: str) -> None:
    print(f"[l3-train] {msg}", flush=True)


def load_jsonl(path: Path, limit: int = 0) -> list[dict]:
    rows = []
    with path.open(encoding="utf-8") as f:
        for line in f:
            rows.append(json.loads(line))
            if limit and len(rows) >= limit:
                break
    return rows


def build_features(rows: list[dict], tokenizer, max_len: int) -> list[dict]:
    feats = []
    dropped = 0
    for r in rows:
        p = tokenizer(r["prompt"], add_special_tokens=False)["input_ids"]
        t = tokenizer(r["target"], add_special_tokens=False)["input_ids"]
        if not t or len(p) + len(t) > max_len:
            dropped += 1
            continue
        feats.append({
            "input_ids": p + t,
            "labels": [-100] * len(p) + t,
            "noise": r.get("meta", {}).get("noise", "?"),
        })
    log(f"tokenized {len(feats)} examples (dropped {dropped} over max_len={max_len})")
    return feats


class Collator:
    def __init__(self, pad_id: int):
        self.pad_id = pad_id

    def __call__(self, batch):
        import torch

        n = max(len(b["input_ids"]) for b in batch)
        ids = torch.full((len(batch), n), self.pad_id, dtype=torch.long)
        labels = torch.full((len(batch), n), -100, dtype=torch.long)
        attn = torch.zeros((len(batch), n), dtype=torch.long)
        for i, b in enumerate(batch):
            L = len(b["input_ids"])
            ids[i, :L] = torch.tensor(b["input_ids"])
            labels[i, :L] = torch.tensor(b["labels"])
            attn[i, :L] = 1
        return {"input_ids": ids, "labels": labels, "attention_mask": attn}


def score_candidates(model, tokenizer, prompt: str, cands: list[str], device) -> list[float]:
    """与推理端同一口径：Σ log P(target tokens | prompt)。"""
    import torch

    p = tokenizer(prompt, add_special_tokens=False)["input_ids"]
    scores = []
    for c in cands:
        t = tokenizer(c, add_special_tokens=False)["input_ids"]
        ids = torch.tensor([p + t], device=device)
        with torch.no_grad():
            logits = model(ids).logits[0, len(p) - 1:-1].float()
        lp = torch.log_softmax(logits, dim=-1)
        scores.append(float(lp[torch.arange(len(t)), torch.tensor(t, device=device)].sum()))
    return scores


def quick_eval(model, tokenizer, val_rows: list[dict], n: int, device) -> dict:
    """抽样验证：正确目标 vs "照抄原文"候选的打分胜率，按噪声类型统计（快速代理指标，不替代 anime-eval）。"""
    model.eval()
    rng = random.Random(0)
    sample = rng.sample(val_rows, min(n, len(val_rows)))
    wins: dict[str, list[int]] = defaultdict(lambda: [0, 0])
    for r in sample:
        typed = r["prompt"].split("\n")[1][len("<|typed|> "):]
        target = r["target"]
        raw_target = " " + typed + "<|end|>"
        tag = r["meta"]["noise"]
        if raw_target == target:
            # 照抄样本：正确答案就是原文，用"改错版"（首词大小写翻转）作对照，检验不乱改
            alt = " " + (typed[:1].swapcase() + typed[1:]) + "<|end|>"
            if alt == target:
                continue
            cands = [target, alt]
        else:
            cands = [target, raw_target]
        s = score_candidates(model, tokenizer, r["prompt"], cands, device)
        wins[tag][0] += int(s[0] > s[1])
        wins[tag][1] += 1
    model.train()
    return {k: round(100 * v[0] / v[1], 1) for k, v in sorted(wins.items()) if v[1]}


CHECK_KEYS = ("model.norm.weight", "model.layers.0.input_layernorm.weight", "model.layers.10.self_attn.q_proj.weight")


def _load_check_tensors(path: Path) -> dict:
    from safetensors import safe_open

    files = [path] if path.is_file() else sorted(path.glob("*.safetensors"))
    out = {}
    for f in files:
        with safe_open(str(f), framework="pt") as h:
            for k in CHECK_KEYS:
                if k in h.keys():
                    out[k] = h.get_tensor(k).float()
    return out


def verify_weights_trained(saved_dir: Path, base: str, model=None) -> None:
    """保存的权重必须 (a) 与内存中的训练后模型一致 (b) 与基座不同。

    Why: run1（2026-11-23）训练 loss 0.07，但导出的 GGUF 与基座逐字节相同（导出时基座权重覆盖了
    训练权重，见 export_base_tokenizer），本地评测才发现；存档、最终保存与转换前都必须校验。
    """
    from huggingface_hub import snapshot_download

    saved = _load_check_tensors(saved_dir)
    base_dir = Path(snapshot_download(base, allow_patterns=["*.safetensors"]))
    ref = _load_check_tensors(base_dir)
    missing = [k for k in CHECK_KEYS if k not in saved or k not in ref]
    if missing:
        raise SystemExit(f"weight check: tensors missing {missing} in {saved_dir}")
    for k in CHECK_KEYS:
        d_base = float((saved[k] - ref[k]).abs().max())
        msg = f"weight check {k}: max|saved-base|={d_base:.3e}"
        if model is not None:
            live = dict(model.named_parameters()).get(k)
            if live is not None:
                d_live = float((saved[k] - live.detach().float().cpu()).abs().max())
                msg += f", max|saved-live|={d_live:.3e}"
                if d_live > 1e-3:
                    raise SystemExit(msg + " -> saved weights differ from the trained model")
        log(msg)
        if d_base < 1e-6:
            raise SystemExit(msg + " -> saved weights are still the base model (training not persisted)")


TOKENIZER_FILES = ("tokenizer.json", "tokenizer_config.json", "vocab.json", "merges.txt", "special_tokens_map.json")


def export_base_tokenizer(hf_dir: Path, base: str) -> None:
    """把基座的原始 tokenizer 文件放进导出目录（词表从未改动）。

    Why: snapshot_download 返回的是整个本地缓存快照目录，里面还有加载模型时下载的基座
    model.safetensors / config.json；run1 曾因遍历拷贝整个目录，用基座权重覆盖了训练好的权重。
    只按文件名白名单拷贝。
    """
    from huggingface_hub import snapshot_download

    src = Path(snapshot_download(base, allow_patterns=list(TOKENIZER_FILES)))
    for name in TOKENIZER_FILES:
        (hf_dir / name).unlink(missing_ok=True)
        if (src / name).is_file():
            (hf_dir / name).write_bytes((src / name).read_bytes())


def export_gguf(hf_dir: Path, out_dir: Path, name: str, work: Path, base: str) -> None:
    llama = work / "llama.cpp"
    if not llama.exists():
        subprocess.run(["git", "clone", "--depth", "1", "https://github.com/ggml-org/llama.cpp", str(llama)], check=True)
    commit = subprocess.run(["git", "-C", str(llama), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    log(f"llama.cpp commit {commit}")
    # 不装 requirements-convert_hf_to_gguf.txt：它会把训练环境的 transformers 降级到 4.x，而 4.x 读不了
    # 5.x 保存的 tokenizer_config.json（extra_special_tokens 为 list -> AttributeError）。
    # 词表从未改动，导出时直接换回基座的原始 tokenizer 文件，转换器只依赖仓库自带的 gguf-py。
    export_base_tokenizer(hf_dir, base)
    verify_weights_trained(hf_dir, base)  # 转换前再确认一次：导出目录里必须是训练后的权重
    f16 = out_dir / f"{name}.F16.gguf"
    subprocess.run([sys.executable, str(llama / "convert_hf_to_gguf.py"), str(hf_dir), "--outtype", "f16", "--outfile", str(f16)], check=True)
    build = llama / "build"
    if not (build / "bin" / "llama-quantize").exists():
        subprocess.run(["cmake", "-S", str(llama), "-B", str(build), "-DGGML_CUDA=OFF", "-DLLAMA_CURL=OFF"], check=True)
        subprocess.run(["cmake", "--build", str(build), "--target", "llama-quantize", "-j", str(os.cpu_count() or 4)], check=True)
    for q in ("Q4_K_M", "Q8_0"):
        subprocess.run([str(build / "bin" / "llama-quantize"), str(f16), str(out_dir / f"{name}.{q}.gguf"), q], check=True)
    (out_dir / "export_info.json").write_text(json.dumps({"llama_cpp_commit": commit, "name": name}, indent=2))
    log(f"exported GGUF files to {out_dir}")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--data-dir", type=Path, required=True, help="含 train.jsonl / val.jsonl 的目录")
    ap.add_argument("--out-dir", type=Path, required=True, help="checkpoint 与导出目录（建议 Google Drive）")
    ap.add_argument("--base", default="HuggingFaceTB/SmolLM2-360M")
    ap.add_argument("--name", default="anime-l3-360m-v1")
    ap.add_argument("--max-len", type=int, default=192)
    ap.add_argument("--epochs", type=float, default=1.0)
    ap.add_argument("--lr", type=float, default=1e-4)
    ap.add_argument("--batch-size", type=int, default=0, help="每卡 batch（0 = 按显存自动）")
    ap.add_argument("--grad-accum", type=int, default=0, help="梯度累积（0 = 自动凑约 256 条/步）")
    ap.add_argument("--save-steps", type=int, default=500)
    ap.add_argument("--eval-samples", type=int, default=400)
    ap.add_argument("--smoke", action="store_true", help="冒烟：2000 条样本、50 步、快速导出验证全流程")
    ap.add_argument("--export", action="store_true", help="训练结束后转换并量化 GGUF")
    ap.add_argument("--export-only", action="store_true", help="跳过训练，直接导出 out-dir/final（或 --export-from）")
    ap.add_argument("--export-from", type=Path, default=None, help="--export-only 时改从该目录（如某个 checkpoint）导出")
    ap.add_argument("--allow-transformers5", action="store_true", help="允许 transformers 5.x（未按 5.x 验证）")
    args = ap.parse_args(argv)

    import torch
    import transformers
    from transformers import AutoModelForCausalLM, AutoTokenizer, Trainer, TrainerCallback, TrainingArguments

    if int(transformers.__version__.split(".")[0]) >= 5 and not args.allow_transformers5:
        # 训练脚本按 4.x 编写与验证（5.x 去掉了 group_by_length 等参数）；锁定版本保证可复现
        raise SystemExit(f"transformers {transformers.__version__} unverified: pip install 'transformers==4.57.6' "
                         "(or pass --allow-transformers5)")

    args.out_dir.mkdir(parents=True, exist_ok=True)
    final_dir = args.out_dir / "final"
    if args.export_only:
        src_dir = final_dir
        if args.export_from:
            # checkpoint 目录含优化器状态，只拷模型文件到干净目录再导出
            import shutil

            final_dir.mkdir(parents=True, exist_ok=True)
            for f in ("config.json", "generation_config.json", "model.safetensors"):
                if (args.export_from / f).is_file():
                    shutil.copy(args.export_from / f, final_dir / f)
            log(f"export from {args.export_from}")
        verify_weights_trained(src_dir, args.base)
        export_gguf(src_dir, args.out_dir, args.name, Path("/tmp/l3_export"), args.base)
        return 0

    gpu = torch.cuda.get_device_name(0) if torch.cuda.is_available() else "cpu"
    mem_gb = torch.cuda.get_device_properties(0).total_memory / 2**30 if torch.cuda.is_available() else 0
    # T4（sm_75）上 is_bf16_supported() 也可能返回 True（软件模拟，极慢）；只在 Ampere（sm_80）及以上用 bf16
    bf16 = torch.cuda.is_available() and torch.cuda.get_device_capability(0)[0] >= 8
    log(f"torch {torch.__version__}, transformers {transformers.__version__}, device {gpu} ({mem_gb:.0f} GB), bf16={bf16}")

    tokenizer = AutoTokenizer.from_pretrained(args.base)
    pad_id = tokenizer.pad_token_id if tokenizer.pad_token_id is not None else tokenizer.eos_token_id
    limit = 2000 if args.smoke else 0
    train_rows = load_jsonl(args.data_dir / "train.jsonl", limit)
    val_rows = load_jsonl(args.data_dir / "val.jsonl", 500 if args.smoke else 0)
    log(f"train rows {len(train_rows)}, val rows {len(val_rows)}; noise mix {Counter(r['meta']['noise'] for r in train_rows).most_common(6)}")
    train_feats = build_features(train_rows, tokenizer, args.max_len)
    val_feats = build_features(val_rows[:5000], tokenizer, args.max_len)

    model = AutoModelForCausalLM.from_pretrained(
        args.base,
        dtype=torch.bfloat16 if bf16 else torch.float32,
        attn_implementation="sdpa",
    )
    bs = args.batch_size or (64 if mem_gb >= 70 else 32 if mem_gb >= 35 else 16)
    accum = args.grad_accum or max(1, 256 // bs)
    use_gc = mem_gb < 35
    if use_gc:
        model.gradient_checkpointing_enable()

    class FeatDS(torch.utils.data.Dataset):
        def __init__(self, feats):
            self.f = feats

        def __len__(self):
            return len(self.f)

        def __getitem__(self, i):
            return {"input_ids": self.f[i]["input_ids"], "labels": self.f[i]["labels"]}

    import inspect

    targ_params = set(inspect.signature(TrainingArguments.__init__).parameters)
    targ_kwargs = dict(
        output_dir=str(args.out_dir / "checkpoints"),
        num_train_epochs=args.epochs,
        max_steps=50 if args.smoke else -1,
        per_device_train_batch_size=bs,
        per_device_eval_batch_size=bs,
        gradient_accumulation_steps=accum,
        learning_rate=args.lr,
        lr_scheduler_type="cosine",
        weight_decay=0.01,
        bf16=bf16,
        fp16=torch.cuda.is_available() and not bf16,
        logging_steps=10 if args.smoke else 50,
        eval_strategy="steps",
        eval_steps=25 if args.smoke else args.save_steps,
        save_steps=25 if args.smoke else args.save_steps,
        save_total_limit=2,
        group_by_length=True,
        dataloader_num_workers=2,
        report_to=[],
        remove_unused_columns=False,
    )
    # transformers 5.x 删除了 warmup_ratio（warmup_steps 接受 < 1 的比例）；按签名兼容 4.x / 5.x
    if "warmup_ratio" in targ_params:
        targ_kwargs["warmup_ratio"] = 0.02
    else:
        targ_kwargs["warmup_steps"] = 0.02
    dropped = sorted(k for k in targ_kwargs if k not in targ_params)
    if dropped:
        log(f"TrainingArguments does not accept {dropped}; ignored")
    targs = TrainingArguments(**{k: v for k, v in targ_kwargs.items() if k in targ_params})
    class VerifyCheckpoint(TrainerCallback):
        """每次存档后立即校验权重已更新，避免在失效的保存路径上跑完整轮。"""

        def on_save(self, a, state, control, **kw):
            verify_weights_trained(Path(a.output_dir) / f"checkpoint-{state.global_step}", args.base, kw.get("model"))

    trainer = Trainer(
        model=model,
        args=targs,
        train_dataset=FeatDS(train_feats),
        eval_dataset=FeatDS(val_feats),
        data_collator=Collator(pad_id),
        callbacks=[VerifyCheckpoint()],
    )
    ckpts = sorted((args.out_dir / "checkpoints").glob("checkpoint-*"), key=lambda p: int(p.name.split("-")[1]))
    resume = str(ckpts[-1]) if ckpts and not args.smoke else None
    log(f"batch {bs} x accum {accum}, grad_checkpointing={use_gc}, resume={resume}")
    train_out = trainer.train(resume_from_checkpoint=resume)

    trainer.save_model(str(final_dir))
    tokenizer.save_pretrained(str(final_dir))
    verify_weights_trained(final_dir, args.base, model)
    device = next(model.parameters()).device
    report = {
        "eval_loss": trainer.evaluate().get("eval_loss"),
        "pairwise_win_pct_by_noise": quick_eval(model, tokenizer, val_rows, 100 if args.smoke else args.eval_samples, device),
        "base": args.base,
        "train_examples": len(train_feats),
        "batch": bs,
        "grad_accum": accum,
        "lr": args.lr,
        "epochs": args.epochs,
        "device": gpu,
        "bf16": bf16,
        "train_metrics": train_out.metrics,
        "mean_tokens_per_example": round(sum(len(f["input_ids"]) for f in train_feats) / max(1, len(train_feats)), 1),
    }
    (args.out_dir / "train_report.json").write_text(json.dumps(report, indent=2, ensure_ascii=False))
    log(f"report: {json.dumps(report, ensure_ascii=False)}")

    if args.export or args.smoke:
        export_gguf(final_dir, args.out_dir, args.name + ("-smoke" if args.smoke else ""), Path("/tmp/l3_export"), args.base)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
