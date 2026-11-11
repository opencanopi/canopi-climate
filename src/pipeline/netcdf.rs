//! NetCDF-4 (HDF5) reading for the Beck et al. (2023) monthly climatologies.
//!
//! `wbhdf` (9c0ff4f) provides object-header discovery
//! (`object_header::ObjectHeader`), the v1 chunk B-tree walk
//! (`btree::read_chunked_storage_records_bounded_in_file`) and zlib
//! (`filters::decompress_zlib`). It stops after the first message of a v2
//! object header that tracks attribute creation order (flag bit 2, set by
//! netCDF), has no v3 layout or v2 filter-pipeline parsing for v2 headers, no
//! shuffle filter and assumes 2-D chunks. This module adds those pieces for
//! `f32` chunked variables (docs/upstream-gaps.md):
//!
//! - a v2 message walker that honours flag bit 2 and follows `OCHK`
//!   continuation blocks;
//! - dataspace, datatype, fill value, layout v3 and filter pipeline messages;
//! - the dataset name, found by scanning link messages for the header address;
//! - shuffle, and N-D chunk assembly (chunks decoded in parallel).

use std::path::Path;

use rayon::prelude::{IntoParallelRefIterator, ParallelIterator};
use wbhdf::btree::{ChunkedStorageLeafRecord, read_chunked_storage_records_bounded_in_file};
use wbhdf::filters::decompress_zlib;
use wbhdf::object_header::{ObjectHeader, ObjectHeaderMessageHeader};

use super::{PipelineError, Result};

/// A chunked variable.
#[derive(Debug, Clone, PartialEq)]
pub struct NcVar {
    /// Dataset name (`@<offset>` when no link names it).
    pub name: String,
    /// Dimension sizes, slowest first.
    pub dims: Vec<u64>,
    /// Element size in bytes.
    pub elem_size: u32,
    /// HDF5 datatype class (1 = floating point).
    pub dtype_class: u8,
    /// Chunk sizes (one more entry than `dims`: the element size).
    pub chunk_dims: Vec<u32>,
    /// Address of the chunk B-tree.
    pub btree_address: u64,
    /// Filter ids in pipeline order (1 deflate, 2 shuffle).
    pub filters: Vec<u16>,
    /// Fill value for `f32` data.
    pub fill: Option<f32>,
}

fn err(msg: impl Into<String>) -> PipelineError {
    PipelineError::NetCdf(msg.into())
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn usize_at(b: &[u8], at: usize) -> Option<usize> {
    usize::try_from(u64_at(b, at)?).ok()
}

/// Names of links pointing at `addr`: a link message stores
/// `... name_length(u8) name address(u64)` for short names.
fn link_name_for(bytes: &[u8], addr: usize) -> Option<String> {
    let pattern = u64::try_from(addr).ok()?.to_le_bytes();
    let mut from = 0;
    while let Some(p) = bytes.get(from..)?.windows(8).position(|w| w == pattern) {
        let at = from + p;
        for len in 1..64usize {
            if at < len + 1 {
                break;
            }
            if usize::from(bytes[at - len - 1]) == len {
                let name = &bytes[at - len..at];
                if name.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_') {
                    return Some(String::from_utf8_lossy(name).into_owned());
                }
            }
        }
        from = at + 1;
    }
    None
}

/// Messages of a v2 object header at `off`, continuation blocks included.
fn v2_messages(bytes: &[u8], off: usize) -> Option<Vec<ObjectHeaderMessageHeader>> {
    let flags = *bytes.get(off + 5)?;
    let mut p = off + 6;
    if flags & 0x20 != 0 {
        p += 16; // access, modification, change, birth times
    }
    if flags & 0x10 != 0 {
        p += 4; // attribute phase change values
    }
    let size_len = 1usize << (flags & 3);
    let mut chunk = 0usize;
    for i in 0..size_len {
        chunk |= usize::from(*bytes.get(p + i)?) << (8 * i);
    }
    p += size_len;
    let tracks_order = flags & 0x04 != 0;
    let hdr = 4 + if tracks_order { 2 } else { 0 };
    let mut out = Vec::new();
    let mut spans = vec![(p, p + chunk)];
    while let Some((mut q, end)) = spans.pop() {
        // The last 4 bytes of a block are its checksum.
        while q + hdr <= end.saturating_sub(4) {
            let type_id = *bytes.get(q)?;
            let size = u16_at(bytes, q + 1)?;
            let m = ObjectHeaderMessageHeader {
                type_id,
                size,
                flags: *bytes.get(q + 3)?,
                header_offset: q,
                data_offset: q + hdr,
            };
            if type_id == 0x10 {
                let a = usize_at(bytes, q + hdr)?;
                let l = usize_at(bytes, q + hdr + 8)?;
                if bytes.get(a..a + 4)? == b"OCHK" {
                    spans.push((a + 4, a + l));
                }
            }
            out.push(m);
            q += hdr + usize::from(size);
        }
    }
    Some(out)
}

