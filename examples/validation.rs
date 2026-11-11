//! Validation against ERA5 normals (Open-Meteo archive) and, where cached, the
//! Open-Meteo CMIP6 API.
//!
//! ```text
//! # daily ERA5 series -> monthly normals fixture
//! cargo run --release --features pipeline --example validation -- \
//!     normals <validation-dir> > tests/fixtures/era5-normals.json
//! # grid vs normals: markdown table on stdout, expected values fixture
//! cargo run --release --features pipeline --example validation -- \
//!     table <grid-dir> tests/fixtures/era5-normals.json tests/fixtures/validation-expected.json \
//!     [<validation-dir>/cmip6/prototype-8-cities-EC_Earth3P_HR.json]
//! ```
//!
//! `<validation-dir>` holds `era5/prototype-12-cities.json` (a list of 12
//! responses, in the order of the first 12 cities below) and
//! `era5/<slug>.json` for the others.

use std::error::Error;
use std::path::Path;

use canopi_climate::derived::MonthlySummary;
use canopi_climate::koppen;
use canopi_climate::pipeline::sample::Grid;
use serde_json::{Value, json};

type Res<T> = Result<T, Box<dyn Error>>;

/// Temperature tolerance (°C) and precipitation tolerance (fraction).
const TOL_T: f64 = 1.5;
const TOL_P: f64 = 0.40;

/// slug, name, latitude, longitude.
const CITIES: [(&str, &str, f64, f64); 22] = [
    ("paris", "Paris", 48.8566, 2.3522),
    ("lyon", "Lyon", 45.7640, 4.8357),
    ("seville", "Seville", 37.3891, -5.9845),
    ("montpellier", "Montpellier", 43.6108, 3.8767),
    ("berlin", "Berlin", 52.5200, 13.4050),
    ("nairobi", "Nairobi", -1.2921, 36.8219),
    ("sao-paulo", "São Paulo", -23.5505, -46.6333),
    ("manaus", "Manaus", -3.1190, -60.0217),
    ("mumbai", "Mumbai", 19.0760, 72.8777),
    ("tokyo", "Tokyo", 35.6762, 139.6503),
    ("phoenix", "Phoenix", 33.4484, -112.0740),
    ("hobart", "Hobart", -42.8821, 147.3272),
    ("reykjavik", "Reykjavik", 64.1466, -21.9426),
    ("yakutsk", "Yakutsk", 62.0355, 129.6755),
    ("cairo", "Cairo", 30.0444, 31.2357),
    ("lima", "Lima", -12.0464, -77.0428),
    ("singapore", "Singapore", 1.3521, 103.8198),
    ("ulaanbaatar", "Ulaanbaatar", 47.8864, 106.9057),
    ("cape-town", "Cape Town", -33.9249, 18.4241),
    ("quito", "Quito", -0.1807, -78.4678),
    ("nuuk", "Nuuk", 64.1814, -51.6941),
    ("longyearbyen", "Longyearbyen", 78.2232, 15.6267),
];

fn main() -> Res<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("normals") if args.len() == 2 => normals(Path::new(&args[1])),
        Some("table") if args.len() >= 4 => table(
            Path::new(&args[1]),
            Path::new(&args[2]),
            Path::new(&args[3]),
            args.get(4).map(Path::new),
        ),
        _ => Err("usage: validation normals <dir> | table <grid> <normals.json> <expected.json> [cmip6.json]".into()),
    }
}

fn read_json(path: &Path) -> Res<Value> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

