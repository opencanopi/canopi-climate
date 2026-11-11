//! Tile writer: one Cloud Optimized GeoTIFF per tile, written by `wbgeotiff`.
//!
//! `wbgeotiff` 9c0ff4f writes COGs pixel-interleaved, without a predictor and
//! without GDAL metadata (band names, scale/offset): the manifest carries
//! those (docs/upstream-gaps.md).

use std::path::Path;

use wbgeotiff::{CogWriter, Compression, GeoTransform};

use super::{PipelineError, Result};
use crate::grid::{CELL_DEG, TILE_CELLS};
use crate::layer::{LAYER_CELL_DEG, LAYER_TILE_CELLS, LayerTile};
use crate::model::{BANDS, NODATA};
use crate::tile::Tile;

/// Internal TIFF tile size; one internal tile covers the 100 x 100 raster.
/// 112 is the smallest multiple of 16 above 100
/// (docs/decisions/0001-tile-format.md).
pub const INTERNAL_TILE: u32 = 112;

/// Write a tile as a COG at `path`.
///
/// # Errors
/// Writer or I/O errors.
pub fn write_tile(tile: &Tile, path: &Path) -> Result<()> {
    let id = tile.id();
    let bands = u16::try_from(BANDS).map_err(|e| PipelineError::Data(e.to_string()))?;
    let side = u32::try_from(TILE_CELLS).map_err(|e| PipelineError::Data(e.to_string()))?;
    CogWriter::new(side, side, bands)
        .compression(Compression::Deflate)
        .tile_size(INTERNAL_TILE)
        .no_data(f64::from(NODATA))
        .overview_levels(vec![])
        .geo_transform(GeoTransform::north_up(
            id.west(),
            CELL_DEG,
            id.north(),
            -CELL_DEG,
        ))
        .epsg(4326)
        .write_i16(path, tile.data())?;
    Ok(())
}

/// Write a tile and read it back with the crate's reader; errors if the
/// values differ.
///
/// # Errors
/// Writer, reader or I/O errors, or a value mismatch.
pub fn write_tile_checked(tile: &Tile, path: &Path) -> Result<Vec<u8>> {
    write_tile(tile, path)?;
    let bytes = std::fs::read(path).map_err(|e| PipelineError::io(path, e))?;
    let back = Tile::decode(&bytes)?;
    if back != *tile {
        return Err(PipelineError::Raster(format!(
            "{}: read-back differs from the written tile",
            path.display()
        )));
    }
    Ok(bytes)
}

/// Internal TIFF tile size of the site layers: 208 is the smallest multiple
/// of 16 above 200, so one internal tile covers the 200 x 200 layer tile.
pub const LAYER_INTERNAL_TILE: u32 = 208;

/// Write a site-layer tile (`crate::layer`) as a COG and read it back with
/// the crate's reader; errors if the values differ.
///
/// # Errors
/// Writer, reader or I/O errors, or a value mismatch.
pub fn write_layer_tile_checked(tile: &LayerTile, path: &Path) -> Result<Vec<u8>> {
    let id = tile.id();
    let bands = u16::try_from(tile.bands()).map_err(|e| PipelineError::Data(e.to_string()))?;
    let side = u32::try_from(LAYER_TILE_CELLS).map_err(|e| PipelineError::Data(e.to_string()))?;
    CogWriter::new(side, side, bands)
        .compression(Compression::Deflate)
        .tile_size(LAYER_INTERNAL_TILE)
        .no_data(f64::from(NODATA))
        .overview_levels(vec![])
        .geo_transform(GeoTransform::north_up(
            id.west(),
            LAYER_CELL_DEG,
            id.north(),
            -LAYER_CELL_DEG,
        ))
        .epsg(4326)
        .write_i16(path, tile.data())?;
    let bytes = std::fs::read(path).map_err(|e| PipelineError::io(path, e))?;
    let back = LayerTile::decode(&bytes, tile.bands())?;
    if back != *tile {
        return Err(PipelineError::Raster(format!(
            "{}: read-back differs from the written tile",
            path.display()
        )));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::TileId;
    use crate::tile::synthetic;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/synthetic-n50e000.tif"
    );

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("canopi-climate-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn tile_round_trips_through_cog() {
        for id in [
            TileId::parse("n50e000").unwrap(),
            TileId::parse("s80w180").unwrap(),
            TileId::parse("n90e170").unwrap(),
        ] {
            let t = synthetic::tile(id);
            let path = temp(&id.file_name());
            let bytes = write_tile_checked(&t, &path).unwrap();
            assert_eq!(Tile::decode(&bytes).unwrap().id(), id);
            // wbgeotiff reads it back identically too.
            let g = wbgeotiff::GeoTiff::from_bytes(&bytes).unwrap();
            assert_eq!(g.band_count(), BANDS);
            assert_eq!(g.no_data(), Some(f64::from(NODATA)));
            let b9 = g.read_band_i16(9).unwrap();
            for (i, v) in b9.iter().enumerate() {
                assert_eq!(*v, t.get(i, 9));
            }
        }
    }

    #[test]
    fn layer_tile_round_trips_through_cog() {
        use crate::layer::LAYER_TILE_LEN;
        for (name, bands) in [("n50e000", 4), ("s80w180", 2), ("n90e170", 1)] {
            let id = TileId::parse(name).unwrap();
            let data: Vec<i16> = (0..LAYER_TILE_LEN * bands)
                .map(|i| {
                    if i % 7 == 0 {
                        NODATA
                    } else {
                        (i % 17_576) as i16
                    }
                })
                .collect();
            let t = LayerTile::new(id, bands, data).unwrap();
            let path = temp(&format!("layer-{name}.tif"));
            let bytes = write_layer_tile_checked(&t, &path).unwrap();
            let back = LayerTile::decode(&bytes, bands).unwrap();
            assert_eq!(back.id(), id);
            assert!(LayerTile::decode(&bytes, bands + 1).is_err());
            // Not a climate tile.
            assert!(Tile::decode(&bytes).is_err());
        }
    }

    /// The committed fixture is what the writer produces today. Set
    /// `CANOPI_REGEN_FIXTURES=1` to rewrite it after a deliberate change.
    #[test]
    fn committed_fixture_is_reproducible() {
        let t = synthetic::tile(TileId::parse("n50e000").unwrap());
        let path = temp("fixture.tif");
        let bytes = write_tile_checked(&t, &path).unwrap();
        if std::env::var_os("CANOPI_REGEN_FIXTURES").is_some() {
            std::fs::write(FIXTURE, &bytes).unwrap();
        }
        assert_eq!(std::fs::read(FIXTURE).unwrap(), bytes, "fixture drifted");
    }
}