fn body<'a>(bytes: &'a [u8], m: &ObjectHeaderMessageHeader) -> &'a [u8] {
    let end = (m.data_offset + usize::from(m.size)).min(bytes.len());
    bytes.get(m.data_offset..end).unwrap_or(&[])
}

fn parse_dataspace(b: &[u8], v: &mut NcVar) {
    let (Some(&ver), Some(&rank)) = (b.first(), b.get(1)) else {
        return;
    };
    let start = if ver == 2 { 4 } else { 8 };
    let dims: Option<Vec<u64>> = (0..usize::from(rank))
        .map(|i| u64_at(b, start + i * 8))
        .collect();
    if let Some(d) = dims {
        v.dims = d;
    }
}

fn parse_layout(b: &[u8], v: &mut NcVar) {
    // Layout message v3, class 2 (chunked): dimensionality, B-tree address,
    // chunk sizes (u32 each, the last one being the element size).
    if b.first() != Some(&3) || b.get(1) != Some(&2) {
        return;
    }
    let Some(&nd) = b.get(2) else { return };
    let Some(addr) = u64_at(b, 3) else { return };
    let dims: Option<Vec<u32>> = (0..usize::from(nd))
        .map(|i| u32_at(b, 11 + i * 4))
        .collect();
    if let Some(d) = dims {
        v.btree_address = addr;
        v.chunk_dims = d;
    }
}

fn parse_filters(b: &[u8], v: &mut NcVar) -> Option<()> {
    let (ver, n) = (*b.first()?, usize::from(*b.get(1)?));
    let mut p = if ver == 1 { 8 } else { 2 };
    for _ in 0..n {
        let id = u16_at(b, p)?;
        p += 2;
        let name_len = if ver == 1 || id >= 256 {
            let l = usize::from(u16_at(b, p)?);
            p += 2;
            l
        } else {
            0
        };
        p += 2; // flags
        let n_values = usize::from(u16_at(b, p)?);
        p += 2;
        p += if ver == 1 {
            name_len.div_ceil(8) * 8
        } else {
            name_len
        };
        p += n_values * 4;
        if ver == 1 && n_values % 2 == 1 {
            p += 4;
        }
        v.filters.push(id);
    }
    Some(())
}

fn parse_fill(b: &[u8], v: &mut NcVar) {
    // Fill value message v3: version, flags, then size (u32) and value if
    // flag bit 5 is set.
    if b.first() == Some(&3) && b.get(1).is_some_and(|f| f & 0x20 != 0) && u32_at(b, 2) == Some(4) {
        v.fill = b
            .get(6..10)
            .and_then(|s| s.try_into().ok())
            .map(f32::from_le_bytes);
    }
}

/// Chunked variables of rank 2 or more in a NetCDF-4 file.
///
/// # Errors
/// I/O or format errors.
pub fn list_vars(path: &Path) -> Result<Vec<NcVar>> {
    let bytes = std::fs::read(path).map_err(|e| PipelineError::io(path, e))?;
    let oh = ObjectHeader::parse(&bytes).map_err(|e| err(format!("{}: {e}", path.display())))?;
    let mut vars = Vec::new();
    for h in &oh.v2_headers {
        let mut v = NcVar {
            name: String::new(),
            dims: vec![],
            elem_size: 0,
            dtype_class: 255,
            chunk_dims: vec![],
            btree_address: 0,
            filters: vec![],
            fill: None,
        };
        let Some(messages) = v2_messages(&bytes, h.offset) else {
            continue;
        };
        for m in &messages {
            let b = body(&bytes, m);
            match m.type_id {
                1 => parse_dataspace(b, &mut v),
                3 => {
                    if let (Some(c), Some(size)) = (b.first(), u32_at(b, 4)) {
                        v.dtype_class = c & 0x0f;
                        v.elem_size = size;
                    }
                }
                5 => parse_fill(b, &mut v),
                8 => parse_layout(b, &mut v),
                11 => {
                    parse_filters(b, &mut v);
                }
                _ => {}
            }
        }
        if v.dims.len() >= 2 && v.btree_address != 0 {
            v.name = link_name_for(&bytes, h.offset).unwrap_or_else(|| format!("@{}", h.offset));
            vars.push(v);
        }
    }
    Ok(vars)
}

