//! Reading a built grid from disk: point lookups and CSV sampling.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::{PipelineError, Result, sha256_file};
use crate::grid::TileId;
use crate::lookup::{Lookup, lookup_within};
use crate::manifest::{FORMAT, FORMAT_VERSION, Manifest};
use crate::tile::Tile;

/// Decoded tiles kept in memory at most (about 1.4 MB each).
const CACHE_TILES: usize = 96;

/// A grid directory (`manifest.json` + `tiles/`).
pub struct Grid {
    dir: PathBuf,
    manifest: Manifest,
    paths: HashMap<String, String>,
    cache: HashMap<TileId, Tile>,
}

impl Grid {
    /// Open a grid directory and check its manifest.
    ///
    /// # Errors
    /// Missing or unreadable manifest, or an unknown format.
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join("manifest.json");
        let text = std::fs::read_to_string(&path).map_err(|e| PipelineError::io(&path, e))?;
        let manifest: Manifest = serde_json::from_str(&text)
            .map_err(|e| PipelineError::Data(format!("{}: {e}", path.display())))?;
        if manifest.format != FORMAT || manifest.format_version != FORMAT_VERSION {
            return Err(PipelineError::Data(format!(
                "{}: format {} v{}, this build reads {FORMAT} v{FORMAT_VERSION}",
                path.display(),
                manifest.format,
                manifest.format_version
            )));
        }
        let paths = manifest
            .tiles
            .iter()
            .map(|t| (t.name.clone(), t.path.clone()))
            .collect();
        Ok(Self {
            dir: dir.to_path_buf(),
            manifest,
            paths,
            cache: HashMap::new(),
        })
    }

    /// The manifest.
    #[must_use]
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn tile(&mut self, id: TileId) -> Result<Option<&Tile>> {
        let Some(rel) = self.paths.get(&id.name()) else {
            return Ok(None); // no land in this tile
        };
        if !self.cache.contains_key(&id) {
            if self.cache.len() >= CACHE_TILES {
                self.cache.clear();
            }
            let path = self.dir.join(rel);
            let bytes = std::fs::read(&path).map_err(|e| PipelineError::io(&path, e))?;
            let tile = Tile::decode(&bytes)?;
            if tile.id() != id {
                return Err(PipelineError::Data(format!(
                    "{}: holds tile {}",
                    path.display(),
                    tile.id()
                )));
            }
            self.cache.insert(id, tile);
        }
        Ok(self.cache.get(&id))
    }

    /// Look a point up. `Ok(None)`: no complete cell within `radius`.
    ///
    /// # Errors
    /// Invalid coordinate, unreadable tile.
    pub fn lookup(&mut self, lat: f64, lon: f64, radius: usize) -> Result<Option<Lookup>> {
        let id = TileId::of(lat, lon)
            .ok_or_else(|| PipelineError::Data(format!("invalid coordinate {lat}, {lon}")))?;
        let Some(tile) = self.tile(id)? else {
            return Ok(None);
        };
        lookup_within(tile, lat, lon, radius).map_err(|e| PipelineError::Data(e.to_string()))
    }

    /// Check every tile against the manifest (size and SHA-256); returns
    /// the problems found.
    ///
    /// # Errors
    /// I/O errors other than a missing file.
    pub fn verify(&self) -> Result<Vec<String>> {
        let mut problems = Vec::new();
        for t in &self.manifest.tiles {
            let p = self.dir.join(&t.path);
            match std::fs::metadata(&p) {
                Err(_) => problems.push(format!("missing {}", t.path)),
                Ok(m) if m.len() != t.bytes => problems.push(format!(
                    "{}: {} bytes, expected {}",
                    t.path,
                    m.len(),
                    t.bytes
                )),
                Ok(_) => {
                    if sha256_file(&p)? != t.sha256 {
                        problems.push(format!("{}: checksum mismatch", t.path));
                    }
                }
            }
        }
        Ok(problems)
    }
}