/// Monthly normals of a daily series: mean daily min, mean daily max,
/// monthly precipitation total, averaged over the years in `[y0, y1]`.
/// A month counts when at least 90 % of its days have all three values.
fn monthly_normals(daily: &Value, y0: i32, y1: i32) -> Res<[[f64; 12]; 3]> {
    let arr = |k: &str| -> Res<&Vec<Value>> {
        daily
            .get(k)
            .and_then(Value::as_array)
            .ok_or_else(|| format!("no daily.{k}").into())
    };
    let (time, tmin, tmax, pr) = (
        arr("time")?,
        arr("temperature_2m_min")?,
        arr("temperature_2m_max")?,
        arr("precipitation_sum")?,
    );
    // (year, month) -> [sum tmin, sum tmax, sum p, valid days, days]
    let mut ym = std::collections::BTreeMap::<(i32, usize), [f64; 5]>::new();
    for i in 0..time.len() {
        let t = time[i].as_str().ok_or("bad time")?;
        let y: i32 = t.get(0..4).ok_or("bad time")?.parse()?;
        let m: usize = t.get(5..7).ok_or("bad time")?.parse()?;
        if y < y0 || y > y1 || !(1..=12).contains(&m) {
            continue;
        }
        let e = ym.entry((y, m - 1)).or_default();
        e[4] += 1.0;
        if let (Some(a), Some(b), Some(c)) = (tmin[i].as_f64(), tmax[i].as_f64(), pr[i].as_f64()) {
            e[0] += a;
            e[1] += b;
            e[2] += c;
            e[3] += 1.0;
        }
    }
    let mut acc = [[0f64; 12]; 3];
    let mut n = [0f64; 12];
    for ((_, m), e) in &ym {
        if e[3] < 0.9 * e[4] {
            continue;
        }
        acc[0][*m] += e[0] / e[3];
        acc[1][*m] += e[1] / e[3];
        acc[2][*m] += e[2] / e[3] * e[4]; // scale to the full month
        n[*m] += 1.0;
    }
    for m in 0..12 {
        if n[m] < 0.9 * f64::from(y1 - y0 + 1) {
            return Err(format!("month {} has {} valid years", m + 1, n[m]).into());
        }
        for a in &mut acc {
            a[m] /= n[m];
        }
    }
    Ok(acc)
}

fn r2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn normals(dir: &Path) -> Res<()> {
    let proto = read_json(&dir.join("era5/prototype-12-cities.json"))?;
    let proto = proto.as_array().ok_or("prototype file is not a list")?;
    let mut out = Vec::new();
    for (k, (slug, name, lat, lon)) in CITIES.iter().enumerate() {
        let resp = match proto.get(k) {
            Some(v) => v.clone(),
            None => read_json(&dir.join(format!("era5/{slug}.json")))?,
        };
        let daily = resp
            .get("daily")
            .ok_or_else(|| format!("{slug}: no daily data"))?;
        let [tmin, tmax, precip] =
            monthly_normals(daily, 1991, 2020).map_err(|e| format!("{slug}: {e}"))?;
        let r = |a: &[f64; 12]| a.iter().map(|&x| r2(x)).collect::<Vec<_>>();
        out.push(json!({
            "slug": slug, "name": name, "lat": lat, "lon": lon,
            "era5_lat": resp.get("latitude"), "era5_lon": resp.get("longitude"),
            "era5_elevation_m": resp.get("elevation"),
            "tmin": r(&tmin), "tmax": r(&tmax), "precip": r(&precip),
        }));
    }
    let doc = json!({
        "what": "ERA5 1991-2020 monthly normals from the Open-Meteo archive API (daily temperature_2m_min/max and precipitation_sum, default model, GMT days); tmin/tmax are mean daily extremes, precip the monthly total in mm",
        "source": "https://archive-api.open-meteo.com/v1/archive (Open-Meteo, CC BY 4.0; ERA5: Hersbach et al. 2020, Copernicus Climate Change Service)",
        "cities": out,
    });
    println!("{}", serde_json::to_string_pretty(&doc)?);
    Ok(())
}

/// Derived ERA5 values of a city from its normals.
pub struct Era5 {
    tmin_cold: f64,
    tmax_warm: f64,
    tmean_cold: f64,
    tmean_warm: f64,
    precip: f64,
    aridity: f64,
    dry_months: u8,
    koppen: String,
}

