//! Generic tiled raster geometry and decoding shared by the climate tiles and
//! every layer (`soil`, `regions`, `extremes`, `monthly`, `monthly_precip`).
//!
//! Every grid and layer shares the same 10 x 10 degree tiles
//! (`crate::grid::TILE_DEG`), with `tile_cells * cell_deg = 10` exactly.

use crate::grid::{TILE_DEG, TILES_X, TILES_Y, TileId, grid_index};
use crate::lookup::LookupError;
use crate::model::NODATA;
use crate::tile::{TileError, decode_raster};

/// Geometry of a 10-degree tiled grid or layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerGrid {
    /// Cells per 10-degree tile side (100 for 0.1°, 200 for 0.05°, 40 for 0.25°).
    tile_cells: usize,
}

impl LayerGrid {
    /// 0.1-degree grid: 100 x 100 cells per tile (climate, `extremes`, `monthly`).
    pub const CLIMATE: Self = Self { tile_cells: 100 };
    /// 0.05-degree grid: 200 x 200 cells per tile (`soil`, `regions`).
    pub const SITE: Self = Self { tile_cells: 200 };
    /// 0.25-degree grid: 40 x 40 cells per tile (`monthly_precip`).
    pub const QUARTER_DEG: Self = Self { tile_cells: 40 };

    /// Construct a grid with `tile_cells` cells per 10-degree tile side.
    /// Returns `None` if `tile_cells == 0`.
    #[must_use]
    pub const fn new(tile_cells: usize) -> Option<Self> {
        if tile_cells == 0 {
            None
        } else {
            Some(Self { tile_cells })
        }
    }

    /// Cells per tile side.
    #[must_use]
    pub const fn tile_cells(self) -> usize {
        self.tile_cells
    }

    /// Cell size in degrees (`10.0 / tile_cells`).
    #[must_use]
    pub fn cell_deg(self) -> f64 {
        TILE_DEG / self.tile_cells as f64
    }

    /// Cells per tile (`tile_cells * tile_cells`, saturating on overflow).
    #[must_use]
    pub const fn tile_len(self) -> usize {
        self.tile_cells.saturating_mul(self.tile_cells)
    }

    /// Global grid width in cells.
    #[must_use]
    pub const fn grid_w(self) -> usize {
        TILES_X * self.tile_cells
    }

    /// Global grid height in cells.
    #[must_use]
    pub const fn grid_h(self) -> usize {
        TILES_Y * self.tile_cells
    }

    /// Internal COG tile size: smallest multiple of 16 >= `tile_cells`
    /// (112 for 100, 208 for 200, 48 for 40).
    #[must_use]
    pub const fn internal_tile(self) -> usize {
        self.tile_cells.div_ceil(16) * 16
    }

    /// The cell holding a point, or `None` for an invalid coordinate.
    #[must_use]
    pub fn cell_of(self, lat: f64, lon: f64) -> Option<GridCell> {
        if !lat.is_finite() || !lon.is_finite() || !(-90.0..=90.0).contains(&lat) {
            return None;
        }
        let lon = (lon + 180.0).rem_euclid(360.0);
        let deg = self.cell_deg();
        Some(GridCell {
            col: grid_index(lon / deg, self.grid_w()),
            row: grid_index((90.0 - lat) / deg, self.grid_h()),
        })
    }

    /// Latitude of a cell's centre.
    #[must_use]
    pub fn center_lat(self, cell: GridCell) -> f64 {
        90.0 - (cell.row as f64 + 0.5) * self.cell_deg()
    }

    /// Longitude of a cell's centre.
    #[must_use]
    pub fn center_lon(self, cell: GridCell) -> f64 {
        -180.0 + (cell.col as f64 + 0.5) * self.cell_deg()
    }

    /// The 10-degree tile holding a cell.
    #[must_use]
    pub const fn tile_of(self, cell: GridCell) -> TileId {
        TileId {
            col: cell.col / self.tile_cells,
            row: cell.row / self.tile_cells,
        }
    }

    /// Row-major index of a cell inside its tile.
    #[must_use]
    pub const fn index_in_tile(self, cell: GridCell) -> usize {
        (cell.row % self.tile_cells) * self.tile_cells + cell.col % self.tile_cells
    }

    /// Row-major index of a cell in the global grid.
    #[must_use]
    pub const fn global_index(self, cell: GridCell) -> usize {
        cell.row * self.grid_w() + cell.col
    }

    /// The cell of a global row-major index.
    #[must_use]
    pub const fn from_global(self, index: usize) -> GridCell {
        let w = self.grid_w();
        GridCell {
            col: index % w,
            row: index / w,
        }
    }

    /// The global cell of a row-major index inside a tile.
    #[must_use]
    pub const fn in_tile(self, tile: TileId, index: usize) -> GridCell {
        GridCell {
            col: tile.col * self.tile_cells + index % self.tile_cells,
            row: tile.row * self.tile_cells + index / self.tile_cells,
        }
    }
}

