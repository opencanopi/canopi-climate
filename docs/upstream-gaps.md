# Upstream gaps in the whitebox crates and proj4rs

The pipeline uses `wbgeotiff` and `wbhdf` from
[whitebox-wasm](https://github.com/opengeos/whitebox-wasm) at rev
`9c0ff4fdf3513f27b89c78e294610c3b418b3a4f`. The gaps below are worked around
in this crate, each in one module. Contributing the fixes upstream is a
follow-up; once they are merged, the workaround can go.

## wbgeotiff

| Gap | Effect here | Workaround |
|---|---|---|
| `GeoTiff::open` reads the whole file and decodes whole bands | One 43200 × 20880 CHELSA band is 3.6 GB as `f32` | `src/pipeline/tiff_rows.rs` parses the IFD with `wbgeotiff::ifd::TiffReader` and decodes one strip, or one row of internal tiles, at a time with `wbgeotiff::compression::{decompress, undo_predictor}` |
| `CogWriter` writes no GDAL metadata (`GDAL_METADATA` band names, scale, offset) | GIS software shows raw int16 bands | `manifest.json` lists the variables, units, scale/offset and band order |
| `CogWriter` writes no horizontal-differencing predictor | Tiles are larger than they could be | None; size measured in `docs/decisions/0001-tile-format.md` |
| The reader is large in wasm (1,415,010 B vs 160,511 B for the `tiff` crate) | Too heavy for a browser lookup | The library and wasm read tiles with the `tiff` crate; `wbgeotiff` writes them and checks them in native tests |

## wbhdf

| Gap | Effect here | Workaround |
|---|---|---|
| The v2 object-header walker stops after the first message when the header tracks attribute creation order (flag bit 2, set by netCDF-4) | Beck's NetCDF variables are not found | `src/pipeline/netcdf.rs` walks v2 messages itself, honouring flag bit 2 and following `OCHK` continuation blocks |
| No layout v3 or filter-pipeline v2 parsing for v2 headers; no dataspace, datatype or fill-value helpers | Chunk geometry and filters unknown | Parsed in `netcdf.rs` |
| No shuffle filter | Chunks decode to scrambled bytes | `netcdf::unshuffle` |
| Chunk assembly assumes 2-D chunks | Beck's variables are 12 × 1800 × 3600 | N-D chunk placement in `netcdf::place_chunk` |
| `read_chunked_storage_records_bounded_in_file` returns each leaf record many times (34,119 records for 1,944 chunks) and re-reads the whole file for every B-tree node | Decoding every record of one variable took more than 5 GB; the first capped build run was killed for out of memory | `netcdf::unique_chunks` keeps one record per chunk offset and fails when a chunk is listed at two addresses |
| Dataset names are not resolved from link messages | Variables have no names | `netcdf::link_name_for` scans the link messages for the header address |

## proj4rs

`proj4rs` 0.3.0 (pinned) has no Interrupted Goode Homolosine (`+proj=igh`),
the projection of SoilGrids 2.0.

| Gap | Effect here | Workaround |
|---|---|---|
| No `igh` projection | SoilGrids pixels cannot be located | `src/pipeline/igh.rs` builds the twelve lobes from `proj4rs`'s `sinu` and `moll`, as PROJ's `igh.cpp` does; tested against PROJ 9 in every lobe (within 1 mm). Forward only. |
