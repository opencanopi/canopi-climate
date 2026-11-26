//! Regions layer: the WGSRPD level-3 botanical area and the country (ISO
//! 3166-1 alpha-2) of each 0.05-degree layer cell ([`crate::layer`]).
//!
//! Two `int16` bands hold the codes themselves, so a tile needs no palette:
//!
//! | band | code | stored |
//! |---|---|---|
//! | `wgsrpd_l3` | three letters, e.g. `FRA` | `a * 676 + b * 26 + c` (letters `A` = 0 ... `Z` = 25), 0-17575 |
//! | `iso_a2` | two letters, e.g. `FR` | `a * 26 + b`, 0-675 |
//!
//! -32768: no area (sea, or a cell whose centre falls outside every
//! polygon). A cell takes the polygon containing its centre; areas too
//! small to contain a centre keep at least the cell of an interior point.

use serde::Serialize;

use crate::grid::TileId;
use crate::layer::{self, Found, LAYER_LOOKUP_RADIUS, LayerTile};
use crate::lookup::LookupError;
use crate::model::NODATA;
use crate::tile::TileError;

/// Regions bands in band order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionBand {
    /// WGSRPD level-3 code.
    WgsrpdL3 = 0,
    /// ISO 3166-1 alpha-2 country code (Natural Earth admin-0).
    IsoA2 = 1,
}

/// Bands per regions tile.
pub const REGION_BANDS: usize = 2;

/// Encode an upper-case ASCII code of 1-3 letters (base 26), `None` for
/// anything else.
#[must_use]
pub fn encode_code(code: &str) -> Option<i16> {
    if code.is_empty() || code.len() > 3 || !code.bytes().all(|b| b.is_ascii_uppercase()) {
        return None;
    }
    let v = code
        .bytes()
        .fold(0i32, |acc, b| acc * 26 + i32::from(b - b'A'));
    i16::try_from(v).ok()
}

/// Decode a stored code of `letters` letters; `None` for [`NODATA`],
/// `letters == 0`, overflow, or an out-of-range value.
#[must_use]
pub fn decode_code(stored: i16, letters: u32) -> Option<String> {
    if letters == 0 {
        return None;
    }
    let max = 26i32.checked_pow(letters)?;
    let mut v = i32::from(stored);
    if stored == NODATA || v < 0 || v >= max {
        return None;
    }
    let mut out = vec![b'A'; letters as usize];
    for slot in out.iter_mut().rev() {
        let digit = u8::try_from(v % 26).ok()?;
        *slot = b'A' + digit;
        v /= 26;
    }
    String::from_utf8(out).ok()
}

/// Regions at a location.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RegionsLookup {
    /// WGSRPD level-3 code (e.g. `FRA`), `None` when no area is near.
    pub wgsrpd_l3: Option<String>,
    /// ISO 3166-1 alpha-2 code (e.g. `FR`), `None` when no country is near
    /// or Natural Earth gives none (Somaliland, Northern Cyprus and the
    /// Siachen Glacier take their de jure country, see the manifest).
    pub iso_a2: Option<String>,
    /// Distance (cells) from the point's cell to the cell of each code
    /// found; 0 when the point's own cell has it.
    pub wgsrpd_l3_distance_cells: Option<f64>,
    /// See `wgsrpd_l3_distance_cells`.
    pub iso_a2_distance_cells: Option<f64>,
    /// Tile name.
    pub tile: String,
}

/// Decode a regions tile file.
///
/// # Errors
/// [`TileError`] when the bytes are not a regions tile.
pub fn decode(bytes: &[u8]) -> Result<LayerTile, TileError> {
    LayerTile::decode(bytes, REGION_BANDS)
}

fn has(band: RegionBand) -> impl Fn(&LayerTile, usize) -> bool {
    let letters = match band {
        RegionBand::WgsrpdL3 => 3,
        RegionBand::IsoA2 => 2,
    };
    move |t, i| decode_code(t.get(i, band as usize), letters).is_some()
}

fn code(found: Option<&Found<'_>>, band: RegionBand, letters: u32) -> Option<String> {
    found.and_then(|f| decode_code(f.tile.get(f.cell.index_in_tile(), band as usize), letters))
}

fn build(l3: Option<Found<'_>>, a2: Option<Found<'_>>, tile: TileId) -> RegionsLookup {
    RegionsLookup {
        wgsrpd_l3: code(l3.as_ref(), RegionBand::WgsrpdL3, 3),
        iso_a2: code(a2.as_ref(), RegionBand::IsoA2, 2),
        wgsrpd_l3_distance_cells: l3.map(|f| f.distance_cells),
        iso_a2_distance_cells: a2.map(|f| f.distance_cells),
        tile: tile.name(),
    }
}

