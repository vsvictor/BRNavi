//! `.env` discovery for this service.
//!
//! All `quic-jsonrpc-server` settings live in `quic-jsonrpc-server/.env` (next to `Cargo.toml`).
//! Nothing is read from the cwd, so the service behaves identically when started via
//! `cargo run -p quic-jsonrpc-server`, from an IDE, or through the `brnavi` orchestrator.
//!
//! Resolution order for every variable (first non-empty value wins):
//! 1. the real process environment,
//! 2. `quic-jsonrpc-server.env` next to the executable (deployed binaries) — when it exists it
//!    **replaces** `quic-jsonrpc-server/.env` (`CARGO_MANIFEST_DIR`, baked in at compile time,
//!    used for `cargo run` / IDE runs), so a deployment never depends on a checkout that may
//!    happen to exist at the build path on the target host,
//! 3. the workspace-root `.env` (or `.env` next to the executable) — **fallback** for anything
//!    the crate file leaves out or sets to an empty value (`KEY=`).
//!
//! Relative paths are anchored to the directory of the file they came from.

use std::path::{Path, PathBuf};

use tracing_subscriber::EnvFilter;

/// Prefix of the QUIC-side variables of this service (listener, sessions, Mongo, TLS, logging).
pub const ENV_PREFIX: &str = "QUIC_RPC";

/// Prefix of the variables describing the upstream HTTP driver API (`HTTP_RPC_BASE_URL`, …).
pub const HTTP_ENV_PREFIX: &str = "HTTP_RPC";

/// Service-specific log filter variable; falls back to `RUST_LOG`.
pub const LOG_FILTER_VAR: &str = "QUIC_RPC_RUST_LOG";

/// File name looked up next to the executable for deployed builds.
pub const DEPLOYED_ENV_FILE: &str = "quic-jsonrpc-server.env";

/// Variables holding filesystem paths that are resolved relative to the file that set them.
const PATH_VARS: [&str; 3] = ["QUIC_RPC_CERT_PATH", "QUIC_RPC_KEY_PATH", "QUIC_RPC_CHAIN_PATH"];

/// Directory of this crate (where its `.env` lives).
pub fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Workspace root (parent of the crate directory) — where the shared fallback `.env` lives.
pub fn workspace_root() -> PathBuf {
    crate_dir().parent().map(Path::to_path_buf).unwrap_or_else(crate_dir)
}

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
}

/// Loads this service's `.env` file(s), fills the gaps from the workspace-root `.env`, and
/// anchors relative paths.
///
/// Must be called single-threaded (before any async runtime starts) because it mutates the
/// process environment. Returns the files that were loaded, in load order; parse failures are
/// reported on stderr (dotenvy applies lines up to the failing one).
pub fn load_dotenv() -> Vec<PathBuf> {
    let mut loaded: Vec<PathBuf> = Vec::new();

    // 1. Own file. A deployed binary must not depend on whatever checkout happens to live at
    //    the compile-time `CARGO_MANIFEST_DIR` on the target host, so `quic-jsonrpc-server.env`
    //    next to the executable *replaces* the crate file when present. Process env still wins.
    let deployed = exe_dir()
        .map(|dir| dir.join(DEPLOYED_ENV_FILE))
        .filter(|path| path.is_file());
    let own = deployed.unwrap_or_else(|| crate_dir().join(".env"));
    if own.is_file() {
        match dotenvy::from_path(&own) {
            Ok(()) => {}
            Err(err) => eprintln!(
                "warning: failed to fully parse {}: {err} — variables after the bad line were NOT loaded",
                own.display()
            ),
        }

        let base = own.parent().map(Path::to_path_buf).unwrap_or_else(crate_dir);
        anchor_relative_paths(&base, &PATH_VARS);
        loaded.push(own);
    } else {
        anchor_relative_paths(&crate_dir(), &PATH_VARS);
    }

    // 2. Root fallback: only keys that are still unset or empty.
    let mut roots = vec![workspace_root().join(".env")];
    if let Some(dir) = exe_dir() {
        roots.push(dir.join(".env"));
    }
    for candidate in roots {
        if !candidate.is_file() || loaded.iter().any(|p| same_file(p, &candidate)) {
            continue;
        }
        let filled = fill_missing_from(&candidate);
        if !filled.is_empty() {
            let base = candidate.parent().map(Path::to_path_buf).unwrap_or_else(workspace_root);
            let path_keys: Vec<&str> = filled
                .iter()
                .map(String::as_str)
                .filter(|k| PATH_VARS.contains(k))
                .collect();
            anchor_relative_paths(&base, &path_keys);
            loaded.push(candidate);
        }
    }

    loaded
}

