//! The `soil` stage: SoilGrids 2.0 5-km aggregates (Homolosine) to the
//! 0.05-degree layer grid ([`crate::soil`]).
//!
//! 1. Each variable (pH in water, sand, clay) is combined over 0-5, 5-15
//!    and 15-30 cm into a depth-weighted 0-30 cm mean (weights 5, 10, 15);
//!    a pixel missing any depth has no value.
//! 2. Each layer cell centre is projected to Homolosine ([`super::igh`])
//!    and takes the SoilGrids pixel under it (nearest pixel: the 5 km
//!    pixel and the 0.05-degree cell are about the same size, and values
//!    stay identical to SoilGrids').
//! 3. Land cells (any code in the `regions` layer) without a value take
//!    the values of the nearest measured cell within
//!    [`SOIL_FILL_RADIUS`] cells; the `fill_distance` band records the
//!    distance. Measured cells outside the land mask are kept.

use std::path::Path;

use rayon::prelude::{
    IndexedParallelIterator, IntoParallelIterator, IntoParallelRefIterator,
    IntoParallelRefMutIterator, ParallelIterator,
};
use serde_json::{Value, json};

use super::build::BuildConfig;
use super::igh::Homolosine;
use super::tiff_rows::TiffRows;
use super::{PipelineError, Result, layers, sources};
use crate::layer::{
    LAYER_CELL_DEG, LAYER_GRID_H, LAYER_GRID_W, LAYER_TILE_CELLS, LayerCell, LayerTile,
};
use crate::manifest::{LayerSpec, VariableSpec};
use crate::model::NODATA;
use crate::regions::REGION_BANDS;
use crate::soil::{SOIL_BANDS, SOIL_DEPTH, SOIL_FILL_RADIUS, SoilBand, has_soil};

/// Output directory of the soil tiles.
pub const DIR: &str = "soil";

/// SoilGrids variables, in band order.
pub const VARIABLES: [&str; 3] = ["phh2o", "sand", "clay"];

/// Depths and their weights (thickness in cm).
pub const DEPTHS: [(&str, f32); 3] = [("0-5cm", 5.0), ("5-15cm", 10.0), ("15-30cm", 15.0)];

/// A SoilGrids raster in memory: values and georeferencing.
pub struct Source {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// West edge (m), pixel width, north edge (m), pixel height (negative).
    pub geo: (f64, f64, f64, f64),
    /// Values, row-major; NaN = no data.
    pub values: Vec<f32>,
}

impl Source {
    /// Index of the pixel holding a Homolosine point, if inside.
    #[must_use]
    pub fn pixel(&self, x: f64, y: f64) -> Option<usize> {
        let (x0, pw, y0, ph) = self.geo;
        let col = ((x - x0) / pw).floor();
        let row = ((y - y0) / ph).floor();
        if col < 0.0 || row < 0.0 || col >= self.width as f64 || row >= self.height as f64 {
            return None;
        }
        Some(row as usize * self.width + col as usize)
    }
}

/// Read a single-band SoilGrids raster (`int16`, nodata -32768) as `f32`.
///
/// # Errors
/// I/O or format errors.
pub fn read_source(path: &Path) -> Result<Source> {
    let mut r = TiffRows::open(path)?;
    let (w, h) = (r.info.width as usize, r.info.height as usize);
    let geo = r
        .info
        .geo
        .ok_or_else(|| PipelineError::Raster(format!("{}: no georeferencing", path.display())))?;
    let nodata = r.info.nodata.unwrap_or(-32768.0) as f32;
    let mut values = vec![f32::NAN; w * h];
    for (row, dst) in values.chunks_mut(w).enumerate() {
        r.read_row(u32::try_from(row).unwrap_or(u32::MAX), dst)?;
        for v in dst.iter_mut() {
            if *v == nodata {
                *v = f32::NAN;
            }
        }
    }
    Ok(Source {
        width: w,
        height: h,
        geo,
        values,
    })
}

