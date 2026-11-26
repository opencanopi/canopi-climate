//! Reading a built grid from disk: point lookups and CSV sampling.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::{PipelineError, Result};
use crate::analogues::AnalogueIndex;
use crate::extremes::{ExtremesLookup, ExtremesTile};
use crate::extremes_tail::{ExtremesTailLookup, ExtremesTailTile};
use crate::frost_calendar::{FrostCalendarLookup, FrostCalendarTile};
use crate::grid::TileId;
use crate::layer::{LAYER_LOOKUP_RADIUS, LayerTile};
use crate::lookup::{Lookup, lookup_across, search_tiles};
use crate::manifest::{FORMAT, FORMAT_VERSION, Manifest};
use crate::monthly::{MONTHLY_SLOT_INDICES, MonthlyLookup, MonthlyTile};
use crate::monthly_precip::{MonthlyPrecipLookup, MonthlyPrecipTile, PRECIP_LOOKUP_RADIUS};
use crate::phenology::{PhenologyLookup, PhenologyTile};
use crate::regions::{REGION_BANDS, RegionsLookup};
use crate::soil::{SOIL_BANDS, SoilLookup};
use crate::soil_plus::{SOIL_PLUS_BANDS, SoilPlusLookup};
use crate::terrain::{TerrainLookup, TerrainTile};
use crate::tile::Tile;
use crate::vectors::{
    ExtremesTailTiles, ExtremesTiles, FrostCalendarTiles, Getters, LayerTiles, MonthlyPrecipTiles,
    MonthlyTiles, PhenologyTiles, Site, TerrainTiles, WaterBalanceTiles,
};
use crate::water_balance::{WaterBalanceLookup, WaterBalanceTile};

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
    extremes_cache: HashMap<TileId, ExtremesTile>,
    extremes_tail_cache: HashMap<TileId, ExtremesTailTile>,
    frost_calendar_cache: HashMap<TileId, FrostCalendarTile>,
    monthly_cache: HashMap<TileId, MonthlyTile>,
    monthly_precip_cache: HashMap<TileId, MonthlyPrecipTile>,
    phenology_cache: HashMap<TileId, PhenologyTile>,
    water_balance_cache: HashMap<TileId, WaterBalanceTile>,
    terrain_cache: HashMap<TileId, TerrainTile>,
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
            extremes_cache: HashMap::new(),
            extremes_tail_cache: HashMap::new(),
            frost_calendar_cache: HashMap::new(),
            monthly_cache: HashMap::new(),
            monthly_precip_cache: HashMap::new(),
            phenology_cache: HashMap::new(),
            water_balance_cache: HashMap::new(),
            terrain_cache: HashMap::new(),
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
        let (soil, soil_plus, regions) = (
            self.has_layer("soil"),
            self.has_layer(crate::soil_plus::DIR),
            self.has_layer("regions"),
        );
        if soil {
            self.load_layer("soil", SOIL_BANDS, lat, lon, r)?;
        }
        if soil_plus {
            self.load_layer(crate::soil_plus::DIR, SOIL_PLUS_BANDS, lat, lon, r)?;
        }
        if regions {
            self.load_layer("regions", REGION_BANDS, lat, lon, r)?;
        }
        let extremes = self.has_layer(crate::extremes::DIR);
        if extremes {
            self.load_extremes(lat, lon, radius)?;
        }
        let extremes_tail = self.has_layer(crate::extremes_tail::DIR);
        if extremes_tail {
            self.load_extremes_tail(lat, lon, radius)?;
        }
        let frost_calendar = self.has_layer(crate::frost_calendar::DIR);
        if frost_calendar {
            self.load_frost_calendar(lat, lon, radius)?;
        }
        let monthly = self.has_layer(crate::monthly::DIR);
        if monthly {
            self.load_monthly(lat, lon, radius)?;
        }
        let monthly_precip = self.has_layer(crate::monthly_precip::DIR);
        if monthly_precip {
            self.load_monthly_precip(lat, lon, PRECIP_LOOKUP_RADIUS)?;
        }
        let phenology = self.has_layer(crate::phenology::DIR);
        if phenology {
            self.load_phenology(lat, lon, radius)?;
        }
        let water_balance = self.has_layer(crate::water_balance::DIR);
        if water_balance {
            self.load_water_balance(lat, lon, radius)?;
        }
        let terrain = self.has_layer(crate::terrain::DIR);
        if terrain {
            self.load_terrain(lat, lon, radius)?;
        }
        let (cache, layers, ext, ext_tail, frost, mon, mon_p, pheno, wb, tr) = (
            &self.cache,
            &self.layer_cache,
            &self.extremes_cache,
            &self.extremes_tail_cache,
            &self.frost_calendar_cache,
            &self.monthly_cache,
            &self.monthly_precip_cache,
            &self.phenology_cache,
            &self.water_balance_cache,
            &self.terrain_cache,
        );
        let climate_tiles = |id: TileId| cache.get(&id);
        let soil_tiles = |id: TileId| layers.get(&("soil".to_string(), id));
        let soil_plus_tiles = |id: TileId| layers.get(&(crate::soil_plus::DIR.to_string(), id));
        let region_tiles = |id: TileId| layers.get(&("regions".to_string(), id));
        let extremes_tiles = |id: TileId| ext.get(&id);
        let extremes_tail_tiles = |id: TileId| ext_tail.get(&id);
        let frost_calendar_tiles = |id: TileId| frost.get(&id);
        let monthly_tiles = |id: TileId| mon.get(&id);
        let monthly_precip_tiles = |id: TileId| mon_p.get(&id);
        let phenology_tiles = |id: TileId| pheno.get(&id);
        let water_balance_tiles = |id: TileId| wb.get(&id);
        let terrain_tiles = |id: TileId| tr.get(&id);
        let g = Getters {
            climate: &climate_tiles,
            soil: soil.then_some(&soil_tiles as LayerTiles<'_, '_>),
            soil_plus: soil_plus.then_some(&soil_plus_tiles as LayerTiles<'_, '_>),
            regions: regions.then_some(&region_tiles as LayerTiles<'_, '_>),
            extremes: extremes.then_some(&extremes_tiles as ExtremesTiles<'_, '_>),
            extremes_tail: extremes_tail
                .then_some(&extremes_tail_tiles as ExtremesTailTiles<'_, '_>),
            frost_calendar: frost_calendar
                .then_some(&frost_calendar_tiles as FrostCalendarTiles<'_, '_>),
            monthly: monthly.then_some(&monthly_tiles as MonthlyTiles<'_, '_>),
            monthly_precip: monthly_precip
                .then_some(&monthly_precip_tiles as MonthlyPrecipTiles<'_, '_>),
            phenology: phenology.then_some(&phenology_tiles as PhenologyTiles<'_, '_>),
            water_balance: water_balance
                .then_some(&water_balance_tiles as WaterBalanceTiles<'_, '_>),
            terrain: terrain.then_some(&terrain_tiles as TerrainTiles<'_, '_>),
        };
        crate::vectors::site(g, (lat, lon, radius), own_tile_only)
            .map_err(|e| PipelineError::Data(format!("{lat}, {lon}: {e}")))
    }

    /// Decode the extremes tiles the climate search can reach (the
    /// extremes are read at the climate lookup's cell).
    fn load_extremes(&mut self, lat: f64, lon: f64, radius: usize) -> Result<()> {
        let bad = |e: crate::lookup::LookupError| PipelineError::Data(format!("{lat}, {lon}: {e}"));
        let ids = search_tiles(lat, lon, radius).map_err(bad)?;
        if self.extremes_cache.len() + ids.len() > CACHE_TILES {
            self.extremes_cache.clear();
        }
        for t in ids {
            if self.extremes_cache.contains_key(&t) {
                continue;
            }
            let key = (crate::extremes::DIR.to_string(), t.name());
            let Some(rel) = self.layer_paths.get(&key) else {
                continue;
            };
            let path = self.dir.join(rel);
            let bytes = std::fs::read(&path).map_err(|e| PipelineError::io(&path, e))?;
            let tile = ExtremesTile::decode(&bytes)?;
            if tile.id() != t {
                return Err(PipelineError::Data(format!(
                    "{}: holds tile {}",
                    path.display(),
                    tile.id()
                )));
            }
            self.extremes_cache.insert(t, tile);
        }
        Ok(())
    }

    /// Decode the `extremes_tail` tiles the climate search can reach.
    fn load_extremes_tail(&mut self, lat: f64, lon: f64, radius: usize) -> Result<()> {
        let bad = |e: crate::lookup::LookupError| PipelineError::Data(format!("{lat}, {lon}: {e}"));
        let ids = search_tiles(lat, lon, radius).map_err(bad)?;
        if self.extremes_tail_cache.len() + ids.len() > CACHE_TILES {
            self.extremes_tail_cache.clear();
        }
        for t in ids {
            if self.extremes_tail_cache.contains_key(&t) {
                continue;
            }
            let key = (crate::extremes_tail::DIR.to_string(), t.name());
            let Some(rel) = self.layer_paths.get(&key) else {
                continue;
            };
            let path = self.dir.join(rel);
            let bytes = std::fs::read(&path).map_err(|e| PipelineError::io(&path, e))?;
            let tile = ExtremesTailTile::decode(&bytes)?;
            if tile.id() != t {
                return Err(PipelineError::Data(format!(
                    "{}: holds tile {}",
                    path.display(),
                    tile.id()
                )));
            }
            self.extremes_tail_cache.insert(t, tile);
        }
        Ok(())
    }

    /// Decode the `frost_calendar` tiles the climate search can reach.
    fn load_frost_calendar(&mut self, lat: f64, lon: f64, radius: usize) -> Result<()> {
        let bad = |e: crate::lookup::LookupError| PipelineError::Data(format!("{lat}, {lon}: {e}"));
        let ids = search_tiles(lat, lon, radius).map_err(bad)?;
        if self.frost_calendar_cache.len() + ids.len() > CACHE_TILES {
            self.frost_calendar_cache.clear();
        }
        for t in ids {
            if self.frost_calendar_cache.contains_key(&t) {
                continue;
            }
            let key = (crate::frost_calendar::DIR.to_string(), t.name());
            let Some(rel) = self.layer_paths.get(&key) else {
                continue;
            };
            let path = self.dir.join(rel);
            let bytes = std::fs::read(&path).map_err(|e| PipelineError::io(&path, e))?;
            let tile = FrostCalendarTile::decode(&bytes)?;
            if tile.id() != t {
                return Err(PipelineError::Data(format!(
                    "{}: holds tile {}",
                    path.display(),
                    tile.id()
                )));
            }
            self.frost_calendar_cache.insert(t, tile);
        }
        Ok(())
    }

    /// Decode the monthly tiles the climate search can reach.
    fn load_monthly(&mut self, lat: f64, lon: f64, radius: usize) -> Result<()> {
        let bad = |e: crate::lookup::LookupError| PipelineError::Data(format!("{lat}, {lon}: {e}"));
        let ids = search_tiles(lat, lon, radius).map_err(bad)?;
        if self.monthly_cache.len() + ids.len() > CACHE_TILES {
            self.monthly_cache.clear();
        }
        for t in ids {
            if self.monthly_cache.contains_key(&t) {
                continue;
            }
            let key = (crate::monthly::DIR.to_string(), t.name());
            let Some(rel) = self.layer_paths.get(&key) else {
                continue;
            };
            let path = self.dir.join(rel);
            let bytes = std::fs::read(&path).map_err(|e| PipelineError::io(&path, e))?;
            let tile = MonthlyTile::decode(&bytes)?;
            if tile.id() != t {
                return Err(PipelineError::Data(format!(
                    "{}: holds tile {}",
                    path.display(),
                    tile.id()
                )));
            }
            self.monthly_cache.insert(t, tile);
        }
        Ok(())
    }

    /// Decode the monthly_precip tiles a search of `radius` 0.25-degree cells
    /// can reach.
    fn load_monthly_precip(&mut self, lat: f64, lon: f64, radius: usize) -> Result<()> {
        let bad = |e: crate::lookup::LookupError| PipelineError::Data(format!("{lat}, {lon}: {e}"));
        let ids = crate::monthly_precip::search_tiles(lat, lon, radius).map_err(bad)?;
        if self.monthly_precip_cache.len() + ids.len() > CACHE_TILES {
            self.monthly_precip_cache.clear();
        }
        for t in ids {
            if self.monthly_precip_cache.contains_key(&t) {
                continue;
            }
            let key = (crate::monthly_precip::DIR.to_string(), t.name());
            let Some(rel) = self.layer_paths.get(&key) else {
                continue;
            };
            let path = self.dir.join(rel);
            let bytes = std::fs::read(&path).map_err(|e| PipelineError::io(&path, e))?;
            let tile = MonthlyPrecipTile::decode(&bytes)?;
            if tile.id() != t {
                return Err(PipelineError::Data(format!(
                    "{}: holds tile {}",
                    path.display(),
                    tile.id()
                )));
            }
            self.monthly_precip_cache.insert(t, tile);
        }
        Ok(())
    }

    /// Extremes at a climate cell (`cell_lat`, `cell_lon`), `Ok(None)` without
    /// the layer or when the cell has none.
    ///
    /// # Errors
    /// Invalid coordinate, unreadable tile.
    pub fn extremes_at_cell(
        &mut self,
        cell_lat: f64,
        cell_lon: f64,
    ) -> Result<Option<ExtremesLookup>> {
        if !self.has_layer(crate::extremes::DIR) {
            return Ok(None);
        }
        self.load_extremes(cell_lat, cell_lon, 0)?;
        let Some(id) = TileId::of(cell_lat, cell_lon) else {
            return Ok(None);
        };
        match self.extremes_cache.get(&id) {
            Some(tile) => crate::extremes::at_cell(tile, cell_lat, cell_lon)
                .map_err(|e| PipelineError::Data(format!("{cell_lat}, {cell_lon}: {e}"))),
            None => Ok(None),
        }
    }

    /// Monthly climatology at a climate cell (`cell_lat`, `cell_lon`),
    /// `Ok(None)` without the layer or when the cell has none.
    ///
    /// # Errors
    /// Invalid coordinate, unreadable tile.
    pub fn monthly_at_cell(
        &mut self,
        cell_lat: f64,
        cell_lon: f64,
    ) -> Result<Option<MonthlyLookup>> {
        if !self.has_layer(crate::monthly::DIR) {
            return Ok(None);
        }
        self.load_monthly(cell_lat, cell_lon, 0)?;
        let Some(id) = TileId::of(cell_lat, cell_lon) else {
            return Ok(None);
        };
        match self.monthly_cache.get(&id) {
            Some(tile) => crate::monthly::at_cell(tile, cell_lat, cell_lon)
                .map_err(|e| PipelineError::Data(format!("{cell_lat}, {cell_lon}: {e}"))),
            None => Ok(None),
        }
    }

    /// Monthly precipitation at a point (0.25-degree grid, searching up to
    /// `radius` cells across tiles).
    ///
    /// # Errors
    /// Invalid coordinate, unreadable tile.
    pub fn monthly_precip(
        &mut self,
        lat: f64,
        lon: f64,
        radius: usize,
    ) -> Result<Option<MonthlyPrecipLookup>> {
        if !self.has_layer(crate::monthly_precip::DIR) {
            return Ok(None);
        }
        self.load_monthly_precip(lat, lon, radius)?;
        let cache = &self.monthly_precip_cache;
        crate::monthly_precip::lookup_across(|t| cache.get(&t), lat, lon, radius)
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
        let count_for_layer = self.layer_cache.keys().filter(|(k, _)| k == id).count();
        if count_for_layer + ids.len() > CACHE_LAYER_TILES {
            // Evict only this layer's tiles so multi-layer lookups (`Grid::site`)
            // cannot evict tiles just loaded for another layer (`soil` vs `soil_plus`).
            self.layer_cache.retain(|(k, _), _| k != id);
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
        let climate = self.manifest.tiles.iter().map(|t| (t, Kind::Climate));
        let layer_tiles = self.manifest.layers.iter().flat_map(|l| {
            let kind = if l.id == crate::extremes::DIR {
                Kind::Extremes
            } else if l.id == crate::extremes_tail::DIR {
                Kind::ExtremesTail
            } else if l.id == crate::frost_calendar::DIR {
                Kind::FrostCalendar
            } else if l.id == crate::monthly::DIR {
                Kind::Monthly
            } else if l.id == crate::monthly_precip::DIR {
                Kind::MonthlyPrecip
            } else if l.id == crate::phenology::DIR {
                Kind::Phenology
            } else if l.id == crate::water_balance::DIR {
                Kind::WaterBalance
            } else if l.id == crate::terrain::DIR {
                Kind::Terrain
            } else if l.id == "soil" {
                Kind::Soil
            } else if l.id == crate::soil_plus::DIR {
                Kind::SoilPlus
            } else if l.id == "regions" {
                Kind::Regions
            } else {
                Kind::Layer(l.bands.len())
            };
            l.tiles.iter().map(move |t| (t, kind))
        });
        for (t, kind) in climate.chain(layer_tiles) {
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
                    if let Some(p) = decode_problem(&b, kind, &t.name, t.complete_cells) {
                        problems.push(format!("{}: {p}", t.path));
                    }
                }
            }
        }
        if let Some(expected) = &self.manifest.climate_tiles_sha256 {
            let actual = super::assemble::climate_tiles_sha256(&self.manifest.tiles);
            if actual != *expected {
                problems.push(format!(
                    "manifest.json: climate_tiles_sha256 mismatch (computed {actual}, manifest {expected})"
                ));
            }
        }
        if let Some(a) = &self.manifest.analogues {
            let p = self.dir.join(&a.path);
            match std::fs::read(&p) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    problems.push(format!("missing {}", a.path));
                }
                Err(e) => return Err(PipelineError::io(&p, e)),
                Ok(b) if b.len() as u64 != a.bytes => {
                    problems.push(format!(
                        "{}: {} bytes, expected {}",
                        a.path,
                        b.len(),
                        a.bytes
                    ));
                }
                Ok(b) if super::sha256_bytes(&b) != a.sha256 => {
                    problems.push(format!("{}: checksum mismatch", a.path));
                }
                Ok(b) => match AnalogueIndex::from_bytes(&b) {
                    Err(e) => problems.push(format!("{}: {e}", a.path)),
                    Ok(idx) if idx.records.len() != a.records as usize => {
                        problems.push(format!(
                            "{}: {} records, expected {}",
                            a.path,
                            idx.records.len(),
                            a.records
                        ));
                    }
                    Ok(_) => {}
                },
            }
        }
        self.verify_sha256sums(&mut problems)?;
        Ok(problems)
    }

    fn verify_sha256sums(&self, problems: &mut Vec<String>) -> Result<()> {
        let sums_path = self.dir.join("SHA256SUMS");
        let text = match std::fs::read_to_string(&sums_path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Real release grids include ATTRIBUTION.md / build-report.json alongside SHA256SUMS.
                if self.dir.join("ATTRIBUTION.md").is_file()
                    || self.dir.join("build-report.json").is_file()
                {
                    problems.push("missing SHA256SUMS".into());
                }
                return Ok(());
            }
            Err(e) => return Err(PipelineError::io(&sums_path, e)),
        };
        let mut sums: HashMap<&str, &str> = HashMap::new();
        for (idx, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            let Some((hash, path)) = line.split_once("  ") else {
                problems.push(format!("SHA256SUMS: line {} malformed", idx + 1));
                continue;
            };
            if sums.insert(path, hash).is_some() {
                problems.push(format!("SHA256SUMS: duplicate entry for {path}"));
            }
        }
        // Verify manifest.json and optional release metadata files match SHA256SUMS.
        for meta in ["manifest.json", "build-report.json", "ATTRIBUTION.md"] {
            let mpath = self.dir.join(meta);
            if meta != "manifest.json" && !mpath.is_file() {
                continue;
            }
            match sums.get(meta) {
                None => problems.push(format!("SHA256SUMS: missing {meta}")),
                Some(&expected) => {
                    let bytes = std::fs::read(&mpath).map_err(|e| PipelineError::io(&mpath, e))?;
                    if super::sha256_bytes(&bytes) != expected {
                        problems.push(format!("SHA256SUMS: {meta} checksum mismatch"));
                    }
                }
            }
        }
        // Verify every tile and analogues.bin listed in manifest appears in SHA256SUMS with matching hash.
        for t in self
            .manifest
            .tiles
            .iter()
            .chain(self.manifest.layers.iter().flat_map(|l| l.tiles.iter()))
        {
            match sums.get(t.path.as_str()) {
                None => problems.push(format!("SHA256SUMS: missing {}", t.path)),
                Some(&h) if h != t.sha256 => {
                    problems.push(format!("SHA256SUMS: checksum mismatch for {}", t.path));
                }
                Some(_) => {}
            }
        }
        if let Some(a) = &self.manifest.analogues {
            match sums.get(a.path.as_str()) {
                None => problems.push(format!("SHA256SUMS: missing {}", a.path)),
                Some(&h) if h != a.sha256 => {
                    problems.push(format!("SHA256SUMS: checksum mismatch for {}", a.path));
                }
                Some(_) => {}
            }
        }
        Ok(())
    }

    /// Tail extremes at a climate cell (`cell_lat`, `cell_lon`).
    ///
    /// # Errors
    /// Invalid coordinate, unreadable tile.
    pub fn extremes_tail_at_cell(
        &mut self,
        cell_lat: f64,
        cell_lon: f64,
    ) -> Result<Option<ExtremesTailLookup>> {
        if !self.has_layer(crate::extremes_tail::DIR) {
            return Ok(None);
        }
        self.load_extremes_tail(cell_lat, cell_lon, 0)?;
        let Some(id) = TileId::of(cell_lat, cell_lon) else {
            return Ok(None);
        };
        match self.extremes_tail_cache.get(&id) {
            Some(tile) => crate::extremes_tail::at_cell(tile, cell_lat, cell_lon)
                .map_err(|e| PipelineError::Data(format!("{cell_lat}, {cell_lon}: {e}"))),
            None => Ok(None),
        }
    }

    /// Frost calendar at a climate cell (`cell_lat`, `cell_lon`).
    ///
    /// # Errors
    /// Invalid coordinate, unreadable tile.
    pub fn frost_calendar_at_cell(
        &mut self,
        cell_lat: f64,
        cell_lon: f64,
    ) -> Result<Option<FrostCalendarLookup>> {
        if !self.has_layer(crate::frost_calendar::DIR) {
            return Ok(None);
        }
        self.load_frost_calendar(cell_lat, cell_lon, 0)?;
        let Some(id) = TileId::of(cell_lat, cell_lon) else {
            return Ok(None);
        };
        match self.frost_calendar_cache.get(&id) {
            Some(tile) => crate::frost_calendar::at_cell(tile, cell_lat, cell_lon)
                .map_err(|e| PipelineError::Data(format!("{cell_lat}, {cell_lon}: {e}"))),
            None => Ok(None),
        }
    }

    fn load_phenology(&mut self, lat: f64, lon: f64, radius: usize) -> Result<()> {
        let bad = |e: crate::lookup::LookupError| PipelineError::Data(format!("{lat}, {lon}: {e}"));
        let ids = search_tiles(lat, lon, radius).map_err(bad)?;
        if self.phenology_cache.len() + ids.len() > CACHE_TILES {
            self.phenology_cache.clear();
        }
        for t in ids {
            if self.phenology_cache.contains_key(&t) {
                continue;
            }
            let key = (crate::phenology::DIR.to_string(), t.name());
            let Some(rel) = self.layer_paths.get(&key) else {
                continue;
            };
            let path = self.dir.join(rel);
            let bytes = std::fs::read(&path).map_err(|e| PipelineError::io(&path, e))?;
            let tile = PhenologyTile::decode(&bytes)?;
            if tile.id() != t {
                return Err(PipelineError::Data(format!(
                    "{}: holds tile {}",
                    path.display(),
                    tile.id()
                )));
            }
            self.phenology_cache.insert(t, tile);
        }
        Ok(())
    }

    fn load_water_balance(&mut self, lat: f64, lon: f64, radius: usize) -> Result<()> {
        let bad = |e: crate::lookup::LookupError| PipelineError::Data(format!("{lat}, {lon}: {e}"));
        let ids = search_tiles(lat, lon, radius).map_err(bad)?;
        if self.water_balance_cache.len() + ids.len() > CACHE_TILES {
            self.water_balance_cache.clear();
        }
        for t in ids {
            if self.water_balance_cache.contains_key(&t) {
                continue;
            }
            let key = (crate::water_balance::DIR.to_string(), t.name());
            let Some(rel) = self.layer_paths.get(&key) else {
                continue;
            };
            let path = self.dir.join(rel);
            let bytes = std::fs::read(&path).map_err(|e| PipelineError::io(&path, e))?;
            let tile = WaterBalanceTile::decode(&bytes)?;
            if tile.id() != t {
                return Err(PipelineError::Data(format!(
                    "{}: holds tile {}",
                    path.display(),
                    tile.id()
                )));
            }
            self.water_balance_cache.insert(t, tile);
        }
        Ok(())
    }

    /// Phenology at a climate cell (`cell_lat`, `cell_lon`).
    ///
    /// # Errors
    /// Invalid coordinate, unreadable tile.
    pub fn phenology_at_cell(
        &mut self,
        cell_lat: f64,
        cell_lon: f64,
    ) -> Result<Option<PhenologyLookup>> {
        if !self.has_layer(crate::phenology::DIR) {
            return Ok(None);
        }
        self.load_phenology(cell_lat, cell_lon, 0)?;
        let Some(id) = TileId::of(cell_lat, cell_lon) else {
            return Ok(None);
        };
        match self.phenology_cache.get(&id) {
            Some(tile) => crate::phenology::at_cell(tile, cell_lat, cell_lon)
                .map_err(|e| PipelineError::Data(format!("{cell_lat}, {cell_lon}: {e}"))),
            None => Ok(None),
        }
    }

    /// Water balance at a climate cell (`cell_lat`, `cell_lon`).
    ///
    /// # Errors
    /// Invalid coordinate, unreadable tile.
    pub fn water_balance_at_cell(
        &mut self,
        cell_lat: f64,
        cell_lon: f64,
    ) -> Result<Option<WaterBalanceLookup>> {
        if !self.has_layer(crate::water_balance::DIR) {
            return Ok(None);
        }
        self.load_water_balance(cell_lat, cell_lon, 0)?;
        let Some(id) = TileId::of(cell_lat, cell_lon) else {
            return Ok(None);
        };
        match self.water_balance_cache.get(&id) {
            Some(tile) => crate::water_balance::at_cell(tile, cell_lat, cell_lon)
                .map_err(|e| PipelineError::Data(format!("{cell_lat}, {cell_lon}: {e}"))),
            None => Ok(None),
        }
    }

    fn load_terrain(&mut self, lat: f64, lon: f64, radius: usize) -> Result<()> {
        let bad = |e: crate::lookup::LookupError| PipelineError::Data(format!("{lat}, {lon}: {e}"));
        let ids = search_tiles(lat, lon, radius).map_err(bad)?;
        if self.terrain_cache.len() + ids.len() > CACHE_TILES {
            self.terrain_cache.clear();
        }
        for t in ids {
            if self.terrain_cache.contains_key(&t) {
                continue;
            }
            let key = (crate::terrain::DIR.to_string(), t.name());
            let Some(rel) = self.layer_paths.get(&key) else {
                continue;
            };
            let path = self.dir.join(rel);
            let bytes = std::fs::read(&path).map_err(|e| PipelineError::io(&path, e))?;
            let tile = TerrainTile::decode(&bytes)?;
            if tile.id() != t {
                return Err(PipelineError::Data(format!(
                    "{}: holds tile {}",
                    path.display(),
                    tile.id()
                )));
            }
            self.terrain_cache.insert(t, tile);
        }
        Ok(())
    }

    /// Deep soil (`soil_plus`) at a point (nearest cell with data within 2 cells).
    ///
    /// # Errors
    /// Invalid coordinate, unreadable tile.
    pub fn soil_plus(&mut self, lat: f64, lon: f64) -> Result<Option<SoilPlusLookup>> {
        if !self.has_layer(crate::soil_plus::DIR) {
            return Ok(None);
        }
        self.load_layer(
            crate::soil_plus::DIR,
            SOIL_PLUS_BANDS,
            lat,
            lon,
            LAYER_LOOKUP_RADIUS,
        )?;
        let layers = &self.layer_cache;
        crate::soil_plus::lookup_across(
            |id| layers.get(&(crate::soil_plus::DIR.to_string(), id)),
            lat,
            lon,
            LAYER_LOOKUP_RADIUS,
        )
        .map_err(|e| PipelineError::Data(format!("{lat}, {lon}: {e}")))
    }

    /// Terrain at a climate cell (`cell_lat`, `cell_lon`).
    ///
    /// # Errors
    /// Invalid coordinate, unreadable tile.
    pub fn terrain_at_cell(
        &mut self,
        cell_lat: f64,
        cell_lon: f64,
    ) -> Result<Option<TerrainLookup>> {
        if !self.has_layer(crate::terrain::DIR) {
            return Ok(None);
        }
        self.load_terrain(cell_lat, cell_lon, 0)?;
        let Some(id) = TileId::of(cell_lat, cell_lon) else {
            return Ok(None);
        };
        match self.terrain_cache.get(&id) {
            Some(tile) => crate::terrain::at_cell(tile, cell_lat, cell_lon)
                .map_err(|e| PipelineError::Data(format!("{cell_lat}, {cell_lon}: {e}"))),
            None => Ok(None),
        }
    }
}

