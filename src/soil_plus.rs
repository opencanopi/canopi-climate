//! Deep soil layer (`soil_plus/`): SoilGrids 2.0 5-km organic carbon, bulk
//! density, coarse fragments (0–30 cm) and subsoil coarse fragments, pH and
//! clay (30–100 cm) on the 0.05-degree layer grid ([`crate::layer`]).
//!
//! Bands ([`SoilPlusBand`]), all `int16`, `NODATA = -32768`:
//!
//! | # | id | unit | scale | content |
//! |---|---|---|---|---|
//! | 0 | `soc_0_30` | g/kg | 0.5 | 0–30 cm soil organic carbon (`OM% = 0.1724 * soc`) |
//! | 1 | `bdod_0_30` | g/cm³ | 0.01 | 0–30 cm bulk density |
//! | 2 | `cfvo_0_30` | % | 1.0 | 0–30 cm coarse fragments (> 2 mm, vol %) |
//! | 3 | `cfvo_30_100` | % | 1.0 | 30–100 cm coarse fragments (> 2 mm, vol %) |
//! | 4 | `ph_30_100` | pH | 0.1 | 30–100 cm soil pH in water |
//! | 5 | `clay_30_100` | % | 1.0 | 30–100 cm clay content of the fine earth |
//!
//! Reuses `soil/`'s 8-cell land-only fill mask without duplicating
//! `fill_distance`. Method: `docs/decisions/0008-soil-plus-terrain-microclimate-packs.md`.

use serde::{Deserialize, Serialize};

use crate::grid::TileId;
use crate::layer::{self, Found, LAYER_LOOKUP_RADIUS, LayerTile};
use crate::lookup::LookupError;
use crate::model::{NODATA, decode_value, quantize};
use crate::soil::{SoilAwc, SoilLookup, soil_awc_with_rock};
use crate::tile::TileError;

/// Directory and layer id of `soil_plus`.
pub const DIR: &str = "soil_plus";

/// Number of bands per `soil_plus` tile.
pub const SOIL_PLUS_BANDS: usize = 6;

/// Van Bemmelen factor converting soil organic carbon (g/kg) to organic matter (%):
/// `OM% = (SOC_g_kg / 10) * 1.724 = 0.1724 * SOC_g_kg`.
pub const SOC_G_KG_TO_OM_PCT: f64 = 0.1724;

/// Bands of a `soil_plus` tile, in band order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoilPlusBand {
    /// 0–30 cm soil organic carbon (g/kg, scale 0.5).
    Soc0To30 = 0,
    /// 0–30 cm bulk density (g/cm³, scale 0.01).
    Bdod0To30 = 1,
    /// 0–30 cm coarse fragments (vol %, scale 1.0).
    Cfvo0To30 = 2,
    /// 30–100 cm coarse fragments (vol %, scale 1.0).
    Cfvo30To100 = 3,
    /// 30–100 cm pH in water (pH, scale 0.1).
    Ph30To100 = 4,
    /// 30–100 cm clay content (%, scale 1.0).
    Clay30To100 = 5,
}

/// All `soil_plus` bands in band order.
pub const SOIL_PLUS_BAND_LIST: [SoilPlusBand; SOIL_PLUS_BANDS] = [
    SoilPlusBand::Soc0To30,
    SoilPlusBand::Bdod0To30,
    SoilPlusBand::Cfvo0To30,
    SoilPlusBand::Cfvo30To100,
    SoilPlusBand::Ph30To100,
    SoilPlusBand::Clay30To100,
];

impl SoilPlusBand {
    /// 0-based band index.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Manifest band identifier.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Soc0To30 => "soc_0_30",
            Self::Bdod0To30 => "bdod_0_30",
            Self::Cfvo0To30 => "cfvo_0_30",
            Self::Cfvo30To100 => "cfvo_30_100",
            Self::Ph30To100 => "ph_30_100",
            Self::Clay30To100 => "clay_30_100",
        }
    }

    /// Physical unit after decoding.
    #[must_use]
    pub const fn unit(self) -> &'static str {
        match self {
            Self::Soc0To30 => "g/kg",
            Self::Bdod0To30 => "g/cm3",
            Self::Cfvo0To30 | Self::Cfvo30To100 | Self::Clay30To100 => "%",
            Self::Ph30To100 => "pH",
        }
    }

    /// Linear scale factor (`decoded = stored * scale`).
    #[must_use]
    pub const fn scale(self) -> f32 {
        match self {
            Self::Soc0To30 => 0.5,
            Self::Bdod0To30 => 0.01,
            Self::Cfvo0To30 | Self::Cfvo30To100 | Self::Clay30To100 => 1.0,
            Self::Ph30To100 => 0.1,
        }
    }

    /// Human-readable manifest description.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Soc0To30 => "0-30 cm soil organic carbon (g/kg; OM% = 0.1724 * soc_0_30)",
            Self::Bdod0To30 => "0-30 cm bulk density of the fine earth fraction (g/cm3)",
            Self::Cfvo0To30 => "0-30 cm coarse fragments (> 2 mm, volumetric %)",
            Self::Cfvo30To100 => "30-100 cm coarse fragments (> 2 mm, volumetric %)",
            Self::Ph30To100 => "30-100 cm soil pH in water (calcareous subsoil proxy)",
            Self::Clay30To100 => "30-100 cm clay content of the fine earth (%, claypan proxy)",
        }
    }

    /// Quantise a physical value in decoded units to `int16`.
    #[must_use]
    pub fn encode(self, value: f32) -> i16 {
        quantize(value, self.scale(), 0.0)
    }

    /// Decode a stored `int16` to physical units (`None` for `NODATA`).
    #[must_use]
    pub fn decode(self, stored: i16) -> Option<f64> {
        decode_value(stored, self.scale(), 0.0)
    }
}

