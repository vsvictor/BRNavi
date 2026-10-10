//! `map-tiles` — transport-agnostic map tile proxy.
//!
//! Fetches raster tiles (`{z}/{x}/{y}`) from an OSM-style HTTP tile server, keeps them in a
//! byte-bounded in-memory LRU cache with TTL and de-duplicates concurrent misses (single-flight).
//! Knows nothing about JSON-RPC or QUIC: the hosting service (`quic-jsonrpc-server`) exposes
//! [`TileService`] as the `map.getTile` method and maps [`TileError`] to its own error space.
//!
//! Layout (Clean Architecture):
//!
//! ```text
//! application (TileService: cache + single-flight)  ──▶  domain (TileCoord, Tile, TileSource, TileError)
//!                      ▲
//!     infrastructure (TileOptions from env, OsmTileSource over reqwest) ┘
//! ```

pub mod application;
pub mod domain;
pub mod infrastructure;

pub use application::{TileCacheConfig, TileLookup, TileService};
pub use domain::{Tile, TileCoord, TileError, TileResult, TileSource};
pub use infrastructure::{OsmTileSource, TileOptions};

use std::sync::Arc;

/// Convenience constructor: reads `MAP_TILE_*` from the environment and wires
/// [`OsmTileSource`] into a [`TileService`]. Returns the options too so the host can expose
/// e.g. `max_zoom` for request validation.
pub fn from_env() -> anyhow::Result<(Arc<TileService>, TileOptions)> {
    let options = TileOptions::from_env()?;
    let service = Arc::new(TileService::new(
        Arc::new(OsmTileSource::new(options.clone())),
        TileCacheConfig {
            max_bytes: options.cache_max_bytes,
            ttl: options.cache_ttl,
        },
    ));
    Ok((service, options))
}