/// A cell of any [`LayerGrid`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GridCell {
    /// Column, 0 at 180 W.
    pub col: usize,
    /// Row, 0 at 90 N.
    pub row: usize,
}

/// A decoded pixel-interleaved `i16` raster tile on a [`LayerGrid`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raster {
    id: TileId,
    grid: LayerGrid,
    bands: usize,
    /// Pixel-interleaved: `data[cell * bands + band]`.
    data: Vec<i16>,
}

impl Raster {
    /// Construct a raster from pixel-interleaved values.
    ///
    /// # Errors
    /// [`TileError::Layout`] if `bands == 0` or `data.len() != grid.tile_len() * bands`.
    pub fn new(
        id: TileId,
        grid: LayerGrid,
        bands: usize,
        data: Vec<i16>,
    ) -> Result<Self, TileError> {
        let Some(expected) = grid
            .tile_cells
            .checked_mul(grid.tile_cells)
            .and_then(|n| n.checked_mul(bands))
        else {
            return Err(TileError::Layout(format!(
                "tile size overflow for {} cells x {bands} bands",
                grid.tile_cells
            )));
        };
        if bands == 0 || data.len() != expected {
            return Err(TileError::Layout(format!(
                "{} values for {bands} bands (expected {expected})",
                data.len()
            )));
        }
        Ok(Self {
            id,
            grid,
            bands,
            data,
        })
    }

    /// Decode a GeoTIFF tile matching `grid` and `bands`.
    ///
    /// # Errors
    /// [`TileError`] when the bytes do not match the expected raster layout or georeferencing.
    pub fn decode(bytes: &[u8], grid: LayerGrid, bands: usize) -> Result<Self, TileError> {
        let (id, data) = decode_raster(bytes, grid.tile_cells(), grid.cell_deg(), bands)?;
        Self::new(id, grid, bands, data)
    }

    /// The tile's position.
    #[must_use]
    pub const fn id(&self) -> TileId {
        self.id
    }

    /// The raster's grid geometry.
    #[must_use]
    pub const fn grid(&self) -> LayerGrid {
        self.grid
    }

    /// Bands per cell.
    #[must_use]
    pub const fn bands(&self) -> usize {
        self.bands
    }

    /// All bands of a cell (row-major index inside the tile), or `&[]` if out of range.
    #[must_use]
    pub fn pixel(&self, index: usize) -> &[i16] {
        let Some(start) = index.checked_mul(self.bands) else {
            return &[];
        };
        let Some(end) = start.checked_add(self.bands) else {
            return &[];
        };
        self.data.get(start..end).unwrap_or(&[])
    }

    /// One stored value; [`NODATA`] outside the tile or band range.
    #[must_use]
    pub fn get(&self, index: usize, band: usize) -> i16 {
        if band >= self.bands {
            return NODATA;
        }
        index
            .checked_mul(self.bands)
            .and_then(|start| start.checked_add(band))
            .and_then(|at| self.data.get(at).copied())
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
        (0..self.grid.tile_len())
            .filter(|&i| has_data(self, i))
            .count()
    }
}

/// Cells within `radius` of `cell` with their squared distance (cells),
/// north to south then west to east. Longitude wraps; latitude stops at the poles.
pub(crate) fn window(
    grid: LayerGrid,
    cell: GridCell,
    radius: usize,
) -> impl Iterator<Item = (GridCell, u32)> {
    let r = i64::try_from(radius.min(grid.tile_cells())).unwrap_or(0);
    let (row0, col0) = (cell.row as i64, cell.col as i64);
    let (grid_w, grid_h) = (grid.grid_w() as i64, grid.grid_h() as i64);
    (-r..=r).flat_map(move |dy| {
        (-r..=r).filter_map(move |dx| {
            let d2 = dx * dx + dy * dy;
            let row = row0 + dy;
            if d2 > r * r || row < 0 || row >= grid_h {
                return None;
            }
            let col = (col0 + dx).rem_euclid(grid_w);
            Some((
                GridCell {
                    col: usize::try_from(col).ok()?,
                    row: usize::try_from(row).ok()?,
                },
                u32::try_from(d2).ok()?,
            ))
        })
    })
}

/// The cell a generic raster lookup resolved to.
#[derive(Debug, Clone, Copy)]
pub struct FoundRaster<'a> {
    /// The cell used.
    pub cell: GridCell,
    /// Its raster tile.
    pub raster: &'a Raster,
    /// Distance from the point's cell, in cells (0 for the point's cell).
    pub distance_cells: f64,
}

