//! Compact binary k-NN index (`analogues.bin`) for the **2050 Climate Twin**
//! search: matches any target climate vector against representative historical
//! (1991–2020) land locations worldwide in `< 2 ms` in pure Rust / WebAssembly.
//!
//! Binary layout (`docs/format.md` & ADR 0006, little-endian):
//! - Header (120 bytes):
//!   - `magic: [u8; 4]` = `b"CNPA"`
//!   - `version: u16` = `1`
//!   - `header_bytes: u16` = `120`
//!   - `record_count: u32`
//!   - `feature_count: u8` = `8`
//!   - `reserved: [u8; 3]` = `[0, 0, 0]`
//!   - `mean: [f32; 8]`, `inv_std: [f32; 8]`, `weight: [f32; 8]`
//!   - `close_threshold: f32` (`1.0`), `novel_threshold: f32` (`2.0`)
//! - Records (`record_count × 26` bytes):
//!   - `lat_e2: i16`, `lon_e2: i16` (0.01°)
//!   - `iso_a2: u16` (`0xFFFF` = none), `wgsrpd_l3: u16` (`0xFFFF` = none)
//!   - `koppen: u8` (1..=30), `flags: u8` (bit 0 = `aridity` valid)
//!   - `features: [i16; 8]` (quantized with [`FEATURE_SCALES`])

use serde::{Deserialize, Serialize};

use crate::koppen;
use crate::regions::decode_code;

/// File name of the analogue index in a grid release.
pub const FILE_NAME: &str = "analogues.bin";

/// Magic bytes at offset 0 of `analogues.bin`.
pub const MAGIC: [u8; 4] = *b"CNPA";

/// Binary format version.
pub const FORMAT_VERSION: u16 = 1;

/// Fixed header size in bytes.
pub const HEADER_BYTES: usize = 120;

/// Number of phase-invariant features per record.
pub const FEATURES: usize = 8;

/// Fixed record size in bytes.
pub const RECORD_BYTES: usize = 26;

/// Sentinel for missing `iso_a2` or `wgsrpd_l3` code in a record.
pub const NO_CODE: u16 = u16::MAX;

/// Flag bit indicating that `aridity` (feature 5) is valid.
pub const FLAG_ARIDITY_VALID: u8 = 1 << 0;

/// Feature index of `aridity` (masked when absent on polar/ice-sheet cells).
pub const ARIDITY_IDX: usize = 5;

/// Quantization scales for the 8 features:
/// `[tmean_cold (°C), tmean_warm (°C), tmin_cold (°C), tmax_warm (°C),
///   ln_precip, aridity, dry_months, summer_rain_frac]`.
pub const FEATURE_SCALES: [f32; FEATURES] = [0.1, 0.1, 0.1, 0.1, 0.001, 0.01, 1.0, 0.001];

/// Default feature weights (emphasizing winter/summer thermal regime and water regime).
pub const DEFAULT_WEIGHTS: [f32; FEATURES] = [1.4, 1.4, 1.0, 1.0, 1.2, 0.8, 1.0, 1.2];

/// Default standardized distance threshold for a `close` match (σ).
pub const DEFAULT_CLOSE_THRESHOLD: f32 = 1.0;

/// Default standardized distance threshold above which a climate is flagged `novel` (σ).
pub const DEFAULT_NOVEL_THRESHOLD: f32 = 2.0;

/// Errors decoding an `analogues.bin` buffer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AnalogueError {
    /// Buffer is truncated or shorter than declared.
    #[error("analogues.bin truncated: {0}")]
    Truncated(String),
    /// Header magic, version, or feature count does not match.
    #[error("analogues.bin invalid header: {0}")]
    Header(String),
}

/// One historical (1991–2020) reference land location in `analogues.bin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnalogueRecord {
    /// Latitude in hundredths of a degree (`round(lat * 100)`).
    pub lat_e2: i16,
    /// Longitude in hundredths of a degree (`round(lon * 100)`).
    pub lon_e2: i16,
    /// Encoded ISO 3166-1 alpha-2 (`0..676`), or [`NO_CODE`].
    pub iso_a2: u16,
    /// Encoded WGSRPD level-3 (`0..17576`), or [`NO_CODE`].
    pub wgsrpd_l3: u16,
    /// Köppen-Geiger class code (`1..=30`).
    pub koppen: u8,
    /// Bit flags ([`FLAG_ARIDITY_VALID`]).
    pub flags: u8,
    /// Quantized 8D phase-invariant feature vector.
    pub features: [i16; FEATURES],
}

