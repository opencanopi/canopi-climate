# Tile format contract (normative)

- Status: accepted (2026-11-12; extremes layer 2026-11-18; monthly & monthly_precip layers 2026-11-22; extremes_tail, frost_calendar & analogues.bin 2026-11-23), crate 0.5.0, `format_version` 1.
- This document is the crate's architecture decision for the published grid: what a release
  contains and how a reader must interpret it. It is normative. The reasons behind the choices are
  in [0001](decisions/0001-tile-format.md) (container), [0002](decisions/0002-koppen-class-vs-monthly-means.md)
  (Köppen class), [0003](decisions/0003-site-layer-formats.md) (site layers),
  [0004](decisions/0004-monthly-and-relief-layers.md) (monthly layers and per-layer schema),
  [0005](decisions/0005-annual-extremes.md) (annual extremes) and
  [0006](decisions/0006-tail-extremes-frost-calendar-analogues.md) (tail extremes, frost calendar and 2050 Climate Twin analogues).
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
| `extremes/<name>.tif` | Annual-extremes tiles (a layer on the 0.1° climate grid, since 0.3.0). |
| `monthly/<name>.tif` | Monthly temperature and dry-month mask tiles (a layer on the 0.1° climate grid, since 0.4.0). |
| `monthly_precip/<name>.tif` | Monthly precipitation tiles (a layer on a 0.25° grid, since 0.4.0). |
| `extremes_tail/<name>.tif` | Static 1-in-10-year tail offsets, heat-stress days and recent warming trend (0.1° climate grid, since 0.5.0). |
| `frost_calendar/<name>.tif` | Static hemisphere-aware frost dates, frost-free days, frost-year % and false-spring % (0.1° climate grid, since 0.5.0). |
| `analogues.bin` | Compact binary k-NN index of historical 1991–2020 land cells for 2050 Climate Twin lookup (since 0.5.0). |
| `ATTRIBUTION.md` | Sources, licences, citations. |
| `build-report.json` | The build's checks and counts (land cells, class/means contradictions, clamped extremes, source comparisons; `layers`: soil fill by distance, region codes). Informative: a reader does not need it. |
| `SHA256SUMS` | `sha256sum` lines for `manifest.json`, `build-report.json`, `ATTRIBUTION.md`, `analogues.bin` (when present) and every tile. |

A tile with no data is absent, not empty. A reader MUST treat an absent tile (listed nowhere in the
manifest) as "no data here", not as an error.

### Manifest

`format` is `"canopi-climate-grid"` and `format_version` is `1`. A reader MUST refuse any other
`format` or `format_version`. A reader MUST ignore manifest fields it does not know: `layers` was
added in 0.2.0, `dry_month_rule`, `layers[].slots`, `layers[].version` and `layers[].bands[].months`
in 0.4.0, and `climate_tiles_sha256` and `analogues` in 0.5.0 — all optional fields added without a
version change, so a 0.1.0 reader still reads the release (`tests/manifest_compat.rs`).

When `climate_tiles_sha256` is present at the top level of the manifest, it is the lowercase hex
SHA-256 of the UTF-8 string `"canopi-climate-tiles-v1\n"` followed by one line
`"<name> <bytes> <sha256>\n"` per entry in `tiles[]`, sorted lexicographically by `name`. It stays
unchanged when additive layers are added to the same climate tiles.

When `dry_month_rule` is present at the top level of the manifest, its value for this contract is
`"gaussen_p_lt_2t"`. A reader that checks dry months SHOULD assert this value when present; a
different rule would be a new identifier and a minor release.

