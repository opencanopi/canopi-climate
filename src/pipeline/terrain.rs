//! The `terrain` stage: static 0.1-degree elevation, relief, slope, and HAND
//! from Earth Engine input `ee/terrain_glo30_merit.tif` (`3600 x 1800`,
//! exact canopi 0.1-degree grid).
//!
//! Preserves below-sea-level land (`elev_m <= 0`) and keeps `hand_m = NODATA`
//! outside MERIT-Hydro's 60°S–90°N coverage (at most a 1-cell land-only coastal
//! fallback for `hand_m`; never fills across sea or defaults to 0).
//! Method: `docs/decisions/0008-soil-plus-terrain-microclimate-packs.md`.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::time::Instant;

use rayon::prelude::{IntoParallelIterator, ParallelIterator};
use serde_json::json;
use tiff::decoder::{Decoder, DecodingResult, Limits};
use tiff::tags::Tag;

use super::build::{BuildConfig, diff_stats};
use super::{PipelineError, Result, cog, layers};
use crate::grid::{CELL_DEG, GRID_H, GRID_W, TILE_CELLS, TILE_LEN, TileId};
use crate::manifest::{Encoding, LayerSpec, TileSpec, VariableSpec};
use crate::model::NODATA;
use crate::terrain::{DIR, TERRAIN_BAND_LIST, TERRAIN_BANDS, TerrainBand, TerrainTile};
use crate::tile::Tile;

/// Earth Engine terrain input raster (`3600 x 1800`, 4 × `int16` bands:
/// `elev_m`, `relief_m`, `slope_deg * 10`, `hand_m * 10`).
pub const SOURCE_PATH: &str = "ee/terrain_glo30_merit.tif";

struct GlobalBand {
    data: Vec<f32>,
    scale: f32,
}

