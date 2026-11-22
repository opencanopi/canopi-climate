//! The `extremes` stage: the coldest night and the hottest day of a typical
//! year for every slot ([`crate::extremes`]), from the release's climate
//! tiles and two rasters computed on Google Earth Engine by
//! `scripts/ee/extremes.py` (`docs/decisions/0005-annual-extremes.md`):
//!
//! ```text
//! tmin_extreme[s] = tmin_cold[s] + gap_min + k_min * (tmean_cold[s] - tmean_cold[0])
//! tmax_extreme[s] = tmax_warm[s] + gap_max + k_max * (tmean_warm[s] - tmean_warm[0])
//! ```
//!
//! - `gap_min`, `gap_max`: ERA5-Land 1991-2020, mean annual lowest daily
//!   minimum minus the coldest month's mean daily minimum, and mean annual
//!   highest daily maximum minus the warmest month's mean daily maximum
//!   (0.1 degree, bilinear at the grid's cell centres). ERA5-Land has no
//!   value at sea: a coastal cell takes the nearest value within
//!   [`GAP_FILL_RADIUS`] cells.
//! - `k_min`, `k_max`: how much each gap changes per degree of
//!   coldest-month (warmest-month) warming, median of 16 NEX-GDDP-CMIP6
//!   models (1 degree, bilinear over the valid neighbours, else the
//!   nearest value within [`K_FILL_RADIUS`] cells, else 0: south of 60 S).
//! - Clamps: `tmin_extreme <= tmin_cold`, `tmax_extreme >= tmax_warm`
//!   (counted).
//!
//! A cell has values where its climate cell is complete and a gap was found.

use std::path::Path;
use std::time::Instant;

use rayon::prelude::{IntoParallelIterator, ParallelIterator};
use serde_json::{Value, json};
use wbgeotiff::GeoTiff;

use super::build::{BuildConfig, diff_stats};
use super::{PipelineError, Result, cog, layers, sources};
use crate::extremes::{DIR, EXTREMES, EXTREMES_BANDS, Extreme, ExtremesTile, band};
use crate::grid::{CELL_DEG, TILE_CELLS, TILE_LEN, TileId};
use crate::koppen;
use crate::manifest::{LayerSpec, SlotSpec, TileSpec, VariableSpec};
use crate::model::{NODATA, SLOTS, Variable};
use crate::tile::Tile;

/// ERA5-Land baseline: 6 bands of `f32` °C (NaN = none) on ERA5-Land's
/// own 0.1-degree grid, whose cell centres lie on multiples of 0.1 degree
/// ([`E5L`]): `gap_min`, `gap_max`, `tnn`, `txx`, `tmin_cold`, `tmax_warm`.
pub const BASELINE: &str = "ee/era5land_baseline.tif";
/// Median scaling factors: 2 bands of `f32` (`k_min`, `k_max`, NaN = none)
/// on a 1-degree grid ([`NEX`]).
pub const SCALING: &str = "ee/nexgddp_k.tif";

/// Search radius (ERA5-Land cells) for a coastal cell's gap.
pub const GAP_FILL_RADIUS: usize = 5;
/// Search radius (1-degree cells) for a scaling factor.
pub const K_FILL_RADIUS: usize = 2;

/// Bands of [`BASELINE`].
mod base_band {
    pub const COUNT: usize = 6;
}

/// A global raster's geometry: `w` x `h` cells of `deg` degrees, the
/// north-west corner of the first at (`west`, `north`); columns wrap.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Geom {
    w: usize,
    h: usize,
    deg: f64,
    west: f64,
    north: f64,
}

/// ERA5-Land's grid (3600 of its 3601 columns: the last repeats the first).
const E5L: Geom = Geom {
    w: 3600,
    h: 1801,
    deg: 0.1,
    west: -180.05,
    north: 90.05,
};
/// The scaling factors' 1-degree grid (90 N to 60 S): NEX-GDDP-CMIP6's
/// change comes from GCMs at about 1 degree or coarser.
const NEX: Geom = Geom {
    w: 360,
    h: 150,
    deg: 1.0,
    west: -180.0,
    north: 90.0,
};

/// A global raster.
struct Global {
    g: Geom,
    data: Vec<f32>,
}

impl Global {
    fn get(&self, col: i64, row: i64) -> Option<f32> {
        if row < 0 || row >= self.g.h as i64 {
            return None;
        }
        let col = col.rem_euclid(self.g.w as i64) as usize;
        let v = self.data[row as usize * self.g.w + col];
        v.is_finite().then_some(v)
    }

