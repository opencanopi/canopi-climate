//! Soil layer: SoilGrids 2.0 pH (water), sand and clay, 0-30 cm, on the
//! 0.05-degree layer grid ([`crate::layer`]).
//!
//! Bands ([`SoilBand`]), all `int16`, nodata -32768:
//!
//! | band | stored | decoded |
//! |---|---|---|
//! | `ph_h2o` | pH x 10 | pH in water |
//! | `sand` | g/kg | percent (x 0.1) |
//! | `clay` | g/kg | percent (x 0.1) |
//! | `fill_distance` | cells | 0: measured; 1-[`SOIL_FILL_RADIUS`]: copied from the nearest measured cell that far away |
//!
//! Values are the depth-weighted mean of SoilGrids' 0-5, 5-15 and 15-30 cm
//! layers (weights 5, 10, 15). SoilGrids has no value on built-up areas,
//! water and ice: land cells within [`SOIL_FILL_RADIUS`] cells of a measured
//! cell take its values and record the distance in `fill_distance`.
//! Silt is not stored: it is 100 % minus sand and clay. The USDA texture
//! class is derived at lookup ([`texture_class`]).

use serde::Serialize;

use crate::grid::TileId;
use crate::layer::{self, Found, LAYER_LOOKUP_RADIUS, LayerTile};
use crate::lookup::LookupError;
use crate::model::NODATA;
use crate::tile::TileError;

/// Soil bands in band order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoilBand {
    /// pH in water x 10.
    PhH2o = 0,
    /// Sand, g/kg.
    Sand = 1,
    /// Clay, g/kg.
    Clay = 2,
    /// 0 measured, else distance (cells) to the measured cell copied.
    FillDistance = 3,
}

/// Bands per soil tile.
pub const SOIL_BANDS: usize = 4;

/// Largest fill distance in cells (0.4 degrees, about 44 km at the
/// equator): enough for the largest built-up areas, small enough not to
/// carry soil far into lakes, ice or across the sea.
pub const SOIL_FILL_RADIUS: usize = 8;

/// Depth interval of the stored values.
pub const SOIL_DEPTH: &str = "0-30 cm";

/// Default soil organic matter (% by weight) assumed by [`soil_awc`] when
/// site-specific SOC (`soil_plus` in v0.7.0) is not provided.
pub const DEFAULT_OM_PCT: f64 = 2.5;

/// Soil Available Water Capacity (AWC) derived from fine-earth texture via
/// Saxton & Rawls (2006, SSSAJ 70:1569–1578, Table 1), 0 bytes stored in the grid.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SoilAwc {
    /// Volumetric water content at field capacity (`-33 kPa`, m³/m³).
    pub field_capacity: f64,
    /// Volumetric water content at permanent wilting point (`-1500 kPa`, m³/m³).
    pub wilting_point: f64,
    /// Available water capacity per meter of soil depth (mm/m).
    pub awc_mm_per_m: f64,
    /// Available water capacity in the 0–30 cm topsoil layer (mm).
    pub awc_30cm_mm: f64,
    /// Extrapolated available water capacity in a 0–100 cm tree root zone (mm).
    pub awc_100cm_mm: f64,
}

impl SoilAwc {
    /// Estimated days of soil moisture buffer for a given evapotranspiration
    /// rate `et_mm_per_day` (mm/day) and effective rooting depth `root_depth_m` (m).
    /// Returns `None` if either argument is non-finite or `<= 0`.
    #[must_use]
    pub fn drought_buffer_days(&self, et_mm_per_day: f64, root_depth_m: f64) -> Option<f64> {
        if !et_mm_per_day.is_finite()
            || !root_depth_m.is_finite()
            || et_mm_per_day <= 0.0
            || root_depth_m <= 0.0
        {
            return None;
        }
        let days = (self.awc_mm_per_m * root_depth_m) / et_mm_per_day;
        Some((days * 10.0).round() / 10.0)
    }
}

