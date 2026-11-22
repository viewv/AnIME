use std::num::NonZeroU32;
use std::sync::mpsc::{self, SyncSender};
use std::sync::OnceLock;
use std::thread::{self, JoinHandle};
use std::time::Instant;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::list_llama_ggml_backend_devices;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::token::LlamaToken;
use llama_cpp_2::{LlamaBackendDeviceType, LogOptions, send_logs_to_tracing};

use crate::config::{NeuralError, NeuralModelConfig, NeuralRuntimeInfo, validate_gguf_header};
use crate::prompt::{
    build_continuation_plan, extract_clean_continuation_word, normalize_rerank_scores,
    split_prefix_and_candidates,
};
use crate::{NeuralCandidate, NeuralSentenceModel};

static GLOBAL_LLAMA_BACKEND: OnceLock<Result<LlamaBackend, String>> = OnceLock::new();

fn global_backend(verbose_logs: bool) -> Result<&'static LlamaBackend, NeuralError> {
    let init_res = GLOBAL_LLAMA_BACKEND.get_or_init(|| {
        if !verbose_logs {
            send_logs_to_tracing(LogOptions::default().with_logs_enabled(false));
        }
        let mut backend = LlamaBackend::init().map_err(|e| e.to_string())?;
        if !verbose_logs {
            backend.void_logs();
        }
        Ok(backend)
    });
    match init_res {
        Ok(b) => Ok(b),
        Err(msg) => Err(NeuralError::BackendInit(msg.clone())),
    }
}

enum WorkerRequest {
    Rerank {
        context_before: String,
        candidates: Vec<String>,
        reply: SyncSender<Vec<NeuralCandidate>>,
    },
    Continuations {
        context_before: String,
        prefix: String,
        limit: usize,
        reply: SyncSender<Vec<NeuralCandidate>>,
    },
    Shutdown,
}

/// 基于专属推理线程的 `llama.cpp` Vulkan GPU 后端：
/// 在单线程栈上持久持有 `LlamaModel` + `LlamaContext`（零 `unsafe`、零内存泄漏），
/// 全程复用 GPU KV-Cache 与 prefill logits。
pub struct LlamaNeuralModel {
    info: NeuralRuntimeInfo,
    tx: SyncSender<WorkerRequest>,
    worker: Option<JoinHandle<()>>,
}

impl LlamaNeuralModel {
    pub fn load(config: NeuralModelConfig) -> Result<Self, NeuralError> {
        validate_gguf_header(&config.model_path)?;
        let backend = global_backend(config.verbose_logs)?;

        let devices = list_llama_ggml_backend_devices();
        let gpu_devs: Vec<String> = devices
            .iter()
            .filter(|d| {
                matches!(
                    d.device_type,
                    LlamaBackendDeviceType::Gpu
                        | LlamaBackendDeviceType::IntegratedGpu
                        | LlamaBackendDeviceType::Accelerator
                )
            })
            .map(|d| format!("{}({})", d.name, d.backend))
            .collect();
        let gpu_offload_active =
            config.n_gpu_layers > 0 && backend.supports_gpu_offload() && !gpu_devs.is_empty();
        let device_summary = if gpu_offload_active {
            gpu_devs.join(", ")
        } else {
            "CPU fallback".to_string()
        };

        let model_name = config
            .model_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("model.gguf")
            .to_string();

        let (init_tx, init_rx) = mpsc::sync_channel::<Result<(u64, SyncSender<WorkerRequest>), NeuralError>>(1);
        let worker_cfg = config.clone();

        let handle = thread::Builder::new()
            .name("anime-llama-worker".to_string())
            .spawn(move || {
                let t0 = Instant::now();
                let requested_layers = if gpu_offload_active {
                    worker_cfg.n_gpu_layers
                } else {
                    0
                };
                let model_params = LlamaModelParams::default().with_n_gpu_layers(requested_layers);
                let model = match LlamaModel::load_from_file(
                    backend,
                    &worker_cfg.model_path,
                    &model_params,
                ) {
                    Ok(m) => m,
                    Err(e) => {
                        let _ = init_tx.send(Err(NeuralError::LoadModel(e.to_string())));
                        return;
                    }
                };

                let n_ctx = worker_cfg.n_ctx.max(256);
                let n_threads = worker_cfg.n_threads.max(1);
                let ctx_params = LlamaContextParams::default()
                    .with_n_ctx(NonZeroU32::new(n_ctx))
                    .with_n_batch(n_ctx)
                    .with_n_ubatch(n_ctx.min(256))
                    .with_n_seq_max(1)
                    .with_n_threads(n_threads)
                    .with_n_threads_batch(n_threads)
                    .with_offload_kqv(gpu_offload_active)
                    .with_no_perf(true);
                let ctx = match model.new_context(backend, ctx_params) {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = init_tx.send(Err(NeuralError::LoadContext(e.to_string())));
                        return;
                    }
                };

                let mut runner = KvRunner::new(&model, ctx, worker_cfg);
                runner.warmup();
                let warmup_ms = t0.elapsed().as_millis() as u64;

                let (tx, rx) = mpsc::sync_channel::<WorkerRequest>(16);
                if init_tx.send(Ok((warmup_ms, tx))).is_err() {
                    return;
                }

                while let Ok(req) = rx.recv() {
                    match req {
                        WorkerRequest::Rerank {
                            context_before,
                            candidates,
                            reply,
                        } => {
                            let res = runner.rerank(&context_before, &candidates);
                            let _ = reply.send(res);
                        }
                        WorkerRequest::Continuations {
                            context_before,
                            prefix,
                            limit,
                            reply,
                        } => {
                            let res = runner.continuations(&context_before, &prefix, limit);
                            let _ = reply.send(res);
                        }
                        WorkerRequest::Shutdown => break,
                    }
                }
            })
            .map_err(|e| NeuralError::BackendInit(format!("failed spawning worker thread: {e}")))?;

