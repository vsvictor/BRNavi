//! Application: the tile use case (cache + single-flight over a [`TileSource`]).

pub mod tiles;

pub use tiles::{TileCacheConfig, TileLookup, TileService};