impl AnalogueRecord {
    /// Latitude in degrees.
    #[must_use]
    pub fn lat(self) -> f64 {
        f64::from(self.lat_e2) / 100.0
    }

    /// Longitude in degrees.
    #[must_use]
    pub fn lon(self) -> f64 {
        f64::from(self.lon_e2) / 100.0
    }

    /// Whether `aridity` (feature 5) is present.
    #[must_use]
    pub const fn aridity_valid(self) -> bool {
        (self.flags & FLAG_ARIDITY_VALID) != 0
    }
}

/// Decoded `analogues.bin` k-NN index.
#[derive(Debug, Clone, PartialEq)]
pub struct AnalogueIndex {
    /// Per-feature global mean used for z-score standardization.
    pub mean: [f32; FEATURES],
    /// Per-feature inverse standard deviation (`1.0 / std`).
    pub inv_std: [f32; FEATURES],
    /// Per-feature distance weights.
    pub weight: [f32; FEATURES],
    /// Upper σ bound for `MatchQuality::Close`.
    pub close_threshold: f32,
    /// Upper σ bound for `MatchQuality::Moderate` (above = `Novel`).
    pub novel_threshold: f32,
    /// Reference records.
    pub records: Vec<AnalogueRecord>,
}

/// Target climate query for [`AnalogueIndex::search`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalogueQuery {
    /// Query latitude in degrees (used for `distance_km`, `exclude_radius_km`, and `delta_lat_deg`).
    pub lat: f64,
    /// Query longitude in degrees.
    pub lon: f64,
    /// Coldest-month mean temperature (°C).
    pub tmean_cold_c: f64,
    /// Warmest-month mean temperature (°C).
    pub tmean_warm_c: f64,
    /// Coldest-month mean daily minimum (°C).
    pub tmin_cold_c: f64,
    /// Warmest-month mean daily maximum (°C).
    pub tmax_warm_c: f64,
    /// Annual precipitation (mm/year).
    pub precip_mm: f64,
    /// Aridity index (`None` on polar cells where PET < 1 mm/year).
    pub aridity_index: Option<f64>,
    /// Gaussen dry-month count (`0..=12`).
    pub dry_months: u8,
    /// Fraction of annual precipitation falling in the 6 warmest months (`0.0..=1.0`).
    pub summer_rain_frac: f64,
}

/// Search parameters for [`AnalogueIndex::search`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SearchOptions {
    /// Maximum number of deduplicated analogues to return (default 5).
    #[serde(default = "default_k")]
    pub k: usize,
    /// Minimum Haversine separation between returned twins in km (default 100.0).
    #[serde(default = "default_min_distance_km")]
    pub min_distance_km: f64,
    /// Minimum Haversine distance from the query location in km (default 0.0).
    #[serde(default)]
    pub exclude_radius_km: f64,
}

const fn default_k() -> usize {
    5
}

const fn default_min_distance_km() -> f64 {
    100.0
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            k: default_k(),
            min_distance_km: default_min_distance_km(),
            exclude_radius_km: 0.0,
        }
    }
}

/// Qualitative band of a climate twin match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchQuality {
    /// Standardized distance `<= close_threshold` (1.0 σ).
    Close,
    /// Standardized distance in `(close_threshold, novel_threshold]` (1.0–2.0 σ).
    Moderate,
    /// Standardized distance `> novel_threshold` (> 2.0 σ; no close historical twin).
    Novel,
}

