//! Reading a built grid from disk: point lookups and CSV sampling.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::{PipelineError, Result};
use crate::grid::TileId;
use crate::layer::{LAYER_LOOKUP_RADIUS, LayerTile};
use crate::lookup::{Lookup, lookup_across, search_tiles};
use crate::manifest::{FORMAT, FORMAT_VERSION, Manifest};
use crate::regions::{REGION_BANDS, RegionsLookup};
use crate::soil::{SOIL_BANDS, SoilLookup};
use crate::tile::Tile;
use crate::vectors::{LayerTiles, Site};

/// Decoded tiles kept in memory at most (about 1.4 MB each).
const CACHE_TILES: usize = 96;

/// Decoded layer tiles kept in memory at most (about 160 KB each).
const CACHE_LAYER_TILES: usize = 64;

/// A grid directory (`manifest.json`, `tiles/`, and the site layers'
/// directories).
pub struct Grid {
    dir: PathBuf,
    manifest: Manifest,
    paths: HashMap<String, String>,
    cache: HashMap<TileId, Tile>,
    /// (layer id, tile name) -> path.
    layer_paths: HashMap<(String, String), String>,
    layer_cache: HashMap<(String, TileId), LayerTile>,
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
        let layer_paths = manifest
            .layers
            .iter()
            .flat_map(|l| {
                l.tiles
                    .iter()
                    .map(|t| ((l.id.clone(), t.name.clone()), t.path.clone()))
            })
            .collect();
        Ok(Self {
            dir: dir.to_path_buf(),
            manifest,
            paths,
            cache: HashMap::new(),
            layer_paths,
            layer_cache: HashMap::new(),
        })
    }

    /// The manifest.
    #[must_use]
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Decode a tile into the cache (no-op for cached tiles and for tiles
    /// without land, which have no file).
    fn load(&mut self, id: TileId) -> Result<()> {
        let Some(rel) = self.paths.get(&id.name()) else {
            return Ok(());
        };
        if !self.cache.contains_key(&id) {
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
        Ok(())
    }

    /// Look a point up. `Ok(None)`: no complete cell within `radius`. The
    /// search crosses tile edges (and the antimeridian), unlike the
    /// single-tile [`crate::lookup::lookup`] used in WebAssembly.
    ///
    /// # Errors
    /// Invalid coordinate, unreadable tile.
    pub fn lookup(&mut self, lat: f64, lon: f64, radius: usize) -> Result<Option<Lookup>> {
        let bad = |e: crate::lookup::LookupError| PipelineError::Data(format!("{lat}, {lon}: {e}"));
        self.load_climate(lat, lon, radius)?;
        let cache = &self.cache;
        lookup_across(|id| cache.get(&id), lat, lon, radius).map_err(bad)
    }

    /// Decode the climate tiles a search of `radius` cells around a point
    /// can reach into the cache.
    fn load_climate(&mut self, lat: f64, lon: f64, radius: usize) -> Result<()> {
        let bad = |e: crate::lookup::LookupError| PipelineError::Data(format!("{lat}, {lon}: {e}"));
        let ids = search_tiles(lat, lon, radius).map_err(bad)?;
        if self.cache.len() + ids.len() > CACHE_TILES {
            self.cache.clear();
        }
        for id in &ids {
            self.load(*id)?;
        }
        Ok(())
    }

    /// Climate (within `radius` cells), soil and regions at a point, as
    /// `canopi-climate lookup` prints them ([`Site::json`]). With
    /// `own_tile_only`, the answer of a reader given the point's own tiles
    /// only (the WebAssembly exports).
    ///
    /// # Errors
    /// Invalid coordinate, unreadable tile.
    pub fn site(&mut self, lat: f64, lon: f64, radius: usize, own_tile_only: bool) -> Result<Site> {
        self.load_climate(lat, lon, radius)?;
        let r = LAYER_LOOKUP_RADIUS;
        let (soil, regions) = (self.has_layer("soil"), self.has_layer("regions"));
        if soil {
            self.load_layer("soil", SOIL_BANDS, lat, lon, r)?;
        }
        if regions {
            self.load_layer("regions", REGION_BANDS, lat, lon, r)?;
        }
        let (cache, layers) = (&self.cache, &self.layer_cache);
        let climate_tiles = |id: TileId| cache.get(&id);
        let soil_tiles = |id: TileId| layers.get(&("soil".to_string(), id));
        let region_tiles = |id: TileId| layers.get(&("regions".to_string(), id));
        crate::vectors::site(
            &climate_tiles,
            soil.then_some(&soil_tiles as LayerTiles<'_, '_>),
            regions.then_some(&region_tiles as LayerTiles<'_, '_>),
            (lat, lon, radius),
            own_tile_only,
        )
        .map_err(|e| PipelineError::Data(format!("{lat}, {lon}: {e}")))
    }

    /// The grid directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether the grid has a site layer (`soil`, `regions`).
    #[must_use]
    pub fn has_layer(&self, id: &str) -> bool {
        self.manifest.layers.iter().any(|l| l.id == id)
    }

    /// Decode the layer tiles a search of `radius` cells around a point
    /// can reach into the cache.
    fn load_layer(
        &mut self,
        id: &str,
        bands: usize,
        lat: f64,
        lon: f64,
        radius: usize,
    ) -> Result<()> {
        let bad = |e: crate::lookup::LookupError| PipelineError::Data(format!("{lat}, {lon}: {e}"));
        let ids = crate::layer::search_tiles(lat, lon, radius).map_err(bad)?;
        if self.layer_cache.len() + ids.len() > CACHE_LAYER_TILES {
            self.layer_cache.clear();
        }
        for t in ids {
            let key = (id.to_string(), t);
            if self.layer_cache.contains_key(&key) {
                continue;
            }
            let Some(rel) = self.layer_paths.get(&(id.to_string(), t.name())) else {
                continue;
            };
            let path = self.dir.join(rel);
            let bytes = std::fs::read(&path).map_err(|e| PipelineError::io(&path, e))?;
            let tile = LayerTile::decode(&bytes, bands)?;
            if tile.id() != t {
                return Err(PipelineError::Data(format!(
                    "{}: holds tile {}",
                    path.display(),
                    tile.id()
                )));
            }
            self.layer_cache.insert(key, tile);
        }
        Ok(())
    }

    /// Soil at a point (searching up to `radius` cells, across tiles);
    /// `Ok(None)` without a soil layer or without a soil cell nearby.
    ///
    /// # Errors
    /// Invalid coordinate, unreadable tile.
    pub fn soil(&mut self, lat: f64, lon: f64, radius: usize) -> Result<Option<SoilLookup>> {
        if !self.has_layer("soil") {
            return Ok(None);
        }
        self.load_layer("soil", SOIL_BANDS, lat, lon, radius)?;
        let cache = &self.layer_cache;
        crate::soil::lookup_across(|t| cache.get(&("soil".to_string(), t)), lat, lon, radius)
            .map_err(|e| PipelineError::Data(format!("{lat}, {lon}: {e}")))
    }

    /// Regions at a point (searching up to `radius` cells, across tiles);
    /// `Ok(None)` without a regions layer.
    ///
    /// # Errors
    /// Invalid coordinate, unreadable tile.
    pub fn regions(&mut self, lat: f64, lon: f64, radius: usize) -> Result<Option<RegionsLookup>> {
        if !self.has_layer("regions") {
            return Ok(None);
        }
        self.load_layer("regions", REGION_BANDS, lat, lon, radius)?;
        let cache = &self.layer_cache;
        let tiles = |t: TileId| cache.get(&("regions".to_string(), t));
        crate::regions::lookup_across(tiles, lat, lon, radius)
            .map(Some)
            .map_err(|e| PipelineError::Data(format!("{lat}, {lon}: {e}")))
    }

    /// Check every tile against the manifest (size and SHA-256) and decode
    /// it with the header rules of docs/format.md (the website's reader
    /// refuses the same layouts); returns the problems found.
    ///
    /// # Errors
    /// I/O errors other than a missing file.
    pub fn verify(&self) -> Result<Vec<String>> {
        let mut problems = Vec::new();
        let climate = self.manifest.tiles.iter().map(|t| (t, None));
        let layer_tiles = self
            .manifest
            .layers
            .iter()
            .flat_map(|l| l.tiles.iter().map(|t| (t, Some(l.bands.len()))));
        for (t, layer_bands) in climate.chain(layer_tiles) {
            let p = self.dir.join(&t.path);
            match std::fs::read(&p) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    problems.push(format!("missing {}", t.path));
                }
                Err(e) => return Err(PipelineError::io(&p, e)),
                Ok(b) if b.len() as u64 != t.bytes => problems.push(format!(
                    "{}: {} bytes, expected {}",
                    t.path,
                    b.len(),
                    t.bytes
                )),
                Ok(b) if super::sha256_bytes(&b) != t.sha256 => {
                    problems.push(format!("{}: checksum mismatch", t.path));
                }
                Ok(b) => {
                    if let Some(p) = decode_problem(&b, layer_bands, &t.name) {
                        problems.push(format!("{}: {p}", t.path));
                    }
                }
            }
        }
        Ok(problems)
    }
}

