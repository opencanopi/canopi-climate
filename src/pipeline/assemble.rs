//! The `tiles` stage: combine the work files into the stored variables,
//! write one COG per tile with land, the manifest, `SHA256SUMS`,
//! `ATTRIBUTION.md` and `build-report.json`.

use std::path::Path;

use rayon::prelude::{IntoParallelRefIterator, IntoParallelRefMutIterator, ParallelIterator};
use serde_json::{Value, json};

use super::build::{
    BASELINE_PAST, BuildConfig, beck_band, beck_path, chelsa_path, diff_stats, koppen_path,
    optional_json, slot_key,
};
use super::{PipelineError, Result, cog, raster, sha256_file, sources};
use crate::grid::{TILE_CELLS, TILE_LEN, TileId};
use crate::koppen;
use crate::manifest::{self, Encoding, GridSpec, Manifest, SlotSpec, TileSpec, VariableSpec};
use crate::model::{BANDS, NODATA, SLOTS, VARIABLES, Variable, band};
use crate::tile::Tile;

/// Baseline layers for the delta method.
pub struct Baseline {
    /// CHELSA bio5, 1981-2010 (°C).
    pub bio5: Vec<f32>,
    /// CHELSA bio6, 1981-2010 (°C).
    pub bio6: Vec<f32>,
    /// Beck coldest-month mean, 1991-2020.
    pub tcold_now: Vec<f32>,
    /// Beck warmest-month mean, 1991-2020.
    pub twarm_now: Vec<f32>,
    /// One third of Beck's 1961-1990 to 1991-2020 change (coldest month).
    pub shift_cold: Vec<f32>,
    /// Same for the warmest month.
    pub shift_warm: Vec<f32>,
}

impl Baseline {
    /// Load from the work directory.
    ///
    /// # Errors
    /// Missing or unreadable work files.
    pub fn load(work: &Path) -> Result<Self> {
        let first = |p: &Path| -> Result<Vec<f32>> {
            raster::read_f32(p)?
                .into_iter()
                .next()
                .ok_or_else(|| PipelineError::Data(format!("{}: no band", p.display())))
        };
        let bio5 = first(&chelsa_path(work, "bio5"))?;
        let bio6 = first(&chelsa_path(work, "bio6"))?;
        let mut now = raster::read_f32(&beck_path(work, "1991_2020"))?;
        let mut past = raster::read_f32(&beck_path(work, BASELINE_PAST))?;
        if now.len() != beck_band::COUNT || past.len() != beck_band::COUNT {
            return Err(PipelineError::Data(
                "beck work files: wrong band count".into(),
            ));
        }
        let twarm_now = std::mem::take(&mut now[beck_band::TWARM]);
        let tcold_now = std::mem::take(&mut now[beck_band::TCOLD]);
        let twarm_past = std::mem::take(&mut past[beck_band::TWARM]);
        let tcold_past = std::mem::take(&mut past[beck_band::TCOLD]);
        // CHELSA 1981-2010 is centred 10 years before 1991-2020; Beck's two
        // observed periods are 30 years apart: move the baseline by a third
        // of their change (linear trend).
        let third = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| (x - y) / 3.0).collect();
        Ok(Self {
            shift_cold: third(&tcold_now, &tcold_past),
            shift_warm: third(&twarm_now, &twarm_past),
            bio5,
            bio6,
            tcold_now,
            twarm_now,
        })
    }

    /// Coldest-month mean daily minimum for a slot whose coldest-month mean
    /// is `tcold_slot`.
    #[must_use]
    pub fn tmin(&self, i: usize, tcold_slot: f32) -> f32 {
        self.bio6[i] + self.shift_cold[i] + (tcold_slot - self.tcold_now[i])
    }

    /// Warmest-month mean daily maximum for a slot whose warmest-month mean
    /// is `twarm_slot`.
    #[must_use]
    pub fn tmax(&self, i: usize, twarm_slot: f32) -> f32 {
        self.bio5[i] + self.shift_warm[i] + (twarm_slot - self.twarm_now[i])
    }
}

/// Encoded values of one slot for one cell (variables in band order).
fn encode_cell(i: usize, kg: &[u8], beck: &[Vec<f32>], base: &Baseline) -> [i16; 8] {
    let tcold = beck[beck_band::TCOLD][i];
    let twarm = beck[beck_band::TWARM][i];
    let k = kg[i];
    [
        if k > 0 { i16::from(k) } else { NODATA },
        Variable::TminCold.encode(base.tmin(i, tcold)),
        Variable::TmaxWarm.encode(base.tmax(i, twarm)),
        Variable::TmeanCold.encode(tcold),
        Variable::TmeanWarm.encode(twarm),
        Variable::Precip.encode(beck[beck_band::PRECIP][i]),
        Variable::Aridity.encode(beck[beck_band::ARIDITY][i]),
        Variable::DryMonths.encode(beck[beck_band::DRY][i]),
    ]
}

