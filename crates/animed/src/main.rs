//! AnIME 守护进程入口：提供 Unix Domain Socket 与 `--stdio` NDJSON 服务、
//! OS 路径发现（`ANIME_DATA_DIR`/`XDG_*`）、三层词典/L2 n-gram/`user.db` 加载与异步 L3 调度。

mod l3_scheduler;
mod paths;
mod server;

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anime_lexicon::{LayeredLexicon, Lexicon, LexiconError};
use anime_lm_neural::{NeuralModelConfig, NeuralSentenceModel, NoopNeuralModel};
use anime_lm_ngram::{NgramLanguageModel, UserStore};
use anime_protocol::PROTOCOL_VERSION;

use crate::paths::{RuntimePaths, discover_runtime_paths_with_model};
use crate::server::{SharedResources, StreamExit, serve_ndjson_stream};

const VERSION: &str = env!("CARGO_PKG_VERSION");

enum CliAction {
    Version,
    Help,
    Stdio,
    Daemon,
}

struct CliArgs {
    action: CliAction,
    socket_override: Option<PathBuf>,
    data_dir_override: Option<PathBuf>,
    model_override: Option<PathBuf>,
    gpu_layers: u32,
    l3_debounce_ms: u64,
    disable_neural: bool,
}

fn parse_cli_args<I>(args: I) -> Result<CliArgs, String>
where
    I: IntoIterator<Item = String>,
{
    let mut action = CliAction::Daemon;
    let mut socket_override = None;
    let mut data_dir_override = None;
    let mut model_override = None;
    let mut gpu_layers = std::env::var("ANIME_GPU_LAYERS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(999);
    let mut l3_debounce_ms = std::env::var("ANIME_L3_DEBOUNCE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(20);
    let mut disable_neural = false;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--version" | "-V" => action = CliAction::Version,
            "--help" | "-h" => action = CliAction::Help,
            "--stdio" => action = CliAction::Stdio,
            "--no-neural" => disable_neural = true,
            "--socket" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--socket requires a path argument".to_string())?;
                socket_override = Some(PathBuf::from(val));
            }
            "--data-dir" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--data-dir requires a directory argument".to_string())?;
                data_dir_override = Some(PathBuf::from(val));
            }
            "--model" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--model requires a GGUF path argument".to_string())?;
                model_override = Some(PathBuf::from(val));
            }
            "--gpu-layers" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--gpu-layers requires a numeric argument".to_string())?;
                gpu_layers = val
                    .parse::<u32>()
                    .map_err(|_| format!("invalid --gpu-layers value '{val}'"))?;
            }
            "--l3-debounce-ms" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--l3-debounce-ms requires a numeric argument".to_string())?;
                l3_debounce_ms = val
                    .parse::<u64>()
                    .map_err(|_| format!("invalid --l3-debounce-ms value '{val}'"))?;
            }
            other => return Err(format!("unknown argument '{other}'")),
        }
    }

    Ok(CliArgs {
        action,
        socket_override,
        data_dir_override,
        model_override,
        gpu_layers,
        l3_debounce_ms,
        disable_neural,
    })
}

fn build_fallback_lexicon() -> Lexicon {
    Lexicon::from_word_zipfs(
        vec![
            ("the", 7.73),
            ("to", 7.43),
            ("and", 7.41),
            ("of", 7.40),
            ("a", 7.36),
            ("in", 7.27),
            ("is", 7.07),
            ("for", 7.01),
            ("that", 7.01),
            ("I", 7.50),
            ("an", 6.50),
            ("want", 6.10),
            ("eat", 5.60),
            ("apple", 5.20),
            ("apples", 4.40),
            ("from", 6.80),
            ("form", 5.80),
            ("here", 6.10),
            ("hello", 5.80),
            ("world", 6.00),
            ("beautiful", 5.10),
            ("algorithm", 4.90),
            ("pneumonia", 3.90),
            ("definitely", 5.30),
            ("receive", 5.05),
            ("go", 6.20),
            ("over", 6.30),
            ("there", 6.50),
            ("their", 6.50),
        ],
        Vec::new(),
    )
}

