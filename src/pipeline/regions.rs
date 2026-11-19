//! The `regions` stage: rasterize the WGSRPD level-3 areas and the Natural
//! Earth 1:50m admin-0 map units onto the 0.05-degree layer grid and write the
//! `regions/` tiles ([`crate::regions`]).
//!
//! A cell takes the polygon that contains its centre (boundary included),
//! tested with `geo`'s point-in-polygon after a bounding-box filter. A
//! polygon too small to contain any cell centre (an islet, a microstate)
//! still takes the cell of its interior point, even from a larger
//! neighbour, so every area keeps at least one cell.
//!
//! Order, so that the result does not depend on chance: a cell inside
//! several polygons takes the first in source file order; tiny polygons
//! are written largest first, so the smallest keeps a shared cell.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use geo::{Area, BoundingRect, InteriorPoint, Intersects};
use geo::{Coord, Geometry, MultiPolygon, Polygon, Rect};
use rayon::prelude::{IntoParallelIterator, ParallelIterator};
use serde_json::{Value, json};

use super::build::BuildConfig;
use super::{PipelineError, Result, layers, sources};
use crate::grid::TileId;
use crate::layer::{
    LAYER_CELL_DEG, LAYER_GRID_H, LAYER_GRID_W, LAYER_TILE_CELLS, LayerCell, LayerTile,
};
use crate::manifest::{LayerSpec, VariableSpec};
use crate::model::NODATA;
use crate::regions::{REGION_BANDS, encode_code};

/// One polygon of an area, with its code and bounding box.
#[derive(Debug, Clone)]
pub struct Part {
    /// Encoded code ([`encode_code`]).
    pub code: i16,
    /// The polygon (lon/lat degrees).
    pub polygon: Polygon<f64>,
    /// Its bounding box.
    pub bbox: Rect<f64>,
}

/// Polygons of a source, split into parts.
#[derive(Debug, Clone, Default)]
pub struct Areas {
    /// Every polygon.
    pub parts: Vec<Part>,
    /// Distinct codes.
    pub codes: BTreeSet<i16>,
    /// Features skipped (no code), with the reason.
    pub skipped: Vec<String>,
}

impl Areas {
    fn push(&mut self, code: i16, geometry: Geometry<f64>) {
        let polys: Vec<Polygon<f64>> = match geometry {
            Geometry::Polygon(p) => vec![p],
            Geometry::MultiPolygon(MultiPolygon(ps)) => ps,
            _ => Vec::new(),
        };
        for polygon in polys {
            if let Some(bbox) = polygon.bounding_rect() {
                self.parts.push(Part {
                    code,
                    polygon,
                    bbox,
                });
            }
        }
        self.codes.insert(code);
    }
}

/// ISO 3166-1 alpha-2 code of a Natural Earth admin-0 map unit (`unit` is
/// its `GU_A3`). Map units split overseas departments and territories from
/// their sovereign (Réunion RE, Svalbard SJ, Bonaire BQ, Christmas Island
/// CX, ...), and carry that code in `ISO_A2_EH`.
///
/// Order: a unit override (Jan Mayen, `NJM`: `ISO_A2_EH` says NO, but ISO
/// 3166-1 files it under SJ, "Svalbard and Jan Mayen"); then `ISO_A2_EH`
/// when it is two letters; then `ISO_A2`; then (both -99) the de jure
/// country of Somaliland (`SOL` -> SO) and Northern Cyprus (`CYN` -> CY).
/// `None` for the Siachen Glacier (`KAS`) and any other unit without a code.
#[must_use]
pub fn natural_earth_iso_a2(iso_a2: &str, iso_a2_eh: &str, unit: &str) -> Option<String> {
    let two = |s: &str| s.len() == 2 && s.bytes().all(|b| b.is_ascii_uppercase());
    if unit == "NJM" {
        return Some("SJ".into());
    }
    if two(iso_a2_eh) {
        return Some(iso_a2_eh.to_string());
    }
    if two(iso_a2) {
        return Some(iso_a2.to_string());
    }
    match unit {
        "SOL" => Some("SO".into()), // Somaliland
        "CYN" => Some("CY".into()), // Northern Cyprus
        _ => None,
    }
}

