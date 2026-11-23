//! The `water-balance` stage: static 1991–2020 Climatic Water Deficit
//! (`cwd_mm`, `cwd_p90_mm`), wettest-month-anchored max consecutive dry days
//! (`cdd_max_days`), and peak-quarter Vapor Pressure Deficit (`vpd_summer_kpa`)
//! from Earth Engine inputs `ee/terraclimate_water_balance.tif` and
//! `ee/era5land_cdd.tif`.
//!
//! Bilinearly interpolated from the 0.1-degree ERA5-Land grid to canopi's cell
//! centres, with nearest-within-5-cells coastal fallback.
//! Method: `docs/decisions/0007-phenology-and-water-balance.md`.

use std::path::Path;
use std::time::Instant;

use rayon::prelude::{IntoParallelIterator, ParallelIterator};
use serde_json::json;
use wbgeotiff::GeoTiff;

use super::build::{BuildConfig, diff_stats};
use super::{PipelineError, Result, cog, layers};
use crate::grid::{CELL_DEG, TILE_CELLS, TILE_LEN, TileId};
use crate::manifest::{Encoding, LayerSpec, TileSpec, VariableSpec};
use crate::model::NODATA;
use crate::tile::Tile;
use crate::water_balance::{
    DIR, WATER_BALANCE_BAND_LIST, WATER_BALANCE_BANDS, WaterBalanceBand, WaterBalanceTile,
};

/// TerraClimate CWD + VPD input raster (3 × `int16` bands on ERA5-Land 0.1° grid:
/// `cwd_mm`, `cwd_p90_mm`, `vpd_summer_kpa * 100`).
pub const TC_SOURCE_PATH: &str = "ee/terraclimate_water_balance.tif";

/// ERA5-Land CDD input raster (1 × `int16` band on ERA5-Land 0.1° grid: `cdd_max_days`).
pub const CDD_SOURCE_PATH: &str = "ee/era5land_cdd.tif";

/// Search radius (ERA5-Land cells) for coastal fallback.
pub const FILL_RADIUS: usize = 5;

const E5L_W: usize = 3600;
const E5L_H: usize = 1801;
const E5L_DEG: f64 = 0.1;
const E5L_WEST: f64 = -180.05;
const E5L_NORTH: f64 = 90.05;

struct E5lBand {
    data: Vec<f32>,
    scale: f32,
}

