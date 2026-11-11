//! The global grid: 0.1-degree cells in 10 x 10 degree tiles, latitude/longitude
//! (EPSG:4326).
//!
//! - Global cell `(col, row)`: `col` 0 starts at 180 W, `row` 0 starts at 90 N.
//!   A cell covers longitudes `[west, west + 0.1)` and latitudes
//!   `(north - 0.1, north]`; latitude -90 belongs to the last row and longitude
//!   180 wraps to -180 (the antimeridian).
//! - Tile `(col, row)` holds 100 x 100 cells; it is named after its north-west
//!   corner, which is also its raster origin: `n50e000` covers 40-50 N,
//!   0-10 E.

use std::fmt;

/// Cell size in degrees.
pub const CELL_DEG: f64 = 0.1;
/// Tile size in degrees.
pub const TILE_DEG: f64 = 10.0;
/// Cells per tile side.
pub const TILE_CELLS: usize = 100;
/// Tiles across the globe (longitude).
pub const TILES_X: usize = 36;
/// Tiles down the globe (latitude).
pub const TILES_Y: usize = 18;
/// Global grid width in cells.
pub const GRID_W: usize = TILES_X * TILE_CELLS;
/// Global grid height in cells.
pub const GRID_H: usize = TILES_Y * TILE_CELLS;
/// Cells per tile.
pub const TILE_LEN: usize = TILE_CELLS * TILE_CELLS;

/// Absorbs binary rounding of decimal inputs (48.9 * 10 = 488.99999...).
const EPS: f64 = 1e-9;

/// A cell of the global grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Cell {
    /// Column, 0 at 180 W.
    pub col: usize,
    /// Row, 0 at 90 N.
    pub row: usize,
}

impl Cell {
    /// The cell holding a point, or `None` for a latitude outside [-90, 90]
    /// or a non-finite coordinate. Longitudes wrap around the globe.
    #[must_use]
    pub fn of(lat: f64, lon: f64) -> Option<Self> {
        if !lat.is_finite() || !lon.is_finite() || !(-90.0..=90.0).contains(&lat) {
            return None;
        }
        let lon = (lon + 180.0).rem_euclid(360.0);
        let col = grid_index(lon / CELL_DEG, GRID_W);
        let row = grid_index((90.0 - lat) / CELL_DEG, GRID_H);
        Some(Self { col, row })
    }

    /// Latitude of the cell centre.
    #[must_use]
    pub fn center_lat(self) -> f64 {
        90.0 - (self.row as f64 + 0.5) * CELL_DEG
    }

    /// Longitude of the cell centre.
    #[must_use]
    pub fn center_lon(self) -> f64 {
        -180.0 + (self.col as f64 + 0.5) * CELL_DEG
    }

    /// The tile holding this cell.
    #[must_use]
    pub fn tile(self) -> TileId {
        TileId {
            col: self.col / TILE_CELLS,
            row: self.row / TILE_CELLS,
        }
    }

    /// Row-major index of the cell inside its tile.
    #[must_use]
    pub fn index_in_tile(self) -> usize {
        (self.row % TILE_CELLS) * TILE_CELLS + self.col % TILE_CELLS
    }

    /// Row-major index of the cell in the global grid.
    #[must_use]
    pub fn global_index(self) -> usize {
        self.row * GRID_W + self.col
    }
}

/// `floor(x)` clamped to `0..n`, tolerant of decimal rounding.
fn grid_index(x: f64, n: usize) -> usize {
    let i = (x + EPS).floor();
    if i <= 0.0 {
        0
    } else {
        // `i` is a small non-negative integer here (at most a few thousand).
        let i = i as usize;
        i.min(n - 1)
    }
}

/// A 10 x 10 degree tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileId {
    /// Tile column, 0 at 180 W.
    pub col: usize,
    /// Tile row, 0 at 90 N.
    pub row: usize,
}

impl TileId {
    /// The tile holding a point (see [`Cell::of`]).
    #[must_use]
    pub fn of(lat: f64, lon: f64) -> Option<Self> {
        Cell::of(lat, lon).map(Cell::tile)
    }

    /// Longitude of the west edge.
    #[must_use]
    pub fn west(self) -> f64 {
        -180.0 + TILE_DEG * self.col as f64
    }

    /// Latitude of the north edge.
    #[must_use]
    pub fn north(self) -> f64 {
        90.0 - TILE_DEG * self.row as f64
    }

    /// Tile name without extension, e.g. `n50e000`, `s10w080`.
    #[must_use]
    pub fn name(self) -> String {
        self.to_string()
    }

    /// File name of the tile, e.g. `n50e000.tif`.
    #[must_use]
    pub fn file_name(self) -> String {
        format!("{self}.tif")
    }

