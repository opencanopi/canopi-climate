//! Static 0.1-degree terrain layer (`terrain/`): elevation, topographic relief,
//! slope, and Height Above Nearest Drainage (HAND).
//!
//! Bands ([`TerrainBand`]), all `int16`, `NODATA = -32768`:
//!
//! | # | id | unit | scale | content |
//! |---|---|---|---|---|
//! | 0 | `elev_m` | m | 1.0 | cell mean elevation (Copernicus DEM GLO-30; signed, preserves `< 0`) |
//! | 1 | `relief_m` | m | 1.0 | within-cell elevation relief (`p95 - p5`, `>= 0`) |
//! | 2 | `slope_deg` | ° | 0.1 | within-cell median slope (`0..=90`) |
//! | 3 | `hand_m` | m | 0.1 | within-cell median HAND (MERIT-Hydro v1.0.1, ODbL 1.0, 60°S–90°N; optional) |
//!
//! No spatial search: read at the climate lookup's `(cell_lat, cell_lon)` in
//! its `tile`. Method: `docs/decisions/0008-soil-plus-terrain-microclimate-packs.md`.

use serde::{Deserialize, Serialize};

use crate::grid::{Cell, TILE_LEN, TileId};
use crate::lookup::LookupError;
use crate::model::{NODATA, decode_value, quantize};
use crate::raster::{LayerGrid, Raster};
use crate::tile::TileError;

/// Directory and layer id of `terrain`.
pub const DIR: &str = "terrain";

/// Number of bands per `terrain` tile.
pub const TERRAIN_BANDS: usize = 4;

/// Bands of a `terrain` tile, in band order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerrainBand {
    /// Cell mean elevation (m, scale 1.0, signed `int16`).
    ElevM = 0,
    /// Within-cell relief `p95 - p5` (m, scale 1.0).
    ReliefM = 1,
    /// Within-cell median slope (degrees, scale 0.1).
    SlopeDeg = 2,
    /// Within-cell median Height Above Nearest Drainage (m, scale 0.1, optional).
    HandM = 3,
}

/// All `terrain` bands in band order.
pub const TERRAIN_BAND_LIST: [TerrainBand; TERRAIN_BANDS] = [
    TerrainBand::ElevM,
    TerrainBand::ReliefM,
    TerrainBand::SlopeDeg,
    TerrainBand::HandM,
];

impl TerrainBand {
    /// 0-based band index.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Manifest band identifier.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::ElevM => "elev_m",
            Self::ReliefM => "relief_m",
            Self::SlopeDeg => "slope_deg",
            Self::HandM => "hand_m",
        }
    }

    /// Physical unit after decoding.
    #[must_use]
    pub const fn unit(self) -> &'static str {
        match self {
            Self::ElevM | Self::ReliefM | Self::HandM => "m",
            Self::SlopeDeg => "deg",
        }
    }

    /// Linear scale factor (`decoded = stored * scale`).
    #[must_use]
    pub const fn scale(self) -> f32 {
        match self {
            Self::ElevM | Self::ReliefM => 1.0,
            Self::SlopeDeg | Self::HandM => 0.1,
        }
    }

    /// Human-readable manifest description.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::ElevM => {
                "Cell mean elevation (m, Copernicus DEM GLO-30; preserves below-sea-level land)"
            }
            Self::ReliefM => "Within-cell elevation relief (p95 - p5, m, Copernicus DEM GLO-30)",
            Self::SlopeDeg => "Within-cell median terrain slope (degrees, Copernicus DEM GLO-30)",
            Self::HandM => {
                "Within-cell median Height Above Nearest Drainage (m, MERIT-Hydro v1.0.1 ODbL 1.0, 60S-90N; null outside coverage)"
            }
        }
    }

    /// Quantise a physical value to `int16`.
    #[must_use]
    pub fn encode(self, value: f32) -> i16 {
        quantize(value, self.scale(), 0.0)
    }

    /// Decode a stored `int16` (`None` for `NODATA`).
    #[must_use]
    pub fn decode(self, stored: i16) -> Option<f64> {
        decode_value(stored, self.scale(), 0.0)
    }
}

