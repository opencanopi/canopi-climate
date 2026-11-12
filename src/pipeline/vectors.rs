//! `canopi-climate vectors`: the lookup vectors of a release grid
//! (`vectors/lookup.json`, docs/format.md), from a list of named points.

use std::path::Path;

use serde_json::{Value, json};

use super::sample::Grid;
use super::{PipelineError, Result, sha256_file};
use crate::vectors::{same_vectors, tables, to_lines};

/// Read `name,lat,lon` rows (header required; names may be quoted).
///
/// # Errors
/// CSV errors, a row whose coordinates are not numbers.
pub fn read_points(text: &str) -> Result<Vec<(String, f64, f64)>> {
    let mut rdr = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(text.as_bytes());
    let headers = rdr
        .headers()
        .map_err(|e| PipelineError::Data(e.to_string()))?
        .clone();
    if headers.iter().collect::<Vec<_>>() != ["name", "lat", "lon"] {
        return Err(PipelineError::Data(
            "points: the header must be name,lat,lon".into(),
        ));
    }
    let mut out = Vec::new();
    for rec in rdr.records() {
        let rec = rec.map_err(|e| PipelineError::Data(e.to_string()))?;
        let num = |i: usize| -> Result<f64> {
            rec.get(i)
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| PipelineError::Data(format!("points: bad row {rec:?}")))
        };
        out.push((rec.get(0).unwrap_or("").to_string(), num(1)?, num(2)?));
    }
    Ok(out)
}

/// The vectors file for `points` on a grid: one line per point, each with
/// the expected `lookup` JSON (or `"invalid"`) and `own_tile` (whether a
/// single-tile reader agrees), then the rule tables.
///
/// # Errors
/// Unreadable manifest or tiles.
pub fn vectors(grid: &mut Grid, points: &[(String, f64, f64)], radius: usize) -> Result<String> {
    let mut rows = Vec::with_capacity(points.len());
    for (name, lat, lon) in points {
        if crate::grid::TileId::of(*lat, *lon).is_none() {
            rows.push(json!({ "name": name, "lat": lat, "lon": lon, "own_tile": false, "expect": "invalid" }));
            continue;
        }
        let all = grid.site(*lat, *lon, radius, false)?;
        let own = grid.site(*lat, *lon, radius, true)?;
        rows.push(json!({
            "name": name, "lat": lat, "lon": lon,
            "own_tile": all.same_values(&own),
            "expect": all.json(),
        }));
    }
    let manifest = sha256_file(&grid.dir().join("manifest.json"))?;
    let cli = format!("canopi-climate {}", env!("CARGO_PKG_VERSION"));
    let head = [
        ("cli", Value::from(cli)),
        ("grid_manifest_sha256", Value::from(manifest)),
    ];
    Ok(to_lines(&head, &rows, &tables()))
}

/// Write the vectors to `out`, or with `check`, compare them with that
/// committed file (ignoring `cli`). Returns the problem, if any.
///
/// # Errors
/// I/O errors.
pub fn write_or_check(
    fresh: &str,
    out: Option<&Path>,
    check: Option<&Path>,
) -> Result<Option<String>> {
    if let Some(c) = check {
        let committed = std::fs::read_to_string(c).map_err(|e| PipelineError::io(c, e))?;
        return Ok(same_vectors(&committed, fresh).err());
    }
    match out {
        Some(p) => std::fs::write(p, fresh).map_err(|e| PipelineError::io(p, e))?,
        None => print!("{fresh}"),
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_need_the_header_and_numbers() {
        let p = read_points("name,lat,lon\n\"Suva, Fiji\",-18.14,178.44\nPole,-90,0\n").unwrap();
        assert_eq!(p[0], ("Suva, Fiji".to_string(), -18.14, 178.44));
        assert_eq!(p.len(), 2);
        assert!(read_points("lat,lon\n1,2\n").is_err());
        assert!(read_points("name,lat,lon\nx,north,2\n").is_err());
    }
}
