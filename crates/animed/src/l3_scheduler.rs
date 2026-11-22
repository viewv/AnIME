//! Per-session 异步 L3 单槽最新优先调度器（Latest-Wins Async L3 Worker）。
//!
//! 当键入较快产生新 `seq` 时，自动覆盖未执行的旧推理任务，并在推理返回前后校验 `latest_seq`，
//! 严格遵守 `ipc-protocol.md` §2 时序抗过期不变量。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

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
    #[allow(dead_code)]
    pub fn new<N, F>(neural: N, latest_seq: Arc<AtomicU64>, on_complete: F) -> Self
    where
        N: NeuralSentenceModel + 'static,
        F: Fn(u64, Vec<NeuralCandidate>, Vec<NeuralCandidate>) + Send + Sync + 'static,
    {
        Self::with_debounce(neural, latest_seq, Duration::ZERO, on_complete)
    }

    pub fn with_debounce<N, F>(
        neural: N,
        latest_seq: Arc<AtomicU64>,
        debounce: Duration,
        on_complete: F,
    ) -> Self
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
                let mut job = {
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

                // 轻去抖以合并连续高速键入，但句末空格直接绕过去抖以第一时间触发续写预测
                while !debounce.is_zero() {
                    let Some(ref cur) = job else {
                        break;
                    };
                    if cur.preedit.ends_with(' ') {
                        break;
                    }
                    let deadline = Instant::now() + debounce;
                    let (lock, cv) = &*worker_slot;
                    let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                    let mut superseded = false;
                    while !guard.stop && guard.job.is_none() {
                        let now = Instant::now();
                        if now >= deadline {
                            break;
                        }
                        let (next_guard, wait_res) = cv
                            .wait_timeout(guard, deadline - now)
                            .unwrap_or_else(|e| e.into_inner());
                        guard = next_guard;
                        if guard.stop || guard.job.is_some() || wait_res.timed_out() {
                            break;
                        }
                    }
                    if guard.stop {
                        return;
                    }
                    if let Some(newer) = guard.job.take() {
                        job = Some(newer);
                        superseded = true;
                    }
                    if !superseded {
                        break;
                    }
                }

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct CountingNeural(Arc<AtomicUsize>);
    impl NeuralSentenceModel for CountingNeural {
        fn backend_name(&self) -> &'static str {
            "counting-mock"
        }
        fn rerank_sentences(&self, _: &str, _: &str, _: &[String]) -> Vec<NeuralCandidate> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Vec::new()
        }
        fn predict_continuations(&self, _: &str, _: &str, _: usize) -> Vec<NeuralCandidate> {
            Vec::new()
        }
    }

    #[test]
    fn debounce_coalesces_burst_and_space_bypasses() {
        let calls = Arc::new(AtomicUsize::new(0));
        let latest = Arc::new(AtomicU64::new(0));
        let completed = Arc::new(Mutex::new(Vec::<u64>::new()));
        let completed_cb = Arc::clone(&completed);

        let sched = L3Scheduler::with_debounce(
            CountingNeural(Arc::clone(&calls)),
            Arc::clone(&latest),
            Duration::from_millis(60),
            move |seq, _, _| {
                completed_cb.lock().unwrap().push(seq);
            },
        );

        for seq in 1..=4 {
            sched.update_latest_seq(seq);
            sched.submit(L3Job {
                seq,
                context_before: String::new(),
                preedit: "tes".to_string(),
                candidate_texts: vec!["test".to_string()],
                max_continuations: 1,
            });
            thread::sleep(Duration::from_millis(5));
        }

        thread::sleep(Duration::from_millis(140));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(*completed.lock().unwrap(), vec![4]);

        // 词尾带空格立即触发、不再等待 debounce
        sched.update_latest_seq(5);
        let t0 = Instant::now();
        sched.submit(L3Job {
            seq: 5,
            context_before: String::new(),
            preedit: "for ".to_string(),
            candidate_texts: vec!["for".to_string()],
            max_continuations: 1,
        });
        while completed.lock().unwrap().len() < 2 && t0.elapsed() < Duration::from_millis(50) {
            thread::sleep(Duration::from_millis(2));
        }
        assert!(t0.elapsed() < Duration::from_millis(50));
        assert_eq!(*completed.lock().unwrap(), vec![4, 5]);
    }
}
