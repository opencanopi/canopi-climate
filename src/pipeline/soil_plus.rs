//! The `soil-plus` stage: SoilGrids 2.0 5-km aggregates (Homolosine) to the
//! 0.05-degree layer grid ([`crate::soil_plus`]).
//!
//! 1. Combines 0–5, 5–15, 15–30 cm (weights 5, 10, 15) for `soc`, `bdod`, `cfvo`
//!    and 30–60, 60–100 cm (weights 30, 40) for `cfvo`, `phh2o`, `clay`.
//! 2. Projects each 0.05-degree cell centre to Homolosine and samples nearest pixel.
//! 3. Reuses the `regions` land mask and 8-cell nearest-measured fill geometry
//!    (matching `soil/`, without duplicating `fill_distance`).

use std::path::Path;
use std::time::Instant;

use rayon::prelude::{IntoParallelIterator, IntoParallelRefIterator, ParallelIterator};
use serde_json::{Value, json};

use super::build::BuildConfig;
use super::soil::{Source, depth_weighted, pixel_map, read_source};
use super::{PipelineError, Result, layers, sources};
use crate::layer::{
    LAYER_CELL_DEG, LAYER_GRID_H, LAYER_GRID_W, LAYER_TILE_CELLS, LAYER_TILE_LEN, LayerCell,
};
use crate::manifest::{Encoding, LayerSpec, TileSpec, VariableSpec};
use crate::model::NODATA;
use crate::regions::REGION_BANDS;
use crate::soil::SOIL_FILL_RADIUS;
use crate::soil_plus::{DIR, SOIL_PLUS_BAND_LIST, SOIL_PLUS_BANDS, SoilPlusBand, has_soil_plus};

const SUB_DEPTHS: [(&str, f32); 2] = [("30-60cm", 30.0), ("60-100cm", 40.0)];

fn depth_weighted_two(layers: &[Source; 2]) -> Result<Source> {
    let [a, b] = layers;
    if (b.width, b.height, b.geo) != (a.width, a.height, a.geo) {
        return Err(PipelineError::Data(
            "SoilGrids subsoil depths differ in grid".into(),
        ));
    }
    let total: f32 = SUB_DEPTHS.iter().map(|d| d.1).sum();
    let values = (0..a.values.len())
        .into_par_iter()
        .map(|i| (a.values[i] * SUB_DEPTHS[0].1 + b.values[i] * SUB_DEPTHS[1].1) / total)
        .collect();
    Ok(Source {
        width: a.width,
        height: a.height,
        geo: a.geo,
        values,
    })
}

fn load_top(dir: &Path, var: &str) -> Result<Source> {
    let r0 = read_source(&sources::pinned(
        dir,
        &format!("soilgrids/{var}_0-5cm_mean_5000.tif"),
    )?)?;
    let r1 = read_source(&sources::pinned(
        dir,
        &format!("soilgrids/{var}_5-15cm_mean_5000.tif"),
    )?)?;
    let r2 = read_source(&sources::pinned(
        dir,
        &format!("soilgrids/{var}_15-30cm_mean_5000.tif"),
    )?)?;
    depth_weighted(&[r0, r1, r2])
}

fn load_sub(dir: &Path, var: &str) -> Result<Source> {
    let r0 = read_source(&sources::pinned(
        dir,
        &format!("soilgrids/{var}_30-60cm_mean_5000.tif"),
    )?)?;
    let r1 = read_source(&sources::pinned(
        dir,
        &format!("soilgrids/{var}_60-100cm_mean_5000.tif"),
    )?)?;
    depth_weighted_two(&[r0, r1])
}

fn sample_band(map: &[u32], src: &Source, band: SoilPlusBand, raw_to_unit: f32) -> Vec<i16> {
    map.par_iter()
        .map(|&p| {
            src.values
                .get(p as usize)
                .copied()
                .filter(|v| v.is_finite())
                .map_or(NODATA, |v| band.encode(v * raw_to_unit))
        })
        .collect()
}

fn fill_six(bands: &mut [Vec<i16>; SOIL_PLUS_BANDS], land: &[bool], radius: usize) -> u64 {
    let n = LAYER_GRID_W * LAYER_GRID_H;
    let measured = |i: usize| bands[0][i] != NODATA;
    let r = i64::try_from(radius).unwrap_or(0);
    let offsets: Vec<(i64, i64, i64)> = {
        let mut o: Vec<(i64, i64, i64)> = (-r..=r)
            .flat_map(|dy| (-r..=r).map(move |dx| (dx * dx + dy * dy, dy, dx)))
            .filter(|&(d2, _, _)| d2 > 0 && d2 <= r * r)
            .collect();
        o.sort_unstable();
        o
    };
    let todo: Vec<usize> = (0..n)
        .into_par_iter()
        .filter(|&i| land[i] && !measured(i))
        .collect();
    let found: Vec<(usize, usize)> = todo
        .into_par_iter()
        .filter_map(|i| {
            let c = LayerCell::from_global(i);
            offsets.iter().find_map(|&(_, dy, dx)| {
                let row = c.row as i64 + dy;
                if row < 0 || row >= LAYER_GRID_H as i64 {
                    return None;
                }
                let col = (c.col as i64 + dx).rem_euclid(LAYER_GRID_W as i64);
                let j = usize::try_from(row).ok()? * LAYER_GRID_W + usize::try_from(col).ok()?;
                measured(j).then_some((i, j))
            })
        })
        .collect();
    let filled_count = found.len() as u64;
    for (i, j) in found {
        for b in bands.iter_mut() {
            b[i] = b[j];
        }
    }
    filled_count
}