        let (warmup_ms, tx) = match init_rx.recv() {
            Ok(Ok(ok)) => ok,
            Ok(Err(err)) => {
                let _ = handle.join();
                return Err(err);
            }
            Err(_) => {
                let _ = handle.join();
                return Err(NeuralError::BackendInit(
                    "llama worker terminated unexpectedly during initialization".to_string(),
                ));
            }
        };

        let backend_name = if !gpu_offload_active {
            "llama-cpu"
        } else if cfg!(feature = "llama-vulkan") {
            "llama-vulkan"
        } else if cfg!(feature = "llama-metal") {
            "llama-metal"
        } else if cfg!(feature = "llama-cuda") {
            "llama-cuda"
        } else {
            "llama-gpu"
        };

        Ok(Self {
            info: NeuralRuntimeInfo {
                backend_name,
                model_name,
                gpu_layers_requested: config.n_gpu_layers,
                gpu_offload_active,
                device_summary,
                warmup_ms,
            },
            tx,
            worker: Some(handle),
        })
    }

    pub fn runtime_info(&self) -> &NeuralRuntimeInfo {
        &self.info
    }
}

impl Drop for LlamaNeuralModel {
    fn drop(&mut self) {
        let _ = self.tx.send(WorkerRequest::Shutdown);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

impl NeuralSentenceModel for LlamaNeuralModel {
    fn backend_name(&self) -> &'static str {
        self.info.backend_name
    }

    fn rerank_sentences(
        &self,
        context_before: &str,
        _preedit: &str,
        candidates: &[String],
    ) -> Vec<NeuralCandidate> {
        if candidates.len() < 2 {
            return Vec::new();
        }
        if context_before.trim().is_empty() && !candidates.iter().any(|c| c.trim().contains(' ')) {
            return Vec::new();
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = WorkerRequest::Rerank {
            context_before: context_before.to_string(),
            candidates: candidates.to_vec(),
            reply: reply_tx,
        };
        if self.tx.send(req).is_err() {
            return Vec::new();
        }
        reply_rx.recv().unwrap_or_default()
    }

    fn predict_continuations(
        &self,
        context_before: &str,
        prefix: &str,
        limit: usize,
    ) -> Vec<NeuralCandidate> {
        if limit == 0 {
            return Vec::new();
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let req = WorkerRequest::Continuations {
            context_before: context_before.to_string(),
            prefix: prefix.to_string(),
            limit,
            reply: reply_tx,
        };
        if self.tx.send(req).is_err() {
            return Vec::new();
        }
        reply_rx.recv().unwrap_or_default()
    }
}

struct KvRunner<'m> {
    model: &'m LlamaModel,
    ctx: LlamaContext<'m>,
    batch: LlamaBatch<'m>,
    cfg: NeuralModelConfig,
    bos: LlamaToken,
    has_bos: bool,
    prefix_tokens: Vec<LlamaToken>,
    prefix_logprobs: Vec<f32>,
}

impl<'m> KvRunner<'m> {
    fn new(model: &'m LlamaModel, ctx: LlamaContext<'m>, cfg: NeuralModelConfig) -> Self {
        let vocab = model.vocab();
        let has_bos = vocab.should_add_bos() && vocab.bos().0 >= 0;
        let bos = vocab.bos();
        let batch_cap = (cfg.n_ctx.max(256) as usize).min(512);
        Self {
            model,
            ctx,
            batch: LlamaBatch::new(batch_cap, 1),
            cfg,
            bos,
            has_bos,
            prefix_tokens: Vec::new(),
            prefix_logprobs: Vec::new(),
        }
    }

    fn warmup(&mut self) {
        let tok = self.tokenize_with_bos("Hello", self.cfg.max_context_tokens);
        let _ = self.ensure_prefix_kv(&tok);
        self.reset_kv_state();
    }

    fn reset_kv_state(&mut self) {
        self.ctx.clear_kv_cache();
        self.prefix_tokens.clear();
        self.prefix_logprobs.clear();
    }

    fn tokenize_with_bos(&self, text: &str, max_tokens: usize) -> Vec<LlamaToken> {
        let vocab = self.model.vocab();
        let mut tokens = vocab.tokenize(text.as_bytes(), false, false);
        let cap = max_tokens.saturating_sub(usize::from(self.has_bos));
        if cap > 0 && tokens.len() > cap {
            tokens.drain(..tokens.len() - cap);
        }
        if self.has_bos {
            let mut full = Vec::with_capacity(tokens.len() + 1);
            full.push(self.bos);
            full.extend(tokens);
            full
        } else if tokens.is_empty() {
            if vocab.bos().0 >= 0 {
                vec![self.bos]
            } else if vocab.eos().0 >= 0 {
                vec![vocab.eos()]
            } else {
                tokens
            }
        } else {
            tokens
        }
    }

    /// 增量同步 GPU KV-Cache 至指定 `target_prefix` 并缓存末位完整 `log_softmax` 分布。
    fn ensure_prefix_kv(&mut self, target_prefix: &[LlamaToken]) -> bool {
        if target_prefix.is_empty() {
            self.reset_kv_state();
            return true;
        }
        if self.prefix_tokens == target_prefix && !self.prefix_logprobs.is_empty() {
            return true;
        }

        let common = self
            .prefix_tokens
            .iter()
            .zip(target_prefix.iter())
            .take_while(|(a, b)| a == b)
            .count();

        // 若目标是旧缓存的真前缀，须退回 1 个位置重做 decode 才能取到新结尾位置的 logits。
        let mut decode_from = if common == target_prefix.len() && common > 0 {
            common - 1
        } else {
            common
        };

        if decode_from == 0 {
            self.ctx.clear_kv_cache();
        } else if decode_from < self.prefix_tokens.len() {
            match self
                .ctx
                .clear_kv_cache_seq(Some(0), Some(decode_from as u32), None)
            {
                Ok(true) => {}
                _ => {
                    self.ctx.clear_kv_cache();
                    decode_from = 0;
                }
            }
        }

        let tail = &target_prefix[decode_from..];
        if tail.is_empty() {
            self.prefix_tokens = target_prefix.to_vec();
            return !self.prefix_logprobs.is_empty();
        }

        self.batch.clear();
        let last_idx = tail.len() - 1;
        for (i, &tok) in tail.iter().enumerate() {
            let pos = (decode_from + i) as i32;
            if self.batch.add(tok, pos, &[0], i == last_idx).is_err() {
                self.reset_kv_state();
                return false;
            }
        }
        if self.ctx.decode(&mut self.batch).is_err() {
            self.reset_kv_state();
            return false;
        }

        let logits = self.ctx.get_logits_ith(last_idx as i32);
        log_softmax_into(logits, &mut self.prefix_logprobs);
        self.prefix_tokens = target_prefix.to_vec();
        true
    }

    fn rerank(&mut self, context_before: &str, candidates: &[String]) -> Vec<NeuralCandidate> {
        let eval_limit = candidates.len().min(self.cfg.max_rerank_candidates);
        if eval_limit < 2 {
            return Vec::new();
        }
        let target_slice = &candidates[..eval_limit];
        let (prefix_str, suffixes) = split_prefix_and_candidates(context_before, target_slice);

        let vocab = self.model.vocab();
        let mut tokenized: Vec<(usize, Vec<LlamaToken>)> = Vec::with_capacity(eval_limit);
        for (orig_idx, suffix) in suffixes.iter().enumerate() {
            if suffix.is_empty() {
                continue;
            }
            let toks = vocab.tokenize(suffix.as_bytes(), false, false);
            if !toks.is_empty() {
                tokenized.push((orig_idx, toks));
            }
        }
        if tokenized.len() < 2 {
            return Vec::new();
        }

        let prefix_toks = self.tokenize_with_bos(&prefix_str, self.cfg.max_context_tokens);
        if prefix_toks.is_empty() || !self.ensure_prefix_kv(&prefix_toks) {
            return Vec::new();
        }
        let prefix_len = self.prefix_tokens.len();

        let mut scores = vec![f32::NAN; eval_limit];
        let mut multi_indices: Vec<usize> = Vec::new();

        // 单 token 候选直接从前缀末位 logprobs O(1) 读取，无需额外 GPU decode。
        for (item_pos, (orig_idx, toks)) in tokenized.iter().enumerate() {
            if toks.len() == 1 {
                if let Some(&lp) = self.prefix_logprobs.get(toks[0].0.max(0) as usize) {
                    scores[*orig_idx] = lp;
                }
            } else {
                multi_indices.push(item_pos);
            }
        }

        if !multi_indices.is_empty() {
            // 按 token 序列排序，最大化相邻候选句子的 KV 公共前缀复用。
            multi_indices.sort_by(|&a, &b| tokenized[a].1.cmp(&tokenized[b].1));

            let mut prev_tokens: Vec<LlamaToken> = Vec::new();
            let mut prev_step_lps: Vec<f32> = Vec::new();

            for &item_pos in &multi_indices {
                let (orig_idx, ref toks) = tokenized[item_pos];
                if let Some(norm_lp) = self.score_multi_token_candidate(
                    prefix_len,
                    toks,
                    &mut prev_tokens,
                    &mut prev_step_lps,
                ) {
                    scores[orig_idx] = norm_lp;
                }
                if self.prefix_tokens.len() != prefix_len {
                    break;
                }
            }

            // 还原 KV Cache 到纯粹的前缀结尾状态，保证下次请求直接命中。
            if !prev_tokens.is_empty() {
                match self
                    .ctx
                    .clear_kv_cache_seq(Some(0), Some(prefix_len as u32), None)
                {
                    Ok(true) => {}
                    _ => self.reset_kv_state(),
                }
            }
        }

        let pairs: Vec<(String, f32)> = target_slice
            .iter()
            .enumerate()
            .filter_map(|(i, text)| {
                let s = scores[i];
                s.is_finite().then(|| (text.clone(), s))
            })
            .collect();

        normalize_rerank_scores(&pairs, self.cfg.rerank_weight)
    }

    fn score_multi_token_candidate(
        &mut self,
        prefix_len: usize,
        toks: &[LlamaToken],
        prev_tokens: &mut Vec<LlamaToken>,
        prev_step_lps: &mut Vec<f32>,
    ) -> Option<f32> {
        let l = toks.len();
        if l <= 1 || prefix_len + l >= self.cfg.n_ctx as usize {
            return None;
        }

        let mut step_lps = Vec::with_capacity(l);
        let first_lp = *self.prefix_logprobs.get(toks[0].0.max(0) as usize)?;
        step_lps.push(first_lp);

        let common = prev_tokens
            .iter()
            .zip(toks.iter())
            .take_while(|(a, b)| a == b)
            .count();

        if common >= l {
            // 新候选是前一条已打分候选的严格前缀，所有单步 logprob 均已在手。
            for k in 1..l {
                step_lps.push(*prev_step_lps.get(k)?);
            }
            let sum: f32 = step_lps.iter().sum();
            return Some(sum / (l as f32).powf(0.8));
        }

        // 重用前 common 步已算过的条件概率；若 common > 0，仅保留 prefix + common - 1，
        // 从 toks[common - 1] 开始重 decode 直到 toks[l - 2] 以获得 toks[common..l] 的准确预测分布。
        for k in 1..common {
            step_lps.push(*prev_step_lps.get(k)?);
        }

        let mut decode_start = if common > 0 { common - 1 } else { 0 };
        if decode_start < prev_tokens.len() {
            let keep_until = (prefix_len + decode_start) as u32;
            match self.ctx.clear_kv_cache_seq(Some(0), Some(keep_until), None) {
                Ok(true) => {}
                _ => {
                    // 局部清 KV 失败时保守全清，保证状态机绝对一致。
                    self.reset_kv_state();
                    prev_tokens.clear();
                    prev_step_lps.clear();
                    return None;
                }
            }
        }

        if decode_start > l - 1 {
            decode_start = l - 1;
        }

        let to_decode = &toks[decode_start..l - 1];
        if !to_decode.is_empty() {
            self.batch.clear();
            for (offset, &tok) in to_decode.iter().enumerate() {
                let pos = (prefix_len + decode_start + offset) as i32;
                if self.batch.add(tok, pos, &[0], true).is_err() {
                    self.reset_kv_state();
                    prev_tokens.clear();
                    prev_step_lps.clear();
                    return None;
                }
            }
            if self.ctx.decode(&mut self.batch).is_err() {
                self.reset_kv_state();
                prev_tokens.clear();
                prev_step_lps.clear();
                return None;
            }
            for offset in 0..to_decode.len() {
                let target_idx = decode_start + offset + 1;
                let target_tok = toks[target_idx];
                let logits = self.ctx.get_logits_ith(offset as i32);
                let lp = single_token_logprob(logits, target_tok.0)?;
                if step_lps.len() == target_idx {
                    step_lps.push(lp);
                }
            }
        }

        if step_lps.len() != l {
            return None;
        }
        *prev_tokens = to_decode
            .is_empty()
            .then(Vec::new)
            .unwrap_or_else(|| toks[..l - 1].to_vec());
        *prev_step_lps = step_lps.clone();

        let sum: f32 = step_lps.iter().sum();
        Some(sum / (l as f32).powf(0.8))
    }

    fn continuations(
        &mut self,
        context_before: &str,
        prefix: &str,
        limit: usize,
    ) -> Vec<NeuralCandidate> {
        let Some(plan) = build_continuation_plan(context_before, prefix) else {
            return Vec::new();
        };
        let prompt_toks = self.tokenize_with_bos(&plan.prompt, self.cfg.max_context_tokens);
        if prompt_toks.is_empty() || !self.ensure_prefix_kv(&prompt_toks) {
            return Vec::new();
        }
        let base_len = self.prefix_tokens.len();
        if base_len + self.cfg.max_continuation_tokens + 2 >= self.cfg.n_ctx as usize {
            return Vec::new();
        }

        let vocab = self.model.vocab();
        let top_k = collect_top_token_logprobs(&self.prefix_logprobs, 24);
        let mut results: Vec<NeuralCandidate> = Vec::with_capacity(limit);

        for (tok_id, first_lp) in top_k {
            if results.len() >= limit {
                break;
            }
            let tok = LlamaToken::new(tok_id);
            if vocab.is_eog(tok) || vocab.is_control(tok) {
                continue;
            }
            let mut piece = String::from_utf8_lossy(&vocab.token_to_piece(tok, false, None)).into_owned();
            if plan.expect_leading_space && !piece.starts_with(' ') {
                continue;
            }

            // 对被 BPE 拆断的长词（例如 " environ" -> "ment"），最多向后延伸 2 步以补完整英文词
            let mut total_lp = first_lp;
            let mut step_pos = base_len;
            let mut cur_tok = tok;
            for _ in 0..self.cfg.max_continuation_tokens.saturating_sub(1) {
                let tail_word = piece.trim_start_matches(' ');
                if tail_word.chars().any(|c| !c.is_ascii_alphanumeric() && c != '\'' && c != '-') {
                    break;
                }
                self.batch.clear();
                if self.batch.add(cur_tok, step_pos as i32, &[0], true).is_err() {
                    break;
                }
                if self.ctx.decode(&mut self.batch).is_err() {
                    break;
                }
                step_pos += 1;
                let next_logits = self.ctx.get_logits_ith(0);
                let (next_id, next_lp) = argmax_token_with_logprob(next_logits);
                let next_tok = LlamaToken::new(next_id);
                if vocab.is_eog(next_tok) || vocab.is_control(next_tok) {
                    break;
                }
                let next_piece = String::from_utf8_lossy(&vocab.token_to_piece(next_tok, false, None)).into_owned();
                if next_piece.is_empty()
                    || next_piece.starts_with(' ')
                    || next_piece.contains('\n')
                    || !next_piece
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '\'' || c == '-')
                {
                    break;
                }
                piece.push_str(&next_piece);
                total_lp += next_lp * 0.5;
                cur_tok = next_tok;
            }

            if step_pos > base_len {
                match self
                    .ctx
                    .clear_kv_cache_seq(Some(0), Some(base_len as u32), None)
                {
                    Ok(true) => {}
                    _ => {
                        self.reset_kv_state();
                        break;
                    }
                }
            }

            let Some(clean_word) = extract_clean_continuation_word(&piece, plan.expect_leading_space)
            else {
                continue;
            };
            let full_text = format!("{}{}", plan.join_base, clean_word);
            if results
                .iter()
                .any(|c| c.text.eq_ignore_ascii_case(&full_text))
            {
                continue;
            }
            // 将续写分数控制在与 L2 一致的可融合温和区间
            let bounded_score = (-0.8 + (total_lp * 0.15)).clamp(-4.0, -0.5);
            results.push(NeuralCandidate {
                text: full_text,
                score: bounded_score,
                is_continuation: true,
            });
        }

        // 确保 KV Cache 对齐基座
        let _ = self
            .ctx
            .clear_kv_cache_seq(Some(0), Some(base_len as u32), None);
        results
    }
}

fn log_softmax_into(logits: &[f32], out: &mut Vec<f32>) {
    out.resize(logits.len(), f32::NEG_INFINITY);
    if logits.is_empty() {
        return;
    }
    let max_val = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut sum_exp = 0.0f64;
    for &x in logits {
        sum_exp += f64::from(x - max_val).exp();
    }
    let log_z = max_val + (sum_exp.ln() as f32);
    for (dst, &src) in out.iter_mut().zip(logits.iter()) {
        *dst = src - log_z;
    }
}

fn single_token_logprob(logits: &[f32], tok_id: i32) -> Option<f32> {
    let idx = usize::try_from(tok_id).ok()?;
    let target = *logits.get(idx)?;
    if !target.is_finite() {
        return None;
    }
    let max_val = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut sum_exp = 0.0f64;
    for &x in logits {
        sum_exp += f64::from(x - max_val).exp();
    }
    let log_z = max_val + (sum_exp.ln() as f32);
    Some(target - log_z)
}

fn collect_top_token_logprobs(logprobs: &[f32], limit: usize) -> Vec<(i32, f32)> {
    let mut top: Vec<(i32, f32)> = Vec::with_capacity(limit + 1);
    for (idx, &lp) in logprobs.iter().enumerate() {
        if !lp.is_finite() {
            continue;
        }
        if top.len() < limit || lp > top[top.len() - 1].1 {
            top.push((idx as i32, lp));
            top.sort_by(|a, b| b.1.total_cmp(&a.1));
            if top.len() > limit {
                top.pop();
            }
        }
    }
    top
}

fn argmax_token_with_logprob(logits: &[f32]) -> (i32, f32) {
    let mut best_id = 0i32;
    let mut best_logit = f32::NEG_INFINITY;
    for (idx, &v) in logits.iter().enumerate() {
        if v > best_logit {
            best_logit = v;
            best_id = idx as i32;
        }
    }
    let lp = single_token_logprob(logits, best_id).unwrap_or(-5.0);
    (best_id, lp)
}
