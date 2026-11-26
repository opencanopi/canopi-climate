//! Measures derived from 12 monthly means: the per-cell summaries stored in
//! the grid. Annual extremes and USDA hardiness zones live in
//! [`crate::extremes`].

use serde::Serialize;

use crate::koppen;
use crate::model::{ARIDITY_CAP, ARIDITY_MIN_PET_MM};

/// Mid-month day of year (1-based, 365-day calendar), January first.
///
/// Pinned to the v0.7.0 values because this table feeds Thornthwaite PET
/// (`aridity`), [`photoperiod`], and [`crate::microclimate`] solar geometry;
/// changing it would alter `aridity` across existing grid tiles.
pub const MID_MONTH_DOY: [f64; 12] = [
    15., 46., 75., 105., 135., 162., 198., 228., 258., 288., 318., 344.,
];
/// Mean days per month (February averaged over leap years).
const DAYS_IN_MONTH: [f64; 12] = [31., 28.25, 31., 30., 31., 30., 31., 31., 30., 31., 30., 31.];

/// Solar obliquity (radians) used by FAO-56 eq. 24.
const MAX_DECLINATION_RAD: f64 = 0.409;

/// Day length (hours) at a latitude on a day of year (FAO-56 eq. 24, 25, 34),
/// hardened at the exact poles (`|lat_deg| >= 90°`) against `tan(±90°)` overflow.
#[must_use]
pub fn day_length_hours(lat_deg: f64, doy: f64) -> f64 {
    let declination = MAX_DECLINATION_RAD * (2.0 * std::f64::consts::PI * doy / 365.0 - 1.39).sin();
    day_length_for_declination(lat_deg, declination)
}

fn day_length_for_declination(lat_deg: f64, declination: f64) -> f64 {
    if lat_deg.abs() >= 90.0 {
        let prod = lat_deg * declination;
        return if prod > 0.0 {
            24.0
        } else if prod < 0.0 {
            0.0
        } else {
            12.0
        };
    }
    let phi = lat_deg.to_radians();
    let x = (-phi.tan() * declination.tan()).clamp(-1.0, 1.0);
    24.0 * x.acos() / std::f64::consts::PI
}

/// Annual photoperiod summary at a latitude (FAO-56 geometric daylength,
/// 0 bytes stored in the grid).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Photoperiod {
    /// Shortest day of the year (hours, analytical at `|δ| = 0.409` rad).
    pub min_hours: f64,
    /// Longest day of the year (hours, analytical at `|δ| = 0.409` rad).
    pub max_hours: f64,
    /// Seasonal day-length amplitude `max_hours - min_hours` (hours).
    pub amplitude_hours: f64,
    /// Mid-month day length for January through December (hours).
    pub monthly_hours: [f64; 12],
    /// Days per 365-day year with 0 hours of geometric daylight (`<= 0.001` h).
    pub polar_night_days: u16,
    /// Days per 365-day year with 24 hours of geometric daylight (`>= 23.999` h).
    pub midnight_sun_days: u16,
}

/// Compute the annual photoperiod summary for `lat_deg` (`-90.0..=90.0`).
/// Returns `None` for non-finite coordinates or `|lat_deg| > 90.0`.
#[must_use]
pub fn photoperiod(lat_deg: f64) -> Option<Photoperiod> {
    if !lat_deg.is_finite() || lat_deg.abs() > 90.0 {
        return None;
    }
    let round2 = |x: f64| (x * 100.0).round() / 100.0;
    let abs_lat = lat_deg.abs();
    let min_raw = day_length_for_declination(abs_lat, -MAX_DECLINATION_RAD);
    let max_raw = day_length_for_declination(abs_lat, MAX_DECLINATION_RAD);
    let min_hours = round2(min_raw);
    let max_hours = round2(max_raw);
    let amplitude_hours = round2(max_raw - min_raw);

    let mut monthly_hours = [0f64; 12];
    for (m, &doy) in MID_MONTH_DOY.iter().enumerate() {
        monthly_hours[m] = round2(day_length_hours(lat_deg, doy));
    }

    let mut polar_night_days = 0u16;
    let mut midnight_sun_days = 0u16;
    for doy in 1..=365 {
        let h = day_length_hours(lat_deg, f64::from(doy));
        if h <= 0.001 {
            polar_night_days += 1;
        } else if h >= 23.999 {
            midnight_sun_days += 1;
        }
    }

    Some(Photoperiod {
        min_hours,
        max_hours,
        amplitude_hours,
        monthly_hours,
        polar_night_days,
        midnight_sun_days,
    })
}

