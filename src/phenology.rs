//! The `phenology` layer: static 1991–2020 Fishman (1987) Dynamic Model
//! Chill Portions across a 5-point warming sensitivity curve
//! (`ΔT ∈ {0, +1, +2, +3, +4 °C}`), on the 0.1-degree climate grid
//! (100 × 100 cells per 10-degree tile, under `phenology/`).
//!
//! Bands (5 × `int16`, scale 1, offset 0, `NODATA = -32768`):
//! - `0`: `chill_portions_0c` (CP, 1991–2020 baseline)
//! - `1`: `chill_portions_1c` (CP, `+1 °C` uniform daily warming)
//! - `2`: `chill_portions_2c` (CP, `+2 °C` uniform daily warming)
//! - `3`: `chill_portions_3c` (CP, `+3 °C` uniform daily warming)
//! - `4`: `chill_portions_4c` (CP, `+4 °C` uniform daily warming)
//!
//! No spatial search of its own: read at the climate lookup's cell ([`at_cell`]).
//! Method: `docs/decisions/0007-phenology-and-water-balance.md`.

use serde::Serialize;

use crate::grid::{Cell, TILE_LEN, TileId};
use crate::lookup::LookupError;
use crate::model::{NODATA, quantize};
use crate::monthly::MonthlyLookup;
use crate::raster::{LayerGrid, Raster};
use crate::tile::TileError;

/// Directory of the layer's tiles, relative to the manifest.
pub const DIR: &str = "phenology";

/// Number of static bands per tile.
pub const PHENOLOGY_BANDS: usize = 5;

/// Stored value = round(value / [`SCALE`]).
pub const SCALE: f32 = 1.0;

/// A band of the `phenology` layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PhenologyBand {
    /// Baseline 1991–2020 mean annual Chill Portions (`ΔT = 0 °C`).
    ChillPortions0c = 0,
    /// Mean annual Chill Portions under `+1 °C` daily warming.
    ChillPortions1c = 1,
    /// Mean annual Chill Portions under `+2 °C` daily warming.
    ChillPortions2c = 2,
    /// Mean annual Chill Portions under `+3 °C` daily warming.
    ChillPortions3c = 3,
    /// Mean annual Chill Portions under `+4 °C` daily warming.
    ChillPortions4c = 4,
}

/// Bands in tile order.
pub const PHENOLOGY_BAND_LIST: [PhenologyBand; PHENOLOGY_BANDS] = [
    PhenologyBand::ChillPortions0c,
    PhenologyBand::ChillPortions1c,
    PhenologyBand::ChillPortions2c,
    PhenologyBand::ChillPortions3c,
    PhenologyBand::ChillPortions4c,
];

impl PhenologyBand {
    /// Manifest band id.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::ChillPortions0c => "chill_portions_0c",
            Self::ChillPortions1c => "chill_portions_1c",
            Self::ChillPortions2c => "chill_portions_2c",
            Self::ChillPortions3c => "chill_portions_3c",
            Self::ChillPortions4c => "chill_portions_4c",
        }
    }

    /// Manifest unit.
    #[must_use]
    pub const fn unit(self) -> &'static str {
        "CP"
    }

    /// Manifest description.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::ChillPortions0c => {
                "1991-2020 mean annual Fishman (1987) Dynamic Model Chill Portions (baseline, dT = 0 C)"
            }
            Self::ChillPortions1c => {
                "Mean annual Fishman Dynamic Model Chill Portions under +1 C uniform daily warming"
            }
            Self::ChillPortions2c => {
                "Mean annual Fishman Dynamic Model Chill Portions under +2 C uniform daily warming"
            }
            Self::ChillPortions3c => {
                "Mean annual Fishman Dynamic Model Chill Portions under +3 C uniform daily warming"
            }
            Self::ChillPortions4c => {
                "Mean annual Fishman Dynamic Model Chill Portions under +4 C uniform daily warming"
            }
        }
    }

    /// 0-based band index.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Encode a Chill Portions value (clamped to `[0, 250]`); non-finite becomes [`NODATA`].
    #[must_use]
    pub fn encode(self, value: f32) -> i16 {
        if !value.is_finite() {
            return NODATA;
        }
        quantize(value.clamp(0.0, 250.0), SCALE, 0.0)
    }

    /// Decode a stored value; [`NODATA`] becomes `None`.
    #[must_use]
    pub fn decode(self, stored: i16) -> Option<u16> {
        (stored != NODATA).then(|| u16::try_from(stored.max(0)).unwrap_or(0))
    }
}

