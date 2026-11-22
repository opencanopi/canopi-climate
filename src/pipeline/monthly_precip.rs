//! The `monthly-precip` build stage: 12-month precipitation on a 0.25-degree
//! grid (40 x 40 cells per 10-degree tile, [`crate::monthly_precip`]),
//! aggregated from Beck et al. (2023) 0.1-degree monthly `precipitation` as
//! the mean of complete land sub-cells.

use std::path::Path;
use std::time::Instant;

use rayon::prelude::{IntoParallelIterator, ParallelIterator};
use serde_json::json;

use super::build::{BuildConfig, read_beck_monthly_var, slot_dir, slot_key};
use super::{PipelineError, Result, cog, layers, sources};
use crate::grid::{GRID_H, GRID_W, TILE_LEN, TileId};
use crate::manifest::{Encoding, LayerSpec, SlotSpec, TileSpec, VariableSpec};
use crate::model::NODATA;
use crate::monthly::MONTHLY_SLOTS;
use crate::monthly_precip::{
    DIR, MonthlyPrecipTile, PRECIP_BANDS, PRECIP_TILE_CELLS, PRECIP_TILE_LEN, encode_precip,
    precip_band,
};
use crate::raster::LayerGrid;
use crate::tile::Tile;

const Q_W: usize = LayerGrid::QUARTER_DEG.grid_w();
const Q_H: usize = LayerGrid::QUARTER_DEG.grid_h();
const Q_LEN: usize = Q_W * Q_H;

/// Load the 0.1-degree land mask (complete climate cells) from the release.
fn load_land_mask(out: &Path) -> Result<Vec<bool>> {
    let m = layers::read_manifest(out)?;
    let mut complete = vec![false; GRID_W * GRID_H];
    for spec in &m.tiles {
        let p = out.join(&spec.path);
        let bytes = std::fs::read(&p).map_err(|e| PipelineError::io(&p, e))?;
        let tile = Tile::decode(&bytes)?;
        let id = tile.id();
        for j in 0..TILE_LEN {
            if tile.is_complete(j) {
                complete[id.cell(j).global_index()] = true;
            }
        }
    }
    Ok(complete)
}

/// Map each 0.1-degree global index to its enclosing 0.25-degree global index.
fn build_cell_map() -> Vec<usize> {
    let grid = LayerGrid::QUARTER_DEG;
    let mut map = vec![0usize; GRID_W * GRID_H];
    for r in 0..GRID_H {
        let lat = 90.0 - (r as f64 + 0.5) * 0.1;
        for c in 0..GRID_W {
            let lon = -180.0 + (c as f64 + 0.5) * 0.1;
            if let Some(q) = grid.cell_of(lat, lon) {
                map[r * GRID_W + c] = grid.global_index(q);
            }
        }
    }
    map
}

/// Index of the maximum (wettest) and minimum (driest) month.
fn wettest_driest(p: &[f32; 12]) -> (usize, usize) {
    let (mut w_idx, mut d_idx) = (0usize, 0usize);
    for m in 1..12 {
        if p[m] > p[w_idx] {
            w_idx = m;
        }
        if p[m] < p[d_idx] {
            d_idx = m;
        }
    }
    (w_idx, d_idx)
}

