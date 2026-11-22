//! The `extremes` layer: the coldest night and the hottest day of a typical
//! year, for every climate slot, on the climate grid (0.1-degree cells in the
//! same 100-cell tiles, under `extremes/`).
//!
//! - `tmin_extreme`: the mean over the slot's 30 years of each year's lowest
//!   daily minimum temperature (°C). USDA hardiness zones are defined on this
//!   quantity ([`usda_zone`]).
//! - `tmax_extreme`: the same for each year's highest daily maximum (°C).
//!
//! Band `var * 9 + slot` (variables in [`EXTREMES`] order, slots in
//! [`SLOTS`] order), `int16`, scale 0.1 °C. A cell has values exactly where
//! the climate tile's cell is complete and the sources cover it, so read the
//! cell the climate lookup used ([`at_cell`]); the layer has no search of its
//! own. Method: `docs/decisions/0005-annual-extremes.md`.

use serde::Serialize;

use crate::grid::{Cell, TILE_LEN, TileId};
use crate::lookup::LookupError;
use crate::model::{NODATA, SLOTS, quantize};
use crate::raster::{LayerGrid, Raster};
use crate::tile::TileError;

/// Directory of the layer's tiles, relative to the manifest.
pub const DIR: &str = "extremes";

/// A stored extreme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Extreme {
    /// Mean annual lowest daily minimum (°C).
    TminExtreme,
    /// Mean annual highest daily maximum (°C).
    TmaxExtreme,
}

/// Variables in band order.
pub const EXTREMES: [Extreme; 2] = [Extreme::TminExtreme, Extreme::TmaxExtreme];

/// Bands per tile: every variable for every slot.
pub const EXTREMES_BANDS: usize = EXTREMES.len() * SLOTS.len();

/// Stored value = round(°C / [`SCALE`]).
pub const SCALE: f32 = 0.1;

impl Extreme {
    /// Identifier (manifest band id).
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::TminExtreme => "tmin_extreme",
            Self::TmaxExtreme => "tmax_extreme",
        }
    }

    /// One-sentence description, including the method.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::TminExtreme => {
                "Coldest night of a typical year: mean of each year's lowest daily minimum. tmin_cold plus the ERA5-Land 1991-2020 gap between the two, plus that gap's change per degree of coldest-month warming (median of 16 NEX-GDDP-CMIP6 models, one per model family) times the slot's warming."
            }
            Self::TmaxExtreme => {
                "Hottest day of a typical year: mean of each year's highest daily maximum. tmax_warm plus the ERA5-Land 1991-2020 gap between the two, plus that gap's change per degree of warmest-month warming (median of 16 NEX-GDDP-CMIP6 models, one per model family) times the slot's warming."
            }
        }
    }

    /// Position in [`EXTREMES`].
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::TminExtreme => 0,
            Self::TmaxExtreme => 1,
        }
    }

    /// Encode °C; non-finite values become [`NODATA`].
    #[must_use]
    pub fn encode(self, celsius: f32) -> i16 {
        quantize(celsius, SCALE, 0.0)
    }

    /// Decode a stored value; [`NODATA`] becomes `None`.
    #[must_use]
    pub fn decode(self, stored: i16) -> Option<f64> {
        (stored != NODATA).then(|| f64::from(stored) / 10.0)
    }
}

/// 0-based band of a variable in a slot.
#[must_use]
pub const fn band(var: Extreme, slot: usize) -> usize {
    var.index() * SLOTS.len() + slot
}

/// A decoded extremes tile: [`TILE_LEN`] cells x [`EXTREMES_BANDS`] values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtremesTile {
    raster: Raster,
}