/// One matched historical analogue location.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalogueMatch {
    /// Latitude of the matched location (°).
    pub lat: f64,
    /// Longitude of the matched location (°).
    pub lon: f64,
    /// Country / territory code (ISO 3166-1 alpha-2), if available.
    pub iso_a2: Option<String>,
    /// Botanical country code (WGSRPD level-3), if available.
    pub wgsrpd_l3: Option<String>,
    /// Köppen-Geiger class symbol (e.g. `Csa`).
    pub koppen: String,
    /// Köppen-Geiger class code (`1..=30`).
    pub koppen_code: u8,
    /// Standardized weighted RMS distance (σ).
    pub sigma: f64,
    /// Match quality band (`close`, `moderate`, `novel`).
    pub match_quality: MatchQuality,
    /// Great-circle Haversine distance from the query location (km).
    pub distance_km: f64,
    /// Absolute difference in `|lat|` between query and twin (degrees, photoperiod proxy).
    pub delta_lat_deg: f64,
}

/// Result of [`AnalogueIndex::search`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalogueResult {
    /// `true` when the closest twin exceeds `novel_threshold` (or no twin exists).
    pub novel_climate: bool,
    /// Standardized distance (σ) of the closest twin, if any.
    pub best_sigma: Option<f64>,
    /// Top-`k` spatially deduplicated historical analogues, sorted by `sigma` ascending.
    pub matches: Vec<AnalogueMatch>,
}

/// Quantize a physical feature vector into `[i16; 8]`.
#[must_use]
pub fn quantize_features(raw: [f32; FEATURES]) -> [i16; FEATURES] {
    let mut out = [0i16; FEATURES];
    for i in 0..FEATURES {
        let v = raw[i];
        out[i] = if v.is_finite() {
            (v / FEATURE_SCALES[i])
                .round()
                .clamp(f32::from(i16::MIN + 1), f32::from(i16::MAX)) as i16
        } else {
            0
        };
    }
    out
}

/// Great-circle distance in kilometres between two WGS84/spherical points.
/// Handles ±180° antimeridian wrap and high-latitude convergence cleanly.
#[must_use]
pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const R_KM: f64 = 6371.0088;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat * 0.5).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon * 0.5).sin().powi(2);
    2.0 * R_KM * a.clamp(0.0, 1.0).sqrt().asin()
}

fn read_u16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn read_i16(b: &[u8], off: usize) -> i16 {
    i16::from_le_bytes([b[off], b[off + 1]])
}

