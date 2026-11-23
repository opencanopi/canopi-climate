//! Zero-byte species climate trajectory and suitability evaluator
//! (`trajectory::evaluate_trajectory`).
//!
//! Evaluates a species' optional ecological envelope across three horizons
//! (`today`, `mid_century`, `late_century`), reporting:
//! - Up to 6 canonical limiting-factor margins (`cold_extreme`, `heat_extreme`,
//!   `water_balance`, `chill`, `soil_ph`, `drainage`, plus optional `false_spring`),
//! - Per-period suitability (`Optimal | Suitable | Marginal | Unsuitable`) and
//!   bottleneck score (`0.0..=1.0`),
//! - Overall `TrajectoryClass` (`Stable | Improving | Declining | Transitional | Unsuitable`),
//! - Typed `EstablishmentVsMaturityGap` (`None | ColdEstablishmentBarrier | FutureMaturityRisk | Both`).
//!
//! Method: `docs/decisions/0008-soil-plus-terrain-microclimate-packs.md`.

use serde::{Deserialize, Serialize};

/// Canonical ecological limiting factor identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactorId {
    /// Extreme cold survival (`tmin_p10` or `tmin_extreme`).
    ColdExtreme,
    /// Extreme heat survival (`tmax_p90` or `tmax_extreme`).
    HeatExtreme,
    /// Water balance (`cwd_mm`, else `precip_mm` / `dry_months`).
    WaterBalance,
    /// Winter chill requirement (Dynamic Model Chill Portions).
    Chill,
    /// Soil pH compatibility (topsoil and subsoil calcareous chlorosis check).
    SoilPh,
    /// Topographic drainage (`hand_m` and optional subsoil claypan).
    Drainage,
    /// False-spring budbreak frost risk (`false_spring_pct`).
    FalseSpring,
}

/// Suitability band derived from the worst (bottleneck) limiting factor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Suitability {
    /// Fails one or more hard constraints (`score < 0.25`).
    Unsuitable,
    /// Near or slightly past constraint boundary (`0.25 <= score < 0.60`).
    Marginal,
    /// Safely within constraints (`0.60 <= score < 0.85`).
    Suitable,
    /// Generous headroom on all evaluated constraints (`score >= 0.85`).
    Optimal,
}

/// Multi-decade suitability trajectory across `today -> mid_century -> late_century`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrajectoryClass {
    /// Suitable or optimal in every horizon.
    Stable,
    /// Unsuitable or marginal today, becoming suitable/optimal later.
    Improving,
    /// Suitable or optimal today, declining to marginal/unsuitable later.
    Declining,
    /// Peak suitability occurs only at mid-century, or fluctuates across horizons.
    Transitional,
    /// Unsuitable or marginal across all horizons.
    Unsuitable,
}

/// Explicit distinction between establishment barriers today and maturity risk later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EstablishmentVsMaturityGap {
    /// No establishment-vs-maturity conflict.
    None,
    /// Too cold to establish reliably today, but suitable by mid/late century.
    ColdEstablishmentBarrier,
    /// Suitable to establish today, but fails before maturity by mid/late century.
    FutureMaturityRisk,
    /// Cold-limited today AND heat/drought/chill-limited by late century.
    Both,
}

/// Optional species ecological envelope (partial profiles evaluate only supplied bounds).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SpeciesEnvelope {
    /// Minimum tolerated coldest night (°C).
    #[serde(default)]
    pub min_tmin_extreme_c: Option<f64>,
    /// Maximum tolerated hottest day (°C).
    #[serde(default)]
    pub max_tmax_extreme_c: Option<f64>,
    /// Maximum tolerated annual Climatic Water Deficit (mm/yr).
    #[serde(default)]
    pub max_cwd_mm: Option<f64>,
    /// Fallback minimum annual precipitation (mm/yr) when `cwd_mm` is absent.
    #[serde(default)]
    pub min_precip_mm: Option<f64>,
    /// Fallback maximum dry months (0–12) when `cwd_mm` is absent.
    #[serde(default)]
    pub max_dry_months: Option<f64>,
    /// Minimum required Dynamic Model Chill Portions (CP).
    #[serde(default)]
    pub min_chill_portions: Option<f64>,
    /// Minimum tolerated soil pH.
    #[serde(default)]
    pub min_ph: Option<f64>,
    /// Maximum tolerated soil pH (checked against both topsoil and subsoil).
    #[serde(default)]
    pub max_ph: Option<f64>,
    /// Minimum required Height Above Nearest Drainage (m).
    #[serde(default)]
    pub min_hand_m: Option<f64>,
    /// Maximum tolerated false-spring frequency (%).
    #[serde(default)]
    pub max_false_spring_pct: Option<f64>,
    /// Whether the species tolerates a heavy claypan subsoil (`clay_30_100 >= 40%`).
    #[serde(default)]
    pub tolerates_claypan: Option<bool>,
}

