//! Windowed GeoTIFF reader on `wbgeotiff`'s public modules.
//!
//! `wbgeotiff::GeoTiff` (9c0ff4f) reads the whole file and decodes whole
//! bands: a 43200 x 20880 CHELSA band is 3.6 GB as `f32`. This reader parses
//! the first IFD with `wbgeotiff::ifd::TiffReader` and decodes one strip or
//! one row of internal tiles at a time with
//! `wbgeotiff::compression::{decompress, undo_predictor}`, so memory stays at
//! one block of rows.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use wbgeotiff::compression::{decompress, undo_predictor};
use wbgeotiff::ifd::{ByteOrder, Ifd, TiffReader};
use wbgeotiff::tags::tag;
use wbgeotiff::{Compression, SampleFormat};

use super::{PipelineError, Result};

/// Strips or internal tiles.
#[derive(Debug, Clone)]
enum Layout {
    Strips {
        rows_per_strip: u32,
        offsets: Vec<u64>,
        counts: Vec<u64>,
    },
    Tiles {
        tile_w: u32,
        tile_h: u32,
        offsets: Vec<u64>,
        counts: Vec<u64>,
    },
}

/// Header facts of a single-band GeoTIFF.
#[derive(Debug, Clone)]
pub struct TiffInfo {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bits per sample.
    pub bits: u16,
    /// Sample format.
    pub sample_format: SampleFormat,
    /// Compression.
    pub compression: Compression,
    /// TIFF predictor (1 none, 2 horizontal, 3 floating point).
    pub predictor: u16,
    /// GDAL nodata value.
    pub nodata: Option<f64>,
    /// West edge, pixel width, north edge, pixel height (negative).
    pub geo: Option<(f64, f64, f64, f64)>,
    byte_order: ByteOrder,
    layout: Layout,
}

impl TiffInfo {
    /// Rows decoded together (strip height or internal tile height).
    #[must_use]
    pub fn block_rows(&self) -> u32 {
        match &self.layout {
            Layout::Strips { rows_per_strip, .. } => *rows_per_strip,
            Layout::Tiles { tile_h, .. } => *tile_h,
        }
    }
}

/// Row-by-row reader with a one-block cache.
pub struct TiffRows {
    /// Header facts.
    pub info: TiffInfo,
    path: PathBuf,
    reader: TiffReader<BufReader<File>>,
    cached_block: Option<u32>,
    cache: Vec<f32>,
}

fn tag_u64(ifd: &Ifd, t: u16) -> Option<u64> {
    ifd.get(t).and_then(|e| e.value.as_u64())
}

fn tag_u64_vec(ifd: &Ifd, t: u16, name: &str) -> Result<Vec<u64>> {
    ifd.get(t)
        .and_then(|e| e.value.as_u64_vec())
        .ok_or_else(|| PipelineError::Raster(format!("missing {name}")))
}

fn to_u32(v: u64, name: &str) -> Result<u32> {
    u32::try_from(v).map_err(|_| PipelineError::Raster(format!("{name} too large: {v}")))
}

fn read_layout(ifd: &Ifd, height: u32) -> Result<Layout> {
    if let (Some(tw), Some(th)) = (tag_u64(ifd, tag::TileWidth), tag_u64(ifd, tag::TileLength)) {
        return Ok(Layout::Tiles {
            tile_w: to_u32(tw, "tile width")?,
            tile_h: to_u32(th, "tile height")?,
            offsets: tag_u64_vec(ifd, tag::TileOffsets, "TileOffsets")?,
            counts: tag_u64_vec(ifd, tag::TileByteCounts, "TileByteCounts")?,
        });
    }
    let rps = tag_u64(ifd, tag::RowsPerStrip)
        .unwrap_or(u64::from(height))
        .min(u64::from(height));
    Ok(Layout::Strips {
        rows_per_strip: to_u32(rps.max(1), "rows per strip")?,
        offsets: tag_u64_vec(ifd, tag::StripOffsets, "StripOffsets")?,
        counts: tag_u64_vec(ifd, tag::StripByteCounts, "StripByteCounts")?,
    })
}

