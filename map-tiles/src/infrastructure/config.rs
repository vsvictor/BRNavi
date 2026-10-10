//! `MAP_TILE_*` environment configuration.
//!
//! The hosting service is responsible for loading its `.env` before calling
//! [`TileOptions::from_env`]; this crate only reads the process environment.

use std::time::Duration;

use anyhow::{Context, Result, bail};

/// Local OSM tile server (plain HTTP, same LAN as the TMS backend).
pub const DEFAULT_URL_TEMPLATE: &str = "http://192.168.1.242:8000/tiles/{z}/{x}/{y}.png";
pub const DEFAULT_USER_AGENT: &str = "ExpressT-TMS/1.0 (com.expresst.tms)";

/// Upstream tile server + cache parameters.
#[derive(Clone, Debug)]
pub struct TileOptions {
    /// URL template with `{z}`, `{x}`, `{y}` placeholders.
    pub url_template: String,
    /// Sent as `User-Agent`. Required by the OSM tile usage policy when pointing at
    /// `tile.openstreetmap.org`.
    pub user_agent: String,
    pub timeout: Duration,
    pub retries: u32,
    pub retry_backoff: Duration,
    /// Highest zoom level clients may request.
    pub max_zoom: u8,
    /// Upstream responses larger than this are rejected.
    pub max_tile_bytes: usize,
    /// In-memory cache budget in bytes; `0` disables caching.
    pub cache_max_bytes: usize,
    pub cache_ttl: Duration,
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

impl Default for TileOptions {
    fn default() -> Self {
        Self {
            url_template: DEFAULT_URL_TEMPLATE.to_string(),
            user_agent: DEFAULT_USER_AGENT.to_string(),
            timeout: Duration::from_secs(10),
            retries: 1,
            retry_backoff: Duration::from_millis(300),
            max_zoom: 19,
            max_tile_bytes: 4 * 1024 * 1024,
            cache_max_bytes: 256 * 1024 * 1024,
            cache_ttl: Duration::from_secs(7 * 86_400),
        }
    }
}

impl TileOptions {
    /// Reads `MAP_TILE_*`. Malformed numeric values are an error (not silently defaulted) so a
    /// typo in `.env` is caught at startup.
    pub fn from_env() -> Result<Self> {
        let defaults = Self::default();

        let url_template = env_non_empty("MAP_TILE_URL_TEMPLATE")
            .map(|v| v.trim().to_string())
            .unwrap_or(defaults.url_template);
        for placeholder in ["{z}", "{x}", "{y}"] {
            if !url_template.contains(placeholder) {
                bail!("MAP_TILE_URL_TEMPLATE must contain the {placeholder} placeholder");
            }
        }

        Ok(Self {
            url_template,
            user_agent: env_non_empty("MAP_TILE_USER_AGENT")
                .map(|v| v.trim().to_string())
                .unwrap_or(defaults.user_agent),
            timeout: Duration::from_millis(
                env_parse("MAP_TILE_TIMEOUT_MS", defaults.timeout.as_millis() as u64)?.max(1),
            ),
            retries: env_parse("MAP_TILE_RETRIES", defaults.retries)?,
            retry_backoff: Duration::from_millis(
                env_parse("MAP_TILE_RETRY_BACKOFF_MS", defaults.retry_backoff.as_millis() as u64)?.max(1),
            ),
            max_zoom: env_parse("MAP_TILE_MAX_ZOOM", defaults.max_zoom)?.min(22),
            max_tile_bytes: env_parse("MAP_TILE_MAX_BYTES", defaults.max_tile_bytes)?.max(1024),
            cache_max_bytes: env_parse("MAP_TILE_CACHE_MAX_BYTES", defaults.cache_max_bytes)?,
            cache_ttl: Duration::from_secs(env_parse("MAP_TILE_CACHE_TTL_SECS", defaults.cache_ttl.as_secs())?.max(1)),
        })
    }
}