fn f64s(v: &Value, k: &str) -> Res<[f64; 12]> {
    let a = v
        .get(k)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("no {k}"))?;
    let mut out = [0f64; 12];
    for (o, x) in out.iter_mut().zip(a) {
        *o = x.as_f64().ok_or("not a number")?;
    }
    Ok(out)
}

fn era5_of(city: &Value) -> Res<Era5> {
    let (tmin, tmax, p) = (
        f64s(city, "tmin")?,
        f64s(city, "tmax")?,
        f64s(city, "precip")?,
    );
    let lat = city.get("lat").and_then(Value::as_f64).ok_or("no lat")?;
    let mut t = [0f32; 12];
    let mut pf = [0f32; 12];
    for m in 0..12 {
        t[m] = ((tmin[m] + tmax[m]) / 2.0) as f32;
        pf[m] = p[m] as f32;
    }
    let s = MonthlySummary::new(&t, &pf, lat).ok_or("no summary")?;
    let fold = |a: &[f64; 12], f: fn(f64, f64) -> f64, init: f64| a.iter().copied().fold(init, f);
    Ok(Era5 {
        tmin_cold: fold(&tmin, f64::min, f64::INFINITY),
        tmax_warm: fold(&tmax, f64::max, f64::NEG_INFINITY),
        tmean_cold: f64::from(s.tmean_cold),
        tmean_warm: f64::from(s.tmean_warm),
        precip: f64::from(s.precip),
        aridity: f64::from(s.aridity),
        dry_months: s.dry_months,
        koppen: s
            .koppen_of_means
            .map_or_else(String::new, |c| koppen::symbol(c).to_string()),
    })
}

fn table(
    grid_dir: &Path,
    normals_path: &Path,
    expected_path: &Path,
    cmip6: Option<&Path>,
) -> Res<()> {
    let mut grid = Grid::open(grid_dir)?;
    let doc = read_json(normals_path)?;
    let cities = doc
        .get("cities")
        .and_then(Value::as_array)
        .ok_or("no cities")?;
    println!(
        "| City | Köppen grid / ERA5 | tmin_cold | tmax_warm | tmean_cold | tmean_warm | precip mm | aridity | dry months | misses |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|");
    let mut expected = Vec::new();
    let mut lookups = Vec::new();
    for c in cities {
        let slug = c.get("slug").and_then(Value::as_str).ok_or("no slug")?;
        let name = c.get("name").and_then(Value::as_str).unwrap_or(slug);
        let lat = c.get("lat").and_then(Value::as_f64).ok_or("no lat")?;
        let lon = c.get("lon").and_then(Value::as_f64).ok_or("no lon")?;
        let e = era5_of(c)?;
        let Some(found) = grid.lookup(lat, lon, 2)? else {
            println!("| {name} | no data | | | | | | | | all |");
            expected.push(json!({ "slug": slug, "lookup": null, "misses": ["no_data"] }));
            continue;
        };
        let g = found.periods.first().ok_or("no periods")?;
        let mut misses = Vec::new();
        let mut tcell = |id: &str, grid_v: f64, era: f64| {
            let d = grid_v - era;
            let miss = d.abs() > TOL_T;
            if miss {
                misses.push(id.to_string());
            }
            format!(
                "{grid_v:.1} / {era:.1} ({d:+.1}){}",
                if miss { " **miss**" } else { "" }
            )
        };
        let cols = [
            tcell("tmin_cold", g.tmin_cold_c, r2(e.tmin_cold)),
            tcell("tmax_warm", g.tmax_warm_c, r2(e.tmax_warm)),
            tcell("tmean_cold", g.tmean_cold_c, r2(e.tmean_cold)),
            tcell("tmean_warm", g.tmean_warm_c, r2(e.tmean_warm)),
        ];
        let era_p = r2(e.precip);
        let rel = (g.precip_mm - era_p) / era_p;
        let pmiss = rel.abs() > TOL_P;
        if pmiss {
            misses.push("precip".to_string());
        }
        println!(
            "| {name} | {} / {} | {} | {} | {} | {} | {:.0} / {:.0} ({:+.0}%){} | {:.2} / {:.2} | {} / {} | {} |",
            g.koppen,
            e.koppen,
            cols[0],
            cols[1],
            cols[2],
            cols[3],
            g.precip_mm,
            e.precip,
            100.0 * rel,
            if pmiss { " **miss**" } else { "" },
            g.aridity_index,
            e.aridity,
            g.dry_months,
            e.dry_months,
            if misses.is_empty() {
                "-".to_string()
            } else {
                misses.join(", ")
            }
        );
        expected.push(json!({
            "slug": slug,
            "era5": {
                "koppen": e.koppen, "tmin_cold_c": r2(e.tmin_cold), "tmax_warm_c": r2(e.tmax_warm),
                "tmean_cold_c": r2(e.tmean_cold), "tmean_warm_c": r2(e.tmean_warm),
                "precip_mm": r2(e.precip), "aridity_index": r2(e.aridity), "dry_months": e.dry_months,
            },
            "misses": misses,
            "lookup": serde_json::to_value(&found)?,
        }));
        lookups.push((slug.to_string(), found));
    }
    let doc = json!({
        "what": "Grid lookups (radius 2) for the validation cities and the tolerance misses against ERA5 1991-2020 normals; regenerate with the validation example after a deliberate grid change",
        "tolerance_temperature_c": TOL_T,
        "tolerance_precip_fraction": TOL_P,
        "grid_builder_version": grid.manifest().builder_version,
        "cities": expected,
    });
    std::fs::write(expected_path, serde_json::to_string_pretty(&doc)? + "\n")?;
    if let Some(p) = cmip6 {
        cmip6_table(p, &lookups)?;
    }
    Ok(())
}

