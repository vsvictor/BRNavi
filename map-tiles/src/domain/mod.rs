//! Domain: tile value objects, the upstream port and the error type. No I/O.

pub mod error;
pub mod tile;

pub use error::{TileError, TileResult};
pub use tile::{Tile, TileCoord, TileSource};
