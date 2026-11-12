//! A decoded tile: 100 x 100 cells x [`BANDS`] `i16` values.
//!
//! On disk a tile is a Cloud Optimized GeoTIFF (EPSG:4326, `int16`,
//! pixel-interleaved, Deflate, nodata -32768) written by `wbgeotiff`
//! (`pipeline` feature) and read here with the `tiff` crate, which keeps the
//! WebAssembly reader small (docs/decisions/0001-tile-format.md).

use std::io::Cursor;

use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;

use crate::grid::{CELL_DEG, TILE_CELLS, TILE_LEN, TileId};
use crate::model::{BANDS, NODATA, SLOTS, Variable};

/// Why a tile could not be decoded.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TileError {
    /// The bytes are not a readable TIFF.
    #[error("not a readable TIFF: {0}")]
    Tiff(String),
    /// The raster does not have the grid's shape or sample type.
    #[error("unexpected raster layout: {0}")]
    Layout(String),
    /// The georeferencing does not match a tile of the grid.
    #[error("georeferencing does not match a grid tile: {0}")]
    Georef(String),
}

impl From<tiff::TiffError> for TileError {
    fn from(e: tiff::TiffError) -> Self {
        Self::Tiff(e.to_string())
    }
}

/// A decoded tile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tile {
    id: TileId,
    /// Pixel-interleaved: `data[cell * BANDS + band]`.
    data: Vec<i16>,
}

impl Tile {
    /// A tile from pixel-interleaved values (`TILE_LEN * BANDS` of them).
    ///
    /// # Errors
    /// [`TileError::Layout`] if the length is wrong.
    pub fn new(id: TileId, data: Vec<i16>) -> Result<Self, TileError> {
        if data.len() != TILE_LEN * BANDS {
            return Err(TileError::Layout(format!(
                "{} values, expected {}",
                data.len(),
                TILE_LEN * BANDS
            )));
        }
        Ok(Self { id, data })
    }

    /// The tile's position in the grid.
    #[must_use]
    pub fn id(&self) -> TileId {
        self.id
    }

    /// All bands of a cell (row-major index inside the tile).
    #[must_use]
    pub fn pixel(&self, index: usize) -> &[i16] {
        self.data
            .get(index * BANDS..(index + 1) * BANDS)
            .unwrap_or(&[])
    }

    /// One stored value, [`NODATA`] outside the tile.
    #[must_use]
    pub fn get(&self, index: usize, band: usize) -> i16 {
        if band >= BANDS {
            return NODATA;
        }
        self.data
            .get(index * BANDS + band)
            .copied()
            .unwrap_or(NODATA)
    }

    /// Whether a cell has every variable for every slot.
    #[must_use]
    pub fn is_complete(&self, index: usize) -> bool {
        let p = self.pixel(index);
        let aridity = Variable::Aridity.index();
        !p.is_empty()
            && p.iter()
                .enumerate()
                .all(|(b, &v)| v != NODATA || b / SLOTS.len() == aridity)
    }

    /// Number of complete cells.
    #[must_use]
    pub fn complete_cells(&self) -> usize {
        (0..TILE_LEN).filter(|&i| self.is_complete(i)).count()
    }

    /// The pixel-interleaved values.
    #[must_use]
    pub fn data(&self) -> &[i16] {
        &self.data
    }

    /// Decode a tile file (the whole file, as fetched).
    ///
    /// # Errors
    /// [`TileError`] when the bytes are not a grid tile.
    pub fn decode(bytes: &[u8]) -> Result<Self, TileError> {
        let (id, data) = decode_raster(bytes, TILE_CELLS, CELL_DEG, BANDS)?;
        Ok(Self { id, data })
    }
}