    /// The value at (col, row), else the nearest value within `radius`
    /// cells (ties north then west), with the distance in cells.
    fn nearest(&self, col: usize, row: usize, radius: usize) -> Option<(f32, f64)> {
        let (c0, r0) = (col as i64, row as i64);
        let r = radius as i64;
        let mut best: Option<(i64, f32)> = None;
        for dy in -r..=r {
            for dx in -r..=r {
                let d2 = dx * dx + dy * dy;
                if d2 > r * r || best.is_some_and(|(b, _)| b <= d2) {
                    continue;
                }
                if let Some(v) = self.get(c0 + dx, r0 + dy) {
                    best = Some((d2, v));
                }
            }
        }
        best.map(|(d2, v)| (v, (d2 as f64).sqrt()))
    }

    /// Bilinear value at a point from the valid ones of the four
    /// surrounding cell centres (weights renormalised); else the nearest
    /// value within `radius` cells of the nearest cell, flagged `true`.
    fn bilinear(&self, lat: f64, lon: f64, radius: usize) -> Option<(f32, bool)> {
        let fx = (lon - self.g.west) / self.g.deg - 0.5;
        let fy = (self.g.north - lat) / self.g.deg - 0.5;
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = ((fx - x0) as f32, (fy - y0) as f32);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let (mut sum, mut wsum) = (0f32, 0f32);
        for (dx, dy, w) in [
            (0, 0, (1.0 - tx) * (1.0 - ty)),
            (1, 0, tx * (1.0 - ty)),
            (0, 1, (1.0 - tx) * ty),
            (1, 1, tx * ty),
        ] {
            if let Some(v) = self.get(x0 + dx, y0 + dy) {
                sum += w * v;
                wsum += w;
            }
        }
        if wsum > 1e-6 {
            return Some((sum / wsum, false));
        }
        let col = usize::try_from(fx.round().rem_euclid(self.g.w as f64) as i64).ok()?;
        let row = usize::try_from((fy.round() as i64).clamp(0, self.g.h as i64 - 1)).ok()?;
        self.nearest(col, row, radius).map(|(v, _)| (v, true))
    }
}

/// Read every band of a global `f32` GeoTIFF (NaN = no data), checking it
/// has geometry `g`.
fn read_global(path: &Path, g: Geom) -> Result<Vec<Global>> {
    let t = GeoTiff::open(path)?;
    if (t.width() as usize, t.height() as usize) != (g.w, g.h) {
        return Err(PipelineError::Data(format!(
            "{}: {}x{}, expected {}x{}",
            path.display(),
            t.width(),
            t.height(),
            g.w,
            g.h
        )));
    }
    if let Some(x) = t.geo_transform() {
        let ok = (x.x_origin - g.west).abs() < 1e-6
            && (x.y_origin - g.north).abs() < 1e-6
            && (x.pixel_width - g.deg).abs() < 1e-9;
        if !ok {
            return Err(PipelineError::Data(format!(
                "{}: origin ({}, {}) and cell {}, expected {g:?}",
                path.display(),
                x.x_origin,
                x.y_origin,
                x.pixel_width
            )));
        }
    }
    (0..t.band_count())
        .map(|b| {
            let data = t.read_band_f32(b)?;
            Ok(Global { g, data })
        })
        .collect()
}

/// Counts of one tile, summed over the build.
#[derive(Default)]
struct Counts {
    cells: u64,
    gap_filled: u64,
    gap_missing: u64,
    k_missing: u64,
    clamped_min: u64,
    clamped_max: u64,
}

impl Counts {
    fn add(&mut self, o: &Self) {
        self.cells += o.cells;
        self.gap_filled += o.gap_filled;
        self.gap_missing += o.gap_missing;
        self.k_missing += o.k_missing;
        self.clamped_min += o.clamped_min;
        self.clamped_max += o.clamped_max;
    }
}

struct Inputs {
    gap_min: Global,
    gap_max: Global,
    e5l_tmin_cold: Global,
    e5l_tmax_warm: Global,
    k_min: Global,
    k_max: Global,
}

/// Samples for the report: (Köppen group, gap_min, gap_max) of slot 0,
/// ERA5-Land minus grid for tmin_cold and tmax_warm, and k.
#[derive(Default)]
struct Samples {
    by_group: Vec<(char, f32, f32)>,
    d_tmin_cold: Vec<f32>,
    d_tmax_warm: Vec<f32>,
    k_min: Vec<f32>,
    k_max: Vec<f32>,
    /// Per slot: tmin_extreme change and tmax_extreme change from slot 0.
    change: Vec<(usize, f32, f32)>,
}

