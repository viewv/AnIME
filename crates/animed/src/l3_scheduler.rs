//! Per-session 异步 L3 单槽最新优先调度器（Latest-Wins Async L3 Worker）。
//!
//! 当键入较快产生新 `seq` 时，自动覆盖未执行的旧推理任务，并在推理返回前后校验 `latest_seq`，
//! 严格遵守 `ipc-protocol.md` §2 时序抗过期不变量。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use anime_lm_neural::{NeuralCandidate, NeuralSentenceModel};

#[derive(Debug, Clone)]
pub struct L3Job {
    pub seq: u64,
    pub context_before: String,
    pub preedit: String,
    pub candidate_texts: Vec<String>,
    pub max_continuations: usize,
}

#[derive(Default)]
struct SlotState {
    job: Option<L3Job>,
    stop: bool,
}

pub struct L3Scheduler {
    latest_seq: Arc<AtomicU64>,
    slot: Arc<(Mutex<SlotState>, Condvar)>,
    worker: Option<JoinHandle<()>>,
}

impl L3Scheduler {
    pub fn new<N, F>(neural: N, latest_seq: Arc<AtomicU64>, on_complete: F) -> Self
    where
        N: NeuralSentenceModel + 'static,
        F: Fn(u64, Vec<NeuralCandidate>, Vec<NeuralCandidate>) + Send + Sync + 'static,
    {
        let slot = Arc::new((Mutex::new(SlotState::default()), Condvar::new()));
        let worker_slot = Arc::clone(&slot);
        let worker_seq = Arc::clone(&latest_seq);

        let worker = thread::Builder::new()
            .name("anime-l3-worker".to_string())
            .spawn(move || loop {
                let job = {
                    let (lock, cv) = &*worker_slot;
                    let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                    while guard.job.is_none() && !guard.stop {
                        guard = cv.wait(guard).unwrap_or_else(|e| e.into_inner());
                    }
                    if guard.stop {
                        break;
                    }
                    guard.job.take()
                };

                let Some(job) = job else {
                    continue;
                };
                if worker_seq.load(Ordering::Acquire) != job.seq {
                    continue;
                }

                let reranked = neural.rerank_sentences(
                    &job.context_before,
                    &job.preedit,
                    &job.candidate_texts,
                );
                if worker_seq.load(Ordering::Acquire) != job.seq {
                    continue;
                }

                let continuations = neural.predict_continuations(
                    &job.context_before,
                    &job.preedit,
                    job.max_continuations,
                );
                if worker_seq.load(Ordering::Acquire) != job.seq {
                    continue;
                }

                on_complete(job.seq, reranked, continuations);
            })
            .ok();

        Self {
            latest_seq,
            slot,
            worker,
        }
    }

    pub fn update_latest_seq(&self, seq: u64) {
        self.latest_seq.store(seq, Ordering::Release);
    }

    pub fn submit(&self, job: L3Job) {
        if self.latest_seq.load(Ordering::Acquire) != job.seq {
            return;
        }
        let (lock, cv) = &*self.slot;
        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        if !guard.stop {
            guard.job = Some(job);
            cv.notify_one();
        }
    }
}

impl Drop for L3Scheduler {
    fn drop(&mut self) {
        {
            let (lock, cv) = &*self.slot;
            let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            guard.stop = true;
            guard.job = None;
            cv.notify_all();
        }
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}
