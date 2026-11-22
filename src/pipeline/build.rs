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
//! | `tiles` | the work files | out: `tiles/*.tif`, `manifest.json`, `SHA256SUMS`, `build-report.json`, `ATTRIBUTION.md` (site layers already built carry over) |
//! | `regions` | WGSRPD level 3, Natural Earth admin-0 map units 1:50m | out: `regions/*.tif`, manifest layer `regions`; removes a `soil` layer built on older regions |
//! | `soil` | SoilGrids 2.0 5 km aggregates, the `regions` layer (land mask) | out: `soil/*.tif`, manifest layer `soil` |
//! | `extremes` | the climate tiles, ERA5-Land and NEX-GDDP-CMIP6 rasters from Earth Engine (`scripts/ee/extremes.py`) | out: `extremes/*.tif`, manifest layer `extremes` |
//! | `check-future` | CHELSA 2041-2070 SSP3-7.0 bio6 (5 GCMs) | `check-future.json` |

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rayon::prelude::{IntoParallelIterator, IntoParallelRefIterator, ParallelIterator};
use serde_json::{Value, json};

use super::aggregate::{block_mean, block_mode};
use super::netcdf::{list_vars, read_var_f32, read_vector_f64};
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
    /// CHELSA V2.1 monthly tasmin means per cell.
    ChelsaMonthly,
    /// Beck monthly climatologies to per-cell summaries.
    Beck,
    /// Assemble, write tiles, manifest and report.
    Tiles,
    /// Site layer: WGSRPD level-3 and country codes (after `tiles`).
    Regions,
    /// Site layer: SoilGrids pH, sand, clay (after `regions`).
    Soil,
    /// Layer: coldest night and hottest day per slot (after `tiles`).
    Extremes,
    /// Layer: monthly tmean, tmin, dry_mask for 3 slots (after `tiles` and `chelsa-monthly`).
    Monthly,
    /// Layer: 0.25-degree monthly precipitation for 3 slots (after `tiles`).
    MonthlyPrecip,
    /// Compare the delta method with CHELSA's own projections.
    CheckFuture,
}

