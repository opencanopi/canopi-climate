//! `build-grid`: staged build of the grid. Each stage writes its outputs to
//! the work directory and is skipped when they exist (unless forced), so a
//! long build can run stage by stage:
//!
//! | stage | reads | writes (work dir) |
//! |---|---|---|
//! | `verify` | every pinned source | `verified.json` |
//! | `koppen` | Beck 1-km Köppen maps | `koppen/<slot>.tif`, `koppen.json` |
//! | `chelsa` | CHELSA bio5, bio6, bio12 | `chelsa/bio{5,6,12}.tif` |
//! | `beck` | Beck 0.1-degree monthly NetCDF | `beck/<period>.tif` (summaries) |
//! | `tiles` | the work files | out: `tiles/*.tif`, `manifest.json`, `SHA256SUMS`, `build-report.json`, `ATTRIBUTION.md` |
//! | `check-future` | CHELSA 2041-2070 SSP3-7.0 bio6 (5 GCMs) | `check-future.json` |

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rayon::prelude::{IntoParallelIterator, IntoParallelRefIterator, ParallelIterator};
use serde_json::{Value, json};

use super::aggregate::{block_mean, block_mode};
use super::netcdf::{list_vars, read_var_f32};
use super::{PipelineError, Result, raster, sources};
use crate::derived::MonthlySummary;
use crate::grid::{GRID_H, GRID_W};
use crate::model::{SLOTS, Scenario, Slot};

/// A build stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "pipeline", derive(clap::ValueEnum))]
pub enum Stage {
    /// Check the source files against their pinned SHA-256.
    Verify,
    /// Köppen-Geiger majority class per cell.
    Koppen,
    /// CHELSA means per cell.
    Chelsa,
    /// Beck monthly climatologies to per-cell summaries.
    Beck,
    /// Assemble, write tiles, manifest and report.
    Tiles,
    /// Compare the delta method with CHELSA's own projections.
    CheckFuture,
}

/// Stages run by default, in order.
pub const DEFAULT_STAGES: [Stage; 5] = [
    Stage::Verify,
    Stage::Koppen,
    Stage::Chelsa,
    Stage::Beck,
    Stage::Tiles,
];

/// Build settings.
#[derive(Debug, Clone)]
pub struct BuildConfig {
    /// Source directory (layout of `sources::SOURCES` paths).
    pub sources: PathBuf,
    /// Release directory.
    pub out: PathBuf,
    /// Work directory for intermediates.
    pub work: PathBuf,
    /// Redo stages whose outputs exist.
    pub force: bool,
    /// Build without `verified.json` (development only).
    pub skip_verify: bool,
    /// Restrict the `beck` stage to these period keys (e.g. `1991_2020`,
    /// `2041_2070_ssp245`); empty = all.
    pub beck_only: Vec<String>,
}

/// The Beck period used to move CHELSA's 1981-2010 baseline to 1991-2020.
pub const BASELINE_PAST: &str = "1961_1990";

/// Key of a slot, e.g. `1991_2020`, `2041_2070_ssp245`.
#[must_use]
pub fn slot_key(slot: Slot) -> String {
    let p = slot.period.label().replace('-', "_");
    match slot.scenario {
        Scenario::Historical => p,
        s => format!("{p}_{}", s.id()),
    }
}

/// Source sub-path of a slot, e.g. `1991_2020`, `2041_2070/ssp245`.
fn slot_dir(key: &str) -> String {
    match key.split_once("_ssp") {
        Some((p, s)) => format!("{p}/ssp{s}"),
        None => key.to_string(),
    }
}

/// Every Beck period key the build reads: 1961-1990 then the slots.
#[must_use]
pub fn beck_keys() -> Vec<String> {
    std::iter::once(BASELINE_PAST.to_string())
        .chain(SLOTS.iter().map(|s| slot_key(*s)))
        .collect()
}

pub(super) fn koppen_path(work: &Path, key: &str) -> PathBuf {
    work.join("koppen").join(format!("{key}.tif"))
}

pub(super) fn beck_path(work: &Path, key: &str) -> PathBuf {
    work.join("beck").join(format!("{key}.tif"))
}