fn load_resources(
    paths: &RuntimePaths,
    transport_cap: &'static str,
    explicit_model: bool,
    gpu_layers: u32,
    l3_debounce_ms: u64,
    disable_neural: bool,
) -> Arc<SharedResources> {
    let base_lex = match &paths.lexicon_path {
        Some(path) => match Lexicon::from_animelex_file(path) {
            Ok(lex) => lex,
            Err(err) => {
                eprintln!(
                    "animed warning: failed loading lexicon '{}': {err}; using built-in fallback",
                    path.display()
                );
                build_fallback_lexicon()
            }
        },
        None => build_fallback_lexicon(),
    };

    let mut layered = LayeredLexicon::new(base_lex);
    match layered.load_extension_dir(&paths.extension_dir, 0.0) {
        Ok(_) => {}
        Err(LexiconError::Io(msg)) => {
            eprintln!(
                "animed warning: failed scanning extension dir '{}': {msg}",
                paths.extension_dir.display()
            );
        }
        Err(err) => {
            eprintln!(
                "animed warning: failed loading extension dir '{}': {err}",
                paths.extension_dir.display()
            );
        }
    }

    let user_store = match UserStore::open_sqlite(&paths.user_db_path) {
        Ok(store) => store,
        Err(err) => {
            eprintln!(
                "animed warning: failed opening SQLite user.db at '{}': {err}; falling back to in-memory learning",
                paths.user_db_path.display()
            );
            UserStore::in_memory()
        }
    };

    let ngram = match &paths.ngram_path {
        Some(path) => match anime_lm_ngram::load_animengr_file(path) {
            Ok(data) => NgramLanguageModel::from_data(data, user_store),
            Err(err) => {
                eprintln!(
                    "animed warning: failed loading n-gram '{}': {err}; running without static n-gram",
                    path.display()
                );
                NgramLanguageModel::with_user_store(user_store)
            }
        },
        None => NgramLanguageModel::with_user_store(user_store),
    };

    // 启动期把已持久化至 user.db 的热词同步到 L1 Layer 3，实现跨会话联想与纠错闭合
    let preloaded = layered.preload_dynamic_words(ngram.snapshot_user_words());
    if preloaded > 0 {
        eprintln!("animed: preloaded {preloaded} learned words into L1 dynamic layer");
    }

    let neural: Arc<dyn NeuralSentenceModel> = if disable_neural {
        Arc::new(NoopNeuralModel)
    } else if let Some(ref model_path) = paths.neural_model_path {
        if !cfg!(feature = "llama") && !explicit_model {
            // 默认无 `llama` 特性编译时静默使用 Noop，以免未传 `--model` 的本地 smoke 测试变慢或报冗余警告
            Arc::new(NoopNeuralModel)
        } else {
            let mut ncfg = NeuralModelConfig::with_model_path(model_path);
            ncfg.n_gpu_layers = gpu_layers;
            match anime_lm_neural::load_neural_model(ncfg) {
                Ok((model, info)) => {
                    eprintln!(
                        "animed: loaded L3 neural model '{}' (backend={}, gpu_offload={}, device={}, warmup={}ms)",
                        info.model_name,
                        info.backend_name,
                        info.gpu_offload_active,
                        info.device_summary,
                        info.warmup_ms
                    );
                    model
                }
                Err(err) => {
                    eprintln!(
                        "animed warning: failed loading L3 neural model '{}': {err}; falling back to NoopNeuralModel",
                        model_path.display()
                    );
                    Arc::new(NoopNeuralModel)
                }
            }
        }
    } else {
        Arc::new(NoopNeuralModel)
    };
    Arc::new(SharedResources {
        lexicon: Arc::new(layered),
        ngram: Arc::new(ngram),
        neural,
        version: VERSION.to_string(),
        transport_cap,
        l3_debounce_ms,
    })
}

fn run_stdio(resources: Arc<SharedResources>) -> io::Result<()> {
    let stdin = io::stdin().lock();
    let stdout = io::stdout();
    let stop_flag = Arc::new(AtomicBool::new(false));
    let _exit: StreamExit = serve_ndjson_stream(stdin, stdout, resources, stop_flag)?;
    Ok(())
}

fn print_help() {
    println!(
        "animed {VERSION} (protocol v{PROTOCOL_VERSION})\n\
         Usage:\n\
         \tanimed [--stdio] [--socket <path>] [--data-dir <dir>] [--model <gguf>] [--gpu-layers <n>] [--l3-debounce-ms <ms>] [--no-neural]\n\
         \tanimed --version | --help"
    );
}