impl E5lBand {
    fn get(&self, col: i64, row: i64) -> Option<f32> {
        if row < 0 || row >= E5L_H as i64 {
            return None;
        }
        let c = col.rem_euclid(E5L_W as i64) as usize;
        let v = self.data[row as usize * E5L_W + c];
        (v.is_finite() && (v - f32::from(NODATA)).abs() > 0.5).then_some(v * self.scale)
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

fn open_checked(path: &Path, expected_bands: usize) -> Result<GeoTiff> {
    let t = GeoTiff::open(path)?;
    if (t.width() as usize, t.height() as usize) != (E5L_W, E5L_H) {
        return Err(PipelineError::Data(format!(
            "{}: {}x{}, expected {E5L_W}x{E5L_H}",
            path.display(),
            t.width(),
            t.height()
        )));
    }
    if t.band_count() != expected_bands {
        return Err(PipelineError::Data(format!(
            "{}: {} bands, expected {expected_bands}",
            path.display(),
            t.band_count()
        )));
    }
    Ok(t)
}

fn read_inputs(tc_path: &Path, cdd_path: &Path) -> Result<[E5lBand; WATER_BALANCE_BANDS]> {
    let tc = open_checked(tc_path, 3)?;
    let cdd = open_checked(cdd_path, 1)?;
    Ok([
        E5lBand {
            data: tc.read_band_f32(0)?,
            scale: 1.0,
        },
        E5lBand {
            data: tc.read_band_f32(1)?,
            scale: 1.0,
        },
        E5lBand {
            data: cdd.read_band_f32(0)?,
            scale: 1.0,
        },
        E5lBand {
            data: tc.read_band_f32(2)?,
            scale: 0.01,
        },
    ])
}

#[derive(Default)]
struct TileReport {
    cells: u64,
    filled: u64,
    missing: u64,
    cwd: Vec<f32>,
    cdd: Vec<f32>,
    vpd: Vec<f32>,
}

fn build_tile(tile: &Tile, bands: &[E5lBand; WATER_BALANCE_BANDS]) -> (Vec<i16>, TileReport) {
    let id = tile.id();
    let mut data = vec![NODATA; TILE_LEN * WATER_BALANCE_BANDS];
    let mut rep = TileReport::default();
    for j in 0..TILE_LEN {
        if !tile.is_complete(j) {
            continue;
        }
        let cell = id.cell(j);
        let (lat, lon) = (cell.center_lat(), cell.center_lon());
        let Some((cwd, f0)) = bands[0].bilinear(lat, lon, FILL_RADIUS) else {
            rep.missing += 1;
            continue;
        };
        let Some((cwd_p90, f1)) = bands[1].bilinear(lat, lon, FILL_RADIUS) else {
            rep.missing += 1;
            continue;
        };
        let Some((cdd, f2)) = bands[2].bilinear(lat, lon, FILL_RADIUS) else {
            rep.missing += 1;
            continue;
        };
        let Some((vpd, f3)) = bands[3].bilinear(lat, lon, FILL_RADIUS) else {
            rep.missing += 1;
            continue;
        };
        let cwd = cwd.max(0.0);
        let cwd_p90 = cwd_p90.max(cwd);
        let cdd = cdd.clamp(0.0, 365.0);
        let vpd = vpd.clamp(0.0, 20.0);
        let base = j * WATER_BALANCE_BANDS;
        data[base + WaterBalanceBand::CwdMm.index()] = WaterBalanceBand::CwdMm.encode(cwd);
        data[base + WaterBalanceBand::CwdP90Mm.index()] =
            WaterBalanceBand::CwdP90Mm.encode(cwd_p90);
        data[base + WaterBalanceBand::CddMaxDays.index()] =
            WaterBalanceBand::CddMaxDays.encode(cdd);
        data[base + WaterBalanceBand::VpdSummerKpa.index()] =
            WaterBalanceBand::VpdSummerKpa.encode(vpd);
        rep.cells += 1;
        rep.filled += u64::from(f0 || f1 || f2 || f3);
        if j % 10 == 0 {
            rep.cwd.push(cwd);
            rep.cdd.push(cdd);
            rep.vpd.push(vpd);
        }
    }
    (data, rep)
}

fn layer_spec(tiles: Vec<TileSpec>) -> LayerSpec {
    LayerSpec {
        id: DIR.into(),
        description: "Static 1991-2020 water balance: TerraClimate CWD (mean & P90) and peak-quarter VPD paired with ERA5-Land wettest-month-anchored max consecutive dry days on the 0.1-degree climate grid.".into(),
        cell_deg: CELL_DEG,
        tile_cells: TILE_CELLS as u32,
        directory: DIR.into(),
        encoding: Encoding {
            container: "Cloud Optimized GeoTIFF (EPSG:4326, one 112 x 112 internal tile, no overviews)".into(),
            sample_format: "int16".into(),
            nodata: NODATA,
            compression: "deflate".into(),
            interleave: "pixel".into(),
            bands: WATER_BALANCE_BANDS as u32,
            band_order: "band (1-based) = index in the bands list + 1".into(),
        },
        bands: WATER_BALANCE_BAND_LIST
            .iter()
            .map(|b| VariableSpec {
                id: b.id().into(),
                unit: b.unit().into(),
                scale: b.scale(),
                offset: 0.0,
                months: None,
                description: b.description().into(),
            })
            .collect(),
        sources: vec![
            "terraclimate-water-balance".into(),
            "era5-land-cdd".into(),
        ],
        tiles,
        notes: vec![
            "No spatial search: read at the climate lookup's cell_lat/cell_lon in the same tile.".into(),
            "vpd_summer_kpa is peak-quarter (max circular 3-month rolling mean) climatological monthly-mean VPD from TerraClimate.".into(),
        ],
        slots: Vec::new(),
        version: Some(env!("CARGO_PKG_VERSION").into()),
    }
}

/// Build the `water_balance` layer under `cfg.out/water_balance` and register
/// it in `manifest.json`, `SHA256SUMS` and `build-report.json`.
///
/// # Errors
/// Missing climate tiles or input rasters, or I/O errors.
pub fn run(cfg: &BuildConfig) -> Result<()> {
    let t0 = Instant::now();
    if !cfg.force && layers::has_layer(&cfg.out, DIR) {
        eprintln!("water_balance: cached (use --force to rebuild)");
        return Ok(());
    }
    let manifest = layers::read_manifest(&cfg.out)?;
    let tc_path = cfg.sources.join(TC_SOURCE_PATH);
    let cdd_path = cfg.sources.join(CDD_SOURCE_PATH);
    let bands = read_inputs(&tc_path, &cdd_path)?;
    eprintln!(
        "water_balance: loaded {} + {}",
        tc_path.display(),
        cdd_path.display()
    );

    let out_dir = cfg.out.join(DIR);
    std::fs::create_dir_all(&out_dir).map_err(|e| PipelineError::io(&out_dir, e))?;

    let built: Vec<(Option<TileSpec>, TileReport)> = manifest
        .tiles
        .into_par_iter()
        .map(|spec| -> Result<(Option<TileSpec>, TileReport)> {
            let p = cfg.out.join(&spec.path);
            let raw = std::fs::read(&p).map_err(|e| PipelineError::io(&p, e))?;
            let tile = Tile::decode(&raw)?;
            let id: TileId = tile.id();
            let (data, rep) = build_tile(&tile, &bands);
            if rep.cells == 0 {
                return Ok((None, rep));
            }
            let wt = WaterBalanceTile::new(id, data)?;
            let path = out_dir.join(id.file_name());
            let bytes = cog::write_raster_checked(wt.as_raster(), &path)?;
            let tspec = TileSpec {
                name: id.name(),
                path: format!("{DIR}/{}", id.file_name()),
                bytes: bytes.len() as u64,
                sha256: super::sha256_bytes(&bytes),
                complete_cells: u32::try_from(wt.cells()).unwrap_or(u32::MAX),
            };
            Ok((Some(tspec), rep))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut tiles = Vec::new();
    let (mut cells, mut filled, mut missing) = (0u64, 0u64, 0u64);
    let (mut cwd, mut cdd, mut vpd) = (Vec::new(), Vec::new(), Vec::new());
    for (spec, rep) in built {
        if let Some(s) = spec {
            tiles.push(s);
        }
        cells += rep.cells;
        filled += rep.filled;
        missing += rep.missing;
        cwd.extend(rep.cwd);
        cdd.extend(rep.cdd);
        vpd.extend(rep.vpd);
    }
    tiles.sort_by(|a, b| a.name.cmp(&b.name));
    let sizes = layers::size_report(&tiles);

    let report = json!({
        "cells": cells,
        "coastal_filled_cells": filled,
        "missing_cells": missing,
        "cwd_mm": diff_stats(&mut cwd),
        "cdd_max_days": diff_stats(&mut cdd),
        "vpd_summer_kpa": diff_stats(&mut vpd),
        "sizes": sizes,
        "seconds": (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0,
    });
    layers::register(&cfg.out, layer_spec(tiles), report)?;
    eprintln!(
        "water_balance: {cells} cells ({filled} coastal-filled, {missing} missing) in {:.1} s",
        t0.elapsed().as_secs_f64(),
    );
    Ok(())
}
