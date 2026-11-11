//! Site layers: soil ([`crate::soil`]) and regions ([`crate::regions`]) on
//! a 0.05-degree grid, in the climate grid's 10-degree tiles.
//!
//! - A layer cell is 0.05 degrees (about 5.5 km at the equator); a tile
//!   holds 200 x 200 of them and has the same name and extent as the
//!   climate tile (`n50e000` covers 40-50 N, 0-10 E). Cell edges follow
//!   [`crate::grid`]: west and north edges inclusive, latitude -90 in the
//!   last row, longitude 180 wraps to -180.
//! - A layer tile is a Cloud Optimized GeoTIFF like the climate tiles
//!   (EPSG:4326, `int16`, pixel-interleaved, Deflate, nodata -32768), in its
//!   own directory (`soil/`, `regions/`), with its own manifest entry
//!   ([`crate::manifest::LayerSpec`]). Tiles where the layer has no data are
//!   absent.

use crate::grid::{TILES_X, TILES_Y, TileId, grid_index};
use crate::lookup::LookupError;
use crate::model::NODATA;
use crate::tile::TileError;

/// Layer cell size in degrees.
pub const LAYER_CELL_DEG: f64 = 0.05;
/// Layer cells per tile side.
pub const LAYER_TILE_CELLS: usize = 200;
/// Layer cells per tile.
pub const LAYER_TILE_LEN: usize = LAYER_TILE_CELLS * LAYER_TILE_CELLS;
/// Global layer grid width in cells.
pub const LAYER_GRID_W: usize = TILES_X * LAYER_TILE_CELLS;
/// Global layer grid height in cells.
pub const LAYER_GRID_H: usize = TILES_Y * LAYER_TILE_CELLS;

/// Default search radius (cells) of a layer lookup whose own cell is empty
/// (a coastal point whose cell centre is at sea).
pub const LAYER_LOOKUP_RADIUS: usize = 2;

/// A cell of the global layer grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LayerCell {
    /// Column, 0 at 180 W.
    pub col: usize,
    /// Row, 0 at 90 N.
    pub row: usize,
}

impl LayerCell {
    /// The cell holding a point (see [`crate::grid::Cell::of`]).
    #[must_use]
    pub fn of(lat: f64, lon: f64) -> Option<Self> {
        if !lat.is_finite() || !lon.is_finite() || !(-90.0..=90.0).contains(&lat) {
            return None;
        }
        let lon = (lon + 180.0).rem_euclid(360.0);
        Some(Self {
            col: grid_index(lon / LAYER_CELL_DEG, LAYER_GRID_W),
            row: grid_index((90.0 - lat) / LAYER_CELL_DEG, LAYER_GRID_H),
        })
    }

    /// Latitude of the cell centre.
    #[must_use]
    pub fn center_lat(self) -> f64 {
        90.0 - (self.row as f64 + 0.5) * LAYER_CELL_DEG
    }

    /// Longitude of the cell centre.
    #[must_use]
    pub fn center_lon(self) -> f64 {
        -180.0 + (self.col as f64 + 0.5) * LAYER_CELL_DEG
    }

    /// The tile holding this cell.
    #[must_use]
    pub fn tile(self) -> TileId {
        TileId {
            col: self.col / LAYER_TILE_CELLS,
            row: self.row / LAYER_TILE_CELLS,
        }
    }

    /// Row-major index of the cell inside its tile.
    #[must_use]
    pub fn index_in_tile(self) -> usize {
        (self.row % LAYER_TILE_CELLS) * LAYER_TILE_CELLS + self.col % LAYER_TILE_CELLS
    }

    /// Row-major index of the cell in the global layer grid.
    #[must_use]
    pub fn global_index(self) -> usize {
        self.row * LAYER_GRID_W + self.col
    }

    /// The cell of a global row-major index.
    #[must_use]
    pub fn from_global(index: usize) -> Self {
        Self {
            col: index % LAYER_GRID_W,
            row: index / LAYER_GRID_W,
        }
    }

    /// The global cell of a row-major index inside a tile.
    #[must_use]
    pub fn in_tile(tile: TileId, index: usize) -> Self {
        Self {
            col: tile.col * LAYER_TILE_CELLS + index % LAYER_TILE_CELLS,
            row: tile.row * LAYER_TILE_CELLS + index / LAYER_TILE_CELLS,
        }
    }
}

