//! The `monthly` layer: 12-month mean temperature, mean daily minimum
//! temperature, and Gaussen dry-month bitmask on the climate grid (0.1-degree
//! cells in the same 100-cell tiles, under `monthly/`), for 3 slots
//! (`1991-2020`, `2041-2070 ssp245`, `2071-2099 ssp245`).
//!
//! Band layout (75 bands, `int16`, pixel-interleaved, Deflate, nodata -32768):
//! - `0..36`: `tmean` (`slot * 12 + month`, scale 0.5 °C)
//! - `36..72`: `tmin` (`36 + slot * 12 + month`, scale 0.5 °C)
//! - `72..75`: `dry_mask` (`72 + slot`, scale 1, bits 0–11 = Jan..Dec where
//!   `P < 2T`, bits 12–14 = 0, bit 15 never set)
//!
//! A cell has values exactly where the climate tile's cell is complete, so
//! read the cell the climate lookup used ([`at_cell`]); the layer has no
//! search of its own. Method: `docs/decisions/0004-monthly-and-relief-layers.md`.

use serde::Serialize;

use crate::derived::{chill_months, gdd_base_10};
use crate::grid::{Cell, TILE_LEN, TileId};
use crate::lookup::LookupError;
use crate::model::{NODATA, SLOTS, Slot, quantize};
use crate::raster::{LayerGrid, Raster};
use crate::tile::TileError;

/// Directory of the layer's tiles, relative to the manifest.
pub const DIR: &str = "monthly";

/// Indices into [`SLOTS`] carried by the `monthly` and `monthly_precip` layers:
/// `1991-2020` (0), `2041-2070 ssp245` (2), `2071-2099 ssp245` (6).
pub const MONTHLY_SLOT_INDICES: [usize; 3] = [0, 2, 6];

/// The 3 slots carried by the `monthly` and `monthly_precip` layers.
pub const MONTHLY_SLOTS: [Slot; 3] = [
    SLOTS[MONTHLY_SLOT_INDICES[0]],
    SLOTS[MONTHLY_SLOT_INDICES[1]],
    SLOTS[MONTHLY_SLOT_INDICES[2]],
];

/// Scale of `tmean` and `tmin` (°C per stored unit).
pub const TEMP_SCALE: f32 = 0.5;

/// Total bands per `monthly` tile: 3 slots x (12 tmean + 12 tmin + 1 dry_mask).
pub const MONTHLY_BANDS: usize = 75;

/// 0-based band index of `tmean` for `slot` (`0..3`) and `month` (`0..12`).
#[must_use]
pub const fn tmean_band(slot: usize, month: usize) -> usize {
    slot * 12 + month
}

/// 0-based band index of `tmin` for `slot` (`0..3`) and `month` (`0..12`).
#[must_use]
pub const fn tmin_band(slot: usize, month: usize) -> usize {
    36 + slot * 12 + month
}

/// 0-based band index of `dry_mask` for `slot` (`0..3`).
#[must_use]
pub const fn dry_mask_band(slot: usize) -> usize {
    72 + slot
}

/// Encode °C at 0.5 °C resolution; non-finite values become [`NODATA`].
#[must_use]
pub fn encode_temp(celsius: f32) -> i16 {
    quantize(celsius, TEMP_SCALE, 0.0)
}

/// Decode a stored 0.5 °C value; [`NODATA`] becomes `None`.
#[must_use]
pub fn decode_temp(stored: i16) -> Option<f64> {
    (stored != NODATA).then(|| f64::from(stored) * 0.5)
}

/// A decoded monthly tile: [`TILE_LEN`] cells x [`MONTHLY_BANDS`] values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonthlyTile {
    raster: Raster,
}