/// Strict loader for administrative utilities. Never includes dotenv parser errors, which
/// can contain the credential-bearing source line, in output.
pub fn load_dotenv_checked() -> anyhow::Result<()> {
    let own = exe_dir()
        .map(|dir| dir.join(DEPLOYED_ENV_FILE))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| crate_dir().join(".env"));
    let mut files = vec![own, workspace_root().join(".env")];
    if let Some(dir) = exe_dir() {
        files.push(dir.join(".env"));
    }
    let mut loaded: Vec<PathBuf> = Vec::new();
    for path in files {
        if !path.is_file() || loaded.iter().any(|previous| same_file(previous, &path)) {
            continue;
        }
        let pairs: Vec<(String, String)> = dotenvy::from_path_iter(&path)
            .map_err(|_| anyhow::anyhow!("Unable to read environment configuration"))?
            .collect::<std::result::Result<_, _>>()
            .map_err(|_| anyhow::anyhow!("Invalid environment configuration"))?;
        for (key, value) in pairs {
            if env_non_empty(&key).is_none() && !value.trim().is_empty() {
                // SAFETY: this utility calls the loader before creating any runtime/threads.
                unsafe { std::env::set_var(&key, &value) };
                if PATH_VARS.contains(&key.as_str()) {
                    anchor_relative_paths(path.parent().unwrap_or_else(|| Path::new(".")), &[&key]);
                }
            }
        }
        loaded.push(path);
    }
    Ok(())
}

/// Applies `KEY=VALUE` pairs from `path` for every key that is currently unset or empty in the
/// process environment. Returns the keys that were filled.
fn fill_missing_from(path: &Path) -> Vec<String> {
    let iter = match dotenvy::from_path_iter(path) {
        Ok(iter) => iter,
        Err(err) => {
            eprintln!("warning: failed to read {}: {err}", path.display());
            return Vec::new();
        }
    };

    let mut filled = Vec::new();
    for item in iter {
        match item {
            Ok((key, value)) => {
                if env_non_empty(&key).is_none() && !value.trim().is_empty() {
                    // SAFETY: called from `main` before the tokio runtime exists — no other threads.
                    unsafe { std::env::set_var(&key, &value) };
                    filled.push(key);
                }
            }
            Err(err) => {
                eprintln!(
                    "warning: failed to fully parse {}: {err} — variables after the bad line were NOT loaded",
                    path.display()
                );
                break;
            }
        }
    }
    filled
}

/// Resolves relative paths (`QUIC_RPC_CERT_PATH=../certs/…`) in `keys` against `base` so the
/// result does not depend on the cwd the process was started from.
fn anchor_relative_paths(base: &Path, keys: &[&str]) {
    for key in keys {
        if let Some(raw) = env_non_empty(key) {
            let path = PathBuf::from(raw.trim());
            if path.is_relative() {
                // SAFETY: called from `main` before the tokio runtime exists — no other threads.
                unsafe { std::env::set_var(key, base.join(path)) };
            }
        }
    }
}

/// Raw filter string declared by this service (`QUIC_RPC_RUST_LOG`, then `RUST_LOG`).
pub fn log_directives() -> Option<String> {
    env_non_empty(LOG_FILTER_VAR).or_else(|| env_non_empty("RUST_LOG"))
}

/// Builds the tracing filter for a standalone run of this service (default `info`).
pub fn log_filter() -> EnvFilter {
    log_directives()
        .and_then(|v| EnvFilter::try_new(v).ok())
        .unwrap_or_else(|| EnvFilter::new("info"))
}
