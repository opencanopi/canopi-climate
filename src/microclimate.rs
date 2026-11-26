//! Zero-byte topoclimatic microclimate adjustment engine (`microclimate::adjust`).
//!
//! Adjusts a 0.1-degree grid climate lookup to a specific site's elevation,
//! slope, compass aspect, and topographic position:
//!
//! 1. **Elevation lapse rate** (`-0.0065 °C/m`), clamped to
//!    `±max(cell_relief_m / 2.0, 150.0)` with a hard cap of `±1500.0 m`.
//! 2. **Katabatic cold-air pooling** on nighttime minimums only (`tmin_extreme`,
//!    `tmin_p10`, `tmin_cold`, monthly `tmin[12]`, and `frost_calendar`; never
//!    daytime `tmax`).
//! 3. **Tilted-surface monthly solar irradiance ratio** via 48-step ENU vector
//!    integration with 25% isotropic diffuse fraction and sky-view factor
//!    `(1 + cos β) / 2` (exact in NH, SH, E/W aspects, and polar latitudes).
//!
//! Method: `docs/decisions/0008-soil-plus-terrain-microclimate-packs.md`.

use std::f64::consts::PI;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::derived::{MID_MONTH_DOY, chill_months, gdd_base_10};
use crate::extremes::usda_zone;

/// Environmental temperature lapse rate (°C per metre of elevation gain).
pub const LAPSE_RATE_C_PER_M: f64 = -0.0065;

/// Minimum symmetric elevation clamp half-width (m) when cell relief is small.
pub const MIN_ELEV_CLAMP_M: f64 = 150.0;

/// Absolute hard cap on elevation delta (m).
pub const MAX_ELEV_CLAMP_M: f64 = 1500.0;

/// Maximum slope angle accepted (degrees); steeper slopes are clamped.
pub const MAX_SLOPE_DEG: f64 = 60.0;

/// Isotropic diffuse sky radiation fraction used in tilted solar ratio.
pub const DIFFUSE_FRACTION: f64 = 0.25;

/// Frost-date shift per °C of nighttime temperature change (days / °C).
pub const FROST_DAYS_PER_C: f64 = 3.5;

/// Errors returned by [`adjust`].
#[derive(Debug, Clone, PartialEq, Error)]
pub enum MicroclimateError {
    /// Non-finite or out-of-range coordinate or site parameter.
    #[error("invalid microclimate input: {0}")]
    InvalidInput(String),
}

/// Topographic slope position governing nighttime katabatic cold-air drainage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TopoPosition {
    /// Exposed ridge or hilltop (cold air drains away).
    Ridge,
    /// Upper slope.
    UpperSlope,
    /// Mid-slope (neutral default).
    #[default]
    MidSlope,
    /// Lower slope / toe-slope.
    LowerSlope,
    /// Valley bottom.
    ValleyBottom,
    /// Enclosed frost hollow / cold-air sink.
    FrostHollow,
}

impl TopoPosition {
    /// Nighttime extreme minimum offset (`°C`) applied to `tmin_extreme` and `tmin_p10`.
    #[must_use]
    pub const fn extreme_tmin_offset_c(self) -> f64 {
        match self {
            Self::Ridge => 1.0,
            Self::UpperSlope => 0.5,
            Self::MidSlope => 0.0,
            Self::LowerSlope => -1.5,
            Self::ValleyBottom => -3.0,
            Self::FrostHollow => -5.0,
        }
    }

    /// Monthly mean daily minimum offset (`°C`) applied to `tmin_cold` and monthly `tmin[12]`.
    #[must_use]
    pub const fn monthly_tmin_offset_c(self) -> f64 {
        match self {
            Self::Ridge => 0.5,
            Self::UpperSlope => 0.25,
            Self::MidSlope => 0.0,
            Self::LowerSlope => -0.8,
            Self::ValleyBottom => -1.5,
            Self::FrostHollow => -2.5,
        }
    }
}

/// Optional terrain context of the enclosing 0.1-degree grid cell.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CellTerrain {
    /// Cell mean elevation (m).
    pub elev_m: f64,
    /// Cell elevation relief `p95 - p5` (m).
    pub relief_m: f64,
}

/// Normalize frost DOY on deserialization: accepts `null` / omitted, legacy
/// sentinels `0` (frost-free) and `366` (year-round), or `1..=365`.
fn de_frost_doy<'de, D>(de: D) -> Result<Option<u16>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Option<u16> = serde::Deserialize::deserialize(de)?;
    Ok(raw.filter(|&d| (1..=365).contains(&d)))
}

