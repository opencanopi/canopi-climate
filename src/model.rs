//! What the grid stores: variables, periods, scenarios and their integer
//! encoding.
//!
//! Every tile holds `VARIABLES.len() * SLOTS.len()` bands of `i16`; band
//! `var * SLOTS.len() + slot` (0-based) holds variable `var` for slot `slot`.
//! A stored value `q` means `q * scale + offset` in the variable's unit;
//! [`NODATA`] means no value.

use serde::Serialize;

/// Stored value meaning "no data" (ocean, or a source without a value).
pub const NODATA: i16 = i16::MIN;

/// A stored climate variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Variable {
    /// Köppen-Geiger class code 1-30 (Beck et al. 2023 legend).
    Koppen,
    /// Mean daily minimum temperature of the coldest month (°C).
    TminCold,
    /// Mean daily maximum temperature of the warmest month (°C).
    TmaxWarm,
    /// Mean temperature of the coldest month (°C).
    TmeanCold,
    /// Mean temperature of the warmest month (°C).
    TmeanWarm,
    /// Annual precipitation (mm).
    Precip,
    /// Aridity index: annual precipitation / Thornthwaite potential
    /// evapotranspiration (dimensionless, capped at [`ARIDITY_CAP`];
    /// [`NODATA`] where PET is below [`ARIDITY_MIN_PET_MM`]).
    Aridity,
    /// Months with precipitation (mm) below twice the mean temperature (°C)
    /// (Gaussen), 0-12.
    DryMonths,
}

/// Largest stored aridity index (very wet cells).
pub const ARIDITY_CAP: f32 = 300.0;

/// Below this annual Thornthwaite PET (mm) the aridity index is not
/// defined and is stored as [`NODATA`]: a near-zero PET (ice sheets, high
/// Arctic) would make any precipitation read as "very wet".
pub const ARIDITY_MIN_PET_MM: f64 = 1.0;

/// Variables in band order.
pub const VARIABLES: [Variable; 8] = [
    Variable::Koppen,
    Variable::TminCold,
    Variable::TmaxWarm,
    Variable::TmeanCold,
    Variable::TmeanWarm,
    Variable::Precip,
    Variable::Aridity,
    Variable::DryMonths,
];

/// Static description of a variable.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct VariableInfo {
    /// Stable identifier (manifest, CSV columns).
    pub id: &'static str,
    /// Unit of the decoded value.
    pub unit: &'static str,
    /// Decoded value = stored * scale + offset.
    pub scale: f32,
    /// See `scale`.
    pub offset: f32,
    /// One-sentence description, including how it is derived.
    pub description: &'static str,
}

impl Variable {
    /// Description, unit and encoding.
    #[must_use]
    pub const fn info(self) -> VariableInfo {
        match self {
            Self::Koppen => VariableInfo {
                id: "koppen",
                unit: "class",
                scale: 1.0,
                offset: 0.0,
                description: "Köppen-Geiger class 1-30 (Beck et al. 2023 legend): the most frequent class of the 1-km map's 144 pixels in the cell.",
            },
            Self::TminCold => VariableInfo {
                id: "tmin_cold",
                unit: "°C",
                scale: 0.1,
                offset: 0.0,
                description: "Mean daily minimum temperature of the coldest month: CHELSA v2.1 bio6 (1981-2010, 1-km mean over the cell) moved to the slot's period by the change in Beck et al. 2023's coldest-month mean.",
            },
            Self::TmaxWarm => VariableInfo {
                id: "tmax_warm",
                unit: "°C",
                scale: 0.1,
                offset: 0.0,
                description: "Mean daily maximum temperature of the warmest month: CHELSA v2.1 bio5 (1981-2010, 1-km mean over the cell) moved to the slot's period by the change in Beck et al. 2023's warmest-month mean.",
            },
            Self::TmeanCold => VariableInfo {
                id: "tmean_cold",
                unit: "°C",
                scale: 0.1,
                offset: 0.0,
                description: "Mean temperature of the coldest month (Beck et al. 2023 monthly climatology, ensemble mean).",
            },
            Self::TmeanWarm => VariableInfo {
                id: "tmean_warm",
                unit: "°C",
                scale: 0.1,
                offset: 0.0,
                description: "Mean temperature of the warmest month (Beck et al. 2023 monthly climatology, ensemble mean).",
            },
            Self::Precip => VariableInfo {
                id: "precip",
                unit: "mm",
                scale: 1.0,
                offset: 0.0,
                description: "Annual precipitation (Beck et al. 2023 monthly climatology, ensemble mean).",
            },
            Self::Aridity => VariableInfo {
                id: "aridity",
                unit: "1",
                scale: 0.01,
                offset: 0.0,
                description: "Aridity index: annual precipitation divided by Thornthwaite potential evapotranspiration from the monthly means, capped at 300; no data where PET is below 1 mm/year (ice sheets, high Arctic), where the ratio is meaningless.",
            },
            Self::DryMonths => VariableInfo {
                id: "dry_months",
                unit: "months",
                scale: 1.0,
                offset: 0.0,
                description: "Months whose precipitation in mm is below twice their mean temperature in °C (Gaussen).",
            },
        }
    }

