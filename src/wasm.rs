//! WebAssembly bindings (`wasm` feature). The host fetches the tile file
//! itself (whole file), then calls [`lookup_js`].
//!
//! ```js
//! import init, { tileName, lookup } from "./canopi_climate.js";
//! const name = tileName(lat, lon);              // "n50e000.tif", or undefined
//! const bytes = new Uint8Array(await (await fetch(`/data/climate/${name}`)).arrayBuffer());
//! const climate = lookup(bytes, lat, lon);      // object, or null at sea
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
        Some(found) => {
            let ser = serde_wasm_bindgen::Serializer::json_compatible();
            serde::Serialize::serialize(&found, &ser).map_err(|e| JsError::new(&e.to_string()))
        }
        None => Ok(JsValue::NULL),
    }
}

/// Crate version, for logs and cache keys.
#[wasm_bindgen(js_name = version)]
#[must_use]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
