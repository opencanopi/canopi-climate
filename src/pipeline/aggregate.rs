//! Block aggregation of 30-arc-second (1 km) rasters to the 0.1-degree grid:
//! 12 x 12 source pixels per cell, read 12 rows at a time.

use std::path::Path;

use super::tiff_rows::TiffRows;
use super::{PipelineError, Result};
use crate::grid::{GRID_H, GRID_W};

/// Source pixels per cell side (0.1 degree / 30 arc seconds).
pub const FACTOR: usize = 12;

/// Rows of the source to skip above the grid's first row, after checking
/// that the source is a 30-arc-second raster aligned with the grid (to a
/// fraction of a pixel) and starting at or below 90 N.
fn source_offset(rows: &TiffRows, path: &Path) -> Result<usize> {
    let (x0, pw, y0, ph) = rows
        .info
        .geo
        .ok_or_else(|| PipelineError::Data(format!("{}: no georeferencing", path.display())))?;
    let px = 0.1 / FACTOR as f64;
    let aligned = (pw - px).abs() < 1e-9 && (ph + px).abs() < 1e-9 && (x0 + 180.0).abs() < px / 2.0;
    if !aligned || (rows.info.width as usize) != GRID_W * FACTOR {
        return Err(PipelineError::Data(format!(
            "{}: not a global 30-arc-second raster (origin {x0}, {y0}; pixel {pw} x {ph}; width {})",
            path.display(),
            rows.info.width
        )));
    }
    let shift = (90.0 - y0) / px;
    if shift < -0.5 || (shift - shift.round()).abs() > 0.5 {
        return Err(PipelineError::Data(format!(
            "{}: north edge {y0}",
            path.display()
        )));
    }
    Ok(shift.round() as usize)
}

/// Run `cell` over every 12 x 12 block of a 30-arc-second raster; rows of the
/// grid not covered by the source get `empty`.
fn for_each_block<T: Copy>(
    path: &Path,
    empty: T,
    mut cell: impl FnMut(&[f32], usize, usize) -> T,
) -> Result<Vec<T>> {
    let mut rows = TiffRows::open(path)?;
    let shift = source_offset(&rows, path)?;
    let sw = rows.info.width as usize;
    let sh = rows.info.height as usize;
    let mut out = vec![empty; GRID_W * GRID_H];
    let mut block = vec![0f32; sw * FACTOR];
    for r in 0..GRID_H {
        let s0 = r * FACTOR;
        if s0 < shift || s0 - shift + FACTOR > sh {
            continue;
        }
        for k in 0..FACTOR {
            let src =
                u32::try_from(s0 - shift + k).map_err(|e| PipelineError::Data(e.to_string()))?;
            rows.read_row(src, &mut block[k * sw..(k + 1) * sw])?;
        }
        for c in 0..GRID_W {
            out[r * GRID_W + c] = cell(&block, sw, c);
        }
    }
    Ok(out)
}

/// Most frequent class (1..=`max_class`) in each block; 0 where the block
/// has no class. Ties go to the smaller code.
///
/// # Errors
/// Reader errors or a misaligned source.
pub fn block_mode(path: &Path, max_class: u8) -> Result<Vec<u8>> {
    let max = usize::from(max_class);
    for_each_block(path, 0u8, |block, sw, c| {
        let mut hist = [0u16; 256];
        for k in 0..FACTOR {
            for v in &block[k * sw + c * FACTOR..k * sw + (c + 1) * FACTOR] {
                if *v >= 1.0 && *v <= max as f32 {
                    hist[*v as usize] += 1;
                }
            }
        }
        let mut best = (0usize, 0u16);
        for (class, &n) in hist.iter().enumerate().take(max + 1).skip(1) {
            if n > best.1 {
                best = (class, n);
            }
        }
        u8::try_from(best.0).unwrap_or(0)
    })
}

/// Mean of the valid pixels of each block, as `value * scale + offset`; NaN
/// where none is valid.
///
/// # Errors
/// Reader errors or a misaligned source.
pub fn block_mean(
    path: &Path,
    scale: f64,
    offset: f64,
    valid: impl Fn(f32) -> bool,
) -> Result<Vec<f32>> {
    for_each_block(path, f32::NAN, |block, sw, c| {
        let (mut sum, mut n) = (0f64, 0u32);
        for k in 0..FACTOR {
            for v in &block[k * sw + c * FACTOR..k * sw + (c + 1) * FACTOR] {
                if v.is_finite() && valid(*v) {
                    sum += f64::from(*v);
                    n += 1;
                }
            }
        }
        if n == 0 {
            f32::NAN
        } else {
            (sum / f64::from(n) * scale + offset) as f32
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wbgeotiff::{Compression, GeoTiffWriter, GeoTransform, WriteLayout};

    /// A global 30-arc-second raster from 84 N down to 83.8 N (CHELSA-like
    /// north edge, two grid rows), stripped, `u16`.
    fn write_source(path: &Path, f: impl Fn(usize, usize) -> u16) -> (usize, usize) {
        let (w, h) = (GRID_W * FACTOR, 2 * FACTOR);
        let data: Vec<u16> = (0..w * h).map(|i| f(i % w, i / w)).collect();
        let px = 0.1 / FACTOR as f64;
        GeoTiffWriter::new(w as u32, h as u32, 1)
            .layout(WriteLayout::Stripped { rows_per_strip: 12 })
            .compression(Compression::Deflate)
            .geo_transform(GeoTransform::north_up(-180.0, px, 84.0, -px))
            .write_u16(path, &data)
            .unwrap();
        (w, h)
    }

    #[test]
    fn mode_and_mean_over_blocks_with_a_northern_offset() {
        let dir = std::env::temp_dir().join(format!("canopi-climate-agg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("src.tif");
        // Class = 3 for most pixels, 7 in the first 5 columns of each block.
        write_source(&p, |x, y| {
            if x % FACTOR < 5 {
                7
            } else if y % 2 == 0 {
                3
            } else {
                0
            }
        });
        let mode = block_mode(&p, 30).unwrap();
        // Grid row 60 (84 N) is the first covered row; 59 is not covered.
        assert_eq!(mode[59 * GRID_W], 0);
        // 5 x 12 = 60 pixels of 7, 7 x 6 = 42 of 3.
        assert_eq!(mode[60 * GRID_W + 10], 7);
        assert_eq!(mode[61 * GRID_W + GRID_W - 1], 7);
        assert_eq!(mode[62 * GRID_W], 0);

        let mean = block_mean(&p, 0.5, 1.0, |v| v > 0.0).unwrap();
        assert!(mean[59 * GRID_W].is_nan());
        let expected = (60.0 * 7.0 + 42.0 * 3.0) / 102.0 * 0.5 + 1.0;
        assert!((f64::from(mean[60 * GRID_W]) - expected).abs() < 1e-4);
    }
}