/// A decoded layer tile: [`LAYER_TILE_LEN`] cells x `bands` `i16` values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerTile {
    id: TileId,
    bands: usize,
    /// Pixel-interleaved: `data[cell * bands + band]`.
    data: Vec<i16>,
}

impl LayerTile {
    /// A tile from pixel-interleaved values.
    ///
    /// # Errors
    /// [`TileError::Layout`] if the length is not `LAYER_TILE_LEN * bands`.
    pub fn new(id: TileId, bands: usize, data: Vec<i16>) -> Result<Self, TileError> {
        if bands == 0 || data.len() != LAYER_TILE_LEN * bands {
            return Err(TileError::Layout(format!(
                "{} values for {bands} bands",
                data.len()
            )));
        }
        Ok(Self { id, bands, data })
    }

    /// Decode a layer tile file holding `bands` bands.
    ///
    /// # Errors
    /// [`TileError`] when the bytes are not such a tile.
    pub fn decode(bytes: &[u8], bands: usize) -> Result<Self, TileError> {
        let (id, data) =
            crate::tile::decode_raster(bytes, LAYER_TILE_CELLS, LAYER_CELL_DEG, bands)?;
        Self::new(id, bands, data)
    }

    /// The tile's position.
    #[must_use]
    pub fn id(&self) -> TileId {
        self.id
    }

    /// Bands per cell.
    #[must_use]
    pub fn bands(&self) -> usize {
        self.bands
    }

    /// One value; [`NODATA`] outside the tile.
    #[must_use]
    pub fn get(&self, index: usize, band: usize) -> i16 {
        if band >= self.bands {
            return NODATA;
        }
        self.data
            .get(index * self.bands + band)
            .copied()
            .unwrap_or(NODATA)
    }

    /// The pixel-interleaved values.
    #[must_use]
    pub fn data(&self) -> &[i16] {
        &self.data
    }

    /// Cells where `has_data` holds.
    #[must_use]
    pub fn count(&self, has_data: impl Fn(&Self, usize) -> bool) -> usize {
        (0..LAYER_TILE_LEN).filter(|&i| has_data(self, i)).count()
    }
}

/// Cells within `radius` of `cell` with their squared distance (cells),
/// north to south then west to east. Longitude wraps; latitude stops at
/// the poles.
fn window(cell: LayerCell, radius: usize) -> impl Iterator<Item = (LayerCell, u32)> {
    let r = i64::try_from(radius.min(LAYER_TILE_CELLS)).unwrap_or(0);
    let (row0, col0) = (cell.row as i64, cell.col as i64);
    (-r..=r).flat_map(move |dy| {
        (-r..=r).filter_map(move |dx| {
            let d2 = dx * dx + dy * dy;
            let row = row0 + dy;
            if d2 > r * r || row < 0 || row >= LAYER_GRID_H as i64 {
                return None;
            }
            let col = (col0 + dx).rem_euclid(LAYER_GRID_W as i64);
            Some((
                LayerCell {
                    col: usize::try_from(col).ok()?,
                    row: usize::try_from(row).ok()?,
                },
                u32::try_from(d2).ok()?,
            ))
        })
    })
}

/// The cell a layer lookup resolved to.
#[derive(Debug, Clone, Copy)]
pub struct Found<'a> {
    /// The cell used.
    pub cell: LayerCell,
    /// Its tile.
    pub tile: &'a LayerTile,
    /// Distance from the point's cell, in cells (0 for the point's cell).
    pub distance_cells: f64,
}

/// Tiles that a search of `radius` cells around a point can reach.
///
/// # Errors
/// [`LookupError::InvalidCoordinate`].
pub fn search_tiles(lat: f64, lon: f64, radius: usize) -> Result<Vec<TileId>, LookupError> {
    let cell = LayerCell::of(lat, lon).ok_or(LookupError::InvalidCoordinate)?;
    let mut ids: Vec<TileId> = window(cell, radius).map(|(c, _)| c.tile()).collect();
    ids.sort_by_key(|t| (t.row, t.col));
    ids.dedup();
    Ok(ids)
}

