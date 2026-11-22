//! The `monthly_precip` layer: 12-month precipitation climatology on a
//! 0.25-degree grid (40 x 40 cells per 10-degree tile, under
//! `monthly_precip/`), for 3 slots (`1991-2020`, `2041-2070 ssp245`,
//! `2071-2099 ssp245`).
//!
//! Band layout (36 bands, `int16`, pixel-interleaved, Deflate, nodata -32768):
//! - `0..36`: `precip` (`slot * 12 + month`, scale 1 mm)
//!
//! Lookup checks the point's own 0.25-degree cell and falls back to the
//! nearest cell with data within [`PRECIP_LOOKUP_RADIUS`] (1 cell) so coastal
//! climate cells never lose monthly rain. Method:
//! `docs/decisions/0004-monthly-and-relief-layers.md`.

use serde::Serialize;

use crate::grid::TileId;
use crate::lookup::LookupError;
use crate::model::{NODATA, quantize};
use crate::monthly::MONTHLY_SLOTS;
use crate::raster::{GridCell, LayerGrid, Raster, nearest, nearest_in_tile};
use crate::tile::TileError;

/// Directory of the layer's tiles, relative to the manifest.
pub const DIR: &str = "monthly_precip";

/// Quarter-degree cells per tile side.
pub const PRECIP_TILE_CELLS: usize = LayerGrid::QUARTER_DEG.tile_cells();

/// Quarter-degree cells per tile (40 x 40 = 1600).
pub const PRECIP_TILE_LEN: usize = LayerGrid::QUARTER_DEG.tile_len();

/// Total bands per `monthly_precip` tile: 3 slots x 12 months.
pub const PRECIP_BANDS: usize = 36;

/// Default coastal fallback search radius in 0.25-degree cells.
pub const PRECIP_LOOKUP_RADIUS: usize = 1;

/// 0-based band index of `precip` for `slot` (`0..3`) and `month` (`0..12`).
#[must_use]
pub const fn precip_band(slot: usize, month: usize) -> usize {
    slot * 12 + month
}

/// Encode monthly precipitation in mm (scale 1 mm); non-finite values become
/// [`NODATA`].
#[must_use]
pub fn encode_precip(mm: f32) -> i16 {
    if !mm.is_finite() {
        return NODATA;
    }
    quantize(mm.max(0.0), 1.0, 0.0)
}

/// Decode a stored precipitation value in mm; [`NODATA`] becomes `None`.
#[must_use]
pub fn decode_precip(stored: i16) -> Option<f64> {
    (stored != NODATA).then(|| f64::from(stored))
}

/// A decoded monthly precipitation tile: [`PRECIP_TILE_LEN`] cells x
/// [`PRECIP_BANDS`] values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonthlyPrecipTile {
    raster: Raster,
}

impl MonthlyPrecipTile {
    /// A tile from pixel-interleaved values.
    ///
    /// # Errors
    /// [`TileError::Layout`] when `data` has the wrong length.
    pub fn new(id: TileId, data: Vec<i16>) -> Result<Self, TileError> {
        Ok(Self {
            raster: Raster::new(id, LayerGrid::QUARTER_DEG, PRECIP_BANDS, data)?,
        })
    }

    /// Underlying generic raster.
    #[must_use]
    pub const fn as_raster(&self) -> &Raster {
        &self.raster
    }

    /// Decode a tile file (the whole file, as fetched).
    ///
    /// # Errors
    /// [`TileError`] when the bytes are not a monthly_precip tile.
    pub fn decode(bytes: &[u8]) -> Result<Self, TileError> {
        Ok(Self {
            raster: Raster::decode(bytes, LayerGrid::QUARTER_DEG, PRECIP_BANDS)?,
        })
    }

    /// The tile's position.
    #[must_use]
    pub const fn id(&self) -> TileId {
        self.raster.id()
    }

    /// One value; [`NODATA`] outside the tile.
    #[must_use]
    pub fn get(&self, index: usize, band: usize) -> i16 {
        self.raster.get(index, band)
    }

    /// Whether a cell has every band.
    #[must_use]
    pub fn has(&self, index: usize) -> bool {
        let p = self.raster.pixel(index);
        !p.is_empty() && p.iter().all(|&v| v != NODATA)
    }

    /// Cells with every band.
    #[must_use]
    pub fn cells(&self) -> usize {
        (0..PRECIP_TILE_LEN).filter(|&i| self.has(i)).count()
    }