/// Depth-weighted mean of three rasters of the same grid; NaN where any
/// depth is missing.
///
/// # Errors
/// Rasters of different grids.
pub fn depth_weighted(layers: &[Source; 3]) -> Result<Source> {
    let [a, b, c] = layers;
    if [b, c]
        .iter()
        .any(|s| (s.width, s.height, s.geo) != (a.width, a.height, a.geo))
    {
        return Err(PipelineError::Data(
            "SoilGrids depths differ in grid".into(),
        ));
    }
    let total: f32 = DEPTHS.iter().map(|d| d.1).sum();
    let values = (0..a.values.len())
        .into_par_iter()
        .map(|i| {
            let s: f32 = [a, b, c]
                .iter()
                .zip(DEPTHS)
                .map(|(l, (_, w))| l.values[i] * w)
                .sum();
            s / total
        })
        .collect();
    Ok(Source {
        width: a.width,
        height: a.height,
        geo: a.geo,
        values,
    })
}

/// Source pixel of every layer cell centre (`u32::MAX`: outside the
/// raster or in a Homolosine interruption).
///
/// # Errors
/// Projection setup errors.
pub fn pixel_map(src: &Source) -> Result<Vec<u32>> {
    let h = Homolosine::new()?;
    let mut map = vec![u32::MAX; LAYER_GRID_W * LAYER_GRID_H];
    map.par_iter_mut().enumerate().for_each(|(i, m)| {
        let c = LayerCell::from_global(i);
        if let Some((x, y)) = h.forward(c.center_lon(), c.center_lat())
            && let Some(p) = src.pixel(x, y)
        {
            *m = u32::try_from(p).unwrap_or(u32::MAX);
        }
    });
    Ok(map)
}

/// Encoded value of a SoilGrids pixel (already pH x 10 or g/kg).
fn encode(v: f32) -> i16 {
    crate::model::quantize(v, 1.0, 0.0)
}

/// The soil bands on the layer grid, before filling.
pub struct Measured {
    /// pH x 10, sand g/kg, clay g/kg; [`NODATA`] where not measured.
    pub bands: [Vec<i16>; 3],
}

/// Sample the three combined variables at every cell.
#[must_use]
pub fn sample(map: &[u32], vars: &[Source; 3]) -> Measured {
    let one = |s: &Source| -> Vec<i16> {
        map.par_iter()
            .map(|&p| {
                s.values
                    .get(p as usize)
                    .copied()
                    .filter(|v| v.is_finite())
                    .map_or(NODATA, encode)
            })
            .collect()
    };
    let mut bands = [one(&vars[0]), one(&vars[1]), one(&vars[2])];
    // A cell is measured only when all three variables are.
    for i in 0..map.len() {
        if bands.iter().any(|b| b[i] == NODATA) {
            for b in &mut bands {
                b[i] = NODATA;
            }
        }
    }
    Measured { bands }
}

/// Fill land cells without a value from the nearest measured cell within
/// `radius` cells (squared cell distance; ties: north, then west).
/// Returns the fill-distance band (0 measured, `d` cells filled, NODATA
/// none) and fills `bands` in place.
#[must_use]
pub fn fill(bands: &mut [Vec<i16>; 3], land: &[bool], radius: usize) -> Vec<i16> {
    let n = LAYER_GRID_W * LAYER_GRID_H;
    let measured = |i: usize| bands[0][i] != NODATA;
    let r = i64::try_from(radius).unwrap_or(0);
    let offsets: Vec<(i64, i64, i64)> = {
        let mut o: Vec<(i64, i64, i64)> = (-r..=r)
            .flat_map(|dy| (-r..=r).map(move |dx| (dx * dx + dy * dy, dy, dx)))
            .filter(|&(d2, _, _)| d2 > 0 && d2 <= r * r)
            .collect();
        o.sort_unstable();
        o
    };
    let mut distance: Vec<i16> = bands[0]
        .par_iter()
        .map(|&v| if v == NODATA { NODATA } else { 0 })
        .collect();
    let todo: Vec<usize> = (0..n)
        .into_par_iter()
        .filter(|&i| land[i] && !measured(i))
        .collect();
    let found: Vec<(usize, usize, i64)> = todo
        .into_par_iter()
        .filter_map(|i| {
            let c = LayerCell::from_global(i);
            offsets.iter().find_map(|&(d2, dy, dx)| {
                let row = c.row as i64 + dy;
                if row < 0 || row >= LAYER_GRID_H as i64 {
                    return None;
                }
                let col = (c.col as i64 + dx).rem_euclid(LAYER_GRID_W as i64);
                let j = usize::try_from(row).ok()? * LAYER_GRID_W + usize::try_from(col).ok()?;
                measured(j).then_some((i, j, d2))
            })
        })
        .collect();
    for (i, j, d2) in found {
        distance[i] = (d2 as f64).sqrt().ceil() as i16;
        for b in bands.iter_mut() {
            b[i] = b[j];
        }
    }
    distance
}

