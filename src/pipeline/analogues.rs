//! The `analogues` stage: build the compact `analogues.bin` k-NN index for the
//! **2050 Climate Twin** search directly from the release's `tiles/`,
//! `monthly/`, `monthly_precip/`, and `regions/` tiles.
//!
//! Samples complete 1991–2020 land cells on a 1.0-degree stride (every 10th
//! 0.1° cell in row and column → ~21,000 land cells globally ≈ 550 KB),
//! computes global per-feature standardization statistics (`mean`, `inv_std`),
//! writes `analogues.bin`, and registers it in `manifest.json`, `build-report.json`,
//! and `SHA256SUMS`.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use rayon::prelude::{IntoParallelRefIterator, ParallelIterator};
use serde_json::json;

use super::build::BuildConfig;
use super::{PipelineError, Result, assemble, layers, sha256_bytes};
use crate::analogues::{
    ARIDITY_IDX, AnalogueIndex, AnalogueRecord, DEFAULT_CLOSE_THRESHOLD, DEFAULT_NOVEL_THRESHOLD,
    DEFAULT_WEIGHTS, FEATURES, FILE_NAME, FLAG_ARIDITY_VALID, NO_CODE, quantize_features,
};
use crate::grid::{TILE_CELLS, TileId};
use crate::layer::{LAYER_LOOKUP_RADIUS, LayerTile};
use crate::manifest::AnaloguesAssetSpec;
use crate::model::{NODATA, Variable, band};
use crate::monthly::{self, MonthlyTile};
use crate::monthly_precip::{self, MonthlyPrecipTile, PRECIP_LOOKUP_RADIUS};
use crate::regions::{REGION_BANDS, encode_code};
use crate::tile::Tile;

/// Subsampling stride in 0.1° cells (10 = 1.0° spacing, ~21k global land cells).
pub const SAMPLE_STRIDE: usize = 10;

/// Compute the fraction of annual precipitation falling in the 6 warmest months
/// centered on `m_warm` (0..12), returning `0.5` if precipitation is zero.
#[must_use]
pub fn summer_rain_fraction(tmean: &[f64; 12], precip: &[f64; 12]) -> f32 {
    let total: f64 = precip.iter().sum();
    if total <= 0.0 || !total.is_finite() {
        return 0.5;
    }
    let m_warm = tmean
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map_or(6, |(i, _)| i);
    // 6-month warm half centered on m_warm: [m_warm - 2 ..= m_warm + 3] mod 12.
    let mut warm_p = 0.0;
    for offset in 0..6 {
        let m = (m_warm + 10 + offset) % 12;
        warm_p += precip[m].max(0.0);
    }
    (warm_p / total).clamp(0.0, 1.0) as f32
}

struct SampledCell {
    lat_e2: i16,
    lon_e2: i16,
    iso_a2: u16,
    wgsrpd_l3: u16,
    koppen: u8,
    arid_valid: bool,
    raw: [f32; FEATURES],
}

fn read_optional_tile<T>(
    out: &Path,
    map: &HashMap<String, String>,
    name: &str,
    decode: impl Fn(&[u8]) -> std::result::Result<T, crate::tile::TileError>,
) -> Option<T> {
    let rel = map.get(name)?;
    let bytes = std::fs::read(out.join(rel)).ok()?;
    decode(&bytes).ok()
}

fn lookup_srf_and_regions(
    lat: f64,
    lon: f64,
    mon: Option<&MonthlyTile>,
    prec: Option<&MonthlyPrecipTile>,
    reg: Option<&LayerTile>,
) -> (f32, u16, u16) {
    let srf = match (mon, prec) {
        (Some(mt), Some(pt)) => {
            let m_ok = monthly::at_cell(mt, lat, lon).ok().flatten();
            let p_ok = monthly_precip::lookup_across(
                |tid: TileId| (tid == pt.id()).then_some(pt),
                lat,
                lon,
                PRECIP_LOOKUP_RADIUS,
            )
            .ok()
            .flatten();
            match (m_ok, p_ok) {
                (Some(ml), Some(pl)) => {
                    summer_rain_fraction(&ml.periods[0].tmean_c, &pl.periods[0].precip_mm)
                }
                _ => 0.5,
            }
        }
        _ => 0.5,
    };
    let (iso_a2, wgsrpd_l3) = match reg {
        Some(rt) => {
            let rl = crate::regions::lookup_across(
                |tid: TileId| (tid == rt.id()).then_some(rt),
                lat,
                lon,
                LAYER_LOOKUP_RADIUS,
            )
            .ok();
            let iso = rl
                .as_ref()
                .and_then(|x| x.iso_a2.as_deref())
                .and_then(encode_code)
                .and_then(|v| u16::try_from(v).ok())
                .unwrap_or(NO_CODE);
            let wgs = rl
                .as_ref()
                .and_then(|x| x.wgsrpd_l3.as_deref())
                .and_then(encode_code)
                .and_then(|v| u16::try_from(v).ok())
                .unwrap_or(NO_CODE);
            (iso, wgs)
        }
        None => (NO_CODE, NO_CODE),
    };
    (srf, iso_a2, wgsrpd_l3)
}

