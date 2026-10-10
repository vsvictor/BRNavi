//! Environment-driven configuration.
//!
//! Variables live in `quic-jsonrpc-server/.env` (see [`super::env`] for how that file is
//! discovered) and are grouped by the protocol they configure:
//! * `QUIC_RPC_*` — the QUIC listener, sessions, Mongo, TLS, logging;
//! * `HTTP_RPC_*` — the upstream HTTP driver API (base URL, login URL, timeout, retries);
//! * `MAP_TILE_*` — read by the `map-tiles` crate (`map_tiles::TileOptions::from_env`).
//!
//! The only exception is the legacy `BASE_URL` alias, kept so that existing deployments and the
//! e2e tests can override the upstream URL.

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};

pub use crate::application::RateLimitConfig;

const DEFAULT_UPSTREAM_BASE_URL: &str = "http://192.168.0.22/tms/hs/driver-api";

/// QUIC `max_idle_timeout`: how long a connection may go without a single packet *received*
/// before it is declared dead.
///
/// This is a **dead-peer detector**, not a session lifetime. Both peers send QUIC keep-alive
/// PINGs every `QUIC_RPC_KEEP_ALIVE_SECS` (10 s), so a healthy connection — even one with no
/// application traffic for hours — never comes close to this limit. A peer that vanished (NAT
/// binding dropped, phone in Doze, Wi‑Fi ↔ LTE switch without a clean close) stops answering
/// those PINGs and is reaped after this long instead of lingering as a half-open connection.
///
/// quinn (per RFC 9000 §10.1) re-arms the idle timer only when a packet is *received*, and the
/// keep-alive timer likewise only after a receive — so once the peer is gone nothing resets the
/// idle timer and it fires exactly `idle_timeout` after the last inbound packet. With the old
/// 24 h value that meant every dead phone occupied a server connection for a full day.
///
/// The session itself (`QUIC_RPC_SESSION_TTL_SECS`, 24 h) is unaffected: the client reconnects
/// with 0‑RTT and `rpc.resume`.
pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 60;
/// `idle_timeout` must cover at least this many keep-alive intervals so that a couple of lost
/// PINGs (plus PTO back-off) on a flaky mobile path do not kill a live connection.
pub const MIN_KEEP_ALIVES_PER_IDLE_TIMEOUT: u32 = 3;

#[derive(Clone, Debug)]
pub struct TlsPaths {
    /// Leaf (server) certificate, optionally followed by intermediates in the same PEM file.
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
    /// Optional PEM bundle with intermediate CA certificates, appended after `cert_path`.
    pub chain_path: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug)]
pub enum SessionBackend {
    Mongo,
    Memory,
}

#[derive(Clone, Debug)]
pub struct ServerOptions {
    pub bind_addr: SocketAddr,
    pub tls_paths: Option<TlsPaths>,
    pub metrics_interval: Duration,
    pub keep_alive_interval: Duration,
    pub idle_timeout: Duration,
    pub session_ttl: Duration,
    pub session_cleanup_interval: Duration,
    pub session_backend: SessionBackend,
    pub session_debug_enabled: bool,
    /// Per-IP limit for `login` / `rpc.resume` — a coarse flood guard. Mobile clients share
    /// carrier-NAT addresses, so keep this permissive. `per_minute == 0` disables it.
    pub auth_rate_limit: RateLimitConfig,
    /// Per-identity limit (phone for `login`, session id for `rpc.resume`) — the tier that
    /// actually stops brute force against one account. `per_minute == 0` disables it.
    pub auth_identity_rate_limit: RateLimitConfig,
}

