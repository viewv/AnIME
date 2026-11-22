//! 操作系统运行时路径与资产发现层（仅限 `crates/animed` 使用，核心引擎保持平台无关）。

use std::env;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePaths {
    pub socket_path: PathBuf,
    pub data_dir: PathBuf,
    pub lexicon_path: Option<PathBuf>,
    pub extension_dir: PathBuf,
    pub ngram_path: Option<PathBuf>,
    pub user_db_path: PathBuf,
    pub neural_model_path: Option<PathBuf>,
}

fn non_empty_env(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[cfg(target_os = "linux")]
fn current_uid_or_fallback() -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .unwrap_or(1000)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn current_uid_or_fallback() -> u32 {
    1000
}

pub fn resolve_socket_path(cli_override: Option<&Path>) -> PathBuf {
    if let Some(p) = cli_override {
        return p.to_path_buf();
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = non_empty_env("HOME") {
            return home.join("Library/Caches/AnIME/engined.sock");
        }
    }
    #[cfg(unix)]
    {
        if let Some(runtime_dir) = non_empty_env("XDG_RUNTIME_DIR") {
            return runtime_dir.join("anime/engined.sock");
        }
        return PathBuf::from(format!("/tmp/anime-{}/engined.sock", current_uid_or_fallback()));
    }
    #[cfg(not(unix))]
    {
        env::temp_dir().join("anime/engined.sock")
    }
}

pub fn resolve_data_dir(cli_override: Option<&Path>) -> PathBuf {
    if let Some(p) = cli_override {
        return p.to_path_buf();
    }
    if let Some(p) = non_empty_env("ANIME_DATA_DIR") {
        return p;
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = non_empty_env("HOME") {
            return home.join("Library/Application Support/AnIME");
        }
    }
    if let Some(xdg) = non_empty_env("XDG_DATA_HOME") {
        return xdg.join("anime");
    }
    if let Some(home) = non_empty_env("HOME") {
        return home.join(".local/share/anime");
    }
    env::temp_dir().join("anime")
}

fn find_first_file(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|p| p.is_file()).cloned()
}

fn find_sorted_gguf_in_dir(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut ggufs: Vec<PathBuf> = entries
        .filter_map(|res| res.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("gguf"))
        })
        .collect();
    ggufs.sort();
    ggufs.into_iter().next()
}

#[allow(dead_code)]
pub fn discover_runtime_paths(
    socket_override: Option<&Path>,
    data_dir_override: Option<&Path>,
) -> RuntimePaths {
    discover_runtime_paths_with_model(socket_override, data_dir_override, None)
}

pub fn discover_runtime_paths_with_model(
    socket_override: Option<&Path>,
    data_dir_override: Option<&Path>,
    model_override: Option<&Path>,
) -> RuntimePaths {
    let socket_path = resolve_socket_path(socket_override);
    let data_dir = resolve_data_dir(data_dir_override);
    let extension_dir = data_dir.join("dicts");
    let user_db_path = data_dir.join("user.db");

    let lexicon_candidates = [
        data_dir.join("models/lexicon.animelex.bin"),
        data_dir.join("lexicon.animelex.bin"),
        PathBuf::from("/usr/share/anime/lexicon.animelex.bin"),
        PathBuf::from("/usr/share/anime/models/lexicon.animelex.bin"),
        PathBuf::from("data/compiled/lexicon.animelex.bin"),
    ];
    let ngram_candidates = [
        data_dir.join("models/ngram.animengr.bin"),
        data_dir.join("ngram.animengr.bin"),
        PathBuf::from("/usr/share/anime/ngram.animengr.bin"),
        PathBuf::from("/usr/share/anime/models/ngram.animengr.bin"),
        PathBuf::from("data/compiled/ngram.animengr.bin"),
    ];

    let data_models_dir = data_dir.join("models");
    let neural_model_path = if let Some(explicit) = model_override {
        Some(explicit.to_path_buf())
    } else if let Some(env_model) = non_empty_env("ANIME_NEURAL_MODEL") {
        Some(env_model)
    } else {
        find_first_file(&[
            data_models_dir.join("l3.gguf"),
            data_models_dir.join("SmolLM2-360M.Q4_K_M.gguf"),
            data_models_dir.join("SmolLM2-135M-Q4_K_M.gguf"),
        ])
        .or_else(|| find_sorted_gguf_in_dir(&data_models_dir))
        .or_else(|| {
            find_first_file(&[
                PathBuf::from("/usr/share/anime/models/l3.gguf"),
                PathBuf::from("/usr/share/anime/models/SmolLM2-360M.Q4_K_M.gguf"),
                PathBuf::from("data/compiled/models/SmolLM2-360M.Q4_K_M.gguf"),
                PathBuf::from("data/compiled/models/SmolLM2-135M-Q4_K_M.gguf"),
            ])
        })
    };

    RuntimePaths {
        socket_path,
        data_dir,
        lexicon_path: find_first_file(&lexicon_candidates),
        extension_dir,
        ngram_path: find_first_file(&ngram_candidates),
        user_db_path,
        neural_model_path,
    }
}
