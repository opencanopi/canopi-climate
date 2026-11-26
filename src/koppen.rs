//! Köppen-Geiger classes: the Beck et al. (2023) legend, the classification
//! of 12 monthly means (Beck et al. 2018, Table 1, the scheme Beck et al.
//! 2023 apply), and a check of a stored class against stored temperatures.

/// Class symbols by code (index 0 is unused: no class).
pub const SYMBOLS: [&str; 31] = [
    "", "Af", "Am", "Aw", "BWh", "BWk", "BSh", "BSk", "Csa", "Csb", "Csc", "Cwa", "Cwb", "Cwc",
    "Cfa", "Cfb", "Cfc", "Dsa", "Dsb", "Dsc", "Dsd", "Dwa", "Dwb", "Dwc", "Dwd", "Dfa", "Dfb",
    "Dfc", "Dfd", "ET", "EF",
];

/// Symbol of a class code (`""` for an unknown code).
#[must_use]
pub fn symbol(code: u8) -> &'static str {
    SYMBOLS.get(usize::from(code)).copied().unwrap_or("")
}

/// Code of a class symbol.
#[must_use]
pub fn code(symbol: &str) -> Option<u8> {
    if symbol.is_empty() {
        return None;
    }
    SYMBOLS
        .iter()
        .position(|s| *s == symbol)
        .and_then(|i| u8::try_from(i).ok())
}

/// Main climate group of a class: `'A'`, `'B'`, `'C'`, `'D'` or `'E'`.
#[must_use]
pub fn group(code: u8) -> Option<char> {
    symbol(code).chars().next()
}

/// Months April-September (0-based January).
const APR_SEP: [usize; 6] = [3, 4, 5, 6, 7, 8];
/// Months October-March.
const OCT_MAR: [usize; 6] = [9, 10, 11, 0, 1, 2];

fn mean(v: &[f32]) -> f32 {
    v.iter().sum::<f32>() / v.len() as f32
}

fn pick(values: &[f32; 12], months: &[usize; 6]) -> [f32; 6] {
    months.map(|m| values[m])
}

fn min_of(v: &[f32]) -> f32 {
    v.iter().copied().fold(f32::INFINITY, f32::min)
}

fn max_of(v: &[f32]) -> f32 {
    v.iter().copied().fold(f32::NEG_INFINITY, f32::max)
}

/// Seasonal precipitation figures of Beck et al. (2018), Table 1.
struct Seasons {
    p_summer: f32,
    p_winter: f32,
    ps_dry: f32,
    ps_wet: f32,
    pw_dry: f32,
    pw_wet: f32,
}

impl Seasons {
    /// Summer is the warmer of April-September and October-March.
    fn new(t: &[f32; 12], p: &[f32; 12]) -> Self {
        let (summer, winter) = if mean(&pick(t, &APR_SEP)) >= mean(&pick(t, &OCT_MAR)) {
            (APR_SEP, OCT_MAR)
        } else {
            (OCT_MAR, APR_SEP)
        };
        let ps = pick(p, &summer);
        let pw = pick(p, &winter);
        Self {
            p_summer: ps.iter().sum(),
            p_winter: pw.iter().sum(),
            ps_dry: min_of(&ps),
            ps_wet: max_of(&ps),
            pw_dry: min_of(&pw),
            pw_wet: max_of(&pw),
        }
    }

    /// Second letter of C and D climates: `s`, `w` or `f`. When both the `s`
    /// and the `w` criteria hold, the wetter half-year decides (`w` if summer
    /// is wetter).
    fn cd_letter(&self) -> char {
        let s = self.ps_dry < 40.0 && self.ps_dry < self.pw_wet / 3.0;
        let w = self.pw_dry < self.ps_wet / 10.0;
        match (s, w) {
            (true, true) if self.p_summer > self.p_winter => 'w',
            (true, _) => 's',
            (false, true) => 'w',
            (false, false) => 'f',
        }
    }
}

