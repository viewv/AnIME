//! `animed` IPC 会话调度器、--stdio 流处理器与 Unix Domain Socket 并发服务器。

use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anime_engine::{EngineOutput, EngineSession};
use anime_lexicon::{
    ConfusableCandidate, LayeredLexicon, LexiconCandidate, LexiconProvider, QueryOptions,
};
use anime_lm_neural::{NeuralCandidate, NeuralSentenceModel};
use anime_lm_ngram::NgramLanguageModel;
use anime_protocol::{
    IpcRequest, IpcResponse, MAX_IPC_LINE_BYTES, PROTOCOL_VERSION, decode_request_line,
    encode_response_line, extract_seq_best_effort,
};

use crate::l3_scheduler::{L3Job, L3Scheduler};

#[derive(Clone)]
pub struct SharedLexicon(pub Arc<LayeredLexicon>);

impl LexiconProvider for SharedLexicon {
    fn query(&self, raw: &str, opts: &QueryOptions) -> Vec<LexiconCandidate> {
        self.0.query(raw, opts)
    }

    fn contextual_confusables(&self, raw: &str, limit: usize) -> Vec<ConfusableCandidate> {
        self.0.contextual_confusables(raw, limit)
    }

    fn segment_runon(&self, raw: &str) -> Option<Vec<String>> {
        self.0.segment_runon(raw)
    }

    fn closed_token_neighbors(&self, raw: &str, limit: usize) -> Vec<LexiconCandidate> {
        self.0.closed_token_neighbors(raw, limit)
    }
}

#[derive(Clone)]
pub struct SharedNeural(pub Arc<dyn NeuralSentenceModel>);

impl NeuralSentenceModel for SharedNeural {
    fn backend_name(&self) -> &'static str {
        self.0.backend_name()
    }

    fn rerank_sentences(
        &self,
        context_before: &str,
        preedit: &str,
        candidates: &[String],
    ) -> Vec<NeuralCandidate> {
        self.0.rerank_sentences(context_before, preedit, candidates)
    }

    fn predict_continuations(
        &self,
        context_before: &str,
        prefix: &str,
        limit: usize,
    ) -> Vec<NeuralCandidate> {
        self.0.predict_continuations(context_before, prefix, limit)
    }
}

pub struct SharedResources {
    pub lexicon: Arc<LayeredLexicon>,
    pub ngram: Arc<NgramLanguageModel>,
    pub neural: Arc<dyn NeuralSentenceModel>,
    pub version: String,
    pub transport_cap: &'static str,
    pub l3_debounce_ms: u64,
}

impl SharedResources {
    pub fn capabilities(&self) -> Vec<String> {
        let mut caps = vec![
            "l1-fuzzy".to_string(),
            "l1-layered".to_string(),
            if self.ngram.has_static_lm() {
                "l2-ngram".to_string()
            } else {
                "l2-viterbi".to_string()
            },
            if self.ngram.user_store().is_sqlite_backed() {
                "l2-userdb".to_string()
            } else {
                "l2-memory".to_string()
            },
        ];
        if self.neural.backend_name() != "noop" {
            caps.push("l3-neural".to_string());
        }
        caps.push(self.transport_cap.to_string());
        caps
    }
}

type DaemonSession = EngineSession<SharedLexicon, Arc<NgramLanguageModel>, SharedNeural>;

