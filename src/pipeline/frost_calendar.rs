//! The `frost-calendar` stage: static hemisphere-aware frost dates, frost-free
//! season length, frost-year percentage, and false-spring killing-freeze risk
//! from the Earth Engine ERA5-Land raster `ee/era5land_frost_calendar.tif`.
//!
//! Uses nearest-neighbor regridding (never bilinear across `0`/`366` sentinels
//! or Dec/Jan wraps) with nearest-within-5-cells coastal fallback.
//! Method: `docs/decisions/0006-tail-extremes-frost-calendar-analogues.md`.

use std::path::Path;
use std::time::Instant;

use rayon::prelude::{IntoParallelIterator, ParallelIterator};
use serde_json::json;
use wbgeotiff::GeoTiff;

use super::build::{BuildConfig, diff_stats};
use super::{PipelineError, Result, cog, layers};
use crate::frost_calendar::{
    DIR, FROST_BAND_LIST, FROST_BANDS, FrostBand, FrostCalendarTile, YEAR_ROUND_FROST_SENTINEL,
};
use crate::grid::{CELL_DEG, TILE_CELLS, TILE_LEN, TileId};
use crate::manifest::{Encoding, LayerSpec, TileSpec, VariableSpec};
use crate::model::NODATA;
use crate::tile::Tile;

/// ERA5-Land frost-calendar input raster (6 × `int16` bands on ERA5-Land 0.1° grid).
pub const SOURCE_PATH: &str = "ee/era5land_frost_calendar.tif";

/// Search radius (ERA5-Land cells) for coastal fallback.
pub const FILL_RADIUS: usize = 5;

const E5L_W: usize = 3600;
const E5L_H: usize = 1801;
const E5L_DEG: f64 = 0.1;
const E5L_WEST: f64 = -180.05;
const E5L_NORTH: f64 = 90.05;

struct E5lFrost {
    bands: [Vec<i16>; FROST_BANDS],
}

impl E5lFrost {
    fn get_cell(&self, col: i64, row: i64) -> Option<[i16; FROST_BANDS]> {
        if row < 0 || row >= E5L_H as i64 {
            return None;
        }
        let c = col.rem_euclid(E5L_W as i64) as usize;
        let idx = row as usize * E5L_W + c;
        let mut out = [NODATA; FROST_BANDS];
        for (b, slot) in out.iter_mut().enumerate() {
            let v = self.bands[b][idx];
            if v == NODATA {
                return None;
            }
            *slot = v;
        }
        Some(out)
    }

    fn nearest(&self, lat: f64, lon: f64, radius: usize) -> Option<([i16; FROST_BANDS], bool)> {
        let fx = (lon - E5L_WEST) / E5L_DEG - 0.5;
        let fy = (E5L_NORTH - lat) / E5L_DEG - 0.5;
        let c0 = fx.round().rem_euclid(E5L_W as f64) as i64;
        let r0 = (fy.round() as i64).clamp(0, E5L_H as i64 - 1);
        if let Some(v) = self.get_cell(c0, r0) {
            return Some((v, false));
        }
        let r = radius as i64;
        let mut best: Option<(i64, [i16; FROST_BANDS])> = None;
        for dy in -r..=r {
            for dx in -r..=r {
                let d2 = dx * dx + dy * dy;
                if d2 > r * r || best.is_some_and(|(b, _)| b <= d2) {
                    continue;
                }
                if let Some(v) = self.get_cell(c0 + dx, r0 + dy) {
                    best = Some((d2, v));
                }
            }
        }
        best.map(|(_, v)| (v, true))
    }
}

fn read_input(path: &Path) -> Result<E5lFrost> {
    let t = GeoTiff::open(path)?;
    if (t.width() as usize, t.height() as usize) != (E5L_W, E5L_H) {
        return Err(PipelineError::Data(format!(
            "{}: {}x{}, expected {E5L_W}x{E5L_H}",
            path.display(),
            t.width(),
            t.height()
        )));
    }
    if t.band_count() != FROST_BANDS {
        return Err(PipelineError::Data(format!(
            "{}: {} bands, expected {FROST_BANDS}",
            path.display(),
            t.band_count()
        )));
    }
    let mut bands = Vec::with_capacity(FROST_BANDS);
    for b in 0..FROST_BANDS {
        let f = t.read_band_f32(b)?;
        let v: Vec<i16> = f
            .into_iter()
            .map(|x| {
                if x.is_finite() {
                    x.round().clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
                } else {
                    NODATA
                }
            })
            .collect();
        bands.push(v);
    }
    Ok(E5lFrost {
        bands: bands
            .try_into()
            .map_err(|_| PipelineError::Data("frost_calendar band count".into()))?,
    })
}

#[derive(Default)]
struct TileReport {
    cells: u64,
    filled: u64,
    missing: u64,
    frost_free: u64,
    seasonal: u64,
    year_round: u64,
    ff_days: Vec<f32>,
    fs_pct: Vec<f32>,
}

fn build_tile(tile: &Tile, input: &E5lFrost) -> (Vec<i16>, TileReport) {
    let id = tile.id();
    let mut data = vec![NODATA; TILE_LEN * FROST_BANDS];
    let mut rep = TileReport::default();
    for j in 0..TILE_LEN {
        if !tile.is_complete(j) {
            continue;
        }
        let cell = id.cell(j);
        let Some((vals, was_filled)) =
            input.nearest(cell.center_lat(), cell.center_lon(), FILL_RADIUS)
        else {
            rep.missing += 1;
            continue;
        };
        let base = j * FROST_BANDS;
        data[base..base + FROST_BANDS].copy_from_slice(&vals);
        rep.cells += 1;
        rep.filled += u64::from(was_filled);
        let fy = vals[FrostBand::FrostYearPct.index()];
        let ff = vals[FrostBand::FrostFreeDays.index()];
        let ls = vals[FrostBand::LastSpringFrostDoy.index()];
        if fy == 0 {
            rep.frost_free += 1;
        } else if ff == 0 || ls == YEAR_ROUND_FROST_SENTINEL {
            rep.year_round += 1;
        } else {
            rep.seasonal += 1;
        }
        if j % 10 == 0 {
            rep.ff_days.push(f32::from(ff));
            rep.fs_pct
                .push(f32::from(vals[FrostBand::FalseSpringPct.index()]));
        }
    }
    (data, rep)
}