/// The point's cell when `has_data` holds there, else the nearest such
/// cell within `radius` cells (ties: northern, then western). `tiles`
/// gives a decoded tile or `None` (no file: the layer has no data there).
///
/// # Errors
/// [`LookupError::InvalidCoordinate`].
pub fn nearest<'a>(
    tiles: impl Fn(TileId) -> Option<&'a LayerTile>,
    lat: f64,
    lon: f64,
    radius: usize,
    has_data: impl Fn(&LayerTile, usize) -> bool,
) -> Result<Option<Found<'a>>, LookupError> {
    let cell = LayerCell::of(lat, lon).ok_or(LookupError::InvalidCoordinate)?;
    let mut best: Option<(u32, LayerCell, &LayerTile)> = None;
    for (c, d2) in window(cell, radius) {
        if best.is_some_and(|(b, _, _)| b <= d2) {
            continue;
        }
        if let Some(t) = tiles(c.tile())
            && has_data(t, c.index_in_tile())
        {
            best = Some((d2, c, t));
        }
    }
    Ok(best.map(|(d2, cell, tile)| Found {
        cell,
        tile,
        distance_cells: f64::from(d2).sqrt(),
    }))
}

/// [`nearest`] inside one tile (the WebAssembly case: the host fetched
/// the point's tile only).
///
/// # Errors
/// [`LookupError`] for an invalid coordinate or a point in another tile.
pub fn nearest_in_tile<'a>(
    tile: &'a LayerTile,
    lat: f64,
    lon: f64,
    radius: usize,
    has_data: impl Fn(&LayerTile, usize) -> bool,
) -> Result<Option<Found<'a>>, LookupError> {
    let cell = LayerCell::of(lat, lon).ok_or(LookupError::InvalidCoordinate)?;
    if cell.tile() != tile.id() {
        return Err(LookupError::WrongTile {
            expected: cell.tile().name(),
            given: tile.id().name(),
        });
    }
    nearest(
        |id| (id == tile.id()).then_some(tile),
        lat,
        lon,
        radius,
        has_data,
    )
}

/// Round to 6 decimals (cell centres in JSON).
pub(crate) fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

#[cfg(test)]
pub(crate) mod synthetic {
    //! Synthetic soil and regions tiles for tests (also the committed
    //! fixtures `tests/fixtures/synthetic-{soil,regions}-n50e000.tif`).

    use super::{LAYER_TILE_CELLS, LAYER_TILE_LEN, LayerTile};
    use crate::grid::TileId;
    use crate::model::NODATA;
    use crate::regions::{REGION_BANDS, encode_code};
    use crate::soil::SOIL_BANDS;

    /// Land: 40-46 N, 0-6 E of tile n50e000.
    pub(crate) fn is_land(x: usize, y: usize) -> bool {
        (80..200).contains(&y) && x < 120
    }

    /// Soil: measured on land except a "city" at 45-45.5 N, 3-3.5 E,
    /// filled from 1-2 cells away.
    pub(crate) fn soil() -> LayerTile {
        let id = TileId { col: 18, row: 4 };
        let mut data = vec![NODATA; LAYER_TILE_LEN * SOIL_BANDS];
        for y in 0..LAYER_TILE_CELLS {
            for x in 0..LAYER_TILE_CELLS {
                if !is_land(x, y) {
                    continue;
                }
                let city = (90..100).contains(&y) && (60..70).contains(&x);
                let fill = if city { 1 + i16::from(x == 65) } else { 0 };
                let i = y * LAYER_TILE_CELLS + x;
                let v = [
                    55 + (x % 30) as i16,
                    100 + 5 * (y % 100) as i16,
                    50 + 3 * (x % 100) as i16,
                    fill,
                ];
                data[i * SOIL_BANDS..(i + 1) * SOIL_BANDS].copy_from_slice(&v);
            }
        }
        LayerTile {
            id,
            bands: SOIL_BANDS,
            data,
        }
    }

