//! The `extremes-tail` stage: static 1-in-10-year tail offsets (`tmin_p10_gap`,
//! `tmax_p90_gap`), annual heat-stress days (`heat_days_35c`), and recent
//! warming trend (`recent_trend_c`, 2015–2025 vs 1991–2020) from the Earth
//! Engine ERA5-Land raster `ee/era5land_extremes_tail.tif`.
//!
//! Bilinearly interpolated from ERA5-Land's 0.1-degree grid to canopi's cell
//! centres, with nearest-within-5-cells coastal fallback.
//! Method: `docs/decisions/0006-tail-extremes-frost-calendar-analogues.md`.

use std::path::Path;
use std::time::Instant;

use rayon::prelude::{IntoParallelIterator, ParallelIterator};
use serde_json::json;
use wbgeotiff::GeoTiff;

use super::build::{BuildConfig, diff_stats};
use super::{PipelineError, Result, cog, layers};
use crate::extremes_tail::{DIR, ExtremesTailTile, SCALE, TAIL_BAND_LIST, TAIL_BANDS, TailBand};
use crate::grid::{CELL_DEG, TILE_CELLS, TILE_LEN, TileId};
use crate::manifest::{Encoding, LayerSpec, TileSpec, VariableSpec};
use crate::model::NODATA;
use crate::tile::Tile;

/// ERA5-Land tail-extremes input raster (4 × `f32` bands on ERA5-Land 0.1° grid).
pub const SOURCE_PATH: &str = "ee/era5land_extremes_tail.tif";

/// Search radius (ERA5-Land cells) for coastal fallback.
pub const FILL_RADIUS: usize = 5;

const E5L_W: usize = 3600;
const E5L_H: usize = 1801;
const E5L_DEG: f64 = 0.1;
const E5L_WEST: f64 = -180.05;
const E5L_NORTH: f64 = 90.05;

struct E5lBand {
    data: Vec<f32>,
}

impl E5lBand {
    fn get(&self, col: i64, row: i64) -> Option<f32> {
        if row < 0 || row >= E5L_H as i64 {
            return None;
        }
        let c = col.rem_euclid(E5L_W as i64) as usize;
        let v = self.data[row as usize * E5L_W + c];
        v.is_finite().then_some(v)
    }

    fn nearest(&self, col: usize, row: usize, radius: usize) -> Option<f32> {
        let (c0, r0, r) = (col as i64, row as i64, radius as i64);
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
        best.map(|(_, v)| v)
    }

    fn bilinear(&self, lat: f64, lon: f64, radius: usize) -> Option<(f32, bool)> {
        let fx = (lon - E5L_WEST) / E5L_DEG - 0.5;
        let fy = (E5L_NORTH - lat) / E5L_DEG - 0.5;
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
        let col = usize::try_from(fx.round().rem_euclid(E5L_W as f64) as i64).ok()?;
        let row = usize::try_from((fy.round() as i64).clamp(0, E5L_H as i64 - 1)).ok()?;
        self.nearest(col, row, radius).map(|v| (v, true))
    }
}

fn read_input(path: &Path) -> Result<[E5lBand; TAIL_BANDS]> {
    let t = GeoTiff::open(path)?;
    if (t.width() as usize, t.height() as usize) != (E5L_W, E5L_H) {
        return Err(PipelineError::Data(format!(
            "{}: {}x{}, expected {E5L_W}x{E5L_H}",
            path.display(),
            t.width(),
            t.height()
        )));
    }
    if t.band_count() != TAIL_BANDS {
        return Err(PipelineError::Data(format!(
            "{}: {} bands, expected {TAIL_BANDS}",
            path.display(),
            t.band_count()
        )));
    }
    let mut bands = Vec::with_capacity(TAIL_BANDS);
    for b in 0..TAIL_BANDS {
        bands.push(E5lBand {
            data: t.read_band_f32(b)?,
        });
    }
    bands
        .try_into()
        .map_err(|_| PipelineError::Data("extremes_tail band count".into()))
}

#[derive(Default)]
struct TileReport {
    cells: u64,
    filled: u64,
    missing: u64,
    tmin_gap: Vec<f32>,
    tmax_gap: Vec<f32>,
    heat_days: Vec<f32>,
    trend: Vec<f32>,
}