fn decode(tile: &Tile, i: usize, var: Variable, slot: usize) -> f32 {
    var.decode(tile.get(i, crate::model::band(var.index(), slot)))
        .map_or(f32::NAN, |v| v as f32)
}

fn build_tile(tile: &Tile, inp: &Inputs) -> (Vec<i16>, Counts, Samples) {
    let id = tile.id();
    let mut data = vec![NODATA; TILE_LEN * EXTREMES_BANDS];
    let (mut n, mut s) = (Counts::default(), Samples::default());
    for j in 0..TILE_LEN {
        if !tile.is_complete(j) {
            continue;
        }
        let cell = id.cell(j);
        let (lat, lon) = (cell.center_lat(), cell.center_lon());
        let (gmin, gmax) = match (
            inp.gap_min.bilinear(lat, lon, GAP_FILL_RADIUS),
            inp.gap_max.bilinear(lat, lon, GAP_FILL_RADIUS),
        ) {
            (Some(a), Some(b)) => {
                n.gap_filled += u64::from(a.1 || b.1);
                (a.0, b.0)
            }
            _ => {
                n.gap_missing += 1;
                continue;
            }
        };
        let (kmin, kmax) = match (
            inp.k_min.bilinear(lat, lon, K_FILL_RADIUS),
            inp.k_max.bilinear(lat, lon, K_FILL_RADIUS),
        ) {
            (Some(a), Some(b)) => (a.0, b.0),
            _ => {
                n.k_missing += 1;
                (0.0, 0.0)
            }
        };
        n.cells += 1;
        let tcold0 = decode(tile, j, Variable::TmeanCold, 0);
        let twarm0 = decode(tile, j, Variable::TmeanWarm, 0);
        let mut first = (0f32, 0f32);
        for slot in 0..SLOTS.len() {
            let tmin_cold = decode(tile, j, Variable::TminCold, slot);
            let tmax_warm = decode(tile, j, Variable::TmaxWarm, slot);
            let dcold = decode(tile, j, Variable::TmeanCold, slot) - tcold0;
            let dwarm = decode(tile, j, Variable::TmeanWarm, slot) - twarm0;
            let mut tn = tmin_cold + gmin + kmin * dcold;
            let mut tx = tmax_warm + gmax + kmax * dwarm;
            if tn > tmin_cold {
                tn = tmin_cold;
                n.clamped_min += 1;
            }
            if tx < tmax_warm {
                tx = tmax_warm;
                n.clamped_max += 1;
            }
            let (a, b) = (
                Extreme::TminExtreme.encode(tn),
                Extreme::TmaxExtreme.encode(tx),
            );
            data[j * EXTREMES_BANDS + band(Extreme::TminExtreme, slot)] = a;
            data[j * EXTREMES_BANDS + band(Extreme::TmaxExtreme, slot)] = b;
            if slot == 0 {
                first = (tn, tx);
            } else {
                s.change.push((slot, tn - first.0, tx - first.1));
            }
        }
        let code = u8::try_from(tile.get(j, 0)).unwrap_or(0);
        if let Some(grp) = koppen::group(code) {
            s.by_group.push((grp, gmin, gmax));
        }
        let grid_tmin = decode(tile, j, Variable::TminCold, 0);
        let grid_tmax = decode(tile, j, Variable::TmaxWarm, 0);
        if let Some((v, false)) = inp.e5l_tmin_cold.bilinear(lat, lon, 0) {
            s.d_tmin_cold.push(v - grid_tmin);
        }
        if let Some((v, false)) = inp.e5l_tmax_warm.bilinear(lat, lon, 0) {
            s.d_tmax_warm.push(v - grid_tmax);
        }
        s.k_min.push(kmin);
        s.k_max.push(kmax);
    }
    (data, n, s)
}