/// Counts from a sampling run.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SampleSummary {
    /// Points read.
    pub points: usize,
    /// Points with data.
    pub ok: usize,
    /// Points with no complete cell nearby.
    pub no_data: usize,
    /// Rows whose coordinates could not be read.
    pub invalid: usize,
}

const OUT_HEADER: [&str; 20] = [
    "id",
    "lat",
    "lon",
    "status",
    "cell_lat",
    "cell_lon",
    "distance_cells",
    "period",
    "scenario",
    "koppen",
    "koppen_code",
    "koppen_temperature_margin_c",
    "tmin_cold_c",
    "tmax_warm_c",
    "tmean_cold_c",
    "tmean_warm_c",
    "precip_mm",
    "aridity_index",
    "dry_months",
    "tile",
];

fn column(headers: &csv::StringRecord, names: &[&str]) -> Option<usize> {
    headers
        .iter()
        .position(|h| names.iter().any(|n| h.trim().eq_ignore_ascii_case(n)))
}

/// Sample a CSV of points (header with `lat`/`latitude` and
/// `lon`/`lng`/`longitude`, optional `id`) and write one row per point and
/// slot (long format). Points without data get one row with their status.
///
/// # Errors
/// CSV errors, a missing coordinate column, unreadable tiles.
pub fn sample_csv(
    grid: &mut Grid,
    input: impl Read,
    output: impl Write,
    radius: usize,
) -> Result<SampleSummary> {
    let csv_err = |e: csv::Error| PipelineError::Data(e.to_string());
    let mut rdr = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(input);
    let headers = rdr.headers().map_err(csv_err)?.clone();
    let lat_col = column(&headers, &["lat", "latitude"])
        .ok_or_else(|| PipelineError::Data("no lat/latitude column".into()))?;
    let lon_col = column(&headers, &["lon", "lng", "long", "longitude"])
        .ok_or_else(|| PipelineError::Data("no lon/lng/longitude column".into()))?;
    let id_col = column(&headers, &["id"]);
    let mut w = csv::Writer::from_writer(output);
    w.write_record(OUT_HEADER).map_err(csv_err)?;
    let mut summary = SampleSummary::default();
    for (n, rec) in rdr.records().enumerate() {
        let rec = rec.map_err(csv_err)?;
        summary.points += 1;
        let id = id_col
            .and_then(|i| rec.get(i))
            .map_or_else(|| (n + 1).to_string(), str::to_string);
        let lat_s = rec.get(lat_col).unwrap_or("");
        let lon_s = rec.get(lon_col).unwrap_or("");
        let coords = lat_s.parse::<f64>().ok().zip(lon_s.parse::<f64>().ok());
        let found = match coords {
            Some((lat, lon)) if TileId::of(lat, lon).is_some() => grid.lookup(lat, lon, radius)?,
            _ => {
                summary.invalid += 1;
                write_status(&mut w, &id, lat_s, lon_s, "invalid")?;
                continue;
            }
        };
        match found {
            None => {
                summary.no_data += 1;
                write_status(&mut w, &id, lat_s, lon_s, "no_data")?;
            }
            Some(f) => {
                summary.ok += 1;
                write_rows(&mut w, &id, lat_s, lon_s, &f)?;
            }
        }
    }
    w.flush().map_err(|e| PipelineError::Data(e.to_string()))?;
    Ok(summary)
}

fn write_status<W: Write>(
    w: &mut csv::Writer<W>,
    id: &str,
    lat: &str,
    lon: &str,
    status: &str,
) -> Result<()> {
    let mut row = vec![id, lat, lon, status];
    row.resize(OUT_HEADER.len(), "");
    w.write_record(&row)
        .map_err(|e| PipelineError::Data(e.to_string()))
}

