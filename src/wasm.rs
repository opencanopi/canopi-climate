//! WebAssembly bindings (`wasm` feature). The host fetches the tile file
//! itself (whole file), then calls [`lookup_js`], [`lookup_soil_js`],
//! [`lookup_regions_js`] or [`lookup_extremes_js`]. The climate, soil,
//! regions and extremes tiles of a point share one name, in four
//! directories.
//!
//! ```js
//! import init, { tileName, lookup, lookupSoil, lookupRegions, lookupExtremes } from "./canopi_climate.js";
//! const name = tileName(lat, lon);              // "n50e000.tif", or undefined
//! const get = async (dir) => {
//!   const res = await fetch(`/data/climate/${dir}/${name}`);  // 404: no data in the tile
//!   return res.ok ? new Uint8Array(await res.arrayBuffer()) : null;
//! };
//! const tiles = await get("tiles"), soilTile = await get("soil"), regionsTile = await get("regions");
//! // The exports take bytes: guard the missing tiles (no data there).
//! const climate = tiles ? lookup(tiles, lat, lon) : null;            // object, or null at sea
//! const soil = soilTile ? lookupSoil(soilTile, lat, lon) : null;     // object, or null
//! const regions = regionsTile ? lookupRegions(regionsTile, lat, lon) : null; // { wgsrpd_l3, iso_a2, ... }
//! // Extremes: read at the climate lookup's own cell (in the point's tile for a single-tile reader).
//! const extTile = climate ? await get("extremes") : null;
//! const extremes = extTile ? lookupExtremes(extTile, climate.cell_lat, climate.cell_lon) : null;
//! ```

use wasm_bindgen::prelude::{JsError, JsValue, wasm_bindgen};

use crate::grid::TileId;
use crate::tile::Tile;

/// File name of the tile holding a point (e.g. `n50e000.tif`); `undefined`
/// for an invalid coordinate.
#[wasm_bindgen(js_name = tileName)]
#[must_use]
pub fn tile_name(lat: f64, lon: f64) -> Option<String> {
    TileId::of(lat, lon).map(TileId::file_name)
}

/// Decode a tile and look a point up (nearest complete cell within 2
/// cells). Returns the [`crate::lookup::Lookup`] as a plain object, or `null`
/// when no land cell with data is near.
///
/// # Errors
/// Throws when the bytes are not a grid tile, the coordinate is invalid or
/// the point lies in another tile.
#[wasm_bindgen(js_name = lookup)]
pub fn lookup_js(tile_bytes: &[u8], lat: f64, lon: f64) -> Result<JsValue, JsError> {
    let tile = Tile::decode(tile_bytes).map_err(|e| JsError::new(&e.to_string()))?;
    match crate::lookup::lookup(&tile, lat, lon).map_err(|e| JsError::new(&e.to_string()))? {
        Some(found) => to_js(&found),
        None => Ok(JsValue::NULL),
    }
}

fn to_js<T: serde::Serialize>(value: &T) -> Result<JsValue, JsError> {
    let ser = serde_wasm_bindgen::Serializer::json_compatible();
    value
        .serialize(&ser)
        .map_err(|e| JsError::new(&e.to_string()))
}

/// Decode a soil tile and look a point up (its cell, else the nearest cell
/// with soil values within 2 cells). Returns the
/// [`crate::soil::SoilLookup`] as a plain object (`ph_h2o`, `sand_pct`,
/// `clay_pct`, `silt_pct`, `texture`, `filled`, ...), or `null`.
///
/// # Errors
/// Throws when the bytes are not a soil tile, the coordinate is invalid or
/// the point lies in another tile.
#[wasm_bindgen(js_name = lookupSoil)]
pub fn lookup_soil_js(tile_bytes: &[u8], lat: f64, lon: f64) -> Result<JsValue, JsError> {
    let tile = crate::soil::decode(tile_bytes).map_err(|e| JsError::new(&e.to_string()))?;
    match crate::soil::lookup(&tile, lat, lon).map_err(|e| JsError::new(&e.to_string()))? {
        Some(found) => to_js(&found),
        None => Ok(JsValue::NULL),
    }
}

/// Decode a regions tile and look a point up. Returns the
/// [`crate::regions::RegionsLookup`] as a plain object: `wgsrpd_l3` (e.g.
/// `"FRA"`) and `iso_a2` (e.g. `"FR"`), each `null` when no area is within
/// 2 cells.
///
/// # Errors
/// Throws when the bytes are not a regions tile, the coordinate is invalid
/// or the point lies in another tile.
#[wasm_bindgen(js_name = lookupRegions)]
pub fn lookup_regions_js(tile_bytes: &[u8], lat: f64, lon: f64) -> Result<JsValue, JsError> {
    let tile = crate::regions::decode(tile_bytes).map_err(|e| JsError::new(&e.to_string()))?;
    let found =
        crate::regions::lookup(&tile, lat, lon).map_err(|e| JsError::new(&e.to_string()))?;
    to_js(&found)
}

/// USDA texture class id (e.g. `"silt_loam"`) of sand and clay
/// percentages; `undefined` for impossible fractions.
#[wasm_bindgen(js_name = textureClass)]
#[must_use]
pub fn texture_class_js(sand_pct: f64, clay_pct: f64) -> Option<String> {
    crate::soil::texture_class(sand_pct, clay_pct).map(|c| c.id().to_string())
}

/// Decode an extremes tile and read the cell holding (`cellLat`, `cellLon`):
/// pass the climate lookup's `cell_lat` and `cell_lon`. Returns the
/// [`crate::extremes::ExtremesLookup`] as a plain object (`periods[]` with
/// `tmin_extreme_c`, `tmax_extreme_c`, `usda_zone`), or `null` when that
/// cell has no extremes.
///
/// # Errors
/// Throws when the bytes are not an extremes tile, the coordinate is
/// invalid or the cell lies in another tile.
#[wasm_bindgen(js_name = lookupExtremes)]
pub fn lookup_extremes_js(
    tile_bytes: &[u8],
    cell_lat: f64,
    cell_lon: f64,
) -> Result<JsValue, JsError> {
    let tile = crate::extremes::ExtremesTile::decode(tile_bytes)
        .map_err(|e| JsError::new(&e.to_string()))?;
    match crate::extremes::at_cell(&tile, cell_lat, cell_lon)
        .map_err(|e| JsError::new(&e.to_string()))?
    {
        Some(found) => to_js(&found),
        None => Ok(JsValue::NULL),
    }
}

/// USDA hardiness zone (`1a` ... `13b`) of a coldest night in °C.
#[wasm_bindgen(js_name = usdaZone)]
#[must_use]
pub fn usda_zone_js(tmin_extreme_c: f64) -> String {
    crate::extremes::usda_zone(tmin_extreme_c)
}

/// Crate version, for logs and cache keys.
#[wasm_bindgen(js_name = version)]
#[must_use]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
