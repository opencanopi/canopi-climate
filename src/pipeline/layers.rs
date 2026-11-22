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
    m.format_version = crate::manifest::FORMAT_VERSION;
    m.builder_version = env!("CARGO_PKG_VERSION").into();
    let id = layer.id.clone();
    m.layers.retain(|l| l.id != id);
    m.layers.push(layer);
    m.layers.sort_by(|a, b| a.id.cmp(&b.id));
    sync_sources(&mut m);
    let rpath = out.join("build-report.json");
    let mut r = super::build::read_json(&rpath)?;
    if !r.get("layers").is_some_and(Value::is_object) {
        r["layers"] = json!({});
    }
    r["layers"][&id] = report;
    write_report(&rpath, &r)?;
    super::assemble::write_release_files(out, &m)
}

/// Sources of the manifest: the climate sources, plus each layer's (from
/// the source table); a source no layer cites any more (replaced in a
/// rebuild) leaves the manifest and `ATTRIBUTION.md`.
fn sync_sources(m: &mut Manifest) {
    for l in &m.layers {
        for src in super::sources::manifest_sources(&l.id) {
            if !m.sources.iter().any(|s| s.id == src.id) {
                m.sources.push(src);
            }
        }
    }
    let climate: Vec<String> = super::sources::manifest_sources("climate")
        .into_iter()
        .map(|s| s.id)
        .collect();
    let layers = &m.layers;
    m.sources
        .retain(|s| climate.contains(&s.id) || layers.iter().any(|l| l.sources.contains(&s.id)));
}

fn write_report(path: &Path, r: &Value) -> Result<()> {
    let text = serde_json::to_string_pretty(r).map_err(|e| PipelineError::Data(e.to_string()))?;
    std::fs::write(path, text + "\n").map_err(|e| PipelineError::io(path, e))
}

/// Before the `tiles` stage rewrites a release: copy the site layers of
/// the existing manifest (and their sources) into `m`, and their part of
/// the existing report into `report`, so that re-running `tiles` keeps
/// them in `manifest.json`, `ATTRIBUTION.md` and `SHA256SUMS`. No-op
/// without an existing manifest.
///
/// # Errors
/// An existing but unreadable manifest or report.
pub fn carry_over(out: &Path, m: &mut Manifest, report: &mut Value) -> Result<()> {
    if !out.join("manifest.json").exists() {
        return Ok(());
    }
    let old = read_manifest(out)?;
    if old.layers.is_empty() {
        return Ok(());
    }
    m.layers = old.layers;
    sync_sources(m);
    let rpath = out.join("build-report.json");
    if rpath.exists()
        && let Some(layers) = super::build::read_json(&rpath)?.get("layers")
    {
        report["layers"] = layers.clone();
    }
    Ok(())
}