/// Climate snapshot of a site for one time horizon.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PeriodSiteSnapshot {
    /// Horizon label (e.g. `"1991-2020"`, `"2041-2070"`, `"2071-2099"`).
    pub label: String,
    /// Coldest night (`tmin_p10` when available, else `tmin_extreme`, °C).
    #[serde(default)]
    pub tmin_extreme_c: Option<f64>,
    /// Hottest day (`tmax_p90` when available, else `tmax_extreme`, °C).
    #[serde(default)]
    pub tmax_extreme_c: Option<f64>,
    /// Annual Climatic Water Deficit (mm/yr).
    #[serde(default)]
    pub cwd_mm: Option<f64>,
    /// Annual precipitation (mm/yr).
    #[serde(default)]
    pub precip_mm: Option<f64>,
    /// Dry months count (0–12).
    #[serde(default)]
    pub dry_months: Option<f64>,
    /// Dynamic Model Chill Portions (CP).
    #[serde(default)]
    pub chill_portions: Option<f64>,
}

/// Full multi-period site input for [`evaluate_trajectory`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrajectorySiteInput {
    /// Present baseline (`1991-2020`).
    pub today: PeriodSiteSnapshot,
    /// Mid-century (`2041-2070`, typically SSP2-4.5).
    pub mid_century: PeriodSiteSnapshot,
    /// Late-century (`2071-2099`, typically SSP2-4.5).
    pub late_century: PeriodSiteSnapshot,
    /// Topsoil 0–30 cm pH in water.
    #[serde(default)]
    pub ph_topsoil: Option<f64>,
    /// Subsoil 30–100 cm pH in water (calcareous chlorosis check).
    #[serde(default)]
    pub ph_subsoil: Option<f64>,
    /// Height Above Nearest Drainage (m).
    #[serde(default)]
    pub hand_m: Option<f64>,
    /// Subsoil 30–100 cm clay content (%).
    #[serde(default)]
    pub clay_30_100_pct: Option<f64>,
    /// False-spring frequency (%).
    #[serde(default)]
    pub false_spring_pct: Option<f64>,
    /// Names of layers present when assembling this input.
    #[serde(default)]
    pub evaluated_layers: Vec<String>,
}

/// Headroom/deficit for one ecological constraint.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FactorMargin {
    /// Factor identifier.
    pub factor: FactorId,
    /// Physical margin (positive = headroom, negative = deficit).
    pub margin: f64,
    /// Physical unit of `margin`.
    pub unit: &'static str,
    /// Normalized score in `0.0..=1.0`.
    pub score: f64,
}

/// Suitability evaluation for a single time horizon.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PeriodEvaluation {
    /// Horizon label.
    pub label: String,
    /// Overall suitability band.
    pub suitability: Suitability,
    /// Bottleneck score (`0.0..=1.0`).
    pub score: f64,
    /// Evaluated factor margins.
    pub margins: Vec<FactorMargin>,
    /// Worst (lowest-scoring) factor, if any constraints were evaluated.
    pub limiting_factor: Option<FactorId>,
}

/// Three-period evaluation bundle.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrajectoryPeriods {
    /// Baseline horizon.
    pub today: PeriodEvaluation,
    /// Mid-century horizon.
    pub mid_century: PeriodEvaluation,
    /// Late-century horizon.
    pub late_century: PeriodEvaluation,
}