/// Optional frost-calendar inputs for microclimate adjustment.
///
/// Compatible with both [`crate::frost_calendar::FrostCalendarLookup`] (`Option<u16>`,
/// where `None` / JSON `null` indicates frost-free or year-round frost) and legacy
/// sentinel encodings (`0` = frost-free, `366` = year-round, normalized to `None` on
/// deserialization).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrostDates {
    /// Median last spring frost DOY (`Some(1..=365)`, or `None` when frost-free / year-round).
    #[serde(default, deserialize_with = "de_frost_doy")]
    pub last_spring_frost_doy: Option<u16>,
    /// 90th-percentile last spring frost DOY (`Some(1..=365)`, or `None`).
    #[serde(default, deserialize_with = "de_frost_doy")]
    pub last_spring_frost_p90_doy: Option<u16>,
    /// Median first autumn frost DOY (`Some(1..=365)`, or `None`).
    #[serde(default, deserialize_with = "de_frost_doy")]
    pub first_autumn_frost_doy: Option<u16>,
    /// Median frost-free season length (`0..=365`).
    pub frost_free_days: u16,
}

/// Site climate values to adjust (gracefully degrades when optional layers are absent).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SiteClimate {
    /// Site latitude in degrees (`-90..=90`).
    pub lat: f64,
    /// Mean temperature of the coldest month (°C).
    pub tmean_cold_c: f64,
    /// Mean temperature of the warmest month (°C).
    pub tmean_warm_c: f64,
    /// Mean daily minimum of the coldest month (°C).
    pub tmin_cold_c: f64,
    /// Mean daily maximum of the warmest month (°C).
    pub tmax_warm_c: f64,
    /// Optional annual coldest night (°C).
    #[serde(default)]
    pub tmin_extreme_c: Option<f64>,
    /// Optional annual hottest day (°C).
    #[serde(default)]
    pub tmax_extreme_c: Option<f64>,
    /// Optional 1-in-10-year coldest-night offset (`<= 0`, °C).
    #[serde(default)]
    pub tmin_p10_gap_c: Option<f64>,
    /// Optional 1-in-10-year hottest-day offset (`>= 0`, °C).
    #[serde(default)]
    pub tmax_p90_gap_c: Option<f64>,
    /// Optional monthly mean temperatures (°C, Jan–Dec).
    #[serde(default)]
    pub monthly_tmean_c: Option<[f64; 12]>,
    /// Optional monthly mean daily minimum temperatures (°C, Jan–Dec).
    #[serde(default)]
    pub monthly_tmin_c: Option<[f64; 12]>,
    /// Optional frost calendar dates.
    #[serde(default)]
    pub frost: Option<FrostDates>,
    /// Optional 5-band Fishman Dynamic Chill Portions curve `[0, +1, +2, +3, +4 °C]`.
    #[serde(default)]
    pub chill_portions_curve: Option<[f64; 5]>,
    /// Optional future winter warming delta (°C) already applied before microclimate.
    #[serde(default)]
    pub base_winter_delta_t_c: Option<f64>,
    /// Optional annual Climatic Water Deficit (mm/yr).
    #[serde(default)]
    pub cwd_mm: Option<f64>,
    /// Optional cell terrain (required for elevation lapse rate).
    #[serde(default)]
    pub cell_terrain: Option<CellTerrain>,
}

/// Site-specific topography passed to [`adjust`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct MicroclimateInput {
    /// Site elevation in metres (`None` skips lapse-rate adjustment).
    #[serde(default)]
    pub site_elev_m: Option<f64>,
    /// Slope angle in degrees (`0..=60`, clamped).
    #[serde(default)]
    pub slope_deg: f64,
    /// Compass aspect in degrees clockwise from north (`0=N, 90=E, 180=S, 270=W`).
    #[serde(default)]
    pub aspect_deg: f64,
    /// Topographic position for nighttime cold-air pooling.
    #[serde(default)]
    pub topo_position: TopoPosition,
}

/// Tilted-surface solar irradiance ratio relative to a horizontal surface.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SolarRatio {
    /// Monthly irradiance ratios (Jan–Dec).
    pub monthly: [f64; 12],
    /// Annual weighted irradiance ratio.
    pub annual: f64,
    /// Warm-season (Apr–Sep in NH, Oct–Mar in SH) irradiance ratio.
    pub growing_season: f64,
}