fn read_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn read_f32(b: &[u8], off: usize) -> f32 {
    f32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

impl AnalogueIndex {
    /// Decode an `analogues.bin` byte slice.
    ///
    /// # Errors
    /// [`AnalogueError`] when the buffer is truncated or has an invalid header.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, AnalogueError> {
        if bytes.len() < HEADER_BYTES {
            return Err(AnalogueError::Truncated(format!(
                "{} bytes, need at least {HEADER_BYTES}",
                bytes.len()
            )));
        }
        if bytes[0..4] != MAGIC {
            return Err(AnalogueError::Header("bad magic".into()));
        }
        let version = read_u16(bytes, 4);
        if version != FORMAT_VERSION {
            return Err(AnalogueError::Header(format!(
                "unsupported version {version}, expected {FORMAT_VERSION}"
            )));
        }
        let header_bytes = usize::from(read_u16(bytes, 6));
        if header_bytes < HEADER_BYTES || bytes.len() < header_bytes {
            return Err(AnalogueError::Header(format!(
                "invalid header_bytes {header_bytes}"
            )));
        }
        let count = read_u32(bytes, 8) as usize;
        let feat = usize::from(bytes[12]);
        if feat != FEATURES {
            return Err(AnalogueError::Header(format!(
                "feature_count {feat}, expected {FEATURES}"
            )));
        }
        let need = header_bytes
            .checked_add(
                count
                    .checked_mul(RECORD_BYTES)
                    .ok_or_else(|| AnalogueError::Truncated("record count overflow".into()))?,
            )
            .ok_or_else(|| AnalogueError::Truncated("size overflow".into()))?;
        if bytes.len() < need {
            return Err(AnalogueError::Truncated(format!(
                "{} bytes, need {need} for {count} records",
                bytes.len()
            )));
        }
        let mut mean = [0f32; FEATURES];
        let mut inv_std = [0f32; FEATURES];
        let mut weight = [0f32; FEATURES];
        for i in 0..FEATURES {
            mean[i] = read_f32(bytes, 16 + i * 4);
            inv_std[i] = read_f32(bytes, 48 + i * 4);
            weight[i] = read_f32(bytes, 80 + i * 4);
        }
        let close_threshold = read_f32(bytes, 112);
        let novel_threshold = read_f32(bytes, 116);

        let mut records = Vec::with_capacity(count);
        for r in 0..count {
            let off = header_bytes + r * RECORD_BYTES;
            let mut features = [0i16; FEATURES];
            for (i, f) in features.iter_mut().enumerate() {
                *f = read_i16(bytes, off + 10 + i * 2);
            }
            records.push(AnalogueRecord {
                lat_e2: read_i16(bytes, off),
                lon_e2: read_i16(bytes, off + 2),
                iso_a2: read_u16(bytes, off + 4),
                wgsrpd_l3: read_u16(bytes, off + 6),
                koppen: bytes[off + 8],
                flags: bytes[off + 9],
                features,
            });
        }
        Ok(Self {
            mean,
            inv_std,
            weight,
            close_threshold,
            novel_threshold,
            records,
        })
    }

    /// Encode this index into the canonical `analogues.bin` binary format.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_BYTES + self.records.len() * RECORD_BYTES);
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&(HEADER_BYTES as u16).to_le_bytes());
        out.extend_from_slice(
            &(u32::try_from(self.records.len()).unwrap_or(u32::MAX)).to_le_bytes(),
        );
        out.push(FEATURES as u8);
        out.extend_from_slice(&[0u8; 3]);
        for v in self.mean {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in self.inv_std {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in self.weight {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.close_threshold.to_le_bytes());
        out.extend_from_slice(&self.novel_threshold.to_le_bytes());
        debug_assert_eq!(out.len(), HEADER_BYTES);
        for rec in &self.records {
            out.extend_from_slice(&rec.lat_e2.to_le_bytes());
            out.extend_from_slice(&rec.lon_e2.to_le_bytes());
            out.extend_from_slice(&rec.iso_a2.to_le_bytes());
            out.extend_from_slice(&rec.wgsrpd_l3.to_le_bytes());
            out.push(rec.koppen);
            out.push(rec.flags);
            for f in rec.features {
                out.extend_from_slice(&f.to_le_bytes());
            }
        }
        out
    }

    /// Search directly over raw `analogues.bin` bytes without materializing all
    /// records into a `Vec<AnalogueRecord>`. Produces the exact same result as
    /// `AnalogueIndex::from_bytes(bytes)?.search(query, opts)` with lower
    /// allocation and latency (ideal for single-shot WASM calls).
    ///
    /// # Errors
    /// Returns [`AnalogueError`] if the header is invalid or the buffer is
    /// truncated.
    pub fn search_bytes(
        bytes: &[u8],
        query: &AnalogueQuery,
        opts: &SearchOptions,
    ) -> Result<AnalogueResult, AnalogueError> {
        let hdr = parse_header(bytes)?;
        if opts.k == 0 || hdr.count == 0 {
            return Ok(AnalogueResult {
                novel_climate: true,
                best_sigma: None,
                matches: Vec::new(),
            });
        }
        let prep = QueryPrep::new(query, &hdr.mean, &hdr.inv_std, &hdr.weight);
        let mut scored: Vec<(f32, usize)> = Vec::with_capacity(hdr.count);
        for r in 0..hdr.count {
            let off = hdr.header_bytes + r * RECORD_BYTES;
            let flags = bytes[off + 9];
            let mut feats = [0i16; FEATURES];
            for (i, f) in feats.iter_mut().enumerate() {
                *f = read_i16(bytes, off + 10 + i * 2);
            }
            let sigma = prep.score(flags, &feats);
            if sigma.is_finite() {
                scored.push((sigma, r));
            }
        }
        Ok(select_deduplicated_partial(
            query,
            opts,
            &mut scored,
            hdr.close_threshold,
            hdr.novel_threshold,
            |r| decode_record_at(bytes, hdr.header_bytes + r * RECORD_BYTES),
        ))
    }

    /// Search for the top-`k` spatially deduplicated historical analogues matching `query`.
    #[must_use]
    pub fn search(&self, query: &AnalogueQuery, opts: &SearchOptions) -> AnalogueResult {
        if opts.k == 0 || self.records.is_empty() {
            return AnalogueResult {
                novel_climate: true,
                best_sigma: None,
                matches: Vec::new(),
            };
        }
        let prep = QueryPrep::new(query, &self.mean, &self.inv_std, &self.weight);
        let mut scored: Vec<(f32, usize)> = Vec::with_capacity(self.records.len());
        for (idx, rec) in self.records.iter().enumerate() {
            let sigma = prep.score(rec.flags, &rec.features);
            if sigma.is_finite() {
                scored.push((sigma, idx));
            }
        }
        select_deduplicated_partial(
            query,
            opts,
            &mut scored,
            self.close_threshold,
            self.novel_threshold,
            |idx| self.records[idx],
        )
    }
}

