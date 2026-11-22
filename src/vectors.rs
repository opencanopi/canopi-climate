//! Test vectors of the tile contract (docs/format.md): what a reader in
//! another language (the website's TypeScript port) must return.
//!
//! - [`site`] answers a point the way `canopi-climate lookup` does (climate,
//!   soil and regions, across tile edges), or the way a single-tile reader
//!   (the WebAssembly exports) does, given only the point's own tiles.
//! - [`tables`] are the rule tables a port re-implements: the Köppen legend
//!   and temperature margins, USDA texture classes, region codes and scale
//!   decoding, each computed by the crate's own functions.
//!
//! `tests/fixtures/synthetic-expected.json` (the committed synthetic tiles)
//! is written by this module's unit test; `vectors/lookup.json` (the
//! release grid) by `canopi-climate vectors` (`pipeline` feature).

use serde::Serialize;
use serde_json::{Value, json};

use crate::derived::{MonthlySummary, dry_month_mask};
use crate::extremes::{self, ExtremesLookup, ExtremesTile};
use crate::grid::TileId;
use crate::koppen;
use crate::layer::{LAYER_LOOKUP_RADIUS, LayerTile};
use crate::lookup::{Lookup, LookupError, lookup_across};
use crate::model::{NODATA, VARIABLES, decode_value};
use crate::regions::{self, RegionsLookup, decode_code, encode_code};
use crate::soil::{self, SoilLookup, texture_class};
use crate::tile::Tile;

/// A climate tile getter: the decoded tile, or `None` (no file).
pub type ClimateTiles<'a, 'b> = &'b dyn Fn(TileId) -> Option<&'a Tile>;
/// A layer tile getter: the decoded tile, or `None` (no file).
pub type LayerTiles<'a, 'b> = &'b dyn Fn(TileId) -> Option<&'a LayerTile>;
/// An extremes tile getter: the decoded tile, or `None` (no file).
pub type ExtremesTiles<'a, 'b> = &'b dyn Fn(TileId) -> Option<&'a ExtremesTile>;

/// The tile getters of a grid: climate, and each layer (`None`: the grid
/// has no such layer).
#[derive(Clone, Copy)]
pub struct Getters<'a, 'b> {
    /// Climate tiles.
    pub climate: ClimateTiles<'a, 'b>,
    /// Soil tiles.
    pub soil: Option<LayerTiles<'a, 'b>>,
    /// Regions tiles.
    pub regions: Option<LayerTiles<'a, 'b>>,
    /// Extremes tiles.
    pub extremes: Option<ExtremesTiles<'a, 'b>>,
}

/// What a reader answers at a point (the CLI's `lookup` JSON).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Site {
    /// Climate of every slot, `None` with no complete cell near.
    pub climate: Option<Lookup>,
    /// Soil, `None` without the layer or with no soil cell near.
    pub soil: Option<SoilLookup>,
    /// Regions, `None` without the layer.
    pub regions: Option<RegionsLookup>,
    /// Coldest night and hottest day at the climate lookup's cell, `None`
    /// without the layer, without climate, or where the cell has none.
    pub extremes: Option<ExtremesLookup>,
}

impl Site {
    /// `{"climate": …, "extremes": …, "regions": …, "soil": …}` (keys
    /// sorted), as printed by `canopi-climate lookup`.
    #[must_use]
    pub fn json(&self) -> Value {
        json!({ "climate": self.climate, "soil": self.soil, "regions": self.regions, "extremes": self.extremes })
    }

    /// Whether any part has data (`lookup` exits 2 when none has).
    #[must_use]
    pub fn found(&self) -> bool {
        self.climate.is_some()
            || self.soil.is_some()
            || self
                .regions
                .as_ref()
                .is_some_and(|r| r.wgsrpd_l3.is_some() || r.iso_a2.is_some())
    }

    /// Whether a single-tile reader gives the same values: climate and soil
    /// equal, and the same region codes at the same distances (a missing
    /// regions tile reads as no codes).
    #[must_use]
    pub fn same_values(&self, other: &Self) -> bool {
        let codes = |s: &Self| {
            s.regions.as_ref().map_or((None, None, None, None), |r| {
                (
                    r.wgsrpd_l3.clone(),
                    r.iso_a2.clone(),
                    r.wgsrpd_l3_distance_cells.map(f64::to_bits),
                    r.iso_a2_distance_cells.map(f64::to_bits),
                )
            })
        };
        self.climate == other.climate
            && self.soil == other.soil
            && self.extremes == other.extremes
            && codes(self) == codes(other)
    }
}

