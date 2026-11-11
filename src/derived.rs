//! Measures derived from 12 monthly means: the per-cell summaries stored in
//! the grid. No hardiness zones: the grid's coldest-month minimum is not an
//! annual extreme minimum.

use crate::koppen;
use crate::model::ARIDITY_CAP;

/// Mid-month day of year, January first.
const MID_MONTH_DOY: [f64; 12] = [
    15., 46., 75., 105., 135., 162., 198., 228., 258., 288., 318., 344.,
];
/// Mean days per month (February averaged over leap years).
const DAYS_IN_MONTH: [f64; 12] = [31., 28.25, 31., 30., 31., 30., 31., 31., 30., 31., 30., 31.];

/// Day length (hours) at a latitude on a day of year (FAO-56 eq. 24, 25, 34).
#[must_use]
pub fn day_length_hours(lat_deg: f64, doy: f64) -> f64 {
    let phi = lat_deg.to_radians();
    let declination = 0.409 * (2.0 * std::f64::consts::PI * doy / 365.0 - 1.39).sin();
    let x = (-phi.tan() * declination.tan()).clamp(-1.0, 1.0);
    24.0 * x.acos() / std::f64::consts::PI
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
    /// Precipitation / Thornthwaite PET, capped at [`ARIDITY_CAP`]
    /// (the cap also applies where PET is 0).
    pub aridity: f32,
    /// Months with P (mm) < 2 T (°C).
    pub dry_months: u8,
    /// Köppen-Geiger class of the monthly means themselves.
    pub koppen_of_means: Option<u8>,
}

impl MonthlySummary {
    /// Summarise 12 monthly means at a latitude; `None` if any value is not
    /// finite.
    #[must_use]
    pub fn new(t: &[f32; 12], p: &[f32; 12], lat_deg: f64) -> Option<Self> {
        if t.iter().chain(p.iter()).any(|x| !x.is_finite()) {
            return None;
        }
        let precip: f32 = p.iter().sum();
        let pet = thornthwaite_pet(t, lat_deg);
        let aridity = if pet > 0.0 {
            (f64::from(precip) / pet).min(f64::from(ARIDITY_CAP)) as f32
        } else {
            ARIDITY_CAP
        };
        let dry = (0..12).filter(|&m| p[m] < 2.0 * t[m]).count();
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
        assert!(s.aridity > 0.3 && s.aridity < 0.8, "{}", s.aridity);
        assert_eq!(s.koppen_of_means.map(koppen::symbol), Some("Csa"));
    }

    #[test]
    fn frozen_and_invalid_cells() {
        let s = MonthlySummary::new(&[-20.0; 12], &[10.0; 12], 75.0).unwrap();
        assert_eq!(s.aridity, ARIDITY_CAP);
        assert_eq!(s.dry_months, 0);
        let mut t = [10.0; 12];
        t[0] = f32::NAN;
        assert_eq!(MonthlySummary::new(&t, &[10.0; 12], 0.0), None);
    }
}