/// Counts for one slot: Köppen class against the cell's own temperatures,
/// and CHELSA-based extremes against Beck's monthly means.
#[derive(Default)]
struct SlotStats {
    land: u64,
    with_class_and_temps: u64,
    margin_gt0: u64,
    margin_gt1: u64,
    margin_gt2: u64,
    margin_gt1_by_group: std::collections::BTreeMap<char, u64>,
    class_of_means_compared: u64,
    class_of_means_same: u64,
    group_of_means_same: u64,
    tmin_above_tmean_cold: u64,
    tmax_below_tmean_warm: u64,
    class_missing: u64,
}

impl SlotStats {
    fn add(&mut self, v: &[i16; 8], of_means: f32) {
        self.land += 1;
        let d = |var: Variable| var.decode(v[var.index()]).map(|x| x as f32);
        let code = u8::try_from(v[0]).ok().filter(|c| (1..=30).contains(c));
        let Some(code) = code else {
            self.class_missing += 1;
            return;
        };
        if let (Some(tc), Some(tw)) = (d(Variable::TmeanCold), d(Variable::TmeanWarm)) {
            self.with_class_and_temps += 1;
            let m = koppen::temperature_margin(code, tc, tw);
            self.margin_gt0 += u64::from(m > 0.0);
            self.margin_gt1 += u64::from(m > 1.0);
            self.margin_gt2 += u64::from(m > 2.0);
            if m > 1.0 {
                let g = koppen::group(code).unwrap_or('?');
                *self.margin_gt1_by_group.entry(g).or_default() += 1;
            }
            if d(Variable::TminCold).is_some_and(|t| t > tc) {
                self.tmin_above_tmean_cold += 1;
            }
            if d(Variable::TmaxWarm).is_some_and(|t| t < tw) {
                self.tmax_below_tmean_warm += 1;
            }
        }
        if of_means.is_finite() {
            self.class_of_means_compared += 1;
            let om = of_means as u8;
            self.class_of_means_same += u64::from(om == code);
            self.group_of_means_same += u64::from(koppen::group(om) == koppen::group(code));
        }
    }

    fn json(&self) -> Value {
        let pct = |a: u64, b: u64| (1000.0 * a as f64 / b.max(1) as f64).round() / 10.0;
        json!({
            "land_cells": self.land,
            "cells_without_class": self.class_missing,
            "class_vs_own_temperatures": {
                "cells": self.with_class_and_temps,
                "margin_gt_0c": self.margin_gt0,
                "margin_gt_1c": self.margin_gt1,
                "margin_gt_2c": self.margin_gt2,
                "percent_gt_1c": pct(self.margin_gt1, self.with_class_and_temps),
                "margin_gt_1c_by_group": self.margin_gt1_by_group,
            },
            "class_vs_class_of_monthly_means": {
                "cells": self.class_of_means_compared,
                "same_class": self.class_of_means_same,
                "same_group": self.group_of_means_same,
                "percent_same_class": pct(self.class_of_means_same, self.class_of_means_compared),
                "percent_same_group": pct(self.group_of_means_same, self.class_of_means_compared),
            },
            "tmin_cold_above_tmean_cold": self.tmin_above_tmean_cold,
            "tmax_warm_below_tmean_warm": self.tmax_below_tmean_warm,
        })
    }
}

/// Tiles holding at least one land cell, with NODATA-filled buffers.
fn empty_tiles(land: &[u8]) -> Vec<(TileId, Vec<i16>)> {
    TileId::all()
        .filter(|t| (0..TILE_LEN).any(|j| land[t.cell(j).global_index()] > 0))
        .map(|t| (t, vec![NODATA; TILE_LEN * BANDS]))
        .collect()
}