/// A decoded `phenology` tile: [`TILE_LEN`] cells × [`PHENOLOGY_BANDS`] bands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhenologyTile {
    raster: Raster,
}

impl PhenologyTile {
    /// Construct from pixel-interleaved values.
    ///
    /// # Errors
    /// [`TileError::Layout`] when `data` has the wrong length.
    pub fn new(id: TileId, data: Vec<i16>) -> Result<Self, TileError> {
        Ok(Self {
            raster: Raster::new(id, LayerGrid::CLIMATE, PHENOLOGY_BANDS, data)?,
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
    /// [`TileError`] when the bytes are not a valid `phenology` tile.
    pub fn decode(bytes: &[u8]) -> Result<Self, TileError> {
        Ok(Self {
            raster: Raster::decode(bytes, LayerGrid::CLIMATE, PHENOLOGY_BANDS)?,
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

    /// Whether a cell has all 5 bands present.
    #[must_use]
    pub fn has(&self, index: usize) -> bool {
        let p = self.raster.pixel(index);
        !p.is_empty() && p.iter().all(|&v| v != NODATA)
    }

    /// Number of cells with all 5 bands present.
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

/// Decoded `phenology` values at one climate cell.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PhenologyLookup {
    /// Centre latitude of the cell read.
    pub cell_lat: f64,
    /// Centre longitude of the cell read.
    pub cell_lon: f64,
    /// Tile name, e.g. `n50e000`.
    pub tile: String,
    /// Baseline 1991–2020 mean annual Chill Portions (`ΔT = 0 °C`).
    pub chill_portions_0c: u16,
    /// Mean annual Chill Portions under `+1 °C` warming.
    pub chill_portions_1c: u16,
    /// Mean annual Chill Portions under `+2 °C` warming.
    pub chill_portions_2c: u16,
    /// Mean annual Chill Portions under `+3 °C` warming.
    pub chill_portions_3c: u16,
    /// Mean annual Chill Portions under `+4 °C` warming.
    pub chill_portions_4c: u16,
}

impl PhenologyLookup {
    /// The 5-point warming sensitivity curve `[CP(0), CP(+1), CP(+2), CP(+3), CP(+4)]`.
    #[must_use]
    pub const fn curve(&self) -> [f64; PHENOLOGY_BANDS] {
        [
            self.chill_portions_0c as f64,
            self.chill_portions_1c as f64,
            self.chill_portions_2c as f64,
            self.chill_portions_3c as f64,
            self.chill_portions_4c as f64,
        ]
    }

    /// Project Chill Portions for an arbitrary warming delta `delta_t_c` (°C)
    /// by piecewise-linear interpolation across `{0, +1, +2, +3, +4 °C}`.
    ///
    /// - For `delta_t_c <= 0.0` (or non-finite): clamps cleanly to `chill_portions_0c`.
    /// - For `0.0 < delta_t_c <= 4.0`: linear interpolation between adjacent integer knots.
    /// - For `delta_t_c > 4.0`: linear extrapolation using the `[+3, +4]` slope,
    ///   clamped to `[0.0, 250.0]` (never negative or runaway).
    #[must_use]
    pub fn project_chill_portions(&self, delta_t_c: f64) -> f64 {
        let c = self.curve();
        if !delta_t_c.is_finite() || delta_t_c <= 0.0 {
            return c[0];
        }
        let val = if delta_t_c >= 4.0 {
            let slope = c[4] - c[3];
            c[4] + slope * (delta_t_c - 4.0)
        } else {
            let lo = delta_t_c.floor() as usize;
            let hi = (lo + 1).min(4);
            let frac = delta_t_c - lo as f64;
            c[lo] + (c[hi] - c[lo]) * frac
        };
        (val.clamp(0.0, 250.0) * 10.0).round() / 10.0
    }
}

/// Projected Chill Portions for one climate slot, derived by pairing
/// [`PhenologyLookup`] with [`MonthlyLookup`] winter warming deltas.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectedChillPortions {
    /// `1991-2020`, `2041-2070` or `2071-2099`.
    pub period: &'static str,
    /// `historical` or `ssp245`.
    pub scenario: &'static str,
    /// Mean winter warming delta relative to 1991–2020 (°C; `0.0` for historical).
    pub winter_delta_c: f64,
    /// Projected annual Chill Portions (CP).
    pub chill_portions: f64,
}

/// Derive projected Chill Portions for every slot in `monthly` by computing
/// the 6-month winter-season mean temperature shift (`tmean_slot - tmean_hist`)
/// centered on the cell's coldest historical month and interpolating
/// [`PhenologyLookup::project_chill_portions`].
#[must_use]
pub fn project_from_monthly(
    pheno: &PhenologyLookup,
    monthly: &MonthlyLookup,
) -> Vec<ProjectedChillPortions> {
    let Some(hist) = monthly.periods.first() else {
        return Vec::new();
    };
    // Identify the coldest historical month (0..12) and average warming over
    // the 6-month winter window [cold - 2 ..= cold + 3] (mod 12).
    let mut cold_m = 0usize;
    let mut cold_t = f64::INFINITY;
    for (m, &t) in hist.tmean_c.iter().enumerate() {
        if t < cold_t {
            cold_t = t;
            cold_m = m;
        }
    }
    let winter_months = [
        (cold_m + 10) % 12,
        (cold_m + 11) % 12,
        cold_m,
        (cold_m + 1) % 12,
        (cold_m + 2) % 12,
        (cold_m + 3) % 12,
    ];
    let round2 = |x: f64| (x * 100.0).round() / 100.0;
    monthly
        .periods
        .iter()
        .enumerate()
        .map(|(idx, p)| {
            let delta = if idx == 0 {
                0.0
            } else {
                winter_months
                    .iter()
                    .map(|&m| p.tmean_c[m] - hist.tmean_c[m])
                    .sum::<f64>()
                    / 6.0
            };
            ProjectedChillPortions {
                period: p.period,
                scenario: p.scenario,
                winter_delta_c: round2(delta),
                chill_portions: pheno.project_chill_portions(delta),
            }
        })
        .collect()
}

/// Read `phenology` at the cell holding (`cell_lat`, `cell_lon`).
/// `Ok(None)` when the cell has no data.
///
/// # Errors
/// [`LookupError`] for an invalid coordinate or a cell outside `tile`.
pub fn at_cell(
    tile: &PhenologyTile,
    cell_lat: f64,
    cell_lon: f64,
) -> Result<Option<PhenologyLookup>, LookupError> {
    let cell = Cell::of(cell_lat, cell_lon).ok_or(LookupError::InvalidCoordinate)?;
    if cell.tile() != tile.id() {
        return Err(LookupError::WrongTile {
            expected: cell.tile().name(),
            given: tile.id().name(),
        });
    }
    let i = cell.index_in_tile();
    let (Some(c0), Some(c1), Some(c2), Some(c3), Some(c4)) = (
        PhenologyBand::ChillPortions0c.decode(tile.get(i, PhenologyBand::ChillPortions0c.index())),
        PhenologyBand::ChillPortions1c.decode(tile.get(i, PhenologyBand::ChillPortions1c.index())),
        PhenologyBand::ChillPortions2c.decode(tile.get(i, PhenologyBand::ChillPortions2c.index())),
        PhenologyBand::ChillPortions3c.decode(tile.get(i, PhenologyBand::ChillPortions3c.index())),
        PhenologyBand::ChillPortions4c.decode(tile.get(i, PhenologyBand::ChillPortions4c.index())),
    ) else {
        return Ok(None);
    };
    let round6 = |x: f64| (x * 1e6).round() / 1e6;
    Ok(Some(PhenologyLookup {
        cell_lat: round6(cell.center_lat()),
        cell_lon: round6(cell.center_lon()),
        tile: tile.id().name(),
        chill_portions_0c: c0,
        chill_portions_1c: c1,
        chill_portions_2c: c2,
        chill_portions_3c: c3,
        chill_portions_4c: c4,
    }))
}

/// Synthetic `phenology` tile matching `tile::synthetic::tile(n50e000)` for tests.
#[cfg(test)]
pub mod synthetic {
    use super::*;
    use crate::tile::synthetic as climate_syn;

    /// Synthetic `phenology` tile for `n50e000` (same land cells as the synthetic
    /// climate tile, with deterministic chill curves).
    #[must_use]
    pub fn phenology() -> PhenologyTile {
        let id = TileId::parse("n50e000").expect("valid tile id");
        let climate = climate_syn::tile(id);
        let mut data = vec![NODATA; TILE_LEN * PHENOLOGY_BANDS];
        for i in 0..TILE_LEN {
            if !climate.is_complete(i) {
                continue;
            }
            let base = i * PHENOLOGY_BANDS;
            data[base + PhenologyBand::ChillPortions0c.index()] =
                PhenologyBand::ChillPortions0c.encode(96.0);
            data[base + PhenologyBand::ChillPortions1c.index()] =
                PhenologyBand::ChillPortions1c.encode(88.0);
            data[base + PhenologyBand::ChillPortions2c.index()] =
                PhenologyBand::ChillPortions2c.encode(76.0);
            data[base + PhenologyBand::ChillPortions3c.index()] =
                PhenologyBand::ChillPortions3c.encode(62.0);
            data[base + PhenologyBand::ChillPortions4c.index()] =
                PhenologyBand::ChillPortions4c.encode(46.0);
        }
        PhenologyTile::new(id, data).expect("valid synthetic phenology tile")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monthly::PeriodMonthly;

    #[test]
    fn at_cell_and_projection_work() {
        let tile = synthetic::phenology();
        let found = at_cell(&tile, 45.25, 3.25)
            .unwrap()
            .expect("land cell has phenology");
        assert_eq!(found.chill_portions_0c, 96);
        assert_eq!(found.chill_portions_1c, 88);
        assert_eq!(found.chill_portions_2c, 76);
        assert_eq!(found.chill_portions_3c, 62);
        assert_eq!(found.chill_portions_4c, 46);

        // Piecewise-linear interpolation & clamping
        assert_eq!(found.project_chill_portions(-1.0), 96.0);
        assert_eq!(found.project_chill_portions(0.0), 96.0);
        assert_eq!(found.project_chill_portions(0.5), 92.0);
        assert_eq!(found.project_chill_portions(1.5), 82.0);
        assert_eq!(found.project_chill_portions(4.0), 46.0);
        // Above +4 C: linear extrapolation using [3,4] slope (-16/C), floored at 0
        assert_eq!(found.project_chill_portions(5.0), 30.0);
        assert_eq!(found.project_chill_portions(10.0), 0.0);
        assert_eq!(found.project_chill_portions(f64::NAN), 96.0);

        // Sea cell returns None
        assert!(at_cell(&tile, 40.55, 0.55).unwrap().is_none());
    }

    #[test]
    fn project_from_monthly_tracks_winter_warming() {
        let tile = synthetic::phenology();
        let pheno = at_cell(&tile, 45.25, 3.25).unwrap().unwrap();
        let monthly = MonthlyLookup {
            cell_lat: pheno.cell_lat,
            cell_lon: pheno.cell_lon,
            tile: pheno.tile.clone(),
            periods: vec![
                PeriodMonthly {
                    period: "1991-2020",
                    scenario: "historical",
                    tmean_c: [
                        2.0, 3.0, 7.0, 10.0, 14.0, 18.0, 20.0, 20.0, 16.0, 11.0, 6.0, 3.0,
                    ],
                    tmin_c: [0.0; 12],
                    dry_mask: 0,
                    dry_months: 0,
                    gdd_base_10: 1000.0,
                    chill_months: 5,
                },
                PeriodMonthly {
                    period: "2041-2070",
                    scenario: "ssp245",
                    tmean_c: [
                        3.5, 4.5, 8.5, 11.5, 15.5, 19.5, 21.5, 21.5, 17.5, 12.5, 7.5, 4.5,
                    ],
                    tmin_c: [1.5; 12],
                    dry_mask: 0,
                    dry_months: 0,
                    gdd_base_10: 1200.0,
                    chill_months: 4,
                },
            ],
        };
        let proj = project_from_monthly(&pheno, &monthly);
        assert_eq!(proj.len(), 2);
        assert_eq!(proj[0].winter_delta_c, 0.0);
        assert_eq!(proj[0].chill_portions, 96.0);
        assert_eq!(proj[1].winter_delta_c, 1.5);
        assert_eq!(proj[1].chill_portions, 82.0);
    }
}
