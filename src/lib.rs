//! Current and future climate for any location on Earth.
//!
//! The grid holds, for every 0.1-degree land cell, eight variables
//! ([`model::VARIABLES`]) for 1991-2020 and for 2041-2070 and 2071-2099 under
//! SSP1-2.6, SSP2-4.5, SSP3-7.0 and SSP5-8.5 ([`model::SLOTS`]). It ships as
//! one Cloud Optimized GeoTIFF per 10-degree tile plus a `manifest.json`.
//!
//! ```no_run
//! use canopi_climate::{grid::TileId, lookup::lookup, tile::Tile};
//!
//! let (lat, lon) = (48.8566, 2.3522);
//! let id = TileId::of(lat, lon).expect("valid coordinate");
//! let bytes = std::fs::read(format!("grid/tiles/{}", id.file_name()))?;
//! let tile = Tile::decode(&bytes)?;
//! if let Some(found) = lookup(&tile, lat, lon)? {
//!     let now = &found.periods[0];
//!     println!("{} {} °C", now.koppen, now.tmin_cold_c);
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Features: `pipeline` (native only) builds the grid and provides the CLI;
//! `wasm` exports [`wasm`] bindings.

pub mod derived;
pub mod grid;
pub mod koppen;
pub mod layer;
pub mod lookup;
pub mod manifest;
pub mod model;
pub mod regions;
pub mod soil;
pub mod tile;
#[cfg(any(test, feature = "pipeline"))]
pub mod vectors;

#[cfg(feature = "pipeline")]
pub mod pipeline;

#[cfg(feature = "wasm")]
pub mod wasm;