/// Tiles on `grid` that a search of `radius` cells around a point can reach.
///
/// # Errors
/// [`LookupError::InvalidCoordinate`].
pub fn search_tiles(
    grid: LayerGrid,
    lat: f64,
    lon: f64,
    radius: usize,
) -> Result<Vec<TileId>, LookupError> {
    let cell = grid
        .cell_of(lat, lon)
        .ok_or(LookupError::InvalidCoordinate)?;
    let mut ids: Vec<TileId> = window(grid, cell, radius)
        .map(|(c, _)| grid.tile_of(c))
        .collect();
    ids.sort_by_key(|t| (t.row, t.col));
    ids.dedup();
    Ok(ids)
}

/// The point's cell when `has_data` holds there, else the nearest such cell
/// within `radius` cells on `grid` (ties: northern, then western).
///
/// # Errors
/// [`LookupError::InvalidCoordinate`].
pub fn nearest<'a>(
    grid: LayerGrid,
    tiles: impl Fn(TileId) -> Option<&'a Raster>,
    lat: f64,
    lon: f64,
    radius: usize,
    has_data: impl Fn(&Raster, usize) -> bool,
) -> Result<Option<FoundRaster<'a>>, LookupError> {
    let cell = grid
        .cell_of(lat, lon)
        .ok_or(LookupError::InvalidCoordinate)?;
    let mut best: Option<(u32, GridCell, &Raster)> = None;
    for (c, d2) in window(grid, cell, radius) {
        if best.is_some_and(|(b, _, _)| b <= d2) {
            continue;
        }
        if let Some(t) = tiles(grid.tile_of(c))
            && has_data(t, grid.index_in_tile(c))
        {
            best = Some((d2, c, t));
        }
    }
    Ok(best.map(|(d2, cell, raster)| FoundRaster {
        cell,
        raster,
        distance_cells: f64::from(d2).sqrt(),
    }))
}

/// [`nearest`] inside one tile (single-tile / WebAssembly case).
///
/// # Errors
/// [`LookupError`] for an invalid coordinate or a point in another tile.
pub fn nearest_in_tile<'a>(
    raster: &'a Raster,
    lat: f64,
    lon: f64,
    radius: usize,
    has_data: impl Fn(&Raster, usize) -> bool,
) -> Result<Option<FoundRaster<'a>>, LookupError> {
    let grid = raster.grid();
    let cell = grid
        .cell_of(lat, lon)
        .ok_or(LookupError::InvalidCoordinate)?;
    if grid.tile_of(cell) != raster.id() {
        return Err(LookupError::WrongTile {
            expected: grid.tile_of(cell).name(),
            given: raster.id().name(),
        });
    }
    nearest(
        grid,
        |id| (id == raster.id()).then_some(raster),
        lat,
        lon,
        radius,
        has_data,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_grids_have_exact_cell_sizes_and_internal_tiles() {
        assert_eq!(LayerGrid::CLIMATE.cell_deg(), 0.1);
        assert_eq!(LayerGrid::CLIMATE.internal_tile(), 112);
        assert_eq!(LayerGrid::SITE.cell_deg(), 0.05);
        assert_eq!(LayerGrid::SITE.internal_tile(), 208);
        assert_eq!(LayerGrid::QUARTER_DEG.cell_deg(), 0.25);
        assert_eq!(LayerGrid::QUARTER_DEG.internal_tile(), 48);
        assert_eq!(LayerGrid::QUARTER_DEG.grid_w(), 1440);
        assert_eq!(LayerGrid::QUARTER_DEG.grid_h(), 720);
        assert_eq!(LayerGrid::new(0), None);
    }

    #[test]
    fn quarter_degree_cells_and_nearest_work_across_antimeridian() {
        let g = LayerGrid::QUARTER_DEG;
        let east = TileId::parse("n70e170").unwrap();
        let west = TileId::parse("n70w180").unwrap();
        let target = g.cell_of(65.1, 179.9).unwrap();
        assert_eq!(g.tile_of(target), east);
        let mut data = vec![NODATA; g.tile_len()];
        data[g.index_in_tile(target)] = 55;
        let r = Raster::new(east, g, 1, data).unwrap();
        let ids = search_tiles(g, 65.1, -179.9, 1).unwrap();
        assert!(ids.contains(&east) && ids.contains(&west));
        let f = nearest(
            g,
            |id| (id == east).then_some(&r),
            65.1,
            -179.9,
            1,
            |t, i| t.get(i, 0) != NODATA,
        )
        .unwrap()
        .unwrap();
        assert_eq!((f.cell, f.distance_cells), (target, 1.0));
        assert_eq!(f.raster.id(), east);

        // BUG-14: huge index must not overflow in pixel() or get().
        assert!(r.pixel(usize::MAX).is_empty());
        assert_eq!(r.get(usize::MAX, 0), NODATA);
        assert_eq!(LayerGrid::new(usize::MAX).unwrap().tile_len(), usize::MAX);
    }
}