fn read_geo(ifd: &Ifd) -> Option<(f64, f64, f64, f64)> {
    let s = ifd.get(tag::ModelPixelScaleTag)?.value.as_f64_vec()?;
    let t = ifd.get(tag::ModelTiepointTag)?.value.as_f64_vec()?;
    if s.len() < 2 || t.len() < 6 {
        return None;
    }
    Some((t[3] - t[0] * s[0], s[0], t[4] + t[1] * s[1], -s[1]))
}

impl TiffRows {
    /// Open a single-band GeoTIFF and read its header.
    ///
    /// # Errors
    /// I/O or format errors, or a multi-band / planar file.
    pub fn open(path: &Path) -> Result<Self> {
        let f = File::open(path).map_err(|e| PipelineError::io(path, e))?;
        let mut reader = TiffReader::new(BufReader::with_capacity(1 << 20, f))?;
        let ifd = reader.read_ifd(reader.first_ifd_offset)?;
        let missing = |n: &str| PipelineError::Raster(format!("{}: no {n}", path.display()));
        let width = to_u32(
            tag_u64(&ifd, tag::ImageWidth).ok_or_else(|| missing("width"))?,
            "width",
        )?;
        let height = to_u32(
            tag_u64(&ifd, tag::ImageLength).ok_or_else(|| missing("height"))?,
            "height",
        )?;
        let spp = tag_u64(&ifd, tag::SamplesPerPixel).unwrap_or(1);
        if spp != 1 {
            return Err(PipelineError::Raster(format!(
                "{}: {spp} samples per pixel; only single-band rasters are read",
                path.display()
            )));
        }
        let small =
            |v: Option<u64>, default: u16| v.and_then(|x| u16::try_from(x).ok()).unwrap_or(default);
        let info = TiffInfo {
            width,
            height,
            bits: small(tag_u64(&ifd, tag::BitsPerSample), 8),
            sample_format: SampleFormat::from_tag(small(tag_u64(&ifd, tag::SampleFormat), 1)),
            compression: Compression::from_tag(small(tag_u64(&ifd, tag::Compression), 1)),
            predictor: small(tag_u64(&ifd, tag::Predictor), 1),
            nodata: ifd
                .get(tag::GdalNodata)
                .and_then(|e| e.value.as_str())
                .and_then(|s| s.trim_matches(char::from(0)).trim().parse::<f64>().ok()),
            geo: read_geo(&ifd),
            byte_order: reader.byte_order,
            layout: read_layout(&ifd, height)?,
        };
        Ok(Self {
            info,
            path: path.to_path_buf(),
            reader,
            cached_block: None,
            cache: Vec::new(),
        })
    }

    fn bytes_per_sample(&self) -> usize {
        usize::from(self.info.bits).div_ceil(8)
    }

    /// Append decoded samples as `f32`.
    fn push_f32(&self, raw: &[u8], out: &mut Vec<f32>) -> Result<()> {
        let le = matches!(self.info.byte_order, ByteOrder::LittleEndian);
        macro_rules! conv {
            ($t:ty, $n:expr) => {
                out.extend(raw.chunks_exact($n).map(|c| {
                    let mut a = [0u8; $n];
                    a.copy_from_slice(c);
                    (if le {
                        <$t>::from_le_bytes(a)
                    } else {
                        <$t>::from_be_bytes(a)
                    }) as f32
                }))
            };
        }
        match (self.info.sample_format, self.info.bits) {
            (SampleFormat::Uint, 8) => out.extend(raw.iter().map(|&b| f32::from(b))),
            (SampleFormat::Int, 8) => out.extend(raw.iter().map(|&b| f32::from(b.cast_signed()))),
            (SampleFormat::Uint, 16) => conv!(u16, 2),
            (SampleFormat::Int, 16) => conv!(i16, 2),
            (SampleFormat::Uint, 32) => conv!(u32, 4),
            (SampleFormat::Int, 32) => conv!(i32, 4),
            (SampleFormat::IeeeFloat, 32) => conv!(f32, 4),
            (SampleFormat::IeeeFloat, 64) => conv!(f64, 8),
            (f, b) => {
                return Err(PipelineError::Raster(format!(
                    "{}: unsupported sample type {f:?}/{b}",
                    self.path.display()
                )));
            }
        }
        Ok(())
    }