fn layer_spec(tiles: Vec<TileSpec>) -> LayerSpec {
    LayerSpec {
        id: DIR.into(),
        description: "Static hemisphere-aware frost calendar (1991-2020 ERA5-Land daily): last spring frost (median & P90), first autumn frost, frost-free days, frost-year %, and false-spring killing-freeze %.".into(),
        cell_deg: CELL_DEG,
        tile_cells: TILE_CELLS as u32,
        directory: DIR.into(),
        encoding: Encoding {
            container: "Cloud Optimized GeoTIFF (EPSG:4326, one 112 x 112 internal tile, no overviews)".into(),
            sample_format: "int16".into(),
            nodata: NODATA,
            compression: "deflate".into(),
            interleave: "pixel".into(),
            bands: FROST_BANDS as u32,
            band_order: "band (1-based) = index in the bands list + 1".into(),
        },
        bands: FROST_BAND_LIST
            .iter()
            .map(|b| VariableSpec {
                id: b.id().into(),
                unit: b.unit().into(),
                scale: 1.0,
                offset: 0.0,
                description: b.description().into(),
                months: None,
            })
            .collect(),
        sources: vec!["era5-land-frost-calendar".into()],
        tiles,
        notes: vec![
            "No spatial search: read at the climate lookup's cell_lat/cell_lon in the same tile.".into(),
            "Per-pixel frost-year anchored to thermal winter (opposite warmest month), handling NH, SH and equatorial highlands uniformly.".into(),
            "Sentinels 0 (frost-free) and 366 (year-round frost) are distinct from NODATA (-32768).".into(),
        ],
        slots: Vec::new(),
        version: Some(env!("CARGO_PKG_VERSION").into()),
    }
}

/// Run the `frost-calendar` stage.
///
/// # Errors
/// Missing climate tiles or ERA5-Land input raster, writer or I/O errors.
pub fn stage_frost_calendar(cfg: &BuildConfig) -> Result<()> {
    if !cfg.force && layers::has_layer(&cfg.out, DIR) {
        eprintln!("frost-calendar: layer already in manifest, skipping");
        return Ok(());
    }
    let t0 = Instant::now();
    let m = layers::read_manifest(&cfg.out)?;
    let input = read_input(&cfg.sources.join(SOURCE_PATH))?;
    let dir = cfg.out.join(DIR);
    std::fs::create_dir_all(&dir).map_err(|e| PipelineError::io(&dir, e))?;

    let climate_specs = m.tiles;
    let built: Vec<(Option<TileSpec>, TileReport)> = climate_specs
        .into_par_iter()
        .map(|spec| {
            let p = cfg.out.join(&spec.path);
            let raw = std::fs::read(&p).map_err(|e| PipelineError::io(&p, e))?;
            let climate = Tile::decode(&raw)?;
            let id: TileId = climate.id();
            let (data, rep) = build_tile(&climate, &input);
            if rep.cells == 0 {
                return Ok((None, rep));
            }
            let tile = FrostCalendarTile::new(id, data)?;
            let path = dir.join(id.file_name());
            let bytes = cog::write_raster_checked(tile.as_raster(), &path)?;
            let tspec = TileSpec {
                name: id.name(),
                path: format!("{DIR}/{}", id.file_name()),
                bytes: bytes.len() as u64,
                sha256: super::sha256_bytes(&bytes),
                complete_cells: u32::try_from(tile.cells()).unwrap_or(u32::MAX),
            };
            Ok((Some(tspec), rep))
        })
        .collect::<Result<_>>()?;

    let mut specs = Vec::new();
    let (mut cells, mut filled, mut missing) = (0u64, 0u64, 0u64);
    let (mut ff_cnt, mut seas_cnt, mut yr_cnt) = (0u64, 0u64, 0u64);
    let (mut ff_days, mut fs_pct) = (Vec::new(), Vec::new());
    for (ts, rep) in built {
        if let Some(s) = ts {
            specs.push(s);
        }
        cells += rep.cells;
        filled += rep.filled;
        missing += rep.missing;
        ff_cnt += rep.frost_free;
        seas_cnt += rep.seasonal;
        yr_cnt += rep.year_round;
        ff_days.extend(rep.ff_days);
        fs_pct.extend(rep.fs_pct);
    }
    specs.sort_by(|a, b| a.name.cmp(&b.name));
    let sizes = layers::size_report(&specs);
    let report = json!({
        "cells": cells,
        "filled_coastal_cells": filled,
        "missing_cells": missing,
        "frost_free_cells": ff_cnt,
        "seasonal_cells": seas_cnt,
        "year_round_frost_cells": yr_cnt,
        "frost_free_days": diff_stats(&mut ff_days),
        "false_spring_pct": diff_stats(&mut fs_pct),
        "sizes": sizes,
    });
    layers::register(&cfg.out, layer_spec(specs), report)?;
    eprintln!(
        "frost-calendar: {cells} cells ({ff_cnt} frost-free, {seas_cnt} seasonal, {yr_cnt} year-round) in {:.1}s",
        t0.elapsed().as_secs_f64()
    );
    Ok(())
}
