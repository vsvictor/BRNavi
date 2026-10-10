//! Tile value objects and the port to the upstream tile server.

use async_trait::async_trait;

use super::error::{TileError, TileResult};

/// Slippy-map tile address (`z/x/y`, Web Mercator). Always valid once constructed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileCoord {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileCoord {
    /// Validates `z <= max_zoom` and `x, y < 2^z`.
    pub fn new(z: u8, x: u32, y: u32, max_zoom: u8) -> TileResult<Self> {
        if z > max_zoom {
            return Err(TileError::InvalidCoord(format!(
                "zoom {z} exceeds the maximum of {max_zoom}"
            )));
        }
        let n = 1u64 << z;
        if u64::from(x) >= n || u64::from(y) >= n {
            return Err(TileError::InvalidCoord(format!("x/y out of range for zoom {z}")));
        }
        Ok(Self { z, x, y })
    }
}

impl std::fmt::Display for TileCoord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}/{}", self.z, self.x, self.y)
    }
}

/// A fetched tile image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tile {
    /// MIME type as reported by the upstream (`image/png` by default).
    pub content_type: String,
    /// Raw encoded image bytes.
    pub bytes: Vec<u8>,
    /// Upstream validator, if any.
    pub etag: Option<String>,
}

impl Tile {
    pub fn size_bytes(&self) -> usize {
        self.bytes.len()
    }
}

/// Port: where tiles come from (the OSM tile server in production).
#[async_trait]
pub trait TileSource: Send + Sync {
    async fn fetch(&self, coord: TileCoord) -> TileResult<Tile>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_out_of_range_coordinates() {
        assert!(TileCoord::new(0, 0, 0, 19).is_ok());
        assert!(TileCoord::new(0, 1, 0, 19).is_err());
        assert!(TileCoord::new(3, 7, 7, 19).is_ok());
        assert!(TileCoord::new(3, 8, 0, 19).is_err());
        assert!(TileCoord::new(20, 0, 0, 19).is_err());
        assert!(TileCoord::new(19, (1 << 19) - 1, 0, 19).is_ok());
        assert!(TileCoord::new(20, 0, 0, 19).unwrap_err().is_client_error());
    }
}