/// Build the manifest `LayerSpec` for `monthly_precip`.
#[must_use]
pub fn layer_spec(tiles: Vec<TileSpec>) -> LayerSpec {
    LayerSpec {
        id: DIR.into(),
        description: "Monthly precipitation totals (Jan..Dec) on a 0.25-degree grid (mean of 0.1-degree land sub-cells) for 1991-2020, 2041-2070 SSP2-4.5 and 2071-2099 SSP2-4.5".into(),
        cell_deg: LayerGrid::QUARTER_DEG.cell_deg(),
        tile_cells: PRECIP_TILE_CELLS as u32,
        directory: DIR.into(),
        encoding: Encoding {
            container: "Cloud Optimized GeoTIFF (EPSG:4326, one 48 x 48 internal tile, no overviews)".into(),
            sample_format: "int16".into(),
            nodata: NODATA,
            compression: "deflate".into(),
            interleave: "pixel".into(),
            bands: PRECIP_BANDS as u32,
            band_order: "band (0-based) = slot_index * 12 + month_index".into(),
        },
        bands: vec![VariableSpec {
            id: "precip".into(),
            unit: "mm".into(),
            scale: 1.0,
            offset: 0.0,
            description: "Monthly precipitation total (Jan..Dec), mean of complete 0.1-degree land sub-cells from Beck et al. 2023".into(),
            months: Some(12),
        }],
        sources: vec!["beck2023".into()],
        tiles,
        notes: vec![
            "0.25-degree grid (40 x 40 cells per 10-degree tile); lookup falls back to the nearest valid cell within radius 1 so coastal climate cells never lose monthly rain.".into(),
        ],
        slots: MONTHLY_SLOTS
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

/// Run the `monthly-precip` stage (after `tiles`).
///
/// # Errors
/// Missing release or sources, writer or I/O errors.
pub fn stage_monthly_precip(cfg: &BuildConfig) -> Result<()> {
    if layers::has_layer(&cfg.out, DIR) && !cfg.force {
        eprintln!("monthly-precip: layer exists, skipping");
        return Ok(());
    }
    let t0 = Instant::now();
    let complete = load_land_mask(&cfg.out)?;
    let map = build_cell_map();
    let n = GRID_W * GRID_H;

    // Global 0.25-degree quantized bands: 36 bands x Q_LEN.
    let mut bands = vec![vec![NODATA; Q_LEN]; PRECIP_BANDS];
    let mut wet_agree = 0u64;
    let mut dry_agree = 0u64;
    let mut checked_cells = 0u64;

    for (s, &slot) in MONTHLY_SLOTS.iter().enumerate() {
        let key = slot_key(slot);
        let rel = format!("beck2023/c0p1/{}/ensemble_mean_0p1.nc", slot_dir(&key));
        let beck_p = read_beck_monthly_var(&sources::pinned(&cfg.sources, &rel)?, "precipitation")?;

        let mut sums = vec![[0.0f64; 12]; Q_LEN];
        let mut counts = vec![0u32; Q_LEN];
        for i in 0..n {
            if !complete[i] {
                continue;
            }
            let qi = map[i];
            let mut ok = true;
            let mut vals = [0.0f64; 12];
            for m in 0..12 {
                let v = beck_p[m * n + i];
                if !v.is_finite() {
                    ok = false;
                    break;
                }
                vals[m] = f64::from(v);
            }
            if !ok {
                continue;
            }
            counts[qi] += 1;
            for m in 0..12 {
                sums[qi][m] += vals[m];
            }
        }
        for qi in 0..Q_LEN {
            if counts[qi] == 0 {
                continue;
            }
            let denom = f64::from(counts[qi]);
            for m in 0..12 {
                bands[precip_band(s, m)][qi] = encode_precip((sums[qi][m] / denom) as f32);
            }
        }

        // Invariant check on slot 0: wettest/driest month agreement against 0.1-degree Beck.
        if s == 0 {
            for i in 0..n {
                if !complete[i] {
                    continue;
                }
                let qi = map[i];
                if counts[qi] == 0 {
                    continue;
                }
                let mut fine = [0.0f32; 12];
                let mut coarse = [0.0f32; 12];
                for m in 0..12 {
                    fine[m] = beck_p[m * n + i];
                    coarse[m] = f32::from(bands[precip_band(0, m)][qi]);
                }
                let (fw, fd) = wettest_driest(&fine);
                let (cw, cd) = wettest_driest(&coarse);
                checked_cells += 1;
                // Count as agreeing if same month or within 5 mm of the 0.1° extreme
                // (avoids penalizing near-ties in desert/equatorial cells).
                if fw == cw || (fine[fw] - fine[cw]).abs() <= 5.0 {
                    wet_agree += 1;
                }
                if fd == cd || (fine[fd] - fine[cd]).abs() <= 5.0 {
                    dry_agree += 1;
                }
            }
        }
        eprintln!("monthly-precip {key}: done");
    }

    let wet_pct = 100.0 * wet_agree as f64 / checked_cells.max(1) as f64;
    let dry_pct = 100.0 * dry_agree as f64 / checked_cells.max(1) as f64;
    let specs = write_precip_tiles(&cfg.out, &bands)?;

    let report = json!({
        "checked_climate_cells": checked_cells,
        "wettest_month_agreement_pct": (wet_pct * 100.0).round() / 100.0,
        "driest_month_agreement_pct": (dry_pct * 100.0).round() / 100.0,
        "sizes": layers::size_report(&specs),
        "seconds": t0.elapsed().as_secs_f64().round(),
    });
    eprintln!("monthly-precip: {}", report["sizes"]);
    layers::register(&cfg.out, layer_spec(specs), report)
}

fn write_precip_tiles(out: &Path, bands: &[Vec<i16>]) -> Result<Vec<TileSpec>> {
    let dir = out.join(DIR);
    std::fs::create_dir_all(&dir).map_err(|e| PipelineError::io(&dir, e))?;
    let mut specs: Vec<TileSpec> = TileId::all()
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|id| {
            let mut data = vec![NODATA; PRECIP_TILE_LEN * PRECIP_BANDS];
            let mut count = 0u32;
            for j in 0..PRECIP_TILE_LEN {
                let g = LayerGrid::QUARTER_DEG.global_index(LayerGrid::QUARTER_DEG.in_tile(id, j));
                if bands[0][g] == NODATA {
                    continue;
                }
                count += 1;
                let base = j * PRECIP_BANDS;
                for (b, band) in bands.iter().enumerate() {
                    data[base + b] = band[g];
                }
            }
            if count == 0 {
                return Ok(None);
            }
            let tile = MonthlyPrecipTile::new(id, data)?;
            let path = dir.join(id.file_name());
            let bytes = cog::write_monthly_precip_tile_checked(&tile, &path)?;
            Ok(Some(TileSpec {
                name: id.name(),
                path: format!("{DIR}/{}", id.file_name()),
                bytes: bytes.len() as u64,
                sha256: super::sha256_bytes(&bytes),
                complete_cells: count,
            }))
        })
        .collect::<Result<Vec<Option<TileSpec>>>>()?
        .into_iter()
        .flatten()
        .collect();
    specs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(specs)
}
