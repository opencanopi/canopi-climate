//! Point lookup: a decoded tile and a latitude/longitude give the climate of
//! every slot.
//!
//! A point resolves to its own cell when that cell is complete (every
//! variable, every slot), else to the nearest complete cell within
//! `radius` cells (coastal points on a 0.1-degree grid), else to nothing.
//! The search stays inside the tile: a coastal point on a tile edge whose
//! nearest land lies in the next tile gets `None`.

use serde::Serialize;

use crate::grid::{Cell, TILE_CELLS, TileId};
use crate::koppen;
use crate::model::{SLOTS, VARIABLES, Variable, band};
use crate::tile::Tile;

/// Default search radius in cells.
pub const DEFAULT_RADIUS: usize = 2;

/// Climate of one slot at a cell.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PeriodClimate {
    /// `1991-2020`, `2041-2070` or `2071-2099`.
    pub period: &'static str,
    /// `historical`, `ssp126`, `ssp245`, `ssp370` or `ssp585`.
    pub scenario: &'static str,
    /// Köppen-Geiger symbol, e.g. `Cfb`.
    pub koppen: &'static str,
    /// Köppen-Geiger code 1-30.
    pub koppen_code: u8,
    /// How far (°C) this cell's coldest/warmest monthly means lie outside the
    /// class's temperature bounds; 0 when consistent (see
    /// [`koppen::temperature_margin`]).
    pub koppen_temperature_margin_c: f64,
    /// Mean daily minimum of the coldest month (°C).
    pub tmin_cold_c: f64,
    /// Mean daily maximum of the warmest month (°C).
    pub tmax_warm_c: f64,
    /// Coldest monthly mean (°C).
    pub tmean_cold_c: f64,
    /// Warmest monthly mean (°C).
    pub tmean_warm_c: f64,
    /// Annual precipitation (mm).
    pub precip_mm: f64,
    /// Precipitation / potential evapotranspiration.
    pub aridity_index: Option<f64>,
    /// Dry months (Gaussen), 0-12.
    pub dry_months: u8,
}

/// Result of a lookup.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Lookup {
    /// Queried latitude.
    pub lat: f64,
    /// Queried longitude.
    pub lon: f64,
    /// Centre of the cell used.
    pub cell_lat: f64,
    /// Centre of the cell used.
    pub cell_lon: f64,
    /// Distance from the point's cell to the cell used, in cells (0 when
    /// the point's own cell has data).
    pub distance_cells: f64,
    /// Tile name, e.g. `n50e000`.
    pub tile: String,
    /// One entry per slot, in [`SLOTS`] order.
    pub periods: Vec<PeriodClimate>,
}

/// Why a lookup could not run.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LookupError {
    /// Latitude outside [-90, 90] or a non-finite coordinate.
    #[error("invalid coordinate")]
    InvalidCoordinate,
    /// The point lies in another tile.
    #[error("the point lies in tile {expected}, not in {given}")]
    WrongTile {
        /// Tile holding the point.
        expected: String,
        /// Tile passed in.
        given: String,
    },
}

/// Look a point up with the [`DEFAULT_RADIUS`].
///
/// # Errors
/// See [`lookup_within`].
pub fn lookup(tile: &Tile, lat: f64, lon: f64) -> Result<Option<Lookup>, LookupError> {
    lookup_within(tile, lat, lon, DEFAULT_RADIUS)
}