/// Deep-soil lookup result at a location.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SoilPlusLookup {
    /// Queried latitude.
    pub lat: f64,
    /// Queried longitude.
    pub lon: f64,
    /// Centre latitude of the cell used.
    pub cell_lat: f64,
    /// Centre longitude of the cell used.
    pub cell_lon: f64,
    /// Search distance in 0.05-degree cells (0 when own cell has data).
    pub distance_cells: f64,
    /// Tile name.
    pub tile: String,
    /// 0–30 cm soil organic carbon (g/kg).
    pub soc_0_30_g_kg: f64,
    /// Derived 0–30 cm soil organic matter (% = `0.1724 * soc_0_30_g_kg`).
    pub om_0_30_pct: f64,
    /// 0–30 cm bulk density (g/cm³).
    pub bdod_0_30_g_cm3: f64,
    /// 0–30 cm coarse fragments (vol %).
    pub cfvo_0_30_pct: f64,
    /// 30–100 cm coarse fragments (vol %).
    pub cfvo_30_100_pct: f64,
    /// 30–100 cm soil pH in water.
    pub ph_30_100: f64,
    /// 30–100 cm clay content (%).
    pub clay_30_100_pct: f64,
}

impl SoilPlusLookup {
    /// Compute rock- and organic-matter-adjusted Saxton & Rawls (2006) AWC by
    /// pairing this `soil_plus` lookup with a `soil` texture lookup.
    #[must_use]
    pub fn awc_with_soil(&self, soil: &SoilLookup) -> Option<SoilAwc> {
        soil_awc_with_rock(
            soil.sand_pct,
            soil.clay_pct,
            Some(self.om_0_30_pct),
            Some(self.cfvo_0_30_pct),
            Some(self.cfvo_30_100_pct),
        )
    }
}

/// Whether a `soil_plus` cell has all 6 bands set.
#[must_use]
pub fn has_soil_plus(tile: &LayerTile, index: usize) -> bool {
    SOIL_PLUS_BAND_LIST
        .iter()
        .all(|b| tile.get(index, b.index()) != NODATA)
}

/// Decode a `soil_plus` tile file.
///
/// # Errors
/// [`TileError`] when the bytes are not a 6-band 0.05-degree layer tile.
pub fn decode(bytes: &[u8]) -> Result<LayerTile, TileError> {
    LayerTile::decode(bytes, SOIL_PLUS_BANDS)
}

fn build(found: &Found<'_>, lat: f64, lon: f64) -> Option<SoilPlusLookup> {
    let i = found.cell.index_in_tile();
    let t = found.tile;
    let soc = SoilPlusBand::Soc0To30.decode(t.get(i, SoilPlusBand::Soc0To30.index()))?;
    let bdod = SoilPlusBand::Bdod0To30.decode(t.get(i, SoilPlusBand::Bdod0To30.index()))?;
    let cfvo0 = SoilPlusBand::Cfvo0To30.decode(t.get(i, SoilPlusBand::Cfvo0To30.index()))?;
    let cfvo1 = SoilPlusBand::Cfvo30To100.decode(t.get(i, SoilPlusBand::Cfvo30To100.index()))?;
    let ph1 = SoilPlusBand::Ph30To100.decode(t.get(i, SoilPlusBand::Ph30To100.index()))?;
    let clay1 = SoilPlusBand::Clay30To100.decode(t.get(i, SoilPlusBand::Clay30To100.index()))?;
    let om = (soc * SOC_G_KG_TO_OM_PCT * 100.0).round() / 100.0;
    Some(SoilPlusLookup {
        lat,
        lon,
        cell_lat: layer::round6(found.cell.center_lat()),
        cell_lon: layer::round6(found.cell.center_lon()),
        distance_cells: found.distance_cells,
        tile: t.id().name(),
        soc_0_30_g_kg: soc,
        om_0_30_pct: om,
        bdod_0_30_g_cm3: bdod,
        cfvo_0_30_pct: cfvo0,
        cfvo_30_100_pct: cfvo1,
        ph_30_100: ph1,
        clay_30_100_pct: clay1,
    })
}

