//! `manifest.json`: what a grid release contains and where it comes from.

use serde::{Deserialize, Serialize};

/// Format identifier written in every manifest.
pub const FORMAT: &str = "canopi-climate-grid";
/// Manifest/tile format version. Fields added without changing the
/// meaning of existing ones keep the version, so older readers still work
/// (they ignore unknown fields): [`Manifest::layers`] (0.2.0) is optional.
pub const FORMAT_VERSION: u32 = 1;
/// Identifier of the Gaussen dry-month rule (`P < 2 * T`, docs/format.md).
pub const DRY_MONTH_RULE: &str = "gaussen_p_lt_2t";

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
    /// Identifier of the dry-month rule (`gaussen_p_lt_2t`; optional for older manifests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dry_month_rule: Option<String>,
    /// Optional layers, each with its own grid and tiles (absent before 0.2.0; optional).
    #[serde(default)]
    pub layers: Vec<LayerSpec>,
}

/// An optional layer: its own tiles, in the climate grid's 10-degree tiles
/// on the layer's declared grid (`cell_deg`, `tile_cells`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerSpec {
    /// Identifier, e.g. `soil`, `regions`, `extremes`, `monthly`, `monthly_precip`.
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
    /// (`extremes`, `monthly`, `monthly_precip`); absent for a static layer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<SlotSpec>,
    /// Optional layer-specific version (incremented when rebuilt alone).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl LayerSpec {
    /// Number of slots (`1` for a static layer whose `slots` list is empty).
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.slots.len().max(1)
    }

    /// 0-based band index under `band_order: "band, slot, month"`:
    /// `start(band) + slot * months(band) + month` (`months` counted as 1 when
    /// absent), where `start(band)` is the sum of `slot_count * months` of all
    /// preceding bands. Returns `None` if `band`, `slot`, or `month` is out of
    /// range.
    #[must_use]
    pub fn band_index(&self, band: usize, slot: usize, month: usize) -> Option<usize> {
        let slots = self.slot_count();
        let target = self.bands.get(band)?;
        let target_months = usize::from(target.months.unwrap_or(1));
        if slot >= slots || target_months == 0 || month >= target_months {
            return None;
        }
        let mut start = 0usize;
        for b in &self.bands[..band] {
            let m = usize::from(b.months.unwrap_or(1));
            if m == 0 {
                return None;
            }
            start = start.checked_add(slots.checked_mul(m)?)?;
        }
        start
            .checked_add(slot.checked_mul(target_months)?)?
            .checked_add(month)
    }

    /// Total bands per tile implied by `bands` and `slots`.
    #[must_use]
    pub fn total_bands(&self) -> usize {
        let slots = self.slot_count();
        self.bands
            .iter()
            .map(|b| slots * usize::from(b.months.unwrap_or(1)))
            .sum()
    }
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
    /// Months per slot for a monthly band (12); absent for non-monthly bands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub months: Option<u8>,
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

    #[test]
    fn layer_spec_band_index_matches_band_slot_month_order() {
        let mk = |id: &str, months: Option<u8>| VariableSpec {
            id: id.into(),
            unit: "°C".into(),
            scale: 0.5,
            offset: 0.0,
            description: String::new(),
            months,
        };
        let slot = |p: &str, s: &str| SlotSpec {
            period: p.into(),
            scenario: s.into(),
            scenario_label: s.into(),
        };
        let layer = LayerSpec {
            id: "monthly".into(),
            description: String::new(),
            cell_deg: 0.1,
            tile_cells: 100,
            directory: "monthly".into(),
            encoding: Encoding {
                container: String::new(),
                sample_format: "int16".into(),
                nodata: -32768,
                compression: "deflate".into(),
                interleave: "pixel".into(),
                bands: 75,
                band_order: "band, slot, month".into(),
            },
            bands: vec![
                mk("tmean", Some(12)),
                mk("tmin", Some(12)),
                mk("dry_mask", None),
            ],
            sources: vec![],
            tiles: vec![],
            notes: vec![],
            slots: vec![
                slot("1991-2020", "historical"),
                slot("2041-2070", "ssp245"),
                slot("2071-2099", "ssp245"),
            ],
            version: Some("0.4.0".into()),
        };
        assert_eq!(layer.total_bands(), 75);
        assert_eq!(layer.band_index(0, 0, 0), Some(0));
        assert_eq!(layer.band_index(0, 2, 11), Some(35));
        assert_eq!(layer.band_index(1, 0, 0), Some(36));
        assert_eq!(layer.band_index(1, 2, 11), Some(71));
        assert_eq!(layer.band_index(2, 0, 0), Some(72));
        assert_eq!(layer.band_index(2, 2, 0), Some(74));
        assert_eq!(layer.band_index(2, 0, 1), None);
        assert_eq!(layer.band_index(0, 3, 0), None);
        assert_eq!(layer.band_index(3, 0, 0), None);
    }
}