fn band(id: &str, unit: &str, scale: f32, description: &str) -> VariableSpec {
    VariableSpec {
        id: id.into(),
        unit: unit.into(),
        scale,
        offset: 0.0,
        description: description.into(),
        months: None,
    }
}

pub(crate) fn layer_spec(tiles: Vec<crate::manifest::TileSpec>) -> LayerSpec {
    LayerSpec {
        id: "soil".into(),
        description: format!(
            "SoilGrids 2.0 pH (water), sand and clay, depth-weighted {SOIL_DEPTH} mean, with a fill flag"
        ),
        cell_deg: LAYER_CELL_DEG,
        tile_cells: LAYER_TILE_CELLS as u32,
        directory: DIR.into(),
        encoding: layers::encoding(SOIL_BANDS),
        bands: vec![
            band("ph_h2o", "pH", 0.1, "pH in water, 0-30 cm (SoilGrids phh2o)"),
            band("sand", "%", 0.1, "sand, percent of the fine earth, 0-30 cm (stored g/kg)"),
            band("clay", "%", 0.1, "clay, percent of the fine earth, 0-30 cm (stored g/kg)"),
            band(
                "fill_distance",
                "cells",
                1.0,
                "0: SoilGrids value of this cell; 1-8: no SoilGrids value here (built-up area, water, ice), values copied from the nearest measured cell this many cells away",
            ),
        ],
        sources: vec!["soilgrids-2.0".into()],
        tiles,
        notes: vec![
            "0-30 cm values are the mean of SoilGrids' 0-5, 5-15 and 15-30 cm predictions weighted by thickness (5, 10, 15 cm).".into(),
            "Reprojection: each cell centre is projected to Interrupted Goode Homolosine (PROJ's igh, built from proj4rs's sinusoidal and Mollweide; matches PROJ 9 to 1 mm) and takes the SoilGrids 5 km pixel under it.".into(),
            format!("Fill: land cells (a code in the regions layer) without a SoilGrids value take the nearest measured cell within {SOIL_FILL_RADIUS} cells (0.4 degrees); fill_distance records the distance. Treat filled values, and all values, as likely rather than measured."),
            "Silt is 100 % minus sand and clay; the USDA texture class is derived at lookup.".into(),
            "Tiles without any soil value are absent.".into(),
        ],
        slots: vec![],
        version: Some(env!("CARGO_PKG_VERSION").into()),
    }
}

/// Land mask from the release's regions layer.
/// Land mask and country code (`NODATA` where none) from the release's
/// regions layer.
fn land_mask(out: &Path) -> Result<(Vec<bool>, Vec<i16>)> {
    let n = LAYER_GRID_W * LAYER_GRID_H;
    let (mut land, mut country) = (vec![false; n], vec![NODATA; n]);
    for t in layers::read_layer(out, super::regions::DIR, REGION_BANDS)? {
        for j in 0..crate::layer::LAYER_TILE_LEN {
            let g = LayerCell::in_tile(t.id(), j).global_index();
            land[g] = (0..REGION_BANDS).any(|b| t.get(j, b) != NODATA);
            country[g] = t.get(j, crate::regions::RegionBand::IsoA2 as usize);
        }
    }
    Ok((land, country))
}

/// Land cells without soil per country, largest first (top 12).
fn without_soil_by_country(distance: &[i16], land: &[bool], country: &[i16]) -> Value {
    let mut by: std::collections::BTreeMap<i16, u64> = std::collections::BTreeMap::new();
    for i in (0..distance.len()).filter(|&i| land[i] && distance[i] == NODATA) {
        *by.entry(country[i]).or_default() += 1;
    }
    let mut v: Vec<(i16, u64)> = by.into_iter().collect();
    v.sort_by_key(|&(c, n)| (std::cmp::Reverse(n), c));
    let rows: serde_json::Map<String, Value> = v
        .into_iter()
        .take(12)
        .map(|(c, n)| {
            let name = crate::regions::decode_code(c, 2).unwrap_or_else(|| "none".into());
            (name, json!(n))
        })
        .collect();
    Value::Object(rows)
}

