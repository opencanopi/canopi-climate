//! Shared steps of the site-layer stages (`regions`, `soil`): write a
//! layer's tiles and add the layer to an existing release (manifest,
//! sources, report, `ATTRIBUTION.md`, `SHA256SUMS`).

use std::path::Path;

use rayon::prelude::{IntoParallelIterator, ParallelIterator};
use serde_json::{Value, json};

use super::{PipelineError, Result, cog};
use crate::grid::TileId;
use crate::layer::{LAYER_GRID_H, LAYER_GRID_W, LAYER_TILE_LEN, LayerCell, LayerTile};
use crate::manifest::{Encoding, LayerSpec, Manifest, TileSpec};
use crate::model::NODATA;

/// The encoding of a layer with `bands` bands.
#[must_use]
pub fn encoding(bands: usize) -> Encoding {
    Encoding {
        container: "Cloud Optimized GeoTIFF (EPSG:4326, one 208 x 208 internal tile, no overviews)"
            .into(),
        sample_format: "int16".into(),
        nodata: NODATA,
        compression: "deflate".into(),
        interleave: "pixel".into(),
        bands: u32::try_from(bands).unwrap_or(u32::MAX),
        band_order: "band (1-based) = index in the bands list + 1".into(),
    }
}

/// Cut global bands (each `LAYER_GRID_W * LAYER_GRID_H`) into tiles,
/// keeping the tiles where `has_data` holds for at least one cell.
///
/// # Errors
/// Bands of the wrong length.
pub fn cut_tiles(
    bands: &[&[i16]],
    has_data: impl Fn(&LayerTile, usize) -> bool + Sync,
) -> Result<Vec<LayerTile>> {
    let n = LAYER_GRID_W * LAYER_GRID_H;
    if bands.is_empty() || bands.iter().any(|b| b.len() != n) {
        return Err(PipelineError::Data("layer band length".into()));
    }
    let nb = bands.len();
    let tiles: Vec<Option<LayerTile>> = TileId::all()
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|id| {
            let mut data = vec![NODATA; LAYER_TILE_LEN * nb];
            for j in 0..LAYER_TILE_LEN {
                let g = LayerCell::in_tile(id, j).global_index();
                for (b, band) in bands.iter().enumerate() {
                    data[j * nb + b] = band[g];
                }
            }
            let t = LayerTile::new(id, nb, data).ok()?;
            (0..LAYER_TILE_LEN).any(|j| has_data(&t, j)).then_some(t)
        })
        .collect();
    Ok(tiles.into_iter().flatten().collect())
}

/// Write tiles under `out/<dir>/` and return their manifest entries,
/// sorted by name.
///
/// # Errors
/// Writer or I/O errors.
pub fn write_tiles(
    out: &Path,
    dir: &str,
    tiles: Vec<LayerTile>,
    has_data: impl Fn(&LayerTile, usize) -> bool + Sync,
) -> Result<Vec<TileSpec>> {
    let path = out.join(dir);
    std::fs::create_dir_all(&path).map_err(|e| PipelineError::io(&path, e))?;
    let mut specs: Vec<TileSpec> = tiles
        .into_par_iter()
        .map(|t| {
            let file = path.join(t.id().file_name());
            let bytes = cog::write_layer_tile_checked(&t, &file)?;
            Ok(TileSpec {
                name: t.id().name(),
                path: format!("{dir}/{}", t.id().file_name()),
                bytes: bytes.len() as u64,
                sha256: super::sha256_bytes(&bytes),
                complete_cells: u32::try_from(t.count(&has_data)).unwrap_or(u32::MAX),
            })
        })
        .collect::<Result<_>>()?;
    specs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(specs)
}

/// Size summary of a layer's tiles, for the report.
#[must_use]
pub fn size_report(specs: &[TileSpec]) -> Value {
    let mut sizes: Vec<u64> = specs.iter().map(|t| t.bytes).collect();
    sizes.sort_unstable();
    json!({
        "tiles": specs.len(),
        "total_bytes": sizes.iter().sum::<u64>(),
        "median_tile_bytes": sizes.get(sizes.len() / 2),
        "max_tile_bytes": sizes.last(),
        "cells_with_data": specs.iter().map(|t| u64::from(t.complete_cells)).sum::<u64>(),
    })
}

/// Read the release manifest.
///
/// # Errors
/// Missing (run the `tiles` stage first) or unreadable manifest.
pub fn read_manifest(out: &Path) -> Result<Manifest> {
    let p = out.join("manifest.json");
    let text = std::fs::read_to_string(&p).map_err(|e| {
        PipelineError::Data(format!(
            "{}: {e} (site layers are added to a release: run `--stage tiles` first)",
            p.display()
        ))
    })?;
    serde_json::from_str(&text).map_err(|e| PipelineError::Data(format!("{}: {e}", p.display())))
}

/// Whether the release already has a layer.
#[must_use]
pub fn has_layer(out: &Path, id: &str) -> bool {
    read_manifest(out).is_ok_and(|m| m.layers.iter().any(|l| l.id == id))
}

/// Decode every tile of a layer of the release.
///
/// # Errors
/// Missing layer, unreadable or mismatched tiles.
pub fn read_layer(out: &Path, id: &str, bands: usize) -> Result<Vec<LayerTile>> {
    let m = read_manifest(out)?;
    let layer =
        m.layers.iter().find(|l| l.id == id).ok_or_else(|| {
            PipelineError::Data(format!("release has no {id} layer: build it first"))
        })?;
    layer
        .tiles
        .iter()
        .map(|t| {
            let p = out.join(&t.path);
            let bytes = std::fs::read(&p).map_err(|e| PipelineError::io(&p, e))?;
            Ok(LayerTile::decode(&bytes, bands)?)
        })
        .collect()
}