/// Stages run by default, in order.
pub const DEFAULT_STAGES: [Stage; 11] = [
    Stage::Verify,
    Stage::Koppen,
    Stage::Chelsa,
    Stage::ChelsaMonthly,
    Stage::Beck,
    Stage::Tiles,
    Stage::Regions,
    Stage::Soil,
    Stage::Extremes,
    Stage::Monthly,
    Stage::MonthlyPrecip,
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
pub(super) fn slot_dir(key: &str) -> String {
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
            Stage::ChelsaMonthly => super::monthly::stage_chelsa_monthly(cfg)?,
            Stage::Beck => stage_beck(cfg)?,
            Stage::Tiles => super::assemble::stage_tiles(cfg)?,
            Stage::Regions => super::regions::stage_regions(cfg)?,
            Stage::Soil => super::soil::stage_soil(cfg)?,
            Stage::Extremes => super::extremes::stage_extremes(cfg)?,
            Stage::Monthly => super::monthly::stage_monthly(cfg)?,
            Stage::MonthlyPrecip => super::monthly_precip::stage_monthly_precip(cfg)?,
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
        .map(|&(v, scale, offset)| {
            let rel = format!("chelsa/CHELSA_{v}_1981-2010_V.2.1.tif");
            let src = sources::pinned(&cfg.sources, &rel)?;
            let layer = chelsa_mean(&src, scale, offset)?;
            raster::write_f32(&chelsa_path(&cfg.work, v), &[&layer])?;
            eprintln!("chelsa {v}: done");
            Ok(())
        })
        .collect::<Result<Vec<()>>>()?;
    Ok(())
}

/// Mean of a CHELSA `u16` layer per cell. Excluded: the nodata value
/// ([`u16_nodata`]) and, for temperatures in K x 10 (negative `offset`),
/// zero.
pub(super) fn chelsa_mean(src: &Path, scale: f64, offset: f64) -> Result<Vec<f32>> {
    let info = super::tiff_rows::TiffRows::open(src)?.info;
    let nodata = u16_nodata(info.sample_format, info.bits, info.nodata)
        .map_err(|e| PipelineError::Data(format!("{}: {e}", src.display())))?;
    let min_valid = if offset < 0.0 { 1.0 } else { 0.0 };
    block_mean(src, scale, offset, move |v| v != nodata && v >= min_valid)
}

/// The nodata value of an unsigned 16-bit source: the declared value when a
/// sample can hold it, else 65535. CHELSA V2.1 declares -99999, which no
/// `u16` sample can equal. Other sample types are refused.
///
/// # Errors
/// A source that is not unsigned 16-bit.
pub(super) fn u16_nodata(
    format: wbgeotiff::SampleFormat,
    bits: u16,
    declared: Option<f64>,
) -> std::result::Result<f32, String> {
    if format != wbgeotiff::SampleFormat::Uint || bits != 16 {
        return Err(format!(
            "expected unsigned 16-bit samples, found {format:?} {bits}-bit"
        ));
    }
    Ok(match declared {
        Some(x) if x.fract() == 0.0 && (0.0..=65535.0).contains(&x) => x as f32,
        _ => 65535.0,
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

/// Whether a variable's rows run south to north, from its coordinate
/// variables: `lon` must start at the 179.95 W cell centre and `lat` at
/// 89.95 N (north-up) or 89.95 S (south-up), both in 0.1-degree steps.
///
/// # Errors
/// Coordinates that are not the 0.1-degree grid.
pub(super) fn grid_orientation(lat: &[f64], lon: &[f64]) -> std::result::Result<bool, String> {
    let steps = |c: &[f64], first: f64, step: f64| {
        c.iter()
            .enumerate()
            .all(|(i, &x)| (x - (first + step * i as f64)).abs() < 1e-4)
    };
    if lon.len() != GRID_W || !steps(lon, -179.95, 0.1) {
        return Err(format!(
            "lon is not 3600 cells from 179.95 W eastwards (starts {:?})",
            lon.first()
        ));
    }
    if lat.len() == GRID_H && steps(lat, 89.95, -0.1) {
        Ok(false)
    } else if lat.len() == GRID_H && steps(lat, -89.95, 0.1) {
        Ok(true)
    } else {
        Err(format!(
            "lat is not 1800 cells from either pole (starts {:?})",
            lat.first()
        ))
    }
}

/// Read one 12-month variable (`air_temperature` or `precipitation`) from a
/// Beck 0.1-degree NetCDF, oriented north-up (`12 * GRID_H * GRID_W`), with
/// invalid/fill values replaced by `f32::NAN`.
pub(super) fn read_beck_monthly_var(nc: &Path, var_name: &str) -> Result<Vec<f32>> {
    let vars = list_vars(nc)?;
    let find = |name: &str| {
        vars.iter()
            .find(|v| v.name == name)
            .ok_or_else(|| PipelineError::NetCdf(format!("{}: no {name}", nc.display())))
    };
    let var = find(var_name)?;
    let lat = read_vector_f64(nc, find("lat")?)?;
    let lon = read_vector_f64(nc, find("lon")?)?;
    let south_up = grid_orientation(&lat, &lon)
        .map_err(|e| PipelineError::NetCdf(format!("{}: {e}", nc.display())))?;
    let n = GRID_W * GRID_H;
    if var.dims != [12, GRID_H as u64, GRID_W as u64] {
        return Err(PipelineError::NetCdf(format!(
            "{}: dims {:?}",
            var.name, var.dims
        )));
    }
    let raw = read_var_f32(nc, var)?;
    let fill = var.fill;
    let valid = |x: f32| x.is_finite() && Some(x) != fill && x.abs() < 1e6;
    let mut out = vec![f32::NAN; 12 * n];
    for m in 0..12 {
        let off = m * n;
        for r in 0..GRID_H {
            let src_row = if south_up { GRID_H - 1 - r } else { r };
            let dst_base = off + r * GRID_W;
            let src_base = off + src_row * GRID_W;
            for c in 0..GRID_W {
                let v = raw[src_base + c];
                if valid(v) {
                    out[dst_base + c] = v;
                }
            }
        }
    }
    Ok(out)
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
    let lat = read_vector_f64(nc, find("lat")?)?;
    let lon = read_vector_f64(nc, find("lon")?)?;
    let south_up = grid_orientation(&lat, &lon)
        .map_err(|e| PipelineError::NetCdf(format!("{}: {e}", nc.display())))?;
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
            layers[beck_band::ARIDITY][i] = s.aridity.unwrap_or(f32::NAN);
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
    let ours: Vec<f32> = (0..n)
        .map(|i| base.tmin(i, fut[beck_band::TCOLD][i]))
        .collect();
    let mut v = future_check(&ours, &chelsa_future, &base.bio6, &land);
    v["gcms"] = json!(gcms);
    eprintln!("check-future: {v}");
    write_json(&out, &v)
}

/// Grid 2041-2070 `tmin_cold` against CHELSA's own projection, both
/// measured from the same 1981-2010 baseline (CHELSA bio6): the change
/// difference `(ours - bio6) - (chelsa - bio6)` equals the value difference.
/// The grid's own change from 1991-2020 is not compared: CHELSA's change
/// starts 10 years earlier.
fn future_check(ours: &[f32], chelsa: &[f32], bio6: &[f32], land: &[u8]) -> Value {
    let mut d: Vec<f32> = (0..ours.len())
        .filter(|&i| {
            land[i] > 0 && ours[i].is_finite() && chelsa[i].is_finite() && bio6[i].is_finite()
        })
        .map(|i| (ours[i] - bio6[i]) - (chelsa[i] - bio6[i]))
        .collect();
    json!({
        "what": "2041-2070 SSP3-7.0 tmin_cold: grid (CHELSA 1981-2010 + baseline shift + Beck ensemble change) minus the mean of CHELSA's own bio6 projections for 5 GCMs; equal to the difference of the two changes from CHELSA's 1981-2010 baseline",
        "difference_c": diff_stats(&mut d),
    })
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
    fn u16_nodata_falls_back_when_unrepresentable() {
        use wbgeotiff::SampleFormat;
        assert_eq!(
            u16_nodata(SampleFormat::Uint, 16, Some(-99999.0)),
            Ok(65535.0)
        );
        assert_eq!(u16_nodata(SampleFormat::Uint, 16, None), Ok(65535.0));
        assert_eq!(u16_nodata(SampleFormat::Uint, 16, Some(0.0)), Ok(0.0));
        assert_eq!(u16_nodata(SampleFormat::Uint, 16, Some(1.5)), Ok(65535.0));
        assert!(u16_nodata(SampleFormat::Int, 16, Some(-99999.0)).is_err());
        assert!(u16_nodata(SampleFormat::Uint, 8, None).is_err());
    }

    #[test]
    fn orientation_comes_from_the_coordinates() {
        let lon: Vec<f64> = (0..GRID_W).map(|i| -179.95 + 0.1 * i as f64).collect();
        let north: Vec<f64> = (0..GRID_H).map(|i| 89.95 - 0.1 * i as f64).collect();
        let south: Vec<f64> = north.iter().rev().copied().collect();
        assert_eq!(grid_orientation(&north, &lon), Ok(false));
        assert_eq!(grid_orientation(&south, &lon), Ok(true));
        // Shifted by half a cell, wrong length, or longitudes from 0 E.
        let shifted: Vec<f64> = north.iter().map(|x| x - 0.05).collect();
        assert!(grid_orientation(&shifted, &lon).is_err());
        assert!(grid_orientation(&north[1..], &lon).is_err());
        let east: Vec<f64> = (0..GRID_W).map(|i| 0.05 + 0.1 * i as f64).collect();
        assert!(grid_orientation(&north, &east).is_err());
    }

    #[test]
    fn future_check_uses_one_baseline() {
        // Grid warms 2.0 from bio6, CHELSA 2.5: the change difference is
        // -0.5 whatever the 1991-2020 shift; ocean cells are skipped.
        let bio6 = [-10.0, 5.0, 0.0];
        let ours = [-8.0, 7.0, 99.0];
        let chelsa = [-7.5, 7.5, 0.0];
        let v = future_check(&ours, &chelsa, &bio6, &[1, 1, 0]);
        assert_eq!(v["difference_c"]["n"], 2);
        assert_eq!(v["difference_c"]["mean"], -0.5);
        assert!(v.get("change_difference_c").is_none());
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