    /// Band-order index of this variable (its position in [`VARIABLES`]).
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Koppen => 0,
            Self::TminCold => 1,
            Self::TmaxWarm => 2,
            Self::TmeanCold => 3,
            Self::TmeanWarm => 4,
            Self::Precip => 5,
            Self::Aridity => 6,
            Self::DryMonths => 7,
        }
    }

    /// Encode a value; non-finite values become [`NODATA`], others are
    /// rounded and clamped to the `i16` range above [`NODATA`].
    #[must_use]
    pub fn encode(self, value: f32) -> i16 {
        let info = self.info();
        quantize(value, info.scale, info.offset)
    }

    /// Decode a stored value; [`NODATA`] becomes `None`. Decimal scales
    /// divide by their inverse, so -123 at scale 0.1 is exactly -12.3 (the
    /// nearest `f64`), which prints cleanly in JSON and JavaScript.
    #[must_use]
    pub fn decode(self, stored: i16) -> Option<f64> {
        let info = self.info();
        decode_value(stored, info.scale, info.offset)
    }
}

/// Decode a stored value at `scale` and `offset` (climate variables and
/// layer bands alike); [`NODATA`] becomes `None`. A decimal scale (one
/// whose inverse is an integer, e.g. 0.1, 0.01) divides by its inverse, so
/// -123 at scale 0.1 is exactly -12.3 (the nearest `f64`), which prints
/// cleanly in JSON and JavaScript; other scales multiply.
#[must_use]
pub fn decode_value(stored: i16, scale: f32, offset: f32) -> Option<f64> {
    if stored == NODATA {
        return None;
    }
    let inverse = (1.0 / f64::from(scale)).round();
    let q = f64::from(stored);
    let v = if (inverse * f64::from(scale) - 1.0).abs() < 1e-6 {
        q / inverse
    } else {
        q * f64::from(scale)
    };
    Some(v + f64::from(offset))
}

/// `round((value - offset) / scale)` clamped to `-32767..=32767`.
#[must_use]
pub fn quantize(value: f32, scale: f32, offset: f32) -> i16 {
    if !value.is_finite() {
        return NODATA;
    }
    ((value - offset) / scale).round().clamp(-32767.0, 32767.0) as i16
}

/// A climate period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Period {
    /// Reference climate 1991-2020.
    #[serde(rename = "1991-2020")]
    P1991_2020,
    /// Mid-century projection.
    #[serde(rename = "2041-2070")]
    P2041_2070,
    /// End-of-century projection.
    #[serde(rename = "2071-2099")]
    P2071_2099,
}

impl Period {
    /// Label, e.g. `1991-2020`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::P1991_2020 => "1991-2020",
            Self::P2041_2070 => "2041-2070",
            Self::P2071_2099 => "2071-2099",
        }
    }
}

/// An emission scenario (CMIP6 Shared Socioeconomic Pathway).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scenario {
    /// Observed reference climate (no scenario).
    Historical,
    /// SSP1-2.6.
    Ssp126,
    /// SSP2-4.5.
    Ssp245,
    /// SSP3-7.0.
    Ssp370,
    /// SSP5-8.5.
    Ssp585,
}