fn unshuffle(raw: &[u8], elem_size: usize) -> Vec<u8> {
    let n = raw.len() / elem_size;
    let mut out = vec![0u8; raw.len()];
    for b in 0..elem_size {
        for i in 0..n {
            out[i * elem_size + b] = raw[b * n + i];
        }
    }
    out[n * elem_size..].copy_from_slice(&raw[n * elem_size..]);
    out
}

/// Undo the filter pipeline of one chunk (filters applied in reverse order,
/// skipping those masked off for this chunk).
fn unfilter(mut raw: Vec<u8>, filters: &[u16], mask: u32, elem_size: usize) -> Result<Vec<u8>> {
    for (fi, &f) in filters.iter().enumerate().rev() {
        if mask & (1 << fi) != 0 {
            continue;
        }
        raw = match f {
            1 => decompress_zlib(&raw).map_err(|e| err(e.to_string()))?,
            2 => unshuffle(&raw, elem_size),
            other => return Err(err(format!("unsupported HDF5 filter {other}"))),
        };
    }
    Ok(raw)
}

/// One record per chunk. `wbhdf`'s B-tree walk (rev 9c0ff4f) returns each
/// leaf record many times (34,119 records for 1,944 chunks in Beck's files);
/// identical repeats are dropped, a chunk listed with two different
/// addresses is an error.
fn unique_chunks(
    records: Vec<ChunkedStorageLeafRecord>,
    nd: usize,
) -> Result<Vec<ChunkedStorageLeafRecord>> {
    let mut by_offset: std::collections::BTreeMap<Vec<u64>, ChunkedStorageLeafRecord> =
        std::collections::BTreeMap::new();
    for r in records {
        let key = r
            .chunk_offsets
            .get(..nd)
            .map(<[u64]>::to_vec)
            .ok_or_else(|| err("chunk record rank"))?;
        match by_offset.get(&key) {
            Some(prev)
                if prev.chunk_address != r.chunk_address || prev.chunk_size != r.chunk_size =>
            {
                return Err(err(format!("chunk {key:?} listed at two addresses")));
            }
            Some(_) => {}
            None => {
                by_offset.insert(key, r);
            }
        }
    }
    Ok(by_offset.into_values().collect())
}

/// Read a whole `f32` variable, row-major in `dims` order; cells never
/// written hold the fill value (or NaN).
///
/// # Errors
/// I/O or format errors, or a non-`f32` variable.
pub fn read_var_f32(path: &Path, v: &NcVar) -> Result<Vec<f32>> {
    if v.dtype_class != 1 || v.elem_size != 4 {
        return Err(err(format!("{}: only f32 is read", v.name)));
    }
    let nd = v.dims.len();
    if v.chunk_dims.len() < nd {
        return Err(err(format!(
            "{}: chunk rank {}",
            v.name,
            v.chunk_dims.len()
        )));
    }
    let records = read_chunked_storage_records_bounded_in_file(
        path,
        v.btree_address,
        nd + 1,
        100_000,
        10_000_000,
    )
    .map_err(|e| err(e.to_string()))?;
    let records = unique_chunks(records, nd)?;
    let bytes = std::fs::read(path).map_err(|e| PipelineError::io(path, e))?;
    let dims: Vec<usize> = v
        .dims
        .iter()
        .map(|&d| usize::try_from(d).unwrap_or(0))
        .collect();
    let total: usize = dims.iter().product();
    let cd: Vec<usize> = v.chunk_dims[..nd].iter().map(|&x| x as usize).collect();
    let chunk_elems: usize = cd.iter().product();

    let decoded: Vec<Result<(Vec<usize>, Vec<u8>)>> = records
        .par_iter()
        .map(|r| {
            let a = usize::try_from(r.chunk_address).map_err(|e| err(e.to_string()))?;
            let size = (r.chunk_size & 0xffff_ffff) as usize;
            let mask = (r.chunk_size >> 32) as u32;
            let raw = bytes
                .get(a..a + size)
                .ok_or_else(|| err(format!("chunk at {a} past end of file")))?
                .to_vec();
            let raw = unfilter(raw, &v.filters, mask, 4)?;
            if raw.len() < chunk_elems * 4 {
                return Err(err(format!(
                    "chunk too short: {} < {}",
                    raw.len(),
                    chunk_elems * 4
                )));
            }
            let off: Vec<usize> = r.chunk_offsets[..nd].iter().map(|&x| x as usize).collect();
            Ok((off, raw))
        })
        .collect();

    let mut out = vec![v.fill.unwrap_or(f32::NAN); total];
    let mut strides = vec![1usize; nd];
    for i in (0..nd - 1).rev() {
        strides[i] = strides[i + 1] * dims[i + 1];
    }
    for item in decoded {
        let (off, raw) = item?;
        place_chunk(&mut out, &raw, &off, &cd, &dims, &strides);
    }
    Ok(out)
}

