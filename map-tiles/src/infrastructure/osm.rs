//! reqwest-based [`TileSource`] fetching raster tiles from an OSM-style `{z}/{x}/{y}` server.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, StatusCode, header};
use tracing::{debug, warn};

use crate::{
    domain::{Tile, TileCoord, TileError, TileResult, TileSource},
    infrastructure::config::TileOptions,
};

const DEFAULT_CONTENT_TYPE: &str = "image/png";

pub struct OsmTileSource {
    options: TileOptions,
    client: Client,
}

impl OsmTileSource {
    pub fn new(options: TileOptions) -> Self {
        let client = Client::builder()
            .user_agent(options.user_agent.clone())
            .connect_timeout(options.timeout.min(Duration::from_secs(5)))
            .timeout(options.timeout)
            .build()
            .unwrap_or_else(|_| Client::new());
        Self { options, client }
    }

    pub fn url(&self, coord: TileCoord) -> String {
        self.options
            .url_template
            .replace("{z}", &coord.z.to_string())
            .replace("{x}", &coord.x.to_string())
            .replace("{y}", &coord.y.to_string())
    }

    async fn attempt(&self, url: &str, coord: TileCoord) -> TileResult<Tile> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| TileError::Transport(e.to_string()))?;
        let status = response.status();

        if status == StatusCode::NOT_FOUND {
            return Err(TileError::NotFound);
        }
        if !status.is_success() {
            return Err(TileError::UpstreamStatus(status.as_u16()));
        }

        let max = self.options.max_tile_bytes;
        if let Some(len) = response.content_length()
            && len > max as u64
        {
            return Err(TileError::TooLarge {
                actual: len as usize,
                max,
            });
        }

        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or(v).trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_string());
        let etag = response
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);

        let bytes = response
            .bytes()
            .await
            .map_err(|e| TileError::ResponseRead(e.to_string()))?;
        if bytes.len() > max {
            return Err(TileError::TooLarge {
                actual: bytes.len(),
                max,
            });
        }
        if !content_type.starts_with("image/") {
            warn!("tile {coord} upstream returned non-image content-type {content_type}");
            return Err(TileError::NonImage(content_type));
        }

        Ok(Tile {
            content_type,
            bytes: bytes.to_vec(),
            etag,
        })
    }
}

#[async_trait]
impl TileSource for OsmTileSource {
    async fn fetch(&self, coord: TileCoord) -> TileResult<Tile> {
        let url = self.url(coord);
        let total_attempts = self.options.retries.saturating_add(1);
        let mut last_err = TileError::Transport("no attempts made".into());

        for attempt in 1..=total_attempts {
            debug!("tile {coord} attempt={attempt}/{total_attempts} GET {url}");
            match self.attempt(&url, coord).await {
                Ok(tile) => return Ok(tile),
                Err(err) => {
                    // GET is idempotent: retry transient failures (transport, 5xx); 4xx and
                    // oversize are final.
                    if err.is_retryable() && attempt < total_attempts {
                        warn!("tile {coord} attempt {attempt}/{total_attempts} failed: {err}; retrying");
                        tokio::time::sleep(self.options.retry_backoff).await;
                        last_err = err;
                        continue;
                    }
                    if !err.is_client_error() && !matches!(err, TileError::NotFound) {
                        warn!("tile {coord} failed after {attempt} attempt(s): {err}");
                    }
                    return Err(err);
                }
            }
        }

        Err(last_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(template: &str) -> TileOptions {
        TileOptions {
            url_template: template.to_string(),
            user_agent: "test".into(),
            timeout: Duration::from_secs(1),
            retries: 0,
            retry_backoff: Duration::from_millis(1),
            max_zoom: 19,
            max_tile_bytes: 1024,
            cache_max_bytes: 0,
            cache_ttl: Duration::from_secs(1),
        }
    }

    #[test]
    fn expands_url_template() {
        let source = OsmTileSource::new(options("https://t.example/{z}/{x}/{y}.png?s={z}"));
        let coord = TileCoord::new(7, 73, 44, 19).unwrap();
        assert_eq!(source.url(coord), "https://t.example/7/73/44.png?s=7");
    }

    #[tokio::test]
    async fn connection_failure_maps_to_transport_error() {
        let source = OsmTileSource::new(options("http://127.0.0.1:9/{z}/{x}/{y}.png"));
        let err = source.fetch(TileCoord::new(1, 0, 0, 19).unwrap()).await.unwrap_err();
        assert!(matches!(err, TileError::Transport(_)), "{err}");
        assert!(err.is_retryable());
    }
}
