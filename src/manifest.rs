//! `manifest.json`: what a grid release contains and where it comes from.

use serde::{Deserialize, Serialize};

/// Format identifier written in every manifest.
pub const FORMAT: &str = "canopi-climate-grid";
/// Manifest/tile format version. Fields added without changing the
/// meaning of existing ones keep the version, so older readers still work
/// (they ignore unknown fields): [`Manifest::layers`] (0.2.0) is optional.
pub const FORMAT_VERSION: u32 = 1;

/// The manifest of a grid release.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Always [`FORMAT`].
    pub format: String,
    /// [`FORMAT_VERSION`] at build time.
    pub format_version: u32,
    /// Version of the crate that built the grid.
    pub builder_version: String,
    /// Build time (UTC, RFC 3339).
    pub built_at: String,
    /// Grid geometry.
    pub grid: GridSpec,
    /// How tiles encode values.
    pub encoding: Encoding,
    /// Variables in band order.
    pub variables: Vec<VariableSpec>,
    /// (period, scenario) slots in band order.
    pub slots: Vec<SlotSpec>,
    /// Source datasets.
    pub sources: Vec<SourceSpec>,
    /// Tiles with land cells.
    pub tiles: Vec<TileSpec>,
    /// Notes on methods and known limits.
    pub notes: Vec<String>,
    /// Site layers on the finer layer grid (absent before 0.2.0; optional).
    #[serde(default)]
    pub layers: Vec<LayerSpec>,
}

/// A site layer: its own tiles, in the climate grid's 10-degree tiles on
/// a finer grid (see `crate::layer`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerSpec {
    /// Identifier: `soil` or `regions`.
    pub id: String,
    /// What the layer holds.
    pub description: String,
    /// Cell size (degrees).
    pub cell_deg: f64,
    /// Cells per tile side.
    pub tile_cells: u32,
    /// Directory of the tiles, relative to the manifest.
    pub directory: String,
    /// How tiles encode values.
    pub encoding: Encoding,
    /// Bands in order (`scale`/`offset` decode the stored value).
    pub bands: Vec<VariableSpec>,
    /// Ids of the [`Manifest::sources`] the layer derives from.
    pub sources: Vec<String>,
    /// Tiles with data (`complete_cells`: cells with a value).
    pub tiles: Vec<TileSpec>,
    /// Notes on methods and known limits.
    pub notes: Vec<String>,
    /// The (period, scenario) slots of a layer whose bands repeat per slot
    /// (`extremes`, 0.3.0); absent for a static layer. Band
    /// `band_index * slots.len() + slot` holds band `band_index` for `slot`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<SlotSpec>,
}

/// Grid geometry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GridSpec {
    /// Coordinate reference system, `EPSG:4326`.
    pub crs: String,
    /// Cell size (degrees).
    pub cell_deg: f64,
    /// Tile size (degrees).
    pub tile_deg: f64,
    /// Cells per tile side.
    pub tile_cells: u32,
    /// How tiles are named.
    pub tile_naming: String,
    /// Which cell edges are inclusive.
    pub cell_edges: String,
}

/// Tile encoding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Encoding {
    /// File format, `COG GeoTIFF`.
    pub container: String,
    /// Sample type, `int16`.
    pub sample_format: String,
    /// Stored value meaning no data.
    pub nodata: i16,
    /// Compression.
    pub compression: String,
    /// Band interleave.
    pub interleave: String,
    /// Bands per tile.
    pub bands: u32,
    /// Band numbering.
    pub band_order: String,
}

/// A variable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VariableSpec {
    /// Identifier.
    pub id: String,
    /// Unit of the decoded value.
    pub unit: String,
    /// Decoded = stored * scale + offset.
    pub scale: f32,
    /// See `scale`.
    pub offset: f32,
    /// Description and derivation.
    pub description: String,
}

/// A (period, scenario) slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlotSpec {
    /// `1991-2020`, `2041-2070`, `2071-2099`.
    pub period: String,
    /// `historical`, `ssp126`, ...
    pub scenario: String,
    /// Display name of the scenario.
    pub scenario_label: String,
}

/// A source dataset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceSpec {
    /// Identifier.
    pub id: String,
    /// Title of the dataset.
    pub title: String,
    /// Citation to show with derived data.
    pub citation: String,
    /// Dataset DOI or landing page.
    pub landing: String,
    /// Licence as published, with its URL.
    pub licence: String,
    /// Notes on the licence or the use made here.
    pub notes: String,
    /// Files used, with download URLs and checksums.
    pub files: Vec<SourceFile>,
}

/// A downloaded source file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceFile {
    /// Path relative to the source cache directory.
    pub path: String,
    /// Download URL (for files extracted from an archive, the archive's).
    pub url: String,
    /// SHA-256 of the file (hex).
    pub sha256: String,
}

/// A tile file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TileSpec {
    /// Tile name, e.g. `n50e000`.
    pub name: String,
    /// File name relative to the manifest, e.g. `tiles/n50e000.tif`.
    pub path: String,
    /// File size in bytes.
    pub bytes: u64,
    /// SHA-256 of the file (hex).
    pub sha256: String,
    /// Cells with every variable for every slot.
    pub complete_cells: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_spec_round_trips_through_json() {
        let t = TileSpec {
            name: "n50e000".into(),
            path: "tiles/n50e000.tif".into(),
            bytes: 12,
            sha256: "ab".into(),
            complete_cells: 3,
        };
        let s = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<TileSpec>(&s).unwrap(), t);
    }
}