/// Answer a point from decoded tiles. With `own_tile_only`, every getter is
/// limited to the point's own tile: the answer of a reader that fetched
/// that tile only. The extremes are read at the climate lookup's cell, in
/// that cell's tile.
///
/// # Errors
/// [`LookupError::InvalidCoordinate`].
pub fn site(
    g: Getters<'_, '_>,
    (lat, lon, radius): (f64, f64, usize),
    own_tile_only: bool,
) -> Result<Site, LookupError> {
    let Getters {
        climate,
        soil,
        regions,
        extremes: ext,
    } = g;
    let own = TileId::of(lat, lon).ok_or(LookupError::InvalidCoordinate)?;
    let keep = |id: TileId| !own_tile_only || id == own;
    let c = |id: TileId| climate(id).filter(|_| keep(id));
    let climate = lookup_across(c, lat, lon, radius)?;
    let extremes = match (ext, &climate) {
        (Some(get), Some(c)) => {
            match TileId::parse(&c.tile).and_then(|id| get(id).filter(|_| keep(id))) {
                Some(t) => extremes::at_cell(t, c.cell_lat, c.cell_lon)?,
                None => None,
            }
        }
        _ => None,
    };
    let soil = match soil {
        Some(get) => {
            let g = |id: TileId| get(id).filter(|_| keep(id));
            soil::lookup_across(g, lat, lon, LAYER_LOOKUP_RADIUS)?
        }
        None => None,
    };
    let regions = match regions {
        Some(get) => {
            let g = |id: TileId| get(id).filter(|_| keep(id));
            Some(regions::lookup_across(g, lat, lon, LAYER_LOOKUP_RADIUS)?)
        }
        None => None,
    };
    Ok(Site {
        climate,
        soil,
        regions,
        extremes,
    })
}

/// One vector point: the expected answer (`"invalid"` for a coordinate
/// every reader refuses) and whether a single-tile reader agrees
/// (`own_tile`).
#[must_use]
pub fn point(name: &str, (lat, lon): (f64, f64), g: Getters<'_, '_>, radius: usize) -> Value {
    let all = site(g, (lat, lon, radius), false);
    let own = site(g, (lat, lon, radius), true);
    match (all, own) {
        (Ok(all), Ok(own)) => json!({
            "name": name, "lat": lat, "lon": lon,
            "own_tile": all.same_values(&own),
            "expect": all.json(),
        }),
        _ => {
            json!({ "name": name, "lat": lat, "lon": lon, "own_tile": false, "expect": "invalid" })
        }
    }
}

/// Margin inputs (code, coldest-month mean, warmest-month mean): the
/// crate's unit-test cases plus boundary and f32-rounding cases.
const MARGIN_CASES: [(&str, f32, f32); 22] = [
    ("Aw", 19.0, 25.0),
    ("Aw", 16.0, 20.0),
    ("Af", 17.9, 27.0),
    ("Cfb", 4.8, 20.1),
    ("Cfb", 4.8, 23.0),
    ("Cfb", -1.0, 20.0),
    ("Cfa", 4.3, 22.4),
    ("Csa", 10.9, 21.7),
    ("Cfc", 2.0, 9.3),
    ("Dwd", -40.0, 19.0),
    ("Dwd", -30.0, 19.0),
    ("Dfc", -40.0, 15.0),
    ("Dfb", -12.3, 23.1),
    ("Dsd", 4.1, 18.4),
    ("Dwc", 2.0, 22.0),
    ("ET", -10.0, 12.0),
    ("ET", -20.0, -0.3),
    ("EF", -30.0, 0.7),
    ("BWh", -50.0, 50.0),
    ("BSk", -5.0, 25.0),
    ("", 0.0, 0.0),
    ("Cwb", 18.3, 21.9),
];