/// Build the manifest `LayerSpec` for `extremes`.
#[must_use]
pub fn layer_spec(tiles: Vec<TileSpec>) -> LayerSpec {
    LayerSpec {
        id: DIR.into(),
        description: "Coldest night and hottest day of a typical year (mean annual extremes of the daily minimum and maximum temperature) for every climate slot".into(),
        cell_deg: CELL_DEG,
        tile_cells: TILE_CELLS as u32,
        directory: DIR.into(),
        encoding: crate::manifest::Encoding {
            container: "Cloud Optimized GeoTIFF (EPSG:4326, one 112 x 112 internal tile, no overviews)".into(),
            sample_format: "int16".into(),
            nodata: NODATA,
            compression: "deflate".into(),
            interleave: "pixel".into(),
            bands: EXTREMES_BANDS as u32,
            band_order: "band (0-based) = band_index * len(slots) + slot_index".into(),
        },
        bands: EXTREMES
            .iter()
            .map(|e| VariableSpec {
                id: e.id().into(),
                unit: "°C".into(),
                scale: crate::extremes::SCALE,
                offset: 0.0,
                description: e.description().into(),
                months: None,
            })
            .collect(),
        sources: vec!["era5-land".into(), "nex-gddp-cmip6".into()],
        tiles,
        notes: vec![
            "Typical, not worst case: each value is the mean over the period of every year's lowest daily minimum (highest daily maximum). A heatwave or cold-wave year goes several degrees beyond it (summer 2026 in France: 40 °C or more over about 40% of the country).".into(),
            "On the climate grid: a cell has values where the climate tile's cell is complete, so read the cell the climate lookup used (no search of its own).".into(),
            "The extremes are offsets from tmin_cold and tmax_warm and inherit their errors (Singapore: tmax_warm 29.2 °C where Changi records about 32, so a hottest day of about 31 °C instead of about 34).".into(),
            "ERA5-Land (about 9 km) smooths cold-air pools: in hilly country the coldest night can be 2-3 °C milder than at a nearby station (Lormes, France: -8.3 °C against about -10 to -12 °C from Météo-France stations). Treat tmin_extreme as an estimate.".into(),
            "Future change: Earth Engine's NEX-GDDP-CMIP6 has SSP2-4.5 and SSP5-8.5 only; the change of each gap per degree of warming is fitted on both and applied to every slot's own warming in Beck et al. 2023 (pattern scaling), so SSP1-2.6 and SSP3-7.0 are scaled, not simulated. In Western Europe, observed hot extremes have risen faster than CMIP6 models simulate (Vautard et al. 2023): in ERA5-Land the hottest day rose about 1.1-1.4 °C per decade over 1991-2025 in France, so recent years already reach the 2041-2070 values there (Lormes: a 2025 trend level of 35.1 °C, the 2041-2070 SSP2-4.5 value). Treat the future hottest days there as a lower bound. No future value accounts for regime shifts or tipping points.".into(),
            format!("Coasts: a cell without an ERA5-Land value takes the nearest within {GAP_FILL_RADIUS} cells; south of 60 S (no NEX-GDDP data) the gaps do not change with warming."),
            "The USDA hardiness zone is derived at lookup from tmin_extreme (10 °F bands, a/b halves).".into(),
        ],
        slots: SLOTS
            .iter()
            .map(|s| SlotSpec {
                period: s.period.label().into(),
                scenario: s.scenario.id().into(),
                scenario_label: s.scenario.label().into(),
            })
            .collect(),
        version: Some(env!("CARGO_PKG_VERSION").into()),
    }
}

fn group_stats(samples: &[(char, f32, f32)]) -> Value {
    let mut out = serde_json::Map::new();
    for g in ['A', 'B', 'C', 'D', 'E'] {
        let mut mins: Vec<f32> = samples.iter().filter(|s| s.0 == g).map(|s| s.1).collect();
        let mut maxs: Vec<f32> = samples.iter().filter(|s| s.0 == g).map(|s| s.2).collect();
        out.insert(
            g.to_string(),
            json!({ "gap_min_c": diff_stats(&mut mins), "gap_max_c": diff_stats(&mut maxs) }),
        );
    }
    Value::Object(out)
}

/// Read the two Earth Engine rasters.
fn load_inputs(dir: &Path) -> Result<Inputs> {
    let base = read_global(&sources::pinned(dir, BASELINE)?, E5L)?;
    let k = read_global(&sources::pinned(dir, SCALING)?, NEX)?;
    let (nb, nk) = (base.len(), k.len());
    let bad = || {
        PipelineError::Data(format!(
            "extremes sources: {nb} baseline bands (expected {}), {nk} scaling bands (expected 2)",
            base_band::COUNT
        ))
    };
    let [gap_min, gap_max, _tnn, _txx, e5l_tmin_cold, e5l_tmax_warm]: [Global; base_band::COUNT] =
        base.try_into().map_err(|_| bad())?;
    let [k_min, k_max]: [Global; 2] = k.try_into().map_err(|_| bad())?;
    Ok(Inputs {
        gap_min,
        gap_max,
        e5l_tmin_cold,
        e5l_tmax_warm,
        k_min,
        k_max,
    })
}