Each layer entry in `layers[]` declares its own geometry (`cell_deg`, `tile_cells`), band list
(`bands[]`, each with optional `months`), optional `slots[]`, optional `version`, and `encoding`
(`bands`, `band_order`). A reader takes a layer's geometry and band positions from the manifest and
checks the band ids, units, scales and offsets it knows.

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
| Size | `side × side` pixels, where `side` is the grid's or layer's `tile_cells` (100 for 0.1° tiles, 200 for 0.05° tiles, 40 for 0.25° tiles), with `tile_cells × cell_deg = 10` exactly. |
| Samples | `SamplesPerPixel` present (no default) and = the band count; `BitsPerSample` 16 for every sample; `SampleFormat` present and 2 (signed integer) for every sample, given once or per band. |
| Interleave | `PlanarConfiguration` absent or 1 (pixel-interleaved). |
| Compression | `Compression` 8 or 32946 (Deflate). |
| Predictor | `Predictor` absent or 1 (none). |
| Internal tiling | One internal tile covering the image: `TileWidth` and `TileLength` ≥ `side`, one `TileOffsets` and one `TileByteCounts` entry. The writer uses the smallest multiple of 16 ≥ `side` (112 for 100, 208 for 200, 48 for 40). |
| No data | −32768 (`NODATA`). |

`wbgeotiff`'s `CogWriter` writes the tiles; the `tiff` crate reads them, natively and in WebAssembly.
The committed fixture `tests/fixtures/synthetic-n50e000.tif` pins the layout, and the tests
`refuses_what_the_website_reader_refuses`, `refuses_what_the_website_reader_refuses_in_the_ifd` and
`accepts_a_per_band_sample_format` (`src/tile.rs`) cover each rule. The crate refuses everything the
website's reader (`tiff.ts` `readTileHeader`) refuses; it is stricter in three places, so a file both
accept is always a valid tile: it requires every `SampleFormat` value to be 2 (the site reads the
first), and it refuses SHORT or LONG georeferencing (the site also reads them as numbers), and it sizes RATIONAL and other wide types by their real width, so an out-of-line value past the end of the file is refused where the site, counting one byte, finds it inline.

## Georeferencing and grid

Every grid and layer shares the same 10° × 10° tiles (36 × 18 tiles globally, EPSG:4326), with
`tile_cells × cell_deg = 10` exactly:

| | Climate, `extremes`, `monthly`, `extremes_tail`, `frost_calendar` | `soil`, `regions` | `monthly_precip` |
|---|---|---|---|
| CRS | EPSG:4326 (latitude/longitude, degrees) | same | same |
| Cell (`cell_deg`) | 0.1° | 0.05° | 0.25° |
| Tile (`tile_cells`) | 10° × 10°, 100 × 100 cells | 200 × 200 cells | 40 × 40 cells |
| Global grid | 3600 × 1800 cells, 36 × 18 tiles | 7200 × 3600 cells | 1440 × 720 cells |

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

A 0.05° layer tile nests exactly in the 0.1° climate tile of the same name: each 0.1° cell holds
2 × 2 cells of 0.05°. A 0.25° layer tile covers the exact same 10° × 10° extent with 40 × 40 cells.

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
MUST check them against its own copy (id, unit, scale, offset) and refuse a mismatch. The eight
climate variables, including `tmean_cold` and `tmean_warm`, are kept in every release of
`format_version` 1.

#### Dry-month rule (`gaussen_p_lt_2t`)