/// Texture inputs (sand %, clay %): the crate's unit-test cases.
const TEXTURE_CASES: [(f64, f64); 19] = [
    (92.0, 3.0),
    (82.0, 6.0),
    (65.0, 10.0),
    (40.0, 20.0),
    (20.0, 15.0),
    (7.0, 5.0),
    (60.0, 27.0),
    (33.0, 34.0),
    (10.0, 34.0),
    (52.0, 42.0),
    (7.0, 47.0),
    (20.0, 60.0),
    (20.0, 40.0),
    (21.0, 40.0),
    (52.0, 7.0),
    (52.1, 7.0),
    (f64::NAN, 10.0),
    (70.0, 40.0),
    (-1.0, 10.0),
];

/// 10-row Gaussen dry-month rule cases (`gaussen_p_lt_2t`, docs/format.md):
/// Seville-like, P = 2T exact, P = 2T - 0.1, boreal T < 0 with P = 0,
/// T = 0 with P = 0, hot desert (12 dry), humid tropics (0 dry), bimodal
/// (Nairobi/Kiambu-like), southern hemisphere dry winter (Mendoza-like),
/// and a non-finite cell.
const DRY_MONTH_CASES: [(&str, [f32; 12], [f32; 12]); 10] = [
    (
        "seville_like_csa",
        [
            11.0, 12.5, 15.0, 17.0, 20.5, 25.0, 28.0, 27.5, 24.0, 19.5, 14.5, 11.5,
        ],
        [
            65.0, 55.0, 45.0, 50.0, 30.0, 10.0, 2.0, 4.0, 25.0, 65.0, 75.0, 80.0,
        ],
    ),
    (
        "exact_boundary_p_eq_2t",
        [
            10.0, 10.0, 10.0, 10.0, 10.0, 15.0, 20.0, 15.0, 10.0, 10.0, 10.0, 10.0,
        ],
        [
            50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 40.0, 50.0, 50.0, 50.0, 50.0, 50.0,
        ],
    ),
    (
        "just_below_boundary_p_lt_2t",
        [
            10.0, 10.0, 10.0, 10.0, 10.0, 15.0, 20.0, 15.0, 10.0, 10.0, 10.0, 10.0,
        ],
        [
            50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 39.9, 50.0, 50.0, 50.0, 50.0, 50.0,
        ],
    ),
    (
        "boreal_negative_t_zero_p",
        [
            -10.0, -9.5, -5.0, 1.5, 8.0, 13.5, 16.5, 14.5, 9.0, 3.0, -3.0, -7.5,
        ],
        [
            0.0, 0.0, 0.0, 25.0, 35.0, 50.0, 65.0, 60.0, 45.0, 40.0, 0.0, 0.0,
        ],
    ),
    ("zero_t_zero_p", [0.0; 12], [0.0; 12]),
    (
        "hot_desert_riyadh_like",
        [
            14.5, 17.0, 21.5, 27.0, 32.5, 35.5, 37.0, 36.5, 33.5, 28.0, 21.0, 16.0,
        ],
        [
            10.0, 8.0, 18.0, 20.0, 5.0, 0.0, 0.0, 0.0, 0.0, 2.0, 8.0, 10.0,
        ],
    ),
    (
        "humid_tropics_manaus_like",
        [
            26.5, 26.5, 26.5, 26.5, 26.5, 26.5, 27.0, 27.5, 28.0, 28.0, 27.5, 27.0,
        ],
        [
            260.0, 280.0, 310.0, 300.0, 250.0, 120.0, 85.0, 65.0, 75.0, 120.0, 175.0, 225.0,
        ],
    ),
    (
        "bimodal_nairobi_like",
        [
            19.5, 20.5, 20.5, 20.0, 19.0, 17.5, 17.0, 17.5, 18.5, 19.5, 19.0, 19.0,
        ],
        [
            35.0, 38.0, 95.0, 185.0, 145.0, 40.0, 15.0, 20.0, 25.0, 55.0, 130.0, 85.0,
        ],
    ),
    (
        "southern_hemisphere_mendoza_like",
        [
            25.0, 23.5, 20.5, 16.0, 11.5, 8.0, 7.5, 10.5, 14.0, 18.0, 21.5, 24.0,
        ],
        [
            55.0, 50.0, 45.0, 18.0, 10.0, 5.0, 6.0, 8.0, 14.0, 20.0, 35.0, 50.0,
        ],
    ),
    (
        "non_finite_value",
        [
            10.0, 12.0, 15.0, 18.0, 22.0, 25.0, 27.0, 26.0, 22.0, 18.0, 14.0, 11.0,
        ],
        [
            50.0,
            50.0,
            50.0,
            50.0,
            50.0,
            f32::NAN,
            50.0,
            50.0,
            50.0,
            50.0,
            50.0,
            50.0,
        ],
    ),
];

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// An `f32` input as its shortest decimal (`0.1f32` → `0.1`), the number a
/// port passes in; `null` when not finite.
fn short(x: f32) -> Option<f64> {
    x.to_string().parse().ok().filter(|v: &f64| v.is_finite())
}

