# 0001: Tiles are Cloud Optimized GeoTIFFs

- Status: accepted (2026-11-11)
- Context: consortium v2 plan, phase P3a; ADR "consortium v2 architecture" §5 and §7 (website repository).
- Contract: the normative format these decisions produce is [docs/format.md](../format.md).

## Question

How is one 10° × 10° tile stored? Each tile holds 100 × 100 cells × 72 values
(8 variables × 9 period/scenario slots). The candidates were a Cloud Optimized
GeoTIFF written with `wbgeotiff`, and CCT1, the prototype's own compact binary
format. The brief said to choose COG unless its cost is clearly unreasonable.

## Measurements

Both formats were measured on the same 1991-2020 + 8 future slots and the
same land mask (2,211,066 land cells, 464 tiles with land).

| | CCT1 (prototype) | COG, internal tile 112 (chosen) | COG, internal tile 128 |
|---|---|---|---|
| Total size | 47.8 MiB | 73.8 MiB (77,387,293 B) | 78.0 MiB |
| Median tile | 90 KB | 125,488 B | n/a |
| Largest tile | 349 KB | 601,316 B | 623,466 B |
| Opens in QGIS/GDAL | no | yes | yes |

COG layout: int16, pixel-interleaved, Deflate, nodata −32768, EPSG:4326, one
internal tile, no overviews. With 112, the smallest multiple of 16 that holds 100
cells, the whole tile is a single compressed block. With 128 the padding costs
4.2 MiB.

The browser reader (wasm32, `opt-level = "z"`, LTO, after `wasm-bindgen`):

| Reader | wasm | gzip |
|---|---|---|
| CCT1 lookup (prototype) | 60 KB | 25.6 KB |
| `tiff` crate 0.11.3, Deflate only (bare decode) | 160,511 B | 58,679 B |
| `wbgeotiff` reader (bare decode) | 1,415,010 B | 494,408 B |
| This crate, `wasm` feature (`tiff` + lookup + Köppen + serde glue) | 357,683 B | 96,562 B |

Both readers decode the tiles identically. `wbgeotiff` writes the tiles, and its
reader is used in native tests to check that the two agree.

## Decision

Use COG for the tiles, written with `wbgeotiff`'s `CogWriter` and read with the
`tiff` crate in the library and in wasm.

COG costs 1.54 times the storage (+26 MiB on a static host) and about 35 KB more
per lookup (median tile). The wasm module is about 71 KB bigger gzipped. None of
this is clearly unreasonable for a page that fetches one tile per lookup. In
return:

- the format is a standard: the tiles carry their georeferencing and open in
  QGIS, GDAL and rasterio;
- there is no custom binary format to specify, version and maintain;
- the writer comes from the GeoLibre/whitebox ecosystem and the reader is a
  mature crate.

## Consequences

- `wbgeotiff` (rev 9c0ff4f) writes no GDAL metadata. Band names, scale and
  offset live in `manifest.json` only: GIS users must read the manifest to
  interpret the bands. See `docs/upstream-gaps.md`.
- `wbgeotiff` writes no horizontal-differencing predictor. A predictor would
  likely shrink the tiles; this is revisited if upstream adds one.
- The `tiff` crate is the only decoder in the wasm build. A change of COG
  layout (planar, other compression) needs a reader test. The fixture
  `tests/fixtures/synthetic-n50e000.tif` pins the current layout.