pub(super) fn chelsa_path(work: &Path, var: &str) -> PathBuf {
    work.join("chelsa").join(format!("{var}.tif"))
}

fn write_json(path: &Path, v: &Value) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| PipelineError::io(d, e))?;
    }
    let s = serde_json::to_string_pretty(v).map_err(|e| PipelineError::Data(e.to_string()))?;
    std::fs::write(path, s + "\n").map_err(|e| PipelineError::io(path, e))
}

pub(super) fn read_json(path: &Path) -> Result<Value> {
    let s = std::fs::read_to_string(path).map_err(|e| PipelineError::io(path, e))?;
    serde_json::from_str(&s).map_err(|e| PipelineError::Data(format!("{}: {e}", path.display())))
}

/// Run stages in order.
///
/// # Errors
/// The first failing stage's error.
pub fn run(cfg: &BuildConfig, stages: &[Stage]) -> Result<()> {
    let mut stages = stages.to_vec();
    stages.sort();
    stages.dedup();
    for stage in stages {
        let t0 = Instant::now();
        if stage != Stage::Verify && !cfg.skip_verify && !cfg.work.join("verified.json").exists() {
            return Err(PipelineError::Source(
                "sources not verified: run `--stage verify` first (or pass --skip-verify)".into(),
            ));
        }
        match stage {
            Stage::Verify => stage_verify(cfg)?,
            Stage::Koppen => stage_koppen(cfg)?,
            Stage::Chelsa => stage_chelsa(cfg)?,
            Stage::Beck => stage_beck(cfg)?,
            Stage::Tiles => super::assemble::stage_tiles(cfg)?,
            Stage::CheckFuture => stage_check_future(cfg)?,
        }
        eprintln!(
            "stage {stage:?} done in {:.1} s",
            t0.elapsed().as_secs_f64()
        );
    }
    Ok(())
}

fn stage_verify(cfg: &BuildConfig) -> Result<()> {
    let out = cfg.work.join("verified.json");
    if out.exists() && !cfg.force {
        eprintln!("verify: {} exists, skipping", out.display());
        return Ok(());
    }
    let problems = sources::verify(&cfg.sources, true)?;
    if !problems.is_empty() {
        return Err(PipelineError::Source(problems.join("\n")));
    }
    let files: Vec<&str> = sources::SOURCES
        .iter()
        .filter(|s| s.required)
        .flat_map(|s| s.files.iter().map(|f| f.path))
        .collect();
    write_json(&out, &json!({ "verified": files, "sources": cfg.sources }))
}

fn stage_koppen(cfg: &BuildConfig) -> Result<()> {
    let keys: Vec<String> = SLOTS.iter().map(|s| slot_key(*s)).collect();
    let todo: Vec<&String> = keys
        .iter()
        .filter(|k| cfg.force || !koppen_path(&cfg.work, k).exists())
        .collect();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .map_err(|e| PipelineError::Data(e.to_string()))?;
    pool.install(|| {
        todo.par_iter()
            .map(|k| {
                let rel = format!("beck2023/kg/{}/koppen_geiger_0p00833333.tif", slot_dir(k));
                let src = sources::pinned(&cfg.sources, &rel)?;
                let classes = block_mode(&src, 30)?;
                raster::write_u8(&koppen_path(&cfg.work, k), &classes)?;
                eprintln!("koppen {k}: done");
                Ok(())
            })
            .collect::<Result<Vec<()>>>()
    })?;
    koppen_agreement(cfg)
}