/// The rule tables a port re-implements, computed by the crate:
/// `legend` (Köppen code → symbol), `margin` (temperature margin as the
/// f32 the crate computes, widened exactly, and rounded to 0.1 as lookups
/// print it), `texture` (USDA class), `codes` (region code encoding) and
/// `decode` (stored value at a scale and offset). Non-finite inputs are
/// written as `null`.
#[must_use]
pub fn tables() -> Value {
    let legend: Vec<Value> = (0..=31u8)
        .map(|c| json!({ "code": c, "symbol": koppen::symbol(c) }))
        .collect();
    let margin: Vec<Value> = MARGIN_CASES
        .iter()
        .map(|&(sym, t_cold, t_warm)| {
            let code = koppen::code(sym).unwrap_or(0);
            let m = f64::from(koppen::temperature_margin(code, t_cold, t_warm));
            json!({ "code": code, "symbol": sym, "t_cold": short(t_cold), "t_warm": short(t_warm),
                    "margin_f32": m, "margin": round1(m) })
        })
        .collect();
    let texture: Vec<Value> = TEXTURE_CASES
        .iter()
        .map(|&(sand, clay)| {
            let class = texture_class(sand, clay).map(soil::TextureClass::id);
            json!({ "sand_pct": sand.is_finite().then_some(sand), "clay_pct": clay, "class": class })
        })
        .collect();
    let encode: Vec<Value> = [
        "FRA", "AAA", "ZZZ", "ABT", "NWG", "FR", "AA", "ZZ", "XK", "", "fr", "-99", "ABCD", "F1",
        "CN-TW",
    ]
    .iter()
    .map(|c| json!({ "code": c, "stored": encode_code(c) }))
    .collect();
    let decode_codes: Vec<Value> = [
        (3822, 3),
        (0, 3),
        (17_575, 3),
        (17_576, 3),
        (-1, 3),
        (NODATA, 3),
        (147, 2),
        (675, 2),
        (676, 2),
        (NODATA, 2),
    ]
    .iter()
    .map(|&(stored, letters)| {
        json!({ "stored": stored, "letters": letters, "code": decode_code(stored, letters) })
    })
    .collect();
    let mut decode: Vec<Value> = [
        (-123i16, 0.1f32, 0.0f32),
        (46, 0.01, 0.0),
        (812, 1.0, 0.0),
        (NODATA, 0.1, 0.0),
        (-32767, 0.1, 0.0),
        (32767, 0.01, 0.0),
        (65, 0.1, 0.0),
        (355, 0.1, 0.0),
        (-30, 0.5, 10.0),
        (7, 0.25, 0.0),
    ]
    .iter()
    .map(|&(stored, scale, offset)| {
        json!({ "stored": stored, "scale": short(scale), "offset": short(offset),
                "value": decode_value(stored, scale, offset) })
    })
    .collect();
    for v in VARIABLES {
        let i = v.info();
        for stored in [-1, 0, 1, 123, 999] {
            decode.push(
                json!({ "variable": i.id, "stored": stored, "scale": short(i.scale),
                                "offset": short(i.offset), "value": v.decode(stored) }),
            );
        }
    }
    let usda: Vec<Value> = [
        -60.0, -51.2, -51.1, -17.8, -17.7, -12.3, -12.2, -8.4, -6.7, -6.6, 0.0, 4.4, 4.5, 21.0,
        40.0,
    ]
    .iter()
    .map(|&c: &f64| json!({ "tmin_extreme_c": c, "zone": extremes::usda_zone(c) }))
    .collect();
    json!({
        "legend": legend,
        "margin": margin,
        "texture": texture,
        "codes": { "encode": encode, "decode": decode_codes },
        "decode": decode,
        "usda": usda,
        "dry_month": dry_month_table(),
    })
}