/// Result of [`evaluate_trajectory`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrajectoryResult {
    /// Per-horizon evaluations.
    pub periods: TrajectoryPeriods,
    /// Overall multi-decade trajectory classification.
    pub trajectory_class: TrajectoryClass,
    /// Typed establishment-vs-maturity gap diagnostic.
    pub establishment_vs_maturity_gap: EstablishmentVsMaturityGap,
    /// Convenience boolean (`establishment_vs_maturity_gap != None`).
    pub has_gap: bool,
    /// Layers present at evaluation time.
    pub evaluated_layers: Vec<String>,
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Map a physical margin to a `0.0..=1.0` suitability score using `scale`
/// (the headroom needed to reach `1.0`, and the deficit at which score hits `0.0`).
fn margin_score(margin: f64, scale: f64) -> f64 {
    if margin >= scale {
        1.0
    } else if margin >= 0.0 {
        0.60 + 0.40 * (margin / scale)
    } else if margin > -scale {
        0.60 * (1.0 + margin / scale)
    } else {
        0.0
    }
}

fn classify_score(score: f64) -> Suitability {
    if score >= 0.85 {
        Suitability::Optimal
    } else if score >= 0.60 {
        Suitability::Suitable
    } else if score >= 0.25 {
        Suitability::Marginal
    } else {
        Suitability::Unsuitable
    }
}

fn eval_soil_ph(site: &TrajectorySiteInput, env: &SpeciesEnvelope) -> Option<FactorMargin> {
    if env.min_ph.is_none() && env.max_ph.is_none() {
        return None;
    }
    let mut worst: Option<f64> = None;
    for ph in [site.ph_topsoil, site.ph_subsoil].into_iter().flatten() {
        if !ph.is_finite() {
            continue;
        }
        let mut m = f64::INFINITY;
        if let Some(min_ph) = env.min_ph {
            m = m.min(ph - min_ph);
        }
        if let Some(max_ph) = env.max_ph {
            m = m.min(max_ph - ph);
        }
        if m.is_finite() {
            worst = Some(worst.map_or(m, |w| w.min(m)));
        }
    }
    let margin = worst?;
    Some(FactorMargin {
        factor: FactorId::SoilPh,
        margin: round2(margin),
        unit: "pH",
        score: round2(margin_score(margin, 0.8)),
    })
}

fn eval_drainage(site: &TrajectorySiteInput, env: &SpeciesEnvelope) -> Option<FactorMargin> {
    let mut worst_score = f64::INFINITY;
    let mut worst_margin = f64::INFINITY;
    let mut worst_unit = "m";
    let mut evaluated = false;

    if let (Some(hand), Some(min_hand)) = (site.hand_m, env.min_hand_m)
        && hand.is_finite()
        && min_hand.is_finite()
    {
        let m = hand - min_hand;
        worst_margin = m;
        worst_score = margin_score(m, 3.0);
        worst_unit = "m";
        evaluated = true;
    }
    if env.tolerates_claypan == Some(false)
        && let Some(clay) = site.clay_30_100_pct
        && clay.is_finite()
        && clay >= 40.0
    {
        let clay_score = margin_score(40.0 - clay, 15.0);
        if !evaluated || clay_score < worst_score {
            worst_score = clay_score;
            worst_margin = 40.0 - clay;
            worst_unit = "%";
            evaluated = true;
        }
    }
    evaluated.then(|| FactorMargin {
        factor: FactorId::Drainage,
        margin: round2(worst_margin),
        unit: worst_unit,
        score: round2(worst_score),
    })
}