/// Why a tile file does not decode as the tile `name` (`None`: it does).
/// `layer_bands`: `None` for a climate tile, else the layer's band count.
fn decode_problem(bytes: &[u8], layer_bands: Option<usize>, name: &str) -> Option<String> {
    let id = match layer_bands {
        None => Tile::decode(bytes).map(|t| t.id()),
        Some(bands) => LayerTile::decode(bytes, bands).map(|t| t.id()),
    };
    match id {
        Err(e) => Some(e.to_string()),
        Ok(id) if id.name() != name => Some(format!("holds tile {id}")),
        Ok(_) => None,
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

/// Columns `--regions` appends to every row.
pub const REGION_COLUMNS: [&str; 2] = ["wgsrpd_l3", "iso_a2"];

/// How [`sample_csv_with`] samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleOptions {
    /// Search radius in climate cells for the nearest complete cell.
    pub radius: usize,
    /// Append [`REGION_COLUMNS`]: each point's WGSRPD level-3 and ISO
    /// 3166-1 alpha-2 codes, as `lookup` gives them (searching
    /// [`LAYER_LOOKUP_RADIUS`] layer cells), on each of its rows; empty
    /// when no code is near or the coordinates are invalid.
    pub regions: bool,
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
    let opts = SampleOptions {
        radius,
        regions: false,
    };
    sample_csv_with(grid, input, output, &opts)
}

/// [`sample_csv`] with options. Without [`SampleOptions::regions`] the
/// output is the same as `sample_csv`'s.
///
/// # Errors
/// As [`sample_csv`]; with `regions`, a grid without the regions layer.
pub fn sample_csv_with(
    grid: &mut Grid,
    input: impl Read,
    output: impl Write,
    opts: &SampleOptions,
) -> Result<SampleSummary> {
    if opts.regions && !grid.has_layer("regions") {
        return Err(PipelineError::Data(
            "--regions: the grid has no regions layer (canopi-climate 0.2 or later)".into(),
        ));
    }
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
    let mut header = OUT_HEADER.to_vec();
    if opts.regions {
        header.extend(REGION_COLUMNS);
    }
    w.write_record(&header).map_err(csv_err)?;
    let mut summary = SampleSummary::default();
    for (n, rec) in rdr.records().enumerate() {
        let rec = rec.map_err(csv_err)?;
        summary.points += 1;
        let id = id_col
            .and_then(|i| rec.get(i))
            .map_or_else(|| (n + 1).to_string(), str::to_string);
        let lat_s = rec.get(lat_col).unwrap_or("");
        let lon_s = rec.get(lon_col).unwrap_or("");
        let point = Point {
            id: &id,
            lat: lat_s,
            lon: lon_s,
        };
        let coords = lat_s.parse::<f64>().ok().zip(lon_s.parse::<f64>().ok());
        let Some((lat, lon)) = coords.filter(|&(lat, lon)| TileId::of(lat, lon).is_some()) else {
            summary.invalid += 1;
            write_status(&mut w, &point, "invalid", opts.regions.then(Codes::default))?;
            continue;
        };
        let codes = if opts.regions {
            Some(region_codes(grid, lat, lon)?)
        } else {
            None
        };
        match grid.lookup(lat, lon, opts.radius)? {
            None => {
                summary.no_data += 1;
                write_status(&mut w, &point, "no_data", codes)?;
            }
            Some(f) => {
                summary.ok += 1;
                write_rows(&mut w, &point, &f, codes.as_ref())?;
            }
        }
    }
    w.flush().map_err(|e| PipelineError::Data(e.to_string()))?;
    Ok(summary)
}

/// A point as read (coordinates kept as written).
struct Point<'a> {
    id: &'a str,
    lat: &'a str,
    lon: &'a str,
}