fn dry_month_table() -> Vec<Value> {
    DRY_MONTH_CASES
        .iter()
        .map(|(name, t, p)| {
            let mask = dry_month_mask(t, p);
            let dry_months = MonthlySummary::new(t, p, 0.0).map(|s| s.dry_months);
            let t_json: Vec<Option<f64>> = t.iter().map(|&v| short(v)).collect();
            let p_json: Vec<Option<f64>> = p.iter().map(|&v| short(v)).collect();
            json!({
                "name": name,
                "t": t_json,
                "p": p_json,
                "mask": mask,
                "dry_months": dry_months,
            })
        })
        .collect()
}

/// Serialise vectors with one point or table row per line (reviewable
/// diffs). `head` holds the leading scalar fields.
#[must_use]
pub fn to_lines(head: &[(&str, Value)], points: &[Value], tables: &Value) -> String {
    let mut out = String::from("{");
    for (k, v) in head {
        out.push_str(&format!("{}: {},\n", Value::from(*k), v));
    }
    out.push_str("\"points\": [\n");
    out.push_str(&rows(points));
    out.push_str("\n],\n\"tables\": {\n");
    if let Value::Object(t) = tables {
        let parts: Vec<String> = t
            .iter()
            .map(|(k, v)| match v {
                Value::Array(a) => format!("{}: [\n{}\n]", Value::from(k.as_str()), rows(a)),
                other => format!("{}: {other}", Value::from(k.as_str())),
            })
            .collect();
        out.push_str(&parts.join(",\n"));
    }
    out.push_str("\n}}\n");
    out
}

fn rows(values: &[Value]) -> String {
    values
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join(",\n")
}

