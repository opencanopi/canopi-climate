//! A v0.1 reader (the manifest types of canopi-climate 0.1.0, verbatim in
//! `fixtures/manifest_v0_1.rs`) must read releases with site layers: the
//! layers are an added optional field, so the format version stays 1.

#[cfg(test)]
#[cfg(feature = "pipeline")]
#[path = "fixtures/manifest_v0_1.rs"]
#[expect(dead_code, reason = "only the types serde fills are used")]
mod v0_1;

#[cfg(test)]
#[cfg(feature = "pipeline")]
mod manifest_compat {
    use super::v0_1;
    use canopi_climate::manifest::{FORMAT, FORMAT_VERSION, LayerSpec};
    use canopi_climate::pipeline::assemble;

    /// What 0.1.0's `Grid::open` checks, on a manifest with site layers.
    fn v0_1_reads(json: &str) -> v0_1::Manifest {
        let m: v0_1::Manifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.format, v0_1::FORMAT);
        assert_eq!(
            m.format_version,
            v0_1::FORMAT_VERSION,
            "0.1.0 refuses other versions"
        );
        m
    }

    #[test]
    fn v0_1_reader_reads_a_release_with_layers() {
        let mut m = assemble::manifest(vec![]);
        let layer: LayerSpec = serde_json::from_value(serde_json::json!({
            "id": "soil", "description": "d", "cell_deg": 0.05, "tile_cells": 200,
            "directory": "soil", "encoding": m.encoding, "bands": m.variables,
            "sources": ["soilgrids-2.0"], "tiles": [], "notes": []
        }))
        .unwrap();
        m.layers.push(layer);
        assert_eq!(
            (m.format.as_str(), m.format_version),
            (FORMAT, FORMAT_VERSION)
        );
        let json = serde_json::to_string(&m).unwrap();
        let old = v0_1_reads(&json);
        assert_eq!(old.tiles.len(), m.tiles.len());
        assert_eq!(old.variables.len(), m.variables.len());
    }

    #[test]
    #[ignore = "needs CANOPI_CLIMATE_GRID"]
    fn v0_1_reader_reads_the_built_release() {
        let dir = std::path::PathBuf::from(std::env::var_os("CANOPI_CLIMATE_GRID").unwrap());
        let json = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
        let old = v0_1_reads(&json);
        assert_eq!(old.tiles.len(), 464);
    }
}