/// Decoded 0.1-degree `terrain` tile (100 × 100 × 4 bands).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerrainTile(Raster);

impl TerrainTile {
    /// Construct from row-major pixel-interleaved `int16` data.
    ///
    /// # Errors
    /// [`TileError::Layout`] if `data.len() != TILE_LEN * TERRAIN_BANDS`.
    pub fn new(id: TileId, data: Vec<i16>) -> Result<Self, TileError> {
        Raster::new(id, LayerGrid::CLIMATE, TERRAIN_BANDS, data).map(Self)
    }

    /// Decode a `terrain` COG tile from bytes.
    ///
    /// # Errors
    /// [`TileError`] if the bytes violate the container contract or band count.
    pub fn decode(bytes: &[u8]) -> Result<Self, TileError> {
        Raster::decode(bytes, LayerGrid::CLIMATE, TERRAIN_BANDS).map(Self)
    }

    /// Underlying generic raster.
    #[must_use]
    pub const fn as_raster(&self) -> &Raster {
        &self.0
    }

    /// Tile identifier.
    #[must_use]
    pub const fn id(&self) -> TileId {
        self.0.id()
    }

    /// Raw pixel-interleaved sample buffer.
    #[must_use]
    pub fn data(&self) -> &[i16] {
        self.0.data()
    }

    /// Stored sample at `cell_index` and `band_index`, or `NODATA` if out of bounds.
    #[must_use]
    pub fn get(&self, cell_index: usize, band_index: usize) -> i16 {
        if cell_index >= TILE_LEN || band_index >= TERRAIN_BANDS {
            return NODATA;
        }
        self.0.get(cell_index, band_index)
    }
}

/// Terrain summary at a climate cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerrainLookup {
    /// Centre latitude of the cell.
    pub cell_lat: f64,
    /// Centre longitude of the cell.
    pub cell_lon: f64,
    /// Tile name.
    pub tile: String,
    /// Cell mean elevation in metres (signed; negative below sea level).
    pub elev_m: i16,
    /// Within-cell relief (`p95 - p5`) in metres.
    pub relief_m: u16,
    /// Within-cell median slope in degrees.
    pub slope_deg: f64,
    /// Within-cell median Height Above Nearest Drainage in metres (`None` outside MERIT-Hydro 60°S–90°N).
    pub hand_m: Option<f64>,
}

/// Read `terrain` at the cell holding `(lat, lon)` inside `tile` (no search).
///
/// A cell is valid whenever `elev_m`, `relief_m`, and `slope_deg` are present;
/// `hand_m` is optional (`None` when `NODATA`, e.g. south of 60°S).
///
/// # Errors
/// [`LookupError::InvalidCoordinate`] or [`LookupError::WrongTile`].
pub fn at_cell(
    tile: &TerrainTile,
    lat: f64,
    lon: f64,
) -> Result<Option<TerrainLookup>, LookupError> {
    let cell = Cell::of(lat, lon).ok_or(LookupError::InvalidCoordinate)?;
    if cell.tile() != tile.id() {
        return Err(LookupError::WrongTile {
            expected: cell.tile().name(),
            given: tile.id().name(),
        });
    }
    let i = cell.index_in_tile();
    let (Some(elev), Some(relief), Some(slope)) = (
        TerrainBand::ElevM.decode(tile.get(i, TerrainBand::ElevM.index())),
        TerrainBand::ReliefM.decode(tile.get(i, TerrainBand::ReliefM.index())),
        TerrainBand::SlopeDeg.decode(tile.get(i, TerrainBand::SlopeDeg.index())),
    ) else {
        return Ok(None);
    };
    let hand = TerrainBand::HandM.decode(tile.get(i, TerrainBand::HandM.index()));
    let round6 = |x: f64| (x * 1e6).round() / 1e6;
    Ok(Some(TerrainLookup {
        cell_lat: round6(cell.center_lat()),
        cell_lon: round6(cell.center_lon()),
        tile: tile.id().name(),
        elev_m: elev.round() as i16,
        relief_m: relief.max(0.0).round() as u16,
        slope_deg: slope,
        hand_m: hand,
    }))
}