    /// The pixel-interleaved values.
    #[must_use]
    pub fn data(&self) -> &[i16] {
        self.raster.data()
    }
}

/// Monthly precipitation of one slot.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PeriodMonthlyPrecip {
    /// `1991-2020`, `2041-2070` or `2071-2099`.
    pub period: &'static str,
    /// `historical` or `ssp245`.
    pub scenario: &'static str,
    /// Monthly precipitation totals for Jan..Dec (mm, 1 mm step).
    pub precip_mm: [f64; 12],
}

/// Monthly precipitation at a point (own 0.25-degree cell, or nearest within
/// radius 1 for coastal fallback).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MonthlyPrecipLookup {
    /// Queried latitude.
    pub lat: f64,
    /// Queried longitude.
    pub lon: f64,
    /// Centre of the cell read.
    pub cell_lat: f64,
    /// Centre of the cell read.
    pub cell_lon: f64,
    /// Distance from the queried cell in 0.25-degree cells (0 = own cell).
    pub distance_cells: f64,
    /// Tile name, e.g. `n50e000`.
    pub tile: String,
    /// One entry per monthly slot, in [`MONTHLY_SLOTS`] order.
    pub periods: Vec<PeriodMonthlyPrecip>,
}

/// Tiles a search of `radius` 0.25-degree cells around (`lat`, `lon`) can
/// touch.
///
/// # Errors
/// [`LookupError::InvalidCoordinate`] when (`lat`, `lon`) is invalid.
pub fn search_tiles(lat: f64, lon: f64, radius: usize) -> Result<Vec<TileId>, LookupError> {
    crate::raster::search_tiles(LayerGrid::QUARTER_DEG, lat, lon, radius)
}

/// Look up monthly precipitation in a single tile, falling back to the
/// nearest valid cell within [`PRECIP_LOOKUP_RADIUS`] (1 cell).
///
/// # Errors
/// [`LookupError`] for an invalid coordinate or a point outside the tile.
pub fn lookup(
    tile: &MonthlyPrecipTile,
    lat: f64,
    lon: f64,
) -> Result<Option<MonthlyPrecipLookup>, LookupError> {
    let found = nearest_in_tile(tile.as_raster(), lat, lon, PRECIP_LOOKUP_RADIUS, |r, i| {
        let p = r.pixel(i);
        !p.is_empty() && p.iter().all(|&v| v != NODATA)
    })?;
    Ok(found.map(|f| decode_lookup(f.raster, f.cell, lat, lon, f.distance_cells)))
}

/// Look up monthly precipitation across tile boundaries within `radius`
/// 0.25-degree cells.
///
/// # Errors
/// [`LookupError::InvalidCoordinate`] for an invalid coordinate.
pub fn lookup_across<'a>(
    tiles: impl Fn(TileId) -> Option<&'a MonthlyPrecipTile>,
    lat: f64,
    lon: f64,
    radius: usize,
) -> Result<Option<MonthlyPrecipLookup>, LookupError> {
    let found = nearest(
        LayerGrid::QUARTER_DEG,
        |id| tiles(id).map(MonthlyPrecipTile::as_raster),
        lat,
        lon,
        radius,
        |r, i| {
            let p = r.pixel(i);
            !p.is_empty() && p.iter().all(|&v| v != NODATA)
        },
    )?;
    Ok(found.map(|f| decode_lookup(f.raster, f.cell, lat, lon, f.distance_cells)))
}

fn decode_lookup(
    raster: &Raster,
    cell: GridCell,
    lat: f64,
    lon: f64,
    distance_cells: f64,
) -> MonthlyPrecipLookup {
    let grid = LayerGrid::QUARTER_DEG;
    let i = grid.index_in_tile(cell);
    let periods = MONTHLY_SLOTS
        .iter()
        .enumerate()
        .map(|(s, slot)| {
            let mut precip_mm = [0.0f64; 12];
            for (m, dst) in precip_mm.iter_mut().enumerate() {
                *dst = decode_precip(raster.get(i, precip_band(s, m))).unwrap_or(f64::NAN);
            }
            PeriodMonthlyPrecip {
                period: slot.period.label(),
                scenario: slot.scenario.id(),
                precip_mm,
            }
        })
        .collect();
    let round6 = |x: f64| (x * 1e6).round() / 1e6;
    MonthlyPrecipLookup {
        lat,
        lon,
        cell_lat: round6(grid.center_lat(cell)),
        cell_lon: round6(grid.center_lon(cell)),
        distance_cells: (distance_cells * 1e4).round() / 1e4,
        tile: raster.id().name(),
        periods,
    }
}