/// Classify 12 monthly mean temperatures (°C) and precipitation totals (mm),
/// January first. Returns a code 1-30, or `None` for non-finite input.
#[must_use]
pub fn classify(t: &[f32; 12], p: &[f32; 12]) -> Option<u8> {
    if t.iter().chain(p.iter()).any(|x| !x.is_finite()) {
        return None;
    }
    let mat = mean(t);
    let map: f32 = p.iter().sum();
    let t_cold = min_of(t);
    let t_hot = max_of(t);
    let t_mon10 = t.iter().filter(|&&x| x > 10.0).count();
    let p_dry = min_of(p);
    let s = Seasons::new(t, p);

    let p_threshold = if s.p_winter > 0.7 * map {
        2.0 * mat
    } else if s.p_summer > 0.7 * map {
        2.0 * mat + 28.0
    } else {
        2.0 * mat + 14.0
    };

    // Peel et al. (2007, HESS 11:1633, p. 1636) / Beck et al. (2018): Group B (arid)
    // is evaluated before A, C, D, and E so dry polar/alpine cells with MAP < 10 * P_thresh
    // classify as BSk/BWk rather than ET/EF.
    let sym = if map < 10.0 * p_threshold {
        let desert = map < 5.0 * p_threshold;
        let hot = mat >= 18.0;
        match (desert, hot) {
            (true, true) => "BWh",
            (true, false) => "BWk",
            (false, true) => "BSh",
            (false, false) => "BSk",
        }
        .to_string()
    } else if t_hot <= 10.0 {
        if t_hot > 0.0 { "ET" } else { "EF" }.to_string()
    } else if t_cold >= 18.0 {
        if p_dry >= 60.0 {
            "Af"
        } else if p_dry >= 100.0 - map / 25.0 {
            "Am"
        } else {
            "Aw"
        }
        .to_string()
    } else {
        let first = if t_cold > 0.0 { 'C' } else { 'D' };
        let third = if t_hot >= 22.0 {
            'a'
        } else if t_mon10 >= 4 {
            'b'
        } else if first == 'D' && t_cold < -38.0 {
            'd'
        } else {
            'c'
        };
        format!("{first}{}{third}", s.cd_letter())
    };
    code(&sym)
}

/// How far (°C) the coldest- and warmest-month means lie outside the
/// temperature bounds of a class; 0 when they agree. Only temperature
/// criteria are checked: precipitation (B, and the second letter of A, C, D)
/// and the month count separating `b` from `c` are not stored.
#[must_use]
pub fn temperature_margin(code: u8, t_cold: f32, t_warm: f32) -> f32 {
    let sym = symbol(code).as_bytes();
    let (mut cold, mut warm) = (
        (f32::NEG_INFINITY, f32::INFINITY),
        (f32::NEG_INFINITY, f32::INFINITY),
    );
    match sym {
        [b'A', ..] => cold.0 = 18.0,
        [b'C', _, third] => {
            cold = (0.0, 18.0);
            warm.0 = 10.0;
            apply_summer(*third, &mut cold, &mut warm);
        }
        [b'D', _, third] => {
            cold.1 = 0.0;
            warm.0 = 10.0;
            apply_summer(*third, &mut cold, &mut warm);
        }
        b"ET" => warm = (0.0, 10.0),
        b"EF" => warm.1 = 0.0,
        _ => return 0.0,
    }
    let outside = |x: f32, (lo, hi): (f32, f32)| (lo - x).max(x - hi).max(0.0);
    outside(t_cold, cold).max(outside(t_warm, warm))
}

