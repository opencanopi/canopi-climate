//! The `phenology` stage: static 1991–2020 Fishman (1987) Dynamic Model
//! Chill Portions across a 5-point warming curve (`ΔT ∈ {0, +1, +2, +3, +4 °C}`)
//! from the Earth Engine ERA5-Land raster `ee/era5land_phenology.tif`.
//!
//! Bilinearly interpolated from ERA5-Land's 0.1-degree grid to canopi's cell
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
use crate::phenology::{DIR, PHENOLOGY_BAND_LIST, PHENOLOGY_BANDS, PhenologyTile, SCALE};
use crate::tile::Tile;

/// ERA5-Land phenology input raster (5 × `int16` bands on ERA5-Land 0.1° grid).
pub const SOURCE_PATH: &str = "ee/era5land_phenology.tif";

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
        (v.is_finite() && (v - f32::from(NODATA)).abs() > 0.5).then_some(v)
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

fn read_input(path: &Path) -> Result<[E5lBand; PHENOLOGY_BANDS]> {
    let t = GeoTiff::open(path)?;
    if (t.width() as usize, t.height() as usize) != (E5L_W, E5L_H) {
        return Err(PipelineError::Data(format!(
            "{}: {}x{}, expected {E5L_W}x{E5L_H}",
            path.display(),
            t.width(),
            t.height()
        )));
    }
    if t.band_count() != PHENOLOGY_BANDS {
        return Err(PipelineError::Data(format!(
            "{}: {} bands, expected {PHENOLOGY_BANDS}",
            path.display(),
            t.band_count()
        )));
    }
    let mut bands = Vec::with_capacity(PHENOLOGY_BANDS);
    for b in 0..PHENOLOGY_BANDS {
        bands.push(E5lBand {
            data: t.read_band_f32(b)?,
        });
    }
    bands
        .try_into()
        .map_err(|_| PipelineError::Data("phenology band count".into()))
}

#[derive(Default)]
struct TileReport {
    cells: u64,
    filled: u64,
    missing: u64,
    cp0: Vec<f32>,
    cp2: Vec<f32>,
    cp4: Vec<f32>,
}

fn build_tile(tile: &Tile, bands: &[E5lBand; PHENOLOGY_BANDS]) -> (Vec<i16>, TileReport) {
    let id = tile.id();
    let mut data = vec![NODATA; TILE_LEN * PHENOLOGY_BANDS];
    let mut rep = TileReport::default();
    for j in 0..TILE_LEN {
        if !tile.is_complete(j) {
            continue;
        }
        let cell = id.cell(j);
        let (lat, lon) = (cell.center_lat(), cell.center_lon());
        let mut vals = [0f32; PHENOLOGY_BANDS];
        let mut any_filled = false;
        let mut ok = true;
        for (b, slot) in vals.iter_mut().enumerate() {
            if let Some((v, f)) = bands[b].bilinear(lat, lon, FILL_RADIUS) {
                *slot = v.clamp(0.0, 250.0);
                any_filled |= f;
            } else {
                ok = false;
                break;
            }
        }
        if !ok {
            rep.missing += 1;
            continue;
        }
        let base = j * PHENOLOGY_BANDS;
        for (b, &band_enum) in PHENOLOGY_BAND_LIST.iter().enumerate() {
            data[base + band_enum.index()] = band_enum.encode(vals[b]);
        }
        rep.cells += 1;
        rep.filled += u64::from(any_filled);
        if j % 10 == 0 {
            rep.cp0.push(vals[0]);
            rep.cp2.push(vals[2]);
            rep.cp4.push(vals[4]);
        }
    }
    (data, rep)
}