struct ParsedHeader {
    header_bytes: usize,
    count: usize,
    mean: [f32; FEATURES],
    inv_std: [f32; FEATURES],
    weight: [f32; FEATURES],
    close_threshold: f32,
    novel_threshold: f32,
}

fn parse_header(bytes: &[u8]) -> Result<ParsedHeader, AnalogueError> {
    if bytes.len() < HEADER_BYTES {
        return Err(AnalogueError::Truncated(format!(
            "{} bytes, need at least {HEADER_BYTES} for header",
            bytes.len()
        )));
    }
    if bytes[0..4] != MAGIC {
        return Err(AnalogueError::Header("bad magic (expected CNPA)".into()));
    }
    let version = read_u16(bytes, 4);
    if version != FORMAT_VERSION {
        return Err(AnalogueError::Header(format!(
            "unsupported version {version}, expected {FORMAT_VERSION}"
        )));
    }
    let header_bytes = usize::from(read_u16(bytes, 6));
    if header_bytes < HEADER_BYTES || bytes.len() < header_bytes {
        return Err(AnalogueError::Header(format!(
            "invalid header_bytes {header_bytes}"
        )));
    }
    let count = read_u32(bytes, 8) as usize;
    let feat = usize::from(bytes[12]);
    if feat != FEATURES {
        return Err(AnalogueError::Header(format!(
            "feature_count {feat}, expected {FEATURES}"
        )));
    }
    let need = header_bytes
        .checked_add(
            count
                .checked_mul(RECORD_BYTES)
                .ok_or_else(|| AnalogueError::Truncated("record count overflow".into()))?,
        )
        .ok_or_else(|| AnalogueError::Truncated("size overflow".into()))?;
    if bytes.len() < need {
        return Err(AnalogueError::Truncated(format!(
            "{} bytes, need {need} for {count} records",
            bytes.len()
        )));
    }
    let mut mean = [0f32; FEATURES];
    let mut inv_std = [0f32; FEATURES];
    let mut weight = [0f32; FEATURES];
    for i in 0..FEATURES {
        mean[i] = read_f32(bytes, 16 + i * 4);
        inv_std[i] = read_f32(bytes, 48 + i * 4);
        weight[i] = read_f32(bytes, 80 + i * 4);
    }
    Ok(ParsedHeader {
        header_bytes,
        count,
        mean,
        inv_std,
        weight,
        close_threshold: read_f32(bytes, 112),
        novel_threshold: read_f32(bytes, 116),
    })
}

#[inline]
fn decode_record_at(bytes: &[u8], off: usize) -> AnalogueRecord {
    let mut features = [0i16; FEATURES];
    for (i, f) in features.iter_mut().enumerate() {
        *f = read_i16(bytes, off + 10 + i * 2);
    }
    AnalogueRecord {
        lat_e2: read_i16(bytes, off),
        lon_e2: read_i16(bytes, off + 2),
        iso_a2: read_u16(bytes, off + 4),
        wgsrpd_l3: read_u16(bytes, off + 6),
        koppen: bytes[off + 8],
        flags: bytes[off + 9],
        features,
    }
}

struct QueryPrep {
    q_arid_valid: bool,
    qz: [f32; FEATURES],
    scale_inv_std: [f32; FEATURES],
    mean_z: [f32; FEATURES],
    weight: [f32; FEATURES],
    inv_w_all: f32,
    inv_w_no_arid: f32,
}