/// Decode a tile-shaped `int16` GeoTIFF: `side` x `side` cells of
/// `cell_deg` degrees, `bands` pixel-interleaved samples per cell, origin
/// on a tile corner. Shared by the climate tiles and the site layers.
///
/// # Errors
/// [`TileError`] when the bytes are not such a raster.
pub(crate) fn decode_raster(
    bytes: &[u8],
    side: usize,
    cell_deg: f64,
    bands: usize,
) -> Result<(TileId, Vec<i16>), TileError> {
    match bytes.get(..4) {
        Some(b"MM\0*" | b"MM\0+") => {
            return Err(TileError::Layout("not little-endian TIFF".into()));
        }
        Some(b"II+\0") => {
            return Err(TileError::Layout(
                "not a classic TIFF (BigTIFF is not read)".into(),
            ));
        }
        _ => {}
    }
    let mut d = Decoder::new(Cursor::new(bytes))?;
    let (w, h) = d.dimensions()?;
    if (w as usize, h as usize) != (side, side) {
        return Err(TileError::Layout(format!(
            "{w}x{h}, expected {side}x{side}"
        )));
    }
    check_layout(&mut d, side, bands)?;
    let id = georef_tile(&mut d, cell_deg)?;
    let data = match d.read_image()? {
        DecodingResult::I16(v) => v,
        _ => return Err(TileError::Layout("samples are not int16".into())),
    };
    let cells = side * side;
    if data.len() != cells * bands {
        return Err(TileError::Layout(format!(
            "{} samples per cell, expected {bands} bands",
            data.len() as f64 / cells as f64
        )));
    }
    Ok((id, data))
}

/// The header rules of the website's reader (`tiff.ts` `readTileHeader`),
/// beyond what the `tiff` crate already refuses: one image, `bands` signed
/// 16-bit samples per cell, pixel-interleaved, Deflate without predictor,
/// one internal tile covering the image (docs/format.md, Container).
fn check_layout<R: std::io::Read + std::io::Seek>(
    d: &mut Decoder<R>,
    side: usize,
    bands: usize,
) -> Result<(), TileError> {
    let layout = |m: String| Err(TileError::Layout(m));
    if d.more_images() {
        return layout("more than one image (overviews are not read)".into());
    }
    let short = |d: &mut Decoder<R>, tag: Tag| -> Result<Option<u64>, TileError> {
        Ok(d.find_tag_unsigned::<u64>(tag)?)
    };
    let samples = short(d, Tag::SamplesPerPixel)?.unwrap_or(1);
    if samples != bands as u64 {
        return layout(format!("{samples} bands, expected {bands}"));
    }
    let bits = d.find_tag_unsigned_vec::<u64>(Tag::BitsPerSample)?;
    if bits.is_none_or(|b| b.iter().any(|&x| x != 16)) {
        return layout("samples are not 16-bit".into());
    }
    if short(d, Tag::SampleFormat)? != Some(2) {
        return layout("samples are not signed integers".into());
    }
    let compression = short(d, Tag::Compression)?;
    if !matches!(compression, Some(8 | 32946)) {
        return layout(format!("compression {compression:?}, expected Deflate"));
    }
    if short(d, Tag::Predictor)?.unwrap_or(1) != 1 {
        return layout("a predictor is set".into());
    }
    if short(d, Tag::PlanarConfiguration)?.unwrap_or(1) != 1 {
        return layout("not pixel-interleaved".into());
    }
    let tw = short(d, Tag::TileWidth)?.unwrap_or(0);
    let tl = short(d, Tag::TileLength)?.unwrap_or(0);
    let offsets = d
        .find_tag_unsigned_vec::<u64>(Tag::TileOffsets)?
        .map_or(0, |v| v.len());
    let counts = d
        .find_tag_unsigned_vec::<u64>(Tag::TileByteCounts)?
        .map_or(0, |v| v.len());
    let side = side as u64;
    if tw < side || tl < side || offsets != 1 || counts != 1 {
        return layout("not one internal tile covering the image".into());
    }
    Ok(())
}

