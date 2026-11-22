//! The `chelsa-monthly` and `monthly` build stages: 12-month `tmean`, `tmin`
//! and `dry_mask` on the 0.1-degree climate grid ([`crate::monthly`]).
//!
//! Method (`docs/decisions/0004-monthly-and-relief-layers.md`):
//! - `tmean`: Beck et al. (2023) monthly air temperature for `1991-2020`,
//!   `2041-2070 ssp245`, `2071-2099 ssp245`, quantized to 0.5 °C.
//! - `tmin`: CHELSA V2.1 monthly `tasmin` (1981-2010) aggregated to 0.1-degree
//!   cell means, shifted to 1991-2020 by 1/3 of the Beck 1961-1990 to
//!   1991-2020 monthly change; future slots add Beck monthly temperature
//!   deltas, clamped to `tmin <= tmean`, quantized to 0.5 °C.
//! - `dry_mask`: Gaussen bitmask (`P < 2T`) computed from unrounded Beck
//!   monthly `tmean` and `precip` (bits 0–11 = Jan..Dec, bits 12–14 = 0,
//!   bit 15 never set).

use std::path::{Path, PathBuf};
use std::time::Instant;

use rayon::prelude::{IntoParallelIterator, ParallelIterator};
use serde_json::json;

use super::build::{
    BASELINE_PAST, BuildConfig, chelsa_mean, diff_stats, read_beck_monthly_var, slot_dir, slot_key,
};
use super::{PipelineError, Result, cog, layers, raster, sources};
use crate::derived::dry_month_mask;
use crate::extremes::{Extreme, ExtremesTile};
use crate::grid::{CELL_DEG, GRID_H, GRID_W, TILE_CELLS, TILE_LEN, TileId};
use crate::manifest::{Encoding, LayerSpec, SlotSpec, TileSpec, VariableSpec};
use crate::model::{NODATA, Variable};
use crate::monthly::{
    DIR, MONTHLY_BANDS, MONTHLY_SLOT_INDICES, MONTHLY_SLOTS, MonthlyTile, TEMP_SCALE, decode_temp,
    dry_mask_band, encode_temp, tmean_band, tmin_band,
};
use crate::tile::Tile;

/// Work file for one aggregated CHELSA V2.1 monthly `tasmin` layer (month 1..=12).
#[must_use]
pub fn chelsa_tasmin_path(work: &Path, month: usize) -> PathBuf {
    work.join("chelsa").join(format!("tasmin_{month:02}.tif"))
}