#[cfg(test)]
pub(crate) mod synthetic {
    //! A deterministic synthetic `monthly_precip` tile for tests (also the
    //! committed fixture `tests/fixtures/synthetic-monthly-precip-n50e000.tif`),
    //! aggregating complete cells of the synthetic climate tile onto the
    //! 0.25-degree grid.

    use super::{MonthlyPrecipTile, PRECIP_BANDS, PRECIP_TILE_LEN, encode_precip, precip_band};
    use crate::grid::{TILE_LEN, TileId};
    use crate::model::{NODATA, Variable};
    use crate::monthly::MONTHLY_SLOT_INDICES;
    use crate::raster::LayerGrid;

    pub(crate) fn monthly_precip() -> MonthlyPrecipTile {
        let id = TileId::parse("n50e000").expect("valid");
        let c = crate::tile::synthetic::tile(id);
        let grid = LayerGrid::QUARTER_DEG;
        let mut sums = vec![[0.0f64; PRECIP_BANDS]; PRECIP_TILE_LEN];
        let mut counts = vec![0u32; PRECIP_TILE_LEN];
        // Seasonal rainfall weights summing to 1.0 (winter-wet Mediterranean-like).
        let weights: [f64; 12] = [
            0.12, 0.10, 0.09, 0.08, 0.06, 0.04, 0.03, 0.04, 0.07, 0.11, 0.13, 0.13,
        ];
        for i in 0..TILE_LEN {
            if !c.is_complete(i) {
                continue;
            }
            let cell = id.cell(i);
            let q = grid
                .cell_of(cell.center_lat(), cell.center_lon())
                .expect("valid cell");
            let qi = grid.index_in_tile(q);
            counts[qi] += 1;
            for (s, &climate_slot) in MONTHLY_SLOT_INDICES.iter().enumerate() {
                let annual = Variable::Precip
                    .decode(c.get(
                        i,
                        crate::model::band(Variable::Precip.index(), climate_slot),
                    ))
                    .unwrap_or(0.0);
                for (m, &w) in weights.iter().enumerate() {
                    sums[qi][precip_band(s, m)] += annual * w;
                }
            }
        }
        let mut data = vec![NODATA; PRECIP_TILE_LEN * PRECIP_BANDS];
        for qi in 0..PRECIP_TILE_LEN {
            if counts[qi] == 0 {
                continue;
            }
            let n = f64::from(counts[qi]);
            for b in 0..PRECIP_BANDS {
                data[qi * PRECIP_BANDS + b] = encode_precip((sums[qi][b] / n) as f32);
            }
        }
        MonthlyPrecipTile::new(id, data).expect("valid monthly_precip tile")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_layout_and_encoding() {
        assert_eq!(precip_band(0, 0), 0);
        assert_eq!(precip_band(2, 11), 35);
        assert_eq!(encode_precip(42.6), 43);
        assert_eq!(encode_precip(-1.0), 0);
        assert_eq!(encode_precip(f32::NAN), NODATA);
        assert_eq!(decode_precip(43), Some(43.0));
        assert_eq!(decode_precip(NODATA), None);
    }

    #[test]
    fn lookup_and_coastal_fallback_work() {
        let tile = synthetic::monthly_precip();
        // Inland cell: exact hit.
        let inland = lookup(&tile, 45.95, 5.05).unwrap().unwrap();
        assert_eq!(inland.tile, "n50e000");
        assert_eq!(inland.distance_cells, 0.0);
        assert_eq!(inland.periods.len(), 3);
        assert!(inland.periods[0].precip_mm.iter().all(|&p| p > 0.0));
        // One 0.25-degree cell off the land block (block starts at lat 47.0 N -> row 12):
        // 47.125 N, 5.125 E is in row 11, adjacent to row 12 (distance = 1).
        let coast = lookup(&tile, 47.125, 5.125).unwrap().unwrap();
        assert_eq!(coast.distance_cells, 1.0);
        // Far ocean returns None.
        assert_eq!(lookup(&tile, 49.5, 9.5).unwrap(), None);
        // Cross-tile lookup works across the northern edge of n50e000 (corner at 49.875 N, 0.125 E).
        let across = lookup_across(
            |id| (id == tile.id()).then_some(&tile),
            50.125,
            0.125,
            PRECIP_LOOKUP_RADIUS,
        )
        .unwrap()
        .unwrap();
        assert_eq!(across.tile, "n50e000");
        assert_eq!(across.distance_cells, 1.0);
    }
}