/// Run the `extremes` stage (after `tiles`).
///
/// # Errors
/// Missing release or sources, writer or I/O errors.
pub fn stage_extremes(cfg: &BuildConfig) -> Result<()> {
    if layers::has_layer(&cfg.out, DIR) && !cfg.force {
        eprintln!("extremes: layer exists, skipping");
        return Ok(());
    }
    let t0 = Instant::now();
    let m = layers::read_manifest(&cfg.out)?;
    let inp = load_inputs(&cfg.sources)?;
    let built: Vec<(ExtremesTile, Counts, Samples)> = m
        .tiles
        .clone()
        .into_par_iter()
        .map(|spec| {
            let p = cfg.out.join(&spec.path);
            let bytes = std::fs::read(&p).map_err(|e| PipelineError::io(&p, e))?;
            let tile = Tile::decode(&bytes)?;
            let (data, n, s) = build_tile(&tile, &inp);
            Ok((ExtremesTile::new(tile.id(), data)?, n, s))
        })
        .collect::<Result<_>>()?;
    let mut counts = Counts::default();
    let mut all = Samples::default();
    let mut tiles = Vec::new();
    for (t, n, s) in built {
        counts.add(&n);
        all.by_group.extend(s.by_group);
        all.d_tmin_cold.extend(s.d_tmin_cold);
        all.d_tmax_warm.extend(s.d_tmax_warm);
        all.k_min.extend(s.k_min);
        all.k_max.extend(s.k_max);
        all.change.extend(s.change);
        if t.cells() > 0 {
            tiles.push(t);
        }
    }
    let specs = write_tiles(&cfg.out, tiles)?;
    let mut change = serde_json::Map::new();
    for (slot, s) in SLOTS.iter().enumerate().skip(1) {
        let mut a: Vec<f32> = all
            .change
            .iter()
            .filter(|c| c.0 == slot)
            .map(|c| c.1)
            .collect();
        let mut b: Vec<f32> = all
            .change
            .iter()
            .filter(|c| c.0 == slot)
            .map(|c| c.2)
            .collect();
        change.insert(
            super::build::slot_key(*s),
            json!({ "tmin_extreme_change_c": diff_stats(&mut a), "tmax_extreme_change_c": diff_stats(&mut b) }),
        );
    }
    let report = json!({
        "cells": counts.cells,
        "gap_filled_cells": counts.gap_filled,
        "gap_missing_cells": counts.gap_missing,
        "scaling_missing_cells": counts.k_missing,
        "clamped_tmin_values": counts.clamped_min,
        "clamped_tmax_values": counts.clamped_max,
        "gaps_by_koppen_group": group_stats(&all.by_group),
        "era5land_minus_grid_tmin_cold_c": diff_stats(&mut all.d_tmin_cold),
        "era5land_minus_grid_tmax_warm_c": diff_stats(&mut all.d_tmax_warm),
        "k_min": diff_stats(&mut all.k_min),
        "k_max": diff_stats(&mut all.k_max),
        "change_from_1991_2020": change,
        "sizes": layers::size_report(&specs),
        "seconds": t0.elapsed().as_secs_f64().round(),
    });
    eprintln!("extremes: {}", report["sizes"]);
    layers::register(&cfg.out, layer_spec(specs), report)
}