/// Adjusted microclimate output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MicroclimateAdjustment {
    /// Requested `site_elev_m - cell_elev_m` (0 when elevation not supplied).
    pub requested_delta_elev_m: f64,
    /// Effective clamped elevation delta (m).
    pub effective_delta_elev_m: f64,
    /// Whether the elevation delta was clamped by `±max(relief/2, 150)` or `±1500 m`.
    pub elev_clamped: bool,
    /// Temperature shift from elevation lapse rate (°C).
    pub delta_t_lapse_c: f64,
    /// Nighttime extreme temperature shift from cold-air pooling (°C).
    pub delta_t_pool_extreme_c: f64,
    /// Nighttime monthly `tmin` shift from cold-air pooling (°C).
    pub delta_t_pool_monthly_c: f64,
    /// Adjusted `tmean_cold_c` (°C).
    pub tmean_cold_c: f64,
    /// Adjusted `tmean_warm_c` (°C).
    pub tmean_warm_c: f64,
    /// Adjusted `tmin_cold_c` (°C).
    pub tmin_cold_c: f64,
    /// Adjusted `tmax_warm_c` (°C).
    pub tmax_warm_c: f64,
    /// Adjusted `tmin_extreme_c` (°C), if supplied.
    pub tmin_extreme_c: Option<f64>,
    /// Adjusted `tmax_extreme_c` (°C), if supplied.
    pub tmax_extreme_c: Option<f64>,
    /// Adjusted 1-in-10-year coldest night (`tmin_extreme + tmin_p10_gap`), if supplied.
    pub tmin_p10_c: Option<f64>,
    /// Adjusted 1-in-10-year hottest day (`tmax_extreme + tmax_p90_gap`), if supplied.
    pub tmax_p90_c: Option<f64>,
    /// Re-derived USDA hardiness zone from adjusted `tmin_extreme_c`.
    pub usda_zone: Option<String>,
    /// Adjusted monthly `tmean` (°C), if supplied.
    pub monthly_tmean_c: Option<[f64; 12]>,
    /// Adjusted monthly `tmin` (°C, clamped `<= monthly_tmean`), if supplied.
    pub monthly_tmin_c: Option<[f64; 12]>,
    /// Re-derived Growing Degree Days (base 10 °C), if monthly `tmean` supplied.
    pub gdd_base_10: Option<f64>,
    /// Re-derived chill months (`0.0 < tmean <= 7.2 °C`), if monthly `tmean` supplied.
    pub chill_months: Option<u8>,
    /// Re-derived Dynamic Chill Portions from the 5-band sensitivity curve, if supplied.
    pub chill_portions: Option<f64>,
    /// Adjusted frost dates, if supplied.
    pub frost: Option<FrostDates>,
    /// Tilted-surface solar irradiance ratio (48-step ENU integration).
    pub solar_ratio: SolarRatio,
    /// Aspect/slope-adjusted annual CWD (`cwd_mm * growing_season.clamp(0.7, 1.3)`), if supplied.
    pub cwd_adjusted_mm: Option<f64>,
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// Project Chill Portions from a 5-band `[0, +1, +2, +3, +4 °C]` curve at arbitrary `delta_t_c`.
fn project_chill_curve(curve: [f64; 5], delta_t_c: f64) -> f64 {
    if !delta_t_c.is_finite() {
        return curve[0].clamp(0.0, 250.0);
    }
    if delta_t_c <= 0.0 {
        // Cooling below baseline: extrapolate gently with [0, +1] slope, capped to [0, 250] CP.
        let slope = curve[1] - curve[0];
        return (curve[0] + slope * delta_t_c).clamp(0.0, 250.0);
    }
    if delta_t_c >= 4.0 {
        let slope = curve[4] - curve[3];
        return (curve[4] + slope * (delta_t_c - 4.0)).clamp(0.0, 250.0);
    }
    let idx = delta_t_c.floor() as usize;
    let frac = delta_t_c - idx as f64;
    let a = curve[idx];
    let b = curve[idx + 1];
    (a + (b - a) * frac).clamp(0.0, 250.0)
}

/// Shift an optional frost DOY by `delta_days` (`1..=365` wraps cyclically; `None` and
/// legacy `0`/`366` sentinels normalize to `None`).
fn shift_doy(doy: Option<u16>, delta_days: i32) -> Option<u16> {
    let d = doy.filter(|&v| (1..=365).contains(&v))?;
    let shifted = (i32::from(d) - 1 + delta_days).rem_euclid(365) + 1;
    u16::try_from(shifted).ok()
}

