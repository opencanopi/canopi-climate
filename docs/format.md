# Tile format contract (normative)

- Status: accepted (2026-11-12), crate 0.2.1, `format_version` 1.
- This document is the crate's architecture decision for the published grid: what a release
  contains and how a reader must interpret it. It is normative. The reasons behind the choices are
  in [0001](decisions/0001-tile-format.md) (container), [0002](decisions/0002-koppen-class-vs-monthly-means.md)
  (Köppen class) and [0003](decisions/0003-site-layer-formats.md) (site layers).
- Machine-readable companions: `tests/fixtures/synthetic-expected.json` and `vectors/lookup.json`
  ([Vectors](#vectors)). Where this text and the vectors disagree, the vectors describe what the
  code does, and the disagreement is a bug in one of them.
- MUST, MUST NOT and MAY are used as in RFC 2119. "Reader" means any implementation: this crate, its
  WebAssembly build, or a port (the website's TypeScript reader `tiff.ts`).

## Release layout

A grid release is one tar (`canopi-climate-grid-vX.Y.Z.tar`) holding:

| Path | Content |
|---|---|
| `manifest.json` | The grid description below; its SHA-256 identifies the grid. |
| `tiles/<name>.tif` | Climate tiles, one per 10° tile with land. |
| `soil/<name>.tif`, `regions/<name>.tif` | Site-layer tiles (`layers` in the manifest). |
| `ATTRIBUTION.md` | Sources, licences, citations. |
| `build-report.json` | The build's checks and counts (land cells, class/means contradictions, clamped extremes, source comparisons; `layers`: soil fill by distance, region codes). Informative: a reader does not need it. |
| `SHA256SUMS` | `sha256sum` lines for `manifest.json`, `build-report.json`, `ATTRIBUTION.md` and every tile. |

A tile with no data is absent, not empty. A reader MUST treat an absent tile (listed nowhere in the
manifest) as "no data here", not as an error.

### Manifest

`format` is `"canopi-climate-grid"` and `format_version` is `1`. A reader MUST refuse any other
`format` or `format_version`. A reader MUST ignore manifest fields it does not know: `layers` was
added in 0.2.0 without a version change, and a 0.1.0 reader still reads the release
(`tests/manifest_compat.rs`).

Each tile entry (`tiles[]`, `layers[].tiles[]`) has `name`, `path`, `bytes` and `sha256`. Climate tile
entries also have `complete_cells`. A reader that fetches tiles SHOULD check `bytes` and MAY check
`sha256`; `canopi-climate verify-grid` checks both and decodes every tile.

## Container

Every tile, climate or layer, is a Cloud Optimized GeoTIFF with exactly this layout. A reader MUST
refuse a file that breaks any rule (`TileError::Layout`, or `TileError::Tiff` when the bytes are not a
readable TIFF or reach past the end of the file), and a writer MUST NOT produce one:

| Rule | Value |
|---|---|
| Byte order and variant | Little-endian classic TIFF (`II*\0`). Big-endian (`MM`) and BigTIFF (`II+\0`) are refused. |
| Images | Exactly one IFD (no overviews, no masks). |
| Bounds | The IFD, the values of every IFD entry (whatever its tag) and the tile data lie inside the file; anything past the end is `TileError::Tiff`. Value sizes follow TIFF 6.0 and BigTIFF (an unknown type counts one byte per value); the website's reader knows only BYTE, SHORT, LONG, DOUBLE and LONG8 sizes and counts the rest as one byte, so the crate may refuse an out-of-line value the site never reads (see below). |
| Field types | `ImageWidth`, `ImageLength`, `BitsPerSample`, `Compression`, `SamplesPerPixel`, `PlanarConfiguration`, `Predictor`, `TileWidth`, `TileLength`, `TileOffsets`, `TileByteCounts` and `SampleFormat` are SHORT or LONG; `ModelPixelScale` and `ModelTiepoint` are DOUBLE. Other entries may have any type. |
| Size | `side × side` pixels: 100 for climate tiles, 200 for layer tiles. |
| Samples | `SamplesPerPixel` present (no default) and = the band count; `BitsPerSample` 16 for every sample; `SampleFormat` present and 2 (signed integer) for every sample, given once or per band. |
| Interleave | `PlanarConfiguration` absent or 1 (pixel-interleaved). |
| Compression | `Compression` 8 or 32946 (Deflate). |
| Predictor | `Predictor` absent or 1 (none). |
| Internal tiling | One internal tile covering the image: `TileWidth` and `TileLength` ≥ `side`, one `TileOffsets` and one `TileByteCounts` entry. The writer uses 112 (climate) and 208 (layers), the smallest multiples of 16 above `side`. |
| No data | −32768 (`NODATA`). |

`wbgeotiff`'s `CogWriter` writes the tiles; the `tiff` crate reads them, natively and in WebAssembly.
The committed fixture `tests/fixtures/synthetic-n50e000.tif` pins the layout, and the tests
`refuses_what_the_website_reader_refuses`, `refuses_what_the_website_reader_refuses_in_the_ifd` and
`accepts_a_per_band_sample_format` (`src/tile.rs`) cover each rule. The crate refuses everything the
website's reader (`tiff.ts` `readTileHeader`) refuses; it is stricter in three places, so a file both
accept is always a valid tile: it requires every `SampleFormat` value to be 2 (the site reads the
first), and it refuses SHORT or LONG georeferencing (the site also reads them as numbers), and it sizes RATIONAL and other wide types by their real width, so an out-of-line value past the end of the file is refused where the site, counting one byte, finds it inline.

## Georeferencing and grid

| | Climate | Layers (`soil`, `regions`) |
|---|---|---|
| CRS | EPSG:4326 (latitude/longitude, degrees) | same |
| Cell | 0.1° | 0.05° |
| Tile | 10° × 10°, 100 × 100 cells | the same 10° tiles, 200 × 200 cells |
| Global grid | 3600 × 1800 cells, 36 × 18 tiles | 7200 × 3600 cells |

- **Tile names** are the tile's north-west corner: `n50e000` covers 40–50° N, 0–10° E; `s10w080`
  covers 10–20° S, 70–80° W. Latitude is two digits after `n`/`s`, longitude three after `e`/`w`.
- **Raster origin.** A tile's `ModelPixelScale` MUST be the cell size in x and y (to within 1e-9°)
  and its `ModelTiepoint` MUST put the raster's top-left corner on the tile's north-west corner (to
  within 1e-6°). The tile identity comes from the georeferencing, not from the file name; a reader
  refuses anything else with `TileError::Georef`. The GeoKey directory says EPSG:4326 but is not read.
- **Cell edges.** Column 0 starts at 180° W, row 0 at 90° N. A cell covers longitudes `[west, west +
  cell)` and latitudes `(north − cell, north]`: west and north edges are inclusive. Latitude −90
  belongs to the last row; longitude 180 wraps to −180, and any longitude is taken modulo 360.
  Index computation adds 1e-9 before flooring, so 48.9 maps as written despite binary rounding.
- **Valid points.** A latitude outside [−90, 90] or a non-finite coordinate is invalid
  (`LookupError::InvalidCoordinate`).
- **Pixel order.** Row-major from the north-west corner; within a pixel, bands in the order below.
  Sample `i` of cell `c` is at `data[c * bands + i]`.

A layer tile nests exactly in the climate tile of the same name: each climate cell holds 2 × 2
layer cells.

## Bands

Every stored value is an `int16` `q`. `q = −32768` means no value. Otherwise the decoded value is
`q × scale + offset` in the band's unit, computed as in [Encodings](#encodings).

### Climate tiles: 72 bands

8 variables × 9 slots. Band (0-based) = `variable_index × 9 + slot_index`.

| # | Variable id | Unit | Scale | Content |
|---|---|---|---|---|
| 0 | `koppen` | class | 1 | Köppen-Geiger code 1–30 ([legend](#köppen-legend)) |
| 1 | `tmin_cold` | °C | 0.1 | mean daily minimum of the coldest month |
| 2 | `tmax_warm` | °C | 0.1 | mean daily maximum of the warmest month |
| 3 | `tmean_cold` | °C | 0.1 | mean temperature of the coldest month |
| 4 | `tmean_warm` | °C | 0.1 | mean temperature of the warmest month |
| 5 | `precip` | mm | 1 | annual precipitation |
| 6 | `aridity` | 1 | 0.01 | precipitation / Thornthwaite PET, capped at 300; no data where PET < 1 mm/year |
| 7 | `dry_months` | months | 1 | months with precipitation (mm) < 2 × mean temperature (°C), 0–12 |

Every offset is 0. Slots, in order:

| # | Period | Scenario |
|---|---|---|
| 0 | 1991-2020 | historical |
| 1–4 | 2041-2070 | ssp126, ssp245, ssp370, ssp585 |
| 5–8 | 2071-2099 | ssp126, ssp245, ssp370, ssp585 |

The manifest's `variables` and `slots` repeat these lists with descriptions and sources. A reader
MUST check them against its own copy (id, unit, scale, offset) and refuse a mismatch.

### Soil layer (`soil/`): 4 bands

| # | Band id | Stored | Unit after decoding | Scale |
|---|---|---|---|---|
| 0 | `ph_h2o` | pH × 10 | pH | 0.1 |
| 1 | `sand` | g/kg | % of the fine earth | 0.1 |
| 2 | `clay` | g/kg | % of the fine earth | 0.1 |
| 3 | `fill_distance` | cells | cells | 1 |

Values are the 0–30 cm depth-weighted mean (weights 5, 10, 15) of SoilGrids 2.0. `fill_distance`
is 0 for a measured cell and 1–8 for a land cell without a SoilGrids value that took the values of the
nearest measured cell that far away.

### Regions layer (`regions/`): 2 bands

| # | Band id | Code | Stored |
|---|---|---|---|
| 0 | `wgsrpd_l3` | WGSRPD level-3 area, three letters | `a × 676 + b × 26 + c`, 0–17575 |
| 1 | `iso_a2` | ISO 3166-1 alpha-2 (Natural Earth admin-0 1:50m), two letters | `a × 26 + b`, 0–675 |

Letters are `A` = 0 … `Z` = 25. The scale is 1 and the unit `code`.

## Encodings

- **Quantisation (writer).** `q = round((value − offset) / scale)`, clamped to −32767…32767;
  non-finite values become −32768. So a valid value is never stored as `NODATA`.
- **Decoding (reader).** `NODATA` decodes to no value. If `1 / scale` rounds to an integer `n` with
  `|n × scale − 1| < 1e-6` (a decimal scale such as 0.1 or 0.01), the value is `q / n + offset`;
  otherwise `q × scale + offset`. Dividing makes −123 at scale 0.1 exactly the `f64` nearest −12.3,
  which prints as `-12.3` in JSON and JavaScript (`model::decode_value`; the `decode` table).
- **Codes.** A region code decodes to letters only when `0 ≤ q < 26^letters`; anything else
  (including `NODATA` and negative values) is no code (the `codes.decode` table). Encoding accepts
  1–3 upper-case ASCII letters only (the `codes.encode` table).
- **Köppen codes** are stored raw. A code outside 1–30 has the symbol `""`; the build report counts
  such cells apart from missing ones (`cells_with_unknown_class_code`), and a release has none.

## Köppen legend

The Beck et al. (2023) legend (code → symbol; 0 is unused):

| 1 Af | 2 Am | 3 Aw | 4 BWh | 5 BWk | 6 BSh | 7 BSk | 8 Csa | 9 Csb | 10 Csc |
|---|---|---|---|---|---|---|---|---|---|
| 11 Cwa | 12 Cwb | 13 Cwc | 14 Cfa | 15 Cfb | 16 Cfc | 17 Dsa | 18 Dsb | 19 Dsc | 20 Dsd |
| 21 Dwa | 22 Dwb | 23 Dwc | 24 Dwd | 25 Dfa | 26 Dfb | 27 Dfc | 28 Dfd | 29 ET | 30 EF |

**Temperature margin.** `koppen_temperature_margin_c` is how far (°C) the slot's coldest- and
warmest-month means lie outside the class's temperature bounds, 0 when consistent (Beck et al.
2018, Table 1; temperature criteria only). Bounds: A cold ≥ 18; C 0 ≤ cold ≤ 18 and warm ≥ 10; D
cold ≤ 0 and warm ≥ 10; third letter `a` warm ≥ 22, `b`/`c` warm ≤ 22, `c` (D only) cold ≥ −38, `d`
warm ≤ 22 and cold ≤ −38; ET 0 ≤ warm ≤ 10; EF warm ≤ 0; B has no temperature bound (margin 0). The
margin is computed in `f32` from the decoded means and rounded to one decimal in `f64`; the `margin`
table holds both the raw `f32` (`margin_f32`) and the rounded value.

## Lookup semantics

A lookup answers one point. Three lookups exist (climate, soil, regions), each in two forms:

- **Cross-tile** (`lookup_across`; the CLI and the website): the search continues into
  neighbouring tiles, across the antimeridian, not across the poles.
- **Single-tile** (`lookup`; the WebAssembly build): the caller passes the point's own tile; the
  search stays inside it. A point in another tile is `LookupError::WrongTile`.

Both give the same answer whenever the cell found lies in the point's own tile; the vectors mark
such points `own_tile: true`.

**Search.** The point's own cell is used when it has data; otherwise the nearest cell with data
within the radius, in Euclidean cell distance (`dx² + dy² ≤ r²`); otherwise no answer. Ties go to the
northern, then the western cell (cells are visited north to south, west to east, and only a strictly
closer cell replaces the best). `distance_cells` is `sqrt(dx² + dy²)`. Cell centres in the output are
rounded to 6 decimals.

| Lookup | Radius | "Has data" |
|---|---|---|
| Climate | 2 cells (0.1°) | every one of the 72 bands is set, except that the 9 `aridity` bands MAY be `NODATA` |
| Soil | 2 cells (0.05°) | `ph_h2o`, `sand` and `clay` are set |
| Regions | 2 cells (0.05°), each code searched on its own | that code's band is set |

**Climate answer**: `lat`, `lon`, `cell_lat`, `cell_lon`, `distance_cells`, `tile`, and `periods`: one
entry per slot in slot order with `period`, `scenario`, `koppen`, `koppen_code`,
`koppen_temperature_margin_c`, `tmin_cold_c`, `tmax_warm_c`, `tmean_cold_c`, `tmean_warm_c`,
`precip_mm`, `aridity_index` (null when not stored) and `dry_months` (clamped to 0–12). No complete
cell within the radius: no answer (`null`).

**Soil answer**: the cell fields above plus `depth` (`"0-30 cm"`), `ph_h2o`, `sand_pct`, `clay_pct`,
`silt_pct` = `round1(max(0, 100 − sand − clay))`, `texture` (the USDA class id, see the `texture`
table; null outside the triangle), `filled` (`fill_distance > 0`) and `fill_distance_cells`. No soil
within the radius, or a grid whose manifest lists no `soil` layer: `null`.

**Regions answer**: `wgsrpd_l3`, `iso_a2`, `wgsrpd_l3_distance_cells`, `iso_a2_distance_cells` (each
null when that code is not found) and `tile` (the point's own tile). Codes only, no names. On a grid
whose manifest lists a `regions` layer the answer is always an object, even where no code is found;
on a grid without one (a 0.1.0 grid) it is `null` (`Grid::site`, `vectors::site`). The WebAssembly
`lookupRegions` takes a regions tile, so it always returns an object; the host passes `null` itself
when it has no tile.

## Errors

| Kind | When | Reader behaviour |
|---|---|---|
| `TileError::Tiff` | The bytes are not a readable TIFF, or the IFD, an entry's values or the tile data lie past the end of the file. | Refuse the tile. |
| `TileError::Layout` | A [Container](#container) rule is broken, or the band count or sample type is wrong. | Refuse the tile. |
| `TileError::Georef` | Missing or short `ModelPixelScale`/`ModelTiepoint`, wrong pixel size, or an origin that is not a tile corner. | Refuse the tile. |
| `LookupError::InvalidCoordinate` | Non-finite coordinate or latitude outside [−90, 90]. | No lookup. |
| `LookupError::WrongTile` | Single-tile lookup with another tile's bytes. | No lookup (caller bug). |
| no answer (`None`, `null`) | No cell with data within the radius. | Not an error: "no data here". |

The WebAssembly functions throw a `JsError` with the error's message for the first five rows and
return `null` for the last.

## Reserved: monthly mask bits (v0.3)

The v0.3 monthly layer (plan step C3) will store a per-slot month mask in an `int16` band:

| Bits | Meaning |
|---|---|
| 0–11 | January … December: the month is dry (Gaussen: precipitation in mm < 2 × mean temperature in °C) |
| 12–14 | Reserved ("water across the year", decided in the website's F5 design check). Writers MUST write 0; readers MUST ignore them. |
| 15 | Never set, so a mask is never negative and never equals `NODATA`. |

For every land cell and slot, `popcount(mask & 0x0FFF)` MUST equal the climate tile's `dry_months`.
Nothing in format 1 uses these bits yet; they are fixed now so a reader written today keeps working.

## Compatibility rule

| Change | Allowed in | Requirement |
|---|---|---|
| New optional manifest field, new layer, new CLI flag or subcommand with output unchanged without it | patch (0.2.x) | `format_version` stays 1; readers ignore what they do not know; vectors unchanged byte for byte except `cli`. |
| New grid built with the same format (new sources, fixes) | minor (0.x.0) | New grid release and manifest SHA-256; vectors regenerated and reviewed. |
| Any change to the container rules, georeferencing, cell edges, band order, a band's unit, scale or offset, the Köppen legend, the lookup search, or an answer's field | minor (0.x.0) with `format_version` + 1 | This document updated first; old readers refuse the new manifest by its version. |

A crate release that keeps the grid (0.2.1) names the grid release it was tested against in
`release/grid.json`; tiles are not re-uploaded.

## Vectors

| File | Grid | Made by | Checked by |
|---|---|---|---|
| `tests/fixtures/synthetic-expected.json` | the committed synthetic tiles `tests/fixtures/synthetic{,-soil,-regions}-n50e000.tif` (written by `tile::synthetic` and `layer::synthetic`) | `UPDATE_FIXTURES=1 cargo test` | plain `cargo test` (stale file fails); the wasm smoke runs every `own_tile` point through the WebAssembly build |
| `vectors/lookup.json` | the release grid in `release/grid.json` | `canopi-climate vectors --grid <dir> --points vectors/points.csv -o vectors/lookup.json` | CI's grid job (`--check`, ignoring `cli`); attached to each release by `release.yml` |

Both hold `points` (`name`, `lat`, `lon`, `own_tile`, and `expect`: the climate, soil and regions
answers, or `"invalid"`) and `tables`: `legend`, `margin`, `texture`, `codes` (`encode`, `decode`) and
`decode` (scale decoding). `vectors/lookup.json` has the website goldens' shape (`cli`,
`grid_manifest_sha256`, `points`) plus `own_tile` and `tables`. A port passes when it reproduces
every `expect` its lookup form can answer and every table row.