fn read_features(path: &Path) -> Result<Vec<geojson::Feature>> {
    let text = std::fs::read_to_string(path).map_err(|e| PipelineError::io(path, e))?;
    match text.parse::<geojson::GeoJson>() {
        Ok(geojson::GeoJson::FeatureCollection(fc)) => Ok(fc.features),
        Ok(_) => Err(PipelineError::Data(format!(
            "{}: not a FeatureCollection",
            path.display()
        ))),
        Err(e) => Err(PipelineError::Data(format!("{}: {e}", path.display()))),
    }
}

fn prop<'a>(f: &'a geojson::Feature, key: &str) -> &'a str {
    f.property(key).and_then(Value::as_str).unwrap_or("")
}

fn geometry(f: &geojson::Feature) -> Option<Geometry<f64>> {
    f.geometry
        .as_ref()
        .and_then(|g| Geometry::<f64>::try_from(g).ok())
}

/// Load features as areas, the code coming from `code_of`.
fn load(path: &Path, code_of: impl Fn(&geojson::Feature) -> Option<String>) -> Result<Areas> {
    let mut areas = Areas::default();
    for f in read_features(path)? {
        let Some(code) = code_of(&f) else {
            areas.skipped.push(format!(
                "{} {}",
                prop(&f, "ADM0_A3"),
                prop(&f, "LEVEL3_NAM")
            ));
            continue;
        };
        let (Some(c), Some(g)) = (encode_code(&code), geometry(&f)) else {
            areas.skipped.push(format!("{code}: bad code or geometry"));
            continue;
        };
        areas.push(c, g);
    }
    Ok(areas)
}

/// WGSRPD level-3 areas (`LEVEL3_COD`).
///
/// # Errors
/// Unreadable or malformed GeoJSON.
pub fn load_wgsrpd(path: &Path) -> Result<Areas> {
    load(path, |f| Some(prop(f, "LEVEL3_COD").to_string()))
}

/// Natural Earth admin-0 map units, relative path in the sources directory.
pub const NATURAL_EARTH_FILE: &str = "naturalearth/ne_50m_admin_0_map_units.geojson";

/// Natural Earth admin-0 map units ([`natural_earth_iso_a2`]).
///
/// # Errors
/// Unreadable or malformed GeoJSON.
pub fn load_natural_earth(path: &Path) -> Result<Areas> {
    load(path, |f| {
        natural_earth_iso_a2(prop(f, "ISO_A2"), prop(f, "ISO_A2_EH"), prop(f, "GU_A3"))
    })
}

/// A rasterized band for one tile.
pub struct TileRaster {
    /// The tile.
    pub id: TileId,
    /// Codes, row-major, `cells * cells`.
    pub codes: Vec<i16>,
    /// Indices (in `Areas::parts`) of the parts that hold a cell centre.
    pub hit: Vec<usize>,
    /// Cells inside two parts with different codes.
    pub overlaps: u64,
}

/// Rasterize the parts over one tile of `cells` x `cells` cells.
#[must_use]
pub fn rasterize_tile(areas: &Areas, id: TileId, cells: usize) -> TileRaster {
    let cell_deg = crate::grid::TILE_DEG / cells as f64;
    let (west, north) = (id.west(), id.north());
    let tile_box = Rect::new(
        Coord {
            x: west,
            y: north - crate::grid::TILE_DEG,
        },
        Coord {
            x: west + crate::grid::TILE_DEG,
            y: north,
        },
    );
    let candidates: Vec<usize> = (0..areas.parts.len())
        .filter(|&i| areas.parts[i].bbox.intersects(&tile_box))
        .collect();
    let mut codes = vec![NODATA; cells * cells];
    let mut hit = BTreeSet::new();
    let mut overlaps = 0;
    if !candidates.is_empty() {
        for r in 0..cells {
            let lat = north - (r as f64 + 0.5) * cell_deg;
            for c in 0..cells {
                let p = Coord {
                    x: west + (c as f64 + 0.5) * cell_deg,
                    y: lat,
                };
                let mut found = NODATA;
                for &i in &candidates {
                    let part = &areas.parts[i];
                    if part.bbox.intersects(&p) && part.polygon.intersects(&p) {
                        hit.insert(i);
                        if found == NODATA {
                            found = part.code;
                        } else if found != part.code {
                            overlaps += 1;
                        }
                    }
                }
                codes[r * cells + c] = found;
            }
        }
    }
    TileRaster {
        id,
        codes,
        hit: hit.into_iter().collect(),
        overlaps,
    }
}