fn eval_period(
    snap: &PeriodSiteSnapshot,
    site: &TrajectorySiteInput,
    env: &SpeciesEnvelope,
) -> PeriodEvaluation {
    let mut margins = Vec::new();

    if let (Some(tmin), Some(min_req)) = (snap.tmin_extreme_c, env.min_tmin_extreme_c) {
        let m = tmin - min_req;
        margins.push(FactorMargin {
            factor: FactorId::ColdExtreme,
            margin: round2(m),
            unit: "C",
            score: round2(margin_score(m, 5.0)),
        });
    }
    if let (Some(tmax), Some(max_req)) = (snap.tmax_extreme_c, env.max_tmax_extreme_c) {
        let m = max_req - tmax;
        margins.push(FactorMargin {
            factor: FactorId::HeatExtreme,
            margin: round2(m),
            unit: "C",
            score: round2(margin_score(m, 4.0)),
        });
    }
    if let (Some(cwd), Some(max_cwd)) = (snap.cwd_mm, env.max_cwd_mm) {
        let m = max_cwd - cwd;
        margins.push(FactorMargin {
            factor: FactorId::WaterBalance,
            margin: round2(m),
            unit: "mm",
            score: round2(margin_score(m, 150.0)),
        });
    } else if let (Some(p), Some(min_p)) = (snap.precip_mm, env.min_precip_mm) {
        let m = p - min_p;
        margins.push(FactorMargin {
            factor: FactorId::WaterBalance,
            margin: round2(m),
            unit: "mm",
            score: round2(margin_score(m, 150.0)),
        });
    } else if let (Some(dm), Some(max_dm)) = (snap.dry_months, env.max_dry_months) {
        let m = max_dm - dm;
        margins.push(FactorMargin {
            factor: FactorId::WaterBalance,
            margin: round2(m),
            unit: "months",
            score: round2(margin_score(m, 2.0)),
        });
    }
    if let (Some(cp), Some(min_cp)) = (snap.chill_portions, env.min_chill_portions) {
        let m = cp - min_cp;
        margins.push(FactorMargin {
            factor: FactorId::Chill,
            margin: round2(m),
            unit: "CP",
            score: round2(margin_score(m, 15.0)),
        });
    }
    if let Some(m) = eval_soil_ph(site, env) {
        margins.push(m);
    }
    if let Some(m) = eval_drainage(site, env) {
        margins.push(m);
    }
    if let (Some(fs), Some(max_fs)) = (site.false_spring_pct, env.max_false_spring_pct) {
        let m = max_fs - fs;
        margins.push(FactorMargin {
            factor: FactorId::FalseSpring,
            margin: round2(m),
            unit: "%",
            score: round2(margin_score(m, 20.0)),
        });
    }

    let worst = margins.iter().min_by(|a, b| a.score.total_cmp(&b.score));
    let (score, limiting_factor) = match worst {
        Some(w) => (w.score, Some(w.factor)),
        None => (1.0, None),
    };
    PeriodEvaluation {
        label: snap.label.clone(),
        suitability: classify_score(score),
        score,
        margins,
        limiting_factor,
    }
}