fn write_rows<W: Write>(
    w: &mut csv::Writer<W>,
    id: &str,
    lat: &str,
    lon: &str,
    f: &Lookup,
) -> Result<()> {
    for p in &f.periods {
        let row = [
            id.to_string(),
            lat.to_string(),
            lon.to_string(),
            "ok".to_string(),
            format!("{:.2}", f.cell_lat),
            format!("{:.2}", f.cell_lon),
            format!("{:.2}", f.distance_cells),
            p.period.to_string(),
            p.scenario.to_string(),
            p.koppen.to_string(),
            p.koppen_code.to_string(),
            format!("{:.1}", p.koppen_temperature_margin_c),
            format!("{:.1}", p.tmin_cold_c),
            format!("{:.1}", p.tmax_warm_c),
            format!("{:.1}", p.tmean_cold_c),
            format!("{:.1}", p.tmean_warm_c),
            format!("{:.0}", p.precip_mm),
            p.aridity_index
                .map_or_else(String::new, |a| format!("{a:.2}")),
            p.dry_months.to_string(),
            f.tile.clone(),
        ];
        w.write_record(&row)
            .map_err(|e| PipelineError::Data(e.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{assemble, cog};
    use crate::tile::synthetic;

    /// A one-tile grid on disk.
    fn grid_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("canopi-climate-grid-{}-{name}", std::process::id()));
        std::fs::create_dir_all(dir.join("tiles")).unwrap();
        let id = TileId::parse("n50e000").unwrap();
        let t = synthetic::tile(id);
        let path = dir.join("tiles").join(id.file_name());
        let bytes = cog::write_tile_checked(&t, &path).unwrap();
        let spec = crate::manifest::TileSpec {
            name: id.name(),
            path: format!("tiles/{}", id.file_name()),
            bytes: bytes.len() as u64,
            sha256: crate::pipeline::sha256_bytes(&bytes),
            complete_cells: t.complete_cells() as u32,
        };
        let m = assemble::manifest(vec![spec]);
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_string(&m).unwrap(),
        )
        .unwrap();
        dir
    }

    #[test]
    fn grid_lookup_and_verify() {
        let dir = grid_dir("lookup");
        let mut g = Grid::open(&dir).unwrap();
        assert!(g.verify().unwrap().is_empty());
        // Cell (50, 40) of n50e000: 45.95 N, 5.05 E.
        let r = g.lookup(45.95, 5.05, 2).unwrap().unwrap();
        assert_eq!(r.periods.len(), 9);
        // A tile absent from the manifest is all ocean.
        assert_eq!(g.lookup(-30.0, -140.0, 2).unwrap(), None);
        assert!(g.lookup(95.0, 0.0, 2).is_err());
    }

    #[test]
    fn csv_sampling_writes_long_rows() {
        let dir = grid_dir("csv");
        let mut g = Grid::open(&dir).unwrap();
        let input = "id,latitude,longitude\na,45.95,5.05\nb,49.5,9.5\nc,abc,1\nd,-30,-140\n";
        let mut out = Vec::new();
        let s = sample_csv(&mut g, input.as_bytes(), &mut out, 2).unwrap();
        assert_eq!(
            s,
            SampleSummary {
                points: 4,
                ok: 1,
                no_data: 2,
                invalid: 1
            }
        );
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], OUT_HEADER.join(","));
        assert_eq!(lines.len(), 1 + 9 + 3);
        assert!(lines[1].starts_with("a,45.95,5.05,ok,45.95,5.05,0.00,1991-2020,historical,"));
        assert!(lines[9].contains(",2071-2099,ssp585,"));
        assert!(lines[10].starts_with("b,49.5,9.5,no_data,"));
        assert!(lines[11].starts_with("c,abc,1,invalid,"));
        assert!(lines[12].starts_with("d,-30,-140,no_data,"));
    }

    #[test]
    fn csv_without_coordinates_is_an_error() {
        let dir = grid_dir("nocoords");
        let mut g = Grid::open(&dir).unwrap();
        assert!(sample_csv(&mut g, "x,y\n1,2\n".as_bytes(), Vec::new(), 2).is_err());
    }
}