/// Aggregate the 12 CHELSA V2.1 `tasmin` GeoTIFFs to 0.1-degree cell means,
/// strip-wise (4 threads max, <1 GB RAM).
///
/// # Errors
/// Missing pinned source files, raster or I/O errors.
pub fn stage_chelsa_monthly(cfg: &BuildConfig) -> Result<()> {
    let todo: Vec<usize> = (1..=12)
        .filter(|&m| cfg.force || !chelsa_tasmin_path(&cfg.work, m).exists())
        .collect();
    if todo.is_empty() {
        eprintln!("chelsa-monthly: all 12 months exist, skipping");
        return Ok(());
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .map_err(|e| PipelineError::Data(e.to_string()))?;
    pool.install(|| {
        todo.into_par_iter()
            .map(|m| {
                let rel = format!("chelsa/CHELSA_tasmin_{m:02}_1981-2010_V.2.1.tif");
                let src = sources::pinned(&cfg.sources, &rel)?;
                let layer = chelsa_mean(&src, 0.1, -273.15)?;
                raster::write_f32(&chelsa_tasmin_path(&cfg.work, m), &[&layer])?;
                eprintln!("chelsa-monthly {m:02}: done");
                Ok(())
            })
            .collect::<Result<Vec<()>>>()
    })?;
    Ok(())
}

/// Compact summary of the climate release (and optional `extremes` slot 0)
/// needed to build and verify the `monthly` layer.
struct ClimateMask {
    tile_ids: Vec<TileId>,
    complete: Vec<bool>,
    tmean_cold: [Vec<i16>; 3],
    tmean_warm: [Vec<i16>; 3],
    dry_months: [Vec<u8>; 3],
    extremes_tmin0: Option<Vec<i16>>,
}

fn load_climate_mask(out: &Path) -> Result<ClimateMask> {
    let m = layers::read_manifest(out)?;
    let n = GRID_W * GRID_H;
    let mut complete = vec![false; n];
    let mut tmean_cold = [vec![NODATA; n], vec![NODATA; n], vec![NODATA; n]];
    let mut tmean_warm = [vec![NODATA; n], vec![NODATA; n], vec![NODATA; n]];
    let mut dry_months = [vec![0u8; n], vec![0u8; n], vec![0u8; n]];
    let mut tile_ids = Vec::with_capacity(m.tiles.len());

    for spec in &m.tiles {
        let p = out.join(&spec.path);
        let bytes = std::fs::read(&p).map_err(|e| PipelineError::io(&p, e))?;
        let tile = Tile::decode(&bytes)?;
        let id = tile.id();
        tile_ids.push(id);
        for j in 0..TILE_LEN {
            if !tile.is_complete(j) {
                continue;
            }
            let g = id.cell(j).global_index();
            complete[g] = true;
            for (s, &cs) in MONTHLY_SLOT_INDICES.iter().enumerate() {
                tmean_cold[s][g] = tile.get(j, crate::model::band(Variable::TmeanCold.index(), cs));
                tmean_warm[s][g] = tile.get(j, crate::model::band(Variable::TmeanWarm.index(), cs));
                let dry = tile.get(j, crate::model::band(Variable::DryMonths.index(), cs));
                dry_months[s][g] = u8::try_from(dry.clamp(0, 12)).unwrap_or(0);
            }
        }
    }

    let extremes_tmin0 = if let Some(ext) = m.layers.iter().find(|l| l.id == crate::extremes::DIR) {
        let mut arr = vec![NODATA; n];
        let b0 = crate::extremes::band(Extreme::TminExtreme, 0);
        for spec in &ext.tiles {
            let p = out.join(&spec.path);
            let bytes = std::fs::read(&p).map_err(|e| PipelineError::io(&p, e))?;
            let tile = ExtremesTile::decode(&bytes)?;
            let id = tile.id();
            for j in 0..TILE_LEN {
                let v = tile.get(j, b0);
                if v != NODATA {
                    arr[id.cell(j).global_index()] = v;
                }
            }
        }
        Some(arr)
    } else {
        None
    };

    Ok(ClimateMask {
        tile_ids,
        complete,
        tmean_cold,
        tmean_warm,
        dry_months,
        extremes_tmin0,
    })
}

/// Nearest finite value within `radius` 0.1-degree cells (wraps longitude).
fn nearest_finite(grid: &[f32], index: usize, radius: usize) -> Option<f32> {
    let (r0, c0) = ((index / GRID_W) as i64, (index % GRID_W) as i64);
    let r = radius as i64;
    let mut best: Option<(i64, f32)> = None;
    for dy in -r..=r {
        let ny = r0 + dy;
        if ny < 0 || ny >= GRID_H as i64 {
            continue;
        }
        for dx in -r..=r {
            let d2 = dx * dx + dy * dy;
            if d2 == 0 || d2 > r * r || best.is_some_and(|(b, _)| b <= d2) {
                continue;
            }
            let nx = (c0 + dx).rem_euclid(GRID_W as i64) as usize;
            let v = grid[ny as usize * GRID_W + nx];
            if v.is_finite() {
                best = Some((d2, v));
            }
        }
    }
    best.map(|(_, v)| v)
}

/// Compute `diurnal_offset[m][i] = tasmin_1991_2020[m][i] - beck_t_1991_2020[m][i]`
/// for all 12 months, so every slot's `tmin[s] = beck_t[s] + diurnal_offset`.
fn compute_diurnal_offset(cfg: &BuildConfig, complete: &[bool]) -> Result<(Vec<f32>, u64, u64)> {
    let n = GRID_W * GRID_H;
    let past_rel = format!(
        "beck2023/c0p1/{}/ensemble_mean_0p1.nc",
        slot_dir(BASELINE_PAST)
    );
    let base_rel = "beck2023/c0p1/1991_2020/ensemble_mean_0p1.nc";
    let beck_past = read_beck_monthly_var(
        &sources::pinned(&cfg.sources, &past_rel)?,
        "air_temperature",
    )?;
    let beck_base =
        read_beck_monthly_var(&sources::pinned(&cfg.sources, base_rel)?, "air_temperature")?;

    let mut offset = vec![f32::NAN; 12 * n];
    let mut filled = 0u64;
    let mut missing = 0u64;

    for m in 0..12 {
        let chelsa = raster::read_f32(&chelsa_tasmin_path(&cfg.work, m + 1))?;
        let band = chelsa
            .first()
            .ok_or_else(|| PipelineError::Data(format!("empty chelsa tasmin month {}", m + 1)))?;
        if band.len() != n {
            return Err(PipelineError::Data(
                "chelsa tasmin grid size mismatch".into(),
            ));
        }
        let base_off = m * n;
        for i in 0..n {
            if !complete[i] {
                continue;
            }
            let tb = beck_base[base_off + i];
            let tp = beck_past[base_off + i];
            let tasmin_raw = if band[i].is_finite() {
                band[i]
            } else if let Some(v) = nearest_finite(band, i, 2) {
                filled += 1;
                v
            } else {
                missing += 1;
                tb
            };
            let shift = if tb.is_finite() && tp.is_finite() {
                (tb - tp) / 3.0
            } else {
                0.0
            };
            let tasmin_1991_2020 = tasmin_raw + shift;
            offset[base_off + i] = tasmin_1991_2020 - tb;
        }
    }
    Ok((offset, filled, missing))
}

struct SlotQuantized {
    tmean: Vec<i16>,
    tmin: Vec<i16>,
    dry_mask: Vec<i16>,
}

#[derive(Default)]
struct MonthlyStats {
    cells: u64,
    clamped_tmin: u64,
    max_tmean_cold_diff: f32,
    max_tmean_warm_diff: f32,
    tmean_cold_diffs: Vec<f32>,
    tmean_warm_diffs: Vec<f32>,
    extremes_above_tmin: u64,
    extremes_diffs: Vec<f32>,
}

fn process_slot(
    cfg: &BuildConfig,
    mask: &ClimateMask,
    diurnal_offset: &[f32],
    slot_idx: usize,
    stats: &mut MonthlyStats,
) -> Result<SlotQuantized> {
    let n = GRID_W * GRID_H;
    let key = slot_key(MONTHLY_SLOTS[slot_idx]);
    let rel = format!("beck2023/c0p1/{}/ensemble_mean_0p1.nc", slot_dir(&key));
    let nc = sources::pinned(&cfg.sources, &rel)?;
    let beck_t = read_beck_monthly_var(&nc, "air_temperature")?;
    let beck_p = read_beck_monthly_var(&nc, "precipitation")?;

    let mut tmean = vec![NODATA; 12 * n];
    let mut tmin = vec![NODATA; 12 * n];
    let mut dry_mask = vec![NODATA; n];

    for i in 0..n {
        if !mask.complete[i] {
            continue;
        }
        let mut tm = [0.0f32; 12];
        let mut pm = [0.0f32; 12];
        for m in 0..12 {
            tm[m] = beck_t[m * n + i];
            pm[m] = beck_p[m * n + i];
        }
        let dmask = dry_month_mask(&tm, &pm).ok_or_else(|| {
            PipelineError::Data(format!(
                "non-finite Beck monthly at complete cell {i} ({key})"
            ))
        })?;
        if (dmask & !0x0FFF) != 0 {
            return Err(PipelineError::Data(format!(
                "dry_mask reserved/sign bit set at cell {i} ({key}): {dmask}"
            )));
        }
        let expected_dry = mask.dry_months[slot_idx][i];
        let got_dry = u8::try_from(dmask.count_ones()).unwrap_or(12);
        if got_dry != expected_dry {
            return Err(PipelineError::Data(format!(
                "dry_mask popcount {got_dry} != climate dry_months {expected_dry} at cell {i} ({key})"
            )));
        }
        dry_mask[i] = dmask;

        let mut min_q_tm = f32::INFINITY;
        let mut max_q_tm = f32::NEG_INFINITY;
        let mut min_q_tn = f32::INFINITY;
        for m in 0..12 {
            let mut tn = tm[m] + diurnal_offset[m * n + i];
            if tn > tm[m] {
                tn = tm[m];
                stats.clamped_tmin += 1;
            }
            let q_tm = encode_temp(tm[m]);
            let mut q_tn = encode_temp(tn);
            if q_tn > q_tm {
                q_tn = q_tm;
                stats.clamped_tmin += 1;
            }
            tmean[m * n + i] = q_tm;
            tmin[m * n + i] = q_tn;
            let d_tm = decode_temp(q_tm).unwrap_or(0.0) as f32;
            let d_tn = decode_temp(q_tn).unwrap_or(0.0) as f32;
            min_q_tm = min_q_tm.min(d_tm);
            max_q_tm = max_q_tm.max(d_tm);
            min_q_tn = min_q_tn.min(d_tn);
        }

        let clim_cold = Variable::TmeanCold
            .decode(mask.tmean_cold[slot_idx][i])
            .unwrap_or(0.0) as f32;
        let clim_warm = Variable::TmeanWarm
            .decode(mask.tmean_warm[slot_idx][i])
            .unwrap_or(0.0) as f32;
        let dc = min_q_tm - clim_cold;
        let dw = max_q_tm - clim_warm;
        stats.max_tmean_cold_diff = stats.max_tmean_cold_diff.max(dc.abs());
        stats.max_tmean_warm_diff = stats.max_tmean_warm_diff.max(dw.abs());
        if slot_idx == 0 {
            stats.cells += 1;
            stats.tmean_cold_diffs.push(dc);
            stats.tmean_warm_diffs.push(dw);
            if let Some(ext_arr) = &mask.extremes_tmin0
                && let Some(ext_v) = Extreme::TminExtreme.decode(ext_arr[i])
            {
                let diff = ext_v as f32 - min_q_tn;
                if diff > 0.0 {
                    stats.extremes_above_tmin += 1;
                }
                stats.extremes_diffs.push(diff);
            }
        }
    }

    if stats.max_tmean_cold_diff > 0.35 || stats.max_tmean_warm_diff > 0.35 {
        return Err(PipelineError::Data(format!(
            "monthly tmean consistency exceeded 0.35 °C in {key}: cold={:.3}, warm={:.3}",
            stats.max_tmean_cold_diff, stats.max_tmean_warm_diff
        )));
    }
    eprintln!("monthly {key}: done");
    Ok(SlotQuantized {
        tmean,
        tmin,
        dry_mask,
    })
}

/// Build the manifest `LayerSpec` for `monthly`.
#[must_use]
pub fn layer_spec(tiles: Vec<TileSpec>) -> LayerSpec {
    LayerSpec {
        id: DIR.into(),
        description: "Monthly mean temperature, mean daily minimum temperature, and Gaussen dry-month bitmask on the climate grid for 1991-2020, 2041-2070 SSP2-4.5 and 2071-2099 SSP2-4.5".into(),
        cell_deg: CELL_DEG,
        tile_cells: TILE_CELLS as u32,
        directory: DIR.into(),
        encoding: Encoding {
            container: "Cloud Optimized GeoTIFF (EPSG:4326, one 112 x 112 internal tile, no overviews)".into(),
            sample_format: "int16".into(),
            nodata: NODATA,
            compression: "deflate".into(),
            interleave: "pixel".into(),
            bands: MONTHLY_BANDS as u32,
            band_order: "band, slot, month".into(),
        },
        bands: vec![
            VariableSpec {
                id: "tmean".into(),
                unit: "°C".into(),
                scale: TEMP_SCALE,
                offset: 0.0,
                description: "Monthly mean air temperature (Jan..Dec) from Beck et al. 2023".into(),
                months: Some(12),
            },
            VariableSpec {
                id: "tmin".into(),
                unit: "°C".into(),
                scale: TEMP_SCALE,
                offset: 0.0,
                description: "Monthly mean daily minimum air temperature (Jan..Dec): CHELSA V2.1 tasmin (1981-2010) shifted to 1991-2020 by 1/3 of Beck 1961-1990->1991-2020 change, plus Beck monthly deltas for future slots, clamped to tmin <= tmean".into(),
                months: Some(12),
            },
            VariableSpec {
                id: "dry_mask".into(),
                unit: "bits".into(),
                scale: 1.0,
                offset: 0.0,
                description: "Gaussen dry-month bitmask (gaussen_p_lt_2t): bit m (0..11 = Jan..Dec) is set where unrounded Beck P < 2*T; bits 12..14 = 0, bit 15 never set".into(),
                months: None,
            },
        ],
        sources: vec!["beck2023".into(), "chelsa-v2.1-tasmin".into()],
        tiles,
        notes: vec![
            "On the climate grid: values are present exactly where the climate cell is complete; read at the climate lookup's cell.".into(),
            "GDD (base 10 °C) and chill-month count (0 < tmean <= 7.2 °C) are derived at lookup from tmean.".into(),
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

/// Run the `monthly` stage (after `tiles` and `chelsa-monthly`).
///
/// # Errors
/// Missing release or work files, invariant violations, writer or I/O errors.
pub fn stage_monthly(cfg: &BuildConfig) -> Result<()> {
    if layers::has_layer(&cfg.out, DIR) && !cfg.force {
        eprintln!("monthly: layer exists, skipping");
        return Ok(());
    }
    let t0 = Instant::now();
    let mask = load_climate_mask(&cfg.out)?;
    let (diurnal_offset, tasmin_filled, tasmin_missing) =
        compute_diurnal_offset(cfg, &mask.complete)?;
    let mut stats = MonthlyStats::default();
    let mut slots = Vec::with_capacity(3);
    for s in 0..3 {
        slots.push(process_slot(cfg, &mask, &diurnal_offset, s, &mut stats)?);
    }
    drop(diurnal_offset);

    let dir = cfg.out.join(DIR);
    std::fs::create_dir_all(&dir).map_err(|e| PipelineError::io(&dir, e))?;
    let n = GRID_W * GRID_H;
    let mut specs: Vec<TileSpec> = mask
        .tile_ids
        .into_par_iter()
        .map(|id| {
            let mut data = vec![NODATA; TILE_LEN * MONTHLY_BANDS];
            let mut count = 0u32;
            for j in 0..TILE_LEN {
                let g = id.cell(j).global_index();
                if !mask.complete[g] {
                    continue;
                }
                count += 1;
                let base = j * MONTHLY_BANDS;
                for (s, sq) in slots.iter().enumerate() {
                    for m in 0..12 {
                        data[base + tmean_band(s, m)] = sq.tmean[m * n + g];
                        data[base + tmin_band(s, m)] = sq.tmin[m * n + g];
                    }
                    data[base + dry_mask_band(s)] = sq.dry_mask[g];
                }
            }
            if count == 0 {
                return Ok(None);
            }
            let tile = MonthlyTile::new(id, data)?;
            let path = dir.join(id.file_name());
            let bytes = cog::write_monthly_tile_checked(&tile, &path)?;
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

    let report = json!({
        "cells": stats.cells,
        "tasmin_filled_cells": tasmin_filled,
        "tasmin_missing_cells": tasmin_missing,
        "clamped_tmin_values": stats.clamped_tmin,
        "max_abs_tmean_cold_diff_c": (f64::from(stats.max_tmean_cold_diff) * 1000.0).round() / 1000.0,
        "max_abs_tmean_warm_diff_c": (f64::from(stats.max_tmean_warm_diff) * 1000.0).round() / 1000.0,
        "tmean_cold_diff_c": diff_stats(&mut stats.tmean_cold_diffs),
        "tmean_warm_diff_c": diff_stats(&mut stats.tmean_warm_diffs),
        "tmin_extreme_minus_monthly_tmin_c": diff_stats(&mut stats.extremes_diffs),
        "tmin_extreme_above_monthly_tmin_cells": stats.extremes_above_tmin,
        "sizes": layers::size_report(&specs),
        "seconds": t0.elapsed().as_secs_f64().round(),
    });
    eprintln!("monthly: {}", report["sizes"]);
    layers::register(&cfg.out, layer_spec(specs), report)
}