fn apply_summer(third: u8, cold: &mut (f32, f32), warm: &mut (f32, f32)) {
    match third {
        b'a' => warm.0 = 22.0,
        b'b' | b'c' => {
            warm.1 = 22.0;
            if third == b'c' && cold.0.is_infinite() {
                cold.0 = -38.0;
            }
        }
        b'd' => {
            warm.1 = 22.0;
            cold.1 = cold.1.min(-38.0);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legend_codes_and_symbols() {
        assert_eq!(symbol(1), "Af");
        assert_eq!(symbol(15), "Cfb");
        assert_eq!(symbol(30), "EF");
        assert_eq!(symbol(0), "");
        assert_eq!(symbol(31), "");
        for c in 1..=30u8 {
            assert_eq!(code(symbol(c)), Some(c));
        }
        assert_eq!(code(""), None);
        assert_eq!(group(26), Some('D'));
    }

    // Monthly normals rounded from published station climatologies
    // (shape matters, not the decimals).
    const PARIS_T: [f32; 12] = [
        5.0, 5.6, 8.8, 11.5, 15.2, 18.3, 20.5, 20.3, 16.9, 13.0, 8.3, 5.5,
    ];
    const PARIS_P: [f32; 12] = [51., 41., 48., 52., 63., 50., 62., 52., 47., 62., 51., 58.];
    const SEVILLE_T: [f32; 12] = [
        10.9, 12.4, 15.2, 17.1, 20.7, 25.0, 28.2, 27.9, 24.8, 20.0, 14.8, 11.8,
    ];
    const SEVILLE_P: [f32; 12] = [62., 52., 45., 51., 28., 9., 1., 4., 27., 64., 85., 92.];
    const MANAUS_T: [f32; 12] = [
        26.1, 26.0, 26.1, 26.2, 26.4, 26.5, 26.6, 27.3, 27.9, 27.8, 27.3, 26.7,
    ];
    const MANAUS_P: [f32; 12] = [
        264., 291., 315., 300., 256., 114., 88., 58., 83., 126., 183., 220.,
    ];
    const CAIRO_T: [f32; 12] = [
        14.0, 15.1, 17.6, 21.3, 24.9, 27.2, 28.0, 28.0, 26.3, 23.6, 19.1, 15.4,
    ];
    const CAIRO_P: [f32; 12] = [5., 4., 4., 1., 0., 0., 0., 0., 0., 1., 3., 6.];
    const YAKUTSK_T: [f32; 12] = [
        -38.6, -33.8, -20.1, -4.8, 7.5, 16.4, 19.5, 15.2, 6.1, -7.8, -27.0, -37.6,
    ];
    const YAKUTSK_P: [f32; 12] = [9., 8., 7., 10., 23., 38., 45., 39., 27., 20., 15., 11.];
    const NUUK_LIKE_T: [f32; 12] = [
        -7.0, -7.5, -7.5, -3.5, 1.0, 4.5, 7.0, 6.5, 3.5, -1.0, -4.0, -6.0,
    ];
    const NUUK_LIKE_P: [f32; 12] = [40., 45., 45., 35., 45., 55., 80., 85., 85., 65., 70., 55.];

    #[test]
    fn classifies_reference_climates() {
        assert_eq!(classify(&PARIS_T, &PARIS_P).map(symbol), Some("Cfb"));
        assert_eq!(classify(&SEVILLE_T, &SEVILLE_P).map(symbol), Some("Csa"));
        assert_eq!(classify(&MANAUS_T, &MANAUS_P).map(symbol), Some("Am"));
        assert_eq!(classify(&CAIRO_T, &CAIRO_P).map(symbol), Some("BWh"));
        assert_eq!(classify(&YAKUTSK_T, &YAKUTSK_P).map(symbol), Some("Dfd"));
        assert_eq!(classify(&NUUK_LIKE_T, &NUUK_LIKE_P).map(symbol), Some("ET"));
        let ef = [-30.0f32; 12];
        assert_eq!(classify(&ef, &[20.0; 12]).map(symbol), Some("EF"));
        // Peel et al. (2007, HESS 11:1633, p. 1636): Group B is identified before E,
        // so dry tundra with MAP = 120 mm (< 10 * 15.3 = 153 mm, > 5 * 15.3 = 76.5 mm) is BSk.
        let dry_tundra_t = [-6.0, -5.0, -3.0, 0.0, 3.0, 6.0, 8.0, 7.0, 4.0, 1.0, -2.0, -5.0];
        let dry_tundra_p = [10.0; 12];
        assert_eq!(
            classify(&dry_tundra_t, &dry_tundra_p).map(symbol),
            Some("BSk")
        );
        let mut bad = PARIS_T;
        bad[3] = f32::NAN;
        assert_eq!(classify(&bad, &PARIS_P), None);
    }

    #[test]
    fn southern_hemisphere_summer_is_october_to_march() {
        // Seville shifted by six months: a southern Mediterranean climate.
        let mut t = [0.0f32; 12];
        let mut p = [0.0f32; 12];
        for m in 0..12 {
            t[m] = SEVILLE_T[(m + 6) % 12];
            p[m] = SEVILLE_P[(m + 6) % 12];
        }
        assert_eq!(classify(&t, &p).map(symbol), Some("Csa"));
    }

    #[test]
    fn temperature_margin_flags_contradictions() {
        let aw = code("Aw").unwrap();
        assert_eq!(temperature_margin(aw, 19.0, 25.0), 0.0);
        // Nairobi-like: a tropical class with a 16 °C coldest month.
        assert!((temperature_margin(aw, 16.0, 20.0) - 2.0).abs() < 1e-6);
        let cfb = code("Cfb").unwrap();
        assert_eq!(temperature_margin(cfb, 4.8, 20.1), 0.0);
        assert!((temperature_margin(cfb, 4.8, 23.0) - 1.0).abs() < 1e-6);
        assert!((temperature_margin(cfb, -1.0, 20.0) - 1.0).abs() < 1e-6);
        let dwd = code("Dwd").unwrap();
        assert_eq!(temperature_margin(dwd, -40.0, 19.0), 0.0);
        assert!((temperature_margin(dwd, -30.0, 19.0) - 8.0).abs() < 1e-6);
        let dfc = code("Dfc").unwrap();
        assert!((temperature_margin(dfc, -40.0, 15.0) - 2.0).abs() < 1e-6);
        let et = code("ET").unwrap();
        assert!((temperature_margin(et, -10.0, 12.0) - 2.0).abs() < 1e-6);
        let bwh = code("BWh").unwrap();
        assert_eq!(temperature_margin(bwh, -50.0, 50.0), 0.0);
        assert_eq!(temperature_margin(0, 0.0, 0.0), 0.0);
    }
}