impl MonthlyTile {
    /// A tile from pixel-interleaved values.
    ///
    /// # Errors
    /// [`TileError::Layout`] when `data` has the wrong length.
    pub fn new(id: TileId, data: Vec<i16>) -> Result<Self, TileError> {
        Ok(Self {
            raster: Raster::new(id, LayerGrid::CLIMATE, MONTHLY_BANDS, data)?,
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
    /// [`TileError`] when the bytes are not a monthly tile.
    pub fn decode(bytes: &[u8]) -> Result<Self, TileError> {
        Ok(Self {
            raster: Raster::decode(bytes, LayerGrid::CLIMATE, MONTHLY_BANDS)?,
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

/// Monthly climatology of one slot, plus derived GDD (base 10 °C) and
/// chill-month count.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PeriodMonthly {
    /// `1991-2020`, `2041-2070` or `2071-2099`.
    pub period: &'static str,
    /// `historical` or `ssp245`.
    pub scenario: &'static str,
    /// Mean temperature for Jan..Dec (°C, 0.5 °C step).
    pub tmean_c: [f64; 12],
    /// Mean daily minimum temperature for Jan..Dec (°C, 0.5 °C step).
    pub tmin_c: [f64; 12],
    /// Gaussen dry-month bitmask (`gaussen_p_lt_2t`, bits 0–11 = Jan..Dec).
    pub dry_mask: u16,
    /// Number of dry months (`popcount(dry_mask)`).
    pub dry_months: u8,
    /// Annual growing degree-days above 10 °C (°C·day), derived from `tmean_c`.
    pub gdd_base_10: f64,
    /// Months with `0 < tmean_c <= 7.2` °C, derived from `tmean_c`.
    pub chill_months: u8,
}

/// Monthly climatology at a cell.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MonthlyLookup {
    /// Centre of the cell read.
    pub cell_lat: f64,
    /// Centre of the cell read.
    pub cell_lon: f64,
    /// Tile name, e.g. `n50e000`.
    pub tile: String,
    /// One entry per monthly slot, in [`MONTHLY_SLOTS`] order.
    pub periods: Vec<PeriodMonthly>,
}

/// The monthly climatology of the cell holding (`cell_lat`, `cell_lon`): pass
/// the climate lookup's `cell_lat`/`cell_lon` and its tile's monthly tile.
/// `Ok(None)` when the cell has no monthly data (sea).
///
/// # Errors
/// [`LookupError`] for an invalid coordinate or a cell outside the tile.
pub fn at_cell(
    tile: &MonthlyTile,
    cell_lat: f64,
    cell_lon: f64,
) -> Result<Option<MonthlyLookup>, LookupError> {
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
    let periods = MONTHLY_SLOTS
        .iter()
        .enumerate()
        .map(|(s, slot)| {
            let mut tmean_c = [0.0f64; 12];
            let mut tmin_c = [0.0f64; 12];
            for m in 0..12 {
                tmean_c[m] = decode_temp(tile.get(i, tmean_band(s, m))).unwrap_or(f64::NAN);
                tmin_c[m] = decode_temp(tile.get(i, tmin_band(s, m))).unwrap_or(f64::NAN);
            }
            let raw_mask = tile.get(i, dry_mask_band(s));
            let dry_mask = (raw_mask & 0x0FFF) as u16;
            let dry_months = u8::try_from(dry_mask.count_ones()).unwrap_or(12);
            let gdd = gdd_base_10(&tmean_c).unwrap_or(0.0);
            let gdd_base_10 = (gdd * 10.0).round() / 10.0;
            let chill_months = chill_months(&tmean_c).unwrap_or(0);
            PeriodMonthly {
                period: slot.period.label(),
                scenario: slot.scenario.id(),
                tmean_c,
                tmin_c,
                dry_mask,
                dry_months,
                gdd_base_10,
                chill_months,
            }
        })
        .collect();
    let round6 = |x: f64| (x * 1e6).round() / 1e6;
    Ok(Some(MonthlyLookup {
        cell_lat: round6(cell.center_lat()),
        cell_lon: round6(cell.center_lon()),
        tile: tile.id().name(),
        periods,
    }))
}

#[cfg(test)]
pub(crate) mod synthetic {
    //! A deterministic synthetic monthly tile for tests (also the committed
    //! fixture `tests/fixtures/synthetic-monthly-n50e000.tif`), on the
    //! synthetic climate tile: every complete cell has a smooth seasonal curve
    //! anchored to `tmean_cold`/`tmean_warm` and `tmin <= tmean`.

    use super::{
        MONTHLY_BANDS, MONTHLY_SLOT_INDICES, MonthlyTile, dry_mask_band, encode_temp, tmean_band,
        tmin_band,
    };
    use crate::grid::{TILE_LEN, TileId};
    use crate::model::{NODATA, Variable};

    pub(crate) fn monthly() -> MonthlyTile {
        let id = TileId::parse("n50e000").expect("valid");
        let c = crate::tile::synthetic::tile(id);
        let mut data = vec![NODATA; TILE_LEN * MONTHLY_BANDS];
        // Cosine seasonal weights: 0 in Jan (month 0), 1 in Jul (month 6).
        let weights: [f32; 12] = [
            0.0, 0.07, 0.25, 0.5, 0.75, 0.93, 1.0, 0.93, 0.75, 0.5, 0.25, 0.07,
        ];
        for i in 0..TILE_LEN {
            if !c.is_complete(i) {
                continue;
            }
            for (s, &climate_slot) in MONTHLY_SLOT_INDICES.iter().enumerate() {
                let get = |v: Variable| {
                    v.decode(c.get(i, crate::model::band(v.index(), climate_slot)))
                        .unwrap_or(0.0) as f32
                };
                let tcold = get(Variable::TmeanCold);
                let twarm = get(Variable::TmeanWarm);
                let dry = c.get(
                    i,
                    crate::model::band(Variable::DryMonths.index(), climate_slot),
                );
                let dry_count = usize::try_from(dry.clamp(0, 12)).unwrap_or(0);
                // Set `dry_count` summer-centred bits so popcount == dry_months.
                let mut mask = 0i16;
                for k in 0..dry_count {
                    let m = (6 + k) % 12;
                    mask |= 1i16 << m;
                }
                for (m, &w) in weights.iter().enumerate() {
                    let tm = tcold + (twarm - tcold) * w;
                    let tn = tm - 5.0;
                    data[i * MONTHLY_BANDS + tmean_band(s, m)] = encode_temp(tm);
                    data[i * MONTHLY_BANDS + tmin_band(s, m)] = encode_temp(tn);
                }
                data[i * MONTHLY_BANDS + dry_mask_band(s)] = mask;
            }
        }
        MonthlyTile::new(id, data).expect("valid monthly tile")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_layout_and_encoding() {
        assert_eq!(tmean_band(0, 0), 0);
        assert_eq!(tmean_band(2, 11), 35);
        assert_eq!(tmin_band(0, 0), 36);
        assert_eq!(tmin_band(2, 11), 71);
        assert_eq!(dry_mask_band(0), 72);
        assert_eq!(dry_mask_band(2), 74);
        assert_eq!(encode_temp(12.25), 25);
        assert_eq!(encode_temp(-8.4), -17);
        assert_eq!(decode_temp(-17), Some(-8.5));
        assert_eq!(decode_temp(NODATA), None);
        assert_eq!(encode_temp(f32::NAN), NODATA);
    }

    #[test]
    fn at_cell_decodes_monthly_and_derived_metrics() {
        let tile = synthetic::monthly();
        let got = at_cell(&tile, 45.95, 5.05).unwrap().unwrap();
        assert_eq!(got.tile, "n50e000");
        assert_eq!(got.periods.len(), 3);
        assert_eq!(got.periods[0].period, "1991-2020");
        assert_eq!(got.periods[0].scenario, "historical");
        assert_eq!(got.periods[1].period, "2041-2070");
        assert_eq!(got.periods[1].scenario, "ssp245");
        assert_eq!(got.periods[2].period, "2071-2099");
        assert_eq!(got.periods[2].scenario, "ssp245");
        for p in &got.periods {
            assert_eq!(p.dry_mask & !0x0FFF, 0);
            assert_eq!(u32::from(p.dry_months), p.dry_mask.count_ones());
            for m in 0..12 {
                assert!(p.tmin_c[m] <= p.tmean_c[m]);
            }
            assert!(p.gdd_base_10 > 0.0);
        }
        // Sea cell returns None.
        assert_eq!(at_cell(&tile, 49.5, 9.5).unwrap(), None);
        // Wrong tile errors.
        assert!(matches!(
            at_cell(&tile, 10.0, 10.0),
            Err(LookupError::WrongTile { .. })
        ));
    }
}