/// Upstream driver-API connection parameters. Read separately from [`ServerOptions`] so that
/// integration tests can construct `ServerOptions` literally while still steering the upstream
/// through `HTTP_RPC_BASE_URL` (or the legacy `BASE_URL` override) / `HTTP_RPC_LOGIN_*`.
#[derive(Clone, Debug)]
pub struct UpstreamOptions {
    pub base_url: String,
    /// Full login URL override (`HTTP_RPC_LOGIN_URL`); when absent `base_url + /auth/login/`.
    pub login_url: Option<String>,
    pub timeout: Duration,
    pub retries: u32,
    pub retry_backoff: Duration,
}

/// MongoDB connection parameters for the session store.
#[derive(Clone, Debug)]
pub struct MongoOptions {
    pub uri: String,
    pub database: String,
}

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    match env_non_empty(key) {
        Some(raw) => raw
            .trim()
            .parse::<T>()
            .with_context(|| format!("failed to parse {key}")),
        None => Ok(default),
    }
}

impl ServerOptions {
    pub fn from_env() -> Result<Self> {
        let bind_addr: SocketAddr = std::env::var("QUIC_RPC_BIND")
            .unwrap_or_else(|_| "127.0.0.1:5000".to_string())
            .parse()
            .context("failed to parse QUIC_RPC_BIND")?;

        let chain_path = env_non_empty("QUIC_RPC_CHAIN_PATH").map(PathBuf::from);
        let tls_paths = match (env_non_empty("QUIC_RPC_CERT_PATH"), env_non_empty("QUIC_RPC_KEY_PATH")) {
            (Some(cert), Some(key)) => Some(TlsPaths {
                cert_path: PathBuf::from(cert),
                key_path: PathBuf::from(key),
                chain_path,
            }),
            (None, None) if chain_path.is_some() => {
                bail!("QUIC_RPC_CHAIN_PATH requires QUIC_RPC_CERT_PATH and QUIC_RPC_KEY_PATH")
            }
            (None, None) => None,
            _ => bail!("QUIC_RPC_CERT_PATH and QUIC_RPC_KEY_PATH must be set together"),
        };

        let session_backend = match std::env::var("QUIC_RPC_SESSION_BACKEND")
            .unwrap_or_else(|_| "mongo".to_string())
            .to_lowercase()
            .as_str()
        {
            "mongo" => SessionBackend::Mongo,
            "memory" => SessionBackend::Memory,
            _ => bail!("QUIC_RPC_SESSION_BACKEND must be either 'mongo' or 'memory'"),
        };

        let session_debug_enabled = std::env::var("QUIC_RPC_ENABLE_SESSION_DEBUG")
            .ok()
            .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);

        let keep_alive_interval = Duration::from_secs(env_parse("QUIC_RPC_KEEP_ALIVE_SECS", 10u64)?.max(1));
        let idle_timeout =
            Duration::from_secs(env_parse("QUIC_RPC_IDLE_TIMEOUT_SECS", DEFAULT_IDLE_TIMEOUT_SECS)?.max(5));
        // A too-short idle timeout would drop live clients on the first lost PING; a too-long one
        // re-creates the half-open-connection problem. Enforce the lower bound, warn about the
        // upper one.
        if idle_timeout < keep_alive_interval * MIN_KEEP_ALIVES_PER_IDLE_TIMEOUT {
            bail!(
                "QUIC_RPC_IDLE_TIMEOUT_SECS ({}s) must be at least {MIN_KEEP_ALIVES_PER_IDLE_TIMEOUT}x QUIC_RPC_KEEP_ALIVE_SECS ({}s)",
                idle_timeout.as_secs(),
                keep_alive_interval.as_secs()
            );
        }
        if idle_timeout > Duration::from_secs(10 * 60) {
            tracing::warn!(
                "QUIC_RPC_IDLE_TIMEOUT_SECS={}s is very long: connections to vanished clients (no CONNECTION_CLOSE received) stay half-open for that long",
                idle_timeout.as_secs()
            );
        }

