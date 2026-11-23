//! Decoupled layer packs (`canopi-climate pack-grid`): packages a built grid
//! directory into modular uncompressed POSIX ustar `.tar` archives alongside a
//! top-level `packs-vX.Y.Z.json` index (plus `.sha256`).
//!
//! Pack hashes live ONLY in `packs-vX.Y.Z.json` (never inside `manifest.json`),
//! avoiding any hash cycle and keeping `manifest.json` byte-for-byte identical
//! whether installed from the monolithic tar or assembled from packs.
//!
//! Method: `docs/decisions/0008-soil-plus-terrain-microclimate-packs.md`.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{PipelineError, Result, layers, sha256_bytes, sha256_file};

/// Definition of a modular pack.
struct PackDef {
    id: &'static str,
    dirs: &'static [&'static str],
    root_files: &'static [&'static str],
    required: bool,
}

const PACK_DEFS: &[PackDef] = &[
    PackDef {
        id: "core",
        dirs: &["tiles", "soil", "regions", "extremes"],
        root_files: &["manifest.json", "ATTRIBUTION.md", "build-report.json"],
        required: true,
    },
    PackDef {
        id: "monthly",
        dirs: &["monthly", "monthly_precip"],
        root_files: &[],
        required: false,
    },
    PackDef {
        id: "v05",
        dirs: &["extremes_tail", "frost_calendar"],
        root_files: &["analogues.bin"],
        required: false,
    },
    PackDef {
        id: "v06",
        dirs: &["phenology", "water_balance"],
        root_files: &[],
        required: false,
    },
    PackDef {
        id: "v07",
        dirs: &["soil_plus", "terrain"],
        root_files: &[],
        required: false,
    },
];

/// Entry for one modular pack in `packs-vX.Y.Z.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackEntry {
    /// Pack identifier (`core`, `monthly`, `v05`, `v06`, `v07`).
    pub id: String,
    /// Archive filename (`canopi-climate-pack-<id>-v<version>.tar`).
    pub file: String,
    /// Archive size in bytes.
    pub bytes: u64,
    /// Archive SHA-256 (lowercase hex).
    pub sha256: String,
    /// Number of payload files inside the pack (excluding `SHA256SUMS.pack`).
    pub file_count: usize,
    /// Layer directories included in this pack.
    pub layers: Vec<String>,
    /// Top-level assets included in this pack (e.g. `manifest.json`, `analogues.bin`).
    pub assets: Vec<String>,
}

/// Top-level `packs-vX.Y.Z.json` release index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PacksIndex {
    /// Format identifier (`"canopi-climate-packs"`).
    pub format: String,
    /// Format version (`1`).
    pub format_version: u32,
    /// Release version string (without leading `v`, e.g. `"0.7.0"`).
    pub version: String,
    /// SHA-256 of `manifest.json` (identical between monolithic and pack installs).
    pub manifest_sha256: String,
    /// `climate_tiles_sha256` from `manifest.json`, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub climate_tiles_sha256: Option<String>,
    /// Modular pack archives in canonical order.
    pub packs: Vec<PackEntry>,
}

/// Write a single POSIX ustar file header + data + 512-byte padding.
fn write_ustar_entry(w: &mut impl Write, rel_path: &str, bytes: &[u8]) -> std::io::Result<()> {
    let mut header = [0u8; 512];
    let path_bytes = rel_path.as_bytes();
    if path_bytes.len() <= 100 {
        header[..path_bytes.len()].copy_from_slice(path_bytes);
    } else {
        // Split into ustar prefix (<=155) + name (<=100) at a '/'.
        let split = rel_path
            .rfind('/')
            .filter(|&i| i <= 155 && path_bytes.len() - i - 1 <= 100)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("tar path too long for ustar: {rel_path}"),
                )
            })?;
        header[..path_bytes.len() - split - 1].copy_from_slice(&path_bytes[split + 1..]);
        header[345..345 + split].copy_from_slice(&path_bytes[..split]);
    }
    // Mode 0644, uid 0, gid 0, size (octal 11 digits + NUL), mtime 0 (deterministic).
    header[100..108].copy_from_slice(b"0000644\0");
    header[108..116].copy_from_slice(b"0000000\0");
    header[116..124].copy_from_slice(b"0000000\0");
    let size_oct = format!("{:011o}\0", bytes.len());
    header[124..136].copy_from_slice(size_oct.as_bytes());
    header[136..148].copy_from_slice(b"00000000000\0");
    // Checksum placeholder: 8 ASCII spaces.
    header[148..156].copy_from_slice(b"        ");
    // Regular file typeflag '0'.
    header[156] = b'0';
    // POSIX ustar magic + version.
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");

    let cksum: u32 = header.iter().map(|&b| u32::from(b)).sum();
    let cksum_str = format!("{cksum:06o}\0 ");
    header[148..156].copy_from_slice(cksum_str.as_bytes());

    w.write_all(&header)?;
    w.write_all(bytes)?;
    let rem = bytes.len() % 512;
    if rem != 0 {
        let pad = [0u8; 512];
        w.write_all(&pad[..512 - rem])?;
    }
    Ok(())
}