impl QueryPrep {
    fn new(
        query: &AnalogueQuery,
        mean: &[f32; FEATURES],
        inv_std: &[f32; FEATURES],
        weight: &[f32; FEATURES],
    ) -> Self {
        let q_raw = [
            query.tmean_cold_c as f32,
            query.tmean_warm_c as f32,
            query.tmin_cold_c as f32,
            query.tmax_warm_c as f32,
            (1.0 + query.precip_mm.max(0.0)).ln() as f32,
            query.aridity_index.unwrap_or(0.0) as f32,
            f32::from(query.dry_months.min(12)),
            query.summer_rain_frac.clamp(0.0, 1.0) as f32,
        ];
        let mut qz = [0f32; FEATURES];
        let mut scale_inv_std = [0f32; FEATURES];
        let mut mean_z = [0f32; FEATURES];
        let mut w_all = 0f32;
        for i in 0..FEATURES {
            qz[i] = (q_raw[i] - mean[i]) * inv_std[i];
            scale_inv_std[i] = FEATURE_SCALES[i] * inv_std[i];
            mean_z[i] = mean[i] * inv_std[i];
            w_all += weight[i];
        }
        let w_no_arid = w_all - weight[ARIDITY_IDX];
        Self {
            q_arid_valid: query.aridity_index.is_some_and(f64::is_finite),
            qz,
            scale_inv_std,
            mean_z,
            weight: *weight,
            inv_w_all: if w_all > 0.0 { 1.0 / w_all } else { 0.0 },
            inv_w_no_arid: if w_no_arid > 0.0 {
                1.0 / w_no_arid
            } else {
                0.0
            },
        }
    }

    #[inline]
    fn score(&self, flags: u8, features: &[i16; FEATURES]) -> f32 {
        let use_arid = self.q_arid_valid && (flags & FLAG_ARIDITY_VALID) != 0;
        let mut sum_d2 = 0f32;
        for (i, &feat) in features.iter().enumerate() {
            if i == ARIDITY_IDX && !use_arid {
                continue;
            }
            let cz = f32::from(feat) * self.scale_inv_std[i] - self.mean_z[i];
            let dz = self.qz[i] - cz;
            sum_d2 += self.weight[i] * dz * dz;
        }
        let inv_w = if use_arid {
            self.inv_w_all
        } else {
            self.inv_w_no_arid
        };
        if inv_w > 0.0 {
            (sum_d2 * inv_w).sqrt()
        } else {
            f32::INFINITY
        }
    }
}

