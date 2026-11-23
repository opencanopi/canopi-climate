//! The `water_balance` layer: static 1991–2020 Climatic Water Deficit (CWD,
//! mean & 1-in-10 drought year), wettest-month-anchored maximum consecutive
//! dry days (CDD), and peak-quarter Vapor Pressure Deficit (VPD), on the
//! 0.1-degree climate grid (100 × 100 cells per 10-degree tile, under
//! `water_balance/`).
//!
//! Bands (4 × `int16`, offset 0, `NODATA = -32768`):
//! - `0`: `cwd_mm` (mm/year, scale 1) — TerraClimate `def` mean annual CWD
//! - `1`: `cwd_p90_mm` (mm/year, scale 1) — TerraClimate `def` P90 annual CWD
//! - `2`: `cdd_max_days` (days, scale 1) — ERA5-Land daily mean annual max dry spell (`P < 1 mm`)
//! - `3`: `vpd_summer_kpa` (kPa, scale 0.01) — TerraClimate peak-quarter monthly-mean VPD
//!
//! No spatial search of its own: read at the climate lookup's cell ([`at_cell`]).
//! Method: `docs/decisions/0007-phenology-and-water-balance.md`.

use serde::Serialize;

use crate::grid::{Cell, TILE_LEN, TileId};
use crate::lookup::LookupError;
use crate::model::{NODATA, quantize};
use crate::raster::{LayerGrid, Raster};
use crate::tile::TileError;

/// Directory of the layer's tiles, relative to the manifest.
pub const DIR: &str = "water_balance";

/// Number of static bands per tile.
pub const WATER_BALANCE_BANDS: usize = 4;

/// A band of the `water_balance` layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaterBalanceBand {
    /// Mean annual Climatic Water Deficit (mm/year, scale 1).
    CwdMm = 0,
    /// 90th-percentile (1-in-10 drought year) annual CWD (mm/year, scale 1).
    CwdP90Mm = 1,
    /// Mean annual max consecutive dry days `P < 1 mm/day` (days, scale 1).
    CddMaxDays = 2,
    /// Peak-quarter (max circular 3-month rolling mean) VPD (kPa, scale 0.01).
    VpdSummerKpa = 3,
}

/// Bands in tile order.
pub const WATER_BALANCE_BAND_LIST: [WaterBalanceBand; WATER_BALANCE_BANDS] = [
    WaterBalanceBand::CwdMm,
    WaterBalanceBand::CwdP90Mm,
    WaterBalanceBand::CddMaxDays,
    WaterBalanceBand::VpdSummerKpa,
];

impl WaterBalanceBand {
    /// Manifest band id.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::CwdMm => "cwd_mm",
            Self::CwdP90Mm => "cwd_p90_mm",
            Self::CddMaxDays => "cdd_max_days",
            Self::VpdSummerKpa => "vpd_summer_kpa",
        }
    }

    /// Manifest unit.
    #[must_use]
    pub const fn unit(self) -> &'static str {
        match self {
            Self::CwdMm | Self::CwdP90Mm => "mm/year",
            Self::CddMaxDays => "days",
            Self::VpdSummerKpa => "kPa",
        }
    }

    /// Manifest scale factor.
    #[must_use]
    pub const fn scale(self) -> f32 {
        match self {
            Self::CwdMm | Self::CwdP90Mm | Self::CddMaxDays => 1.0,
            Self::VpdSummerKpa => 0.01,
        }
    }

    /// Manifest description.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::CwdMm => {
                "Mean annual Climatic Water Deficit (TerraClimate def, 1991-2020, mm/year)"
            }
            Self::CwdP90Mm => {
                "90th-percentile (1-in-10 drought year) annual Climatic Water Deficit (TerraClimate def, 1991-2020, mm/year)"
            }
            Self::CddMaxDays => {
                "Mean annual maximum consecutive dry days with precipitation < 1 mm/day (ERA5-Land daily 1991-2020, anchored at each cell's wettest month)"
            }
            Self::VpdSummerKpa => {
                "Peak-quarter (max circular 3-month rolling mean) climatological monthly-mean Vapor Pressure Deficit (TerraClimate vpd, 1991-2020, kPa)"
            }
        }
    }

    /// 0-based band index.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Encode a physical value; non-finite becomes [`NODATA`].
    #[must_use]
    pub fn encode(self, value: f32) -> i16 {
        if !value.is_finite() {
            return NODATA;
        }
        let clamped = match self {
            Self::CwdMm | Self::CwdP90Mm => value.clamp(0.0, 10_000.0),
            Self::CddMaxDays => value.clamp(0.0, 365.0),
            Self::VpdSummerKpa => value.clamp(0.0, 20.0),
        };
        quantize(clamped, self.scale(), 0.0)
    }

    /// Decode a stored value; [`NODATA`] becomes `None`.
    #[must_use]
    pub fn decode(self, stored: i16) -> Option<f64> {
        if stored == NODATA {
            return None;
        }
        match self {
            Self::CwdMm | Self::CwdP90Mm | Self::CddMaxDays => Some(f64::from(stored.max(0))),
            Self::VpdSummerKpa => Some(f64::from(stored.max(0)) / 100.0),
        }
    }
}

/// A decoded `water_balance` tile: [`TILE_LEN`] cells × [`WATER_BALANCE_BANDS`] bands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaterBalanceTile {
    raster: Raster,
}