/// Saxton & Rawls (2006) pedotransfer function for Available Water Capacity
/// from `sand_pct`, `clay_pct` (0–100, `sand + clay <= 100`), and optional
/// organic matter `om_pct` (defaults to [`DEFAULT_OM_PCT`] = `2.5%`).
///
/// Calibration clamps prevent negative wilting points on pure sand and
/// quadratic blow-up on heavy Vertisol clays (`clay <= 60%`, `OM <= 8%`,
/// `θ_1500 ∈ [0.01, 0.50]`, `θ_33 ∈ [θ_1500 + 0.03, 0.60]`, `AWC_v ∈ [0.03, 0.30]`).
/// Returns `None` for non-finite inputs, negative fractions, or `sand + clay > 100`.
#[must_use]
pub fn soil_awc(sand_pct: f64, clay_pct: f64, om_pct: Option<f64>) -> Option<SoilAwc> {
    let ok = |x: f64| x.is_finite() && (0.0..=100.0).contains(&x);
    if !ok(sand_pct) || !ok(clay_pct) || sand_pct + clay_pct > 100.0 + 1e-9 {
        return None;
    }
    let om_raw = om_pct.unwrap_or(DEFAULT_OM_PCT);
    if !om_raw.is_finite() || om_raw < 0.0 {
        return None;
    }
    let s = sand_pct / 100.0;
    let c = (clay_pct / 100.0).min(0.60);
    let om = om_raw.clamp(0.0, 8.0);

    let t1500t = -0.024 * s + 0.487 * c + 0.006 * om + 0.005 * s * om - 0.013 * c * om
        + 0.068 * s * c
        + 0.031;
    let t1500 = (t1500t + (0.14 * t1500t - 0.02)).clamp(0.01, 0.50);

    let t33t = -0.251 * s + 0.195 * c + 0.011 * om + 0.006 * s * om - 0.027 * c * om
        + 0.452 * s * c
        + 0.299;
    let t33 = (t33t + (1.283 * t33t * t33t - 0.374 * t33t - 0.015)).clamp(t1500 + 0.03, 0.60);

    let awc_v = (t33 - t1500).clamp(0.03, 0.30);
    let round3 = |x: f64| (x * 1000.0).round() / 1000.0;
    let round1 = |x: f64| (x * 10.0).round() / 10.0;
    Some(SoilAwc {
        field_capacity: round3(t33),
        wilting_point: round3(t1500),
        awc_mm_per_m: round1(awc_v * 1000.0),
        awc_30cm_mm: round1(awc_v * 300.0),
        awc_100cm_mm: round1(awc_v * 1000.0),
    })
}

/// Saxton & Rawls (2006) AWC with optional coarse-fragment rock discounts
/// (`cfvo_0_30_pct` for 0–30 cm topsoil and `cfvo_30_100_pct` for 30–100 cm
/// subsoil, clamped to `0..=95%`).
#[must_use]
pub fn soil_awc_with_rock(
    sand_pct: f64,
    clay_pct: f64,
    om_pct: Option<f64>,
    cfvo_0_30_pct: Option<f64>,
    cfvo_30_100_pct: Option<f64>,
) -> Option<SoilAwc> {
    let base = soil_awc(sand_pct, clay_pct, om_pct)?;
    let rock = |v: Option<f64>| -> Option<f64> {
        let x = v.unwrap_or(0.0);
        (x.is_finite() && (0.0..=100.0).contains(&x)).then(|| 1.0 - x.clamp(0.0, 95.0) / 100.0)
    };
    let f_top = rock(cfvo_0_30_pct)?;
    let f_sub = rock(cfvo_30_100_pct.or(cfvo_0_30_pct))?;
    let round1 = |x: f64| (x * 10.0).round() / 10.0;
    let awc_30 = base.awc_30cm_mm * f_top;
    let awc_70 = (base.awc_mm_per_m * 0.7) * f_sub;
    let awc_100 = awc_30 + awc_70;
    Some(SoilAwc {
        field_capacity: base.field_capacity,
        wilting_point: base.wilting_point,
        awc_mm_per_m: round1(awc_100),
        awc_30cm_mm: round1(awc_30),
        awc_100cm_mm: round1(awc_100),
    })
}

/// USDA soil texture classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TextureClass {
    /// Sand.
    Sand,
    /// Loamy sand.
    LoamySand,
    /// Sandy loam.
    SandyLoam,
    /// Loam.
    Loam,
    /// Silt loam.
    SiltLoam,
    /// Silt.
    Silt,
    /// Sandy clay loam.
    SandyClayLoam,
    /// Clay loam.
    ClayLoam,
    /// Silty clay loam.
    SiltyClayLoam,
    /// Sandy clay.
    SandyClay,
    /// Silty clay.
    SiltyClay,
    /// Clay.
    Clay,
}

