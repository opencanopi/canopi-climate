//! Tile writer: one Cloud Optimized GeoTIFF per tile, written by `wbgeotiff`.
//!
//! `wbgeotiff` 9c0ff4f writes COGs pixel-interleaved, without a predictor and
//! without GDAL metadata (band names, scale/offset): the manifest carries
//! those (docs/upstream-gaps.md).

use std::path::Path;

use wbgeotiff::{CogWriter, Compression, GeoTransform};

use super::{PipelineError, Result};
use crate::extremes::ExtremesTile;
use crate::layer::LayerTile;
use crate::model::NODATA;
use crate::monthly::MonthlyTile;
use crate::monthly_precip::MonthlyPrecipTile;
use crate::raster::Raster;
use crate::tile::Tile;

/// Internal TIFF tile size; one internal tile covers the 100 x 100 raster.
/// 112 is the smallest multiple of 16 above 100
/// (docs/decisions/0001-tile-format.md).
pub const INTERNAL_TILE: u32 = 112;

/// Write any [`Raster`] as a COG at `path`.
///
/// # Errors
/// Writer or I/O errors.
pub fn write_raster(raster: &Raster, path: &Path) -> Result<()> {
    let grid = raster.grid();
    let id = raster.id();
    let bands = u16::try_from(raster.bands()).map_err(|e| PipelineError::Data(e.to_string()))?;
    let side = u32::try_from(grid.tile_cells()).map_err(|e| PipelineError::Data(e.to_string()))?;
    let internal =
        u32::try_from(grid.internal_tile()).map_err(|e| PipelineError::Data(e.to_string()))?;
    let deg = grid.cell_deg();
    CogWriter::new(side, side, bands)
        .compression(Compression::Deflate)
        .tile_size(internal)
        .no_data(f64::from(NODATA))
        .overview_levels(vec![])
        .geo_transform(GeoTransform::north_up(id.west(), deg, id.north(), -deg))
        .epsg(4326)
        .write_i16(path, raster.data())?;
    Ok(())
}

/// Write any [`Raster`] as a COG and read it back with the crate's reader;
/// errors if the decoded raster differs.
///
/// # Errors
/// Writer, reader or I/O errors, or a value mismatch.
pub fn write_raster_checked(raster: &Raster, path: &Path) -> Result<Vec<u8>> {
    write_raster(raster, path)?;
    let bytes = std::fs::read(path).map_err(|e| PipelineError::io(path, e))?;
    let back = Raster::decode(&bytes, raster.grid(), raster.bands())?;
    if back != *raster {
        return Err(PipelineError::Raster(format!(
            "{}: read-back differs from the written tile",
            path.display()
        )));
    }
    Ok(bytes)
}

/// Write a tile as a COG at `path`.
///
/// # Errors
/// Writer or I/O errors.
pub fn write_tile(tile: &Tile, path: &Path) -> Result<()> {
    write_raster(tile.as_raster(), path)
}

/// Write an `extremes` tile (0.1 degree, 18 bands) and read it back with
/// the crate's reader; errors if the values differ.
///
/// # Errors
/// Writer, reader or I/O errors, or a value mismatch.
pub fn write_extremes_tile_checked(tile: &ExtremesTile, path: &Path) -> Result<Vec<u8>> {
    write_raster_checked(tile.as_raster(), path)
}

/// Write a `monthly` tile (0.1 degree, 75 bands) and read it back with the
/// crate's reader; errors if the values differ.
///
/// # Errors
/// Writer, reader or I/O errors, or a value mismatch.
pub fn write_monthly_tile_checked(tile: &MonthlyTile, path: &Path) -> Result<Vec<u8>> {
    write_raster_checked(tile.as_raster(), path)
}

/// Write a `monthly_precip` tile (0.25 degree, 36 bands) and read it back
/// with the crate's reader; errors if the values differ.
///
/// # Errors
/// Writer, reader or I/O errors, or a value mismatch.
pub fn write_monthly_precip_tile_checked(tile: &MonthlyPrecipTile, path: &Path) -> Result<Vec<u8>> {
    write_raster_checked(tile.as_raster(), path)
}