fn write_tiles(out: &Path, tiles: Vec<ExtremesTile>) -> Result<Vec<TileSpec>> {
    let dir = out.join(DIR);
    std::fs::create_dir_all(&dir).map_err(|e| PipelineError::io(&dir, e))?;
    let mut specs: Vec<TileSpec> = tiles
        .into_par_iter()
        .map(|t| {
            let id: TileId = t.id();
            let path = dir.join(id.file_name());
            let bytes = cog::write_extremes_tile_checked(&t, &path)?;
            Ok(TileSpec {
                name: id.name(),
                path: format!("{DIR}/{}", id.file_name()),
                bytes: bytes.len() as u64,
                sha256: super::sha256_bytes(&bytes),
                complete_cells: u32::try_from(t.cells()).unwrap_or(u32::MAX),
            })
        })
        .collect::<Result<_>>()?;
    specs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(specs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tile::synthetic;

    /// A w x h raster covering the globe from 180 W, 90 N.
    fn global(w: usize, h: usize, f: impl Fn(usize, usize) -> f32) -> Global {
        let data = (0..w * h).map(|i| f(i % w, i / w)).collect();
        let g = Geom {
            w,
            h,
            deg: 360.0 / w as f64,
            west: -180.0,
            north: 90.0,
        };
        Global { g, data }
    }

    #[test]
    fn nearest_and_bilinear() {
        let g = global(8, 4, |c, r| if c == 3 && r == 1 { 5.0 } else { f32::NAN });
        assert_eq!(g.nearest(3, 1, 0), Some((5.0, 0.0)));
        assert_eq!(g.nearest(5, 1, 2), Some((5.0, 2.0)));
        assert_eq!(g.nearest(6, 1, 2), None);
        // Wraps at the antimeridian.
        let w = global(8, 4, |c, _| if c == 0 { 1.0 } else { f32::NAN });
        assert_eq!(w.nearest(7, 2, 1), Some((1.0, 1.0)));
        // Bilinear on a plane is exact; NaN neighbours are left out.
        let p = global(8, 4, |c, r| c as f32 + 10.0 * r as f32);
        // Halfway between the centres of (0, 0) and (1, 1): lon -135, lat 45.
        let (v, filled) = p.bilinear(45.0, -135.0, 1).unwrap();
        assert!((v - 5.5).abs() < 1e-5 && !filled);
        let one = global(8, 4, |c, r| if (c, r) == (1, 1) { 7.0 } else { f32::NAN });
        assert_eq!(one.bilinear(45.0, -135.0, 1), Some((7.0, false)));
        // No neighbour: the nearest within the radius, flagged.
        let far = global(8, 4, |c, r| if (c, r) == (3, 1) { 2.0 } else { f32::NAN });
        assert_eq!(far.bilinear(45.0, -135.0, 2), Some((2.0, true)));
        let none = global(8, 4, |_, _| f32::NAN);
        assert_eq!(none.bilinear(45.0, -135.0, 3), None);
        // ERA5-Land's half-cell offset: a canopi cell centre is the mean
        // of its four ERA5-Land neighbours.
        let e = Global {
            g: E5L,
            data: (0..E5L.w * E5L.h).map(|i| (i % E5L.w) as f32).collect(),
        };
        let (v, _) = e.bilinear(47.05, 3.05, 0).unwrap();
        assert!((v - 1830.5).abs() < 1e-3, "{v}");
    }

    #[test]
    fn extremes_follow_the_climate_cell_and_clamp() {
        let id = TileId::parse("n50e000").unwrap();
        let tile = synthetic::tile(id);
        let flat = |g: Geom, v: f32| Global {
            g,
            data: vec![v; g.w * g.h],
        };
        let inp = Inputs {
            gap_min: flat(E5L, -9.0),
            gap_max: flat(E5L, 10.0),
            e5l_tmin_cold: flat(E5L, f32::NAN),
            e5l_tmax_warm: flat(E5L, f32::NAN),
            k_min: flat(NEX, 0.5),
            k_max: flat(NEX, -30.0),
        };
        let (data, counts, _) = build_tile(&tile, &inp);
        let t = ExtremesTile::new(id, data).unwrap();
        assert_eq!(t.cells(), tile.complete_cells());
        assert_eq!(counts.cells as usize, tile.complete_cells());
        let j = (0..TILE_LEN).find(|&j| tile.is_complete(j)).unwrap();
        let tmin0 = decode(&tile, j, Variable::TminCold, 0);
        let got = Extreme::TminExtreme
            .decode(t.get(j, band(Extreme::TminExtreme, 0)))
            .unwrap();
        assert!(
            (got - f64::from(tmin0 - 9.0)).abs() < 0.051,
            "{got} vs {tmin0}"
        );
        // A strongly negative k_max with warming would put the hottest day
        // below tmax_warm: clamped.
        assert!(counts.clamped_max > 0);
        for s in 0..SLOTS.len() {
            let tx = Extreme::TmaxExtreme
                .decode(t.get(j, band(Extreme::TmaxExtreme, s)))
                .unwrap();
            assert!(tx + 0.051 >= f64::from(decode(&tile, j, Variable::TmaxWarm, s)));
        }
        let incomplete = (0..TILE_LEN).find(|&j| !tile.is_complete(j));
        if let Some(j) = incomplete {
            assert!(!t.has(j));
        }
    }
}