impl TextureClass {
    /// Identifier, e.g. `silty_clay_loam` (names come from the site's i18n).
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Sand => "sand",
            Self::LoamySand => "loamy_sand",
            Self::SandyLoam => "sandy_loam",
            Self::Loam => "loam",
            Self::SiltLoam => "silt_loam",
            Self::Silt => "silt",
            Self::SandyClayLoam => "sandy_clay_loam",
            Self::ClayLoam => "clay_loam",
            Self::SiltyClayLoam => "silty_clay_loam",
            Self::SandyClay => "sandy_clay",
            Self::SiltyClay => "silty_clay",
            Self::Clay => "clay",
        }
    }
}

/// USDA texture class of a fine-earth fraction (percent; silt = 100 -
/// sand - clay). Boundaries of the USDA NRCS texture triangle (Soil
/// Survey Manual, 2017), in the order of the NRCS texture calculator.
/// `None` for a non-finite input or fractions outside 0-100.
#[must_use]
pub fn texture_class(sand_pct: f64, clay_pct: f64) -> Option<TextureClass> {
    use TextureClass as T;
    let ok = |x: f64| x.is_finite() && (0.0..=100.0).contains(&x);
    if !ok(sand_pct) || !ok(clay_pct) || sand_pct + clay_pct > 100.0 + 1e-9 {
        return None;
    }
    let (sand, clay) = (sand_pct, clay_pct);
    let silt = (100.0 - sand - clay).max(0.0);
    let class = if silt + 1.5 * clay < 15.0 {
        T::Sand
    } else if silt + 2.0 * clay < 30.0 {
        T::LoamySand
    } else if ((7.0..20.0).contains(&clay) && sand > 52.0) || (clay < 7.0 && silt < 50.0) {
        T::SandyLoam
    } else if (7.0..27.0).contains(&clay) && (28.0..50.0).contains(&silt) && sand <= 52.0 {
        T::Loam
    } else if (silt >= 50.0 && (12.0..27.0).contains(&clay))
        || ((50.0..80.0).contains(&silt) && clay < 12.0)
    {
        T::SiltLoam
    } else if silt >= 80.0 && clay < 12.0 {
        T::Silt
    } else if (20.0..35.0).contains(&clay) && silt < 28.0 && sand > 45.0 {
        T::SandyClayLoam
    } else if (27.0..40.0).contains(&clay) && sand > 20.0 && sand <= 45.0 {
        T::ClayLoam
    } else if (27.0..40.0).contains(&clay) && sand <= 20.0 {
        T::SiltyClayLoam
    } else if clay >= 35.0 && sand > 45.0 {
        T::SandyClay
    } else if clay >= 40.0 && silt >= 40.0 {
        T::SiltyClay
    } else {
        T::Clay
    };
    Some(class)
}

/// Soil at a location.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SoilLookup {
    /// Queried latitude.
    pub lat: f64,
    /// Queried longitude.
    pub lon: f64,
    /// Centre of the cell used.
    pub cell_lat: f64,
    /// Centre of the cell used.
    pub cell_lon: f64,
    /// Distance from the point's cell to the cell used (lookup search),
    /// in cells; 0 when the point's own cell has a value.
    pub distance_cells: f64,
    /// Tile name.
    pub tile: String,
    /// Depth interval, `0-30 cm`.
    pub depth: &'static str,
    /// pH in water.
    pub ph_h2o: f64,
    /// Sand, percent of the fine earth.
    pub sand_pct: f64,
    /// Clay, percent of the fine earth.
    pub clay_pct: f64,
    /// Silt, 100 - sand - clay (percent).
    pub silt_pct: f64,
    /// USDA texture class id, e.g. `silt_loam`.
    pub texture: Option<&'static str>,
    /// `true` when the cell had no SoilGrids value (built-up area, water,
    /// ice) and took the values of the nearest measured cell at build time.
    pub filled: bool,
    /// Distance (cells) to that measured cell; 0 when measured.
    pub fill_distance_cells: u8,
}