impl GlobalBand {
    fn get(&self, col: i64, row: i64) -> Option<f32> {
        if row < 0 || row >= GRID_H as i64 {
            return None;
        }
        let c = col.rem_euclid(GRID_W as i64) as usize;
        let v = self.data[row as usize * GRID_W + c];
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
}

fn read_inputs(path: &Path) -> Result<[GlobalBand; TERRAIN_BANDS]> {
    let f = File::open(path).map_err(|e| PipelineError::io(path, e))?;
    let mut dec = Decoder::new(BufReader::new(f))
        .map_err(|e| PipelineError::Data(format!("{}: {e}", path.display())))?
        .with_limits(Limits::unlimited());
    let (w, h) = dec
        .dimensions()
        .map_err(|e| PipelineError::Data(format!("{}: {e}", path.display())))?;
    if (w as usize, h as usize) != (GRID_W, GRID_H) {
        return Err(PipelineError::Data(format!(
            "{}: {w}x{h}, expected {GRID_W}x{GRID_H}",
            path.display()
        )));
    }
    let spp =
        dec.get_tag_u32(Tag::SamplesPerPixel)
            .map_err(|e| PipelineError::Data(format!("{}: {e}", path.display())))? as usize;
    if spp != TERRAIN_BANDS {
        return Err(PipelineError::Data(format!(
            "{}: {spp} bands, expected {TERRAIN_BANDS}",
            path.display()
        )));
    }
    let planar = dec.get_tag_u32(Tag::PlanarConfiguration).unwrap_or(1);
    let n_px = GRID_W * GRID_H;
    let raw: Vec<f32> = match dec
        .read_image()
        .map_err(|e| PipelineError::Data(format!("{}: {e}", path.display())))?
    {
        DecodingResult::I16(v) => v.into_iter().map(f32::from).collect(),
        DecodingResult::F32(v) => v,
        other => {
            return Err(PipelineError::Data(format!(
                "{}: unsupported sample type {:?}",
                path.display(),
                std::mem::discriminant(&other)
            )));
        }
    };
    if raw.len() != n_px * TERRAIN_BANDS {
        return Err(PipelineError::Data(format!(
            "{}: decoded {} samples, expected {}",
            path.display(),
            raw.len(),
            n_px * TERRAIN_BANDS
        )));
    }
    let mut bands = vec![vec![f32::from(NODATA); n_px]; TERRAIN_BANDS];
    if planar == 2 {
        for (b, dst) in bands.iter_mut().enumerate() {
            dst.copy_from_slice(&raw[b * n_px..(b + 1) * n_px]);
        }
    } else {
        for i in 0..n_px {
            let base = i * TERRAIN_BANDS;
            for (b, dst) in bands.iter_mut().enumerate() {
                dst[i] = raw[base + b];
            }
        }
    }
    let [elev, relief, slope, hand]: [Vec<f32>; TERRAIN_BANDS] =
        bands.try_into().map_err(|_| {
            PipelineError::Data(format!(
                "{}: expected {TERRAIN_BANDS} bands",
                path.display()
            ))
        })?;
    Ok([
        GlobalBand {
            data: elev,
            scale: 1.0,
        },
        GlobalBand {
            data: relief,
            scale: 1.0,
        },
        GlobalBand {
            data: slope,
            scale: 0.1,
        },
        GlobalBand {
            data: hand,
            scale: 0.1,
        },
    ])
}

#[derive(Default)]
struct TileReport {
    cells: u64,
    missing: u64,
    hand_missing: u64,
    below_sea_level: u64,
    elev: Vec<f32>,
    relief: Vec<f32>,
    hand: Vec<f32>,
}

fn build_tile(tile: &Tile, bands: &[GlobalBand; TERRAIN_BANDS]) -> (Vec<i16>, TileReport) {
    let id = tile.id();
    let mut data = vec![NODATA; TILE_LEN * TERRAIN_BANDS];
    let mut rep = TileReport::default();
    for j in 0..TILE_LEN {
        if !tile.is_complete(j) {
            continue;
        }
        let cell = id.cell(j);
        let (col, row) = (cell.col, cell.row);
        let Some(elev) = bands[0]
            .get(col as i64, row as i64)
            .or_else(|| bands[0].nearest(col, row, 1))
        else {
            rep.missing += 1;
            continue;
        };
        let relief = bands[1]
            .get(col as i64, row as i64)
            .or_else(|| bands[1].nearest(col, row, 1))
            .unwrap_or(0.0)
            .max(0.0);
        let slope = bands[2]
            .get(col as i64, row as i64)
            .or_else(|| bands[2].nearest(col, row, 1))
            .unwrap_or(0.0)
            .clamp(0.0, 90.0);
        // MERIT-Hydro HAND: 60S-90N only; keep NODATA outside coverage (at most 1-cell fallback).
        let hand = if cell.center_lat() >= -60.0 {
            bands[3]
                .get(col as i64, row as i64)
                .or_else(|| bands[3].nearest(col, row, 1))
                .map(|v| v.clamp(0.0, 3000.0))
        } else {
            None
        };

        let base = j * TERRAIN_BANDS;
        data[base + TerrainBand::ElevM.index()] = TerrainBand::ElevM.encode(elev);
        data[base + TerrainBand::ReliefM.index()] = TerrainBand::ReliefM.encode(relief);
        data[base + TerrainBand::SlopeDeg.index()] = TerrainBand::SlopeDeg.encode(slope);
        data[base + TerrainBand::HandM.index()] =
            hand.map_or(NODATA, |h| TerrainBand::HandM.encode(h));

        rep.cells += 1;
        rep.below_sea_level += u64::from(elev < 0.0);
        if hand.is_none() {
            rep.hand_missing += 1;
        }
        if j % 10 == 0 {
            rep.elev.push(elev);
            rep.relief.push(relief);
            if let Some(h) = hand {
                rep.hand.push(h);
            }
        }
    }
    (data, rep)
}

fn layer_spec(tiles: Vec<TileSpec>) -> LayerSpec {
    LayerSpec {
        id: DIR.into(),
        description: "Static 0.1-degree topography: Copernicus DEM GLO-30 mean elevation, p95-p5 relief, median slope, and MERIT-Hydro v1.0.1 median HAND (ODbL 1.0, 60S-90N).".into(),
        cell_deg: CELL_DEG,
        tile_cells: TILE_CELLS as u32,
        directory: DIR.into(),
        encoding: Encoding {
            container: "Cloud Optimized GeoTIFF (EPSG:4326, one 112 x 112 internal tile, no overviews)".into(),
            sample_format: "int16".into(),
            nodata: NODATA,
            compression: "deflate".into(),
            interleave: "pixel".into(),
            bands: TERRAIN_BANDS as u32,
            band_order: "band (1-based) = index in the bands list + 1".into(),
        },
        bands: TERRAIN_BAND_LIST
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
        sources: vec!["copernicus-dem-glo30".into(), "merit-hydro-v1.0.1".into()],
        tiles,
        notes: vec![
            "No spatial search: read at the climate lookup's cell_lat/cell_lon in the same tile.".into(),
            "Below-sea-level land (elev_m <= 0) is preserved. hand_m is null south of 60S or on unmapped islands (ODbL 1.0).".into(),
        ],
        slots: Vec::new(),
        version: Some(env!("CARGO_PKG_VERSION").into()),
    }
}

/// Build the `terrain` layer under `cfg.out/terrain` and register it in
/// `manifest.json`, `SHA256SUMS` and `build-report.json`.
///
/// # Errors
/// Missing climate tiles or `ee/terrain_glo30_merit.tif`, or I/O errors.
pub fn run(cfg: &BuildConfig) -> Result<serde_json::Value> {
    if !cfg.force && layers::has_layer(&cfg.out, DIR) {
        eprintln!("{DIR}: already built (pass --force to rebuild)");
        return Ok(json!({ "skipped": true }));
    }
    let t0 = Instant::now();
    let src_path = cfg.sources.join(SOURCE_PATH);
    if !src_path.exists() {
        return Err(PipelineError::Source(format!(
            "missing {} (run scripts/ee/v07_terrain.py download && combine)",
            src_path.display()
        )));
    }
    let bands = read_inputs(&src_path)?;

    let climate_manifest = layers::read_manifest(&cfg.out)?;
    let out_dir = cfg.out.join(DIR);
    if out_dir.exists() {
        std::fs::remove_dir_all(&out_dir).map_err(|e| PipelineError::io(&out_dir, e))?;
    }
    std::fs::create_dir_all(&out_dir).map_err(|e| PipelineError::io(&out_dir, e))?;

    let results: Result<Vec<(Option<TileSpec>, TileReport)>> = climate_manifest
        .tiles
        .into_par_iter()
        .map(|ct| {
            let id = TileId::parse(&ct.name)
                .ok_or_else(|| PipelineError::Data(format!("bad tile name {}", ct.name)))?;
            let bytes = std::fs::read(cfg.out.join(&ct.path))
                .map_err(|e| PipelineError::io(cfg.out.join(&ct.path), e))?;
            let tile = Tile::decode(&bytes)?;
            let (data, rep) = build_tile(&tile, &bands);
            if rep.cells == 0 {
                return Ok((None, rep));
            }
            let out_path = out_dir.join(id.file_name());
            let terrain_tile = TerrainTile::new(id, data)?;
            let written = cog::write_raster_checked(terrain_tile.as_raster(), &out_path)?;
            let _ = TerrainTile::decode(&written)?;
            let spec = TileSpec {
                name: id.name(),
                path: format!("{DIR}/{}", id.file_name()),
                bytes: written.len() as u64,
                sha256: super::sha256_bytes(&written),
                complete_cells: u32::try_from(rep.cells).unwrap_or(u32::MAX),
            };
            Ok((Some(spec), rep))
        })
        .collect();

    let mut tiles = Vec::new();
    let (mut cells, mut missing, mut hand_missing, mut below_sea) = (0u64, 0u64, 0u64, 0u64);
    let (mut elev, mut relief, mut hand) = (Vec::new(), Vec::new(), Vec::new());
    for (spec, rep) in results? {
        if let Some(s) = spec {
            tiles.push(s);
        }
        cells += rep.cells;
        missing += rep.missing;
        hand_missing += rep.hand_missing;
        below_sea += rep.below_sea_level;
        elev.extend(rep.elev);
        relief.extend(rep.relief);
        hand.extend(rep.hand);
    }
    tiles.sort_by(|a, b| a.name.cmp(&b.name));
    let total_bytes: u64 = tiles.iter().map(|t| t.bytes).sum();

    let report = json!({
        "tiles": tiles.len(),
        "bytes": total_bytes,
        "cells": cells,
        "missing_cells": missing,
        "hand_missing_cells": hand_missing,
        "below_sea_level_cells": below_sea,
        "elev_m_sample": diff_stats(&mut elev),
        "relief_m_sample": diff_stats(&mut relief),
        "hand_m_sample": diff_stats(&mut hand),
        "seconds": (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0,
    });
    layers::register(&cfg.out, layer_spec(tiles), report.clone())?;
    Ok(report)
}