fn main() {
    let cli = match parse_cli_args(std::env::args().skip(1)) {
        Ok(parsed) => parsed,
        Err(msg) => {
            eprintln!("animed argument error: {msg}");
            std::process::exit(2);
        }
    };

    let explicit_model = cli.model_override.is_some()
        || std::env::var_os("ANIME_NEURAL_MODEL").is_some_and(|v| !v.is_empty());

    match cli.action {
        CliAction::Version => {
            println!("animed {VERSION} (protocol v{PROTOCOL_VERSION})");
        }
        CliAction::Help => {
            print_help();
        }
        CliAction::Stdio => {
            let paths = discover_runtime_paths_with_model(
                cli.socket_override.as_deref(),
                cli.data_dir_override.as_deref(),
                cli.model_override.as_deref(),
            );
            let resources = load_resources(
                &paths,
                "stdio",
                explicit_model,
                cli.gpu_layers,
                cli.l3_debounce_ms,
                cli.disable_neural,
            );
            if let Err(err) = run_stdio(resources) {
                eprintln!("animed stdio error: {err}");
                std::process::exit(1);
            }
        }
        CliAction::Daemon => {
            let paths = discover_runtime_paths_with_model(
                cli.socket_override.as_deref(),
                cli.data_dir_override.as_deref(),
                cli.model_override.as_deref(),
            );
            #[cfg(unix)]
            {
                let resources = load_resources(
                    &paths,
                    "unix-socket",
                    explicit_model,
                    cli.gpu_layers,
                    cli.l3_debounce_ms,
                    cli.disable_neural,
                );
                let stop_flag = Arc::new(AtomicBool::new(false));
                eprintln!(
                    "animed {VERSION} listening on '{}' (data_dir='{}')",
                    paths.socket_path.display(),
                    paths.data_dir.display()
                );
                if let Err(err) = server::run_unix_server(&paths.socket_path, resources, stop_flag) {
                    eprintln!("animed unix socket error: {err}");
                    std::process::exit(1);
                }
            }
            #[cfg(not(unix))]
            {
                let _ = paths;
                eprintln!("Unix domain socket daemon is only supported on Unix targets; use --stdio");
                std::process::exit(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Cursor;
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use anime_lm_neural::NeuralCandidate;
    use anime_protocol::{CandidateKind, IpcResponse, MAX_IPC_LINE_BYTES, decode_response_line};

    struct TempDirGuard(PathBuf);
    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn make_temp_dir(label: &str) -> (TempDirGuard, PathBuf) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "anime_animed_test_{label}_{}_{}",
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        (TempDirGuard(dir.clone()), dir)
    }

    fn make_test_resources(transport_cap: &'static str, user_store: UserStore) -> Arc<SharedResources> {
        let lex = LayeredLexicon::new(build_fallback_lexicon());
        let ngram = NgramLanguageModel::with_user_store(user_store);
        Arc::new(SharedResources {
            lexicon: Arc::new(lex),
            ngram: Arc::new(ngram),
            neural: Arc::new(NoopNeuralModel),
            version: VERSION.to_string(),
            transport_cap,
            l3_debounce_ms: 0,
        })
    }

    #[test]
    fn stdio_protocol_dispatches_all_commands_and_recovers_from_errors() {
        let resources = make_test_resources("stdio", UserStore::in_memory());
        let huge_line = format!(r#"{{"type":"ping","pad":"{}"}}"#, "x".repeat(MAX_IPC_LINE_BYTES + 10));
        let input = [
            "".to_string(),
            r#"{"type":"ping"}"#.to_string(),
            "".to_string(),
            r#"{"type":"hello","client":"test","protocol_version":99}"#.to_string(),
            r#"{"type":"hello","client":"test","protocol_version":1}"#.to_string(),
            r#"{"type":"context","before":"Hello. ","after":""}"#.to_string(),
            r#"{"type":"key","seq":10,"key":"char","text":"t"}"#.to_string(),
            r#"{"type":"key","seq":11,"key":"char","text":"o"}"#.to_string(),
            r#"{"type":"select","seq":12,"index":0}"#.to_string(),
            r#"{"type":"reset","seq":13}"#.to_string(),
            r#"{"type":"privacy","enabled":true}"#.to_string(),
            r#"{"type":"config","space_commits":false,"smart_spacing":true,"max_candidates":5,"enable_phonetic":true,"enable_neural":false}"#.to_string(),
            r#"{"type":"key","seq":14,"bad_json"#.to_string(),
            huge_line,
            r#"{"type":"ping","id":77}"#.to_string(),
            r#"{"type":"shutdown"}"#.to_string(),
        ]
        .join("\n")
            + "\n";

        let out_buf: Arc<std::sync::Mutex<Vec<u8>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        struct SharedVecWriter(Arc<std::sync::Mutex<Vec<u8>>>);
        impl io::Write for SharedVecWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let stop = Arc::new(AtomicBool::new(false));
        let exit = serve_ndjson_stream(
            Cursor::new(input.as_bytes()),
            SharedVecWriter(Arc::clone(&out_buf)),
            resources,
            stop.clone(),
        )
        .expect("stdio session");
        assert!(matches!(exit, StreamExit::ShutdownRequested));
        assert!(stop.load(std::sync::atomic::Ordering::Acquire));

        let raw_out = String::from_utf8(out_buf.lock().unwrap().clone()).unwrap();
        let lines: Vec<&str> = raw_out.lines().collect();
        assert_eq!(lines[0], r#"{"type":"pong","version":"0.1.0"}"#);

        let responses: Vec<IpcResponse> = lines
            .iter()
            .map(|l| decode_response_line(l).expect("valid response line"))
            .collect();

        assert!(matches!(
            &responses[1],
            IpcResponse::Error { code, .. } if code == "unsupported_protocol_version"
        ));
        assert!(matches!(
            &responses[2],
            IpcResponse::HelloAck { protocol_version: 1, capabilities, .. } if capabilities.contains(&"stdio".to_string())
        ));
        assert!(matches!(
            &responses[4],
            IpcResponse::State(st) if st.seq == 11 && st.preedit == "to" && st.candidates[0].text == "To"
        ));
        assert!(matches!(
            &responses[5],
            IpcResponse::Commit(cm) if cm.seq == 12 && cm.text == "To "
        ));
        assert!(matches!(
            &responses[6],
            IpcResponse::State(st) if st.seq == 13 && st.preedit.is_empty()
        ));
        assert!(matches!(
            &responses[7],
            IpcResponse::Error { seq: Some(14), code, .. } if code == "invalid_json"
        ));
        assert!(matches!(
            &responses[8],
            IpcResponse::Error { code, .. } if code == "line_too_large"
        ));
        assert!(matches!(
            &responses[9],
            IpcResponse::Pong { id: Some(77), .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unix_socket_daemon_permissions_and_closed_loop_userdb_learning() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixStream;

        let (_guard, dir) = make_temp_dir("unix_daemon");
        let sock_dir = dir.join("run");
        let sock_path = sock_dir.join("engined.sock");
        let db_path = dir.join("user.db");
        let dicts_dir = dir.join("dicts");
        fs::create_dir_all(&dicts_dir).unwrap();
        fs::write(dicts_dir.join("tech.tsv"), "wayland\t4.8\n").unwrap();

        let mut layered = LayeredLexicon::new(build_fallback_lexicon());
        assert_eq!(layered.load_extension_dir(&dicts_dir, 0.0).unwrap(), 1);
        let user_store = UserStore::open_sqlite(&db_path).expect("open sqlite user.db");
        let ngram = NgramLanguageModel::with_user_store(user_store);

        let resources = Arc::new(SharedResources {
            lexicon: Arc::new(layered),
            ngram: Arc::new(ngram),
            neural: Arc::new(NoopNeuralModel),
            version: VERSION.to_string(),
            transport_cap: "unix-socket",
            l3_debounce_ms: 0,
        });
        let stop_flag = Arc::new(AtomicBool::new(false));

        let srv_res = Arc::clone(&resources);
        let srv_stop = Arc::clone(&stop_flag);
        let srv_sock = sock_path.clone();
        let server_thread = thread::spawn(move || {
            server::run_unix_server(&srv_sock, srv_res, srv_stop).expect("run unix server");
        });

        let mut stream = None;
        for _ in 0..80 {
            if let Ok(s) = UnixStream::connect(&sock_path) {
                stream = Some(s);
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        let mut client1 = stream.expect("connect client1 to unix socket");

        let dir_mode = fs::metadata(&sock_dir).unwrap().permissions().mode() & 0o777;
        let sock_mode = fs::metadata(&sock_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
        assert_eq!(sock_mode, 0o600);

        let mut reader1 = BufReader::new(client1.try_clone().unwrap());
        let send_recv = |w: &mut UnixStream, r: &mut BufReader<UnixStream>, req: &str| -> IpcResponse {
            writeln!(w, "{req}").unwrap();
            w.flush().unwrap();
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            decode_response_line(&line).unwrap()
        };

        // 验证 Layer 2 扩展词表加载 + 学习新词 Hyprland + privacy 拦截 SecretToken
        assert!(matches!(
            send_recv(&mut client1, &mut reader1, r#"{"type":"hello","client":"c1","protocol_version":1}"#),
            IpcResponse::HelloAck { .. }
        ));
        for (seq, ch) in "wayland".chars().enumerate() {
            let msg = format!(r#"{{"type":"key","seq":{},"key":"char","text":"{ch}"}}"#, seq + 1);
            let resp = send_recv(&mut client1, &mut reader1, &msg);
            if ch == 'd' {
                match resp {
                    IpcResponse::State(st) => assert_eq!(st.candidates[0].text, "wayland"),
                    other => panic!("expected wayland state, got {other:?}"),
                }
            }
        }
        let _ = send_recv(&mut client1, &mut reader1, r#"{"type":"reset","seq":20}"#);
        for (seq, ch) in "Hyprland".chars().enumerate() {
            let msg = format!(r#"{{"type":"key","seq":{},"key":"char","text":"{ch}"}}"#, 30 + seq);
            let _ = send_recv(&mut client1, &mut reader1, &msg);
        }
        let _ = send_recv(&mut client1, &mut reader1, r#"{"type":"key","seq":40,"key":"enter"}"#);

        writeln!(client1, r#"{{"type":"privacy","enabled":true}}"#).unwrap();
        for (seq, ch) in "SecretToken".chars().enumerate() {
            let msg = format!(r#"{{"type":"key","seq":{},"key":"char","text":"{ch}"}}"#, 50 + seq);
            let _ = send_recv(&mut client1, &mut reader1, &msg);
        }
        let _ = send_recv(&mut client1, &mut reader1, r#"{"type":"key","seq":70,"key":"enter"}"#);
        drop(reader1);
        drop(client1);

        // 客户端 2 连入仍在运行的守护进程：拼错 hyprlnd 应从 Layer 3 同步联想出 Hyprland
        let mut client2 = UnixStream::connect(&sock_path).expect("connect client2");
        let mut reader2 = BufReader::new(client2.try_clone().unwrap());
        let mut last_state = None;
        for (seq, ch) in "hyprlnd".chars().enumerate() {
            let msg = format!(r#"{{"type":"key","seq":{},"key":"char","text":"{ch}"}}"#, 100 + seq);
            last_state = Some(send_recv(&mut client2, &mut reader2, &msg));
        }
        match last_state.unwrap() {
            IpcResponse::State(st) => assert_eq!(st.candidates[0].text, "Hyprland"),
            other => panic!("expected learned Hyprland completion/typo, got {other:?}"),
        }

        writeln!(client2, r#"{{"type":"shutdown"}}"#).unwrap();
        client2.flush().unwrap();
        drop(reader2);
        drop(client2);

        server_thread.join().expect("server exits cleanly");
        assert!(!sock_path.exists());

        // 验证重启持久化状态：Hyprland 写入 user.db，SecretToken 绝未写入
        drop(resources);
        let reopened = UserStore::open_sqlite(&db_path).expect("reopen user.db");
        assert_eq!(reopened.word_freq("Hyprland"), 1);
        assert_eq!(reopened.word_freq("SecretToken"), 0);
    }

    #[cfg(unix)]
    #[test]
    fn async_l3_scheduler_pushes_revision_1_and_drops_stale_seq() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;

        struct DelayedMockNeural;
        impl NeuralSentenceModel for DelayedMockNeural {
            fn backend_name(&self) -> &'static str {
                "mock-async-l3"
            }

            fn rerank_sentences(
                &self,
                _context_before: &str,
                preedit: &str,
                _candidates: &[String],
            ) -> Vec<NeuralCandidate> {
                if preedit == "wo" {
                    thread::sleep(Duration::from_millis(50));
                } else {
                    thread::sleep(Duration::from_millis(10));
                }
                vec![NeuralCandidate {
                    text: "world".into(),
                    score: 12.0,
                    is_continuation: false,
                }]
            }

            fn predict_continuations(
                &self,
                _context_before: &str,
                _prefix: &str,
                _limit: usize,
            ) -> Vec<NeuralCandidate> {
                vec![NeuralCandidate {
                    text: "worldwide web".into(),
                    score: 3.0,
                    is_continuation: true,
                }]
            }
        }

        let resources = Arc::new(SharedResources {
            lexicon: Arc::new(LayeredLexicon::new(build_fallback_lexicon())),
            ngram: Arc::new(NgramLanguageModel::empty()),
            neural: Arc::new(DelayedMockNeural),
            version: VERSION.to_string(),
            transport_cap: "unix-socket",
            l3_debounce_ms: 0,
        });

        let (mut client_stream, server_stream) = UnixStream::pair().expect("unix stream pair");
        client_stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let server_writer = server_stream.try_clone().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let srv_stop = Arc::clone(&stop);
        let srv_handle = thread::spawn(move || {
            let _ = serve_ndjson_stream(server_stream, server_writer, resources, srv_stop);
        });

        let mut reader = BufReader::new(client_stream.try_clone().unwrap());
        let read_resp = |r: &mut BufReader<UnixStream>| -> IpcResponse {
            let mut line = String::new();
            r.read_line(&mut line).expect("read response");
            decode_response_line(&line).expect("decode response")
        };

        // 1. 单按键 seq=1：先收 revision=0, final_l3=false；再收异步推送 revision=1, final_l3=true
        writeln!(client_stream, r#"{{"type":"key","seq":1,"key":"char","text":"w"}}"#).unwrap();
        client_stream.flush().unwrap();
        match read_resp(&mut reader) {
            IpcResponse::State(st) => {
                assert_eq!(st.seq, 1);
                assert_eq!(st.revision, 0);
                assert!(!st.final_l3);
            }
            other => panic!("expected sync state seq=1 rev=0, got {other:?}"),
        }
        match read_resp(&mut reader) {
            IpcResponse::State(st) => {
                assert_eq!(st.seq, 1);
                assert_eq!(st.revision, 1);
                assert!(st.final_l3);
                assert_eq!(st.candidates[0].text, "world");
                assert!(st
                    .candidates
                    .iter()
                    .any(|c| c.kind == CandidateKind::Continuation && c.text == "worldwide web"));
            }
            other => panic!("expected async state seq=1 rev=1, got {other:?}"),
        }

        // 2. 快速连击：发 seq=2 ("wo") 后立刻发 seq=3 ("wor")，seq=2 的异步 rev=1 必须被丢弃
        writeln!(client_stream, r#"{{"type":"key","seq":2,"key":"char","text":"o"}}"#).unwrap();
        client_stream.flush().unwrap();
        match read_resp(&mut reader) {
            IpcResponse::State(st) => {
                assert_eq!(st.seq, 2);
                assert_eq!(st.revision, 0);
            }
            other => panic!("expected seq=2 rev=0, got {other:?}"),
        }

        thread::sleep(Duration::from_millis(5));
        writeln!(client_stream, r#"{{"type":"key","seq":3,"key":"char","text":"r"}}"#).unwrap();
        client_stream.flush().unwrap();

        let mut saw_seq3_rev0 = false;
        let mut saw_seq3_rev1 = false;
        while !(saw_seq3_rev0 && saw_seq3_rev1) {
            match read_resp(&mut reader) {
                IpcResponse::State(st) => {
                    assert!(
                        !(st.seq == 2 && st.revision > 0),
                        "stale L3 frame for seq=2 leaked after seq=3!"
                    );
                    if st.seq == 3 && st.revision == 0 {
                        saw_seq3_rev0 = true;
                    } else if st.seq == 3 && st.revision == 1 && st.final_l3 {
                        saw_seq3_rev1 = true;
                    }
                }
                other => panic!("unexpected frame {other:?}"),
            }
        }

        writeln!(client_stream, r#"{{"type":"shutdown"}}"#).unwrap();
        client_stream.flush().unwrap();
        drop(reader);
        drop(client_stream);
        srv_handle.join().expect("server finishes");
    }
}