/// Whether a soil cell has values (measured or filled).
#[must_use]
pub fn has_soil(tile: &LayerTile, index: usize) -> bool {
    [SoilBand::PhH2o, SoilBand::Sand, SoilBand::Clay]
        .iter()
        .all(|b| tile.get(index, *b as usize) != NODATA)
}

/// Decode a soil tile file.
///
/// # Errors
/// [`TileError`] when the bytes are not a soil tile.
pub fn decode(bytes: &[u8]) -> Result<LayerTile, TileError> {
    LayerTile::decode(bytes, SOIL_BANDS)
}

fn tenth(stored: i16) -> f64 {
    f64::from(stored) / 10.0
}

fn build(found: &Found<'_>, lat: f64, lon: f64) -> SoilLookup {
    let i = found.cell.index_in_tile();
    let t = found.tile;
    let ph = tenth(t.get(i, SoilBand::PhH2o as usize));
    let sand = tenth(t.get(i, SoilBand::Sand as usize));
    let clay = tenth(t.get(i, SoilBand::Clay as usize));
    let silt = (((100.0 - sand - clay).max(0.0)) * 10.0).round() / 10.0;
    let fill = t.get(i, SoilBand::FillDistance as usize);
    let fill = u8::try_from(fill.max(0)).unwrap_or(u8::MAX);
    SoilLookup {
        lat,
        lon,
        cell_lat: layer::round6(found.cell.center_lat()),
        cell_lon: layer::round6(found.cell.center_lon()),
        distance_cells: found.distance_cells,
        tile: t.id().name(),
        depth: SOIL_DEPTH,
        ph_h2o: ph,
        sand_pct: sand,
        clay_pct: clay,
        silt_pct: silt,
        texture: texture_class(sand, clay).map(TextureClass::id),
        filled: fill > 0,
        fill_distance_cells: fill,
    }
}

/// Soil at a point from its tile (the point's cell, else the nearest cell
/// with values within [`LAYER_LOOKUP_RADIUS`] cells in the tile). `None`:
/// no soil value near (sea, ice sheets, or a gap wider than the fill).
///
/// # Errors
/// [`LookupError`] for an invalid coordinate or a point in another tile.
pub fn lookup(tile: &LayerTile, lat: f64, lon: f64) -> Result<Option<SoilLookup>, LookupError> {
    let found = layer::nearest_in_tile(tile, lat, lon, LAYER_LOOKUP_RADIUS, has_soil)?;
    Ok(found.map(|f| build(&f, lat, lon)))
}