/// Copy one decoded chunk into the row-major output.
fn place_chunk(
    out: &mut [f32],
    raw: &[u8],
    off: &[usize],
    cd: &[usize],
    dims: &[usize],
    strides: &[usize],
) {
    let nd = dims.len();
    let last = nd - 1;
    let outer: usize = cd[..last].iter().product();
    let mut idx = vec![0usize; nd];
    for k in 0..outer {
        let mut rem = k;
        for d in (0..last).rev() {
            idx[d] = rem % cd[d];
            rem /= cd[d];
        }
        if (0..last).any(|d| off[d] + idx[d] >= dims[d]) || off[last] >= dims[last] {
            continue;
        }
        let dst: usize = (0..last)
            .map(|d| (off[d] + idx[d]) * strides[d])
            .sum::<usize>()
            + off[last];
        let src = k * cd[last];
        let n = cd[last].min(dims[last] - off[last]);
        for j in 0..n {
            let s = (src + j) * 4;
            if let (Some(b), Some(o)) = (raw.get(s..s + 4), out.get_mut(dst + j)) {
                *o = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(off: &[u64], addr: u64) -> ChunkedStorageLeafRecord {
        ChunkedStorageLeafRecord {
            chunk_size: 100,
            chunk_offsets: off.iter().copied().chain([0]).collect(),
            chunk_address: addr,
        }
    }

    #[test]
    fn repeated_chunk_records_are_dropped() {
        let recs = vec![
            rec(&[0, 0], 10),
            rec(&[0, 200], 20),
            rec(&[0, 0], 10),
            rec(&[0, 200], 20),
        ];
        let u = unique_chunks(recs, 2).unwrap();
        assert_eq!(u.len(), 2);
        assert!(unique_chunks(vec![rec(&[0, 0], 10), rec(&[0, 0], 11)], 2).is_err());
    }

    #[test]
    fn unshuffle_restores_element_bytes() {
        let values = [1.5f32, -2.25, 1e6];
        let raw: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut shuffled = vec![0u8; raw.len()];
        for b in 0..4 {
            for i in 0..3 {
                shuffled[b * 3 + i] = raw[i * 4 + b];
            }
        }
        assert_eq!(unshuffle(&shuffled, 4), raw);
    }

    #[test]
    fn place_chunk_clips_edge_chunks() {
        // 3 x 5 array, 2 x 4 chunks: chunk at (2, 4) is mostly outside.
        let dims = [3usize, 5];
        let strides = [5usize, 1];
        let cd = [2usize, 4];
        let mut out = vec![0f32; 15];
        let raw: Vec<u8> = (1..=8).flat_map(|i| (i as f32).to_le_bytes()).collect();
        place_chunk(&mut out, &raw, &[0, 0], &cd, &dims, &strides);
        place_chunk(&mut out, &raw, &[2, 4], &cd, &dims, &strides);
        assert_eq!(&out[0..4], &[1., 2., 3., 4.]);
        assert_eq!(&out[5..9], &[5., 6., 7., 8.]);
        assert_eq!(out[14], 1.0);
        assert_eq!(out[4], 0.0);
    }

    #[test]
    fn link_names_are_found_by_address() {
        let mut b = vec![0u8; 16];
        b.push(5);
        b.extend_from_slice(b"pr_ok");
        b.extend_from_slice(&1234u64.to_le_bytes());
        assert_eq!(link_name_for(&b, 1234), Some("pr_ok".into()));
        assert_eq!(link_name_for(&b, 999), None);
    }
}
