//! Environment bootstrap for the orchestrator.
//!
//! Each service owns its configuration file and knows how to load it:
//! * `quic-jsonrpc-server/.env` — `QUIC_RPC_*` + `HTTP_RPC_*` (loaded by `quic_jsonrpc_server::env`).
//!
//! Resolution order for every variable: process environment → the owning crate's `.env` →
//! the workspace-root `.env` as a fallback for anything missing or empty. The service loaders
//! implement that fallback themselves, so the orchestrator only has to run them *before* it
//! reads the root file for its own `TMS_*` settings (dotenvy never overrides already-set
//! variables, so the order matters).

use std::path::PathBuf;

use tracing_subscriber::EnvFilter;

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Loads each service's `.env` via the service crate (crate file, then root fallback), then
/// the orchestrator's own `TMS_*` settings from the workspace-root `.env`.
///
/// Must run single-threaded, before the Tokio runtime starts (mutates the process environment).
/// Returns every distinct file that was loaded, in load order.
pub fn load_dotenv_files() -> Vec<PathBuf> {
    let mut loaded: Vec<PathBuf> = Vec::new();
    let push_unique = |loaded: &mut Vec<PathBuf>, path: PathBuf| {
        let dup = loaded.iter().any(|p| match (p.canonicalize(), path.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => *p == path,
        });
        if !dup {
            loaded.push(path);
        }
    };

    for path in quic_jsonrpc_server::env::load_dotenv() {
        push_unique(&mut loaded, path);
    }

    let own = workspace_root().join(".env");
    if own.is_file() {
        match dotenvy::from_path(&own) {
            Ok(()) => push_unique(&mut loaded, own),
            Err(err) => eprintln!("warning: failed to fully parse {}: {err}", own.display()),
        }
    }
    loaded
}

/// Builds the process-wide log filter.
///
/// * `TMS_RUST_LOG` — used verbatim when set.
/// * otherwise: `info` for the orchestrator, plus each service's own per-target directives
///   (`QUIC_RPC_RUST_LOG`, falling back to `RUST_LOG`) concatenated.
pub fn log_filter() -> EnvFilter {
    if let Some(explicit) = env_non_empty("TMS_RUST_LOG")
        && let Ok(filter) = EnvFilter::try_new(&explicit)
    {
        return filter;
    }

    let mut directives = vec!["info".to_string()];
    for raw in [quic_jsonrpc_server::env::log_directives()].into_iter().flatten() {
        // Keep only per-target directives; a bare level (e.g. `debug`) from one service must
        // not change the global default for the orchestrator.
        directives.extend(
            raw.split(',')
                .map(str::trim)
                .filter(|d| d.contains('='))
                .map(str::to_string),
        );
    }
    EnvFilter::try_new(directives.join(",")).unwrap_or_else(|_| EnvFilter::new("info"))
}