fn write_response_line<W: Write + ?Sized>(writer: &Mutex<W>, resp: &IpcResponse) -> io::Result<()> {
    let encoded = match encode_response_line(resp) {
        Ok(line) => line,
        Err(err) => {
            let fallback = IpcResponse::Error {
                seq: match resp {
                    IpcResponse::State(s) => Some(s.seq),
                    IpcResponse::Commit(c) => Some(c.seq),
                    IpcResponse::Error { seq, .. } => *seq,
                    _ => None,
                },
                code: err.code().to_string(),
                message: err.to_string(),
            };
            encode_response_line(&fallback)
                .unwrap_or_else(|_| r#"{"type":"error","code":"encode_error","message":"failed to encode IPC response"}"#.to_string())
        }
    };

    let mut guard = writer.lock().unwrap_or_else(|e| e.into_inner());
    writeln!(guard, "{encoded}")?;
    guard.flush()
}

fn teach_lexicon_commit(lexicon: &LayeredLexicon, text: &str) {
    for raw in text.split(|c: char| !c.is_ascii_alphanumeric() && c != '\'' && c != '-') {
        let tok = raw.trim_matches(|c| c == '\'' || c == '-');
        if tok.is_empty() || !tok.bytes().any(|b| b.is_ascii_alphabetic()) {
            continue;
        }
        let _ = lexicon.learn_word(tok, 1);
    }
}

enum BoundedLine {
    Eof,
    Empty,
    Line(String),
    TooLarge(usize),
    InvalidUtf8,
}

/// 带 64 KiB 截断防护的 NDJSON 行读取器：避免恶意超长行触发无上限内存增长。
fn read_bounded_line<R: BufRead>(reader: &mut R) -> io::Result<BoundedLine> {
    let mut buf = Vec::with_capacity(256);
    let mut total_bytes = 0usize;
    let mut too_large = false;
    let mut saw_any_byte = false;

    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            break;
        }
        saw_any_byte = true;
        if let Some(nl_idx) = chunk.iter().position(|&b| b == b'\n') {
            total_bytes = total_bytes.saturating_add(nl_idx);
            if !too_large && buf.len() + nl_idx <= MAX_IPC_LINE_BYTES + 1 {
                buf.extend_from_slice(&chunk[..nl_idx]);
            } else if buf.len() + nl_idx > MAX_IPC_LINE_BYTES + 1 {
                too_large = true;
            }
            reader.consume(nl_idx + 1);
            break;
        }

        let len = chunk.len();
        total_bytes = total_bytes.saturating_add(len);
        if !too_large && buf.len() + len <= MAX_IPC_LINE_BYTES + 1 {
            buf.extend_from_slice(chunk);
        } else {
            too_large = true;
        }
        reader.consume(len);
    }

    if !saw_any_byte {
        return Ok(BoundedLine::Eof);
    }
    if too_large {
        return Ok(BoundedLine::TooLarge(total_bytes));
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    if buf.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(BoundedLine::Empty);
    }
    match String::from_utf8(buf) {
        Ok(s) => Ok(BoundedLine::Line(s)),
        Err(_) => Ok(BoundedLine::InvalidUtf8),
    }
}

pub enum StreamExit {
    Eof,
    ShutdownRequested,
}

