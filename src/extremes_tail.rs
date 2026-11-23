//! The `extremes_tail` layer: static 1-in-10-year tail extreme offsets,
//! annual heat-stress days (`Tmax >= 35 °C`), and recent realized warming
//! (2015–2025 vs 1991–2020), on the 0.1-degree climate grid (100 × 100 cells
//! per 10-degree tile, under `extremes_tail/`).
//!
//! Bands (4 × `int16`, scale 0.1, offset 0, `NODATA = -32768`):
//! - `0`: `tmin_p10_gap` (°C, `<= 0.0`) — 1-in-10-year coldest-night offset below `tmin_extreme`
//! - `1`: `tmax_p90_gap` (°C, `>= 0.0`) — 1-in-10-year hottest-day offset above `tmax_extreme`
//! - `2`: `heat_days_35c` (days/year, `>= 0.0`) — mean annual days with `Tmax >= 35.0 °C`
//! - `3`: `recent_trend_c` (°C) — `mean(Tmean, 2015–2025) - mean(Tmean, 1991–2020)`
//!
//! No spatial search of its own: read at the climate lookup's cell ([`at_cell`]).
//! Method: `docs/decisions/0006-tail-extremes-frost-calendar-analogues.md`.

use serde::Serialize;

use crate::extremes::{ExtremesLookup, usda_zone};
use crate::grid::{Cell, TILE_LEN, TileId};
use crate::lookup::LookupError;
use crate::model::{NODATA, quantize};
use crate::raster::{LayerGrid, Raster};
use crate::tile::TileError;

/// Directory of the layer's tiles, relative to the manifest.
pub const DIR: &str = "extremes_tail";

/// Number of static bands per tile.
pub const TAIL_BANDS: usize = 4;

/// Stored value = round(value / [`SCALE`]).
pub const SCALE: f32 = 0.1;

/// A band of the `extremes_tail` layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TailBand {
    /// 1-in-10-year coldest-night offset below `tmin_extreme` (°C, `<= 0`).
    TminP10Gap = 0,
    /// 1-in-10-year hottest-day offset above `tmax_extreme` (°C, `>= 0`).
    TmaxP90Gap = 1,
    /// Mean annual days with `Tmax >= 35.0 °C` (days/year).
    HeatDays35c = 2,
    /// Realized warming: `mean(Tmean, 2015-2025) - mean(Tmean, 1991-2020)` (°C).
    RecentTrendC = 3,
}

/// Bands in tile order.
pub const TAIL_BAND_LIST: [TailBand; TAIL_BANDS] = [
    TailBand::TminP10Gap,
    TailBand::TmaxP90Gap,
    TailBand::HeatDays35c,
    TailBand::RecentTrendC,
];

impl TailBand {
    /// Manifest band id.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::TminP10Gap => "tmin_p10_gap",
            Self::TmaxP90Gap => "tmax_p90_gap",
            Self::HeatDays35c => "heat_days_35c",
            Self::RecentTrendC => "recent_trend_c",
        }
    }

    /// Manifest unit.
    #[must_use]
    pub const fn unit(self) -> &'static str {
        match self {
            Self::HeatDays35c => "days/year",
            Self::TminP10Gap | Self::TmaxP90Gap | Self::RecentTrendC => "°C",
        }
    }

    /// Manifest description.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::TminP10Gap => {
                "1-in-10-year coldest-night tail offset below tmin_extreme (ERA5-Land 1991-2020 P10(TNn) - mean(TNn), <= 0)"
            }
            Self::TmaxP90Gap => {
                "1-in-10-year hottest-day tail offset above tmax_extreme (ERA5-Land 1991-2020 P90(TXx) - mean(TXx), >= 0)"
            }
            Self::HeatDays35c => {
                "Mean annual count of days with daily maximum temperature >= 35.0 °C (ERA5-Land 1991-2020)"
            }
            Self::RecentTrendC => {
                "Realized warming shift: mean 2015-2025 temperature minus 1991-2020 baseline (ERA5-Land daily temperature_2m)"
            }
        }
    }

    /// 0-based band index.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Encode a physical value at scale 0.1; non-finite becomes [`NODATA`].
    #[must_use]
    pub fn encode(self, value: f32) -> i16 {
        quantize(value, SCALE, 0.0)
    }

    /// Decode a stored value; [`NODATA`] becomes `None`.
    #[must_use]
    pub fn decode(self, stored: i16) -> Option<f64> {
        (stored != NODATA).then(|| f64::from(stored) / 10.0)
    }
}