/// Like [`lookup`], searching neighbouring tiles too (native reader).
///
/// # Errors
/// [`LookupError::InvalidCoordinate`].
pub fn lookup_across<'a>(
    tiles: impl Fn(TileId) -> Option<&'a LayerTile>,
    lat: f64,
    lon: f64,
    radius: usize,
) -> Result<Option<SoilLookup>, LookupError> {
    let found = layer::nearest(tiles, lat, lon, radius, has_soil)?;
    Ok(found.map(|f| build(&f, lat, lon)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::{LAYER_TILE_LEN, LayerCell};

    #[test]
    fn texture_classes_at_triangle_reference_points() {
        use TextureClass as T;
        // (sand, clay) -> class; centres of the USDA triangle's classes.
        let cases = [
            (92.0, 3.0, T::Sand),
            (82.0, 6.0, T::LoamySand),
            (65.0, 10.0, T::SandyLoam),
            (40.0, 20.0, T::Loam),
            (20.0, 15.0, T::SiltLoam),
            (7.0, 5.0, T::Silt),
            (60.0, 27.0, T::SandyClayLoam),
            (33.0, 34.0, T::ClayLoam),
            (10.0, 34.0, T::SiltyClayLoam),
            (52.0, 42.0, T::SandyClay),
            (7.0, 47.0, T::SiltyClay),
            (20.0, 60.0, T::Clay),
        ];
        for (sand, clay, want) in cases {
            assert_eq!(texture_class(sand, clay), Some(want), "{sand} {clay}");
        }
        // Boundaries: 40 % clay, 20 % sand, 40 % silt is clay (silty clay
        // needs silt >= 40 and clay >= 40: 40/40/20 -> silty clay).
        assert_eq!(texture_class(20.0, 40.0), Some(T::SiltyClay));
        assert_eq!(texture_class(21.0, 40.0), Some(T::Clay));
        assert_eq!(texture_class(52.0, 7.0), Some(T::Loam));
        assert_eq!(texture_class(52.1, 7.0), Some(T::SandyLoam));
        assert_eq!(texture_class(f64::NAN, 10.0), None);
        assert_eq!(texture_class(70.0, 40.0), None);
        assert_eq!(texture_class(-1.0, 10.0), None);
        assert_eq!(T::SiltyClayLoam.id(), "silty_clay_loam");
    }

    fn tile() -> (LayerTile, LayerCell, LayerCell) {
        let id = TileId::parse("n50e000").unwrap();
        let measured = LayerCell::of(48.3, 1.75).unwrap();
        let filled = LayerCell::of(48.85, 2.35).unwrap();
        let mut data = vec![NODATA; LAYER_TILE_LEN * SOIL_BANDS];
        for (cell, values) in [(measured, [68, 107, 230, 0]), (filled, [72, 150, 260, 3])] {
            let i = cell.index_in_tile();
            data[i * SOIL_BANDS..(i + 1) * SOIL_BANDS].copy_from_slice(&values);
        }
        (
            LayerTile::new(id, SOIL_BANDS, data).unwrap(),
            measured,
            filled,
        )
    }

    #[test]
    fn lookup_decodes_values_and_fill_flag() {
        let (t, measured, _) = tile();
        let r = lookup(&t, 48.3, 1.75).unwrap().unwrap();
        assert_eq!(
            (r.ph_h2o, r.sand_pct, r.clay_pct, r.silt_pct),
            (6.8, 10.7, 23.0, 66.3)
        );
        assert_eq!(r.texture, Some("silt_loam"));
        assert!(!r.filled);
        assert_eq!((r.distance_cells, r.tile.as_str()), (0.0, "n50e000"));
        assert!((r.cell_lat - measured.center_lat()).abs() < 1e-9);
        let r = lookup(&t, 48.85, 2.35).unwrap().unwrap();
        assert!(r.filled);
        assert_eq!(r.fill_distance_cells, 3);
        assert_eq!(r.ph_h2o, 7.2);
        // Nothing within 2 cells.
        assert!(lookup(&t, 45.0, 5.0).unwrap().is_none());
        assert!(lookup(&t, 30.0, 5.0).is_err());
    }

    #[test]
    fn decode_rejects_wrong_band_counts() {
        assert!(decode(b"nope").is_err());
    }

    #[test]
    fn saxton_rawls_awc_handles_sand_loam_vertisol_and_bounds() {
        // Pure sand with 0% OM must stay non-negative (wilting_point clamped >= 0.01)
        let sand = soil_awc(100.0, 0.0, Some(0.0)).unwrap();
        assert!(sand.wilting_point >= 0.01);
        assert!(sand.field_capacity > sand.wilting_point);
        assert!(sand.awc_30cm_mm >= 9.0 && sand.awc_30cm_mm <= 40.0);

        // Loam (40% sand, 20% clay, default 2.5% OM) ~ 40-55 mm / 30 cm
        let loam = soil_awc(40.0, 20.0, None).unwrap();
        assert!(loam.awc_30cm_mm >= 35.0 && loam.awc_30cm_mm <= 60.0);
        assert!((loam.awc_100cm_mm - loam.awc_mm_per_m).abs() < 1e-9);
        let buf = loam.drought_buffer_days(5.0, 0.6).unwrap();
        assert!(buf > 10.0 && buf < 30.0);
        assert_eq!(loam.drought_buffer_days(0.0, 0.6), None);

        // Heavy Vertisol clay (10% sand, 75% clay) must clamp cleanly without quadratic blow-up
        let vert = soil_awc(10.0, 75.0, Some(12.0)).unwrap();
        assert!(vert.field_capacity <= 0.60);
        assert!(vert.wilting_point <= 0.50);
        assert!(vert.awc_mm_per_m >= 30.0 && vert.awc_mm_per_m <= 300.0);

        // Invalid domain
        assert_eq!(soil_awc(60.0, 50.0, None), None);
        assert_eq!(soil_awc(-1.0, 20.0, None), None);
        assert_eq!(soil_awc(40.0, 20.0, Some(-1.0)), None);
        assert_eq!(soil_awc(f64::NAN, 20.0, None), None);
    }
}