/// Whether two vector files hold the same vectors, ignoring the `cli`
/// field (a version bump alone is not a change). `Err` names the first
/// difference.
///
/// # Errors
/// A file that is not JSON, or the first differing point or table.
pub fn same_vectors(committed: &str, fresh: &str) -> Result<(), String> {
    let parse = |s: &str, what: &str| -> Result<Value, String> {
        let mut v: Value = serde_json::from_str(s).map_err(|e| format!("{what}: {e}"))?;
        if let Value::Object(m) = &mut v {
            m.remove("cli");
        }
        Ok(v)
    };
    let (a, b) = (parse(committed, "committed")?, parse(fresh, "fresh")?);
    if a == b {
        return Ok(());
    }
    let first = |v: &Value, k: &str| v.get(k).cloned().unwrap_or(Value::Null);
    for k in ["grid_manifest_sha256", "tiles"] {
        if first(&a, k) != first(&b, k) {
            return Err(format!("{k} differs"));
        }
    }
    let (pa, pb) = (first(&a, "points"), first(&b, "points"));
    let empty = Vec::new();
    let (la, lb) = (
        pa.as_array().unwrap_or(&empty),
        pb.as_array().unwrap_or(&empty),
    );
    if la.len() != lb.len() {
        return Err(format!("{} points, now {}", la.len(), lb.len()));
    }
    if let Some((x, _)) = la.iter().zip(lb).find(|(x, y)| x != y) {
        return Err(format!("point {} differs", first(x, "name")));
    }
    Err("tables differ".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::synthetic as layers;
    use crate::tile::synthetic;

    const EXPECTED: &str = "tests/fixtures/synthetic-expected.json";

    /// Points on the synthetic tiles (n50e000): climate block, ocean
    /// snapping, the incomplete island, corners, a tile edge, soil fill and
    /// coast, regions, a sweep, points in other tiles, invalid coordinates.
    fn points() -> Vec<(String, f64, f64)> {
        // Climate cell (x, y) and layer cell (x, y) centres.
        let r6 = |v: f64| (v * 1e6).round() / 1e6;
        let c = |x: f64, y: f64| (r6(50.0 - (y + 0.5) * 0.1), r6((x + 0.5) * 0.1));
        let l = |x: f64, y: f64| (r6(50.0 - (y + 0.5) * 0.05), r6((x + 0.5) * 0.05));
        let mut p: Vec<(String, (f64, f64))> = vec![
            ("climate cell 50,40".into(), c(50.0, 40.0)),
            ("extremes gap, climate cell 60,40".into(), c(60.0, 40.0)),
            ("two cells west of the block".into(), c(18.0, 40.0)),
            ("three cells west of the block".into(), c(17.0, 40.0)),
            ("2.83-cell diagonal".into(), c(18.0, 28.0)),
            ("incomplete island".into(), c(5.0, 5.0)),
            ("block corner 20,30".into(), c(20.0, 30.0)),
            ("block corner 79,69".into(), c(79.0, 69.0)),
            ("corner cell 99,99".into(), c(99.0, 99.0)),
            ("north-west corner exactly".into(), (50.0, 0.0)),
            ("north of the corner (n60e000)".into(), (50.05, 0.05)),
            ("west of the corner (n50w010)".into(), (49.95, -0.05)),
            ("ocean far from land".into(), (49.5, 9.5)),
            ("soil cell 10,100".into(), l(10.0, 100.0)),
            ("soil city filled 2 cells".into(), l(65.0, 95.0)),
            ("soil city filled 1 cell".into(), l(61.0, 92.0)),
            ("soil coast 1 cell".into(), l(10.0, 79.0)),
            ("soil sea 3 cells".into(), l(10.0, 77.0)),
            ("regions FRA/SWI border west".into(), l(59.0, 150.0)),
            ("regions FRA/SWI border east".into(), l(60.0, 150.0)),
            ("soil and regions only (41 N 1 E)".into(), (41.0, 1.0)),
            ("tile in the south (n40e000)".into(), (35.0, 5.0)),
            ("far tile (s20w050)".into(), (-23.55, -46.63)),
        ];
        for x in (0..100).step_by(9) {
            p.push((format!("sweep climate x={x}"), c(f64::from(x), 50.0)));
        }
        for y in (0..200).step_by(17) {
            p.push((format!("sweep layer y={y}"), l(30.0, f64::from(y))));
        }
        p.push(("latitude 91".into(), (91.0, 0.0)));
        p.push(("latitude -90.5".into(), (-90.5, 10.0)));
        p.into_iter().map(|(n, (a, b))| (n, a, b)).collect()
    }

    fn expected_json() -> String {
        let t = synthetic::tile(TileId::parse("n50e000").unwrap());
        let (soil, regions) = (layers::soil(), layers::regions());
        let ext = crate::extremes::synthetic::extremes();
        let ct = |id: TileId| (id == t.id()).then_some(&t);
        let st = |id: TileId| (id == soil.id()).then_some(&soil);
        let rt = |id: TileId| (id == regions.id()).then_some(&regions);
        let et = |id: TileId| (id == ext.id()).then_some(&ext);
        let g = Getters {
            climate: &ct,
            soil: Some(&st),
            regions: Some(&rt),
            extremes: Some(&et),
        };
        let points: Vec<Value> = points()
            .iter()
            .map(|(name, lat, lon)| point(name, (*lat, *lon), g, 2))
            .collect();
        let tiles = json!({
            "climate": "tests/fixtures/synthetic-n50e000.tif",
            "extremes": "tests/fixtures/synthetic-extremes-n50e000.tif",
            "soil": "tests/fixtures/synthetic-soil-n50e000.tif",
            "regions": "tests/fixtures/synthetic-regions-n50e000.tif",
        });
        to_lines(&[("tiles", tiles)], &points, &tables())
    }

    /// `tests/fixtures/synthetic-expected.json` is what the crate answers
    /// on the committed synthetic tiles. Regenerate with
    /// `UPDATE_FIXTURES=1 cargo test vectors`.
    #[test]
    fn synthetic_expected_is_current() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(EXPECTED);
        let fresh = expected_json();
        if std::env::var_os("UPDATE_FIXTURES").is_some() {
            std::fs::write(&path, &fresh).unwrap();
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        if let Err(e) = same_vectors(&committed, &fresh) {
            panic!("{EXPECTED} is stale ({e}): run UPDATE_FIXTURES=1 cargo test vectors");
        }
    }

    #[test]
    fn synthetic_vectors_cover_both_readers() {
        let v: Value = serde_json::from_str(&expected_json()).unwrap();
        let points = v["points"].as_array().unwrap();
        let own = |b: bool| points.iter().filter(|p| p["own_tile"] == b).count();
        assert!(own(true) > 30, "most points: a single-tile reader agrees");
        // Across the north edge: only the cross-tile reader finds the corner.
        let north = points
            .iter()
            .find(|p| p["name"] == "north of the corner (n60e000)")
            .unwrap();
        assert_eq!(north["own_tile"], false);
        assert_eq!(north["expect"]["climate"]["distance_cells"], 1.0);
        let invalid = points.iter().filter(|p| p["expect"] == "invalid").count();
        assert_eq!(invalid, 2);
        let city = points
            .iter()
            .find(|p| p["name"] == "soil city filled 2 cells")
            .unwrap();
        assert_eq!(city["expect"]["soil"]["fill_distance_cells"], 2);
        assert_eq!(city["expect"]["regions"]["iso_a2"], "CH");
    }

    #[test]
    fn tables_match_the_unit_tests() {
        let t = tables();
        let margin = t["margin"].as_array().unwrap();
        let aw = margin
            .iter()
            .find(|m| m["symbol"] == "Aw" && m["t_cold"] == 16.0)
            .unwrap();
        assert_eq!(aw["margin"], 2.0);
        assert_eq!(t["legend"][15]["symbol"], "Cfb");
        assert_eq!(t["legend"][31]["symbol"], "");
        let tex = t["texture"].as_array().unwrap();
        assert_eq!(tex[3]["class"], "loam");
        assert_eq!(tex[16]["sand_pct"], Value::Null);
        assert_eq!(tex[16]["class"], Value::Null);
        assert_eq!(t["codes"]["encode"][0]["stored"], 5 * 676 + 17 * 26);
        assert_eq!(t["decode"][0]["value"], -12.3);
        assert_eq!(t["decode"][3]["value"], Value::Null);
    }

    #[test]
    fn dry_month_vectors_match_monthly_summary() {
        let t = tables();
        let dry = t["dry_month"].as_array().unwrap();
        assert_eq!(dry.len(), 10);
        assert_eq!(dry[0]["mask"], 0x01F0);
        assert_eq!(dry[0]["dry_months"], 5);
        assert_eq!(dry[1]["mask"], 0);
        assert_eq!(dry[1]["dry_months"], 0);
        assert_eq!(dry[2]["mask"], 1 << 6);
        assert_eq!(dry[2]["dry_months"], 1);
        assert_eq!(dry[3]["mask"], 0);
        assert_eq!(dry[4]["mask"], 0);
        assert_eq!(dry[5]["mask"], 0x0FFF);
        assert_eq!(dry[5]["dry_months"], 12);
        assert_eq!(dry[6]["mask"], 0);
        assert_eq!(dry[7]["mask"], 0x01C3);
        assert_eq!(dry[7]["dry_months"], 5);
        assert_eq!(dry[9]["mask"], Value::Null);
        assert_eq!(dry[9]["dry_months"], Value::Null);
        for (i, (_, t_arr, p_arr)) in DRY_MONTH_CASES.iter().enumerate() {
            let sum = MonthlySummary::new(t_arr, p_arr, 0.0);
            let mask = dry_month_mask(t_arr, p_arr);
            assert_eq!(
                sum.map(|s| u32::from(s.dry_months)),
                mask.map(i16::count_ones),
                "case {i}"
            );
            if let Some(m) = mask {
                assert_eq!(m & !0x0FFF, 0, "reserved/sign bits must be 0 in case {i}");
            }
        }
    }

    #[test]
    fn stale_vectors_name_the_difference() {
        let a = r#"{"cli": "x 0.2.0", "points": [{"name": "a", "v": 1}], "tables": {}}"#;
        let b = r#"{"cli": "x 0.2.1", "points": [{"name": "a", "v": 1}], "tables": {}}"#;
        let c = r#"{"cli": "x 0.2.1", "points": [{"name": "a", "v": 2}], "tables": {}}"#;
        assert_eq!(same_vectors(a, b), Ok(()), "a version bump is not a change");
        assert_eq!(same_vectors(a, c), Err("point \"a\" differs".into()));
        assert!(same_vectors("", b).is_err());
    }
}