        Ok(Self {
            bind_addr,
            tls_paths,
            metrics_interval: Duration::from_secs(env_parse("QUIC_RPC_METRICS_INTERVAL_SECS", 30u64)?.max(1)),
            keep_alive_interval,
            idle_timeout,
            session_ttl: Duration::from_secs(env_parse("QUIC_RPC_SESSION_TTL_SECS", 86_400u64)?.max(1)),
            session_cleanup_interval: Duration::from_secs(
                env_parse("QUIC_RPC_SESSION_CLEANUP_INTERVAL_SECS", 60u64)?.max(1),
            ),
            session_backend,
            session_debug_enabled,
            // Per-IP: a busy carrier-NAT egress may carry dozens of drivers starting a shift at
            // the same minute, so this only stops outright floods.
            auth_rate_limit: RateLimitConfig {
                per_minute: env_parse("QUIC_RPC_AUTH_RATE_PER_MINUTE", 120u32)?,
                burst: env_parse("QUIC_RPC_AUTH_RATE_BURST", 60u32)?,
            },
            // Per-identity: a human retyping a password never needs more than a handful of
            // attempts per minute.
            auth_identity_rate_limit: RateLimitConfig {
                per_minute: env_parse("QUIC_RPC_AUTH_IDENTITY_RATE_PER_MINUTE", 10u32)?,
                burst: env_parse("QUIC_RPC_AUTH_IDENTITY_RATE_BURST", 5u32)?,
            },
        })
    }
}

impl UpstreamOptions {
    /// Reads the upstream driver-API parameters. Malformed numeric values are an error (not
    /// silently replaced by defaults) so a typo in `.env` is caught at startup.
    pub fn from_env() -> Result<Self> {
        let base_url = env_non_empty("BASE_URL")
            .or_else(|| env_non_empty("HTTP_RPC_BASE_URL"))
            .unwrap_or_else(|| DEFAULT_UPSTREAM_BASE_URL.to_string());

        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            login_url: env_non_empty("HTTP_RPC_LOGIN_URL").map(|u| u.trim().to_string()),
            timeout: Duration::from_millis(env_parse("HTTP_RPC_LOGIN_TIMEOUT_MS", 5000u64)?.max(1)),
            retries: env_parse("HTTP_RPC_LOGIN_RETRIES", 2u32)?,
            retry_backoff: Duration::from_millis(env_parse("HTTP_RPC_LOGIN_RETRY_BACKOFF_MS", 300u64)?.max(1)),
        })
    }
}

impl MongoOptions {
    /// Reads the Mongo connection parameters. Credentials are **required** (either a full
    /// `QUIC_RPC_MONGO_URI` or `QUIC_RPC_MONGO_USER` + `QUIC_RPC_MONGO_PASSWORD`) — there are
    /// deliberately no compiled-in defaults for them.
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(env_non_empty)
    }

    pub(crate) fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let database = lookup("QUIC_RPC_MONGO_DB").unwrap_or_else(|| "tms_db".to_string());

        if let Some(uri) = lookup("QUIC_RPC_MONGO_URI") {
            return Ok(Self { uri, database });
        }

        let host = lookup("QUIC_RPC_MONGO_HOST").unwrap_or_else(|| "127.0.0.1".to_string());
        let port = lookup("QUIC_RPC_MONGO_PORT").unwrap_or_else(|| "27017".to_string());
        let auth_source = lookup("QUIC_RPC_MONGO_AUTH_SOURCE").unwrap_or_else(|| database.clone());
        let user = lookup("QUIC_RPC_MONGO_USER")
            .context("QUIC_RPC_MONGO_USER is required when QUIC_RPC_MONGO_URI is not set")?;
        let password = lookup("QUIC_RPC_MONGO_PASSWORD")
            .context("QUIC_RPC_MONGO_PASSWORD is required when QUIC_RPC_MONGO_URI is not set")?;

        Ok(Self {
            uri: format!("mongodb://{user}:{password}@{host}:{port}/?authSource={auth_source}"),
            database,
        })
    }
}
