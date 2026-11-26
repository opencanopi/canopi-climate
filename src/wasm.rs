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

/// USDA hardiness zone (`1a` ... `13b`) of a coldest night in °C, or
/// `undefined` for non-finite input.
#[wasm_bindgen(js_name = usdaZone)]
#[must_use]
pub fn usda_zone_js(tmin_extreme_c: f64) -> Option<String> {
    if !tmin_extreme_c.is_finite() {
        return None;
    }
    Some(crate::extremes::usda_zone(tmin_extreme_c))
}

/// Decode a `monthly` tile and read the cell holding (`cellLat`, `cellLon`):
/// pass the climate lookup's `cell_lat` and `cell_lon`. Returns the
/// [`crate::monthly::MonthlyLookup`] as a plain object (`periods[]` with
/// `tmean_c`, `tmin_c`, `dry_mask`, `dry_months`, `gdd_base_10`,
/// `chill_months`), or `null` when that cell has no data.
///
/// # Errors
/// Throws when the bytes are not a monthly tile, the coordinate is invalid
/// or the cell lies in another tile.
#[wasm_bindgen(js_name = lookupMonthly)]
pub fn lookup_monthly_js(
    tile_bytes: &[u8],
    cell_lat: f64,
    cell_lon: f64,
) -> Result<JsValue, JsError> {
    let tile = crate::monthly::MonthlyTile::decode(tile_bytes)
        .map_err(|e| JsError::new(&e.to_string()))?;
    match crate::monthly::at_cell(&tile, cell_lat, cell_lon)
        .map_err(|e| JsError::new(&e.to_string()))?
    {
        Some(found) => to_js(&found),
        None => Ok(JsValue::NULL),
    }
}

/// Decode a `monthly_precip` tile (0.25-degree grid) and look a point up
/// (own cell, else nearest valid cell within radius 1 for coastal fallback).
/// Returns the [`crate::monthly_precip::MonthlyPrecipLookup`] as a plain
/// object (`periods[]` with `precip_mm`), or `null`.
///
/// # Errors
/// Throws when the bytes are not a monthly_precip tile, the coordinate is
/// invalid or the point lies in another tile.
#[wasm_bindgen(js_name = lookupMonthlyPrecip)]
pub fn lookup_monthly_precip_js(tile_bytes: &[u8], lat: f64, lon: f64) -> Result<JsValue, JsError> {
    let tile = crate::monthly_precip::MonthlyPrecipTile::decode(tile_bytes)
        .map_err(|e| JsError::new(&e.to_string()))?;
    match crate::monthly_precip::lookup(&tile, lat, lon)
        .map_err(|e| JsError::new(&e.to_string()))?
    {
        Some(found) => to_js(&found),
        None => Ok(JsValue::NULL),
    }
}

/// Decode an `extremes_tail` tile and read the cell holding (`cellLat`, `cellLon`):
/// pass the climate lookup's `cell_lat` and `cell_lon`. Returns the
/// [`crate::extremes_tail::ExtremesTailLookup`] as a plain object, or `null`.
///
/// # Errors
/// Throws when the bytes are not an extremes_tail tile, the coordinate is
/// invalid or the cell lies in another tile.
#[wasm_bindgen(js_name = lookupExtremesTail)]
pub fn lookup_extremes_tail_js(
    tile_bytes: &[u8],
    cell_lat: f64,
    cell_lon: f64,
) -> Result<JsValue, JsError> {
    let tile = crate::extremes_tail::ExtremesTailTile::decode(tile_bytes)
        .map_err(|e| JsError::new(&e.to_string()))?;
    match crate::extremes_tail::at_cell(&tile, cell_lat, cell_lon)
        .map_err(|e| JsError::new(&e.to_string()))?
    {
        Some(found) => to_js(&found),
        None => Ok(JsValue::NULL),
    }
}

/// Decode a `frost_calendar` tile and read the cell holding (`cellLat`, `cellLon`):
/// pass the climate lookup's `cell_lat` and `cell_lon`. Returns the
/// [`crate::frost_calendar::FrostCalendarLookup`] as a plain object, or `null`.
///
/// # Errors
/// Throws when the bytes are not a frost_calendar tile, the coordinate is
/// invalid or the cell lies in another tile.
#[wasm_bindgen(js_name = lookupFrostCalendar)]
pub fn lookup_frost_calendar_js(
    tile_bytes: &[u8],
    cell_lat: f64,
    cell_lon: f64,
) -> Result<JsValue, JsError> {
    let tile = crate::frost_calendar::FrostCalendarTile::decode(tile_bytes)
        .map_err(|e| JsError::new(&e.to_string()))?;
    match crate::frost_calendar::at_cell(&tile, cell_lat, cell_lon)
        .map_err(|e| JsError::new(&e.to_string()))?
    {
        Some(found) => to_js(&found),
        None => Ok(JsValue::NULL),
    }
}