/// What a tile file holds.
#[derive(Debug, Clone, Copy)]
enum Kind {
    Climate,
    Soil,
    SoilPlus,
    Regions,
    /// A generic 0.05-degree site layer with this many bands.
    Layer(usize),
    Extremes,
    ExtremesTail,
    FrostCalendar,
    Monthly,
    MonthlyPrecip,
    Phenology,
    WaterBalance,
    Terrain,
}

fn count_all_present(r: &crate::raster::Raster) -> usize {
    (0..r.grid().tile_len())
        .filter(|&i| {
            let p = r.pixel(i);
            !p.is_empty() && p.iter().all(|&v| v != crate::model::NODATA)
        })
        .count()
}

/// Why a tile file does not decode as the tile `name` or match `expected_complete`
/// (`None`: it does).
fn decode_problem(bytes: &[u8], kind: Kind, name: &str, expected_complete: u32) -> Option<String> {
    let decoded: std::result::Result<(TileId, Option<usize>), crate::tile::TileError> =
        match kind {
            Kind::Climate => Tile::decode(bytes).map(|t| (t.id(), Some(t.complete_cells()))),
            Kind::Soil => LayerTile::decode(bytes, SOIL_BANDS).map(|t| {
                let n = (0..crate::layer::LAYER_TILE_LEN)
                    .filter(|&i| crate::soil::has_soil(&t, i))
                    .count();
                (t.id(), Some(n))
            }),
            Kind::SoilPlus => LayerTile::decode(bytes, SOIL_PLUS_BANDS).map(|t| {
                let n = (0..crate::layer::LAYER_TILE_LEN)
                    .filter(|&i| crate::soil_plus::has_soil_plus(&t, i))
                    .count();
                (t.id(), Some(n))
            }),
            Kind::Regions => LayerTile::decode(bytes, REGION_BANDS).map(|t| {
                let r = t.as_raster();
                let n = (0..crate::layer::LAYER_TILE_LEN)
                    .filter(|&i| r.pixel(i).iter().any(|&v| v != crate::model::NODATA))
                    .count();
                (t.id(), Some(n))
            }),
            Kind::Layer(bands) => LayerTile::decode(bytes, bands).map(|t| (t.id(), None)),
            Kind::Extremes => ExtremesTile::decode(bytes)
                .map(|t| (t.id(), Some(count_all_present(t.as_raster())))),
            Kind::ExtremesTail => ExtremesTailTile::decode(bytes)
                .map(|t| (t.id(), Some(count_all_present(t.as_raster())))),
            Kind::FrostCalendar => FrostCalendarTile::decode(bytes)
                .map(|t| (t.id(), Some(count_all_present(t.as_raster())))),
            Kind::Monthly => {
                MonthlyTile::decode(bytes).map(|t| (t.id(), Some(count_all_present(t.as_raster()))))
            }
            Kind::MonthlyPrecip => MonthlyPrecipTile::decode(bytes)
                .map(|t| (t.id(), Some(count_all_present(t.as_raster())))),
            Kind::Phenology => PhenologyTile::decode(bytes)
                .map(|t| (t.id(), Some(count_all_present(t.as_raster())))),
            Kind::WaterBalance => WaterBalanceTile::decode(bytes)
                .map(|t| (t.id(), Some(count_all_present(t.as_raster())))),
            Kind::Terrain => TerrainTile::decode(bytes).map(|t| {
                let r = t.as_raster();
                let n = (0..r.grid().tile_len())
                    .filter(|&i| {
                        let p = r.pixel(i);
                        p.len() >= 3
                            && p[0] != crate::model::NODATA
                            && p[1] != crate::model::NODATA
                            && p[2] != crate::model::NODATA
                    })
                    .count();
                (t.id(), Some(n))
            }),
        };
    match decoded {
        Err(e) => Some(e.to_string()),
        Ok((id, _)) if id.name() != name => Some(format!("holds tile {id}")),
        Ok((_, Some(actual)))
            if (matches!(kind, Kind::Climate) || expected_complete > 0)
                && actual != expected_complete as usize =>
        {
            Some(format!(
                "{actual} complete cells, manifest says {expected_complete}"
            ))
        }
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

/// Columns `--extremes` appends to every row.
pub const EXTREMES_COLUMNS: [&str; 3] = ["tmin_extreme_c", "tmax_extreme_c", "usda_zone"];

/// Columns `--monthly` appends to every row (populated for the 3 monthly
/// slots; empty for other slots and no-data/invalid rows).
pub const MONTHLY_COLUMNS: [&str; 39] = [
    "tmean_01_c",
    "tmean_02_c",
    "tmean_03_c",
    "tmean_04_c",
    "tmean_05_c",
    "tmean_06_c",
    "tmean_07_c",
    "tmean_08_c",
    "tmean_09_c",
    "tmean_10_c",
    "tmean_11_c",
    "tmean_12_c",
    "tmin_01_c",
    "tmin_02_c",
    "tmin_03_c",
    "tmin_04_c",
    "tmin_05_c",
    "tmin_06_c",
    "tmin_07_c",
    "tmin_08_c",
    "tmin_09_c",
    "tmin_10_c",
    "tmin_11_c",
    "tmin_12_c",
    "dry_mask",
    "gdd_base_10",
    "chill_months",
    "precip_01_mm",
    "precip_02_mm",
    "precip_03_mm",
    "precip_04_mm",
    "precip_05_mm",
    "precip_06_mm",
    "precip_07_mm",
    "precip_08_mm",
    "precip_09_mm",
    "precip_10_mm",
    "precip_11_mm",
    "precip_12_mm",
];

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
    /// Append [`EXTREMES_COLUMNS`]: `tmin_extreme_c`, `tmax_extreme_c`,
    /// `usda_zone` at the climate lookup's cell (needs the `extremes` layer).
    pub extremes: bool,
    /// Append [`MONTHLY_COLUMNS`]: 12-month `tmean`, `tmin`, `dry_mask`,
    /// `gdd_base_10`, `chill_months`, and 12-month `precip_mm` for the 3
    /// monthly slots (needs the `monthly` layer; `monthly_precip` used when
    /// present).
    pub monthly: bool,
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
        extremes: false,
        monthly: false,
    };
    sample_csv_with(grid, input, output, &opts)
}

