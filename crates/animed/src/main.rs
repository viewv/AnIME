//! AnIME 守护进程入口（Phase 0 提供 --version 与 --stdio NDJSON 烟雾通道；Task 03 扩展 Unix Socket 与完整状态循环）。

use std::io::{self, BufRead, Write};

use anime_engine::EngineSession;
use anime_lexicon::StaticWordList;
use anime_lm_neural::NoopNeuralModel;
use anime_lm_ngram::NoopNgramModel;
use anime_protocol::PROTOCOL_VERSION;

const MAX_IPC_LINE_BYTES: usize = 64 * 1024;
const VERSION: &str = env!("CARGO_PKG_VERSION");

enum StdioResponse {
    Line(String),
    Shutdown,
}

fn handle_stdio_line(line: &str) -> StdioResponse {
    let trimmed = line.trim();
    if trimmed.len() > MAX_IPC_LINE_BYTES {
        return StdioResponse::Line(
            r#"{"type":"error","code":"line_too_large","message":"IPC frame exceeds 64 KiB"}"#
                .to_string(),
        );
    }

    match extract_json_type(trimmed) {
        Some("ping") => StdioResponse::Line(format!(r#"{{"type":"pong","version":"{VERSION}"}}"#)),
        Some("hello") => StdioResponse::Line(format!(
            r#"{{"type":"hello_ack","protocol_version":{PROTOCOL_VERSION},"version":"{VERSION}","capabilities":["l1-smoke","stdio"]}}"#
        )),
        Some("shutdown") => StdioResponse::Shutdown,
        Some(other) => StdioResponse::Line(format!(
            r#"{{"type":"error","code":"not_implemented","message":"phase 0 stdio stub does not dispatch '{other}'"}}"#
        )),
        None => StdioResponse::Line(
            r#"{"type":"error","code":"invalid_json","message":"missing string 'type' field"}"#
                .to_string(),
        ),
    }
}

fn extract_json_type(raw: &str) -> Option<&str> {
    if !(raw.starts_with('{') && raw.ends_with('}')) {
        return None;
    }
    let key_idx = raw.find(r#""type""#)?;
    let after_key = &raw[key_idx + 6..];
    let colon_idx = after_key.find(':')?;
    let after_colon = after_key[colon_idx + 1..].trim_start();
    let rest = after_colon.strip_prefix('"')?;
    let end_quote = rest.find('"')?;
    Some(&rest[..end_quote])
}

fn run_stdio() -> io::Result<()> {
    // 预构建会话对象，校验核心 workspace 链路可初始化。
    let _session = EngineSession::new(
        StaticWordList::smoke_default(),
        NoopNgramModel,
        NoopNeuralModel,
    );

    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line_res in stdin.lock().lines() {
        let line = line_res?;
        if line.trim().is_empty() {
            continue;
        }
        match handle_stdio_line(&line) {
            StdioResponse::Line(resp) => {
                writeln!(stdout, "{resp}")?;
                stdout.flush()?;
            }
            StdioResponse::Shutdown => break,
        }
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("animed {VERSION} (protocol v{PROTOCOL_VERSION})");
        return;
    }
    if args.iter().any(|a| a == "--stdio") {
        if let Err(err) = run_stdio() {
            eprintln!("animed stdio error: {err}");
            std::process::exit(1);
        }
        return;
    }

    eprintln!(
        "animed {VERSION} (Phase 0 skeleton): use --version or --stdio; Unix socket loop is scheduled for task-03-ngram-and-engine"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_hello_and_shutdown_in_stdio_stub() {
        match handle_stdio_line(r#"{"type":"ping"}"#) {
            StdioResponse::Line(s) => {
                assert_eq!(s, r#"{"type":"pong","version":"0.1.0"}"#);
            }
            StdioResponse::Shutdown => panic!("unexpected shutdown"),
        }
        match handle_stdio_line(r#"{"type":"hello","client":"test","protocol_version":1}"#) {
            StdioResponse::Line(s) => assert!(s.contains(r#""type":"hello_ack""#)),
            StdioResponse::Shutdown => panic!("unexpected shutdown"),
        }
        assert!(matches!(
            handle_stdio_line(r#"{"type":"shutdown"}"#),
            StdioResponse::Shutdown
        ));
    }
}