/// The tile named by the GeoTIFF pixel scale and tie point.
fn georef_tile<R: std::io::Read + std::io::Seek>(
    d: &mut Decoder<R>,
    cell_deg: f64,
) -> Result<TileId, TileError> {
    let scale = d
        .find_tag(Tag::ModelPixelScaleTag)?
        .ok_or_else(|| TileError::Georef("no ModelPixelScale".into()))?
        .into_f64_vec()?;
    let tie = d
        .find_tag(Tag::ModelTiepointTag)?
        .ok_or_else(|| TileError::Georef("no ModelTiepoint".into()))?
        .into_f64_vec()?;
    let (Some(&sx), Some(&sy)) = (scale.first(), scale.get(1)) else {
        return Err(TileError::Georef("short ModelPixelScale".into()));
    };
    if (sx - cell_deg).abs() > 1e-9 || (sy - cell_deg).abs() > 1e-9 {
        return Err(TileError::Georef(format!("pixel size {sx} x {sy}")));
    }
    let [i, j, _, x, y, ..] = tie.as_slice() else {
        return Err(TileError::Georef("short ModelTiepoint".into()));
    };
    let west = x - i * cell_deg;
    let north = y + j * cell_deg;
    let id = TileId::of(north - cell_deg / 2.0, west + cell_deg / 2.0)
        .ok_or_else(|| TileError::Georef(format!("origin {west}, {north}")))?;
    if (id.west() - west).abs() > 1e-6 || (id.north() - north).abs() > 1e-6 {
        return Err(TileError::Georef(format!(
            "origin {west}, {north} is not a tile corner"
        )));
    }
    Ok(id)
}

#[cfg(test)]
pub(crate) mod synthetic {
    //! A deterministic synthetic tile for tests (also the committed fixture).

    use super::{BANDS, NODATA, TILE_CELLS, TILE_LEN, Tile, TileId};
    use crate::model::{SLOTS, VARIABLES, Variable, band};

    /// Land: a block in the middle plus the four corner cells and an
    /// island; everything else is ocean. Cell (5, 5) has one missing value.
    pub(crate) fn is_land(x: usize, y: usize) -> bool {
        let corner = (x == 0 || x == TILE_CELLS - 1) && (y == 0 || y == TILE_CELLS - 1);
        let block = (20..80).contains(&x) && (30..70).contains(&y);
        let island = x == 5 && y == 5;
        corner || block || island
    }

    pub(crate) fn value(var: Variable, slot: usize, x: usize, y: usize) -> f32 {
        let (xf, yf, s) = (x as f32, y as f32, slot as f32);
        match var {
            Variable::Koppen => (1 + (x / 4 + y / 4) % 30) as f32,
            Variable::TminCold => -20.0 + 0.3 * yf + 0.5 * s,
            Variable::TmaxWarm => 15.0 + 0.2 * xf + 0.6 * s,
            Variable::TmeanCold => -10.0 + 0.3 * yf + 0.5 * s,
            Variable::TmeanWarm => 12.0 + 0.2 * xf + 0.5 * s,
            Variable::Precip => 200.0 + 10.0 * xf - 5.0 * s,
            Variable::Aridity => 0.1 + 0.01 * (xf + yf) - 0.01 * s,
            Variable::DryMonths => ((x + y + slot) % 13) as f32,
        }
    }