/// Search an `analogues.bin` binary index for the closest historical
/// (1991–2020) climate twins matching `query` (`AnalogueQuery`) with optional
/// `options` (`SearchOptions`). Returns an [`crate::analogues::AnalogueResult`]
/// plain object (`novel_climate`, `best_sigma`, `matches[]`).
///
/// # Errors
/// Throws when `indexBytes` is not a valid `analogues.bin` buffer or when
/// `query`/`options` cannot be deserialized.
#[wasm_bindgen(js_name = findAnalogues)]
pub fn find_analogues_js(
    index_bytes: &[u8],
    query: JsValue,
    options: JsValue,
) -> Result<JsValue, JsError> {
    let q: crate::analogues::AnalogueQuery =
        serde_wasm_bindgen::from_value(query).map_err(|e| JsError::new(&e.to_string()))?;
    let opts: crate::analogues::SearchOptions = if options.is_null() || options.is_undefined() {
        crate::analogues::SearchOptions::default()
    } else {
        serde_wasm_bindgen::from_value(options).map_err(|e| JsError::new(&e.to_string()))?
    };
    let result = crate::analogues::AnalogueIndex::search_bytes(index_bytes, &q, &opts)
        .map_err(|e| JsError::new(&e.to_string()))?;
    to_js(&result)
}

/// Decode a `phenology` tile and read the cell holding (`cellLat`, `cellLon`):
/// pass the climate lookup's `cell_lat` and `cell_lon`. Returns the
/// [`crate::phenology::PhenologyLookup`] as a plain object, or `null`.
///
/// # Errors
/// Throws when the bytes are not a phenology tile, the coordinate is invalid
/// or the cell lies in another tile.
#[wasm_bindgen(js_name = lookupPhenology)]
pub fn lookup_phenology_js(
    tile_bytes: &[u8],
    cell_lat: f64,
    cell_lon: f64,
) -> Result<JsValue, JsError> {
    let tile = crate::phenology::PhenologyTile::decode(tile_bytes)
        .map_err(|e| JsError::new(&e.to_string()))?;
    match crate::phenology::at_cell(&tile, cell_lat, cell_lon)
        .map_err(|e| JsError::new(&e.to_string()))?
    {
        Some(found) => to_js(&found),
        None => Ok(JsValue::NULL),
    }
}

/// Decode a `water_balance` tile and read the cell holding (`cellLat`, `cellLon`):
/// pass the climate lookup's `cell_lat` and `cell_lon`. Returns the
/// [`crate::water_balance::WaterBalanceLookup`] as a plain object, or `null`.
///
/// # Errors
/// Throws when the bytes are not a water_balance tile, the coordinate is invalid
/// or the cell lies in another tile.
#[wasm_bindgen(js_name = lookupWaterBalance)]
pub fn lookup_water_balance_js(
    tile_bytes: &[u8],
    cell_lat: f64,
    cell_lon: f64,
) -> Result<JsValue, JsError> {
    let tile = crate::water_balance::WaterBalanceTile::decode(tile_bytes)
        .map_err(|e| JsError::new(&e.to_string()))?;
    match crate::water_balance::at_cell(&tile, cell_lat, cell_lon)
        .map_err(|e| JsError::new(&e.to_string()))?
    {
        Some(found) => to_js(&found),
        None => Ok(JsValue::NULL),
    }
}

/// Compute annual FAO-56 geometric photoperiod summary for `lat` (`-90..=90`),
/// returning `{ min_hours, max_hours, amplitude_hours, monthly_hours,
/// polar_night_days, midnight_sun_days }` or `null` if invalid.
///
/// # Errors
/// Throws only on serialization failure.
#[wasm_bindgen(js_name = photoperiod)]
pub fn photoperiod_js(lat: f64) -> Result<JsValue, JsError> {
    match crate::derived::photoperiod(lat) {
        Some(p) => to_js(&p),
        None => Ok(JsValue::NULL),
    }
}

/// Compute Saxton & Rawls (2006) soil Available Water Capacity from fine-earth
/// texture (`sandPct`, `clayPct`, optional `omPct` defaulting to `2.5%`),
/// returning `{ field_capacity, wilting_point, awc_mm_per_m, awc_30cm_mm,
/// awc_100cm_mm }` or `null` if out of domain.
///
/// # Errors
/// Throws only on serialization failure.
#[wasm_bindgen(js_name = soilAwc)]
pub fn soil_awc_js(sand_pct: f64, clay_pct: f64, om_pct: Option<f64>) -> Result<JsValue, JsError> {
    match crate::soil::soil_awc(sand_pct, clay_pct, om_pct) {
        Some(a) => to_js(&a),
        None => Ok(JsValue::NULL),
    }
}