fn adjust_frost(frost: FrostDates, delta_t_night_c: f64) -> FrostDates {
    // Already year-round frost (0) or fully frost-free (365 with no frost DOYs): preserve regime.
    let has_doy = frost
        .last_spring_frost_doy
        .is_some_and(|d| (1..=365).contains(&d))
        || frost
            .last_spring_frost_p90_doy
            .is_some_and(|d| (1..=365).contains(&d))
        || frost
            .first_autumn_frost_doy
            .is_some_and(|d| (1..=365).contains(&d));
    if frost.frost_free_days == 0 || (frost.frost_free_days >= 365 && !has_doy) {
        return FrostDates {
            last_spring_frost_doy: None,
            last_spring_frost_p90_doy: None,
            first_autumn_frost_doy: None,
            frost_free_days: frost.frost_free_days.min(365),
        };
    }

    // Warmer nights (delta_t > 0) -> earlier last spring frost (-shift), later first autumn frost (+shift).
    let shift = (delta_t_night_c * FROST_DAYS_PER_C).round() as i32;
    let adjusted_free = (i32::from(frost.frost_free_days) + 2 * shift).clamp(0, 365);
    let free = u16::try_from(adjusted_free).unwrap_or(frost.frost_free_days.min(365));

    // When cooling collapses the frost-free season to 0 (year-round frost) or warming
    // eliminates frost (365 days), emit `None` instead of crossing/wrapping DOYs.
    if free == 0 || free >= 365 {
        return FrostDates {
            last_spring_frost_doy: None,
            last_spring_frost_p90_doy: None,
            first_autumn_frost_doy: None,
            frost_free_days: free,
        };
    }

    FrostDates {
        last_spring_frost_doy: shift_doy(frost.last_spring_frost_doy, -shift),
        last_spring_frost_p90_doy: shift_doy(frost.last_spring_frost_p90_doy, -shift),
        first_autumn_frost_doy: shift_doy(frost.first_autumn_frost_doy, shift),
        frost_free_days: free,
    }
}

/// Compute tilted-surface solar irradiance ratio via 48-step (30-min) ENU vector
/// integration with isotropic diffuse fraction [`DIFFUSE_FRACTION`] (`0.25`).
///
/// Safe across Northern Hemisphere, Southern Hemisphere, E/W aspects, and polar
/// latitudes (polar night returns `1.0` with no NaN/Inf).
#[must_use]
pub fn solar_irradiance_ratio(lat_deg: f64, slope_deg: f64, aspect_deg: f64) -> SolarRatio {
    let phi = lat_deg.clamp(-90.0, 90.0).to_radians();
    let beta = slope_deg.clamp(0.0, MAX_SLOPE_DEG).to_radians();
    let aspect = aspect_deg.rem_euclid(360.0).to_radians();

    let (sin_phi, cos_phi) = phi.sin_cos();
    let (sin_b, cos_b) = beta.sin_cos();
    let (sin_a, cos_a) = aspect.sin_cos();

    // Slope surface normal in local East-North-Up (ENU) coordinates.
    let n_e = sin_b * sin_a;
    let n_n = sin_b * cos_a;
    let n_u = cos_b;
    let svf = 0.5 * (1.0 + cos_b);

    let mut monthly = [1.0; 12];
    let mut sum_tilted_all = 0.0;
    let mut sum_horiz_all = 0.0;
    let mut sum_tilted_grow = 0.0;
    let mut sum_horiz_grow = 0.0;

    let steps = 48usize;
    let d_omega = 2.0 * PI / steps as f64;

    for (m, &doy) in MID_MONTH_DOY.iter().enumerate() {
        let delta = 0.409 * (2.0 * PI * doy / 365.0 - 1.39).sin();
        let (sin_d, cos_d) = delta.sin_cos();

        let mut i_tilted = 0.0;
        let mut i_horiz = 0.0;
        for step in 0..steps {
            let omega = -PI + (step as f64 + 0.5) * d_omega;
            let (sin_w, cos_w) = omega.sin_cos();
            let s_e = -cos_d * sin_w;
            let s_n = sin_d * cos_phi - cos_d * sin_phi * cos_w;
            let s_u = sin_d * sin_phi + cos_d * cos_phi * cos_w;
            if s_u > 0.0 {
                let cos_theta = (s_e * n_e + s_n * n_n + s_u * n_u).max(0.0);
                i_tilted += (1.0 - DIFFUSE_FRACTION) * cos_theta + DIFFUSE_FRACTION * s_u * svf;
                i_horiz += s_u;
            }
        }
        monthly[m] = if i_horiz < 1e-6 {
            1.0
        } else {
            round3(i_tilted / i_horiz)
        };
        sum_tilted_all += i_tilted;
        sum_horiz_all += i_horiz;

        let is_growing = if lat_deg >= 0.0 {
            (3..=8).contains(&m) // Apr-Sep in NH
        } else {
            !(3..=8).contains(&m) // Oct-Mar in SH
        };
        if is_growing {
            sum_tilted_grow += i_tilted;
            sum_horiz_grow += i_horiz;
        }
    }

    let annual = if sum_horiz_all < 1e-6 {
        1.0
    } else {
        round3(sum_tilted_all / sum_horiz_all)
    };
    let growing_season = if sum_horiz_grow < 1e-6 {
        annual
    } else {
        round3(sum_tilted_grow / sum_horiz_grow)
    };

    SolarRatio {
        monthly,
        annual,
        growing_season,
    }
}