    /// Parse a tile name (`n50e000`, with or without `.tif`).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        let name = name.strip_suffix(".tif").unwrap_or(name);
        let b = name.as_bytes();
        if b.len() != 7 || !name.is_ascii() {
            return None;
        }
        let lat: i32 = name.get(1..3)?.parse().ok()?;
        let lon: i32 = name.get(4..7)?.parse().ok()?;
        let north = match b[0] {
            b'n' => lat,
            b's' => -lat,
            _ => return None,
        };
        let west = match b[3] {
            b'e' => lon,
            b'w' => -lon,
            _ => return None,
        };
        if north % 10 != 0 || west % 10 != 0 || !(-80..=90).contains(&north) {
            return None;
        }
        if !(-180..=170).contains(&west) {
            return None;
        }
        let row = usize::try_from((90 - north) / 10).ok()?;
        let col = usize::try_from((west + 180) / 10).ok()?;
        Some(Self { col, row })
    }

    /// Global cell of a row-major index inside this tile.
    #[must_use]
    pub fn cell(self, index: usize) -> Cell {
        Cell {
            col: self.col * TILE_CELLS + index % TILE_CELLS,
            row: self.row * TILE_CELLS + index / TILE_CELLS,
        }
    }

    /// Every tile of the globe, row by row from the north.
    pub fn all() -> impl Iterator<Item = Self> {
        (0..TILES_Y).flat_map(|row| (0..TILES_X).map(move |col| Self { col, row }))
    }
}

impl fmt::Display for TileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let north = 90 - 10 * i64::try_from(self.row).unwrap_or(0);
        let west = -180 + 10 * i64::try_from(self.col).unwrap_or(0);
        write!(
            f,
            "{}{:02}{}{:03}",
            if north >= 0 { 'n' } else { 's' },
            north.abs(),
            if west >= 0 { 'e' } else { 'w' },
            west.abs()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paris_is_in_n50e000() {
        let t = TileId::of(48.8566, 2.3522).unwrap();
        assert_eq!(t.name(), "n50e000");
        assert_eq!(t.file_name(), "n50e000.tif");
        assert_eq!((t.west(), t.north()), (0.0, 50.0));
    }

    #[test]
    fn southern_and_western_names() {
        assert_eq!(TileId::of(-1.29, 36.82).unwrap().name(), "n00e030");
        assert_eq!(TileId::of(-23.55, -46.63).unwrap().name(), "s20w050");
        assert_eq!(TileId::of(-0.0001, -0.0001).unwrap().name(), "n00w010");
        assert_eq!(TileId::of(-89.0, 0.0).unwrap().name(), "s80e000");
    }

    #[test]
    fn names_round_trip_for_every_tile() {
        let mut n = 0;
        for t in TileId::all() {
            assert_eq!(TileId::parse(&t.name()), Some(t));
            assert_eq!(TileId::parse(&t.file_name()), Some(t));
            n += 1;
        }
        assert_eq!(n, TILES_X * TILES_Y);
        assert_eq!(TileId::parse("n55e000"), None);
        assert_eq!(TileId::parse("x50e000"), None);
        assert_eq!(TileId::parse("n50e180"), None);
        assert_eq!(TileId::parse("s80e000"), Some(TileId { col: 18, row: 17 }));
        assert_eq!(TileId::parse("s90e000"), None);
    }

    #[test]
    fn edges_follow_the_documented_convention() {
        // West edge inclusive, north edge inclusive.
        assert_eq!(
            Cell::of(50.0, 0.0).unwrap(),
            Cell {
                col: 1800,
                row: 400
            }
        );
        assert_eq!(
            Cell::of(49.95, 0.05).unwrap(),
            Cell {
                col: 1800,
                row: 400
            }
        );
        assert_eq!(
            Cell::of(49.9, 0.1).unwrap(),
            Cell {
                col: 1801,
                row: 401
            }
        );
        // Decimal inputs land on the cell they name.
        assert_eq!(
            Cell::of(48.9, 0.3).unwrap(),
            Cell {
                col: 1803,
                row: 411
            }
        );
    }

    #[test]
    fn poles_and_antimeridian() {
        assert_eq!(Cell::of(90.0, 0.0).unwrap().row, 0);
        assert_eq!(Cell::of(-90.0, 0.0).unwrap().row, GRID_H - 1);
        assert_eq!(Cell::of(0.0, 180.0).unwrap().col, 0);
        assert_eq!(Cell::of(0.0, -180.0).unwrap().col, 0);
        assert_eq!(Cell::of(0.0, 179.99).unwrap().col, GRID_W - 1);
        assert_eq!(Cell::of(0.0, 190.0), Cell::of(0.0, -170.0));
        assert_eq!(Cell::of(0.0, -190.0), Cell::of(0.0, 170.0));
        assert_eq!(Cell::of(90.1, 0.0), None);
        assert_eq!(Cell::of(f64::NAN, 0.0), None);
        assert_eq!(Cell::of(0.0, f64::INFINITY), None);
    }

    #[test]
    fn cell_centres_and_indices() {
        let c = Cell::of(48.8566, 2.3522).unwrap();
        assert!((c.center_lat() - 48.85).abs() < 1e-9);
        assert!((c.center_lon() - 2.35).abs() < 1e-9);
        let t = c.tile();
        assert_eq!(t.cell(c.index_in_tile()), c);
        assert_eq!(c.global_index(), c.row * GRID_W + c.col);
    }
}