For a cell, a slot and a month, with `P` the month's precipitation in mm and `T` its mean
temperature in °C (both Beck et al.'s unrounded `f32` values before quantisation), the month is dry
when `P < 2 × T` (strict inequality, Gaussen). Consequences:

- `P = 2 × T` is **not** dry.
- A month with `T ≤ 0 °C` is never dry (since `P ≥ 0`).
- If any of the 12 monthly temperature or precipitation values for that cell and slot is non-finite
  (a missing cell), both `dry_months` and the monthly `dry_mask` are `NODATA`; there is no
  per-month unknown bit.
- `dry_months` and `dry_mask` are computed once from the unrounded values. A reader MUST NOT
  recompute dryness from stored `tmean` and `precip`: at 0.5 °C and 1 mm steps (and at 0.25° for
  monthly rain) a recomputed answer can differ near `P ≈ 2 × T`.

This rule is named `"gaussen_p_lt_2t"` in the manifest's `dry_month_rule` field and pinned by the
`dry_month` vector table.

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

### Extremes layer (`extremes/`): 18 bands

On the climate grid: 0.1° cells, 100 × 100 per tile, the climate tiles' container (one internal tile
of 112). 2 bands × 9 slots, band (0-based) = `band_index × 9 + slot_index`, slots as in the climate
tiles (the layer's manifest entry repeats them under `slots`).

| # | Band id | Unit | Scale | Content |
|---|---|---|---|---|
| 0 | `tmin_extreme` | °C | 0.1 | coldest night of a typical year: mean annual lowest daily minimum |
| 1 | `tmax_extreme` | °C | 0.1 | hottest day of a typical year: mean annual highest daily maximum |

A cell has all 18 values or none. It has values only where the climate cell is complete; a complete
climate cell MAY have none (no source value nearby). `tmin_extreme ≤ tmin_cold` and
`tmax_extreme ≥ tmax_warm` of the same slot.

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

A lookup answers one point. Three lookups exist (climate, soil, regions), each in two forms (the
extremes are read at the climate answer's cell, below):

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

**Extremes answer**: no search. Read the cell holding the climate answer's (`cell_lat`, `cell_lon`)
in the extremes tile named by its `tile` (`extremes::at_cell`; WebAssembly
`lookupExtremes(tile, cell_lat, cell_lon)`). Fields: `cell_lat`, `cell_lon`, `tile`, and `periods`:
one entry per slot with `period`, `scenario`, `tmin_extreme_c`, `tmax_extreme_c` and `usda_zone`.
`null` without a climate answer, on a grid without the layer, without the tile, or where the cell has
no values. `usda_zone` of `t` °C: `f = t × 9/5 + 32`, `h = clamp(floor((f + 60) / 5), 0, 25)`,
zone `h / 2 + 1` (integer division) followed by `a` for even `h`, `b` for odd (the `usda` table).

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

## Monthly layers (`monthly/` and `monthly_precip/`)

Multi-band layers with per-band month counts declare `band_order: "band, slot, month"` in their
manifest `encoding`. Band (0-based) = `start(band) + slot × months(band) + month`, where `months`
is counted as `1` when absent on a band and `start(band)` is the sum of `len(slots) × months` of
all preceding bands.

Both monthly layers store 3 slots in order:

| # | Period | Scenario |
|---|---|---|
| 0 | 1991-2020 | historical |
| 1 | 2041-2070 | ssp245 |
| 2 | 2071-2099 | ssp245 |

### Monthly temperature and dry-month mask (`monthly/`): 75 bands

On the 0.1° climate grid: 100 × 100 cells per tile, internal tile 112. 3 slots (above).

| # | Band id | Months | Unit | Scale | Indices (3 slots) | Content |
|---|---|---|---|---|---|---|
| 0 | `tmean` | 12 | °C | 0.5 | 0–35 | monthly mean temperature (Beck et al. 0.1°) |
| 1 | `tmin` | 12 | °C | 0.5 | 36–71 | monthly mean daily minimum ("night low", CHELSA V2.1 `tasmin` 1981–2010 with Beck monthly deltas, clamped to `tmin ≤ tmean`) |
| 2 | `dry_mask` | – | bits | 1 | 72–74 | per-slot `int16` bitmask (below) |

Every offset is 0. `dry_mask` bit layout:

| Bits | Meaning |
|---|---|
| 0–11 | January … December: the month is dry under `gaussen_p_lt_2t` (`P < 2 × T`) |
| 12–14 | Reserved ("water across the year"). Writers MUST write 0; readers MUST ignore them. |
| 15 | Never set, so a valid mask is never negative and never equals `NODATA`. |

A cell has all 75 values or none, on the exact cells where the climate tile is complete, so a
monthly lookup reads the climate answer's `(cell_lat, cell_lon)` in the climate answer's `tile`
(no search of its own). For every land cell and slot, `popcount(dry_mask & 0x0FFF)` MUST equal the
climate tile's `dry_months` for that slot, and `tmin ≤ tmean` MUST hold for every month.

### Monthly precipitation (`monthly_precip/`): 36 bands

On a 0.25° grid: 40 × 40 cells per 10° tile, internal tile 48. 3 slots (above).

| # | Band id | Months | Unit | Scale | Indices (3 slots) | Content |
|---|---|---|---|---|---|---|
| 0 | `precip` | 12 | mm | 1 | 0–35 | monthly precipitation (mean of complete 0.1° Beck land cells inside the 0.25° cell) |

Offset 0. A 0.25° cell has all 36 values or none. Because the 0.25° grid does not align 1:1 with
the 0.1° climate cells on irregular coasts, a `monthly_precip` lookup uses the cell holding the
point (or the climate answer's cell centre) when it has data, else falls back to the nearest cell
with data within radius 1 (0.25° cells, Euclidean, ties north then west, cross-tile when using
`lookup_across`).

## Tail extremes and frost calendar (`extremes_tail/` and `frost_calendar/`)

Both layers sit on the 0.1° climate grid (100 × 100 cells per 10° tile, internal tile 112), are
static (no `slots` array), and have no spatial search of their own: a reader reads the cell holding
the climate answer's `(cell_lat, cell_lon)` in its `tile`.

### Tail extremes (`extremes_tail/`): 4 bands

| # | Band id | Unit | Scale | Content |
|---|---|---|---|---|
| 0 | `tmin_p10_gap` | °C | 0.1 | 1-in-10-year coldest-night offset below `tmin_extreme` (`<= 0.0`, ERA5-Land 1991–2020 `P10(TNn) − mean(TNn)`) |
| 1 | `tmax_p90_gap` | °C | 0.1 | 1-in-10-year hottest-day offset above `tmax_extreme` (`>= 0.0`, ERA5-Land 1991–2020 `P90(TXx) − mean(TXx)`) |
| 2 | `heat_days_35c` | days/year | 0.1 | mean annual days with daily `Tmax >= 35.0 °C` (1991–2020) |
| 3 | `recent_trend_c` | °C | 0.1 | realized warming shift: `mean(Tmean, 2015–2025) − mean(Tmean, 1991–2020)` |

Every offset is 0. A cell has all 4 values or none. For any `extremes` slot `s`, the 1-in-10-year
extremes are `tmin_extreme[s] + tmin_p10_gap` and `tmax_extreme[s] + tmax_p90_gap`.

### Frost calendar (`frost_calendar/`): 6 bands

Computed from 1991–2020 ERA5-Land daily `Tmin` and `Tmean` in per-pixel frost-years anchored at each
cell's thermal-winter DOY (`winter_doy`, opposite the warmest month) with leap day folded to DOY 365.

| # | Band id | Unit | Scale | Range & sentinels (`NODATA = -32768`) | Content |
|---|---|---|---|---|---|
| 0 | `last_spring_frost_doy` | doy | 1 | `1..=365`, `0` = frost-free (or no spring frost), `366` = year-round frost | conditional median last spring frost (`Tmin <= 0 °C`) |
| 1 | `last_spring_frost_p90_doy` | doy | 1 | `1..=365`, `0` = frost-free (or no spring frost), `366` = year-round frost | conditional 90th-percentile (1-in-10 late) last spring frost |
| 2 | `first_autumn_frost_doy` | doy | 1 | `1..=365`, `0` = frost-free (or no autumn frost), `366` = year-round frost | conditional median first autumn frost (`Tmin <= 0 °C`) |
| 3 | `frost_free_days` | days | 1 | `0..=365` (`365` = frost-free, `0` = year-round frost) | unconditional median frost-free season length |
| 4 | `frost_year_pct` | % | 1 | `0..=100` | percentage of years with at least one day `Tmin <= 0 °C` |
| 5 | `false_spring_pct` | % | 1 | `0..=100` (`0` when frost-free or GDD < 150) | percentage of years with `Tmin <= -2 °C` after `>= 150` GDD (base 5 °C) from `winter_doy` |

Every offset is 0. A cell has all 6 values or none. Sentinel `0` and `366` are distinct from
`NODATA` (`-32768`).

## 2050 Climate Twin index (`analogues.bin`)

`analogues.bin` is a little-endian binary table of representative 1991–2020 land cells for fast
client-side k-NN analogue search. Its path, byte size, SHA-256, version and record count are listed
under the optional top-level `analogues` manifest object and in `SHA256SUMS`.

- **Header (120 bytes):**
  - `magic`: `[u8; 4]` = `b"CNPA"`
  - `version`: `u16` = `1`
  - `header_bytes`: `u16` = `120`
  - `record_count`: `u32`
  - `feature_count`: `u8` = `8`
  - `reserved`: `[u8; 3]` = `[0, 0, 0]`
  - `mean`: `[f32; 8]`, `inv_std`: `[f32; 8]`, `weight`: `[f32; 8]`
  - `close_threshold`: `f32` (`1.0`), `novel_threshold`: `f32` (`2.0`)
- **Records (`record_count × 26` bytes, packed little-endian):**
  - `lat_e2: i16`, `lon_e2: i16` (0.01°), `iso_a2: u16` (`0xFFFF` = none), `wgsrpd_l3: u16`
    (`0xFFFF` = none), `koppen: u8` (1–30), `flags: u8` (bit 0 = `aridity` valid), `features: [i16; 8]`.
  - Feature order & scales: `[tmean_cold (0.1 °C), tmean_warm (0.1 °C), tmin_cold (0.1 °C),
    tmax_warm (0.1 °C), ln_precip (0.001), aridity (0.01), dry_months (1), summer_rain_frac (0.001)]`.
- **Search semantics:** Standardized weighted RMS distance `sigma` over valid features (masking
  `aridity` when absent on either query or candidate). `match_quality`: `"close"` (`<= 1.0`),
  `"moderate"` (`<= 2.0`), `"novel"` (`> 2.0`, `novel_climate = true`). Results are deduplicated by
  Haversine great-circle distance (`min_distance_km`, default `100 km`) and report `delta_lat_deg =
  ||query_lat| − |cand_lat||`.

## Phenology and water balance (`phenology/` and `water_balance/`)

Both layers sit on the 0.1° climate grid (100 × 100 cells per 10° tile, internal tile 112), are
static (no `slots` array), and have no spatial search of their own: a reader reads the cell holding
the climate answer's `(cell_lat, cell_lon)` in its `tile`.

### Phenology (`phenology/`): 5 bands

1991–2020 mean annual Fishman (1987) Dynamic Model Chill Portions over a 7-month (~212-day) winter
window (Oct–Apr for thermal-NH pixels where `winter_m` is Oct–Mar, Apr–Oct for thermal-SH pixels
where `winter_m` is Apr–Sep, via 12 two-hourly closed-form ODE steps per day from ERA5-Land daily
`Tmin`/`Tmax`), evaluated simultaneously at five uniform daily warming shifts `ΔT ∈ {0, +1, +2, +3, +4 °C}`
so readers can interpolate future SSP2-4.5 and microclimate chill collapse from winter warming deltas.

| # | Band id | Unit | Scale | Content |
|---|---|---|---|---|
| 0 | `chill_portions_0c` | CP | 1 | baseline 1991–2020 mean annual Chill Portions (`ΔT = 0 °C`) |
| 1 | `chill_portions_1c` | CP | 1 | mean annual Chill Portions under `+1 °C` warming |
| 2 | `chill_portions_2c` | CP | 1 | mean annual Chill Portions under `+2 °C` warming |
| 3 | `chill_portions_3c` | CP | 1 | mean annual Chill Portions under `+3 °C` warming |
| 4 | `chill_portions_4c` | CP | 1 | mean annual Chill Portions under `+4 °C` warming |

Every offset is 0. A cell has all 5 values or none (`0` is a valid zero-chill value in frost-free
tropics, distinct from `NODATA = -32768`).

### Water balance (`water_balance/`): 4 bands

1991–2020 drought and atmospheric evaporative-demand climatology combining TerraClimate (4 km, CC0)
with ERA5-Land daily precipitation (0.1°).

| # | Band id | Unit | Scale | Content |
|---|---|---|---|---|
| 0 | `cwd_mm` | mm/year | 1 | mean annual Climatic Water Deficit (TerraClimate `def`, 1991–2020) |
| 1 | `cwd_p90_mm` | mm/year | 1 | 90th-percentile (1-in-10 drought year) annual CWD (TerraClimate `def`, 1991–2020) |
| 2 | `cdd_max_days` | days | 1 | mean annual max consecutive dry days (`P < 1 mm/day`, ERA5-Land daily 1991–2020, anchored at each cell's wettest month) |
| 3 | `vpd_summer_kpa` | kPa | 0.01 | peak-quarter (max circular 3-month rolling mean) climatological monthly-mean VPD (TerraClimate `vpd`, 1991–2020) |

Every offset is 0. A cell has all 4 values or none.

## Compatibility rule

| Change | Allowed in | Requirement |
|---|---|---|
| New optional manifest field, new CLI flag or subcommand with output unchanged without it, docs or internal refactor keeping the same grid | patch (0.x.y) | `format_version` stays 1; readers ignore what they do not know; `release/grid.json` stays on the existing grid; vectors unchanged byte for byte except `cli` and additive rule tables. |
| New grid built with the same format (new layers, new sources, data fixes) | minor (0.x.0) | `format_version` stays 1; new grid release tar and manifest SHA-256; vectors regenerated and reviewed. |
| Any change to the container rules, georeferencing, cell edges, band order, a band's unit, scale or offset, the Köppen legend, the lookup search, or an answer's field | minor (0.x.0) with `format_version` + 1 | This document updated first; old readers refuse the new manifest by its version. |

A crate release that keeps the grid (0.2.1) names the grid release it was tested against in
`release/grid.json`; tiles are not re-uploaded.

## Vectors

| File | Grid | Made by | Checked by |
|---|---|---|---|
| `tests/fixtures/synthetic-expected.json` | the committed synthetic tiles `tests/fixtures/synthetic{,-soil,-regions,-extremes}-n50e000.tif` (written by `tile::synthetic`, `layer::synthetic` and `extremes::synthetic`) | `UPDATE_FIXTURES=1 cargo test` | plain `cargo test` (stale file fails); the wasm smoke runs every `own_tile` point through the WebAssembly build |
| `vectors/lookup.json` | the release grid in `release/grid.json` | `canopi-climate vectors --grid <dir> --points vectors/points.csv -o vectors/lookup.json` | CI's grid job (`--check`, ignoring `cli`); attached to each release by `release.yml` |

Both hold `points` (`name`, `lat`, `lon`, `own_tile`, and `expect`: the climate, soil, regions and
extremes answers, or `"invalid"`) and `tables`: `legend`, `margin`, `texture`, `codes` (`encode`,
`decode`), `decode` (scale decoding), `usda` (USDA zones) and `dry_month` (Gaussen `gaussen_p_lt_2t`
rule). `vectors/lookup.json` has the website goldens' shape (`cli`, `grid_manifest_sha256`,
`points`) plus `own_tile` and `tables`. A port passes when it reproduces every `expect` its lookup
form can answer and every table row.