/// Compute Saxton & Rawls (2006) soil Available Water Capacity discounted for
/// coarse fragments (`cfvo_0_30_pct` for 0–30 cm topsoil and `cfvo_30_100_pct`
/// for 30–100 cm subsoil, in vol %), or `null` if out of domain.
///
/// # Errors
/// Throws only on serialization failure.
#[wasm_bindgen(js_name = soilAwcWithRock)]
pub fn soil_awc_with_rock_js(
    sand_pct: f64,
    clay_pct: f64,
    om_pct: Option<f64>,
    cfvo_0_30_pct: Option<f64>,
    cfvo_30_100_pct: Option<f64>,
) -> Result<JsValue, JsError> {
    match crate::soil::soil_awc_with_rock(
        sand_pct,
        clay_pct,
        om_pct,
        cfvo_0_30_pct,
        cfvo_30_100_pct,
    ) {
        Some(a) => to_js(&a),
        None => Ok(JsValue::NULL),
    }
}

/// Decode a `soil_plus` tile (0.05-degree, 6 bands) and look a point up.
/// Returns [`crate::soil_plus::SoilPlusLookup`] as a plain object, or `null`.
///
/// # Errors
/// Throws when the bytes are not a `soil_plus` tile, the coordinate is invalid,
/// or the point lies in another tile.
#[wasm_bindgen(js_name = lookupSoilPlus)]
pub fn lookup_soil_plus_js(tile_bytes: &[u8], lat: f64, lon: f64) -> Result<JsValue, JsError> {
    let tile = crate::soil_plus::decode(tile_bytes).map_err(|e| JsError::new(&e.to_string()))?;
    match crate::soil_plus::lookup(&tile, lat, lon).map_err(|e| JsError::new(&e.to_string()))? {
        Some(found) => to_js(&found),
        None => Ok(JsValue::NULL),
    }
}

/// Decode a `terrain` tile (0.1-degree, 4 bands) and read the cell holding
/// (`cellLat`, `cellLon`): pass the climate lookup's `cell_lat` and `cell_lon`.
/// Returns [`crate::terrain::TerrainLookup`] as a plain object, or `null`.
///
/// # Errors
/// Throws when the bytes are not a `terrain` tile, the coordinate is invalid,
/// or the cell lies in another tile.
#[wasm_bindgen(js_name = lookupTerrain)]
pub fn lookup_terrain_js(
    tile_bytes: &[u8],
    cell_lat: f64,
    cell_lon: f64,
) -> Result<JsValue, JsError> {
    let tile = crate::terrain::TerrainTile::decode(tile_bytes)
        .map_err(|e| JsError::new(&e.to_string()))?;
    match crate::terrain::at_cell(&tile, cell_lat, cell_lon)
        .map_err(|e| JsError::new(&e.to_string()))?
    {
        Some(found) => to_js(&found),
        None => Ok(JsValue::NULL),
    }
}

/// Adjust a 0.1-degree site climate (`SiteClimate`) to local topography
/// (`MicroclimateInput`: `site_elev_m`, `slope_deg`, `aspect_deg`, `topo_position`).
/// Returns [`crate::microclimate::MicroclimateAdjustment`] as a plain object.
///
/// # Errors
/// Throws on deserialization failure or invalid non-finite topography inputs.
#[wasm_bindgen(js_name = adjustMicroclimate)]
pub fn adjust_microclimate_js(site: JsValue, input: JsValue) -> Result<JsValue, JsError> {
    let s: crate::microclimate::SiteClimate =
        serde_wasm_bindgen::from_value(site).map_err(|e| JsError::new(&e.to_string()))?;
    let inp: crate::microclimate::MicroclimateInput =
        serde_wasm_bindgen::from_value(input).map_err(|e| JsError::new(&e.to_string()))?;
    let adjusted =
        crate::microclimate::adjust(&s, &inp).map_err(|e| JsError::new(&e.to_string()))?;
    to_js(&adjusted)
}

/// Evaluate a species' multi-decade ecological trajectory (`TrajectorySiteInput`,
/// `SpeciesEnvelope`). Returns [`crate::trajectory::TrajectoryResult`] as a plain
/// object (`periods`, `trajectory_class`, `establishment_vs_maturity_gap`, `has_gap`).
///
/// # Errors
/// Throws on deserialization or serialization failure.
#[wasm_bindgen(js_name = evaluateTrajectory)]
pub fn evaluate_trajectory_js(site: JsValue, envelope: JsValue) -> Result<JsValue, JsError> {
    let s: crate::trajectory::TrajectorySiteInput =
        serde_wasm_bindgen::from_value(site).map_err(|e| JsError::new(&e.to_string()))?;
    let env: crate::trajectory::SpeciesEnvelope =
        serde_wasm_bindgen::from_value(envelope).map_err(|e| JsError::new(&e.to_string()))?;
    let res = crate::trajectory::evaluate_trajectory(&s, &env);
    to_js(&res)
}

/// Crate version, for logs and cache keys.
#[wasm_bindgen(js_name = version)]
#[must_use]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
