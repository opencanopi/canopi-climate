//! The `frost_calendar` layer: static hemisphere-aware frost dates, frost-free
//! season length, frost-year percentage, and false-spring killing-freeze risk
//! (1991–2020 ERA5-Land daily), on the 0.1-degree climate grid (100 × 100 cells
//! per 10-degree tile, under `frost_calendar/`).
//!
//! Bands (6 × `int16`, scale 1, offset 0, `NODATA = -32768`):
//! - `0`: `last_spring_frost_doy` (`1..=365`, `0` = frost-free, `366` = year-round frost)
//! - `1`: `last_spring_frost_p90_doy` (`1..=365`, `0` = frost-free, `366` = year-round frost)
//! - `2`: `first_autumn_frost_doy` (`1..=365`, `0` = frost-free, `366` = year-round frost)
//! - `3`: `frost_free_days` (`0..=365`; `365` = frost-free, `0` = year-round frost)
//! - `4`: `frost_year_pct` (`0..=100`, % of years with `Tmin <= 0 °C`)
//! - `5`: `false_spring_pct` (`0..=100`, % of years with `Tmin <= -2 °C` after `>= 150` GDD base 5 °C)
//!
//! Sentinels `0` and `366` are distinct from `NODATA` (`-32768`). No spatial
//! search of its own: read at the climate lookup's cell ([`at_cell`]).
//! Method: `docs/decisions/0006-tail-extremes-frost-calendar-analogues.md`.

use serde::Serialize;

use crate::grid::{Cell, TILE_LEN, TileId};
use crate::lookup::LookupError;
use crate::model::NODATA;
use crate::raster::{LayerGrid, Raster};
use crate::tile::TileError;

/// Directory of the layer's tiles, relative to the manifest.
pub const DIR: &str = "frost_calendar";

/// Number of static bands per tile.
pub const FROST_BANDS: usize = 6;

/// Stored DOY sentinel meaning frost-free (no frost in that half-year / 0% frost years).
pub const FROST_FREE_SENTINEL: i16 = 0;

/// Stored DOY sentinel meaning year-round frost (`frost_free_days == 0`).
pub const YEAR_ROUND_FROST_SENTINEL: i16 = 366;

/// A band of the `frost_calendar` layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrostBand {
    /// Conditional median last spring frost DOY (`1..=365`, `0` = frost-free, `366` = year-round).
    LastSpringFrostDoy = 0,
    /// Conditional 90th-percentile (1-in-10 late) last spring frost DOY (`1..=365`, `0`, `366`).
    LastSpringFrostP90Doy = 1,
    /// Conditional median first autumn frost DOY (`1..=365`, `0` = frost-free, `366` = year-round).
    FirstAutumnFrostDoy = 2,
    /// Unconditional median frost-free season length (`0..=365`).
    FrostFreeDays = 3,
    /// Percentage of years (1991–2020) with at least one day `Tmin <= 0 °C` (`0..=100`).
    FrostYearPct = 4,
    /// Percentage of years with `Tmin <= -2 °C` after `>= 150` GDD (base 5 °C) from thermal winter.
    FalseSpringPct = 5,
}

/// Bands in tile order.
pub const FROST_BAND_LIST: [FrostBand; FROST_BANDS] = [
    FrostBand::LastSpringFrostDoy,
    FrostBand::LastSpringFrostP90Doy,
    FrostBand::FirstAutumnFrostDoy,
    FrostBand::FrostFreeDays,
    FrostBand::FrostYearPct,
    FrostBand::FalseSpringPct,
];