fn layer_spec(tiles: Vec<TileSpec>) -> LayerSpec {
    LayerSpec {
        id: DIR.into(),
        description: "SoilGrids 2.0 deep soil and physical properties: 0-30 cm SOC, bulk density, coarse fragments and 30-100 cm coarse fragments, pH, clay on the 0.05-degree grid.".into(),
        cell_deg: LAYER_CELL_DEG,
        tile_cells: LAYER_TILE_CELLS as u32,
        directory: DIR.into(),
        encoding: Encoding {
            container: "Cloud Optimized GeoTIFF (EPSG:4326, one 208 x 208 internal tile, no overviews)".into(),
            sample_format: "int16".into(),
            nodata: NODATA,
            compression: "deflate".into(),
            interleave: "pixel".into(),
            bands: SOIL_PLUS_BANDS as u32,
            band_order: "band (1-based) = index in the bands list + 1".into(),
        },
        bands: SOIL_PLUS_BAND_LIST
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
        sources: vec!["soilgrids-2.0-plus".into()],
        tiles,
        notes: vec![
            "Reuses soil/'s 8-cell land-only fill mask without duplicating fill_distance.".into(),
            "OM% = 0.1724 * soc_0_30 (g/kg).".into(),
        ],
        slots: Vec::new(),
        version: Some(env!("CARGO_PKG_VERSION").into()),
    }
}

fn land_mask(out: &Path) -> Result<Vec<bool>> {
    let mut land = vec![false; LAYER_GRID_W * LAYER_GRID_H];
    for t in layers::read_layer(out, super::regions::DIR, REGION_BANDS)? {
        for j in 0..LAYER_TILE_LEN {
            let g = LayerCell::in_tile(t.id(), j).global_index();
            land[g] = (0..REGION_BANDS).any(|b| t.get(j, b) != NODATA);
        }
    }
    Ok(land)
}

/// Build the `soil_plus` layer under `cfg.out/soil_plus` and register it in
/// `manifest.json`, `SHA256SUMS` and `build-report.json`.
///
/// # Errors
/// Missing `regions` layer, missing SoilGrids sources, or I/O errors.
pub fn run(cfg: &BuildConfig) -> Result<Value> {
    if !cfg.force && layers::has_layer(&cfg.out, DIR) {
        eprintln!("{DIR}: already built (pass --force to rebuild)");
        return Ok(json!({ "skipped": true }));
    }
    let t0 = Instant::now();
    let land = land_mask(&cfg.out)?;

    let soc0 = load_top(&cfg.sources, "soc")?;
    let bdod0 = load_top(&cfg.sources, "bdod")?;
    let cfvo0 = load_top(&cfg.sources, "cfvo")?;
    let cfvo1 = load_sub(&cfg.sources, "cfvo")?;
    let ph1 = load_sub(&cfg.sources, "phh2o")?;
    let clay1 = load_sub(&cfg.sources, "clay")?;

    let map = pixel_map(&soc0)?;
    // Native SoilGrids units -> decoded units:
    // soc: dg/kg -> g/kg (* 0.1); bdod: cg/cm3 -> g/cm3 (* 0.01);
    // cfvo: cm3/dm3 -> % (* 0.1); phh2o: pH*10 -> pH (* 0.1); clay: g/kg -> % (* 0.1).
    let mut bands = [
        sample_band(&map, &soc0, SoilPlusBand::Soc0To30, 0.1),
        sample_band(&map, &bdod0, SoilPlusBand::Bdod0To30, 0.01),
        sample_band(&map, &cfvo0, SoilPlusBand::Cfvo0To30, 0.1),
        sample_band(&map, &cfvo1, SoilPlusBand::Cfvo30To100, 0.1),
        sample_band(&map, &ph1, SoilPlusBand::Ph30To100, 0.1),
        sample_band(&map, &clay1, SoilPlusBand::Clay30To100, 0.1),
    ];

    // If a cell has 0-30 cm cfvo but lacks 30-100 cm cfvo, fall back to 0-30 cm.
    let mut measured = 0u64;
    for i in 0..map.len() {
        if bands[3][i] == NODATA && bands[2][i] != NODATA {
            bands[3][i] = bands[2][i];
        }
        if bands.iter().any(|b| b[i] == NODATA) {
            for b in &mut bands {
                b[i] = NODATA;
            }
        } else {
            measured += 1;
        }
    }

    let filled = fill_six(&mut bands, &land, SOIL_FILL_RADIUS);
    let slices: Vec<&[i16]> = bands.iter().map(Vec::as_slice).collect();
    let cut = layers::cut_tiles(&slices, has_soil_plus)?;
    let tiles = layers::write_tiles(&cfg.out, DIR, cut, has_soil_plus)?;
    let total_bytes: u64 = tiles.iter().map(|t| t.bytes).sum();
    let report = json!({
        "tiles": tiles.len(),
        "bytes": total_bytes,
        "measured_cells": measured,
        "filled_cells": filled,
        "seconds": (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0,
    });
    layers::register(&cfg.out, layer_spec(tiles), report.clone())?;
    Ok(report)
}