/// Evaluate a species' multi-decade climate and soil trajectory.
#[must_use]
pub fn evaluate_trajectory(site: &TrajectorySiteInput, env: &SpeciesEnvelope) -> TrajectoryResult {
    let today = eval_period(&site.today, site, env);
    let mid = eval_period(&site.mid_century, site, env);
    let late = eval_period(&site.late_century, site, env);

    let ok = |s: Suitability| s >= Suitability::Suitable;
    let t_ok = ok(today.suitability);
    let m_ok = ok(mid.suitability);
    let l_ok = ok(late.suitability);

    let trajectory_class = match (t_ok, m_ok, l_ok) {
        (true, true, true) => TrajectoryClass::Stable,
        (false, true, true) | (false, false, true) => TrajectoryClass::Improving,
        (true, true, false) | (true, false, false) => TrajectoryClass::Declining,
        (false, true, false) | (true, false, true) => TrajectoryClass::Transitional,
        (false, false, false) => TrajectoryClass::Unsuitable,
    };

    let cold_barrier_today =
        !t_ok && today.limiting_factor == Some(FactorId::ColdExtreme) && (m_ok || l_ok);
    let future_risk = (t_ok || m_ok) && !l_ok;
    let gap = match (cold_barrier_today, future_risk) {
        (true, true) => EstablishmentVsMaturityGap::Both,
        (true, false) => EstablishmentVsMaturityGap::ColdEstablishmentBarrier,
        (false, true) => EstablishmentVsMaturityGap::FutureMaturityRisk,
        (false, false) => EstablishmentVsMaturityGap::None,
    };

    TrajectoryResult {
        periods: TrajectoryPeriods {
            today,
            mid_century: mid,
            late_century: late,
        },
        trajectory_class,
        establishment_vs_maturity_gap: gap,
        has_gap: gap != EstablishmentVsMaturityGap::None,
        evaluated_layers: site.evaluated_layers.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(label: &str, tmin: f64, tmax: f64, cwd: f64, cp: f64) -> PeriodSiteSnapshot {
        PeriodSiteSnapshot {
            label: label.into(),
            tmin_extreme_c: Some(tmin),
            tmax_extreme_c: Some(tmax),
            cwd_mm: Some(cwd),
            precip_mm: Some(700.0),
            dry_months: Some(1.0),
            chill_portions: Some(cp),
        }
    }

    #[test]
    fn distinguishes_cold_establishment_barrier_and_future_maturity_risk() {
        // Olive-like species: cold-limited today, thrives in 2050 & 2080 -> ColdEstablishmentBarrier
        let olive = SpeciesEnvelope {
            min_tmin_extreme_c: Some(-10.0),
            max_tmax_extreme_c: Some(45.0),
            max_cwd_mm: Some(700.0),
            min_chill_portions: Some(20.0),
            ..Default::default()
        };
        let site = TrajectorySiteInput {
            today: snap("1991-2020", -14.0, 35.0, 220.0, 80.0),
            mid_century: snap("2041-2070", -9.0, 38.0, 300.0, 65.0),
            late_century: snap("2071-2099", -7.0, 40.0, 360.0, 50.0),
            ph_topsoil: Some(7.0),
            ph_subsoil: Some(7.4),
            hand_m: Some(12.0),
            clay_30_100_pct: Some(25.0),
            false_spring_pct: Some(10.0),
            evaluated_layers: vec!["climate".into(), "extremes".into(), "phenology".into()],
        };
        let res = evaluate_trajectory(&site, &olive);
        assert_eq!(res.trajectory_class, TrajectoryClass::Improving);
        assert_eq!(
            res.establishment_vs_maturity_gap,
            EstablishmentVsMaturityGap::ColdEstablishmentBarrier
        );
        assert!(res.has_gap);

        // High-chill apple: thrives today, fails chill by 2080 -> FutureMaturityRisk
        let apple = SpeciesEnvelope {
            min_tmin_extreme_c: Some(-25.0),
            max_tmax_extreme_c: Some(42.0),
            min_chill_portions: Some(60.0),
            ..Default::default()
        };
        let res_apple = evaluate_trajectory(&site, &apple);
        assert_eq!(res_apple.trajectory_class, TrajectoryClass::Declining);
        assert_eq!(
            res_apple.establishment_vs_maturity_gap,
            EstablishmentVsMaturityGap::FutureMaturityRisk
        );
        assert_eq!(
            res_apple.periods.late_century.limiting_factor,
            Some(FactorId::Chill)
        );
    }

    #[test]
    fn detects_calcareous_subsoil_and_claypan_and_partial_envelopes() {
        let chestnut = SpeciesEnvelope {
            min_ph: Some(4.5),
            max_ph: Some(6.5),
            tolerates_claypan: Some(false),
            ..Default::default()
        };
        let site = TrajectorySiteInput {
            today: snap("1991-2020", -8.0, 34.0, 200.0, 80.0),
            mid_century: snap("2041-2070", -6.0, 36.0, 250.0, 65.0),
            late_century: snap("2071-2099", -5.0, 38.0, 300.0, 55.0),
            ph_topsoil: Some(6.2), // topsoil looks fine!
            ph_subsoil: Some(7.9), // calcareous subsoil triggers chlorosis!
            hand_m: Some(10.0),
            clay_30_100_pct: Some(48.0),
            false_spring_pct: None,
            evaluated_layers: vec!["soil".into(), "soil_plus".into()],
        };
        let res = evaluate_trajectory(&site, &chestnut);
        assert_eq!(res.trajectory_class, TrajectoryClass::Unsuitable);
        assert_eq!(res.periods.today.limiting_factor, Some(FactorId::SoilPh));

        // Empty partial envelope gracefully returns Optimal with 0 margins
        let empty = evaluate_trajectory(&site, &SpeciesEnvelope::default());
        assert_eq!(empty.trajectory_class, TrajectoryClass::Stable);
        assert!(empty.periods.today.margins.is_empty());
    }
}