/// Fill one slot into every tile and return its statistics.
fn fill_slot(
    tiles: &mut [(TileId, Vec<i16>)],
    slot: usize,
    land: &[u8],
    kg: &[u8],
    beck: &[Vec<f32>],
    base: &Baseline,
) -> SlotStats {
    let parts: Vec<SlotStats> = tiles
        .par_iter_mut()
        .map(|(id, data)| {
            let mut st = SlotStats::default();
            for j in 0..TILE_LEN {
                let i = id.cell(j).global_index();
                if land[i] == 0 {
                    continue;
                }
                let v = encode_cell(i, kg, beck, base);
                for (vi, x) in v.iter().enumerate() {
                    data[j * BANDS + band(vi, slot)] = *x;
                }
                st.add(&v, beck[beck_band::KOPPEN_OF_MEANS][i]);
            }
            st
        })
        .collect();
    let mut total = SlotStats::default();
    for p in parts {
        total.land += p.land;
        total.with_class_and_temps += p.with_class_and_temps;
        total.margin_gt0 += p.margin_gt0;
        total.margin_gt1 += p.margin_gt1;
        total.margin_gt2 += p.margin_gt2;
        for (g, n) in p.margin_gt1_by_group {
            *total.margin_gt1_by_group.entry(g).or_default() += n;
        }
        total.class_of_means_compared += p.class_of_means_compared;
        total.class_of_means_same += p.class_of_means_same;
        total.group_of_means_same += p.group_of_means_same;
        total.tmin_above_tmean_cold += p.tmin_above_tmean_cold;
        total.tmax_below_tmean_warm += p.tmax_below_tmean_warm;
        total.class_missing += p.class_missing;
    }
    total
}

/// Differences between sources over land, for the report.
fn source_checks(
    land: &[u8],
    beck_now: &[Vec<f32>],
    base: &Baseline,
    work: &Path,
) -> Result<Value> {
    let bio12 = raster::read_f32(&chelsa_path(work, "bio12"))?
        .into_iter()
        .next()
        .unwrap_or_default();
    let mut d_precip = Vec::new();
    let mut d_bio6 = Vec::new();
    let mut shift_c = Vec::new();
    let mut shift_w = Vec::new();
    for i in 0..land.len() {
        if land[i] == 0 {
            continue;
        }
        let p = beck_now[beck_band::PRECIP][i];
        if let Some(&b12) = bio12.get(i).filter(|x| x.is_finite() && **x > 0.0)
            && p.is_finite()
        {
            d_precip.push(100.0 * (p - b12) / b12);
        }
        let tc = base.tcold_now[i];
        if base.bio6[i].is_finite() && tc.is_finite() {
            d_bio6.push(base.bio6[i] - tc);
        }
        if base.shift_cold[i].is_finite() {
            shift_c.push(base.shift_cold[i]);
        }
        if base.shift_warm[i].is_finite() {
            shift_w.push(base.shift_warm[i]);
        }
    }
    Ok(json!({
        "beck_precip_vs_chelsa_bio12_percent": diff_stats(&mut d_precip),
        "chelsa_bio6_minus_beck_coldest_month_mean_c": diff_stats(&mut d_bio6),
        "baseline_shift_cold_c": diff_stats(&mut shift_c),
        "baseline_shift_warm_c": diff_stats(&mut shift_w),
    }))
}

/// Write every tile; returns the manifest entries sorted by name.
fn write_tiles(tiles: Vec<(TileId, Vec<i16>)>, out: &Path) -> Result<Vec<TileSpec>> {
    let dir = out.join("tiles");
    std::fs::create_dir_all(&dir).map_err(|e| PipelineError::io(&dir, e))?;
    let mut specs: Vec<TileSpec> = tiles
        .into_iter()
        .collect::<Vec<_>>()
        .par_iter()
        .map(|(id, data)| {
            let tile = Tile::new(*id, data.clone())?;
            let path = dir.join(id.file_name());
            let bytes = cog::write_tile_checked(&tile, &path)?;
            Ok(TileSpec {
                name: id.name(),
                path: format!("tiles/{}", id.file_name()),
                bytes: bytes.len() as u64,
                sha256: super::sha256_bytes(&bytes),
                complete_cells: u32::try_from(tile.complete_cells()).unwrap_or(u32::MAX),
            })
        })
        .collect::<Result<_>>()?;
    specs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(specs)
}