pub fn serve_ndjson_stream<R, W>(
    reader: R,
    writer: W,
    resources: Arc<SharedResources>,
    stop_flag: Arc<AtomicBool>,
) -> io::Result<StreamExit>
where
    R: io::Read,
    W: io::Write + Send + 'static,
{
    let session = Arc::new(Mutex::new(DaemonSession::new(
        SharedLexicon(Arc::clone(&resources.lexicon)),
        Arc::clone(&resources.ngram),
        SharedNeural(Arc::clone(&resources.neural)),
    )));
    let writer: Arc<Mutex<W>> = Arc::new(Mutex::new(writer));
    let latest_seq = Arc::new(AtomicU64::new(0));

    let cb_session = Arc::clone(&session);
    let cb_writer = Arc::clone(&writer);
    let scheduler = L3Scheduler::with_debounce(
        SharedNeural(Arc::clone(&resources.neural)),
        Arc::clone(&latest_seq),
        Duration::from_millis(resources.l3_debounce_ms),
        move |seq, reranked, continuations| {
            let maybe_state = {
                let mut guard = cb_session.lock().unwrap_or_else(|e| e.into_inner());
                guard.apply_neural_rerank(seq, &reranked, &continuations)
            };
            if let Some(st) = maybe_state {
                let _ = write_response_line(&cb_writer, &IpcResponse::State(st));
            }
        },
    );

    let mut buf_reader = BufReader::new(reader);
    let mut exit_reason = StreamExit::Eof;

    loop {
        match read_bounded_line(&mut buf_reader)? {
            BoundedLine::Eof => break,
            BoundedLine::Empty => continue,
            BoundedLine::TooLarge(len) => {
                let err = IpcResponse::Error {
                    seq: None,
                    code: "line_too_large".to_string(),
                    message: format!("IPC frame length {len} exceeds {MAX_IPC_LINE_BYTES} bytes"),
                };
                write_response_line(&writer, &err)?;
            }
            BoundedLine::InvalidUtf8 => {
                let err = IpcResponse::Error {
                    seq: None,
                    code: "invalid_utf8".to_string(),
                    message: "IPC line is not valid UTF-8".to_string(),
                };
                write_response_line(&writer, &err)?;
            }
            BoundedLine::Line(line) => {
                let req = match decode_request_line(&line) {
                    Ok(r) => r,
                    Err(e) => {
                        let err = IpcResponse::Error {
                            seq: extract_seq_best_effort(&line),
                            code: e.code().to_string(),
                            message: e.to_string(),
                        };
                        write_response_line(&writer, &err)?;
                        continue;
                    }
                };

                match req {
                    IpcRequest::Hello {
                        protocol_version, ..
                    } => {
                        if protocol_version != PROTOCOL_VERSION {
                            write_response_line(
                                &writer,
                                &IpcResponse::Error {
                                    seq: None,
                                    code: "unsupported_protocol_version".to_string(),
                                    message: format!(
                                        "unsupported protocol_version {protocol_version} (expected {PROTOCOL_VERSION})"
                                    ),
                                },
                            )?;
                        } else {
                            write_response_line(
                                &writer,
                                &IpcResponse::HelloAck {
                                    protocol_version: PROTOCOL_VERSION,
                                    version: resources.version.clone(),
                                    capabilities: resources.capabilities(),
                                },
                            )?;
                        }
                    }
                    IpcRequest::Ping { id } => {
                        write_response_line(
                            &writer,
                            &IpcResponse::Pong {
                                version: resources.version.clone(),
                                id,
                            },
                        )?;
                    }
                    IpcRequest::Context { before, after } => {
                        let mut guard = session.lock().unwrap_or_else(|e| e.into_inner());
                        guard.set_context(before, after);
                    }
                    IpcRequest::Privacy { enabled } => {
                        let mut guard = session.lock().unwrap_or_else(|e| e.into_inner());
                        guard.set_privacy(enabled);
                    }
                    IpcRequest::Config(cfg) => {
                        let mut guard = session.lock().unwrap_or_else(|e| e.into_inner());
                        guard.set_config(cfg);
                    }
                    IpcRequest::Reset { seq } => {
                        scheduler.update_latest_seq(seq);
                        let st = {
                            let mut guard = session.lock().unwrap_or_else(|e| e.into_inner());
                            guard.reset_with_seq(seq)
                        };
                        write_response_line(&writer, &IpcResponse::State(st))?;
                    }
                    IpcRequest::Key { seq, key, .. } => {
                        scheduler.update_latest_seq(seq);
                        let (resp, l3_job) = {
                            let mut guard = session.lock().unwrap_or_else(|e| e.into_inner());
                            let privacy = guard.is_privacy();
                            match guard.handle_key_with_seq(seq, key) {
                                EngineOutput::State(st) => {
                                    let job = if !st.final_l3 {
                                        let total_chars = st.preedit.chars().count();
                                        Some(L3Job {
                                            seq: st.seq,
                                            context_before: guard.context().before.clone(),
                                            preedit: st.preedit.clone(),
                                            candidate_texts: st
                                                .candidates
                                                .iter()
                                                .filter(|c| {
                                                    c.consumed_chars == total_chars
                                                        && !matches!(
                                                            c.kind,
                                                            anime_protocol::CandidateKind::Raw
                                                                | anime_protocol::CandidateKind::Continuation
                                                        )
                                                })
                                                .map(|c| c.text.clone())
                                                .collect(),
                                            max_continuations: guard
                                                .config()
                                                .max_candidates
                                                .min(3),
                                        })
                                    } else {
                                        None
                                    };
                                    (IpcResponse::State(st), job)
                                }
                                EngineOutput::Commit(cm) => {
                                    let learned = guard.take_last_learned_text();
                                    if !privacy && cm.delete_before == 0 {
                                        teach_lexicon_commit(&resources.lexicon, learned.as_deref().unwrap_or(&cm.text));
                                    }
                                    (IpcResponse::Commit(cm), None)
                                }
                            }
                        };
                        // 先确保同步 revision: 0 响应刷新到连接后再调度后台 L3，保证 revision 严格有序
                        write_response_line(&writer, &resp)?;
                        if let Some(job) = l3_job {
                            scheduler.submit(job);
                        }
                    }
                    IpcRequest::Select { seq, index } => {
                        scheduler.update_latest_seq(seq);
                        let resp = {
                            let mut guard = session.lock().unwrap_or_else(|e| e.into_inner());
                            let privacy = guard.is_privacy();
                            if guard.preedit().is_empty() || index >= guard.candidates().len() {
                                IpcResponse::Error {
                                    seq: Some(seq),
                                    code: "invalid_select_index".to_string(),
                                    message: format!(
                                        "cannot select candidate index {index} in current preedit state"
                                    ),
                                }
                            } else if let Some(cm) = guard.select_with_seq(seq, index) {
                                let learned = guard.take_last_learned_text();
                                if !privacy && cm.delete_before == 0 {
                                    teach_lexicon_commit(&resources.lexicon, learned.as_deref().unwrap_or(&cm.text));
                                }
                                IpcResponse::Commit(cm)
                            } else {
                                IpcResponse::Error {
                                    seq: Some(seq),
                                    code: "select_failed".to_string(),
                                    message: "candidate selection failed".to_string(),
                                }
                            }
                        };
                        write_response_line(&writer, &resp)?;
                    }
                    IpcRequest::Shutdown => {
                        resources.ngram.flush_user_db();
                        stop_flag.store(true, Ordering::Release);
                        exit_reason = StreamExit::ShutdownRequested;
                        break;
                    }
                }
            }
        }
    }

    drop(scheduler);
    resources.ngram.flush_user_db();
    Ok(exit_reason)
}

