# 0003: Soil and regions are 0.05° int16 COG layers in the climate tiles

- Status: proposed (2026-11-11), awaiting review
- Context: consortium v2 plan, phase P3b; ADR "consortium v2 architecture" (website repository). Builds on [0001](0001-tile-format.md).

## Question

The consortium tool needs, for one location, the topsoil pH and texture and
the place's WGSRPD level-3 area and country. Each comes from a global source
in its own geometry: SoilGrids 2.0 (Homolosine raster, 5 km), WGSRPD level 3
(369 polygons) and Natural Earth admin-0 1:50m (polygons). How are they
stored so that the website fetches little per location, with the same reader
as the climate tiles?

## Decision

1. **One grid for both layers**: 0.05° cells (200 × 200 per tile) in the
   climate grid's 10° tiles, with the same tile names. A layer tile nests
   exactly in the climate tile (2 × 2 layer cells per climate cell).
   0.05° is the SoilGrids 5-km pixel at the equator (0.045°), so the soil
   keeps its resolution.
2. **Same container as the climate tiles** (0001): int16 COG,
   pixel-interleaved, Deflate, nodata -32768, EPSG:4326, one internal tile
   of 208, no overviews; written by `wbgeotiff`, read by the `tiff` crate.
   One file per layer and tile, under `soil/` and `regions/`. Tiles without
   data are absent. `manifest.json` (format 2) lists each layer under
   `layers` with its bands, sources and tiles (size, SHA-256).
3. **Separate soil and regions files**, not one 6-band file: the regions
   tiles are tiny (median 1.4 KB) and exist where soil does not (Greenland,
   Antarctica, ice and sand). Each layer can be rebuilt alone.
4. **Soil bands**: pH × 10, sand and clay in g/kg (SoilGrids' own units),
   and `fill_distance`. 0–30 cm is the depth-weighted mean (5, 10, 15) of the
   three standard depths. Silt and the USDA texture class are derived at
   lookup, not stored.
5. **Regions bands hold the codes**, as base-26 integers (`FRA` =
   5·676 + 17·26 + 0), so there is no palette or lookup table to ship and
   version. The lookup returns `{ wgsrpd_l3, iso_a2 }` and no names: names
   and translations belong to the website.

## Reprojection

`proj4rs` 0.3.0 has no `igh`. `src/pipeline/igh.rs` composes it from the
`sinu` and `moll` projections that `proj4rs` has, as PROJ's `igh.cpp` does:
twelve lobes with their own central meridian and false easting, Mollweide
lobes shifted to meet the sinusoidal ones at 40°44'11.8", spherical radius
6378137 m. The test `matches_proj_in_every_lobe` compares 26 points (every
lobe, both sides of the join latitude, the interruptions) with PROJ 9
`gdaltransform`: all within 1 mm.

Only the forward direction is used: each layer cell centre is projected and
takes the SoilGrids pixel under it (nearest neighbour). No averaging: the
source pixel and the cell have nearly the same size, and the values stay
SoilGrids' own. At 7 points, including Fiji and Chukotka across the
antimeridian, every value equals `gdallocationinfo -wgs84` on the source
files at the cell centre (`docs/validation.md`).

## Fill

SoilGrids has no value on built-up land, water bodies and ice. A website
user in a city still needs a topsoil estimate. Land cells (any regions
code) with no value take the values of the nearest measured cell within
**8 cells** (0.4°: about 44 km at the equator; 44 km north-south and 31 km east-west at 45°), with the distance in
the `fill_distance` band and `filled: true` in the lookup. Distances are
Euclidean in cells, rounded up, and wrap at the antimeridian.

Measured: every tested city is filled within 4 cells (Paris 3, Montpellier
1, São Paulo 2, Tokyo 4, Shanghai, Beijing and Buenos Aires 3). Of 391,541
filled cells, 71 % are at distance 1 and 1.5 % at distance 8. 8 cells leaves
headroom for large metropolitan areas and lakeshores while keeping the
ice sheets empty: 2,637,507 land cells stay without soil, 98.5 % of them in
Antarctica and Greenland.

## Measurements (out-v0.2, 2026-11-11)

| Layer | Tiles | Total | Median tile | Largest tile | Build |
|---|---|---|---|---|---|
| climate (`tiles/`, unchanged from v0.1) | 464 | 77,387,293 B | 125,488 B | 601,316 B | 42 s |
| soil (`soil/`, 4 bands) | 330 | 19,645,866 B | 45,041 B | 153,435 B | 8 s |
| regions (`regions/`, 2 bands) | 463 | 752,005 B | 1,447 B | 3,612 B | 24 s |

A location fetches about 172 KB at the median (125 + 45 + 1.4 KB).

Regions format candidates (`examples/regions_formats.rs`, every 4th of 452
land tiles, extrapolated to all):

| Candidate | Median tile | Largest (sample) | All tiles (est.) |
|---|---|---|---|
| raster 0.05° (chosen) | 1,408 B | 3,333 B | 714 KB |
| raster 0.025° | 2,236 B | 7,264 B | 1,277 KB |
| raster 0.01° | 9,648 B | 22,212 B | 4,860 KB |
| clipped + simplified (0.01°) polygons, GeoJSON | 2,788 B | 32,248 B | 2,587 KB |
| same, Deflate (as served compressed) | 788 B | 7,112 B | 650 KB |

Compressed polygons are slightly smaller, but they need a point-in-polygon
reader in the browser, a second format and simplification choices, and the
largest tiles are twice as big. The 0.05° raster reuses the tile reader,
shares the soil grid (the soil stage uses it as its land mask), and its
precision (±2.8 km at borders) matches what a site lookup needs. A finer
raster is a later option if border cases matter.

Browser reader (wasm32, `opt-level = "z"`, LTO, after `wasm-bindgen`):

| Build | wasm | gzip -9 |
|---|---|---|
| v0.1 (climate only) | 357,683 B | 96,562 B |
| this phase (+ `lookupSoil`, `lookupRegions`, `textureClass`) | 375,169 B | 100,953 B |

## Consequences

- Region boundaries are 0.05° cells: a point within one cell of a border can
  get the neighbouring area. Areas too small for any cell centre (islets,
  Monaco, Vatican) take their interior point's cell, so all 369 WGSRPD
  areas and 237 Natural Earth codes appear; Natural Earth 1:50m places
  the Vatican about 1.5 km west of its real position (12.428–12.439 E against 12.446–12.458 E), so a lookup at the real
  Vatican returns IT.
- WGSRPD's geometry repository declares no licence. It is cited
  (Brummitt 2001, TDWG) in `ATTRIBUTION.md`; asking TDWG for a licence
  statement is a follow-up.
- Soil is SoilGrids' mean prediction only; the uncertainty layers are not
  carried. Filled values are flagged, not hidden.
- `proj4rs` lacks `igh`; contributing it upstream is a follow-up
  (`docs/upstream-gaps.md`).