/// Remove a layer from the release (its tiles, manifest entry, sources and
/// report), e.g. `soil` when `regions` (its land mask) is rebuilt. Returns
/// whether the layer was there.
///
/// # Errors
/// Missing release files, I/O errors.
pub fn unregister(out: &Path, id: &str) -> Result<bool> {
    let mut m = read_manifest(out)?;
    let Some(pos) = m.layers.iter().position(|l| l.id == id) else {
        return Ok(false);
    };
    let layer = m.layers.remove(pos);
    for t in &layer.tiles {
        let p = out.join(&t.path);
        if p.exists() {
            std::fs::remove_file(&p).map_err(|e| PipelineError::io(&p, e))?;
        }
    }
    sync_sources(&mut m);
    let rpath = out.join("build-report.json");
    let mut r = super::build::read_json(&rpath)?;
    if let Some(layers) = r.get_mut("layers").and_then(Value::as_object_mut) {
        layers.remove(id);
    }
    write_report(&rpath, &r)?;
    super::assemble::write_release_files(out, &m)?;
    Ok(true)
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

    /// A release with a stale source and a `soil` test layer (Paris = 42,
    /// a cell at the antimeridian = 7), registered twice.
    fn release_with_layer(name: &str) -> (std::path::PathBuf, LayerCell) {
        let out = temp(name);
        let mut m = super::super::assemble::manifest(vec![]);
        let mut stale = m.sources[0].clone();
        stale.id = "replaced-source".into();
        m.sources.push(stale);
        std::fs::write(out.join("build-report.json"), "{}").unwrap();
        super::super::assemble::write_release_files(&out, &m).unwrap();

        let mut band = vec![NODATA; LAYER_GRID_W * LAYER_GRID_H];
        let paris = LayerCell::of(48.85, 2.35).unwrap();
        band[paris.global_index()] = 42;
        band[LayerCell::of(-17.0, -179.99).unwrap().global_index()] = 7;
        let has = |t: &LayerTile, i: usize| t.get(i, 0) != NODATA;
        let tiles = cut_tiles(&[&band], has).unwrap();
        assert_eq!(tiles.len(), 2);
        let specs = write_tiles(&out, "test", tiles, has).unwrap();
        assert_eq!(specs[0].name, "n50e000");
        assert_eq!(specs[1].path, "test/s10w180.tif");
        assert!(specs.iter().all(|s| s.complete_cells == 1));
        let x = VariableSpec {
            id: "x".into(),
            unit: "-".into(),
            scale: 1.0,
            offset: 0.0,
            description: "x".into(),
            months: None,
        };
        let layer = LayerSpec {
            id: "soil".into(),
            description: "test".into(),
            cell_deg: 0.05,
            tile_cells: 200,
            directory: "test".into(),
            encoding: encoding(1),
            bands: vec![x],
            sources: vec!["soilgrids-2.0".into()],
            tiles: specs,
            notes: vec![],
            slots: vec![],
            version: None,
        };
        register(&out, layer.clone(), json!({"ok": 1})).unwrap();
        register(&out, layer, json!({"ok": 2})).unwrap();
        (out, paris)
    }

    #[test]
    fn cut_write_register_and_read_back() {
        let (out, paris) = release_with_layer("release");
        let m = read_manifest(&out).unwrap();
        assert_eq!(m.layers.len(), 1);
        assert_eq!(m.format_version, crate::manifest::FORMAT_VERSION);
        let ids: Vec<&str> = m.sources.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"soilgrids-2.0") && ids.contains(&"chelsa-v2.1"));
        assert!(!ids.contains(&"replaced-source"), "uncited source dropped");
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

    /// Re-running `tiles` writes a fresh manifest and report: the layers
    /// carry over.
    #[test]
    fn layers_carry_over_a_new_tiles_stage() {
        let (out, _) = release_with_layer("carry");
        let mut fresh = super::super::assemble::manifest(vec![]);
        let mut report = json!({"tiles": 0});
        carry_over(&out, &mut fresh, &mut report).unwrap();
        assert_eq!(fresh.layers.len(), 1);
        assert!(fresh.sources.iter().any(|s| s.id == "soilgrids-2.0"));
        assert_eq!(report["layers"]["soil"]["ok"], 2);
        assert_eq!(report["tiles"], 0);
    }

    /// Rebuilding a layer another depends on removes the dependent one.
    #[test]
    fn unregister_removes_tiles_entry_sources_and_report() {
        let (out, _) = release_with_layer("unregister");
        assert!(unregister(&out, "soil").unwrap());
        assert!(!unregister(&out, "soil").unwrap());
        let m = read_manifest(&out).unwrap();
        assert!(m.layers.is_empty());
        assert!(!m.sources.iter().any(|s| s.id == "soilgrids-2.0"));
        assert!(!out.join("test/n50e000.tif").exists());
        let report = super::super::build::read_json(&out.join("build-report.json")).unwrap();
        assert!(report["layers"].get("soil").is_none());
        let sums = std::fs::read_to_string(out.join("SHA256SUMS")).unwrap();
        assert!(!sums.contains("test/n50e000.tif"));
    }

    #[test]
    fn carry_over_without_a_release_is_a_no_op() {
        let out = temp("empty");
        let mut m = super::super::assemble::manifest(vec![]);
        let before = m.clone();
        let mut report = json!({});
        carry_over(&out, &mut m, &mut report).unwrap();
        assert_eq!(m, before);
        assert_eq!(report, json!({}));
    }
}