/// Add (or replace) a layer in the release: manifest entry, its sources,
/// its report under `layers.<id>` in `build-report.json`, then
/// `ATTRIBUTION.md` and `SHA256SUMS`.
///
/// # Errors
/// Missing release files, I/O errors.
pub fn register(out: &Path, layer: LayerSpec, report: Value) -> Result<()> {
    let mut m = read_manifest(out)?;
    for src in super::sources::manifest_sources(&layer.id) {
        if !m.sources.iter().any(|s| s.id == src.id) {
            m.sources.push(src);
        }
    }
    m.format_version = crate::manifest::FORMAT_VERSION;
    m.builder_version = env!("CARGO_PKG_VERSION").into();
    let id = layer.id.clone();
    m.layers.retain(|l| l.id != id);
    m.layers.push(layer);
    m.layers.sort_by(|a, b| a.id.cmp(&b.id));
    // Keep the climate sources and those a layer still cites (a source
    // replaced in a rebuild must leave ATTRIBUTION.md).
    let climate: Vec<String> = super::sources::manifest_sources("climate")
        .into_iter()
        .map(|s| s.id)
        .collect();
    let cited =
        |sid: &String| climate.contains(sid) || m.layers.iter().any(|l| l.sources.contains(sid));
    let kept: Vec<_> = m.sources.iter().filter(|s| cited(&s.id)).cloned().collect();
    m.sources = kept;
    let rpath = out.join("build-report.json");
    let mut r = super::build::read_json(&rpath)?;
    if !r.get("layers").is_some_and(Value::is_object) {
        r["layers"] = json!({});
    }
    r["layers"][&id] = report;
    let text = serde_json::to_string_pretty(&r).map_err(|e| PipelineError::Data(e.to_string()))?;
    std::fs::write(&rpath, text + "\n").map_err(|e| PipelineError::io(&rpath, e))?;
    super::assemble::write_release_files(out, &m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::VariableSpec;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "canopi-climate-layers-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn cut_write_register_and_read_back() {
        let out = temp("release");
        let mut m = super::super::assemble::manifest(vec![]);
        let mut stale = m.sources[0].clone();
        stale.id = "replaced-source".into();
        m.sources.push(stale);
        std::fs::write(out.join("build-report.json"), "{}").unwrap();
        super::super::assemble::write_release_files(&out, &m).unwrap();

        let n = LAYER_GRID_W * LAYER_GRID_H;
        let mut band = vec![NODATA; n];
        let paris = LayerCell::of(48.85, 2.35).unwrap();
        let fiji = LayerCell::of(-17.0, -179.99).unwrap();
        band[paris.global_index()] = 42;
        band[fiji.global_index()] = 7;
        let has = |t: &LayerTile, i: usize| t.get(i, 0) != NODATA;
        let tiles = cut_tiles(&[&band], has).unwrap();
        assert_eq!(tiles.len(), 2);
        let specs = write_tiles(&out, "test", tiles, has).unwrap();
        assert_eq!(specs[0].name, "n50e000");
        assert_eq!(specs[1].path, "test/s10w180.tif");
        assert!(specs.iter().all(|s| s.complete_cells == 1));
        let layer = LayerSpec {
            id: "soil".into(),
            description: "test".into(),
            cell_deg: 0.05,
            tile_cells: 200,
            directory: "test".into(),
            encoding: encoding(1),
            bands: vec![VariableSpec {
                id: "x".into(),
                unit: "-".into(),
                scale: 1.0,
                offset: 0.0,
                description: "x".into(),
            }],
            sources: vec!["soilgrids-2.0".into()],
            tiles: specs.clone(),
            notes: vec![],
        };
        register(&out, layer.clone(), json!({"ok": 1})).unwrap();
        register(&out, layer, json!({"ok": 2})).unwrap();
        let m = read_manifest(&out).unwrap();
        assert_eq!(m.layers.len(), 1);
        assert_eq!(m.format_version, crate::manifest::FORMAT_VERSION);
        assert!(m.sources.iter().any(|s| s.id == "soilgrids-2.0"));
        assert!(!m.sources.iter().any(|s| s.id == "replaced-source"));
        assert!(
            m.sources.iter().any(|s| s.id == "chelsa-v2.1"),
            "climate kept"
        );
        assert!(has_layer(&out, "soil") && !has_layer(&out, "regions"));
        let sums = std::fs::read_to_string(out.join("SHA256SUMS")).unwrap();
        assert!(sums.contains("test/n50e000.tif") && sums.contains("manifest.json"));
        let attribution = std::fs::read_to_string(out.join("ATTRIBUTION.md")).unwrap();
        assert!(attribution.contains("Poggio"));
        let report = super::super::build::read_json(&out.join("build-report.json")).unwrap();
        assert_eq!(report["layers"]["soil"]["ok"], 2);
        let back = read_layer(&out, "soil", 1).unwrap();
        let t = back.iter().find(|t| t.id() == paris.tile()).unwrap();
        assert_eq!(t.get(paris.index_in_tile(), 0), 42);
        assert!(read_layer(&out, "regions", 2).is_err());
    }
}