/// A global band on the layer grid, row-major from 90 N, 180 W.
pub struct GlobalBand {
    /// Codes, `LAYER_GRID_W * LAYER_GRID_H`.
    pub codes: Vec<i16>,
    /// Report: parts, codes present, cells, overlaps, interior-point cells.
    pub report: Value,
}

/// Rasterize a source over the whole layer grid; `letters` is the code
/// length (3 for WGSRPD, 2 for ISO alpha-2), for the report.
#[must_use]
pub fn rasterize_global(areas: &Areas, letters: u32) -> GlobalBand {
    let tiles: Vec<TileRaster> = TileId::all()
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|id| rasterize_tile(areas, id, LAYER_TILE_CELLS))
        .collect();
    let mut codes = vec![NODATA; LAYER_GRID_W * LAYER_GRID_H];
    let mut hit = vec![false; areas.parts.len()];
    let mut overlaps = 0;
    for t in &tiles {
        for (j, &v) in t.codes.iter().enumerate() {
            codes[LayerCell::in_tile(t.id, j).global_index()] = v;
        }
        for &i in &t.hit {
            hit[i] = true;
        }
        overlaps += t.overlaps;
    }
    // Parts too small for a cell centre (islets, microstates) take their
    // interior point's cell, even from a larger neighbour: otherwise Monaco
    // or the Vatican would vanish into France or Italy. They are written
    // largest first, so when several share a cell the smallest keeps it
    // (ties: by code, then source order); the result does not depend on
    // the order of the source file.
    let mut tiny: Vec<(f64, i16, usize)> = (0..areas.parts.len())
        .filter(|&i| !hit[i])
        .map(|i| {
            (
                areas.parts[i].polygon.unsigned_area(),
                areas.parts[i].code,
                i,
            )
        })
        .collect();
    tiny.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let (mut marked, mut taken, mut no_point) = (0u64, 0u64, 0u64);
    for part in tiny.iter().map(|&(_, _, i)| &areas.parts[i]) {
        let cell = part
            .polygon
            .interior_point()
            .and_then(|p| LayerCell::of(p.y(), p.x()));
        let Some(c) = cell else {
            no_point += 1;
            continue;
        };
        let old = codes[c.global_index()];
        if old != NODATA && old != part.code {
            taken += 1;
        }
        marked += u64::from(old != part.code);
        codes[c.global_index()] = part.code;
    }
    let present: BTreeSet<i16> = codes.iter().copied().filter(|&v| v != NODATA).collect();
    let missing: Vec<String> = areas
        .codes
        .difference(&present)
        .filter_map(|&c| crate::regions::decode_code(c, letters))
        .collect();
    let mut per_code: BTreeMap<i16, u64> = BTreeMap::new();
    for &v in codes.iter().filter(|&&v| v != NODATA) {
        *per_code.entry(v).or_default() += 1;
    }
    let report = json!({
        "parts": areas.parts.len(),
        "codes": areas.codes.len(),
        "codes_present": present.len(),
        "codes_missing": missing,
        "skipped_features": areas.skipped,
        "cells": per_code.values().sum::<u64>(),
        "cells_in_two_areas": overlaps,
        "parts_without_a_cell_centre": hit.iter().filter(|h| !**h).count(),
        "cells_set_at_interior_point": marked,
        "of_which_taken_from_another_area": taken,
        "parts_without_interior_point": no_point,
    });
    GlobalBand { codes, report }
}