    fn read_block_bytes(
        &mut self,
        offset: u64,
        count: u64,
        cols: usize,
        rows: usize,
    ) -> Result<Vec<u8>> {
        let bps = self.bytes_per_sample();
        let count = usize::try_from(count).map_err(|e| PipelineError::Raster(e.to_string()))?;
        let compressed = self.reader.read_bytes_at(offset, count)?;
        let mut raw = decompress(self.info.compression, &compressed, rows * cols * bps)?;
        undo_predictor(
            &mut raw,
            self.info.predictor,
            cols,
            rows,
            1,
            bps,
            self.info.byte_order,
        )?;
        if raw.len() < rows * cols * bps {
            return Err(PipelineError::Raster(format!(
                "{}: short block at offset {offset}",
                self.path.display()
            )));
        }
        raw.truncate(rows * cols * bps);
        Ok(raw)
    }

    /// Decode block `bi` (a strip, or a row of tiles) into `rows x width`.
    fn decode_block(&mut self, bi: u32) -> Result<Vec<f32>> {
        let w = self.info.width as usize;
        let br = self.info.block_rows() as usize;
        let rows_here = br.min(self.info.height as usize - bi as usize * br);
        let mut out: Vec<f32> = Vec::with_capacity(rows_here * w);
        let layout = self.info.layout.clone();
        match layout {
            Layout::Strips {
                offsets, counts, ..
            } => {
                let i = bi as usize;
                let (Some(&o), Some(&c)) = (offsets.get(i), counts.get(i)) else {
                    return Err(PipelineError::Raster(format!("strip {i} missing")));
                };
                let raw = self.read_block_bytes(o, c, w, rows_here)?;
                self.push_f32(&raw, &mut out)?;
            }
            Layout::Tiles {
                tile_w,
                tile_h,
                offsets,
                counts,
            } => {
                let (tw, th) = (tile_w as usize, tile_h as usize);
                let tiles_x = w.div_ceil(tw);
                out.resize(rows_here * w, f32::NAN);
                let mut tile = Vec::with_capacity(tw * th);
                for tx in 0..tiles_x {
                    let i = bi as usize * tiles_x + tx;
                    let (Some(&o), Some(&c)) = (offsets.get(i), counts.get(i)) else {
                        return Err(PipelineError::Raster(format!("tile {i} missing")));
                    };
                    if c == 0 {
                        continue;
                    }
                    let raw = self.read_block_bytes(o, c, tw, th)?;
                    tile.clear();
                    self.push_f32(&raw, &mut tile)?;
                    let x0 = tx * tw;
                    let cw = tw.min(w - x0);
                    for r in 0..rows_here {
                        out[r * w + x0..r * w + x0 + cw]
                            .copy_from_slice(&tile[r * tw..r * tw + cw]);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Copy row `row` (full width) into `dst`. Sequential reads decode each
    /// block once.
    ///
    /// # Errors
    /// Row out of range, wrong `dst` length, or decoding errors.
    pub fn read_row(&mut self, row: u32, dst: &mut [f32]) -> Result<()> {
        let w = self.info.width as usize;
        if row >= self.info.height || dst.len() != w {
            return Err(PipelineError::Raster(format!(
                "{}: row {row} / buffer {} out of range",
                self.path.display(),
                dst.len()
            )));
        }
        let br = self.info.block_rows();
        let bi = row / br;
        if self.cached_block != Some(bi) {
            self.cache = self.decode_block(bi)?;
            self.cached_block = Some(bi);
        }
        let r = (row - bi * br) as usize;
        let src = self
            .cache
            .get(r * w..(r + 1) * w)
            .ok_or_else(|| PipelineError::Raster("short block".into()))?;
        dst.copy_from_slice(src);
        Ok(())
    }

    /// Value of the pixel holding a point (decodes its block).
    ///
    /// # Errors
    /// No georeferencing, point outside the raster, or decoding errors.
    pub fn sample(&mut self, lat: f64, lon: f64) -> Result<f32> {
        let (x0, pw, y0, ph) = self
            .info
            .geo
            .ok_or_else(|| PipelineError::Raster("no georeferencing".into()))?;
        if !lat.is_finite() || !lon.is_finite() {
            return Err(PipelineError::Raster("non-finite coordinate".into()));
        }
        let col = ((lon - x0) / pw).floor();
        let row = ((lat - y0) / ph).floor();
        if !col.is_finite()
            || !row.is_finite()
            || col < 0.0
            || row < 0.0
            || col >= f64::from(self.info.width)
            || row >= f64::from(self.info.height)
        {
            return Err(PipelineError::Raster("point outside the raster".into()));
        }
        let mut buf = vec![0f32; self.info.width as usize];
        self.read_row(row as u32, &mut buf)?;
        Ok(buf[col as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wbgeotiff::{GeoTiffWriter, GeoTransform, WriteLayout};

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("canopi-climate-rows-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn pattern(w: usize, h: usize) -> Vec<u16> {
        (0..w * h).map(|i| ((i * 7) % 65_000) as u16).collect()
    }

    #[test]
    fn reads_rows_of_stripped_and_tiled_files() {
        let (w, h) = (300usize, 70usize);
        let data = pattern(w, h);
        for (name, layout) in [
            ("strips.tif", WriteLayout::Stripped { rows_per_strip: 16 }),
            (
                "tiles.tif",
                WriteLayout::Tiled {
                    tile_width: 64,
                    tile_height: 32,
                },
            ),
        ] {
            let path = temp(name);
            GeoTiffWriter::new(w as u32, h as u32, 1)
                .layout(layout)
                .compression(Compression::Deflate)
                .geo_transform(GeoTransform::north_up(-180.0, 0.5, 90.0, -0.5))
                .no_data(65535.0)
                .write_u16(&path, &data)
                .unwrap();
            let mut r = TiffRows::open(&path).unwrap();
            assert_eq!((r.info.width, r.info.height), (w as u32, h as u32));
            assert_eq!(r.info.nodata, Some(65535.0));
            let mut row = vec![0f32; w];
            for y in [0usize, 15, 16, 33, 69, 2] {
                r.read_row(y as u32, &mut row).unwrap();
                for x in 0..w {
                    assert_eq!(row[x], f32::from(data[y * w + x]), "{name} {x},{y}");
                }
            }
            assert!(r.read_row(70, &mut row).is_err());
            assert!(r.read_row(0, &mut row[..10]).is_err());
            // (lat 89.9, lon -179.9) is pixel (0, 0); (lat 55.1, lon -30.1) is (299, 69).
            assert_eq!(r.sample(89.9, -179.9).unwrap(), f32::from(data[0]));
            assert_eq!(
                r.sample(55.1, -30.1).unwrap(),
                f32::from(data[69 * w + 299])
            );
            assert!(r.sample(0.0, 0.0).is_err());
            assert!(r.sample(f64::NAN, 0.0).is_err());
            assert!(r.sample(89.9, f64::NAN).is_err());
            assert!(r.sample(f64::INFINITY, -179.9).is_err());
        }
    }
}