fn select_deduplicated_partial<F>(
    query: &AnalogueQuery,
    opts: &SearchOptions,
    scored: &mut [(f32, usize)],
    close_threshold: f32,
    novel_threshold: f32,
    get_record: F,
) -> AnalogueResult
where
    F: Fn(usize) -> AnalogueRecord,
{
    let cmp = |a: &(f32, usize), b: &(f32, usize)| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1));
    let mut matches: Vec<AnalogueMatch> = Vec::with_capacity(opts.k);
    let round2 = |x: f64| (x * 100.0).round() / 100.0;
    let round3 = |x: f64| (x * 1000.0).round() / 1000.0;
    let round1 = |x: f64| (x * 10.0).round() / 10.0;

    let mut try_push = |sigma: f32, idx: usize| -> bool {
        let rec = get_record(idx);
        let (lat, lon) = (rec.lat(), rec.lon());
        let dist_q = haversine_km(query.lat, query.lon, lat, lon);
        if opts.exclude_radius_km > 0.0 && dist_q < opts.exclude_radius_km {
            return false;
        }
        if opts.min_distance_km > 0.0
            && matches
                .iter()
                .any(|m| haversine_km(m.lat, m.lon, lat, lon) < opts.min_distance_km)
        {
            return false;
        }
        let quality = if sigma <= close_threshold {
            MatchQuality::Close
        } else if sigma <= novel_threshold {
            MatchQuality::Moderate
        } else {
            MatchQuality::Novel
        };
        let iso = (rec.iso_a2 != NO_CODE)
            .then(|| decode_code(i16::try_from(rec.iso_a2).unwrap_or(-1), 2))
            .flatten();
        let wgs = (rec.wgsrpd_l3 != NO_CODE)
            .then(|| decode_code(i16::try_from(rec.wgsrpd_l3).unwrap_or(-1), 3))
            .flatten();
        matches.push(AnalogueMatch {
            lat: round2(lat),
            lon: round2(lon),
            iso_a2: iso,
            wgsrpd_l3: wgs,
            koppen: koppen::symbol(rec.koppen).to_string(),
            koppen_code: rec.koppen,
            sigma: round3(f64::from(sigma)),
            match_quality: quality,
            distance_km: round1(dist_q),
            delta_lat_deg: round2((query.lat.abs() - lat.abs()).abs()),
        });
        matches.len() >= opts.k
    };

    let window = (opts.k.saturating_mul(64)).clamp(128, 1024);
    if scored.len() > window {
        let (head, _, _) = scored.select_nth_unstable_by(window, cmp);
        head.sort_unstable_by(cmp);
        let mut done = false;
        for &(sigma, idx) in head.iter() {
            if try_push(sigma, idx) {
                done = true;
                break;
            }
        }
        if !done {
            let rest = &mut scored[window..];
            rest.sort_unstable_by(cmp);
            for &(sigma, idx) in rest.iter() {
                if try_push(sigma, idx) {
                    break;
                }
            }
        }
    } else {
        scored.sort_unstable_by(cmp);
        for &(sigma, idx) in scored.iter() {
            if try_push(sigma, idx) {
                break;
            }
        }
    }

    let best_sigma = matches.first().map(|m| m.sigma);
    let novel_climate = best_sigma.is_none_or(|s| s > f64::from(novel_threshold));
    AnalogueResult {
        novel_climate,
        best_sigma,
        matches,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::encode_code;

    fn make_record(
        lat: f64,
        lon: f64,
        iso: &str,
        wgs: &str,
        koppen: u8,
        raw: [f32; FEATURES],
        arid_valid: bool,
    ) -> AnalogueRecord {
        AnalogueRecord {
            lat_e2: (lat * 100.0).round() as i16,
            lon_e2: (lon * 100.0).round() as i16,
            iso_a2: u16::try_from(encode_code(iso).unwrap()).unwrap(),
            wgsrpd_l3: u16::try_from(encode_code(wgs).unwrap()).unwrap(),
            koppen,
            flags: if arid_valid { FLAG_ARIDITY_VALID } else { 0 },
            features: quantize_features(raw),
        }
    }

    fn sample_index() -> AnalogueIndex {
        let records = vec![
            // Toulouse-like (exact match for 2050 Paris)
            make_record(
                43.60,
                1.44,
                "FR",
                "FRA",
                14,
                [6.0, 22.5, 2.5, 28.5, 656.0_f32.ln(), 0.90, 1.0, 0.48],
                true,
            ),
            // Very close neighbor to Toulouse (15 km away -> should be deduplicated!)
            make_record(
                43.70,
                1.50,
                "FR",
                "FRA",
                14,
                [6.1, 22.5, 2.6, 28.4, 660.0_f32.ln(), 0.91, 1.0, 0.48],
                true,
            ),
            // Southern Hemisphere twin (Melbourne-like, phase-invariant!)
            make_record(
                -37.81,
                144.96,
                "AU",
                "VIC",
                15,
                [9.5, 20.5, 6.0, 26.0, 650.0_f32.ln(), 0.85, 1.0, 0.45],
                true,
            ),
            // Polar cell with aridity_valid = false (NODATA aridity)
            make_record(
                78.22,
                15.63,
                "SJ",
                "SVA",
                29,
                [-14.0, 5.0, -17.3, 6.0, 366.0_f32.ln(), 0.0, 0.0, 0.50],
                false,
            ),
        ];
        AnalogueIndex {
            mean: [5.0, 20.0, 0.0, 26.0, 6.5, 1.0, 2.0, 0.5],
            inv_std: [0.1, 0.125, 0.1, 0.125, 1.0, 1.5, 0.25, 4.0],
            weight: DEFAULT_WEIGHTS,
            close_threshold: DEFAULT_CLOSE_THRESHOLD,
            novel_threshold: DEFAULT_NOVEL_THRESHOLD,
            records,
        }
    }

    #[test]
    fn round_trips_binary_format() {
        let idx = sample_index();
        let bytes = idx.to_bytes();
        assert_eq!(bytes.len(), HEADER_BYTES + idx.records.len() * RECORD_BYTES);
        let decoded = AnalogueIndex::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, idx);
        assert!(AnalogueIndex::from_bytes(&bytes[..20]).is_err());
    }

    #[test]
    fn search_deduplicates_handles_polar_and_flags_novel_climate() {
        let idx = sample_index();
        let query = AnalogueQuery {
            lat: 48.86,
            lon: 2.35,
            tmean_cold_c: 6.0,
            tmean_warm_c: 22.5,
            tmin_cold_c: 2.5,
            tmax_warm_c: 28.5,
            precip_mm: 655.0,
            aridity_index: Some(0.90),
            dry_months: 1,
            summer_rain_frac: 0.48,
        };
        let res = idx.search(&query, &SearchOptions::default());
        assert!(!res.novel_climate);
        assert_eq!(res.matches[0].iso_a2.as_deref(), Some("FR"));
        assert_eq!(res.matches[0].match_quality, MatchQuality::Close);
        // The 15 km neighbor of Toulouse must be deduplicated under 100 km min_distance_km.
        assert_eq!(res.matches.len(), 3);
        assert_eq!(res.matches[1].iso_a2.as_deref(), Some("AU"));

        // Polar query with aridity_index = None must match Svalbard cleanly without NaN.
        let polar_q = AnalogueQuery {
            lat: 78.0,
            lon: 15.0,
            tmean_cold_c: -14.0,
            tmean_warm_c: 5.0,
            tmin_cold_c: -17.3,
            tmax_warm_c: 6.0,
            precip_mm: 365.0,
            aridity_index: None,
            dry_months: 0,
            summer_rain_frac: 0.50,
        };
        let polar_res = idx.search(&polar_q, &SearchOptions::default());
        assert_eq!(polar_res.matches[0].iso_a2.as_deref(), Some("SJ"));
        assert!(polar_res.matches[0].sigma < 0.05);

        // Extreme 2050 lowland tropics with no historical twin -> novel_climate = true.
        let novel_q = AnalogueQuery {
            lat: 0.0,
            lon: -60.0,
            tmean_cold_c: 35.0,
            tmean_warm_c: 44.0,
            tmin_cold_c: 32.0,
            tmax_warm_c: 52.0,
            precip_mm: 5500.0,
            aridity_index: Some(2.5),
            dry_months: 0,
            summer_rain_frac: 0.90,
        };
        let novel_res = idx.search(&novel_q, &SearchOptions::default());
        assert!(novel_res.novel_climate);
        assert_eq!(novel_res.matches[0].match_quality, MatchQuality::Novel);
    }

    #[test]
    fn search_20k_records_scales_cleanly() {
        let mut idx = sample_index();
        let base = idx.records[0];
        idx.records.clear();
        idx.records.reserve(20_000);
        for i in 0..20_000u32 {
            let mut r = base;
            let lat = -55.0 + f64::from(i % 220) * 0.5;
            let lon = -179.0 + f64::from(i / 220) * 3.5;
            r.lat_e2 = (lat * 100.0).round() as i16;
            r.lon_e2 = (lon * 100.0).round() as i16;
            r.features[0] = ((i % 400) as i16) - 100;
            r.features[1] = ((i % 350) as i16) + 50;
            idx.records.push(r);
        }
        let query = AnalogueQuery {
            lat: 48.86,
            lon: 2.35,
            tmean_cold_c: 6.0,
            tmean_warm_c: 22.5,
            tmin_cold_c: 2.5,
            tmax_warm_c: 28.5,
            precip_mm: 655.0,
            aridity_index: Some(0.90),
            dry_months: 1,
            summer_rain_frac: 0.48,
        };
        let res = idx.search(&query, &SearchOptions::default());
        assert_eq!(res.matches.len(), 5);
        assert!(res.matches[0].sigma <= res.matches[4].sigma);
        let bytes = idx.to_bytes();
        let res_bytes = AnalogueIndex::search_bytes(&bytes, &query, &SearchOptions::default())
            .expect("search_bytes succeeds");
        assert_eq!(res_bytes, res);
    }

    #[test]
    fn haversine_handles_antimeridian_cleanly() {
        let d = haversine_km(0.0, 179.9, 0.0, -179.9);
        assert!((d - 22.24).abs() < 0.5, "got {d} km");
    }
}