/// Counts for the report.
fn counts(distance: &[i16], land: &[bool]) -> Value {
    let mut hist = [0u64; SOIL_FILL_RADIUS + 1];
    let (mut land_without, mut measured_sea) = (0u64, 0u64);
    for (i, &d) in distance.iter().enumerate() {
        if d == NODATA {
            land_without += u64::from(land[i]);
        } else {
            if let Some(h) = usize::try_from(d).ok().and_then(|d| hist.get_mut(d)) {
                *h += 1;
            }
            measured_sea += u64::from(d == 0 && !land[i]);
        }
    }
    json!({
        "measured_cells": hist[0],
        "filled_cells": hist[1..].iter().sum::<u64>(),
        "filled_by_distance_cells": hist[1..].to_vec(),
        "land_cells_without_soil": land_without,
        "measured_cells_outside_land_mask": measured_sea,
    })
}

/// Run the `soil` stage.
///
/// # Errors
/// Missing sources, release or regions layer; writer or I/O errors.
pub fn stage_soil(cfg: &BuildConfig) -> Result<()> {
    if layers::has_layer(&cfg.out, "soil") && !cfg.force {
        eprintln!("soil: layer present, skipping");
        return Ok(());
    }
    let t0 = std::time::Instant::now();
    let (land, country) = land_mask(&cfg.out)?;
    let mut combined = Vec::new();
    for v in VARIABLES {
        let read = |d: &str| {
            read_source(&sources::pinned(
                &cfg.sources,
                &format!("soilgrids/{v}_{d}_mean_5000.tif"),
            )?)
        };
        let depths = [read(DEPTHS[0].0)?, read(DEPTHS[1].0)?, read(DEPTHS[2].0)?];
        combined.push(depth_weighted(&depths)?);
        eprintln!("soil: {v} combined, {:.1} s", t0.elapsed().as_secs_f64());
    }
    let vars: [Source; 3] = combined
        .try_into()
        .map_err(|_| PipelineError::Data("three SoilGrids variables".into()))?;
    for (idx, v) in vars.iter().enumerate().skip(1) {
        if (v.width, v.height, v.geo) != (vars[0].width, vars[0].height, vars[0].geo) {
            return Err(PipelineError::Data(format!(
                "soil: raster {} grid ({}x{}, {:?}) does not match {} ({}x{}, {:?})",
                VARIABLES[idx], v.width, v.height, v.geo, VARIABLES[0], vars[0].width, vars[0].height, vars[0].geo
            )));
        }
    }
    let map = pixel_map(&vars[0])?;
    let Measured { mut bands } = sample(&map, &vars);
    drop(vars);
    let distance = fill(&mut bands, &land, SOIL_FILL_RADIUS);
    let mut report_counts = counts(&distance, &land);
    report_counts["land_cells_without_soil_by_country"] =
        without_soil_by_country(&distance, &land, &country);
    drop(country);
    eprintln!("soil: {report_counts}, {:.1} s", t0.elapsed().as_secs_f64());
    let has = |t: &LayerTile, i: usize| has_soil(t, i);
    let [ph, sand, clay] = &bands;
    let tiles = layers::cut_tiles(&[ph, sand, clay, &distance], has)?;
    let specs = layers::write_tiles(&cfg.out, DIR, tiles, has)?;
    let report = json!({
        "counts": report_counts,
        "fill_radius_cells": SOIL_FILL_RADIUS,
        "sizes": layers::size_report(&specs),
        "seconds": t0.elapsed().as_secs_f64().round(),
    });
    eprintln!("soil: {}", report["sizes"]);
    debug_assert_eq!(SoilBand::FillDistance as usize, 3);
    layers::register(&cfg.out, layer_spec(specs), report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(values: Vec<f32>) -> Source {
        Source {
            width: 2,
            height: 1,
            geo: (0.0, 5000.0, 0.0, -5000.0),
            values,
        }
    }

    #[test]
    fn depth_weighting_and_missing_depths() {
        let d = depth_weighted(&[
            src(vec![60.0, 60.0]),
            src(vec![66.0, f32::NAN]),
            src(vec![72.0, 70.0]),
        ])
        .unwrap();
        // (60*5 + 66*10 + 72*15) / 30 = 68
        assert!((d.values[0] - 68.0).abs() < 1e-5);
        assert!(d.values[1].is_nan());
        let mut other = src(vec![1.0, 1.0]);
        other.geo.0 = 1.0;
        assert!(depth_weighted(&[src(vec![0.0; 2]), other, src(vec![0.0; 2])]).is_err());
    }

    #[test]
    fn pixel_lookup_in_homolosine_metres() {
        let s = src(vec![1.0, 2.0]);
        assert_eq!(s.pixel(10.0, -10.0), Some(0));
        assert_eq!(s.pixel(5000.0, -1.0), Some(1));
        assert_eq!(s.pixel(10_000.0, -1.0), None);
        assert_eq!(s.pixel(-1.0, -1.0), None);
        assert_eq!(s.pixel(1.0, 1.0), None);
    }

    fn global(cells: &[(LayerCell, i16)]) -> [Vec<i16>; 3] {
        let n = LAYER_GRID_W * LAYER_GRID_H;
        let mut b = [vec![NODATA; n], vec![NODATA; n], vec![NODATA; n]];
        for &(c, v) in cells {
            for band in &mut b {
                band[c.global_index()] = v;
            }
        }
        b
    }

    #[test]
    fn fill_copies_the_nearest_measured_cell_on_land_only() {
        let n = LAYER_GRID_W * LAYER_GRID_H;
        let m1 = LayerCell {
            col: 3000,
            row: 800,
        };
        let m2 = LayerCell {
            col: 3005,
            row: 800,
        };
        let mut bands = global(&[(m1, 10), (m2, 20)]);
        let mut land = vec![false; n];
        let near1 = LayerCell {
            col: 3002,
            row: 800,
        }; // 2 from m1, 3 from m2
        let far = LayerCell {
            col: 3020,
            row: 800,
        }; // 15 from m2: beyond 8
        let sea = LayerCell {
            col: 3003,
            row: 801,
        };
        for c in [near1, far] {
            land[c.global_index()] = true;
        }
        let d = fill(&mut bands, &land, SOIL_FILL_RADIUS);
        assert_eq!(d[m1.global_index()], 0);
        assert_eq!(
            (d[near1.global_index()], bands[0][near1.global_index()]),
            (2, 10)
        );
        assert_eq!(bands[2][near1.global_index()], 10);
        assert_eq!(d[far.global_index()], NODATA);
        assert_eq!(bands[0][far.global_index()], NODATA);
        assert_eq!(d[sea.global_index()], NODATA, "sea cells stay empty");
        let c = counts(&d, &land);
        assert_eq!(c["measured_cells"], 2);
        assert_eq!(c["filled_cells"], 1);
        assert_eq!(c["land_cells_without_soil"], 1);
        assert_eq!(c["measured_cells_outside_land_mask"], 2);
    }

    #[test]
    fn fill_wraps_at_the_antimeridian_and_rounds_up() {
        let n = LAYER_GRID_W * LAYER_GRID_H;
        let east = LayerCell {
            col: LAYER_GRID_W - 1,
            row: 500,
        };
        let west = LayerCell { col: 1, row: 501 }; // dx 2, dy 1: d = 2.24 -> 3
        let mut bands = global(&[(east, 55)]);
        let mut land = vec![false; n];
        land[west.global_index()] = true;
        let d = fill(&mut bands, &land, SOIL_FILL_RADIUS);
        assert_eq!(d[west.global_index()], 3);
        assert_eq!(bands[1][west.global_index()], 55);
    }

    #[test]
    fn sample_requires_all_three_variables() {
        let n = LAYER_GRID_W * LAYER_GRID_H;
        let mut map = vec![u32::MAX; n];
        map[0] = 0;
        map[1] = 1;
        let vars = [
            src(vec![65.0, 70.0]),
            src(vec![300.0, f32::NAN]),
            src(vec![200.4, 100.0]),
        ];
        let m = sample(&map, &vars);
        assert_eq!(
            (m.bands[0][0], m.bands[1][0], m.bands[2][0]),
            (65, 300, 200)
        );
        assert_eq!(m.bands[0][1], NODATA);
        assert_eq!(m.bands[2][2], NODATA);
    }
}