/// Agreement of the 1-km majority with Beck's own 0.1-degree map (1991-2020).
fn koppen_agreement(cfg: &BuildConfig) -> Result<()> {
    let ours = raster::read_u8(&koppen_path(&cfg.work, "1991_2020"))?;
    let beck_path = sources::pinned(&cfg.sources, "beck2023/kg/1991_2020/koppen_geiger_0p1.tif")?;
    let g = wbgeotiff::GeoTiff::open(&beck_path)?;
    if (g.width() as usize, g.height() as usize) != (GRID_W, GRID_H) {
        return Err(PipelineError::Data(
            "Beck 0.1-degree map is not 3600 x 1800".into(),
        ));
    }
    let theirs = g.read_band_u8(0)?;
    let (mut both, mut same, mut only_ours, mut only_theirs) = (0u64, 0u64, 0u64, 0u64);
    for (a, b) in ours.iter().zip(&theirs) {
        let (a, b) = (*a, if (1..=30).contains(b) { *b } else { 0 });
        match (a > 0, b > 0) {
            (true, true) => {
                both += 1;
                same += u64::from(a == b);
            }
            (true, false) => only_ours += 1,
            (false, true) => only_theirs += 1,
            (false, false) => {}
        }
    }
    let land = ours.iter().filter(|&&k| k > 0).count();
    let v = json!({
        "land_cells_1991_2020": land,
        "agreement_with_beck_0p1_map": {
            "cells_both": both,
            "same_class": same,
            "percent": 100.0 * same as f64 / both.max(1) as f64,
            "cells_only_majority": only_ours,
            "cells_only_beck_0p1": only_theirs,
        }
    });
    eprintln!("koppen agreement: {v}");
    write_json(&cfg.work.join("koppen.json"), &v)
}

fn stage_chelsa(cfg: &BuildConfig) -> Result<()> {
    let vars = [
        ("bio5", 0.1, -273.15),
        ("bio6", 0.1, -273.15),
        ("bio12", 0.1, 0.0),
    ];
    vars.par_iter()
        .filter(|(v, _, _)| cfg.force || !chelsa_path(&cfg.work, v).exists())
        .map(|(v, scale, offset)| {
            let rel = format!("chelsa/CHELSA_{v}_1981-2010_V.2.1.tif");
            let src = sources::pinned(&cfg.sources, &rel)?;
            let layer = chelsa_mean(&src, *scale, *offset)?;
            raster::write_f32(&chelsa_path(&cfg.work, v), &[&layer])?;
            eprintln!("chelsa {v}: done");
            Ok(())
        })
        .collect::<Result<Vec<()>>>()?;
    Ok(())
}

/// Mean of a CHELSA `u16` layer per cell; the file's nodata is excluded.
pub(super) fn chelsa_mean(src: &Path, scale: f64, offset: f64) -> Result<Vec<f32>> {
    let nodata = super::tiff_rows::TiffRows::open(src)?.info.nodata;
    block_mean(src, scale, offset, move |v| {
        Some(f64::from(v)) != nodata && v >= 0.0
    })
}

fn stage_beck(cfg: &BuildConfig) -> Result<()> {
    for key in beck_keys() {
        if !cfg.beck_only.is_empty() && !cfg.beck_only.contains(&key) {
            continue;
        }
        let out = beck_path(&cfg.work, &key);
        if out.exists() && !cfg.force {
            continue;
        }
        let t0 = Instant::now();
        let rel = format!("beck2023/c0p1/{}/ensemble_mean_0p1.nc", slot_dir(&key));
        let layers = beck_summaries(&sources::pinned(&cfg.sources, &rel)?)?;
        let refs: Vec<&[f32]> = layers.iter().map(Vec::as_slice).collect();
        raster::write_f32(&out, &refs)?;
        eprintln!("beck {key}: {:.1} s", t0.elapsed().as_secs_f64());
    }
    Ok(())
}

/// Bands of a `beck/<key>.tif` work file.
pub(super) mod beck_band {
    pub const TCOLD: usize = 0;
    pub const TWARM: usize = 1;
    pub const PRECIP: usize = 2;
    pub const ARIDITY: usize = 3;
    pub const DRY: usize = 4;
    pub const KOPPEN_OF_MEANS: usize = 5;
    pub const COUNT: usize = 6;
}