/// Write a tile and read it back with the crate's reader; errors if the
/// values differ.
///
/// # Errors
/// Writer, reader or I/O errors, or a value mismatch.
pub fn write_tile_checked(tile: &Tile, path: &Path) -> Result<Vec<u8>> {
    write_raster_checked(tile.as_raster(), path)
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
    write_raster_checked(tile.as_raster(), path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::TileId;
    use crate::model::BANDS;
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

    /// The committed layer fixtures are what the writer produces today
    /// (`CANOPI_REGEN_FIXTURES=1` rewrites them).
    #[test]
    fn committed_layer_fixtures_are_reproducible() {
        use crate::layer::synthetic;
        for (name, tile) in [
            ("soil", synthetic::soil()),
            ("regions", synthetic::regions()),
        ] {
            let fixture = format!(
                "{}/tests/fixtures/synthetic-{name}-n50e000.tif",
                env!("CARGO_MANIFEST_DIR")
            );
            let bytes =
                write_layer_tile_checked(&tile, &temp(&format!("{name}-fixture.tif"))).unwrap();
            if std::env::var_os("CANOPI_REGEN_FIXTURES").is_some() {
                std::fs::write(&fixture, &bytes).unwrap();
            }
            assert_eq!(
                std::fs::read(&fixture).unwrap(),
                bytes,
                "{name} fixture drifted"
            );
        }
    }

    /// The committed extremes fixture is what the writer produces today
    /// (`CANOPI_REGEN_FIXTURES=1` rewrites it).
    #[test]
    fn committed_extremes_fixture_is_reproducible() {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/synthetic-extremes-n50e000.tif"
        );
        let tile = crate::extremes::synthetic::extremes();
        let bytes = write_extremes_tile_checked(&tile, &temp("extremes-fixture.tif")).unwrap();
        if std::env::var_os("CANOPI_REGEN_FIXTURES").is_some() {
            std::fs::write(fixture, &bytes).unwrap();
        }
        assert_eq!(
            std::fs::read(fixture).unwrap(),
            bytes,
            "extremes fixture drifted"
        );
        assert!(Tile::decode(&bytes).is_err(), "not a climate tile");
    }

    /// The committed monthly and monthly_precip fixtures are what the writer
    /// produces today (`CANOPI_REGEN_FIXTURES=1` rewrites them).
    #[test]
    fn committed_monthly_fixtures_are_reproducible() {
        let m_fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/synthetic-monthly-n50e000.tif"
        );
        let m_tile = crate::monthly::synthetic::monthly();
        let m_bytes = write_monthly_tile_checked(&m_tile, &temp("monthly-fixture.tif")).unwrap();
        if std::env::var_os("CANOPI_REGEN_FIXTURES").is_some() {
            std::fs::write(m_fixture, &m_bytes).unwrap();
        }
        assert_eq!(
            std::fs::read(m_fixture).unwrap(),
            m_bytes,
            "monthly fixture drifted"
        );

        let p_fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/synthetic-monthly-precip-n50e000.tif"
        );
        let p_tile = crate::monthly_precip::synthetic::monthly_precip();
        let p_bytes =
            write_monthly_precip_tile_checked(&p_tile, &temp("monthly-precip-fixture.tif"))
                .unwrap();
        if std::env::var_os("CANOPI_REGEN_FIXTURES").is_some() {
            std::fs::write(p_fixture, &p_bytes).unwrap();
        }
        assert_eq!(
            std::fs::read(p_fixture).unwrap(),
            p_bytes,
            "monthly_precip fixture drifted"
        );
    }

    /// The committed v0.5.0 fixtures (`extremes_tail`, `frost_calendar`, `analogues.bin`)
    /// are what the writer produces today (`CANOPI_REGEN_FIXTURES=1` rewrites them).
    #[test]
    fn committed_v05_fixtures_are_reproducible() {
        let et_fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/synthetic-extremes-tail-n50e000.tif"
        );
        let et_tile = crate::extremes_tail::synthetic::extremes_tail();
        let et_bytes =
            write_raster_checked(et_tile.as_raster(), &temp("extremes-tail-fixture.tif")).unwrap();
        if std::env::var_os("CANOPI_REGEN_FIXTURES").is_some() {
            std::fs::write(et_fixture, &et_bytes).unwrap();
        }
        assert_eq!(
            std::fs::read(et_fixture).unwrap(),
            et_bytes,
            "extremes_tail fixture drifted"
        );

        let fc_fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/synthetic-frost-calendar-n50e000.tif"
        );
        let fc_tile = crate::frost_calendar::synthetic::frost_calendar();
        let fc_bytes =
            write_raster_checked(fc_tile.as_raster(), &temp("frost-calendar-fixture.tif")).unwrap();
        if std::env::var_os("CANOPI_REGEN_FIXTURES").is_some() {
            std::fs::write(fc_fixture, &fc_bytes).unwrap();
        }
        assert_eq!(
            std::fs::read(fc_fixture).unwrap(),
            fc_bytes,
            "frost_calendar fixture drifted"
        );
    }

    /// The committed v0.6.0 fixtures (`phenology`, `water_balance`) are what
    /// the writer produces today (`CANOPI_REGEN_FIXTURES=1` rewrites them).
    #[test]
    fn committed_v06_fixtures_are_reproducible() {
        let ph_fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/synthetic-phenology-n50e000.tif"
        );
        let ph_tile = crate::phenology::synthetic::phenology();
        let ph_bytes =
            write_raster_checked(ph_tile.as_raster(), &temp("phenology-fixture.tif")).unwrap();
        if std::env::var_os("CANOPI_REGEN_FIXTURES").is_some() {
            std::fs::write(ph_fixture, &ph_bytes).unwrap();
        }
        assert_eq!(
            std::fs::read(ph_fixture).unwrap(),
            ph_bytes,
            "phenology fixture drifted"
        );

        let wb_fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/synthetic-water-balance-n50e000.tif"
        );
        let wb_tile = crate::water_balance::synthetic::water_balance();
        let wb_bytes =
            write_raster_checked(wb_tile.as_raster(), &temp("water-balance-fixture.tif")).unwrap();
        if std::env::var_os("CANOPI_REGEN_FIXTURES").is_some() {
            std::fs::write(wb_fixture, &wb_bytes).unwrap();
        }
        assert_eq!(
            std::fs::read(wb_fixture).unwrap(),
            wb_bytes,
            "water_balance fixture drifted"
        );
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