/// Thornthwaite (1948) annual potential evapotranspiration (mm) from 12
/// monthly mean temperatures (°C), corrected for day length and month length.
/// Months at or below 0 °C contribute nothing; above 26.5 °C the
/// Willmott et al. (1985) high-temperature formula applies.
#[must_use]
pub fn thornthwaite_pet(t: &[f32; 12], lat_deg: f64) -> f64 {
    let heat_index: f64 = t
        .iter()
        .filter(|&&x| x > 0.0)
        .map(|&x| (f64::from(x) / 5.0).powf(1.514))
        .sum();
    if heat_index <= 0.0 {
        return 0.0;
    }
    let i = heat_index;
    let a = 6.75e-7 * i.powi(3) - 7.71e-5 * i.powi(2) + 1.792e-2 * i + 0.492_39;
    let mut pet = 0.0;
    for m in 0..12 {
        let tm = f64::from(t[m]);
        if tm <= 0.0 {
            continue;
        }
        let unadjusted = if tm < 26.5 {
            16.0 * (10.0 * tm / i).powf(a)
        } else {
            -415.85 + 32.24 * tm - 0.43 * tm * tm
        };
        pet += unadjusted
            * (day_length_hours(lat_deg, MID_MONTH_DOY[m]) / 12.0)
            * (DAYS_IN_MONTH[m] / 30.0);
    }
    pet
}

/// Per-cell summary of a monthly climatology.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonthlySummary {
    /// Coldest monthly mean (°C).
    pub tmean_cold: f32,
    /// Warmest monthly mean (°C).
    pub tmean_warm: f32,
    /// Annual precipitation (mm).
    pub precip: f32,
    /// Precipitation / Thornthwaite PET, capped at [`ARIDITY_CAP`]; `None`
    /// where PET is below [`ARIDITY_MIN_PET_MM`].
    pub aridity: Option<f32>,
    /// Months with P (mm) < 2 T (°C).
    pub dry_months: u8,
    /// Köppen-Geiger class of the monthly means themselves.
    pub koppen_of_means: Option<u8>,
}

/// Gaussen dry-month bitmask (`gaussen_p_lt_2t`, docs/format.md): bit `m`
/// (`0..12`, January…December) is set when `p[m] < 2.0 * t[m]`. Bits 12–14
/// are reserved (written 0) and bit 15 is never set. Returns `None` if any
/// temperature or precipitation value is non-finite.
#[must_use]
pub fn dry_month_mask(t: &[f32; 12], p: &[f32; 12]) -> Option<i16> {
    if t.iter().chain(p.iter()).any(|x| !x.is_finite()) {
        return None;
    }
    let mut mask = 0i16;
    for m in 0..12 {
        if p[m] < 2.0 * t[m] {
            mask |= 1i16 << m;
        }
    }
    Some(mask)
}

/// Annual growing degree-days above 10 °C (°C·day), approximated from 12
/// monthly mean temperatures (°C) weighted by mean days per month
/// (`[31, 28.25, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]`):
/// `sum_m max(0, tmean[m] - 10) * days[m]`.
///
/// Returns `None` if any monthly mean is non-finite.
#[must_use]
pub fn gdd_base_10(tmean: &[f64; 12]) -> Option<f64> {
    if tmean.iter().any(|x| !x.is_finite()) {
        return None;
    }
    let sum: f64 = tmean
        .iter()
        .zip(DAYS_IN_MONTH)
        .map(|(&t, days)| (t - 10.0).max(0.0) * days)
        .sum();
    Some(sum)
}

/// Horticultural chill-month count from 12 monthly mean temperatures (°C):
/// number of months with `0.0 < tmean[m] <= 7.2` (the 0–7.2 °C / 32–45 °F
/// effective chilling band).
///
/// Returns `None` if any monthly mean is non-finite.
#[must_use]
pub fn chill_months(tmean: &[f64; 12]) -> Option<u8> {
    if tmean.iter().any(|x| !x.is_finite()) {
        return None;
    }
    let count = tmean.iter().filter(|&&t| t > 0.0 && t <= 7.2).count();
    u8::try_from(count).ok()
}