/// Per-cell summaries of a Beck monthly NetCDF, north-up rows.
fn beck_summaries(nc: &Path) -> Result<Vec<Vec<f32>>> {
    let vars = list_vars(nc)?;
    let find = |name: &str| {
        vars.iter()
            .find(|v| v.name == name)
            .ok_or_else(|| PipelineError::NetCdf(format!("{}: no {name}", nc.display())))
    };
    let (tv, pv) = (find("air_temperature")?, find("precipitation")?);
    let n = GRID_W * GRID_H;
    for v in [tv, pv] {
        if v.dims != [12, GRID_H as u64, GRID_W as u64] {
            return Err(PipelineError::NetCdf(format!(
                "{}: dims {:?}",
                v.name, v.dims
            )));
        }
    }
    let t = read_var_f32(nc, tv)?;
    let p = read_var_f32(nc, pv)?;
    let (t_fill, p_fill) = (tv.fill, pv.fill);
    let valid = |x: f32, fill: Option<f32>| x.is_finite() && Some(x) != fill && x.abs() < 1e6;
    // Row 0 is the North Pole (all ocean) or the South Pole (Antarctic
    // land): pick the orientation with valid data at the south.
    let valid_in_row = |row: usize| {
        (0..GRID_W)
            .filter(|&c| valid(t[row * GRID_W + c], t_fill))
            .count()
    };
    let south_up = valid_in_row(0) > valid_in_row(GRID_H - 1);
    let cells: Vec<Option<MonthlySummary>> = (0..n)
        .into_par_iter()
        .map(|i| {
            let (r, c) = (i / GRID_W, i % GRID_W);
            let src_row = if south_up { GRID_H - 1 - r } else { r };
            let j = src_row * GRID_W + c;
            let mut tm = [0f32; 12];
            let mut pm = [0f32; 12];
            for m in 0..12 {
                tm[m] = t[m * n + j];
                pm[m] = p[m * n + j];
                if !valid(tm[m], t_fill) || !valid(pm[m], p_fill) {
                    return None;
                }
            }
            let lat = 90.0 - (r as f64 + 0.5) * 0.1;
            MonthlySummary::new(&tm, &pm, lat)
        })
        .collect();
    let mut layers = vec![vec![f32::NAN; n]; beck_band::COUNT];
    for (i, s) in cells.iter().enumerate() {
        if let Some(s) = s {
            layers[beck_band::TCOLD][i] = s.tmean_cold;
            layers[beck_band::TWARM][i] = s.tmean_warm;
            layers[beck_band::PRECIP][i] = s.precip;
            layers[beck_band::ARIDITY][i] = s.aridity;
            layers[beck_band::DRY][i] = f32::from(s.dry_months);
            layers[beck_band::KOPPEN_OF_MEANS][i] = s.koppen_of_means.map_or(f32::NAN, f32::from);
        }
    }
    eprintln!(
        "  {} south_up={south_up} valid={}",
        nc.display(),
        cells.iter().filter(|c| c.is_some()).count()
    );
    Ok(layers)
}

/// Distribution of differences.
#[must_use]
pub fn diff_stats(diffs: &mut [f32]) -> Value {
    diffs.sort_by(f32::total_cmp);
    let n = diffs.len();
    if n == 0 {
        return json!({ "n": 0 });
    }
    let mean = diffs.iter().map(|&x| f64::from(x)).sum::<f64>() / n as f64;
    let mae = diffs.iter().map(|&x| f64::from(x.abs())).sum::<f64>() / n as f64;
    let q = |f: f64| f64::from(diffs[((n - 1) as f64 * f).round() as usize]);
    let r2 = |x: f64| (x * 100.0).round() / 100.0;
    json!({
        "n": n, "mean": r2(mean), "mean_abs": r2(mae),
        "p05": r2(q(0.05)), "p50": r2(q(0.5)), "p95": r2(q(0.95)),
    })
}