/// A decoded `extremes_tail` tile: [`TILE_LEN`] cells × [`TAIL_BANDS`] bands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtremesTailTile {
    raster: Raster,
}

impl ExtremesTailTile {
    /// Construct from pixel-interleaved values.
    ///
    /// # Errors
    /// [`TileError::Layout`] when `data` has the wrong length.
    pub fn new(id: TileId, data: Vec<i16>) -> Result<Self, TileError> {
        Ok(Self {
            raster: Raster::new(id, LayerGrid::CLIMATE, TAIL_BANDS, data)?,
        })
    }

    /// Underlying generic raster.
    #[must_use]
    pub const fn as_raster(&self) -> &Raster {
        &self.raster
    }

    /// Decode a tile file.
    ///
    /// # Errors
    /// [`TileError`] when the bytes are not a valid `extremes_tail` tile.
    pub fn decode(bytes: &[u8]) -> Result<Self, TileError> {
        Ok(Self {
            raster: Raster::decode(bytes, LayerGrid::CLIMATE, TAIL_BANDS)?,
        })
    }

    /// Tile identifier.
    #[must_use]
    pub const fn id(&self) -> TileId {
        self.raster.id()
    }

    /// One stored sample; [`NODATA`] outside the tile.
    #[must_use]
    pub fn get(&self, index: usize, band: usize) -> i16 {
        self.raster.get(index, band)
    }

    /// Whether a cell has all 4 bands present.
    #[must_use]
    pub fn has(&self, index: usize) -> bool {
        let p = self.raster.pixel(index);
        !p.is_empty() && p.iter().all(|&v| v != NODATA)
    }

    /// Number of cells with all 4 bands present.
    #[must_use]
    pub fn cells(&self) -> usize {
        (0..TILE_LEN).filter(|&i| self.has(i)).count()
    }

    /// Raw pixel-interleaved samples.
    #[must_use]
    pub fn data(&self) -> &[i16] {
        self.raster.data()
    }
}

/// Decoded `extremes_tail` values at one climate cell.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExtremesTailLookup {
    /// Centre latitude of the cell read.
    pub cell_lat: f64,
    /// Centre longitude of the cell read.
    pub cell_lon: f64,
    /// Tile name, e.g. `n50e000`.
    pub tile: String,
    /// 1-in-10-year coldest-night offset below `tmin_extreme` (°C, `<= 0`).
    pub tmin_p10_gap_c: f64,
    /// 1-in-10-year hottest-day offset above `tmax_extreme` (°C, `>= 0`).
    pub tmax_p90_gap_c: f64,
    /// Mean annual days with `Tmax >= 35.0 °C` (days/year).
    pub heat_days_35c: f64,
    /// Realized warming shift 2015–2025 vs 1991–2020 (°C).
    pub recent_trend_c: f64,
}

/// 1-in-10-year extremes for one climate slot, derived by combining
/// [`ExtremesTailLookup`] gaps with [`ExtremesLookup`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PeriodTailExtremes {
    /// `1991-2020`, `2041-2070` or `2071-2099`.
    pub period: &'static str,
    /// `historical`, `ssp126`, `ssp245`, `ssp370` or `ssp585`.
    pub scenario: &'static str,
    /// 1-in-10-year coldest night (°C): `tmin_extreme_c + tmin_p10_gap_c`.
    pub tmin_p10_c: f64,
    /// 1-in-10-year hottest day (°C): `tmax_extreme_c + tmax_p90_gap_c`.
    pub tmax_p90_c: f64,
    /// USDA hardiness zone of `tmin_p10_c`.
    pub usda_zone_p10: String,
}

/// Combine static tail offsets with per-slot typical extremes to obtain
/// 1-in-10-year coldest night and hottest day for every climate slot.
#[must_use]
pub fn combine_with_extremes(
    tail: &ExtremesTailLookup,
    extremes: &ExtremesLookup,
) -> Vec<PeriodTailExtremes> {
    let round1 = |x: f64| (x * 10.0).round() / 10.0;
    extremes
        .periods
        .iter()
        .map(|p| {
            let tmin_p10 = round1(p.tmin_extreme_c + tail.tmin_p10_gap_c);
            let tmax_p90 = round1(p.tmax_extreme_c + tail.tmax_p90_gap_c);
            PeriodTailExtremes {
                period: p.period,
                scenario: p.scenario,
                tmin_p10_c: tmin_p10,
                tmax_p90_c: tmax_p90,
                usda_zone_p10: usda_zone(tmin_p10),
            }
        })
        .collect()
}