    pub(crate) fn tile(id: TileId) -> Tile {
        let mut data = vec![NODATA; TILE_LEN * BANDS];
        for y in 0..TILE_CELLS {
            for x in 0..TILE_CELLS {
                if !is_land(x, y) {
                    continue;
                }
                let i = y * TILE_CELLS + x;
                for (vi, var) in VARIABLES.iter().enumerate() {
                    for si in 0..SLOTS.len() {
                        data[i * BANDS + band(vi, si)] = var.encode(value(*var, si, x, y));
                    }
                }
            }
        }
        // The island misses its 2071-2099 SSP5-8.5 precipitation.
        data[(5 * TILE_CELLS + 5) * BANDS + band(5, 8)] = NODATA;
        Tile { id, data }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/synthetic-n50e000.tif");

    #[test]
    fn rejects_wrong_lengths() {
        let id = TileId { col: 18, row: 4 };
        assert!(Tile::new(id, vec![0; 10]).is_err());
        assert!(Tile::new(id, vec![0; TILE_LEN * BANDS]).is_ok());
    }

    #[test]
    fn decodes_the_committed_fixture() {
        let t = Tile::decode(FIXTURE).unwrap();
        let id = TileId::parse("n50e000").unwrap();
        assert_eq!(t, synthetic::tile(id));
        assert_eq!(t.id(), id);
        assert!(t.is_complete(50 * TILE_CELLS + 50));
        assert!(!t.is_complete(5 * TILE_CELLS + 5));
        assert!(!t.is_complete(1));
        assert_eq!(t.get(0, BANDS), NODATA);
        assert_eq!(t.get(TILE_LEN, 0), NODATA);
        assert!(t.pixel(TILE_LEN).is_empty());
    }

    /// The fixture with IFD entry `tag` rewritten to `new_tag` holding the
    /// inline SHORT `value` (the fixture's IFD is sorted, so a rename keeps
    /// it sorted only for neighbouring tag numbers; the decoder does not
    /// care either way).
    fn patched(tag: u16, new_tag: u16, value: u16) -> Vec<u8> {
        let mut b = FIXTURE.to_vec();
        let u16_at = |b: &[u8], at: usize| u16::from_le_bytes([b[at], b[at + 1]]);
        let ifd = u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize;
        let n = usize::from(u16_at(&b, ifd));
        let entry = (0..n)
            .map(|i| ifd + 2 + 12 * i)
            .find(|&e| u16_at(&b, e) == tag)
            .unwrap();
        b[entry..entry + 2].copy_from_slice(&new_tag.to_le_bytes());
        b[entry + 2..entry + 4].copy_from_slice(&3u16.to_le_bytes());
        b[entry + 4..entry + 8].copy_from_slice(&1u32.to_le_bytes());
        b[entry + 8..entry + 12].copy_from_slice(&u32::from(value).to_le_bytes());
        b
    }

    fn layout_error(bytes: &[u8], words: &str) {
        match Tile::decode(bytes) {
            Err(TileError::Layout(m)) => assert!(m.contains(words), "{m}"),
            other => panic!("expected a layout error naming {words:?}, got {other:?}"),
        }
    }

    /// The website's reader (`src/lib/climate/tiff.ts` `readTileHeader`)
    /// refuses these layouts; the crate refuses them too, so the writer can
    /// never ship a tile the site cannot read (docs/format.md, Container).
    #[test]
    fn refuses_what_the_website_reader_refuses() {
        let mut big_endian = FIXTURE.to_vec();
        big_endian[..4].copy_from_slice(b"MM\0*");
        layout_error(&big_endian, "little-endian");
        let mut big_tiff = FIXTURE.to_vec();
        big_tiff[2] = 43;
        big_tiff[4..8].copy_from_slice(&[8, 0, 0, 0]);
        layout_error(&big_tiff, "classic TIFF");
        // PlanarConfiguration (284) renamed to Predictor (317) = 2: planar
        // falls back to 1 (chunky), the predictor is set.
        layout_error(&patched(284, 317, 2), "predictor");
        layout_error(&patched(284, 317, 3), "predictor");
        assert!(
            Tile::decode(&patched(284, 317, 1)).is_ok(),
            "predictor 1 is none"
        );
        // Planar layout: the tiff crate already refuses it with this
        // fixture's single tile offset; the check backs it up.
        assert!(Tile::decode(&patched(284, 284, 2)).is_err());
        layout_error(&patched(339, 339, 1), "signed");
        layout_error(&patched(259, 259, 1), "Deflate");
        assert!(
            Tile::decode(&patched(259, 259, 32946)).is_ok(),
            "old Deflate code"
        );
        // A second IFD (overviews): a copy of the first appended to the file.
        let mut two = FIXTURE.to_vec();
        let ifd = u32::from_le_bytes([two[4], two[5], two[6], two[7]]) as usize;
        let n = usize::from(u16::from_le_bytes([two[ifd], two[ifd + 1]]));
        let next = ifd + 2 + 12 * n;
        two.resize(two.len().next_multiple_of(2), 0);
        let copy = two.len();
        two.extend_from_within(ifd..next + 4);
        two[next..next + 4].copy_from_slice(&(copy as u32).to_le_bytes());
        layout_error(&two, "more than one image");
        // Internal tile smaller than the image: refused, whichever layer
        // (the tiff crate or the check) sees it first.
        assert!(Tile::decode(&patched(322, 322, 56)).is_err());
    }

    #[test]
    fn rejects_garbage_and_truncated_files() {
        assert!(matches!(
            Tile::decode(b"not a tiff"),
            Err(TileError::Tiff(_))
        ));
        assert!(Tile::decode(&FIXTURE[..FIXTURE.len() / 2]).is_err());
        assert!(Tile::decode(&[]).is_err());
    }
}