fn stage_check_future(cfg: &BuildConfig) -> Result<()> {
    let out = cfg.work.join("check-future.json");
    if out.exists() && !cfg.force {
        return Ok(());
    }
    let problems = sources::verify(&cfg.sources, false)?;
    if !problems.is_empty() {
        return Err(PipelineError::Source(problems.join("\n")));
    }
    let gcms = [
        "gfdl-esm4",
        "ipsl-cm6a-lr",
        "mpi-esm1-2-hr",
        "mri-esm2-0",
        "ukesm1-0-ll",
    ];
    let layers: Vec<Vec<f32>> = gcms
        .par_iter()
        .map(|g| {
            let rel = format!("chelsa/CHELSA_bio6_2041-2070_{g}_ssp370_V.2.1.tif");
            chelsa_mean(&sources::pinned(&cfg.sources, &rel)?, 0.1, -273.15)
        })
        .collect::<Result<_>>()?;
    let n = GRID_W * GRID_H;
    let chelsa_future: Vec<f32> = (0..n)
        .map(|i| layers.iter().map(|l| l[i]).sum::<f32>() / layers.len() as f32)
        .collect();
    let base = super::assemble::Baseline::load(&cfg.work)?;
    let slot = SLOTS
        .iter()
        .position(|s| slot_key(*s) == "2041_2070_ssp370")
        .ok_or_else(|| PipelineError::Data("no 2041-2070 SSP3-7.0 slot".into()))?;
    let fut = raster::read_f32(&beck_path(&cfg.work, &slot_key(SLOTS[slot])))?;
    let land = raster::read_u8(&koppen_path(&cfg.work, "1991_2020"))?;
    let mut d_value = Vec::new();
    let mut d_change = Vec::new();
    for i in 0..n {
        let ours = base.tmin(i, fut[beck_band::TCOLD][i]);
        let (c, b6) = (chelsa_future[i], base.bio6[i]);
        if land[i] > 0 && ours.is_finite() && c.is_finite() && b6.is_finite() {
            d_value.push(ours - c);
            d_change.push((ours - base.tmin(i, base.tcold_now[i])) - (c - b6));
        }
    }
    let v = json!({
        "what": "2041-2070 SSP3-7.0 tmin_cold: grid (CHELSA 1981-2010 + baseline shift + Beck ensemble change) minus the mean of CHELSA's own bio6 projections for 5 GCMs",
        "gcms": gcms,
        "value_difference_c": diff_stats(&mut d_value),
        "change_difference_c": diff_stats(&mut d_change),
    });
    eprintln!("check-future: {v}");
    write_json(&out, &v)
}

/// Summary of a JSON file for the build report (`null` when absent).
pub(super) fn optional_json(path: &Path) -> Value {
    read_json(path).unwrap_or(Value::Null)
}

/// Map of slot keys to their index.
#[must_use]
pub fn slot_indices() -> BTreeMap<String, usize> {
    SLOTS
        .iter()
        .enumerate()
        .map(|(i, s)| (slot_key(*s), i))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_keys_and_source_dirs() {
        assert_eq!(slot_key(SLOTS[0]), "1991_2020");
        assert_eq!(slot_key(SLOTS[2]), "2041_2070_ssp245");
        assert_eq!(slot_dir("2071_2099_ssp585"), "2071_2099/ssp585");
        assert_eq!(slot_dir("1961_1990"), "1961_1990");
        let keys = beck_keys();
        assert_eq!(keys.len(), 10);
        assert_eq!(keys[0], "1961_1990");
        assert_eq!(slot_indices()["2071_2099_ssp370"], 7);
    }

    #[test]
    fn every_slot_has_pinned_sources() {
        let dir = Path::new("/nonexistent");
        for key in beck_keys() {
            let nc = format!("beck2023/c0p1/{}/ensemble_mean_0p1.nc", slot_dir(&key));
            assert!(sources::pinned(dir, &nc).is_ok(), "{nc}");
        }
        for s in SLOTS {
            let kg = format!(
                "beck2023/kg/{}/koppen_geiger_0p00833333.tif",
                slot_dir(&slot_key(s))
            );
            assert!(sources::pinned(dir, &kg).is_ok(), "{kg}");
        }
    }

    #[test]
    fn diff_stats_quantiles() {
        let mut d: Vec<f32> = (0..=100).map(|i| i as f32).collect();
        let v = diff_stats(&mut d);
        assert_eq!(v["n"], 101);
        assert_eq!(v["p50"], 50.0);
        assert_eq!(v["p05"], 5.0);
        assert_eq!(v["mean"], 50.0);
        assert_eq!(diff_stats(&mut [])["n"], 0);
    }
}