    /// Regions: "FRA"/"FR" west of 3 E, "SWI"/"CH" east of it on land.
    pub(crate) fn regions() -> LayerTile {
        let id = TileId { col: 18, row: 4 };
        let mut data = vec![NODATA; LAYER_TILE_LEN * REGION_BANDS];
        for j in 0..LAYER_TILE_LEN {
            let (x, y) = (j % LAYER_TILE_CELLS, j / LAYER_TILE_CELLS);
            if !is_land(x, y) {
                continue;
            }
            let (l3, a2) = if x < 60 { ("FRA", "FR") } else { ("SWI", "CH") };
            data[j * 2] = encode_code(l3).unwrap_or(NODATA);
            data[j * 2 + 1] = encode_code(a2).unwrap_or(NODATA);
        }
        LayerTile {
            id,
            bands: REGION_BANDS,
            data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Cell;

    #[test]
    fn cells_nest_in_the_climate_grid() {
        for (lat, lon) in [
            (48.8566, 2.3522),
            (-23.55, -46.63),
            (0.0, 0.0),
            (-90.0, 180.0),
            (89.99, -179.99),
            (48.9, 0.3),
        ] {
            let fine = LayerCell::of(lat, lon).unwrap();
            let coarse = Cell::of(lat, lon).unwrap();
            assert_eq!(fine.tile(), coarse.tile(), "{lat} {lon}");
            assert_eq!((fine.col / 2, fine.row / 2), (coarse.col, coarse.row));
            assert_eq!(LayerCell::in_tile(fine.tile(), fine.index_in_tile()), fine);
            assert_eq!(LayerCell::from_global(fine.global_index()), fine);
        }
        let c = LayerCell::of(48.8566, 2.3522).unwrap();
        assert!((c.center_lat() - 48.875).abs() < 1e-9);
        assert!((c.center_lon() - 2.375).abs() < 1e-9);
        assert_eq!(LayerCell::of(0.0, 180.0), LayerCell::of(0.0, -180.0));
        assert_eq!(LayerCell::of(f64::NAN, 0.0), None);
    }

    fn tile_with(id: TileId, cells: &[usize]) -> LayerTile {
        let mut data = vec![NODATA; LAYER_TILE_LEN];
        for &i in cells {
            data[i] = 7;
        }
        LayerTile::new(id, 1, data).unwrap()
    }

    fn has(t: &LayerTile, i: usize) -> bool {
        t.get(i, 0) != NODATA
    }

    #[test]
    fn nearest_prefers_own_cell_then_closest() {
        let id = TileId::parse("n50e000").unwrap();
        let own = LayerCell::of(45.0, 5.0).unwrap();
        let t = tile_with(id, &[own.index_in_tile()]);
        let f = nearest_in_tile(&t, 45.0, 5.0, 2, has).unwrap().unwrap();
        assert_eq!((f.cell, f.distance_cells), (own, 0.0));
        // Two cells east: found within 2, not within 1.
        let f = nearest_in_tile(&t, 45.0, 4.9, 2, has).unwrap().unwrap();
        assert_eq!((f.cell, f.distance_cells), (own, 2.0));
        assert!(nearest_in_tile(&t, 45.0, 4.9, 1, has).unwrap().is_none());
        assert!(matches!(
            nearest_in_tile(&t, 35.0, 5.0, 2, has),
            Err(LookupError::WrongTile { .. })
        ));
        assert!(LayerTile::new(id, 2, vec![0; LAYER_TILE_LEN]).is_err());
    }

    #[test]
    fn committed_fixtures_decode() {
        let soil = include_bytes!("../tests/fixtures/synthetic-soil-n50e000.tif");
        let regions = include_bytes!("../tests/fixtures/synthetic-regions-n50e000.tif");
        assert_eq!(crate::soil::decode(soil).unwrap(), synthetic::soil());
        assert_eq!(
            crate::regions::decode(regions).unwrap(),
            synthetic::regions()
        );
        assert!(crate::soil::decode(regions).is_err(), "band count checked");
        assert!(crate::tile::Tile::decode(soil).is_err());
        assert!(crate::soil::decode(&soil[..soil.len() / 2]).is_err());
    }

    #[test]
    fn nearest_crosses_the_antimeridian() {
        // Data in the easternmost cell of n70e170; the point is in the
        // westernmost cell of n70w180.
        let east = TileId::parse("n70e170").unwrap();
        let west = TileId::parse("n70w180").unwrap();
        let target = LayerCell::of(65.0, 179.99).unwrap();
        let t = tile_with(east, &[target.index_in_tile()]);
        let ids = search_tiles(65.0, -179.99, 2).unwrap();
        assert!(ids.contains(&east) && ids.contains(&west));
        let f = nearest(|id| (id == east).then_some(&t), 65.0, -179.99, 2, has)
            .unwrap()
            .unwrap();
        assert_eq!((f.cell, f.distance_cells), (target, 1.0));
        assert_eq!(f.tile.id(), east);
    }
}