#[cfg(unix)]
pub fn run_unix_server(
    socket_path: &Path,
    resources: Arc<SharedResources>,
    stop_flag: Arc<AtomicBool>,
) -> io::Result<()> {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};

    if let Some(parent) = socket_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
    }

    if socket_path.exists() {
        match UnixStream::connect(socket_path) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("active daemon already listening on '{}'", socket_path.display()),
                ));
            }
            Err(_) => {
                // 连接被拒绝说明为残留坏死 socket 文件，安全清理后再重建
                fs::remove_file(socket_path)?;
            }
        }
    }

    let listener = UnixListener::bind(socket_path)?;
    fs::set_permissions(socket_path, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;

    let mut clients: Vec<JoinHandle<()>> = Vec::new();
    while !stop_flag.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _addr)) => {
                let _ = stream.set_nonblocking(false);
                let writer_stream = stream.try_clone()?;
                let res = Arc::clone(&resources);
                let stop = Arc::clone(&stop_flag);
                if let Ok(handle) = thread::Builder::new()
                    .name("anime-ipc-client".to_string())
                    .spawn(move || {
                        let _ = serve_ndjson_stream(stream, writer_stream, res, stop);
                    })
                {
                    clients.push(handle);
                }
                // 定期回收已退出的客户端线程句柄
                let mut i = 0;
                while i < clients.len() {
                    if clients[i].is_finished() {
                        let h = clients.swap_remove(i);
                        let _ = h.join();
                    } else {
                        i += 1;
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                let _ = fs::remove_file(socket_path);
                return Err(e);
            }
        }
    }

    let _ = fs::remove_file(socket_path);
    for h in clients {
        let _ = h.join();
    }
    resources.ngram.flush_user_db();
    Ok(())
}