fn sample_tile(
    out: &Path,
    tile_path: &str,
    tile_name: &str,
    monthly_map: &HashMap<String, String>,
    precip_map: &HashMap<String, String>,
    regions_map: &HashMap<String, String>,
) -> Result<Vec<SampledCell>> {
    let p = out.join(tile_path);
    let raw_bytes = std::fs::read(&p).map_err(|e| PipelineError::io(&p, e))?;
    let climate = Tile::decode(&raw_bytes)?;
    let id = climate.id();
    let mon = read_optional_tile(out, monthly_map, tile_name, MonthlyTile::decode);
    let prec = read_optional_tile(out, precip_map, tile_name, MonthlyPrecipTile::decode);
    let reg = read_optional_tile(out, regions_map, tile_name, |b| {
        LayerTile::decode(b, REGION_BANDS)
    });

    let mut out_cells = Vec::new();
    for r in (SAMPLE_STRIDE / 2..TILE_CELLS).step_by(SAMPLE_STRIDE) {
        for c in (SAMPLE_STRIDE / 2..TILE_CELLS).step_by(SAMPLE_STRIDE) {
            let j = r * TILE_CELLS + c;
            if !climate.is_complete(j) {
                continue;
            }
            let cell = id.cell(j);
            let (lat, lon) = (cell.center_lat(), cell.center_lon());
            let get = |v: Variable| climate.get(j, band(v.index(), 0));
            let Ok(koppen) = u8::try_from(get(Variable::Koppen)) else {
                continue;
            };
            if !(1..=30).contains(&koppen) {
                continue;
            }
            let (Some(tc), Some(tw), Some(tn), Some(tx), Some(pr), Some(dm)) = (
                Variable::TmeanCold.decode(get(Variable::TmeanCold)),
                Variable::TmeanWarm.decode(get(Variable::TmeanWarm)),
                Variable::TminCold.decode(get(Variable::TminCold)),
                Variable::TmaxWarm.decode(get(Variable::TmaxWarm)),
                Variable::Precip.decode(get(Variable::Precip)),
                Variable::DryMonths.decode(get(Variable::DryMonths)),
            ) else {
                continue;
            };
            let arid_raw = get(Variable::Aridity);
            let arid = (arid_raw != NODATA)
                .then(|| Variable::Aridity.decode(arid_raw))
                .flatten();
            let (srf, iso_a2, wgsrpd_l3) =
                lookup_srf_and_regions(lat, lon, mon.as_ref(), prec.as_ref(), reg.as_ref());
            out_cells.push(SampledCell {
                lat_e2: (lat * 100.0).round() as i16,
                lon_e2: (lon * 100.0).round() as i16,
                iso_a2,
                wgsrpd_l3,
                koppen,
                arid_valid: arid.is_some(),
                raw: [
                    tc as f32,
                    tw as f32,
                    tn as f32,
                    tx as f32,
                    (1.0 + pr.max(0.0)).ln() as f32,
                    arid.unwrap_or(0.0) as f32,
                    dm as f32,
                    srf,
                ],
            });
        }
    }
    Ok(out_cells)
}

fn compute_stats(cells: &[SampledCell]) -> ([f32; FEATURES], [f32; FEATURES]) {
    let mut sum = [0f64; FEATURES];
    let mut cnt = [0u64; FEATURES];
    for c in cells {
        for i in 0..FEATURES {
            if i == ARIDITY_IDX && !c.arid_valid {
                continue;
            }
            sum[i] += f64::from(c.raw[i]);
            cnt[i] += 1;
        }
    }
    let mut mean = [0f32; FEATURES];
    for i in 0..FEATURES {
        mean[i] = if cnt[i] > 0 {
            (sum[i] / cnt[i] as f64) as f32
        } else {
            0.0
        };
    }
    let mut var = [0f64; FEATURES];
    for c in cells {
        for i in 0..FEATURES {
            if i == ARIDITY_IDX && !c.arid_valid {
                continue;
            }
            let d = f64::from(c.raw[i]) - f64::from(mean[i]);
            var[i] += d * d;
        }
    }
    let mut inv_std = [1f32; FEATURES];
    for i in 0..FEATURES {
        let sd = if cnt[i] > 1 {
            (var[i] / cnt[i] as f64).sqrt()
        } else {
            1.0
        };
        inv_std[i] = if sd > 1e-6 { (1.0 / sd) as f32 } else { 1.0 };
    }
    (mean, inv_std)
}