/// The release layer for the two rasterized bands.
pub(crate) fn layer_spec(tiles: Vec<crate::manifest::TileSpec>) -> LayerSpec {
    let band = |id: &str, description: &str| VariableSpec {
        id: id.into(),
        unit: "code".into(),
        scale: 1.0,
        offset: 0.0,
        description: description.into(),
    };
    LayerSpec {
        id: "regions".into(),
        description: "WGSRPD level-3 botanical area and country (ISO 3166-1 alpha-2) of each cell, stored as the codes themselves".into(),
        cell_deg: LAYER_CELL_DEG,
        tile_cells: LAYER_TILE_CELLS as u32,
        directory: DIR.into(),
        encoding: layers::encoding(REGION_BANDS),
        bands: vec![
            band("wgsrpd_l3", "WGSRPD level-3 code, three letters a b c (A = 0 ... Z = 25) stored as a * 676 + b * 26 + c"),
            band("iso_a2", "ISO 3166-1 alpha-2 code from Natural Earth admin-0 map units 1:50m (overseas territories keep their own code), two letters a b stored as a * 26 + b"),
        ],
        sources: vec!["wgsrpd-level3".into(), "natural-earth-map-units-50m".into()],
        tiles,
        notes: vec![
            "A cell takes the polygon containing its centre (boundary included); when several contain it, the first in source file order. A polygon too small to contain any cell centre (an islet, a microstate) then takes the cell of its interior point, overwriting a larger area's code there; when several such polygons share a cell, the smallest keeps it.".into(),
            "Borders are accurate to about half a cell (2.8 km) on top of the sources' own accuracy (Natural Earth 1:50m, WGSRPD level 3). Lookups whose cell is empty (a coastal point) take the nearest coded cell within 2 cells.".into(),
            "Natural Earth country codes (map units): Jan Mayen -> SJ as in ISO 3166-1, else ISO_A2_EH when two letters, else ISO_A2, else Somaliland -> SO and Northern Cyprus -> CY (de jure); the Siachen Glacier has no country. Natural Earth draws de facto boundaries.".into(),
            "Tiles where neither band has a code are absent.".into(),
        ],
        slots: vec![],
    }
}

/// Output directory of the regions tiles.
pub const DIR: &str = "regions";

