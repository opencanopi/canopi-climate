//! Offline pipeline (`pipeline` feature, native only): reads the source
//! rasters, aggregates them to the 0.1-degree grid, derives the variables and
//! writes the tiles and the manifest.
//!
//! Workarounds for gaps in `wbgeotiff`/`wbhdf` (rev 9c0ff4f) live here and
//! are listed in `docs/upstream-gaps.md`; upstream contributions are a
//! follow-up.

pub mod aggregate;
pub mod assemble;
pub mod build;
pub mod cog;
pub mod igh;
pub mod netcdf;
pub mod raster;
pub mod sample;
pub mod sources;
pub mod tiff_rows;

use std::path::PathBuf;

/// Pipeline error.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    /// File system error, with the path involved.
    #[error("{path}: {source}")]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// A raster could not be read or written.
    #[error("raster: {0}")]
    Raster(String),
    /// A NetCDF/HDF5 file could not be read.
    #[error("netcdf: {0}")]
    NetCdf(String),
    /// A source file is missing or does not match its checksum.
    #[error("source: {0}")]
    Source(String),
    /// Inputs are inconsistent.
    #[error("data: {0}")]
    Data(String),
}

/// Pipeline result.
pub type Result<T> = std::result::Result<T, PipelineError>;

impl PipelineError {
    /// Wrap an I/O error with its path.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

impl From<wbgeotiff::GeoTiffError> for PipelineError {
    fn from(e: wbgeotiff::GeoTiffError) -> Self {
        Self::Raster(e.to_string())
    }
}

impl From<crate::tile::TileError> for PipelineError {
    fn from(e: crate::tile::TileError) -> Self {
        Self::Raster(e.to_string())
    }
}

/// SHA-256 of a file (hex), streamed.
///
/// # Errors
/// I/O errors.
pub fn sha256_file(path: &std::path::Path) -> Result<String> {
    use sha2::Digest;
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| PipelineError::io(path, e))?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| PipelineError::io(path, e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

/// SHA-256 of bytes (hex).
#[must_use]
pub fn sha256_bytes(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex(&sha2::Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn sha256_of_known_input() {
        assert_eq!(
            super::sha256_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