/// Synthetic `terrain` tile matching `tile::synthetic::tile(n50e000)` for tests.
#[cfg(test)]
pub mod synthetic {
    use super::*;
    use crate::tile::synthetic as climate_syn;

    /// Synthetic `terrain` tile for `n50e000` (includes normal land, below-sea-level
    /// corner cell, and `hand_m = NODATA` cell).
    #[must_use]
    pub fn terrain() -> TerrainTile {
        let id = TileId::parse("n50e000").expect("valid tile id");
        let climate = climate_syn::tile(id);
        let mut data = vec![NODATA; TILE_LEN * TERRAIN_BANDS];
        for i in 0..TILE_LEN {
            if !climate.is_complete(i) {
                continue;
            }
            let base = i * TERRAIN_BANDS;
            if i == 0 {
                // NW corner: below sea level (like polders / Dead Sea), valid HAND = 0.5 m.
                data[base + TerrainBand::ElevM.index()] = TerrainBand::ElevM.encode(-15.0);
                data[base + TerrainBand::ReliefM.index()] = TerrainBand::ReliefM.encode(12.0);
                data[base + TerrainBand::SlopeDeg.index()] = TerrainBand::SlopeDeg.encode(0.4);
                data[base + TerrainBand::HandM.index()] = TerrainBand::HandM.encode(0.5);
            } else if i == 99 {
                // NE corner: valid terrain, missing HAND (simulating south of 60S / unmapped island).
                data[base + TerrainBand::ElevM.index()] = TerrainBand::ElevM.encode(420.0);
                data[base + TerrainBand::ReliefM.index()] = TerrainBand::ReliefM.encode(310.0);
                data[base + TerrainBand::SlopeDeg.index()] = TerrainBand::SlopeDeg.encode(14.5);
                data[base + TerrainBand::HandM.index()] = NODATA;
            } else {
                data[base + TerrainBand::ElevM.index()] = TerrainBand::ElevM.encode(210.0);
                data[base + TerrainBand::ReliefM.index()] = TerrainBand::ReliefM.encode(180.0);
                data[base + TerrainBand::SlopeDeg.index()] = TerrainBand::SlopeDeg.encode(4.2);
                data[base + TerrainBand::HandM.index()] = TerrainBand::HandM.encode(12.5);
            }
        }
        TerrainTile::new(id, data).expect("valid synthetic terrain tile")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_cell_preserves_below_sea_level_and_optional_hand() {
        let tile = synthetic::terrain();

        // Standard land cell
        let mid = at_cell(&tile, 45.25, 3.25)
            .unwrap()
            .expect("land cell has terrain");
        assert_eq!(mid.elev_m, 210);
        assert_eq!(mid.relief_m, 180);
        assert!((mid.slope_deg - 4.2).abs() < 1e-9);
        assert_eq!(mid.hand_m, Some(12.5));

        // NW corner (49.95, 0.05): below sea level
        let low = at_cell(&tile, 49.95, 0.05)
            .unwrap()
            .expect("below-sea-level cell is preserved");
        assert_eq!(low.elev_m, -15);
        assert_eq!(low.hand_m, Some(0.5));

        // NE corner (49.95, 9.95): missing HAND stays None without invalidating cell
        let no_hand = at_cell(&tile, 49.95, 9.95)
            .unwrap()
            .expect("missing HAND keeps terrain cell valid");
        assert_eq!(no_hand.elev_m, 420);
        assert_eq!(no_hand.hand_m, None);

        // Sea cell returns None
        assert!(at_cell(&tile, 40.55, 0.55).unwrap().is_none());
    }
}