impl MonthlySummary {
    /// Summarise 12 monthly means at a latitude; `None` if any value is not
    /// finite.
    #[must_use]
    pub fn new(t: &[f32; 12], p: &[f32; 12], lat_deg: f64) -> Option<Self> {
        let mask = dry_month_mask(t, p)?;
        let precip: f32 = p.iter().sum();
        let pet = thornthwaite_pet(t, lat_deg);
        let aridity = (pet >= ARIDITY_MIN_PET_MM)
            .then(|| (f64::from(precip) / pet).min(f64::from(ARIDITY_CAP)) as f32);
        let dry = mask.count_ones();
        Some(Self {
            tmean_cold: t.iter().copied().fold(f32::INFINITY, f32::min),
            tmean_warm: t.iter().copied().fold(f32::NEG_INFINITY, f32::max),
            precip,
            aridity,
            dry_months: u8::try_from(dry).unwrap_or(12),
            koppen_of_means: koppen::classify(t, p),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_length_is_twelve_hours_at_the_equator_and_polar_extremes() {
        assert!((day_length_hours(0.0, 80.0) - 12.0).abs() < 0.2);
        assert!((day_length_hours(80.0, 172.0) - 24.0).abs() < 1e-9);
        assert!(day_length_hours(80.0, 355.0).abs() < 1e-9);
        assert!(day_length_hours(50.0, 172.0) > 16.0);
    }

    #[test]
    fn thornthwaite_matches_known_magnitudes() {
        // Uniform 20 °C at the equator: about 900 mm a year.
        let pet = thornthwaite_pet(&[20.0; 12], 0.0);
        assert!((800.0..1000.0).contains(&pet), "{pet}");
        // Frozen all year: no PET.
        assert_eq!(thornthwaite_pet(&[-5.0; 12], 70.0), 0.0);
        // Hot branch is continuous enough at 26.5 °C.
        let below = thornthwaite_pet(&[26.49; 12], 0.0);
        let above = thornthwaite_pet(&[26.51; 12], 0.0);
        assert!((below - above).abs() / below < 0.05, "{below} {above}");
    }

    #[test]
    fn summary_of_a_mediterranean_climate() {
        let t = [
            10.9, 12.4, 15.2, 17.1, 20.7, 25.0, 28.2, 27.9, 24.8, 20.0, 14.8, 11.8,
        ];
        let p = [62., 52., 45., 51., 28., 9., 1., 4., 27., 64., 85., 92.];
        let s = MonthlySummary::new(&t, &p, 37.4).unwrap();
        assert!((s.tmean_cold - 10.9).abs() < 1e-6);
        assert!((s.tmean_warm - 28.2).abs() < 1e-6);
        assert!((s.precip - 520.0).abs() < 1e-3);
        // May, Jun, Jul, Aug, Sep are dry by Gaussen's rule.
        assert_eq!(s.dry_months, 5);
        let a = s.aridity.unwrap();
        assert!(a > 0.3 && a < 0.8, "{a}");
        assert_eq!(s.koppen_of_means.map(koppen::symbol), Some("Csa"));
    }

    #[test]
    fn frozen_and_invalid_cells() {
        let s = MonthlySummary::new(&[-20.0; 12], &[10.0; 12], 75.0).unwrap();
        assert_eq!(s.aridity, None);
        // McMurdo-like, 197 mm a year: the one month above 0 °C falls in
        // the polar night, so PET is 0: no aridity rather than "very wet".
        let mut t = [-20.0; 12];
        t[5] = 0.3;
        let s = MonthlySummary::new(&t, &[197.0 / 12.0; 12], -77.8).unwrap();
        assert!(thornthwaite_pet(&t, -77.8) < ARIDITY_MIN_PET_MM);
        assert_eq!(s.aridity, None);
        // A very wet cell with real PET is capped, not dropped.
        let mut t = [-5.0; 12];
        t[6] = 0.5;
        let s = MonthlySummary::new(&t, &[30000.0; 12], 60.0).unwrap();
        assert!(thornthwaite_pet(&t, 60.0) >= ARIDITY_MIN_PET_MM);
        assert_eq!(s.aridity, Some(ARIDITY_CAP));
        assert_eq!(s.dry_months, 0);
        let mut t = [10.0; 12];
        t[0] = f32::NAN;
        assert_eq!(MonthlySummary::new(&t, &[10.0; 12], 0.0), None);
    }

    #[test]
    fn gdd_base_10_and_chill_months_cover_edge_cases() {
        // Uniform 10 °C: 0 GDD, 0 chill months.
        assert_eq!(gdd_base_10(&[10.0; 12]), Some(0.0));
        assert_eq!(chill_months(&[10.0; 12]), Some(0));
        // Uniform 20 °C: 10 * 365.25 = 3652.5 GDD, 0 chill months.
        assert_eq!(gdd_base_10(&[20.0; 12]), Some(3652.5));
        // Temperate Paris-like: Jan..Dec means.
        let paris = [
            4.5, 5.5, 8.5, 11.5, 15.0, 18.5, 20.5, 20.0, 16.5, 12.5, 8.0, 5.0,
        ];
        // Chill months: Jan (4.5), Feb (5.5), Dec (5.0) => 3.
        assert_eq!(chill_months(&paris), Some(3));
        // Boundary checks: 0.0 is NOT a chill month; 7.2 IS; 7.21 is NOT.
        let bounds = [
            -1.0, 0.0, 0.01, 7.2, 7.21, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0,
        ];
        assert_eq!(chill_months(&bounds), Some(2));
        // Non-finite returns None.
        let mut bad = [5.0; 12];
        bad[3] = f64::NAN;
        assert_eq!(gdd_base_10(&bad), None);
        assert_eq!(chill_months(&bad), None);
    }

    #[test]
    fn photoperiod_covers_equator_hemispheres_and_poles() {
        let eq = photoperiod(0.0).unwrap();
        assert_eq!(eq.min_hours, 12.0);
        assert_eq!(eq.max_hours, 12.0);
        assert_eq!(eq.amplitude_hours, 0.0);
        assert_eq!(eq.polar_night_days, 0);
        assert_eq!(eq.midnight_sun_days, 0);

        let nh = photoperiod(45.0).unwrap();
        let sh = photoperiod(-45.0).unwrap();
        // Exact NH/SH symmetry
        assert_eq!(nh.min_hours, sh.min_hours);
        assert_eq!(nh.max_hours, sh.max_hours);
        assert!((nh.min_hours + nh.max_hours - 24.0).abs() < 1e-9);
        assert!(nh.monthly_hours[5] > nh.monthly_hours[11]); // June > Dec in NH
        assert!(sh.monthly_hours[11] > sh.monthly_hours[5]); // Dec > June in SH

        // Exact poles hardened against tan(±90°)
        let np = photoperiod(90.0).unwrap();
        let sp = photoperiod(-90.0).unwrap();
        assert_eq!(
            (np.min_hours, np.max_hours, np.amplitude_hours),
            (0.0, 24.0, 24.0)
        );
        assert_eq!(
            (sp.min_hours, sp.max_hours, sp.amplitude_hours),
            (0.0, 24.0, 24.0)
        );
        assert!(np.polar_night_days > 170 && np.midnight_sun_days > 170);
        assert_eq!(np.polar_night_days + np.midnight_sun_days, 365);

        // Out-of-domain rejected
        assert_eq!(photoperiod(90.1), None);
        assert_eq!(photoperiod(-90.1), None);
        assert_eq!(photoperiod(f64::NAN), None);

        // BUG-07: monthly_hours is [f64; 12] and serializes cleanly without f32 float noise.
        let json = serde_json::to_string(&nh).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        for h in val["monthly_hours"].as_array().unwrap() {
            assert!(h.to_string().len() <= 6, "noisy float in JSON: {h}");
        }
    }

    #[test]
    fn mid_month_doy_is_pinned_for_aridity_reproducibility() {
        // Feeds Thornthwaite PET -> aridity in climate tiles; must not drift.
        assert_eq!(
            MID_MONTH_DOY,
            [
                15., 46., 75., 105., 135., 162., 198., 228., 258., 288., 318., 344.
            ]
        );
    }
}