/// [`sample_csv`] with options. Without any optional flag (`regions`,
/// `extremes`, `monthly`) the output is byte-identical to `sample_csv`'s.
///
/// # Errors
/// As [`sample_csv`]; or a missing layer required by an enabled flag.
pub fn sample_csv_with(
    grid: &mut Grid,
    input: impl Read,
    output: impl Write,
    opts: &SampleOptions,
) -> Result<SampleSummary> {
    check_sample_layers(grid, opts)?;
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
    if opts.extremes {
        header.extend(EXTREMES_COLUMNS);
    }
    if opts.monthly {
        header.extend(MONTHLY_COLUMNS);
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
            write_status(&mut w, &point, "invalid", opts, None)?;
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
                write_status(&mut w, &point, "no_data", opts, codes.as_ref())?;
            }
            Some(f) => {
                summary.ok += 1;
                let extras = load_row_extras(grid, &f, lat, lon, opts, codes)?;
                write_rows(&mut w, &point, &f, opts, &extras)?;
            }
        }
    }
    w.flush().map_err(|e| PipelineError::Data(e.to_string()))?;
    Ok(summary)
}

fn check_sample_layers(grid: &Grid, opts: &SampleOptions) -> Result<()> {
    if opts.regions && !grid.has_layer("regions") {
        return Err(PipelineError::Data(
            "--regions: the grid has no regions layer (canopi-climate 0.2 or later)".into(),
        ));
    }
    if opts.extremes && !grid.has_layer(crate::extremes::DIR) {
        return Err(PipelineError::Data(
            "--extremes: the grid has no extremes layer (canopi-climate 0.3 or later)".into(),
        ));
    }
    if opts.monthly && !grid.has_layer(crate::monthly::DIR) {
        return Err(PipelineError::Data(
            "--monthly: the grid has no monthly layer (canopi-climate 0.4 or later)".into(),
        ));
    }
    Ok(())
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

/// Optional per-point layer lookups loaded when flags are set.
struct RowExtras {
    codes: Option<Codes>,
    extremes: Option<ExtremesLookup>,
    monthly: Option<MonthlyLookup>,
    monthly_precip: Option<MonthlyPrecipLookup>,
}

fn load_row_extras(
    grid: &mut Grid,
    f: &Lookup,
    lat: f64,
    lon: f64,
    opts: &SampleOptions,
    codes: Option<Codes>,
) -> Result<RowExtras> {
    let extremes = if opts.extremes {
        grid.extremes_at_cell(f.cell_lat, f.cell_lon)?
    } else {
        None
    };
    let (monthly, monthly_precip) = if opts.monthly {
        (
            grid.monthly_at_cell(f.cell_lat, f.cell_lon)?,
            grid.monthly_precip(lat, lon, PRECIP_LOOKUP_RADIUS)?,
        )
    } else {
        (None, None)
    };
    Ok(RowExtras {
        codes,
        extremes,
        monthly,
        monthly_precip,
    })
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
    opts: &SampleOptions,
    codes: Option<&Codes>,
) -> Result<()> {
    let mut row = vec![p.id, p.lat, p.lon, status];
    row.resize(OUT_HEADER.len(), "");
    if opts.regions {
        match codes {
            Some(c) => row.extend([c.wgsrpd_l3.as_str(), c.iso_a2.as_str()]),
            None => row.extend(["", ""]),
        }
    }
    if opts.extremes {
        row.extend(std::iter::repeat_n("", EXTREMES_COLUMNS.len()));
    }
    if opts.monthly {
        row.extend(std::iter::repeat_n("", MONTHLY_COLUMNS.len()));
    }
    w.write_record(&row)
        .map_err(|e| PipelineError::Data(e.to_string()))
}

fn append_monthly_cols(
    row: &mut Vec<String>,
    slot_idx: usize,
    monthly: Option<&MonthlyLookup>,
    monthly_precip: Option<&MonthlyPrecipLookup>,
) {
    let Some(ms) = MONTHLY_SLOT_INDICES.iter().position(|&s| s == slot_idx) else {
        row.extend(std::iter::repeat_n(String::new(), MONTHLY_COLUMNS.len()));
        return;
    };
    if let Some(p) = monthly.and_then(|m| m.periods.get(ms)) {
        for v in p.tmean_c {
            row.push(format!("{v:.1}"));
        }
        for v in p.tmin_c {
            row.push(format!("{v:.1}"));
        }
        row.push(p.dry_mask.to_string());
        row.push(format!("{:.1}", p.gdd_base_10));
        row.push(p.chill_months.to_string());
    } else {
        row.extend(std::iter::repeat_n(String::new(), 27));
    }
    if let Some(p) = monthly_precip.and_then(|mp| mp.periods.get(ms)) {
        for v in p.precip_mm {
            row.push(format!("{v:.0}"));
        }
    } else {
        row.extend(std::iter::repeat_n(String::new(), 12));
    }
}

fn write_rows<W: Write>(
    w: &mut csv::Writer<W>,
    pt: &Point<'_>,
    f: &Lookup,
    opts: &SampleOptions,
    extras: &RowExtras,
) -> Result<()> {
    for (slot_idx, p) in f.periods.iter().enumerate() {
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
        if let Some(c) = &extras.codes {
            row.extend([c.wgsrpd_l3.clone(), c.iso_a2.clone()]);
        }
        if opts.extremes {
            if let Some(ep) = extras
                .extremes
                .as_ref()
                .and_then(|e| e.periods.get(slot_idx))
            {
                row.extend([
                    format!("{:.1}", ep.tmin_extreme_c),
                    format!("{:.1}", ep.tmax_extreme_c),
                    ep.usda_zone.clone(),
                ]);
            } else {
                row.extend(std::iter::repeat_n(String::new(), EXTREMES_COLUMNS.len()));
            }
        }
        if opts.monthly {
            append_monthly_cols(
                &mut row,
                slot_idx,
                extras.monthly.as_ref(),
                extras.monthly_precip.as_ref(),
            );
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
            extremes: false,
            monthly: false,
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
        m.climate_tiles_sha256 = Some(super::super::assemble::climate_tiles_sha256(&m.tiles));
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

    fn grid_dir_with_all_layers(name: &str) -> PathBuf {
        let dir = grid_dir_with_regions(name);
        let path = dir.join("manifest.json");
        let mut m: Manifest =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        std::fs::create_dir_all(dir.join("extremes")).unwrap();
        let et = crate::extremes::synthetic::extremes();
        let rel = format!("extremes/{}", et.id().file_name());
        let bytes = cog::write_extremes_tile_checked(&et, &dir.join(&rel)).unwrap();
        let espec = crate::manifest::TileSpec {
            name: et.id().name(),
            path: rel,
            bytes: bytes.len() as u64,
            sha256: crate::pipeline::sha256_bytes(&bytes),
            complete_cells: et.cells() as u32,
        };
        m.layers
            .push(crate::pipeline::extremes::layer_spec(vec![espec]));

        std::fs::create_dir_all(dir.join("monthly")).unwrap();
        let mt = crate::monthly::synthetic::monthly();
        let rel = format!("monthly/{}", mt.id().file_name());
        let bytes = cog::write_monthly_tile_checked(&mt, &dir.join(&rel)).unwrap();
        let mspec = crate::manifest::TileSpec {
            name: mt.id().name(),
            path: rel,
            bytes: bytes.len() as u64,
            sha256: crate::pipeline::sha256_bytes(&bytes),
            complete_cells: mt.cells() as u32,
        };
        m.layers
            .push(crate::pipeline::monthly::layer_spec(vec![mspec]));

        std::fs::create_dir_all(dir.join("monthly_precip")).unwrap();
        let mpt = crate::monthly_precip::synthetic::monthly_precip();
        let rel = format!("monthly_precip/{}", mpt.id().file_name());
        let bytes = cog::write_monthly_precip_tile_checked(&mpt, &dir.join(&rel)).unwrap();
        let mpspec = crate::manifest::TileSpec {
            name: mpt.id().name(),
            path: rel,
            bytes: bytes.len() as u64,
            sha256: crate::pipeline::sha256_bytes(&bytes),
            complete_cells: mpt.cells() as u32,
        };
        m.layers
            .push(crate::pipeline::monthly_precip::layer_spec(vec![mpspec]));

        std::fs::write(&path, serde_json::to_string(&m).unwrap()).unwrap();
        dir
    }

    #[test]
    fn csv_sampling_with_extremes_and_monthly_flags() {
        let dir = grid_dir_with_all_layers("all-layers");
        let mut g = Grid::open(&dir).unwrap();
        assert!(g.verify().unwrap().is_empty());

        // Default output (all flags false) stays byte-identical to v0.2.0/v0.3.0 golden.
        let golden = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample-v0.2.0.csv"),
        )
        .unwrap();
        let mut default_out = Vec::new();
        sample_csv(&mut g, SAMPLE_POINTS.as_bytes(), &mut default_out, 2).unwrap();
        assert_eq!(String::from_utf8(default_out).unwrap(), golden);

        // With --extremes and --monthly enabled:
        let opts = SampleOptions {
            radius: 2,
            regions: false,
            extremes: true,
            monthly: true,
        };
        let mut out = Vec::new();
        let input = "id,lat,lon\ninland,45.95,5.05\nsea,49.5,9.5\n";
        let s = sample_csv_with(&mut g, input.as_bytes(), &mut out, &opts).unwrap();
        assert_eq!((s.points, s.ok, s.no_data, s.invalid), (2, 1, 1, 0));
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let expected_cols = OUT_HEADER.len() + EXTREMES_COLUMNS.len() + MONTHLY_COLUMNS.len();
        assert_eq!(lines[0].split(',').count(), expected_cols);
        // Slot 0 (1991-2020 historical) has both extremes and monthly populated.
        let slot0: Vec<&str> = lines[1].split(',').collect();
        assert_eq!(slot0.len(), expected_cols);
        assert!(!slot0[20].is_empty(), "tmin_extreme_c populated");
        assert!(!slot0[22].is_empty(), "usda_zone populated");
        assert!(!slot0[23].is_empty(), "tmean_01_c populated");
        assert!(
            !slot0[expected_cols - 1].is_empty(),
            "precip_12_mm populated"
        );
        // Slot 1 (2041-2070 ssp126) has extremes populated but monthly empty.
        let slot1: Vec<&str> = lines[2].split(',').collect();
        assert!(!slot1[20].is_empty(), "extremes populated on ssp126");
        assert!(slot1[23].is_empty(), "monthly empty on ssp126");
        // Slot 2 (2041-2070 ssp245) has monthly populated.
        let slot2: Vec<&str> = lines[3].split(',').collect();
        assert!(!slot2[23].is_empty(), "monthly populated on ssp245");
        // Sea row has the right column count with empty extras.
        let sea: Vec<&str> = lines[10].split(',').collect();
        assert_eq!(sea.len(), expected_cols);
        assert_eq!(sea[3], "no_data");

        // Missing layers error cleanly.
        let mut plain = Grid::open(&grid_dir("plain-flags")).unwrap();
        assert!(
            sample_csv_with(
                &mut plain,
                input.as_bytes(),
                Vec::new(),
                &SampleOptions {
                    radius: 2,
                    regions: false,
                    extremes: true,
                    monthly: false,
                }
            )
            .is_err()
        );
        assert!(
            sample_csv_with(
                &mut plain,
                input.as_bytes(),
                Vec::new(),
                &SampleOptions {
                    radius: 2,
                    regions: false,
                    extremes: false,
                    monthly: true,
                }
            )
            .is_err()
        );
    }

    #[test]
    fn verify_catches_climate_tiles_sha256_sha256sums_and_complete_cells() {
        // BUG-10: verify() catches tampered climate_tiles_sha256, complete_cells, and SHA256SUMS.
        let dir = grid_dir("verify-hardened");
        let mpath = dir.join("manifest.json");
        let mut m: Manifest =
            serde_json::from_str(&std::fs::read_to_string(&mpath).unwrap()).unwrap();
        m.climate_tiles_sha256 = Some("0".repeat(64));
        m.tiles[0].complete_cells += 1;
        let mbytes = serde_json::to_vec(&m).unwrap();
        std::fs::write(&mpath, &mbytes).unwrap();
        std::fs::write(
            dir.join("SHA256SUMS"),
            format!("{}  manifest.json\n", "f".repeat(64)),
        )
        .unwrap();

        let problems = Grid::open(&dir).unwrap().verify().unwrap();
        assert!(
            problems.iter().any(|p| p.contains("climate_tiles_sha256")),
            "expected climate_tiles_sha256 mismatch, got {problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("complete cells")),
            "expected complete_cells mismatch, got {problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("SHA256SUMS")),
            "expected SHA256SUMS problem, got {problems:?}"
        );
    }

    #[test]
    fn site_layer_cache_does_not_evict_other_layers() {
        // Pre-fill layer_cache for "regions" up to CACHE_LAYER_TILES so loading "regions"
        // inside Grid::site triggers eviction; "soil" tiles loaded just before must survive.
        let dir = grid_dir_with_regions("cache-isolation");
        let path = dir.join("manifest.json");
        let mut m: Manifest =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        std::fs::create_dir_all(dir.join("soil")).unwrap();
        let st = crate::layer::synthetic::soil();
        let rel = format!("soil/{}", st.id().file_name());
        let bytes = cog::write_layer_tile_checked(&st, &dir.join(&rel)).unwrap();
        let sspec = crate::manifest::TileSpec {
            name: st.id().name(),
            path: rel,
            bytes: bytes.len() as u64,
            sha256: crate::pipeline::sha256_bytes(&bytes),
            complete_cells: 0,
        };
        m.layers
            .push(crate::pipeline::soil::layer_spec(vec![sspec]));
        std::fs::write(&path, serde_json::to_string(&m).unwrap()).unwrap();

        let mut g = Grid::open(&dir).unwrap();
        let dummy_reg = crate::layer::synthetic::regions();
        for (idx, tid) in TileId::all().take(CACHE_LAYER_TILES).enumerate() {
            if tid != dummy_reg.id() {
                let _ = idx;
                g.layer_cache
                    .insert(("regions".to_string(), tid), dummy_reg.clone());
            }
        }
        let site = g.site(45.25, 3.25, 2, false).unwrap();
        assert!(
            site.soil.is_some(),
            "soil tile must not be evicted when regions layer overflows its cache"
        );
        assert!(site.regions.is_some());
    }
}
