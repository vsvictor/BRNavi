//! Map tiles: `map.getTile`, backed by the `map-tiles` crate.
//!
//! This is the only place that knows both worlds: it validates JSON-RPC params into a
//! `TileCoord`, asks the [`TileService`], maps [`TileError`] onto the JSON-RPC error space and
//! keeps the tile counters in [`Metrics`].

use std::sync::{Arc, atomic::Ordering};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use map_tiles::{TileError, TileService};
use serde_json::{Value, json};

use crate::{
    application::Metrics,
    domain::{RpcError, RpcResult, codes},
    presentation::model::TileParams,
};

pub struct MapController {
    service: Arc<TileService>,
    metrics: Arc<Metrics>,
    max_zoom: u8,
}

impl MapController {
    pub fn new(service: Arc<TileService>, metrics: Arc<Metrics>, max_zoom: u8) -> Self {
        Self {
            service,
            metrics,
            max_zoom,
        }
    }

    pub async fn handle(&self, method: &str, params: Option<Value>) -> RpcResult<Value> {
        match method {
            "map.getTile" => {
                let coord = TileParams::parse(params, self.max_zoom)?;
                let lookup = match self.service.get(coord).await {
                    Ok(lookup) => lookup,
                    Err(err) => {
                        if !err.is_client_error() {
                            self.metrics.tile_upstream_errors.fetch_add(1, Ordering::Relaxed);
                        }
                        return Err(map_tile_error(err));
                    }
                };
                if lookup.from_cache {
                    self.metrics.tile_cache_hits.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.metrics.tile_cache_misses.fetch_add(1, Ordering::Relaxed);
                }
                Ok(json!({
                    "z": coord.z,
                    "x": coord.x,
                    "y": coord.y,
                    "contentType": lookup.tile.content_type,
                    "etag": lookup.tile.etag,
                    "size": lookup.tile.size_bytes(),
                    "fromCache": lookup.from_cache,
                    "data": BASE64.encode(&lookup.tile.bytes),
                }))
            }
            _ => Err(RpcError::method_not_found()),
        }
    }
}

/// Maps tile-domain failures onto JSON-RPC error codes. Transport messages may embed the
/// upstream URL — only relayed in debug builds.
fn map_tile_error(err: TileError) -> RpcError {
    match err {
        TileError::InvalidCoord(msg) => RpcError::owned(codes::INVALID_PARAMS, format!("Invalid params: {msg}")),
        TileError::NotFound => RpcError::new(codes::UPSTREAM_CLIENT_ERROR, "Tile not found"),
        TileError::UpstreamStatus(status) => RpcError::owned(
            RpcError::code_for_upstream_status(status),
            format!("Tile server returned HTTP {status}"),
        ),
        TileError::TooLarge { .. } => {
            RpcError::new(codes::UPSTREAM_RESPONSE_READ_FAILED, "Tile exceeds the size limit")
        }
        TileError::NonImage(_) => RpcError::new(codes::UPSTREAM_OTHER_STATUS, "Upstream returned a non-image tile"),
        TileError::Transport(msg) => {
            if cfg!(debug_assertions) && !msg.trim().is_empty() {
                RpcError::owned(codes::UPSTREAM_REQUEST_FAILED, msg)
            } else {
                RpcError::new(codes::UPSTREAM_REQUEST_FAILED, "Tile request failed")
            }
        }
        TileError::ResponseRead(_) => {
            RpcError::new(codes::UPSTREAM_RESPONSE_READ_FAILED, "Tile response could not be read")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_tile_errors_to_rpc_codes() {
        assert_eq!(map_tile_error(TileError::NotFound).code, codes::UPSTREAM_CLIENT_ERROR);
        assert_eq!(
            map_tile_error(TileError::UpstreamStatus(503)).code,
            codes::UPSTREAM_SERVER_ERROR
        );
        assert_eq!(
            map_tile_error(TileError::UpstreamStatus(403)).code,
            codes::UPSTREAM_FORBIDDEN
        );
        assert_eq!(
            map_tile_error(TileError::TooLarge { actual: 9, max: 1 }).code,
            codes::UPSTREAM_RESPONSE_READ_FAILED
        );
        assert_eq!(
            map_tile_error(TileError::Transport("x".into())).code,
            codes::UPSTREAM_REQUEST_FAILED
        );
        assert_eq!(
            map_tile_error(TileError::InvalidCoord("z".into())).code,
            codes::INVALID_PARAMS
        );
    }
}
