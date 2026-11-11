//! Measures candidate formats for the regions layer on a sample of land
//! tiles (every 4th tile with a code, by name), for
//! `docs/decisions/0003-site-layer-formats.md`:
//!
//! - a raster of codes (2 `int16` bands, Deflate COG as written by the
//!   pipeline) at 0.05, 0.025 and 0.01 degrees;
//! - per-tile polygons clipped to the tile and simplified
//!   (Douglas-Peucker, 0.01 degree), as GeoJSON with 3 decimals, raw and
//!   Deflate-compressed (what a gzip/brotli static host sends, roughly).
//!
//! ```text
//! cargo run --release --features pipeline --example regions_formats -- <sources-dir> <scratch-dir>
//! ```

use std::error::Error;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

use canopi_climate::grid::{TILE_DEG, TileId};
use canopi_climate::model::NODATA;
use canopi_climate::pipeline::regions::{Areas, load_natural_earth, load_wgsrpd, rasterize_tile};
use geo::{BooleanOps, Coord, MultiPolygon, Rect, Simplify};
use rayon::prelude::{IntoParallelRefIterator, ParallelIterator};
use wbgeotiff::{CogWriter, Compression, GeoTransform};

type Res<T> = Result<T, Box<dyn Error>>;
/// Errors crossing rayon threads.
type Par<T> = Result<T, String>;

fn deflated(bytes: &[u8]) -> Par<usize> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    e.write_all(bytes).map_err(|e| e.to_string())?;
    Ok(e.finish().map_err(|e| e.to_string())?.len())
}

/// Bytes of a 2-band code raster of `cells` x `cells` for a tile.
fn raster_bytes(
    l3: &Areas,
    ne: &Areas,
    id: TileId,
    cells: usize,
    scratch: &Path,
) -> Par<(usize, bool)> {
    let a = rasterize_tile(l3, id, cells);
    let b = rasterize_tile(ne, id, cells);
    let any = a.codes.iter().chain(&b.codes).any(|&v| v != NODATA);
    let data: Vec<i16> = a
        .codes
        .iter()
        .zip(&b.codes)
        .flat_map(|(x, y)| [*x, *y])
        .collect();
    let side = u32::try_from(cells).map_err(|e| e.to_string())?;
    let deg = TILE_DEG / cells as f64;
    let path = scratch.join(format!("{}-{cells}.tif", id.name()));
    CogWriter::new(side, side, 2)
        .compression(Compression::Deflate)
        .tile_size(side.div_ceil(16) * 16)
        .no_data(f64::from(NODATA))
        .overview_levels(vec![])
        .geo_transform(GeoTransform::north_up(id.west(), deg, id.north(), -deg))
        .epsg(4326)
        .write_i16(&path, &data)
        .map_err(|e| e.to_string())?;
    let n = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
    std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    Ok((usize::try_from(n).map_err(|e| e.to_string())?, any))
}

/// GeoJSON bytes (raw, deflated) and vertex count of both sources clipped
/// to a tile and simplified.
fn polygon_bytes(sources: &[(&Areas, &str, u32)], id: TileId) -> Par<(usize, usize, usize)> {
    let rect = Rect::new(
        Coord {
            x: id.west(),
            y: id.north() - TILE_DEG,
        },
        Coord {
            x: id.west() + TILE_DEG,
            y: id.north(),
        },
    )
    .to_polygon();
    let mut features = Vec::new();
    let mut vertices = 0;
    for (areas, key, letters) in sources {
        for part in &areas.parts {
            if !geo::Intersects::intersects(&part.bbox, &rect) {
                continue;
            }
            let clipped: MultiPolygon<f64> = part.polygon.intersection(&rect);
            let simple = clipped.simplify(0.01);
            if simple.0.is_empty() {
                continue;
            }
            vertices += geo::CoordsIter::coords_count(&simple);
            let rounded = geo::MapCoords::map_coords(&simple, |c| Coord {
                x: (c.x * 1000.0).round() / 1000.0,
                y: (c.y * 1000.0).round() / 1000.0,
            });
            let code =
                canopi_climate::regions::decode_code(part.code, *letters).unwrap_or_default();
            let mut f = geojson::Feature::from(geojson::Geometry::from(&rounded));
            f.set_property(*key, code);
            features.push(f);
        }
    }
    let fc = geojson::FeatureCollection::from_iter(features);
    let text = fc.to_string();
    Ok((text.len(), deflated(text.as_bytes())?, vertices))
}

fn main() -> Res<()> {
    let args: Vec<String> = std::env::args().collect();
    let (Some(src), Some(scratch)) = (args.get(1), args.get(2)) else {
        return Err("usage: regions_formats <sources-dir> <scratch-dir>".into());
    };
    let (src, scratch) = (Path::new(src), Path::new(scratch));
    std::fs::create_dir_all(scratch)?;
    let l3 = load_wgsrpd(&src.join("wgsrpd/level3.geojson"))?;
    let ne = load_natural_earth(&src.join("naturalearth/ne_50m_admin_0_countries.geojson"))?;
    // Land tiles: those with a code at 0.05 degrees.
    let all: Vec<TileId> = TileId::all().collect();
    let flags = all
        .par_iter()
        .map(|&id| Ok((id, raster_bytes(&l3, &ne, id, 200, scratch)?.1)))
        .collect::<Result<Vec<_>, String>>()?;
    let mut land: Vec<TileId> = flags.into_iter().filter(|f| f.1).map(|f| f.0).collect();
    land.sort_by_key(|t| t.name());
    let sample: Vec<TileId> = land.iter().copied().step_by(4).collect();
    println!("land tiles {}, sample {}", land.len(), sample.len());
    for cells in [200, 400, 1000] {
        let t0 = Instant::now();
        let mut sizes = sample
            .par_iter()
            .map(|&id| Ok(raster_bytes(&l3, &ne, id, cells, scratch)?.0))
            .collect::<Result<Vec<_>, String>>()?;
        report(
            &format!("raster {} deg", TILE_DEG / cells as f64),
            &mut sizes,
            land.len(),
            t0,
        );
    }
    let t0 = Instant::now();
    let measured = sample
        .par_iter()
        .map(|&id| polygon_bytes(&[(&l3, "wgsrpd_l3", 3), (&ne, "iso_a2", 2)], id))
        .collect::<Result<Vec<_>, String>>()?;
    let mut raw: Vec<usize> = measured.iter().map(|m| m.0).collect();
    let mut gz: Vec<usize> = measured.iter().map(|m| m.1).collect();
    let verts: usize = measured.iter().map(|m| m.2).sum();
    report("polygons geojson raw", &mut raw, land.len(), t0);
    report("polygons geojson deflated", &mut gz, land.len(), t0);
    println!("polygon vertices in sample: {verts}");
    Ok(())
}

fn report(what: &str, sizes: &mut [usize], land: usize, t0: Instant) {
    sizes.sort_unstable();
    let total: usize = sizes.iter().sum();
    let mean = total as f64 / sizes.len().max(1) as f64;
    println!(
        "{what}: sample total {total} B, median {} B, max {} B, estimated all {land} tiles {:.0} KB, {:.1} s",
        sizes.get(sizes.len() / 2).copied().unwrap_or(0),
        sizes.last().copied().unwrap_or(0),
        mean * land as f64 / 1000.0,
        t0.elapsed().as_secs_f64()
    );
}