/// Run the `tiles` stage.
///
/// # Errors
/// Missing work files, writer or I/O errors.
pub fn stage_tiles(cfg: &BuildConfig) -> Result<()> {
    let manifest_path = cfg.out.join("manifest.json");
    if manifest_path.exists() && !cfg.force {
        eprintln!("tiles: {} exists, skipping", manifest_path.display());
        return Ok(());
    }
    let land = raster::read_u8(&koppen_path(&cfg.work, "1991_2020"))?;
    let base = Baseline::load(&cfg.work)?;
    let mut tiles = empty_tiles(&land);
    let mut slot_stats = serde_json::Map::new();
    let mut checks = Value::Null;
    for (s, slot) in SLOTS.iter().enumerate() {
        let key = slot_key(*slot);
        let kg = raster::read_u8(&koppen_path(&cfg.work, &key))?;
        let beck = raster::read_f32(&beck_path(&cfg.work, &key))?;
        if beck.len() != beck_band::COUNT {
            return Err(PipelineError::Data(format!(
                "beck/{key}.tif: wrong band count"
            )));
        }
        let st = fill_slot(&mut tiles, s, &land, &kg, &beck, &base);
        slot_stats.insert(key.clone(), st.json());
        if s == 0 {
            checks = source_checks(&land, &beck, &base, &cfg.work)?;
        }
        eprintln!("tiles: slot {key} filled");
    }
    let specs = write_tiles(tiles, &cfg.out)?;
    finish(cfg, specs, Value::Object(slot_stats), checks)
}

fn finish(cfg: &BuildConfig, specs: Vec<TileSpec>, slot_stats: Value, checks: Value) -> Result<()> {
    let total: u64 = specs.iter().map(|t| t.bytes).sum();
    let mut sizes: Vec<u64> = specs.iter().map(|t| t.bytes).collect();
    sizes.sort_unstable();
    let complete: u64 = specs.iter().map(|t| u64::from(t.complete_cells)).sum();
    let report = json!({
        "tiles": specs.len(),
        "total_bytes": total,
        "median_tile_bytes": sizes.get(sizes.len() / 2),
        "max_tile_bytes": sizes.last(),
        "complete_cells": complete,
        "koppen": optional_json(&cfg.work.join("koppen.json")),
        "slots": slot_stats,
        "source_checks": checks,
        "check_future": optional_json(&cfg.work.join("check-future.json")),
    });
    write_json_file(&cfg.out.join("build-report.json"), &report)?;
    let m = manifest(specs);
    let text = serde_json::to_string_pretty(&m).map_err(|e| PipelineError::Data(e.to_string()))?;
    let mpath = cfg.out.join("manifest.json");
    std::fs::write(&mpath, text + "\n").map_err(|e| PipelineError::io(&mpath, e))?;
    write_sums(&cfg.out, &m)?;
    write_attribution(&cfg.out)?;
    eprintln!(
        "tiles: {} files, {:.1} MiB, max {} B, {complete} complete cells",
        m.tiles.len(),
        total as f64 / 1_048_576.0,
        sizes.last().copied().unwrap_or(0)
    );
    Ok(())
}

fn write_json_file(path: &Path, v: &Value) -> Result<()> {
    let s = serde_json::to_string_pretty(v).map_err(|e| PipelineError::Data(e.to_string()))?;
    std::fs::write(path, s + "\n").map_err(|e| PipelineError::io(path, e))
}

/// `SHA256SUMS` over the manifest, the report and every tile.
fn write_sums(out: &Path, m: &Manifest) -> Result<()> {
    let mut lines = Vec::new();
    for name in ["manifest.json", "build-report.json"] {
        lines.push(format!("{}  {name}", sha256_file(&out.join(name))?));
    }
    for t in &m.tiles {
        lines.push(format!("{}  {}", t.sha256, t.path));
    }
    let p = out.join("SHA256SUMS");
    std::fs::write(&p, lines.join("\n") + "\n").map_err(|e| PipelineError::io(&p, e))
}

fn write_attribution(out: &Path) -> Result<()> {
    let mut s = String::from(
        "# Attribution\n\nThis grid is derived from the datasets below by canopi-climate \
         (https://github.com/opencanopi/canopi-climate, AGPL-3.0-or-later). Their licences and \
         attribution requirements apply to the grid; cite them when you use it.\n",
    );
    for src in sources::manifest_sources() {
        s.push_str(&format!(
            "\n## {}\n\n- Citation: {}\n- Data: {}\n- Licence: {}\n- Notes: {}\n",
            src.title, src.citation, src.landing, src.licence, src.notes
        ));
    }
    let p = out.join("ATTRIBUTION.md");
    std::fs::write(&p, s).map_err(|e| PipelineError::io(&p, e))
}

/// UTC time as RFC 3339, from `SOURCE_DATE_EPOCH` when set.
fn built_at() -> String {
    let secs = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
        });
    format_utc(secs)
}