/// Regions at a point from its tile: each code from the point's cell, else
/// from the nearest cell having it within [`LAYER_LOOKUP_RADIUS`] cells in
/// the tile (coastal points).
///
/// # Errors
/// [`LookupError`] for an invalid coordinate or a point in another tile.
pub fn lookup(tile: &LayerTile, lat: f64, lon: f64) -> Result<RegionsLookup, LookupError> {
    let r = LAYER_LOOKUP_RADIUS;
    let l3 = layer::nearest_in_tile(tile, lat, lon, r, has(RegionBand::WgsrpdL3))?;
    let a2 = layer::nearest_in_tile(tile, lat, lon, r, has(RegionBand::IsoA2))?;
    Ok(build(l3, a2, tile.id()))
}

/// Like [`lookup`], searching neighbouring tiles too (native reader).
///
/// # Errors
/// [`LookupError::InvalidCoordinate`].
pub fn lookup_across<'a>(
    tiles: impl Fn(TileId) -> Option<&'a LayerTile> + Copy,
    lat: f64,
    lon: f64,
    radius: usize,
) -> Result<RegionsLookup, LookupError> {
    let tile = TileId::of(lat, lon).ok_or(LookupError::InvalidCoordinate)?;
    let l3 = layer::nearest(tiles, lat, lon, radius, has(RegionBand::WgsrpdL3))?;
    let a2 = layer::nearest(tiles, lat, lon, radius, has(RegionBand::IsoA2))?;
    Ok(build(l3, a2, tile))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::{LAYER_TILE_LEN, LayerCell};

    #[test]
    fn codes_round_trip() {
        for c in ["FRA", "AAA", "ZZZ", "ABT", "NWG"] {
            let v = encode_code(c).unwrap();
            assert!((0..17_576).contains(&v));
            assert_eq!(decode_code(v, 3).as_deref(), Some(c));
        }
        for c in ["FR", "AA", "ZZ", "XK"] {
            let v = encode_code(c).unwrap();
            assert!((0..676).contains(&v));
            assert_eq!(decode_code(v, 2).as_deref(), Some(c));
        }
        assert_eq!(encode_code("FRA"), Some(5 * 676 + 17 * 26));
        for bad in ["", "fr", "-99", "ABCD", "F1", "CN-TW"] {
            assert_eq!(encode_code(bad), None, "{bad}");
        }
        assert_eq!(decode_code(NODATA, 2), None);
        assert_eq!(decode_code(676, 2), None);
        assert_eq!(decode_code(-1, 3), None);
        // BUG-13: letters == 0 and letters >= 7 must return None without panicking.
        assert_eq!(decode_code(0, 0), None);
        assert_eq!(decode_code(0, 7), None);
        assert_eq!(decode_code(0, 100), None);
    }

    #[test]
    fn lookup_reads_both_codes_and_searches_the_coast() {
        let id = TileId::parse("n50e000").unwrap();
        let mut data = vec![NODATA; LAYER_TILE_LEN * REGION_BANDS];
        let land = LayerCell::of(43.6, 3.9).unwrap().index_in_tile();
        data[land * 2] = encode_code("FRA").unwrap();
        data[land * 2 + 1] = encode_code("FR").unwrap();
        let t = LayerTile::new(id, REGION_BANDS, data).unwrap();
        let r = lookup(&t, 43.6, 3.9).unwrap();
        assert_eq!(r.wgsrpd_l3.as_deref(), Some("FRA"));
        assert_eq!(r.iso_a2.as_deref(), Some("FR"));
        assert_eq!(r.iso_a2_distance_cells, Some(0.0));
        // One cell south, at sea: the coast search finds the land cell.
        let r = lookup(&t, 43.55, 3.9).unwrap();
        assert_eq!(r.iso_a2.as_deref(), Some("FR"));
        assert_eq!(r.wgsrpd_l3_distance_cells, Some(1.0));
        // Far at sea: nothing.
        let r = lookup(&t, 41.0, 5.0).unwrap();
        assert_eq!((r.wgsrpd_l3, r.iso_a2), (None, None));
        assert!(lookup(&t, 35.0, 5.0).is_err());

        // BUG-13: corrupt out-of-range non-NODATA iso_a2 does not block coastal search.
        let mut corrupt = vec![NODATA; LAYER_TILE_LEN * REGION_BANDS];
        let valid_cell = LayerCell::of(43.6, 3.9).unwrap().index_in_tile();
        let bad_cell = LayerCell::of(43.55, 3.9).unwrap().index_in_tile();
        corrupt[valid_cell * 2 + 1] = encode_code("FR").unwrap();
        corrupt[bad_cell * 2 + 1] = 1000; // >= 676: invalid ISO-A2 code
        let ct = LayerTile::new(id, REGION_BANDS, corrupt).unwrap();
        let cr = lookup(&ct, 43.55, 3.9).unwrap();
        assert_eq!(cr.iso_a2.as_deref(), Some("FR"));
        assert_eq!(cr.iso_a2_distance_cells, Some(1.0));
    }
}