/// Run the `regions` stage.
///
/// # Errors
/// Missing sources or release, writer or I/O errors.
pub fn stage_regions(cfg: &BuildConfig) -> Result<()> {
    if layers::has_layer(&cfg.out, "regions") && !cfg.force {
        eprintln!("regions: layer present, skipping");
        return Ok(());
    }
    layers::read_manifest(&cfg.out)?;
    let t0 = std::time::Instant::now();
    let l3 = load_wgsrpd(&sources::pinned(&cfg.sources, "wgsrpd/level3.geojson")?)?;
    let ne = load_natural_earth(&sources::pinned(&cfg.sources, NATURAL_EARTH_FILE)?)?;
    let l3 = rasterize_global(&l3, 3);
    eprintln!(
        "regions: wgsrpd {:.1} s {}",
        t0.elapsed().as_secs_f64(),
        l3.report
    );
    let ne = rasterize_global(&ne, 2);
    eprintln!(
        "regions: natural earth {:.1} s {}",
        t0.elapsed().as_secs_f64(),
        ne.report
    );
    let has = |t: &LayerTile, i: usize| (0..REGION_BANDS).any(|b| t.get(i, b) != NODATA);
    let tiles = layers::cut_tiles(&[&l3.codes, &ne.codes], has)?;
    let specs = layers::write_tiles(&cfg.out, DIR, tiles, has)?;
    let report = json!({
        "wgsrpd_l3": l3.report,
        "iso_a2": ne.report,
        "sizes": layers::size_report(&specs),
        "seconds": t0.elapsed().as_secs_f64().round(),
    });
    eprintln!("regions: {}", report["sizes"]);
    layers::register(&cfg.out, layer_spec(specs), report)?;
    // The soil fill uses the regions as its land mask: soil built on the
    // old regions is stale.
    if layers::unregister(&cfg.out, "soil")? {
        eprintln!("regions: removed the soil layer (built on the old land mask); run --stage soil");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo::{LineString, polygon};

    #[test]
    fn microstates_take_their_cell_from_a_larger_neighbour() {
        // "Monaco" (smaller than a cell, between cell centres) inside the
        // cell of a large "France".
        let france =
            polygon![(x: 6.0, y: 43.0), (x: 8.0, y: 43.0), (x: 8.0, y: 44.0), (x: 6.0, y: 44.0)];
        let monaco = polygon![(x: 7.43, y: 43.73), (x: 7.44, y: 43.73), (x: 7.44, y: 43.74), (x: 7.43, y: 43.74)];
        let g = rasterize_global(&areas(vec![("FR", france), ("MC", monaco)]), 2);
        let c = LayerCell::of(43.735, 7.435).unwrap();
        assert_eq!(g.codes[c.global_index()], encode_code("MC").unwrap());
        assert_eq!(g.report["of_which_taken_from_another_area"], 1);
        assert_eq!(g.report["codes_missing"], json!([]));
        let next = LayerCell::of(43.73, 7.5).unwrap();
        assert_eq!(g.codes[next.global_index()], encode_code("FR").unwrap());
    }

    #[test]
    fn smallest_tiny_part_wins_a_shared_cell_in_any_order() {
        // Two parts too small for a cell centre (the cell is 7.40-7.45 E,
        // 43.70-43.75 N, centre 7.425, 43.725) whose interior points fall
        // in that cell: the smaller one keeps it, whatever the order.
        let big = polygon![(x: 7.430, y: 43.730), (x: 7.445, y: 43.730), (x: 7.445, y: 43.745), (x: 7.430, y: 43.745)];
        let small = polygon![(x: 7.432, y: 43.732), (x: 7.435, y: 43.732), (x: 7.435, y: 43.735), (x: 7.432, y: 43.735)];
        let cell = LayerCell::of(43.74, 7.44).unwrap().global_index();
        assert_eq!(cell, LayerCell::of(43.733, 7.433).unwrap().global_index());
        for parts in [
            vec![("AA", big.clone()), ("BB", small.clone())],
            vec![("BB", small.clone()), ("AA", big.clone())],
        ] {
            let g = rasterize_global(&areas(parts), 2);
            assert_eq!(g.codes[cell], encode_code("BB").unwrap());
        }
    }

    #[test]
    fn overseas_map_units_keep_their_own_codes() {
        // Map units carry the overseas ISO code in ISO_A2_EH.
        assert_eq!(
            natural_earth_iso_a2("FR-974", "RE", "REU").as_deref(),
            Some("RE")
        );
        assert_eq!(
            natural_earth_iso_a2("SJ", "SJ", "NSV").as_deref(),
            Some("SJ")
        );
        assert_eq!(
            natural_earth_iso_a2("BQ", "BQ", "NLY").as_deref(),
            Some("BQ")
        );
        // Jan Mayen is in ISO's SJ (Svalbard and Jan Mayen), not NO.
        assert_eq!(
            natural_earth_iso_a2("-99", "NO", "NJM").as_deref(),
            Some("SJ")
        );
        assert_eq!(
            natural_earth_iso_a2("-99", "NO", "NOR").as_deref(),
            Some("NO")
        );
    }

    #[test]
    fn natural_earth_codes_follow_the_documented_order() {
        assert_eq!(
            natural_earth_iso_a2("-99", "FR", "FRA").as_deref(),
            Some("FR")
        );
        assert_eq!(
            natural_earth_iso_a2("-99", "NO", "NOR").as_deref(),
            Some("NO")
        );
        assert_eq!(
            natural_earth_iso_a2("CN-TW", "TW", "TWN").as_deref(),
            Some("TW")
        );
        assert_eq!(
            natural_earth_iso_a2("DE", "DE", "DEU").as_deref(),
            Some("DE")
        );
        assert_eq!(
            natural_earth_iso_a2("DE", "-99", "DEU").as_deref(),
            Some("DE")
        );
        assert_eq!(
            natural_earth_iso_a2("-99", "-99", "SOL").as_deref(),
            Some("SO")
        );
        assert_eq!(
            natural_earth_iso_a2("-99", "-99", "CYN").as_deref(),
            Some("CY")
        );
        assert_eq!(natural_earth_iso_a2("-99", "-99", "KAS"), None);
        assert_eq!(
            natural_earth_iso_a2("-99", "XK", "KOS").as_deref(),
            Some("XK")
        );
    }

    fn areas(parts: Vec<(&str, Polygon<f64>)>) -> Areas {
        let mut a = Areas::default();
        for (code, p) in parts {
            a.push(encode_code(code).unwrap(), Geometry::Polygon(p));
        }
        a
    }

    #[test]
    fn cells_take_the_polygon_holding_their_centre() {
        assert!((crate::grid::TILE_DEG / LAYER_TILE_CELLS as f64 - LAYER_CELL_DEG).abs() < 1e-12);
        // A square from 1.0 to 2.0 E, 45.0 to 46.0 N in tile n50e000, and a
        // tiny island between cell centres.
        let square =
            polygon![(x: 1.0, y: 45.0), (x: 2.0, y: 45.0), (x: 2.0, y: 46.0), (x: 1.0, y: 46.0)];
        let island = polygon![(x: 5.01, y: 45.01), (x: 5.02, y: 45.01), (x: 5.02, y: 45.02)];
        let a = areas(vec![("AAA", square), ("BBB", island)]);
        let id = TileId::parse("n50e000").unwrap();
        let t = rasterize_tile(&a, id, LAYER_TILE_CELLS);
        let at = |lat: f64, lon: f64| t.codes[LayerCell::of(lat, lon).unwrap().index_in_tile()];
        let aaa = encode_code("AAA").unwrap();
        assert_eq!(at(45.5, 1.5), aaa);
        assert_eq!(at(45.01, 1.01), aaa);
        assert_eq!(at(45.99, 1.99), aaa);
        assert_eq!(at(44.99, 1.5), NODATA);
        assert_eq!(at(45.5, 2.01), NODATA);
        // 20 x 20 cells exactly.
        assert_eq!(t.codes.iter().filter(|&&v| v == aaa).count(), 400);
        assert_eq!(t.hit, vec![0]);
        assert_eq!(t.overlaps, 0);
        // Holes are outside.
        let holed = Polygon::new(
            LineString::from(vec![
                (1.0, 41.0),
                (3.0, 41.0),
                (3.0, 43.0),
                (1.0, 43.0),
                (1.0, 41.0),
            ]),
            vec![LineString::from(vec![
                (1.5, 41.5),
                (2.5, 41.5),
                (2.5, 42.5),
                (1.5, 42.5),
                (1.5, 41.5),
            ])],
        );
        let t = rasterize_tile(&areas(vec![("CCC", holed)]), id, LAYER_TILE_CELLS);
        let at = |lat: f64, lon: f64| t.codes[LayerCell::of(lat, lon).unwrap().index_in_tile()];
        assert_eq!(at(42.0, 2.0), NODATA);
        assert_eq!(at(41.2, 2.0), encode_code("CCC").unwrap());
    }

    #[test]
    fn polygons_split_at_the_antimeridian_meet() {
        // Two halves of an island across 180, as Natural Earth stores them.
        let east = polygon![(x: 179.0, y: -17.0), (x: 180.0, y: -17.0), (x: 180.0, y: -16.0), (x: 179.0, y: -16.0)];
        let west = polygon![(x: -180.0, y: -17.0), (x: -179.0, y: -17.0), (x: -179.0, y: -16.0), (x: -180.0, y: -16.0)];
        let a = areas(vec![("FIJ", east), ("FIJ", west)]);
        let fij = encode_code("FIJ").unwrap();
        for (lat, lon) in [(-16.5, 179.99), (-16.5, -179.99), (-16.5, 180.0)] {
            let c = LayerCell::of(lat, lon).unwrap();
            let t = rasterize_tile(&a, c.tile(), LAYER_TILE_CELLS);
            assert_eq!(t.codes[c.index_in_tile()], fij, "{lat} {lon}");
        }
    }

    #[test]
    fn global_raster_marks_tiny_islands() {
        let island = polygon![(x: 5.01, y: 45.01), (x: 5.02, y: 45.01), (x: 5.02, y: 45.02)];
        let g = rasterize_global(&areas(vec![("BBB", island)]), 3);
        let c = LayerCell::of(45.015, 5.015).unwrap();
        assert_eq!(g.codes[c.global_index()], encode_code("BBB").unwrap());
        assert_eq!(g.report["cells_set_at_interior_point"], 1);
        assert_eq!(g.report["codes_present"], 1);
        assert_eq!(g.report["codes_missing"], json!([]));
    }
}