fn build_tile(tile: &Tile, bands: &[E5lBand; TAIL_BANDS]) -> (Vec<i16>, TileReport) {
    let id = tile.id();
    let mut data = vec![NODATA; TILE_LEN * TAIL_BANDS];
    let mut rep = TileReport::default();
    for j in 0..TILE_LEN {
        if !tile.is_complete(j) {
            continue;
        }
        let cell = id.cell(j);
        let (lat, lon) = (cell.center_lat(), cell.center_lon());
        let Some((g_min, f0)) = bands[0].bilinear(lat, lon, FILL_RADIUS) else {
            rep.missing += 1;
            continue;
        };
        let Some((g_max, f1)) = bands[1].bilinear(lat, lon, FILL_RADIUS) else {
            rep.missing += 1;
            continue;
        };
        let Some((hd35, f2)) = bands[2].bilinear(lat, lon, FILL_RADIUS) else {
            rep.missing += 1;
            continue;
        };
        let Some((trend, f3)) = bands[3].bilinear(lat, lon, FILL_RADIUS) else {
            rep.missing += 1;
            continue;
        };
        let g_min = g_min.min(0.0);
        let g_max = g_max.max(0.0);
        let hd35 = hd35.clamp(0.0, 366.0);
        let base = j * TAIL_BANDS;
        data[base + TailBand::TminP10Gap.index()] = TailBand::TminP10Gap.encode(g_min);
        data[base + TailBand::TmaxP90Gap.index()] = TailBand::TmaxP90Gap.encode(g_max);
        data[base + TailBand::HeatDays35c.index()] = TailBand::HeatDays35c.encode(hd35);
        data[base + TailBand::RecentTrendC.index()] = TailBand::RecentTrendC.encode(trend);
        rep.cells += 1;
        rep.filled += u64::from(f0 || f1 || f2 || f3);
        if j % 10 == 0 {
            rep.tmin_gap.push(g_min);
            rep.tmax_gap.push(g_max);
            rep.heat_days.push(hd35);
            rep.trend.push(trend);
        }
    }
    (data, rep)
}

fn layer_spec(tiles: Vec<TileSpec>) -> LayerSpec {
    LayerSpec {
        id: DIR.into(),
        description: "Static 1-in-10-year tail offsets (tmin_p10_gap, tmax_p90_gap), annual heat-stress days >= 35 °C, and recent 2015-2025 vs 1991-2020 warming trend on the 0.1-degree climate grid.".into(),
        cell_deg: CELL_DEG,
        tile_cells: TILE_CELLS as u32,
        directory: DIR.into(),
        encoding: Encoding {
            container: "Cloud Optimized GeoTIFF (EPSG:4326, one 112 x 112 internal tile, no overviews)".into(),
            sample_format: "int16".into(),
            nodata: NODATA,
            compression: "deflate".into(),
            interleave: "pixel".into(),
            bands: TAIL_BANDS as u32,
            band_order: "band (1-based) = index in the bands list + 1".into(),
        },
        bands: TAIL_BAND_LIST
            .iter()
            .map(|b| VariableSpec {
                id: b.id().into(),
                unit: b.unit().into(),
                scale: SCALE,
                offset: 0.0,
                description: b.description().into(),
                months: None,
            })
            .collect(),
        sources: vec!["era5-land-extremes-tail".into()],
        tiles,
        notes: vec![
            "No spatial search: read at the climate lookup's cell_lat/cell_lon in the same tile.".into(),
            "For any extremes slot s, 1-in-10-year extremes are tmin_extreme[s] + tmin_p10_gap and tmax_extreme[s] + tmax_p90_gap.".into(),
        ],
        slots: Vec::new(),
        version: Some(env!("CARGO_PKG_VERSION").into()),
    }
}

/// Run the `extremes-tail` stage.
///
/// # Errors
/// Missing climate tiles or ERA5-Land input raster, writer or I/O errors.
pub fn stage_extremes_tail(cfg: &BuildConfig) -> Result<()> {
    if !cfg.force && layers::has_layer(&cfg.out, DIR) {
        eprintln!("extremes-tail: layer already in manifest, skipping");
        return Ok(());
    }
    let t0 = Instant::now();
    let m = layers::read_manifest(&cfg.out)?;
    let bands = read_input(&cfg.sources.join(SOURCE_PATH))?;
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
            let (data, rep) = build_tile(&climate, &bands);
            if rep.cells == 0 {
                return Ok((None, rep));
            }
            let tile = ExtremesTailTile::new(id, data)?;
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
    let (mut tmin_g, mut tmax_g, mut hd, mut tr) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (ts, rep) in built {
        if let Some(s) = ts {
            specs.push(s);
        }
        cells += rep.cells;
        filled += rep.filled;
        missing += rep.missing;
        tmin_g.extend(rep.tmin_gap);
        tmax_g.extend(rep.tmax_gap);
        hd.extend(rep.heat_days);
        tr.extend(rep.trend);
    }
    specs.sort_by(|a, b| a.name.cmp(&b.name));
    let sizes = layers::size_report(&specs);
    let report = json!({
        "cells": cells,
        "filled_coastal_cells": filled,
        "missing_cells": missing,
        "tmin_p10_gap_c": diff_stats(&mut tmin_g),
        "tmax_p90_gap_c": diff_stats(&mut tmax_g),
        "heat_days_35c": diff_stats(&mut hd),
        "recent_trend_c": diff_stats(&mut tr),
        "sizes": sizes,
    });
    layers::register(&cfg.out, layer_spec(specs), report)?;
    eprintln!(
        "extremes-tail: {cells} cells ({filled} coastal-filled, {missing} missing) in {:.1}s",
        t0.elapsed().as_secs_f64()
    );
    Ok(())
}