/// Region codes of a point (empty strings: none near).
#[derive(Default)]
struct Codes {
    wgsrpd_l3: String,
    iso_a2: String,
}

fn region_codes(grid: &mut Grid, lat: f64, lon: f64) -> Result<Codes> {
    let r = grid.regions(lat, lon, LAYER_LOOKUP_RADIUS)?;
    Ok(r.map_or_else(Codes::default, |r| Codes {
        wgsrpd_l3: r.wgsrpd_l3.unwrap_or_default(),
        iso_a2: r.iso_a2.unwrap_or_default(),
    }))
}

fn write_status<W: Write>(
    w: &mut csv::Writer<W>,
    p: &Point<'_>,
    status: &str,
    codes: Option<Codes>,
) -> Result<()> {
    let mut row = vec![p.id, p.lat, p.lon, status];
    row.resize(OUT_HEADER.len(), "");
    if let Some(c) = &codes {
        row.extend([c.wgsrpd_l3.as_str(), c.iso_a2.as_str()]);
    }
    w.write_record(&row)
        .map_err(|e| PipelineError::Data(e.to_string()))
}

fn write_rows<W: Write>(
    w: &mut csv::Writer<W>,
    pt: &Point<'_>,
    f: &Lookup,
    codes: Option<&Codes>,
) -> Result<()> {
    for p in &f.periods {
        let mut row = vec![
            pt.id.to_string(),
            pt.lat.to_string(),
            pt.lon.to_string(),
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
        if let Some(c) = codes {
            row.extend([c.wgsrpd_l3.clone(), c.iso_a2.clone()]);
        }
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

    /// [`grid_dir`] plus the synthetic regions layer.
    fn grid_dir_with_regions(name: &str) -> PathBuf {
        let dir = grid_dir(name);
        std::fs::create_dir_all(dir.join("regions")).unwrap();
        let t = crate::layer::synthetic::regions();
        let rel = format!("regions/{}", t.id().file_name());
        let bytes = cog::write_layer_tile_checked(&t, &dir.join(&rel)).unwrap();
        let spec = crate::manifest::TileSpec {
            name: t.id().name(),
            path: rel,
            bytes: bytes.len() as u64,
            sha256: crate::pipeline::sha256_bytes(&bytes),
            complete_cells: 0,
        };
        let path = dir.join("manifest.json");
        let mut m: Manifest =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        m.layers
            .push(crate::pipeline::regions::layer_spec(vec![spec]));
        std::fs::write(&path, serde_json::to_string(&m).unwrap()).unwrap();
        dir
    }

    /// Points for the sampling goldens: inland, snapped, sea, a point
    /// with regions but no climate (41 N 1 E), invalid, no id column.
    const SAMPLE_POINTS: &str = "latitude,longitude\n45.95,5.05\n45.95,2.05\n49.5,9.5\n41.0,1.0\n45.25,3.25\nabc,1\n50.05,0.05\n91,0\n";

    /// `sample` output without `--regions` stays byte-identical to v0.2.0
    /// (the golden was written by the v0.2.0 code, before the flag).
    #[test]
    fn csv_sampling_matches_the_v0_2_0_golden() {
        let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample-v0.2.0.csv");
        for dir in [grid_dir("golden"), grid_dir_with_regions("golden-regions")] {
            let mut g = Grid::open(&dir).unwrap();
            let mut out = Vec::new();
            sample_csv(&mut g, SAMPLE_POINTS.as_bytes(), &mut out, 2).unwrap();
            if std::env::var_os("UPDATE_FIXTURES").is_some() {
                std::fs::write(&golden, &out).unwrap();
            }
            assert_eq!(
                String::from_utf8(out).unwrap(),
                std::fs::read_to_string(&golden).unwrap()
            );
        }
    }

    /// `--regions`: two more columns, the codes `lookup` gives for the
    /// point, the same on each of its rows (no-data points included).
    #[test]
    fn csv_sampling_with_regions_adds_the_lookup_codes() {
        let dir = grid_dir_with_regions("with-regions");
        let mut g = Grid::open(&dir).unwrap();
        let opts = SampleOptions {
            radius: 2,
            regions: true,
        };
        let mut out = Vec::new();
        let s = sample_csv_with(&mut g, SAMPLE_POINTS.as_bytes(), &mut out, &opts).unwrap();
        assert_eq!((s.points, s.ok, s.no_data, s.invalid), (8, 4, 2, 2));
        let text = String::from_utf8(out).unwrap();
        let mut lines = text.lines();
        let mut header = OUT_HEADER.to_vec();
        header.extend(REGION_COLUMNS);
        assert_eq!(lines.next(), Some(header.join(",").as_str()));
        let golden = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample-v0.2.0.csv"),
        )
        .unwrap();
        let mut rows = 0;
        for (line, old) in lines.zip(golden.lines().skip(1)) {
            rows += 1;
            let (base, codes) = line.rsplit_once(',').map_or((line, ""), |(a, b)| (a, b));
            let (base, l3) = base.rsplit_once(',').unwrap();
            assert_eq!(base, old, "the v0.2.0 columns are unchanged");
            let f: Vec<&str> = old.split(',').collect();
            let want = match (f[1].parse::<f64>(), f[2].parse::<f64>()) {
                (Ok(lat), Ok(lon)) if f[3] != "invalid" => {
                    let r = g
                        .regions(lat, lon, crate::layer::LAYER_LOOKUP_RADIUS)
                        .unwrap()
                        .unwrap();
                    (
                        r.wgsrpd_l3.unwrap_or_default(),
                        r.iso_a2.unwrap_or_default(),
                    )
                }
                _ => (String::new(), String::new()),
            };
            assert_eq!((l3.to_string(), codes.to_string()), want, "{line}");
        }
        assert_eq!(rows, golden.lines().count() - 1);
        assert!(text.contains("\n4,41.0,1.0,no_data,"));
        assert!(text.contains(",FRA,FR\n"), "{text}");
        assert!(text.contains(",SWI,CH\n"), "{text}");
        // A grid without the regions layer refuses the flag.
        let mut plain = Grid::open(&grid_dir("no-regions")).unwrap();
        let err = sample_csv_with(&mut plain, SAMPLE_POINTS.as_bytes(), Vec::new(), &opts);
        assert!(err.is_err());
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
        // 50.05 N, 0.05 E lies in n60e000 (no file), one cell north of
        // n50e000's land corner: the search crosses the tile edge.
        let r = g.lookup(50.05, 0.05, 2).unwrap().unwrap();
        assert_eq!((r.tile.as_str(), r.distance_cells), ("n50e000", 1.0));
        assert!(g.lookup(95.0, 0.0, 2).is_err());
    }

    #[test]
    fn verify_decodes_every_tile() {
        let dir = grid_dir("verify-decode");
        let path = dir.join("manifest.json");
        let mut m: Manifest =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // The same file listed under another tile's name: size and checksum
        // match, the georeferencing does not.
        let mut other = m.tiles[0].clone();
        other.name = "n40e000".into();
        m.tiles.push(other);
        std::fs::write(&path, serde_json::to_string(&m).unwrap()).unwrap();
        let problems = Grid::open(&dir).unwrap().verify().unwrap();
        assert_eq!(problems, ["tiles/n50e000.tif: holds tile n50e000"]);
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