impl FrostBand {
    /// Manifest band id.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::LastSpringFrostDoy => "last_spring_frost_doy",
            Self::LastSpringFrostP90Doy => "last_spring_frost_p90_doy",
            Self::FirstAutumnFrostDoy => "first_autumn_frost_doy",
            Self::FrostFreeDays => "frost_free_days",
            Self::FrostYearPct => "frost_year_pct",
            Self::FalseSpringPct => "false_spring_pct",
        }
    }

    /// Manifest unit.
    #[must_use]
    pub const fn unit(self) -> &'static str {
        match self {
            Self::LastSpringFrostDoy | Self::LastSpringFrostP90Doy | Self::FirstAutumnFrostDoy => {
                "doy"
            }
            Self::FrostFreeDays => "days",
            Self::FrostYearPct | Self::FalseSpringPct => "%",
        }
    }

    /// Manifest description.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::LastSpringFrostDoy => {
                "Median calendar day-of-year (1-365) of last spring frost (Tmin <= 0 °C) in frost years, anchored per pixel to thermal winter; 0 = frost-free, 366 = year-round frost"
            }
            Self::LastSpringFrostP90Doy => {
                "90th-percentile (1-in-10 late) calendar DOY (1-365) of last spring frost in frost years; 0 = frost-free, 366 = year-round frost"
            }
            Self::FirstAutumnFrostDoy => {
                "Median calendar DOY (1-365) of first autumn frost (Tmin <= 0 °C) in frost years; 0 = frost-free, 366 = year-round frost"
            }
            Self::FrostFreeDays => {
                "Unconditional median frost-free season length (0-365 days; 365 = frost-free, 0 = year-round frost)"
            }
            Self::FrostYearPct => {
                "Percentage of years (1991-2020, 0-100) with at least one day Tmin <= 0 °C"
            }
            Self::FalseSpringPct => {
                "Percentage of years (1991-2020, 0-100) with a killing freeze (Tmin <= -2 °C) after >= 150 GDD (base 5 °C) from thermal winter"
            }
        }
    }

    /// 0-based band index.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// Classification of a cell's frost regime derived from stored sentinels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrostRegime {
    /// Frost-free (`frost_year_pct == 0`, DOY sentinels = `0`).
    FrostFree,
    /// Seasonal or intermittent frost (has a frost-free window and frost occurs in `> 0%` of years).
    Seasonal,
    /// Year-round frost (`frost_free_days == 0`, DOY sentinels = `366`).
    YearRound,
}

/// A decoded `frost_calendar` tile: [`TILE_LEN`] cells × [`FROST_BANDS`] bands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrostCalendarTile {
    raster: Raster,
}