/// Look a point up, searching up to `radius` cells for a complete cell.
/// `Ok(None)` means no complete cell (ocean, or land without data).
///
/// # Errors
/// [`LookupError`] for an invalid coordinate or a point outside the tile.
pub fn lookup_within(
    tile: &Tile,
    lat: f64,
    lon: f64,
    radius: usize,
) -> Result<Option<Lookup>, LookupError> {
    let cell = Cell::of(lat, lon).ok_or(LookupError::InvalidCoordinate)?;
    if cell.tile() != tile.id() {
        return Err(LookupError::WrongTile {
            expected: cell.tile().name(),
            given: tile.id().name(),
        });
    }
    let Some((found, d2)) = nearest_complete(tile, cell, radius) else {
        return Ok(None);
    };
    let index = found.index_in_tile();
    let periods = (0..SLOTS.len()).map(|s| period(tile, index, s)).collect();
    Ok(Some(Lookup {
        lat,
        lon,
        cell_lat: round6(found.center_lat()),
        cell_lon: round6(found.center_lon()),
        distance_cells: f64::from(d2).sqrt(),
        tile: tile.id().name(),
        periods,
    }))
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// Nearest complete cell within `radius` (squared distance in cells); ties go
/// to the northern, then western cell.
fn nearest_complete(tile: &Tile, cell: Cell, radius: usize) -> Option<(Cell, u32)> {
    let id: TileId = tile.id();
    let (x0, y0) = (cell.col % TILE_CELLS, cell.row % TILE_CELLS);
    let r = radius.min(TILE_CELLS);
    let mut best: Option<(u32, usize, usize)> = None;
    for y in y0.saturating_sub(r)..=(y0 + r).min(TILE_CELLS - 1) {
        for x in x0.saturating_sub(r)..=(x0 + r).min(TILE_CELLS - 1) {
            let (dx, dy) = (x.abs_diff(x0), y.abs_diff(y0));
            let d2 = u32::try_from(dx * dx + dy * dy).unwrap_or(u32::MAX);
            if d2 > u32::try_from(r * r).unwrap_or(u32::MAX) {
                continue;
            }
            if best.is_some_and(|(b, _, _)| b <= d2) {
                continue;
            }
            if tile.is_complete(y * TILE_CELLS + x) {
                best = Some((d2, x, y));
            }
        }
    }
    best.map(|(d2, x, y)| (id.cell(y * TILE_CELLS + x), d2))
}

fn value(tile: &Tile, index: usize, var: Variable, slot: usize) -> f64 {
    var.decode(tile.get(index, band(var.index(), slot)))
        .unwrap_or(f64::NAN)
}

fn period(tile: &Tile, index: usize, slot: usize) -> PeriodClimate {
    let s = SLOTS[slot];
    let v = |var| value(tile, index, var, slot);
    let code = u8::try_from(tile.get(index, band(0, slot))).unwrap_or(0);
    let (tmean_cold, tmean_warm) = (v(Variable::TmeanCold), v(Variable::TmeanWarm));
    let dry_months = v(Variable::DryMonths).clamp(0.0, 12.0) as u8;
    debug_assert_eq!(VARIABLES[0], Variable::Koppen);
    PeriodClimate {
        period: s.period.label(),
        scenario: s.scenario.id(),
        koppen: koppen::symbol(code),
        koppen_code: code,
        koppen_temperature_margin_c: round1(f64::from(koppen::temperature_margin(
            code,
            tmean_cold as f32,
            tmean_warm as f32,
        ))),
        tmin_cold_c: v(Variable::TminCold),
        tmax_warm_c: v(Variable::TmaxWarm),
        tmean_cold_c: tmean_cold,
        tmean_warm_c: tmean_warm,
        precip_mm: v(Variable::Precip),
        aridity_index: Variable::Aridity
            .decode(tile.get(index, band(Variable::Aridity.index(), slot))),
        dry_months,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{GRID_H, TILE_LEN, TILES_X, TILES_Y};
    use crate::model::{BANDS, NODATA};
    use crate::tile::synthetic;

    fn n50e000() -> Tile {
        synthetic::tile(TileId::parse("n50e000").unwrap())
    }

    /// Latitude/longitude of the centre of cell (x, y) of a tile.
    fn at(t: &Tile, x: usize, y: usize) -> (f64, f64) {
        let c = t.id().cell(y * TILE_CELLS + x);
        (c.center_lat(), c.center_lon())
    }

    #[test]
    fn missing_aridity_keeps_the_cell_and_reads_null() {
        let t = n50e000();
        let j = 40 * TILE_CELLS + 50;
        let mut data = t.data().to_vec();
        for s in 0..SLOTS.len() {
            data[j * BANDS + band(Variable::Aridity.index(), s)] = NODATA;
        }
        let t = Tile::new(t.id(), data).unwrap();
        assert!(t.is_complete(j));
        let (lat, lon) = at(&t, 50, 40);
        let r = lookup(&t, lat, lon).unwrap().unwrap();
        assert_eq!(r.distance_cells, 0.0);
        assert!(r.periods.iter().all(|p| p.aridity_index.is_none()));
        let v = serde_json::to_value(&r).unwrap();
        assert!(v["periods"][0]["aridity_index"].is_null());
        // Any other missing value still makes the cell incomplete.
        let mut data = t.data().to_vec();
        data[j * BANDS + band(Variable::Precip.index(), 0)] = NODATA;
        assert!(!Tile::new(t.id(), data).unwrap().is_complete(j));
    }

    #[test]
    fn land_cell_returns_its_own_values() {
        let t = n50e000();
        let (lat, lon) = at(&t, 50, 40);
        let r = lookup(&t, lat, lon).unwrap().unwrap();
        assert_eq!(r.distance_cells, 0.0);
        assert_eq!(r.tile, "n50e000");
        assert_eq!(r.periods.len(), 9);
        let p0 = &r.periods[0];
        assert_eq!((p0.period, p0.scenario), ("1991-2020", "historical"));
        assert!(
            (p0.tmin_cold_c - f64::from(synthetic::value(Variable::TminCold, 0, 50, 40))).abs()
                < 0.051
        );
        assert!(
            (p0.precip_mm - f64::from(synthetic::value(Variable::Precip, 0, 50, 40))).abs() < 0.51
        );
        assert_eq!(
            p0.koppen_code,
            synthetic::value(Variable::Koppen, 0, 50, 40) as u8
        );
        assert_eq!(p0.koppen, koppen::symbol(p0.koppen_code));
        let p8 = &r.periods[8];
        assert_eq!((p8.period, p8.scenario), ("2071-2099", "ssp585"));
        assert!(
            (p8.tmax_warm_c - f64::from(synthetic::value(Variable::TmaxWarm, 8, 50, 40))).abs()
                < 0.051
        );
        assert_eq!(
            p8.dry_months,
            synthetic::value(Variable::DryMonths, 8, 50, 40) as u8
        );
        assert!((r.cell_lat - lat).abs() < 1e-6 && (r.cell_lon - lon).abs() < 1e-6);
    }

    #[test]
    fn ocean_snaps_to_land_within_two_cells_or_none() {
        let t = n50e000();
        // Block spans x 20..80, y 30..70.
        let (lat, lon) = at(&t, 18, 40);
        let r = lookup(&t, lat, lon).unwrap().unwrap();
        assert_eq!(r.distance_cells, 2.0);
        let (elat, elon) = at(&t, 20, 40);
        assert!((r.cell_lat - elat).abs() < 1e-6 && (r.cell_lon - elon).abs() < 1e-6);
        // Diagonal: (18, 28) to (20, 30) is 2.83 cells away: too far.
        let (lat, lon) = at(&t, 18, 28);
        assert_eq!(lookup(&t, lat, lon).unwrap(), None);
        let r = lookup_within(&t, lat, lon, 3).unwrap().unwrap();
        assert!((r.distance_cells - 8f64.sqrt()).abs() < 1e-9);
        // Three cells off the coast.
        let (lat, lon) = at(&t, 17, 40);
        assert_eq!(lookup(&t, lat, lon).unwrap(), None);
        // Radius 0: only the point's own cell.
        let (lat, lon) = at(&t, 19, 40);
        assert_eq!(lookup_within(&t, lat, lon, 0).unwrap(), None);
    }

    #[test]
    fn incomplete_cells_are_skipped() {
        let t = n50e000();
        // The island (5, 5) misses one value: nothing else within 2 cells.
        let (lat, lon) = at(&t, 5, 5);
        assert_eq!(lookup(&t, lat, lon).unwrap(), None);
    }

    #[test]
    fn tile_edges_and_corners() {
        let t = n50e000();
        // Exact north-west corner of the tile: corner cell (0, 0) is land.
        let r = lookup(&t, 50.0, 0.0).unwrap().unwrap();
        assert_eq!(r.distance_cells, 0.0);
        assert!((r.cell_lat - 49.95).abs() < 1e-9 && (r.cell_lon - 0.05).abs() < 1e-9);
        // Near the south-east corner, inside the tile.
        let r = lookup(&t, 40.03, 9.97).unwrap().unwrap();
        assert!((r.cell_lat - 40.05).abs() < 1e-9 && (r.cell_lon - 9.95).abs() < 1e-9);
        // 40.0 N belongs to the tile to the south, 10.0 E to the east.
        assert!(matches!(
            lookup(&t, 40.0, 5.0),
            Err(LookupError::WrongTile { .. })
        ));
        assert!(matches!(
            lookup(&t, 45.0, 10.0),
            Err(LookupError::WrongTile { .. })
        ));
        // Search does not cross the tile edge: one cell inside the north edge
        // next to ocean only.
        let (lat, lon) = at(&t, 50, 1);
        assert_eq!(lookup(&t, lat, lon).unwrap(), None);
    }

    #[test]
    fn antimeridian_points_use_the_western_tile() {
        let west = synthetic::tile(TileId { col: 0, row: 4 });
        assert_eq!(west.id().name(), "n50w180");
        // 180 E and -180 are the same meridian: column 0.
        let a = lookup(&west, 49.95, 180.0).unwrap().unwrap();
        let b = lookup(&west, 49.95, -180.0).unwrap().unwrap();
        assert_eq!(a.periods, b.periods);
        assert!((a.cell_lon + 179.95).abs() < 1e-9);
        // 179.99 E lies in the eastern tile.
        let east = synthetic::tile(TileId {
            col: TILES_X - 1,
            row: 4,
        });
        assert_eq!(east.id().name(), "n50e170");
        assert!(lookup(&west, 49.95, 179.99).is_err());
        let r = lookup(&east, 49.95, 179.99).unwrap().unwrap();
        assert!((r.cell_lon - 179.95).abs() < 1e-9);
    }

    #[test]
    fn poles() {
        let north = synthetic::tile(TileId { col: 18, row: 0 });
        let r = lookup(&north, 90.0, 0.0).unwrap().unwrap();
        assert!((r.cell_lat - 89.95).abs() < 1e-9);
        let south = synthetic::tile(TileId {
            col: 18,
            row: TILES_Y - 1,
        });
        let r = lookup(&south, -90.0, 0.0).unwrap().unwrap();
        assert_eq!(Cell::of(-90.0, 0.0).unwrap().row, GRID_H - 1);
        assert!((r.cell_lat + 89.95).abs() < 1e-9);
        assert_eq!(
            lookup(&north, 90.5, 0.0),
            Err(LookupError::InvalidCoordinate)
        );
        assert_eq!(
            lookup(&north, f64::NAN, 0.0),
            Err(LookupError::InvalidCoordinate)
        );
    }

    #[test]
    fn all_ocean_tile_returns_none() {
        let t = Tile::new(TileId { col: 0, row: 9 }, vec![NODATA; TILE_LEN * BANDS]).unwrap();
        assert_eq!(lookup(&t, -5.0, -175.0).unwrap(), None);
        assert_eq!(t.complete_cells(), 0);
    }

    #[test]
    fn serializes_with_stable_field_names() {
        let t = n50e000();
        let (lat, lon) = at(&t, 50, 40);
        let r = lookup(&t, lat, lon).unwrap().unwrap();
        let j = serde_json::to_value(&r).unwrap();
        let p = &j["periods"][0];
        for k in [
            "period",
            "scenario",
            "koppen",
            "koppen_code",
            "koppen_temperature_margin_c",
            "tmin_cold_c",
            "tmax_warm_c",
            "tmean_cold_c",
            "tmean_warm_c",
            "precip_mm",
            "aridity_index",
            "dry_months",
        ] {
            assert!(p.get(k).is_some(), "missing {k}");
        }
        assert!(j.get("distance_cells").is_some());
    }
}