fn collect_dir_files(grid_dir: &Path, sub: &str) -> Result<Vec<(String, PathBuf)>> {
    let dir = grid_dir.join(sub);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| PipelineError::io(&dir, e))? {
        let entry = entry.map_err(|e| PipelineError::io(&dir, e))?;
        let path = entry.path();
        if path.is_file() {
            let name = entry.file_name().to_string_lossy().into_owned();
            out.push((format!("{sub}/{name}"), path));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Package `grid_dir` into modular `.tar` packs + `packs-v<version>.json` (+ `.sha256`)
/// inside `out_dir`.
///
/// # Errors
/// Missing `manifest.json` or I/O errors.
pub fn pack_grid(grid_dir: &Path, out_dir: &Path, version: &str) -> Result<PacksIndex> {
    let ver = version.trim_start_matches('v');
    let manifest_path = grid_dir.join("manifest.json");
    let manifest = layers::read_manifest(grid_dir)?;
    let manifest_sha256 = sha256_file(&manifest_path)?;

    fs::create_dir_all(out_dir).map_err(|e| PipelineError::io(out_dir, e))?;
    let mut packs = Vec::new();

    for def in PACK_DEFS {
        let mut files: Vec<(String, PathBuf)> = Vec::new();
        let mut present_layers = Vec::new();
        let mut present_assets = Vec::new();

        for &root_file in def.root_files {
            let p = grid_dir.join(root_file);
            if p.is_file() {
                files.push((root_file.to_string(), p));
                present_assets.push(root_file.to_string());
            }
        }
        for &dir_name in def.dirs {
            let dir_files = collect_dir_files(grid_dir, dir_name)?;
            if !dir_files.is_empty() {
                if dir_name != "tiles" {
                    present_layers.push(dir_name.to_string());
                } else {
                    present_assets.push("tiles/".to_string());
                }
                files.extend(dir_files);
            }
        }

        if files.is_empty() {
            if def.required {
                return Err(PipelineError::Data(format!(
                    "required pack '{}' has no files in {}",
                    def.id,
                    grid_dir.display()
                )));
            }
            continue;
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));

        let archive_name = format!("canopi-climate-pack-{}-v{ver}.tar", def.id);
        let archive_path = out_dir.join(&archive_name);
        let file = File::create(&archive_path).map_err(|e| PipelineError::io(&archive_path, e))?;
        let mut writer = BufWriter::new(file);

        let mut sums = String::new();
        for (rel, abs) in &files {
            let bytes = fs::read(abs).map_err(|e| PipelineError::io(abs, e))?;
            let hash = sha256_bytes(&bytes);
            sums.push_str(&format!("{hash}  {rel}\n"));
            write_ustar_entry(&mut writer, rel, &bytes)
                .map_err(|e| PipelineError::io(&archive_path, e))?;
        }
        write_ustar_entry(&mut writer, "SHA256SUMS.pack", sums.as_bytes())
            .map_err(|e| PipelineError::io(&archive_path, e))?;
        // Two 512-byte zero blocks terminate a POSIX tar archive.
        writer
            .write_all(&[0u8; 1024])
            .map_err(|e| PipelineError::io(&archive_path, e))?;
        writer
            .flush()
            .map_err(|e| PipelineError::io(&archive_path, e))?;
        drop(writer);

        let bytes = fs::metadata(&archive_path)
            .map_err(|e| PipelineError::io(&archive_path, e))?
            .len();
        let sha256 = sha256_file(&archive_path)?;
        let sha_path = out_dir.join(format!("{archive_name}.sha256"));
        fs::write(&sha_path, format!("{sha256}  {archive_name}\n"))
            .map_err(|e| PipelineError::io(&sha_path, e))?;

        packs.push(PackEntry {
            id: def.id.to_string(),
            file: archive_name,
            bytes,
            sha256,
            file_count: files.len(),
            layers: present_layers,
            assets: present_assets,
        });
    }

    let index = PacksIndex {
        format: "canopi-climate-packs".into(),
        format_version: 1,
        version: ver.into(),
        manifest_sha256,
        climate_tiles_sha256: manifest.climate_tiles_sha256,
        packs,
    };
    let index_name = format!("packs-v{ver}.json");
    let index_path = out_dir.join(&index_name);
    let json_bytes =
        serde_json::to_vec_pretty(&index).map_err(|e| PipelineError::Data(e.to_string()))?;
    fs::write(&index_path, &json_bytes).map_err(|e| PipelineError::io(&index_path, e))?;
    let index_sha = sha256_bytes(&json_bytes);
    let index_sha_path = out_dir.join(format!("{index_name}.sha256"));
    fs::write(&index_sha_path, format!("{index_sha}  {index_name}\n"))
        .map_err(|e| PipelineError::io(&index_sha_path, e))?;

    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Encoding, GridSpec, Manifest, TileSpec};

    /// Minimal ustar reader for round-trip verification in tests.
    fn list_ustar_entries(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut off = 0usize;
        while off + 512 <= bytes.len() {
            let hdr = &bytes[off..off + 512];
            if hdr.iter().all(|&b| b == 0) {
                break;
            }
            assert_eq!(&hdr[257..263], b"ustar\0");
            let name_end = hdr[..100].iter().position(|&b| b == 0).unwrap_or(100);
            let name = std::str::from_utf8(&hdr[..name_end]).unwrap().to_string();
            let size_end = hdr[124..136].iter().position(|&b| b == 0).unwrap_or(12);
            let size_str = std::str::from_utf8(&hdr[124..124 + size_end])
                .unwrap()
                .trim();
            let size = usize::from_str_radix(size_str, 8).unwrap();
            off += 512;
            let data = bytes[off..off + size].to_vec();
            off += size.div_ceil(512) * 512;
            out.push((name, data));
        }
        out
    }

    #[test]
    fn pack_grid_round_trips_and_avoids_manifest_hash_cycle() {
        let tmp = std::env::temp_dir().join(format!("canopi-pack-test-{}", std::process::id()));
        let grid_dir = tmp.join("grid");
        let out_dir = tmp.join("packs");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(grid_dir.join("tiles")).unwrap();
        fs::create_dir_all(grid_dir.join("soil_plus")).unwrap();
        fs::create_dir_all(grid_dir.join("terrain")).unwrap();

        let manifest = Manifest {
            format: "canopi-climate-grid".into(),
            format_version: 1,
            builder_version: "0.7.0".into(),
            built_at: "2026-11-23".into(),
            grid: GridSpec {
                crs: "EPSG:4326".into(),
                cell_deg: 0.1,
                tile_deg: 10.0,
                tile_cells: 100,
                tile_naming: "nw".into(),
                cell_edges: "nw".into(),
            },
            encoding: Encoding {
                container: "cog".into(),
                sample_format: "int16".into(),
                nodata: -32768,
                compression: "deflate".into(),
                interleave: "pixel".into(),
                bands: 72,
                band_order: "var,slot".into(),
            },
            slots: Vec::new(),
            variables: Vec::new(),
            sources: Vec::new(),
            tiles: vec![TileSpec {
                name: "n50e000".into(),
                path: "tiles/n50e000.tif".into(),
                bytes: 4,
                sha256: sha256_bytes(b"tile"),
                complete_cells: 1,
            }],
            layers: Vec::new(),
            dry_month_rule: None,
            climate_tiles_sha256: Some("abc123".into()),
            analogues: None,
            notes: Vec::new(),
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        fs::write(grid_dir.join("manifest.json"), &manifest_bytes).unwrap();
        fs::write(grid_dir.join("tiles/n50e000.tif"), b"tile").unwrap();
        fs::write(grid_dir.join("soil_plus/n50e000.tif"), b"soilplus").unwrap();
        fs::write(grid_dir.join("terrain/n50e000.tif"), b"terrain").unwrap();

        let index = pack_grid(&grid_dir, &out_dir, "0.7.0").unwrap();
        assert_eq!(index.format, "canopi-climate-packs");
        assert_eq!(index.format_version, 1);
        assert_eq!(index.manifest_sha256, sha256_bytes(&manifest_bytes));
        assert_eq!(index.climate_tiles_sha256.as_deref(), Some("abc123"));
        assert_eq!(index.packs.len(), 2); // core + v07
        assert_eq!(index.packs[0].id, "core");
        assert_eq!(index.packs[1].id, "v07");

        // Verify ustar contents of core and v07 packs
        let core_bytes = fs::read(out_dir.join(&index.packs[0].file)).unwrap();
        assert_eq!(sha256_bytes(&core_bytes), index.packs[0].sha256);
        let core_entries = list_ustar_entries(&core_bytes);
        let manifest_in_tar = core_entries
            .iter()
            .find(|(n, _)| n == "manifest.json")
            .unwrap();
        assert_eq!(manifest_in_tar.1, manifest_bytes);

        let v07_bytes = fs::read(out_dir.join(&index.packs[1].file)).unwrap();
        let v07_entries = list_ustar_entries(&v07_bytes);
        assert!(
            v07_entries
                .iter()
                .any(|(n, _)| n == "soil_plus/n50e000.tif")
        );
        assert!(v07_entries.iter().any(|(n, _)| n == "terrain/n50e000.tif"));
        assert!(v07_entries.iter().any(|(n, _)| n == "SHA256SUMS.pack"));

        let _ = fs::remove_dir_all(&tmp);
    }
}