/// RFC 3339 UTC time of a Unix timestamp.
fn format_utc(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// The manifest for a set of tiles.
#[must_use]
pub fn manifest(tiles: Vec<TileSpec>) -> Manifest {
    Manifest {
        format: manifest::FORMAT.into(),
        format_version: manifest::FORMAT_VERSION,
        builder_version: env!("CARGO_PKG_VERSION").into(),
        built_at: built_at(),
        grid: GridSpec {
            crs: "EPSG:4326".into(),
            cell_deg: crate::grid::CELL_DEG,
            tile_deg: crate::grid::TILE_DEG,
            tile_cells: TILE_CELLS as u32,
            tile_naming: "north-west corner: n50e000 covers 40-50 N, 0-10 E; tiles without land are absent".into(),
            cell_edges: "west and north edges inclusive; latitude -90 in the last row; longitude 180 wraps to -180".into(),
        },
        encoding: Encoding {
            container: "Cloud Optimized GeoTIFF (EPSG:4326, one 112 x 112 internal tile, no overviews)".into(),
            sample_format: "int16".into(),
            nodata: NODATA,
            compression: "deflate".into(),
            interleave: "pixel".into(),
            bands: BANDS as u32,
            band_order: format!(
                "band (1-based) = variable_index * {} + slot_index + 1, indices 0-based in the variables and slots lists",
                SLOTS.len()
            ),
        },
        variables: VARIABLES
            .iter()
            .map(|v| {
                let i = v.info();
                VariableSpec {
                    id: i.id.into(),
                    unit: i.unit.into(),
                    scale: i.scale,
                    offset: i.offset,
                    description: i.description.into(),
                }
            })
            .collect(),
        slots: SLOTS
            .iter()
            .map(|s| SlotSpec {
                period: s.period.label().into(),
                scenario: s.scenario.id().into(),
                scenario_label: s.scenario.label().into(),
            })
            .collect(),
        sources: sources::manifest_sources(),
        tiles,
        notes: notes(),
    }
}

fn notes() -> Vec<String> {
    [
        "Future values: tmin_cold and tmax_warm add the change in Beck et al. (2023)'s coldest/warmest monthly mean (ensemble mean of constrained CMIP6 models) to the 1991-2020 baseline (delta method); the other variables are computed from Beck's projected monthly means directly.",
        "Baseline: CHELSA bio5/bio6 cover 1981-2010; they are moved to 1991-2020 by one third of the change of Beck's coldest/warmest monthly mean between 1961-1990 and 1991-2020 (linear trend over the 10-year offset).",
        "Köppen classes come from Beck's 1-km maps (most frequent class in the cell), temperatures from Beck's 0.1-degree monthly means: in some cells the two disagree. Lookups report koppen_temperature_margin_c; build-report.json counts the cells.",
        "Aridity uses Thornthwaite PET, which runs low in hot dry climates compared with Penman-Monteith: compare aridity between places, not with thresholds computed from other PET formulas.",
        "Precipitation products disagree by tens of percent at a point (build-report.json compares Beck with CHELSA): treat precipitation and aridity as estimates.",
        "tmin_cold is the mean daily minimum of the coldest month, not the annual extreme minimum: no USDA hardiness zone is derived.",
        "Lookups use the point's cell when it has every value, else the nearest complete cell within 2 cells inside the same tile, else nothing.",
        "GeoTIFF tags carry no band names or scale/offset (wbgeotiff writes no GDAL metadata): use this manifest to read bands in GIS software.",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_at_formats_utc() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(1_762_823_045), "2025-11-11T01:04:05Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        let s = built_at();
        assert_eq!(s.len(), 20);
        assert!(s.ends_with('Z'));
    }

    #[test]
    fn manifest_lists_variables_slots_and_sources() {
        let m = manifest(vec![]);
        assert_eq!(m.variables.len(), 8);
        assert_eq!(m.slots.len(), 9);
        assert_eq!(m.encoding.bands, 72);
        assert_eq!(m.variables[1].id, "tmin_cold");
        assert!((m.variables[1].scale - 0.1).abs() < 1e-9);
        assert!(m.sources.iter().any(|s| s.id == "beck2023"));
        assert!(m.sources.iter().all(|s| s.id != "chelsa-v2.1-future-check"));
        assert!(m.notes.iter().any(|n| n.contains("USDA")));
    }

    #[test]
    fn baseline_formulas() {
        let b = Baseline {
            bio5: vec![30.0],
            bio6: vec![-5.0],
            tcold_now: vec![0.0],
            twarm_now: vec![22.0],
            shift_cold: vec![0.3],
            shift_warm: vec![0.4],
        };
        assert!((b.tmin(0, 0.0) + 4.7).abs() < 1e-6);
        assert!((b.tmin(0, 2.0) + 2.7).abs() < 1e-6);
        assert!((b.tmax(0, 25.0) - 33.4).abs() < 1e-6);
    }
}
