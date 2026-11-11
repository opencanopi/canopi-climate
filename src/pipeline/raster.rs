//! Global 0.1-degree intermediate rasters (GeoTIFF via `wbgeotiff`), kept in
//! the work directory between build stages and readable in QGIS.

use std::path::Path;

use wbgeotiff::{Compression, GeoTiff, GeoTiffWriter, GeoTransform, WriteLayout};

use super::{PipelineError, Result};
use crate::grid::{CELL_DEG, GRID_H, GRID_W};

fn writer(bands: u16) -> Result<GeoTiffWriter> {
    let w = u32::try_from(GRID_W).map_err(|e| PipelineError::Data(e.to_string()))?;
    let h = u32::try_from(GRID_H).map_err(|e| PipelineError::Data(e.to_string()))?;
    Ok(GeoTiffWriter::new(w, h, bands)
        .layout(WriteLayout::Tiled {
            tile_width: 512,
            tile_height: 512,
        })
        .compression(Compression::Deflate)
        .geo_transform(GeoTransform::north_up(-180.0, CELL_DEG, 90.0, -CELL_DEG))
        .epsg(4326))
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| PipelineError::io(dir, e))?;
    }
    Ok(())
}

/// Write `bands` global `f32` layers (each `GRID_W * GRID_H`, NaN = no data).
///
/// # Errors
/// Wrong lengths, writer or I/O errors.
pub fn write_f32(path: &Path, layers: &[&[f32]]) -> Result<()> {
    let n = GRID_W * GRID_H;
    if layers.is_empty() || layers.iter().any(|l| l.len() != n) {
        return Err(PipelineError::Data(format!(
            "{}: layer length",
            path.display()
        )));
    }
    let bands = layers.len();
    let mut inter = vec![0f32; n * bands];
    for (b, layer) in layers.iter().enumerate() {
        for (i, v) in layer.iter().enumerate() {
            inter[i * bands + b] = *v;
        }
    }
    ensure_parent(path)?;
    let bands = u16::try_from(bands).map_err(|e| PipelineError::Data(e.to_string()))?;
    writer(bands)?.no_data(f64::NAN).write_f32(path, &inter)?;
    Ok(())
}

/// Write one global `u8` layer (0 = no data).
///
/// # Errors
/// Wrong length, writer or I/O errors.
pub fn write_u8(path: &Path, layer: &[u8]) -> Result<()> {
    if layer.len() != GRID_W * GRID_H {
        return Err(PipelineError::Data(format!(
            "{}: layer length",
            path.display()
        )));
    }
    ensure_parent(path)?;
    writer(1)?.no_data(0.0).write_u8(path, layer)?;
    Ok(())
}

fn open_global(path: &Path) -> Result<GeoTiff> {
    let g = GeoTiff::open(path)?;
    if (g.width() as usize, g.height() as usize) != (GRID_W, GRID_H) {
        return Err(PipelineError::Data(format!(
            "{}: {}x{}, expected the 0.1-degree grid",
            path.display(),
            g.width(),
            g.height()
        )));
    }
    Ok(g)
}

/// Read every band of a global `f32` raster written by [`write_f32`].
///
/// # Errors
/// Reader errors or a raster that is not the global grid.
pub fn read_f32(path: &Path) -> Result<Vec<Vec<f32>>> {
    let g = open_global(path)?;
    (0..g.band_count())
        .map(|b| g.read_band_f32(b).map_err(PipelineError::from))
        .collect()
}

/// Read a global `u8` raster written by [`write_u8`].
///
/// # Errors
/// Reader errors or a raster that is not the global grid.
pub fn read_u8(path: &Path) -> Result<Vec<u8>> {
    Ok(open_global(path)?.read_band_u8(0)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_layers_round_trip() {
        let dir =
            std::env::temp_dir().join(format!("canopi-climate-raster-{}", std::process::id()));
        let n = GRID_W * GRID_H;
        let a: Vec<f32> = (0..n)
            .map(|i| if i % 7 == 0 { f32::NAN } else { i as f32 * 0.5 })
            .collect();
        let b: Vec<f32> = (0..n).map(|i| -(i as f32)).collect();
        let p = dir.join("f.tif");
        write_f32(&p, &[&a, &b]).unwrap();
        let back = read_f32(&p).unwrap();
        assert_eq!(back.len(), 2);
        for i in (0..n).step_by(9973) {
            assert!(back[0][i] == a[i] || (back[0][i].is_nan() && a[i].is_nan()));
            assert_eq!(back[1][i], b[i]);
        }
        let k: Vec<u8> = (0..n).map(|i| (i % 31) as u8).collect();
        let pk = dir.join("k.tif");
        write_u8(&pk, &k).unwrap();
        assert_eq!(read_u8(&pk).unwrap(), k);
        assert!(write_u8(&pk, &k[..10]).is_err());
    }
}