fn build_index_from_cells(cells: &[SampledCell]) -> AnalogueIndex {
    let (mean, inv_std) = compute_stats(cells);
    let records = cells
        .iter()
        .map(|c| AnalogueRecord {
            lat_e2: c.lat_e2,
            lon_e2: c.lon_e2,
            iso_a2: c.iso_a2,
            wgsrpd_l3: c.wgsrpd_l3,
            koppen: c.koppen,
            flags: if c.arid_valid { FLAG_ARIDITY_VALID } else { 0 },
            features: quantize_features(c.raw),
        })
        .collect();
    AnalogueIndex {
        mean,
        inv_std,
        weight: DEFAULT_WEIGHTS,
        close_threshold: DEFAULT_CLOSE_THRESHOLD,
        novel_threshold: DEFAULT_NOVEL_THRESHOLD,
        records,
    }
}

/// Run the `analogues` stage, writing `analogues.bin` and updating the
/// release manifest, `build-report.json`, and `SHA256SUMS`.
///
/// # Errors
/// Missing release files, unreadable tiles, or I/O errors.
pub fn stage_analogues(cfg: &BuildConfig) -> Result<()> {
    let out_path = cfg.out.join(FILE_NAME);
    let mut m = layers::read_manifest(&cfg.out)?;
    if !cfg.force && m.analogues.is_some() && out_path.exists() {
        eprintln!("analogues: {FILE_NAME} already in manifest, skipping");
        return Ok(());
    }
    let t0 = Instant::now();
    let layer_map = |id: &str| -> HashMap<String, String> {
        m.layers
            .iter()
            .find(|l| l.id == id)
            .map(|l| {
                l.tiles
                    .iter()
                    .map(|t| (t.name.clone(), t.path.clone()))
                    .collect()
            })
            .unwrap_or_default()
    };
    let (monthly_map, precip_map, regions_map) = (
        layer_map(monthly::DIR),
        layer_map(monthly_precip::DIR),
        layer_map("regions"),
    );

    let parts: Vec<Vec<SampledCell>> = m
        .tiles
        .par_iter()
        .map(|spec| {
            sample_tile(
                &cfg.out,
                &spec.path,
                &spec.name,
                &monthly_map,
                &precip_map,
                &regions_map,
            )
        })
        .collect::<Result<_>>()?;
    let mut cells: Vec<SampledCell> = parts.into_iter().flatten().collect();
    cells.sort_unstable_by(|a, b| {
        b.lat_e2
            .cmp(&a.lat_e2)
            .then_with(|| a.lon_e2.cmp(&b.lon_e2))
    });

    let index = build_index_from_cells(&cells);
    let bytes = index.to_bytes();
    let back = AnalogueIndex::from_bytes(&bytes)
        .map_err(|e| PipelineError::Data(format!("analogues round-trip: {e}")))?;
    if back.records.len() != index.records.len() {
        return Err(PipelineError::Data(
            "analogues record count mismatch".into(),
        ));
    }
    std::fs::write(&out_path, &bytes).map_err(|e| PipelineError::io(&out_path, e))?;

    let count = u32::try_from(index.records.len()).unwrap_or(u32::MAX);
    m.builder_version = env!("CARGO_PKG_VERSION").into();
    m.climate_tiles_sha256 = Some(assemble::climate_tiles_sha256(&m.tiles));
    m.analogues = Some(AnaloguesAssetSpec {
        path: FILE_NAME.into(),
        bytes: bytes.len() as u64,
        sha256: sha256_bytes(&bytes),
        version: env!("CARGO_PKG_VERSION").into(),
        records: count,
    });
    let rpath = cfg.out.join("build-report.json");
    let mut r = super::build::read_json(&rpath)?;
    r["analogues"] = json!({
        "records": count,
        "bytes": bytes.len(),
        "stride_cells": SAMPLE_STRIDE,
        "mean": index.mean,
        "inv_std": index.inv_std,
    });
    let rtext = serde_json::to_string_pretty(&r).map_err(|e| PipelineError::Data(e.to_string()))?;
    std::fs::write(&rpath, rtext + "\n").map_err(|e| PipelineError::io(&rpath, e))?;
    assemble::write_release_files(&cfg.out, &m)?;

    eprintln!(
        "analogues: {count} records, {} bytes ({:.1} KB) in {:.1}s",
        bytes.len(),
        bytes.len() as f64 / 1024.0,
        t0.elapsed().as_secs_f64()
    );
    Ok(())
}
