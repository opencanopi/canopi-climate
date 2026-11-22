# 0004: Monthly layers declare their own grid and band schema (CLIM-1, CLIM-2, CLIM-3)

- Status: accepted (2026-11-22), target release `v0.4.0` (`format_version` 1).
- Context: the website's asks CLIM-1 to CLIM-5 (Plant palette redesign, website
  `knowledge/project/consortium-v2-plan.md`, section "Website needs from canopi-climate"); roadmap
  steps C2 (crate refactor) and C3 (monthly layers). Builds on [0001](0001-tile-format.md),
  [0003](0003-site-layer-formats.md), [0005](0005-annual-extremes.md) and `docs/format.md`.
  CLIM-6 (`tmin_year`) was superseded and shipped in `v0.3.0` as the dedicated `extremes` layer
  (ADR 0005) and is dropped from `monthly`. CLIM-4 (`relief`) is deferred to `v0.5.0`.

## Question

The website needs month-by-month values (mean temperature, night low, which months are dry, and
monthly rain) on grids suited to each layer. Earlier layers had fixed grids in the code and in
`docs/format.md` (climate and `extremes` 0.1° in 100-cell tiles; `soil` and `regions` 0.05° in
200-cell tiles) and fixed band lists in each reader. How are the new layers stored so that:

1. the location step does not fetch more than today;
2. the resolution of monthly rain (CLIM-3: 0.25° in `v0.4.0`, or 0.1° later) can be changed without
   a reader change;
3. `format_version` stays 1, and old readers keep reading the release?

## Decision

1. **Each layer declares its grid and band schema in the manifest.** `layers[]` already carries
   `cell_deg`, `tile_cells`, `encoding.bands`, `encoding.band_order`, `bands` and `slots` (added
   for `extremes` in `v0.3.0`). Step C2 completes the per-layer schema:
   - `slots`: the layer's period/scenario slots (or absent/empty for a static layer);
   - per band, optional `months`: `12` for a monthly band, absent (one value) otherwise;
   - `band_order: "band, slot, month"`, with the 0-based band index computed as
     `start(band) + slot × months(band) + month` (`months` counted as `1` when absent), where
     `start(band)` is the sum of `len(slots) × months` of the bands before it;
   - optional `version`: the layer's own version (a layer rebuilt alone gets a new one).

   `docs/format.md` generalizes the container rule from hardcoded 100/200 to the layer's
   `tile_cells`, with `tile_cells × cell_deg = 10` exactly and the internal tile the smallest
   multiple of 16 ≥ `tile_cells` (112 for 100, 208 for 200, 48 for 40). A reader takes grid, bands
   and band positions from the manifest and checks them against the band ids, units, scales and
   offsets it knows.

2. **`monthly` (CLIM-1, CLIM-2):** 0.1° cells in 100-cell tiles (internal tile 112), on the exact
   climate grid, under `monthly/`. **3 slots**: `1991-2020 historical`, `2041-2070 ssp245`,
   `2071-2099 ssp245` (matching the Plant Palette screen's SSP2-4.5 monthly scope; ~38 MB vs
   ~111 MB for 9 slots; the slot list is declared in the manifest so a future 9-slot build needs
   no reader change). **75 bands**:
   - `tmean` (12 months × 3 slots, °C, scale 0.5, indices 0–35): Beck et al. 0.1° monthly means;
   - `tmin` (12 months × 3 slots, °C, scale 0.5, indices 36–71): CHELSA V2.1 monthly `tasmin`
     1981–2010 aggregated to 0.1° and shifted by Beck monthly mean deltas (same method as
     `tmin_cold`), clamped to `tmin ≤ tmean`;
   - `dry_mask` (3 slots, indices 72–74): `int16` bitmask, bits 0–11 = January…December dry under
     `gaussen_p_lt_2t`, bits 12–14 reserved and written 0, bit 15 never set.

   A cell has all 75 values exactly where the climate tile's cell is complete, so a monthly lookup
   reads the climate answer's cell (no search of its own) and `popcount(dry_mask & 0x0FFF) == dry_months`
   holds on every land cell and slot. Annual extremes (`tmin_extreme`, `tmax_extreme`) remain in
   the `extremes` layer (ADR 0005), not in `monthly`.

3. **Monthly rain is its own layer, `monthly_precip` (CLIM-3), at 0.25°.** A layer has one grid;
   a 0.25° rain layer cannot share 0.1° tiles with `dry_mask`, which must stay on the climate grid
   for `popcount(mask) == dry_months`. Separate files also keep `monthly` smaller for readers that
   do not draw rain. `monthly_precip/` uses 0.25° cells (40 × 40 per 10° tile, internal tile 48),
   3 slots (same as `monthly`), 36 bands (`precip`, 12 months × 3 slots, mm, scale 1, ~9.0 MB),
   aggregated as the mean of complete 0.1° Beck land cells inside each 0.25° cell, with a radius-1
   coastal fallback at lookup.

4. **`dry_month_rule` (CLIM-1):** a top-level optional manifest string, `"gaussen_p_lt_2t"`, with
   its normative definition in `docs/format.md` and pinned by the `dry_month` vector table. A reader
   asserts it when present; a different rule would be a new identifier and a minor release.

5. **`relief` (CLIM-4) deferred to `v0.5.0`.** When built, it will follow the same per-layer schema
   as a static 0.1° layer (`elev_mean`, `elev_p05`, `elev_p95`), and its lapse-rate rule will also
   cover `extremes` (`tmin_extreme`, `tmax_extreme`, and recomputed `usda_zone`).

6. **`format_version` stays 1.** Every addition is an optional manifest field or a new layer. A
   0.1.x–0.3.x crate reader ignores unknown manifest fields and layers. Adding new layers produces
   a new grid release tar, manifest SHA-256 and regenerated vectors, so it ships as a minor release
   (`v0.4.0`).

## Alternatives considered

| Alternative | Why not |
|---|---|
| Add monthly bands to the climate tiles | The location step would fetch months it does not use; every climate tile and SHA-256 would change; a band-order change requires `format_version` 2. |
| Store all 9 slots in `monthly` | Triples `monthly` from ~38 MB to ~111 MB; no Plant Palette screen asks for multi-SSP monthly charts. The manifest `slots` field allows expanding later without a reader change. |
| One `monthly` layer holding rain too | Forces one resolution for all monthly bands: 0.25° breaks `popcount(dry_mask) == dry_months` against the 0.1° climate grid, while 0.1° rain adds 44.1 MB instead of 9.0 MB. |
| Fixed grids per layer id in the readers | Each resolution choice would need a reader release on every port; the manifest schema lets readers stay generic. |
| Include `tmin_year` (CLIM-6) in `monthly` | Superseded by `extremes` in `v0.3.0` (ADR 0005), which provides both `tmin_extreme` and `tmax_extreme` for all 9 slots at 0.1 °C precision. |
| A `cool_mask` band | The website computes cool months from `tmin` with its own thresholds, so a threshold change needs no grid release. |

## Consequences

- C2 (generic `Raster` / `LayerGrid` and per-layer schema in code and manifest) lands first as a
  behaviour-preserving refactor.
- `Tile`, `LayerTile`, `ExtremesTile`, and the new monthly layers share one `Raster` / `LayerGrid`
  abstraction instead of duplicating tile geometry and decode logic.
- `vectors/lookup.json` gains the `dry_month` table and (once the grid is built) `monthly` and
  `monthly_precip` answers per point, shipping as `v0.4.0` while `tiles/`, `soil/`, `regions/` and
  `extremes/` remain byte-for-byte identical to `v0.3.0`.