fn layer_spec(tiles: Vec<TileSpec>) -> LayerSpec {
    LayerSpec {
        id: DIR.into(),
        description: "Static 1991-2020 Fishman (1987) Dynamic Model Chill Portions across a 5-point warming sensitivity curve (dT = 0..+4 C) on the 0.1-degree climate grid.".into(),
        cell_deg: CELL_DEG,
        tile_cells: TILE_CELLS as u32,
        directory: DIR.into(),
        encoding: Encoding {
            container: "Cloud Optimized GeoTIFF (EPSG:4326, one 112 x 112 internal tile, no overviews)".into(),
            sample_format: "int16".into(),
            nodata: NODATA,
            compression: "deflate".into(),
            interleave: "pixel".into(),
            bands: PHENOLOGY_BANDS as u32,
            band_order: "band (1-based) = index in the bands list + 1".into(),
        },
        bands: PHENOLOGY_BAND_LIST
            .iter()
            .map(|b| VariableSpec {
                id: b.id().into(),
                unit: b.unit().into(),
                scale: SCALE,
                offset: 0.0,
                months: None,
                description: b.description().into(),
            })
            .collect(),
        sources: vec!["era5-land-phenology".into()],
        tiles,
        notes: vec![
            "No spatial search: read at the climate lookup's cell_lat/cell_lon in the same tile.".into(),
            "Piecewise-linearly interpolate chill_portions_{0..4}c using monthly winter warming deltas to project 2041-2070 and 2071-2099 SSP2-4.5 chill collapse.".into(),
        ],
        slots: Vec::new(),
        version: Some(env!("CARGO_PKG_VERSION").into()),
    }
}

/// Build the `phenology` layer under `cfg.out/phenology` and register it in
/// `manifest.json`, `SHA256SUMS` and `build-report.json`.
///
/// # Errors
/// Missing climate tiles or `ee/era5land_phenology.tif`, or I/O errors.
pub fn run(cfg: &BuildConfig) -> Result<()> {
    let t0 = Instant::now();
    if !cfg.force && layers::has_layer(&cfg.out, DIR) {
        eprintln!("phenology: cached (use --force to rebuild)");
        return Ok(());
    }
    let manifest = layers::read_manifest(&cfg.out)?;
    let src_path = cfg.sources.join(SOURCE_PATH);
    let bands = read_input(&src_path)?;
    eprintln!("phenology: loaded {}", src_path.display());

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
            let pt = PhenologyTile::new(id, data)?;
            let path = out_dir.join(id.file_name());
            let bytes = cog::write_raster_checked(pt.as_raster(), &path)?;
            let tspec = TileSpec {
                name: id.name(),
                path: format!("{DIR}/{}", id.file_name()),
                bytes: bytes.len() as u64,
                sha256: super::sha256_bytes(&bytes),
                complete_cells: u32::try_from(pt.cells()).unwrap_or(u32::MAX),
            };
            Ok((Some(tspec), rep))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut tiles = Vec::new();
    let (mut cells, mut filled, mut missing) = (0u64, 0u64, 0u64);
    let (mut cp0, mut cp2, mut cp4) = (Vec::new(), Vec::new(), Vec::new());
    for (spec, rep) in built {
        if let Some(s) = spec {
            tiles.push(s);
        }
        cells += rep.cells;
        filled += rep.filled;
        missing += rep.missing;
        cp0.extend(rep.cp0);
        cp2.extend(rep.cp2);
        cp4.extend(rep.cp4);
    }
    tiles.sort_by(|a, b| a.name.cmp(&b.name));
    let sizes = layers::size_report(&tiles);

    let report = json!({
        "cells": cells,
        "coastal_filled_cells": filled,
        "missing_cells": missing,
        "chill_portions_0c": diff_stats(&mut cp0),
        "chill_portions_2c": diff_stats(&mut cp2),
        "chill_portions_4c": diff_stats(&mut cp4),
        "sizes": sizes,
        "seconds": (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0,
    });
    layers::register(&cfg.out, layer_spec(tiles), report)?;
    eprintln!(
        "phenology: {cells} cells ({filled} coastal-filled, {missing} missing) in {:.1} s",
        t0.elapsed().as_secs_f64(),
    );
    Ok(())
}