/// Read `extremes_tail` at the cell holding (`cell_lat`, `cell_lon`).
/// `Ok(None)` when the cell has no data.
///
/// # Errors
/// [`LookupError`] for an invalid coordinate or a cell outside `tile`.
pub fn at_cell(
    tile: &ExtremesTailTile,
    cell_lat: f64,
    cell_lon: f64,
) -> Result<Option<ExtremesTailLookup>, LookupError> {
    let cell = Cell::of(cell_lat, cell_lon).ok_or(LookupError::InvalidCoordinate)?;
    if cell.tile() != tile.id() {
        return Err(LookupError::WrongTile {
            expected: cell.tile().name(),
            given: tile.id().name(),
        });
    }
    let i = cell.index_in_tile();
    if !tile.has(i) {
        return Ok(None);
    }
    let dec = |b: TailBand| b.decode(tile.get(i, b.index())).unwrap_or(f64::NAN);
    let round6 = |x: f64| (x * 1e6).round() / 1e6;
    Ok(Some(ExtremesTailLookup {
        cell_lat: round6(cell.center_lat()),
        cell_lon: round6(cell.center_lon()),
        tile: tile.id().name(),
        tmin_p10_gap_c: dec(TailBand::TminP10Gap),
        tmax_p90_gap_c: dec(TailBand::TmaxP90Gap),
        heat_days_35c: dec(TailBand::HeatDays35c),
        recent_trend_c: dec(TailBand::RecentTrendC),
    }))
}

#[cfg(test)]
pub(crate) mod synthetic {
    use super::{ExtremesTailTile, TAIL_BANDS, TailBand};
    use crate::grid::{TILE_CELLS, TILE_LEN, TileId};
    use crate::model::NODATA;

    pub(crate) fn extremes_tail() -> ExtremesTailTile {
        let id = TileId::parse("n50e000").expect("valid");
        let c = crate::tile::synthetic::tile(id);
        let mut data = vec![NODATA; TILE_LEN * TAIL_BANDS];
        for i in 0..TILE_LEN {
            if !c.is_complete(i) || i % TILE_CELLS == 60 {
                continue;
            }
            let base = i * TAIL_BANDS;
            data[base + TailBand::TminP10Gap.index()] = TailBand::TminP10Gap.encode(-3.5);
            data[base + TailBand::TmaxP90Gap.index()] = TailBand::TmaxP90Gap.encode(2.8);
            data[base + TailBand::HeatDays35c.index()] = TailBand::HeatDays35c.encode(4.2);
            data[base + TailBand::RecentTrendC.index()] = TailBand::RecentTrendC.encode(0.9);
        }
        ExtremesTailTile::new(id, data).expect("valid tile")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extremes::PeriodExtremes;

    #[test]
    fn at_cell_and_combine_with_extremes_work() {
        let tile = synthetic::extremes_tail();
        assert!(tile.cells() > 0);
        let found = at_cell(&tile, 45.95, 5.05).unwrap().unwrap();
        assert_eq!(found.tile, "n50e000");
        assert_eq!(found.tmin_p10_gap_c, -3.5);
        assert_eq!(found.tmax_p90_gap_c, 2.8);
        assert_eq!(found.heat_days_35c, 4.2);
        assert_eq!(found.recent_trend_c, 0.9);

        let ext = ExtremesLookup {
            cell_lat: found.cell_lat,
            cell_lon: found.cell_lon,
            tile: found.tile.clone(),
            periods: vec![PeriodExtremes {
                period: "1991-2020",
                scenario: "historical",
                tmin_extreme_c: -8.3,
                tmax_extreme_c: 33.1,
                usda_zone: "8b".into(),
            }],
        };
        let combined = combine_with_extremes(&found, &ext);
        assert_eq!(combined.len(), 1);
        assert_eq!(combined[0].tmin_p10_c, -11.8);
        assert_eq!(combined[0].tmax_p90_c, 35.9);
        assert_eq!(combined[0].usda_zone_p10, "8a");

        // Ocean cell -> None; wrong tile -> WrongTile.
        assert_eq!(at_cell(&tile, 49.5, 9.5).unwrap(), None);
        assert!(matches!(
            at_cell(&tile, 10.0, 10.0),
            Err(LookupError::WrongTile { .. })
        ));
    }
}