impl WaterBalanceTile {
    /// Construct from pixel-interleaved values.
    ///
    /// # Errors
    /// [`TileError::Layout`] when `data` has the wrong length.
    pub fn new(id: TileId, data: Vec<i16>) -> Result<Self, TileError> {
        Ok(Self {
            raster: Raster::new(id, LayerGrid::CLIMATE, WATER_BALANCE_BANDS, data)?,
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
    /// [`TileError`] when the bytes are not a valid `water_balance` tile.
    pub fn decode(bytes: &[u8]) -> Result<Self, TileError> {
        Ok(Self {
            raster: Raster::decode(bytes, LayerGrid::CLIMATE, WATER_BALANCE_BANDS)?,
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

/// Decoded `water_balance` values at one climate cell.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WaterBalanceLookup {
    /// Centre latitude of the cell read.
    pub cell_lat: f64,
    /// Centre longitude of the cell read.
    pub cell_lon: f64,
    /// Tile name, e.g. `n50e000`.
    pub tile: String,
    /// Mean annual Climatic Water Deficit (mm/year).
    pub cwd_mm: u16,
    /// 90th-percentile (1-in-10 drought year) annual CWD (mm/year).
    pub cwd_p90_mm: u16,
    /// Mean annual max consecutive dry days `P < 1 mm/day` (days).
    pub cdd_max_days: u16,
    /// Peak-quarter climatological monthly-mean VPD (kPa).
    pub vpd_summer_kpa: f64,
}

/// Read `water_balance` at the cell holding (`cell_lat`, `cell_lon`).
/// `Ok(None)` when the cell has no data.
///
/// # Errors
/// [`LookupError`] for an invalid coordinate or a cell outside `tile`.
pub fn at_cell(
    tile: &WaterBalanceTile,
    cell_lat: f64,
    cell_lon: f64,
) -> Result<Option<WaterBalanceLookup>, LookupError> {
    let cell = Cell::of(cell_lat, cell_lon).ok_or(LookupError::InvalidCoordinate)?;
    if cell.tile() != tile.id() {
        return Err(LookupError::WrongTile {
            expected: cell.tile().name(),
            given: tile.id().name(),
        });
    }
    let i = cell.index_in_tile();
    let (Some(cwd), Some(cwd_p90), Some(cdd), Some(vpd)) = (
        WaterBalanceBand::CwdMm.decode(tile.get(i, WaterBalanceBand::CwdMm.index())),
        WaterBalanceBand::CwdP90Mm.decode(tile.get(i, WaterBalanceBand::CwdP90Mm.index())),
        WaterBalanceBand::CddMaxDays.decode(tile.get(i, WaterBalanceBand::CddMaxDays.index())),
        WaterBalanceBand::VpdSummerKpa.decode(tile.get(i, WaterBalanceBand::VpdSummerKpa.index())),
    ) else {
        return Ok(None);
    };
    let round6 = |x: f64| (x * 1e6).round() / 1e6;
    Ok(Some(WaterBalanceLookup {
        cell_lat: round6(cell.center_lat()),
        cell_lon: round6(cell.center_lon()),
        tile: tile.id().name(),
        cwd_mm: cwd.round() as u16,
        cwd_p90_mm: cwd_p90.round() as u16,
        cdd_max_days: cdd.round() as u16,
        vpd_summer_kpa: vpd,
    }))
}

/// Synthetic `water_balance` tile matching `tile::synthetic::tile(n50e000)` for tests.
#[cfg(test)]
pub mod synthetic {
    use super::*;
    use crate::tile::synthetic as climate_syn;

    /// Synthetic `water_balance` tile for `n50e000`.
    #[must_use]
    pub fn water_balance() -> WaterBalanceTile {
        let id = TileId::parse("n50e000").expect("valid tile id");
        let climate = climate_syn::tile(id);
        let mut data = vec![NODATA; TILE_LEN * WATER_BALANCE_BANDS];
        for i in 0..TILE_LEN {
            if !climate.is_complete(i) {
                continue;
            }
            let base = i * WATER_BALANCE_BANDS;
            data[base + WaterBalanceBand::CwdMm.index()] = WaterBalanceBand::CwdMm.encode(340.0);
            data[base + WaterBalanceBand::CwdP90Mm.index()] =
                WaterBalanceBand::CwdP90Mm.encode(465.0);
            data[base + WaterBalanceBand::CddMaxDays.index()] =
                WaterBalanceBand::CddMaxDays.encode(22.0);
            data[base + WaterBalanceBand::VpdSummerKpa.index()] =
                WaterBalanceBand::VpdSummerKpa.encode(1.05);
        }
        WaterBalanceTile::new(id, data).expect("valid synthetic water_balance tile")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_cell_decodes_water_balance_and_skips_sea() {
        let tile = synthetic::water_balance();
        let found = at_cell(&tile, 45.25, 3.25)
            .unwrap()
            .expect("land cell has water_balance");
        assert_eq!(found.cwd_mm, 340);
        assert_eq!(found.cwd_p90_mm, 465);
        assert_eq!(found.cdd_max_days, 22);
        assert!((found.vpd_summer_kpa - 1.05).abs() < 1e-9);
        assert!(at_cell(&tile, 40.55, 0.55).unwrap().is_none());
    }
}