fn check_finite(val: f64, name: &str) -> Result<(), MicroclimateError> {
    if !val.is_finite() {
        return Err(MicroclimateError::InvalidInput(format!(
            "{name} must be finite"
        )));
    }
    Ok(())
}

fn validate_inputs(site: &SiteClimate, input: &MicroclimateInput) -> Result<(), MicroclimateError> {
    if !site.lat.is_finite() || !(-90.0..=90.0).contains(&site.lat) {
        return Err(MicroclimateError::InvalidInput(
            "lat must be finite and in [-90, 90]".into(),
        ));
    }
    check_finite(site.tmean_cold_c, "tmean_cold_c")?;
    check_finite(site.tmean_warm_c, "tmean_warm_c")?;
    check_finite(site.tmin_cold_c, "tmin_cold_c")?;
    check_finite(site.tmax_warm_c, "tmax_warm_c")?;
    if let Some(v) = site.tmin_extreme_c {
        check_finite(v, "tmin_extreme_c")?;
    }
    if let Some(v) = site.tmax_extreme_c {
        check_finite(v, "tmax_extreme_c")?;
    }
    if let Some(v) = site.tmin_p10_gap_c {
        check_finite(v, "tmin_p10_gap_c")?;
    }
    if let Some(v) = site.tmax_p90_gap_c {
        check_finite(v, "tmax_p90_gap_c")?;
    }
    if let Some(arr) = site.monthly_tmean_c {
        for v in arr {
            check_finite(v, "monthly_tmean_c")?;
        }
    }
    if let Some(arr) = site.monthly_tmin_c {
        for v in arr {
            check_finite(v, "monthly_tmin_c")?;
        }
    }
    if let Some(arr) = site.chill_portions_curve {
        for v in arr {
            check_finite(v, "chill_portions_curve")?;
        }
    }
    if let Some(v) = site.base_winter_delta_t_c {
        check_finite(v, "base_winter_delta_t_c")?;
    }
    if let Some(v) = site.cwd_mm {
        check_finite(v, "cwd_mm")?;
    }
    if let Some(ct) = site.cell_terrain {
        check_finite(ct.elev_m, "cell_terrain.elev_m")?;
        check_finite(ct.relief_m, "cell_terrain.relief_m")?;
    }
    if !input.slope_deg.is_finite() || input.slope_deg < 0.0 {
        return Err(MicroclimateError::InvalidInput(
            "slope_deg must be finite and >= 0".into(),
        ));
    }
    check_finite(input.aspect_deg, "aspect_deg")?;
    if let Some(e) = input.site_elev_m {
        check_finite(e, "site_elev_m")?;
    }
    Ok(())
}

fn clamp_elevation_delta(
    site_elev_m: Option<f64>,
    cell_terrain: Option<CellTerrain>,
) -> (f64, f64, bool) {
    match (site_elev_m, cell_terrain) {
        (Some(site_e), Some(cell_t))
            if cell_t.elev_m.is_finite() && cell_t.relief_m.is_finite() =>
        {
            let req = site_e - cell_t.elev_m;
            let limit = (cell_t.relief_m.max(0.0) / 2.0).clamp(MIN_ELEV_CLAMP_M, MAX_ELEV_CLAMP_M);
            let eff = req.clamp(-limit, limit);
            (round2(req), round2(eff), (eff - req).abs() > 1e-6)
        }
        _ => (0.0, 0.0, false),
    }
}