impl FrostCalendarTile {
    /// Construct from pixel-interleaved values.
    ///
    /// # Errors
    /// [`TileError::Layout`] when `data` has the wrong length.
    pub fn new(id: TileId, data: Vec<i16>) -> Result<Self, TileError> {
        Ok(Self {
            raster: Raster::new(id, LayerGrid::CLIMATE, FROST_BANDS, data)?,
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
    /// [`TileError`] when the bytes are not a valid `frost_calendar` tile.
    pub fn decode(bytes: &[u8]) -> Result<Self, TileError> {
        Ok(Self {
            raster: Raster::decode(bytes, LayerGrid::CLIMATE, FROST_BANDS)?,
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

    /// Whether a cell has all 6 bands present (not `NODATA`).
    #[must_use]
    pub fn has(&self, index: usize) -> bool {
        let p = self.raster.pixel(index);
        !p.is_empty() && p.iter().all(|&v| v != NODATA)
    }

    /// Number of cells with all 6 bands present.
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

/// Decoded `frost_calendar` values at one climate cell.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FrostCalendarLookup {
    /// Centre latitude of the cell read.
    pub cell_lat: f64,
    /// Centre longitude of the cell read.
    pub cell_lon: f64,
    /// Tile name, e.g. `n50e000`.
    pub tile: String,
    /// Frost regime (`frost_free`, `seasonal`, or `year_round`).
    pub regime: FrostRegime,
    /// Median last spring frost calendar DOY (`1..=365`), or `None` when frost-free/year-round.
    pub last_spring_frost_doy: Option<u16>,
    /// 90th-percentile last spring frost calendar DOY (`1..=365`), or `None`.
    pub last_spring_frost_p90_doy: Option<u16>,
    /// Median first autumn frost calendar DOY (`1..=365`), or `None`.
    pub first_autumn_frost_doy: Option<u16>,
    /// Unconditional median frost-free season length (`0..=365`).
    pub frost_free_days: u16,
    /// Percentage of years (1991–2020) with at least one frost day (`0..=100`).
    pub frost_year_pct: u8,
    /// Percentage of years (1991–2020) with a false spring (`0..=100`).
    pub false_spring_pct: u8,
}

fn decode_doy(raw: i16) -> Option<u16> {
    if (1..=365).contains(&raw) {
        u16::try_from(raw).ok()
    } else {
        None
    }
}

/// Read `frost_calendar` at the cell holding (`cell_lat`, `cell_lon`).
/// `Ok(None)` when the cell has no data (`NODATA`).
///
/// # Errors
/// [`LookupError`] for an invalid coordinate or a cell outside `tile`.
pub fn at_cell(
    tile: &FrostCalendarTile,
    cell_lat: f64,
    cell_lon: f64,
) -> Result<Option<FrostCalendarLookup>, LookupError> {
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
    let raw_ls = tile.get(i, FrostBand::LastSpringFrostDoy.index());
    let raw_ls90 = tile.get(i, FrostBand::LastSpringFrostP90Doy.index());
    let raw_fa = tile.get(i, FrostBand::FirstAutumnFrostDoy.index());
    let ff =
        u16::try_from(tile.get(i, FrostBand::FrostFreeDays.index()).clamp(0, 365)).unwrap_or(0);
    let fy = u8::try_from(tile.get(i, FrostBand::FrostYearPct.index()).clamp(0, 100)).unwrap_or(0);
    let fs =
        u8::try_from(tile.get(i, FrostBand::FalseSpringPct.index()).clamp(0, 100)).unwrap_or(0);

    let regime = if fy == 0 {
        FrostRegime::FrostFree
    } else if ff == 0 || raw_ls == YEAR_ROUND_FROST_SENTINEL {
        FrostRegime::YearRound
    } else {
        FrostRegime::Seasonal
    };

    let round6 = |x: f64| (x * 1e6).round() / 1e6;
    Ok(Some(FrostCalendarLookup {
        cell_lat: round6(cell.center_lat()),
        cell_lon: round6(cell.center_lon()),
        tile: tile.id().name(),
        regime,
        last_spring_frost_doy: decode_doy(raw_ls),
        last_spring_frost_p90_doy: decode_doy(raw_ls90),
        first_autumn_frost_doy: decode_doy(raw_fa),
        frost_free_days: ff,
        frost_year_pct: fy,
        false_spring_pct: fs,
    }))
}

#[cfg(test)]
pub(crate) mod synthetic {
    use super::{
        FROST_BANDS, FROST_FREE_SENTINEL, FrostBand, FrostCalendarTile, YEAR_ROUND_FROST_SENTINEL,
    };
    use crate::grid::{TILE_CELLS, TILE_LEN, TileId};
    use crate::model::NODATA;

    pub(crate) fn frost_calendar() -> FrostCalendarTile {
        let id = TileId::parse("n50e000").expect("valid");
        let c = crate::tile::synthetic::tile(id);
        let mut data = vec![NODATA; TILE_LEN * FROST_BANDS];
        for i in 0..TILE_LEN {
            if !c.is_complete(i) || i % TILE_CELLS == 60 {
                continue;
            }
            let base = i * FROST_BANDS;
            // Vary regime by column mod 3 for test coverage (seasonal, frost-free, year-round).
            match i % TILE_CELLS {
                51 => {
                    // Frost-free sentinel (0)
                    data[base + FrostBand::LastSpringFrostDoy.index()] = FROST_FREE_SENTINEL;
                    data[base + FrostBand::LastSpringFrostP90Doy.index()] = FROST_FREE_SENTINEL;
                    data[base + FrostBand::FirstAutumnFrostDoy.index()] = FROST_FREE_SENTINEL;
                    data[base + FrostBand::FrostFreeDays.index()] = 365;
                    data[base + FrostBand::FrostYearPct.index()] = 0;
                    data[base + FrostBand::FalseSpringPct.index()] = 0;
                }
                52 => {
                    // Year-round frost sentinel (366)
                    data[base + FrostBand::LastSpringFrostDoy.index()] = YEAR_ROUND_FROST_SENTINEL;
                    data[base + FrostBand::LastSpringFrostP90Doy.index()] =
                        YEAR_ROUND_FROST_SENTINEL;
                    data[base + FrostBand::FirstAutumnFrostDoy.index()] = YEAR_ROUND_FROST_SENTINEL;
                    data[base + FrostBand::FrostFreeDays.index()] = 0;
                    data[base + FrostBand::FrostYearPct.index()] = 100;
                    data[base + FrostBand::FalseSpringPct.index()] = 0;
                }
                _ => {
                    // Seasonal NH temperate frost
                    data[base + FrostBand::LastSpringFrostDoy.index()] = 105;
                    data[base + FrostBand::LastSpringFrostP90Doy.index()] = 120;
                    data[base + FrostBand::FirstAutumnFrostDoy.index()] = 305;
                    data[base + FrostBand::FrostFreeDays.index()] = 200;
                    data[base + FrostBand::FrostYearPct.index()] = 100;
                    data[base + FrostBand::FalseSpringPct.index()] = 23;
                }
            }
        }
        FrostCalendarTile::new(id, data).expect("valid tile")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_seasonal_frost_free_year_round_and_sh_wrap() {
        let tile = synthetic::frost_calendar();
        // Seasonal cell (col 50 -> lon 5.05)
        let s = at_cell(&tile, 45.95, 5.05).unwrap().unwrap();
        assert_eq!(s.regime, FrostRegime::Seasonal);
        assert_eq!(s.last_spring_frost_doy, Some(105));
        assert_eq!(s.last_spring_frost_p90_doy, Some(120));
        assert_eq!(s.first_autumn_frost_doy, Some(305));
        assert_eq!(s.frost_free_days, 200);
        assert_eq!(s.frost_year_pct, 100);
        assert_eq!(s.false_spring_pct, 23);

        // Frost-free sentinel (col 51 -> lon 5.15)
        let ff = at_cell(&tile, 45.95, 5.15).unwrap().unwrap();
        assert_eq!(ff.regime, FrostRegime::FrostFree);
        assert_eq!(ff.last_spring_frost_doy, None);
        assert_eq!(ff.first_autumn_frost_doy, None);
        assert_eq!(ff.frost_free_days, 365);
        assert_eq!(ff.frost_year_pct, 0);

        // Year-round frost sentinel (col 52 -> lon 5.25)
        let yr = at_cell(&tile, 45.95, 5.25).unwrap().unwrap();
        assert_eq!(yr.regime, FrostRegime::YearRound);
        assert_eq!(yr.last_spring_frost_doy, None);
        assert_eq!(yr.frost_free_days, 0);
        assert_eq!(yr.frost_year_pct, 100);
        assert_eq!(yr.false_spring_pct, 0);

        // Southern Hemisphere wrap (e.g. Canberra: last spring frost DOY 280, first autumn DOY 130)
        let sh_id = TileId::of(-35.25, 149.15).unwrap();
        let mut sh_data = vec![NODATA; TILE_LEN * FROST_BANDS];
        let idx = Cell::of(-35.25, 149.15).unwrap().index_in_tile();
        let base = idx * FROST_BANDS;
        sh_data[base + FrostBand::LastSpringFrostDoy.index()] = 280;
        sh_data[base + FrostBand::LastSpringFrostP90Doy.index()] = 298;
        sh_data[base + FrostBand::FirstAutumnFrostDoy.index()] = 130;
        sh_data[base + FrostBand::FrostFreeDays.index()] = 215;
        sh_data[base + FrostBand::FrostYearPct.index()] = 97;
        sh_data[base + FrostBand::FalseSpringPct.index()] = 17;
        let sh_tile = FrostCalendarTile::new(sh_id, sh_data).unwrap();
        let sh = at_cell(&sh_tile, -35.25, 149.15).unwrap().unwrap();
        assert_eq!(sh.regime, FrostRegime::Seasonal);
        assert_eq!(sh.last_spring_frost_doy, Some(280));
        assert_eq!(sh.first_autumn_frost_doy, Some(130));
        assert_eq!(sh.frost_free_days, 215);

        // Ocean -> None
        assert_eq!(at_cell(&tile, 49.5, 9.5).unwrap(), None);
    }
}