impl ExtremesTile {
    /// A tile from pixel-interleaved values.
    ///
    /// # Errors
    /// [`TileError::Layout`] when `data` has the wrong length.
    pub fn new(id: TileId, data: Vec<i16>) -> Result<Self, TileError> {
        Ok(Self {
            raster: Raster::new(id, LayerGrid::CLIMATE, EXTREMES_BANDS, data)?,
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
    /// [`TileError`] when the bytes are not an extremes tile.
    pub fn decode(bytes: &[u8]) -> Result<Self, TileError> {
        Ok(Self {
            raster: Raster::decode(bytes, LayerGrid::CLIMATE, EXTREMES_BANDS)?,
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
        (0..TILE_LEN).filter(|&i| self.has(i)).count()
    }

    /// The pixel-interleaved values.
    #[must_use]
    pub fn data(&self) -> &[i16] {
        self.raster.data()
    }
}

/// Extremes of one slot.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PeriodExtremes {
    /// `1991-2020`, `2041-2070` or `2071-2099`.
    pub period: &'static str,
    /// `historical`, `ssp126`, `ssp245`, `ssp370` or `ssp585`.
    pub scenario: &'static str,
    /// Coldest night of a typical year (°C).
    pub tmin_extreme_c: f64,
    /// Hottest day of a typical year (°C).
    pub tmax_extreme_c: f64,
    /// USDA hardiness zone of `tmin_extreme_c`, e.g. `8a`.
    pub usda_zone: String,
}

/// Extremes at a cell.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExtremesLookup {
    /// Centre of the cell read.
    pub cell_lat: f64,
    /// Centre of the cell read.
    pub cell_lon: f64,
    /// Tile name, e.g. `n50e000`.
    pub tile: String,
    /// One entry per slot, in [`SLOTS`] order.
    pub periods: Vec<PeriodExtremes>,
}

/// The extremes of the cell holding (`cell_lat`, `cell_lon`): pass the
/// climate lookup's `cell_lat`/`cell_lon` and its tile's extremes tile.
/// `Ok(None)` when the cell has no extremes (sea, or a gap in the sources).
///
/// # Errors
/// [`LookupError`] for an invalid coordinate or a cell outside the tile.
pub fn at_cell(
    tile: &ExtremesTile,
    cell_lat: f64,
    cell_lon: f64,
) -> Result<Option<ExtremesLookup>, LookupError> {
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
    let periods = SLOTS
        .iter()
        .enumerate()
        .map(|(s, slot)| {
            let v = |var| var_value(tile, i, var, s);
            let tmin = v(Extreme::TminExtreme);
            PeriodExtremes {
                period: slot.period.label(),
                scenario: slot.scenario.id(),
                tmin_extreme_c: tmin,
                tmax_extreme_c: v(Extreme::TmaxExtreme),
                usda_zone: usda_zone(tmin),
            }
        })
        .collect();
    let round6 = |x: f64| (x * 1e6).round() / 1e6;
    Ok(Some(ExtremesLookup {
        cell_lat: round6(cell.center_lat()),
        cell_lon: round6(cell.center_lon()),
        tile: tile.id().name(),
        periods,
    }))
}

fn var_value(tile: &ExtremesTile, index: usize, var: Extreme, slot: usize) -> f64 {
    var.decode(tile.get(index, band(var, slot)))
        .unwrap_or(f64::NAN)
}

/// USDA hardiness zone of a mean annual extreme minimum (°C): 10 °F bands
/// from zone 1 (-60 to -50 °F) to 13 (60 to 70 °F), each split into `a`
/// (colder half) and `b`. Colder than -60 °F is `1a`, warmer than 70 °F
/// `13b`. The bounds are in °F, so a value is converted before banding.
#[must_use]
pub fn usda_zone(tmin_extreme_c: f64) -> String {
    let f = tmin_extreme_c * 9.0 / 5.0 + 32.0;
    // Half-zones of 5 °F counted from -60 °F; 0 = 1a, 25 = 13b.
    let half = ((f + 60.0) / 5.0).floor().clamp(0.0, 25.0) as u32;
    format!(
        "{}{}",
        half / 2 + 1,
        if half.is_multiple_of(2) { 'a' } else { 'b' }
    )
}

#[cfg(test)]
pub(crate) mod synthetic {
    //! A synthetic extremes tile for tests (also the committed fixture
    //! `tests/fixtures/synthetic-extremes-n50e000.tif`), on the synthetic
    //! climate tile: every complete cell but column 60 (a gap in the
    //! sources) has tmin_extreme = tmin_cold - 8 - slot / 10 and
    //! tmax_extreme = tmax_warm + 10 + slot / 10.

    use super::{EXTREMES_BANDS, Extreme, ExtremesTile, band};
    use crate::grid::{TILE_CELLS, TILE_LEN, TileId};
    use crate::model::{NODATA, SLOTS, Variable};

    pub(crate) fn extremes() -> ExtremesTile {
        let id = TileId::parse("n50e000").expect("valid");
        let c = crate::tile::synthetic::tile(id);
        let mut data = vec![NODATA; TILE_LEN * EXTREMES_BANDS];
        for i in 0..TILE_LEN {
            if !c.is_complete(i) || i % TILE_CELLS == 60 {
                continue;
            }
            for s in 0..SLOTS.len() {
                let get = |v: Variable| c.get(i, crate::model::band(v.index(), s));
                let ds = s as i16;
                data[i * EXTREMES_BANDS + band(Extreme::TminExtreme, s)] =
                    get(Variable::TminCold) - 80 - ds;
                data[i * EXTREMES_BANDS + band(Extreme::TmaxExtreme, s)] =
                    get(Variable::TmaxWarm) + 100 + ds;
            }
        }
        ExtremesTile::new(id, data).expect("valid tile")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usda_zones_follow_the_fahrenheit_bands() {
        // -8.4 °C = 16.9 °F: zone 8a covers 10-15 °F, 8b 15-20 °F.
        assert_eq!(usda_zone(-8.4), "8b");
        assert_eq!(usda_zone(-12.2), "8a"); // 10.04 °F
        assert_eq!(usda_zone(-17.8), "6b"); // -0.04 °F
        assert_eq!(usda_zone(-17.7), "7a"); // 0.14 °F
        assert_eq!(usda_zone(-51.1), "1a"); // -59.98 °F
        assert_eq!(usda_zone(-70.0), "1a");
        assert_eq!(usda_zone(0.0), "10a"); // 32 °F: 30-35 °F
        assert_eq!(usda_zone(21.0), "13b"); // 69.8 °F
        assert_eq!(usda_zone(30.0), "13b");
    }

    #[test]
    fn band_layout_and_encoding() {
        assert_eq!(EXTREMES_BANDS, 18);
        assert_eq!(band(Extreme::TminExtreme, 0), 0);
        assert_eq!(band(Extreme::TmaxExtreme, 0), 9);
        assert_eq!(band(Extreme::TmaxExtreme, 8), 17);
        assert_eq!(Extreme::TminExtreme.encode(-8.44), -84);
        assert_eq!(Extreme::TminExtreme.decode(-84), Some(-8.4));
        assert_eq!(Extreme::TmaxExtreme.decode(NODATA), None);
        assert_eq!(Extreme::TmaxExtreme.encode(f32::NAN), NODATA);
        for (i, v) in EXTREMES.iter().enumerate() {
            assert_eq!(v.index(), i);
        }
    }

    #[test]
    fn at_cell_reads_the_given_cell() {
        let id = TileId::parse("n50e000").unwrap();
        let mut data = vec![NODATA; TILE_LEN * EXTREMES_BANDS];
        let cell = Cell::of(47.2894, 3.8186).unwrap();
        let i = cell.index_in_tile();
        for s in 0..SLOTS.len() {
            data[i * EXTREMES_BANDS + band(Extreme::TminExtreme, s)] = -84 + s as i16;
            data[i * EXTREMES_BANDS + band(Extreme::TmaxExtreme, s)] = 330 + s as i16;
        }
        let t = ExtremesTile::new(id, data).unwrap();
        assert_eq!(t.cells(), 1);
        let e = at_cell(&t, 47.25, 3.85).unwrap().unwrap();
        assert_eq!((e.cell_lat, e.cell_lon), (47.25, 3.85));
        assert_eq!(e.periods.len(), 9);
        assert_eq!(e.periods[0].tmin_extreme_c, -8.4);
        assert_eq!(e.periods[0].tmax_extreme_c, 33.0);
        assert_eq!(e.periods[0].usda_zone, "8b");
        assert_eq!(e.periods[8].scenario, "ssp585");
        assert_eq!(at_cell(&t, 47.15, 3.85).unwrap(), None);
        assert!(matches!(
            at_cell(&t, 51.0, 3.85),
            Err(LookupError::WrongTile { .. })
        ));
        assert!(ExtremesTile::new(id, vec![]).is_err());
    }
}