/// Deep soil at a point from its tile (own cell, else nearest valid cell within
/// [`LAYER_LOOKUP_RADIUS`] cells in the tile).
///
/// # Errors
/// [`LookupError`] for an invalid coordinate or a point in another tile.
pub fn lookup(tile: &LayerTile, lat: f64, lon: f64) -> Result<Option<SoilPlusLookup>, LookupError> {
    let found = layer::nearest_in_tile(tile, lat, lon, LAYER_LOOKUP_RADIUS, has_soil_plus)?;
    Ok(found.and_then(|f| build(&f, lat, lon)))
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
) -> Result<Option<SoilPlusLookup>, LookupError> {
    let found = layer::nearest(tiles, lat, lon, radius, has_soil_plus)?;
    Ok(found.and_then(|f| build(&f, lat, lon)))
}

/// Synthetic `soil_plus` tile matching `layer::synthetic::soil()` for tests.
#[cfg(test)]
pub mod synthetic {
    use super::*;
    use crate::layer::{LAYER_TILE_LEN, synthetic as layer_syn};

    /// Synthetic `soil_plus` tile for `n50e000` with identical land mask to `soil()`.
    #[must_use]
    pub fn soil_plus() -> LayerTile {
        let id = TileId::parse("n50e000").expect("valid tile id");
        let soil = layer_syn::soil();
        let mut data = vec![NODATA; LAYER_TILE_LEN * SOIL_PLUS_BANDS];
        for i in 0..LAYER_TILE_LEN {
            if !crate::soil::has_soil(&soil, i) {
                continue;
            }
            let base = i * SOIL_PLUS_BANDS;
            data[base + SoilPlusBand::Soc0To30.index()] = SoilPlusBand::Soc0To30.encode(24.5);
            data[base + SoilPlusBand::Bdod0To30.index()] = SoilPlusBand::Bdod0To30.encode(1.32);
            data[base + SoilPlusBand::Cfvo0To30.index()] = SoilPlusBand::Cfvo0To30.encode(8.0);
            data[base + SoilPlusBand::Cfvo30To100.index()] = SoilPlusBand::Cfvo30To100.encode(15.0);
            data[base + SoilPlusBand::Ph30To100.index()] = SoilPlusBand::Ph30To100.encode(7.4);
            data[base + SoilPlusBand::Clay30To100.index()] = SoilPlusBand::Clay30To100.encode(28.0);
        }
        LayerTile::new(id, SOIL_PLUS_BANDS, data).expect("valid synthetic soil_plus tile")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_decodes_and_integrates_with_soil_awc() {
        let sp_tile = synthetic::soil_plus();
        let soil_tile = crate::layer::synthetic::soil();
        let sp = lookup(&sp_tile, 45.25, 3.25)
            .unwrap()
            .expect("land cell has soil_plus");
        assert!((sp.soc_0_30_g_kg - 24.5).abs() < 1e-9);
        assert!((sp.om_0_30_pct - 4.22).abs() < 1e-9);
        assert!((sp.bdod_0_30_g_cm3 - 1.32).abs() < 1e-9);
        assert_eq!(sp.cfvo_0_30_pct, 8.0);
        assert_eq!(sp.cfvo_30_100_pct, 15.0);
        assert!((sp.ph_30_100 - 7.4).abs() < 1e-9);
        assert_eq!(sp.clay_30_100_pct, 28.0);

        let soil = crate::soil::lookup(&soil_tile, 45.25, 3.25)
            .unwrap()
            .expect("land cell has soil");
        let base_awc = crate::soil::soil_awc(soil.sand_pct, soil.clay_pct, None).unwrap();
        let plus_awc = sp.awc_with_soil(&soil).unwrap();
        assert!(plus_awc.awc_30cm_mm > 0.0 && plus_awc.awc_100cm_mm > plus_awc.awc_30cm_mm);
        // Rock fragments discount effective AWC relative to stone-free soil at the same OM.
        let stone_free =
            crate::soil::soil_awc(soil.sand_pct, soil.clay_pct, Some(sp.om_0_30_pct)).unwrap();
        assert!(plus_awc.awc_30cm_mm < stone_free.awc_30cm_mm);
        assert!(base_awc.awc_30cm_mm > 0.0);

        // Sea cell returns None.
        assert!(lookup(&sp_tile, 48.0, 1.0).unwrap().is_none());
    }
}