impl Scenario {
    /// Identifier, e.g. `ssp245`.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Historical => "historical",
            Self::Ssp126 => "ssp126",
            Self::Ssp245 => "ssp245",
            Self::Ssp370 => "ssp370",
            Self::Ssp585 => "ssp585",
        }
    }

    /// Display name, e.g. `SSP2-4.5`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Historical => "Historical",
            Self::Ssp126 => "SSP1-2.6",
            Self::Ssp245 => "SSP2-4.5",
            Self::Ssp370 => "SSP3-7.0",
            Self::Ssp585 => "SSP5-8.5",
        }
    }
}

/// A (period, scenario) pair stored in the tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct Slot {
    /// Period.
    pub period: Period,
    /// Scenario (`Historical` for 1991-2020).
    pub scenario: Scenario,
}

const fn slot(period: Period, scenario: Scenario) -> Slot {
    Slot { period, scenario }
}

/// Slots in band order.
pub const SLOTS: [Slot; 9] = [
    slot(Period::P1991_2020, Scenario::Historical),
    slot(Period::P2041_2070, Scenario::Ssp126),
    slot(Period::P2041_2070, Scenario::Ssp245),
    slot(Period::P2041_2070, Scenario::Ssp370),
    slot(Period::P2041_2070, Scenario::Ssp585),
    slot(Period::P2071_2099, Scenario::Ssp126),
    slot(Period::P2071_2099, Scenario::Ssp245),
    slot(Period::P2071_2099, Scenario::Ssp370),
    slot(Period::P2071_2099, Scenario::Ssp585),
];

/// Bands per tile.
pub const BANDS: usize = VARIABLES.len() * SLOTS.len();

/// 0-based band index of a variable in a slot.
#[must_use]
pub const fn band(var_index: usize, slot_index: usize) -> usize {
    var_index * SLOTS.len() + slot_index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_and_offset_round_trip() {
        assert_eq!(Variable::TminCold.encode(-12.34), -123);
        assert_eq!(Variable::TminCold.decode(-123), Some(-12.3));
        assert_eq!(
            Variable::TminCold.decode(-123).unwrap().to_string(),
            "-12.3"
        );
        assert_eq!(Variable::Aridity.encode(0.456), 46);
        assert_eq!(Variable::Aridity.decode(46), Some(0.46));
        assert_eq!(Variable::Precip.decode(812), Some(812.0));
        assert_eq!(Variable::Precip.encode(1234.5), 1235);
        assert_eq!(Variable::Koppen.encode(14.0), 14);
        for v in VARIABLES {
            assert_eq!(v.encode(f32::NAN), NODATA);
            assert_eq!(v.decode(NODATA), None);
            assert_eq!(v.encode(f32::INFINITY), NODATA);
        }
    }

    #[test]
    fn encoding_clamps_above_nodata() {
        assert_eq!(Variable::Precip.encode(1e9), 32767);
        assert_eq!(Variable::Precip.encode(-1e9), -32767);
        assert_eq!(Variable::Aridity.encode(ARIDITY_CAP), 30000);
        assert_eq!(quantize(-5.0, 0.5, 10.0), -30);
    }

    #[test]
    fn band_layout() {
        assert_eq!(BANDS, 72);
        assert_eq!(band(0, 0), 0);
        assert_eq!(band(1, 0), 9);
        assert_eq!(band(7, 8), 71);
        for (i, v) in VARIABLES.iter().enumerate() {
            assert_eq!(v.index(), i);
        }
        let ids: Vec<_> = VARIABLES.iter().map(|v| v.info().id).collect();
        assert_eq!(
            ids,
            [
                "koppen",
                "tmin_cold",
                "tmax_warm",
                "tmean_cold",
                "tmean_warm",
                "precip",
                "aridity",
                "dry_months"
            ]
        );
    }

    #[test]
    fn slots_cover_three_periods_and_four_scenarios() {
        assert_eq!(SLOTS[0].scenario, Scenario::Historical);
        assert_eq!(SLOTS[0].period.label(), "1991-2020");
        let future: Vec<_> = SLOTS[1..]
            .iter()
            .map(|s| format!("{} {}", s.period.label(), s.scenario.label()))
            .collect();
        assert_eq!(future.len(), 8);
        assert!(future.contains(&"2071-2099 SSP3-7.0".to_string()));
    }
}