/// Change of the coldest/warmest monthly mean in one CMIP6 model (Open-Meteo
/// climate API) between 1991-2020 and 2021-2050, next to the grid's
/// 2041-2070 changes (indicative only: different periods and models).
fn cmip6_table(path: &Path, lookups: &[(String, canopi_climate::lookup::Lookup)]) -> Res<()> {
    let model = read_json(path)?;
    let model = model.as_array().ok_or("cmip6 file is not a list")?;
    println!();
    println!(
        "| City | EC_Earth3P_HR 2021-2050 minus 1991-2020: coldest / warmest month (°C) | grid 2041-2070 SSP2-4.5 | grid 2041-2070 SSP5-8.5 |"
    );
    println!("|---|---|---|---|");
    for (k, resp) in model.iter().enumerate() {
        let Some((slug, found)) = lookups.get(k) else {
            break;
        };
        let daily = resp.get("daily").ok_or("no daily")?;
        let a = monthly_normals(daily, 1991, 2020)?;
        let b = monthly_normals(daily, 2021, 2050)?;
        let means = |n: &[[f64; 12]; 3]| -> (f64, f64) {
            let t: Vec<f64> = (0..12).map(|m| f64::midpoint(n[0][m], n[1][m])).collect();
            (
                t.iter().copied().fold(f64::INFINITY, f64::min),
                t.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            )
        };
        let ((ac, aw), (bc, bw)) = (means(&a), means(&b));
        let delta = |sc: &str| -> String {
            let now = found.periods.first();
            let fut = found
                .periods
                .iter()
                .find(|p| p.period == "2041-2070" && p.scenario == sc);
            match (now, fut) {
                (Some(n), Some(f)) => format!(
                    "{:+.1} / {:+.1}",
                    f.tmean_cold_c - n.tmean_cold_c,
                    f.tmean_warm_c - n.tmean_warm_c
                ),
                _ => "-".into(),
            }
        };
        println!(
            "| {slug} | {:+.1} / {:+.1} | {} | {} |",
            bc - ac,
            bw - aw,
            delta("ssp245"),
            delta("ssp585")
        );
    }
    Ok(())
}