/// Adjust a 0.1-degree grid climate lookup to site topography.
///
/// # Errors
/// [`MicroclimateError::InvalidInput`] if latitude or any supplied numeric
/// parameter is non-finite or out of range.
pub fn adjust(
    site: &SiteClimate,
    input: &MicroclimateInput,
) -> Result<MicroclimateAdjustment, MicroclimateError> {
    validate_inputs(site, input)?;
    let (requested_delta_elev_m, effective_delta_elev_m, elev_clamped) =
        clamp_elevation_delta(input.site_elev_m, site.cell_terrain);

    let delta_t_lapse = LAPSE_RATE_C_PER_M * effective_delta_elev_m;
    let pool_ext = input.topo_position.extreme_tmin_offset_c();
    let pool_mon = input.topo_position.monthly_tmin_offset_c();

    let tmean_cold = round2(site.tmean_cold_c + delta_t_lapse);
    let tmean_warm = round2(site.tmean_warm_c + delta_t_lapse);
    let tmin_cold = round2((site.tmin_cold_c + delta_t_lapse + pool_mon).min(tmean_cold));
    // Daytime maximums receive elevation lapse only (never cold-air pooling).
    let tmax_warm = round2(site.tmax_warm_c + delta_t_lapse);

    let tmin_extreme = site
        .tmin_extreme_c
        .map(|t| round2(t + delta_t_lapse + pool_ext));
    let tmax_extreme = site.tmax_extreme_c.map(|t| round2(t + delta_t_lapse));
    let tmin_p10 = match (tmin_extreme, site.tmin_p10_gap_c) {
        (Some(t), Some(gap)) => Some(round2(t + gap)),
        _ => None,
    };
    let tmax_p90 = match (tmax_extreme, site.tmax_p90_gap_c) {
        (Some(t), Some(gap)) => Some(round2(t + gap)),
        _ => None,
    };
    let usda = tmin_extreme.map(usda_zone);

    let monthly_tmean = site
        .monthly_tmean_c
        .map(|arr| arr.map(|t| round2(t + delta_t_lapse)));
    let monthly_tmin = match (site.monthly_tmin_c, monthly_tmean) {
        (Some(arr), Some(tm)) => {
            let mut out = [0.0; 12];
            for i in 0..12 {
                out[i] = round2((arr[i] + delta_t_lapse + pool_mon).min(tm[i]));
            }
            Some(out)
        }
        (Some(arr), None) => Some(arr.map(|t| round2(t + delta_t_lapse + pool_mon))),
        _ => None,
    };

    let gdd = monthly_tmean.and_then(|tm| gdd_base_10(&tm).map(round2));
    let chill_m = monthly_tmean.and_then(|tm| chill_months(&tm));
    let chill_p = site.chill_portions_curve.map(|curve| {
        let dt = site.base_winter_delta_t_c.unwrap_or(0.0) + delta_t_lapse;
        (project_chill_curve(curve, dt) * 10.0).round() / 10.0
    });

    let frost = site
        .frost
        .map(|f| adjust_frost(f, delta_t_lapse + pool_mon));

    let solar = solar_irradiance_ratio(site.lat, input.slope_deg, input.aspect_deg);
    let cwd_adjusted = site
        .cwd_mm
        .map(|cwd| (cwd * solar.growing_season.clamp(0.7, 1.3)).round());

    Ok(MicroclimateAdjustment {
        requested_delta_elev_m,
        effective_delta_elev_m,
        elev_clamped,
        delta_t_lapse_c: round2(delta_t_lapse),
        delta_t_pool_extreme_c: pool_ext,
        delta_t_pool_monthly_c: pool_mon,
        tmean_cold_c: tmean_cold,
        tmean_warm_c: tmean_warm,
        tmin_cold_c: tmin_cold,
        tmax_warm_c: tmax_warm,
        tmin_extreme_c: tmin_extreme,
        tmax_extreme_c: tmax_extreme,
        tmin_p10_c: tmin_p10,
        tmax_p90_c: tmax_p90,
        usda_zone: usda,
        monthly_tmean_c: monthly_tmean,
        monthly_tmin_c: monthly_tmin,
        gdd_base_10: gdd,
        chill_months: chill_m,
        chill_portions: chill_p,
        frost,
        solar_ratio: solar,
        cwd_adjusted_mm: cwd_adjusted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paris_site() -> SiteClimate {
        SiteClimate {
            lat: 48.85,
            tmean_cold_c: 4.5,
            tmean_warm_c: 20.0,
            tmin_cold_c: 2.0,
            tmax_warm_c: 25.5,
            tmin_extreme_c: Some(-8.0),
            tmax_extreme_c: Some(36.0),
            tmin_p10_gap_c: Some(-4.0),
            tmax_p90_gap_c: Some(3.0),
            monthly_tmean_c: Some([
                4.5, 5.2, 8.2, 11.2, 15.0, 18.2, 20.0, 19.8, 16.5, 12.5, 8.0, 5.0,
            ]),
            monthly_tmin_c: Some([
                2.0, 2.3, 4.5, 6.8, 10.4, 13.5, 15.3, 15.1, 12.2, 9.0, 5.1, 2.6,
            ]),
            frost: Some(FrostDates {
                last_spring_frost_doy: Some(95),
                last_spring_frost_p90_doy: Some(112),
                first_autumn_frost_doy: Some(315),
                frost_free_days: 220,
            }),
            chill_portions_curve: Some([96.0, 88.0, 76.0, 62.0, 46.0]),
            base_winter_delta_t_c: Some(0.0),
            cwd_mm: Some(250.0),
            cell_terrain: Some(CellTerrain {
                elev_m: 60.0,
                relief_m: 60.0,
            }),
        }
    }

    #[test]
    fn solar_ratio_handles_nh_sh_and_polar_safely() {
        // NH: South-facing (180 deg) > flat (1.0) > North-facing (0 deg)
        let nh_s = solar_irradiance_ratio(45.0, 20.0, 180.0);
        let nh_n = solar_irradiance_ratio(45.0, 20.0, 0.0);
        assert!(nh_s.annual > 1.05 && nh_n.annual < 0.85);
        assert!(nh_s.growing_season > nh_n.growing_season);

        // SH: North-facing (0 deg) > flat (1.0) > South-facing (180 deg)
        let sh_n = solar_irradiance_ratio(-35.0, 20.0, 0.0);
        let sh_s = solar_irradiance_ratio(-35.0, 20.0, 180.0);
        assert!(sh_n.annual > 1.05 && sh_s.annual < 0.85);

        // Polar latitude (80N): December polar night returns 1.0, never NaN
        let polar = solar_irradiance_ratio(80.0, 25.0, 180.0);
        assert!(polar.monthly.iter().all(|v| v.is_finite() && *v > 0.0));
        assert_eq!(polar.monthly[11], 1.0);
    }

    #[test]
    fn lapse_clamping_and_cold_air_pooling_behave_physically() {
        let site = paris_site();

        // Typo elevation (4000 m in Paris with relief 60 m) clamps to +150 m
        let clamped = adjust(
            &site,
            &MicroclimateInput {
                site_elev_m: Some(4000.0),
                slope_deg: 0.0,
                aspect_deg: 180.0,
                topo_position: TopoPosition::MidSlope,
            },
        )
        .unwrap();
        assert!(clamped.elev_clamped);
        assert_eq!(clamped.effective_delta_elev_m, 150.0);
        assert!((clamped.delta_t_lapse_c - (-0.98)).abs() < 0.02);

        // Frost hollow cools nighttime minimums (-5 C extreme, -2.5 C monthly) but leaves daytime tmax untouched
        let hollow = adjust(
            &site,
            &MicroclimateInput {
                site_elev_m: Some(60.0),
                slope_deg: 0.0,
                aspect_deg: 0.0,
                topo_position: TopoPosition::FrostHollow,
            },
        )
        .unwrap();
        assert!(!hollow.elev_clamped);
        assert_eq!(hollow.tmax_warm_c, site.tmax_warm_c);
        assert_eq!(hollow.tmax_extreme_c, site.tmax_extreme_c);
        assert_eq!(hollow.tmin_extreme_c, Some(-13.0));
        assert_eq!(hollow.tmin_p10_c, Some(-17.0));
        assert!(
            hollow.frost.unwrap().last_spring_frost_doy > site.frost.unwrap().last_spring_frost_doy
        );
        assert!(hollow.frost.unwrap().frost_free_days < site.frost.unwrap().frost_free_days);

        // Ridge warms nighttime extremes by +1.0 C
        let ridge = adjust(
            &site,
            &MicroclimateInput {
                site_elev_m: Some(60.0),
                slope_deg: 15.0,
                aspect_deg: 180.0,
                topo_position: TopoPosition::Ridge,
            },
        )
        .unwrap();
        assert_eq!(ridge.tmin_extreme_c, Some(-7.0));
        assert!(ridge.cwd_adjusted_mm.unwrap() > 250.0);
    }

    #[test]
    fn frost_dates_accepts_null_and_legacy_sentinels_and_prevents_crossover() {
        // BUG-03: FrostCalendarLookup JSON (with null DOYs) deserializes directly into FrostDates.
        let frost_free_json = r#"{"last_spring_frost_doy":null,"last_spring_frost_p90_doy":null,"first_autumn_frost_doy":null,"frost_free_days":365,"regime":"frost_free"}"#;
        let ff: FrostDates = serde_json::from_str(frost_free_json).unwrap();
        assert_eq!(ff.last_spring_frost_doy, None);
        assert_eq!(ff.frost_free_days, 365);

        // Legacy sentinels (0 and 366) also deserialize cleanly to None.
        let legacy_yr: FrostDates = serde_json::from_str(
            r#"{"last_spring_frost_doy":366,"last_spring_frost_p90_doy":366,"first_autumn_frost_doy":366,"frost_free_days":0}"#,
        )
        .unwrap();
        assert_eq!(legacy_yr.last_spring_frost_doy, None);
        assert_eq!(legacy_yr.frost_free_days, 0);

        // BUG-04: Strong cooling collapsing a short season to 0 days emits None instead of crossed DOYs.
        let mut alpine = paris_site();
        alpine.frost = Some(FrostDates {
            last_spring_frost_doy: Some(170),
            last_spring_frost_p90_doy: Some(180),
            first_autumn_frost_doy: Some(185),
            frost_free_days: 15,
        });
        let cooled = adjust(
            &alpine,
            &MicroclimateInput {
                site_elev_m: Some(60.0),
                slope_deg: 0.0,
                aspect_deg: 0.0,
                topo_position: TopoPosition::FrostHollow,
            },
        )
        .unwrap();
        let cf = cooled.frost.unwrap();
        assert_eq!(cf.frost_free_days, 0);
        assert_eq!(cf.last_spring_frost_doy, None);
        assert_eq!(cf.first_autumn_frost_doy, None);

        // Strong warming pushing frost-free days to 365 emits None instead of wrapped DOYs.
        let mut mild = paris_site();
        mild.cell_terrain = Some(CellTerrain {
            elev_m: 1000.0,
            relief_m: 1000.0,
        });
        mild.frost = Some(FrostDates {
            last_spring_frost_doy: Some(15),
            last_spring_frost_p90_doy: Some(25),
            first_autumn_frost_doy: Some(355),
            frost_free_days: 350,
        });
        let warmed = adjust(
            &mild,
            &MicroclimateInput {
                site_elev_m: Some(500.0),
                slope_deg: 0.0,
                aspect_deg: 180.0,
                topo_position: TopoPosition::Ridge,
            },
        )
        .unwrap();
        let wf = warmed.frost.unwrap();
        assert_eq!(wf.frost_free_days, 365);
        assert_eq!(wf.last_spring_frost_doy, None);
        assert_eq!(wf.first_autumn_frost_doy, None);
    }

    #[test]
    fn rejects_non_finite_site_climate_and_caps_cooling_chill() {
        // BUG-06: Non-finite SiteClimate / CellTerrain fields are rejected.
        let mut bad = paris_site();
        bad.tmin_extreme_c = Some(f64::NAN);
        assert!(adjust(&bad, &MicroclimateInput::default()).is_err());

        let mut bad_terrain = paris_site();
        bad_terrain.cell_terrain = Some(CellTerrain {
            elev_m: f64::NAN,
            relief_m: 100.0,
        });
        assert!(adjust(&bad_terrain, &MicroclimateInput::default()).is_err());

        // Cooling chill extrapolation is capped at <= 250.0 CP.
        let mut chill_site = paris_site();
        chill_site.cell_terrain = Some(CellTerrain {
            elev_m: 500.0,
            relief_m: 3000.0,
        });
        chill_site.chill_portions_curve = Some([220.0, 180.0, 140.0, 100.0, 60.0]);
        let cooled = adjust(
            &chill_site,
            &MicroclimateInput {
                site_elev_m: Some(2000.0),
                slope_deg: 0.0,
                aspect_deg: 0.0,
                topo_position: TopoPosition::MidSlope,
            },
        )
        .unwrap();
        assert!(cooled.chill_portions.unwrap() <= 250.0);
    }
}
