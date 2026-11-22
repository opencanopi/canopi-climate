# 0006: Tail extremes, hemisphere-aware frost calendar, 2050 Climate Twin analogues, and `climate_tiles_sha256`

- Status: accepted (2026-11-23)
- Context: `v0.4.0` added monthly temperature/precipitation climatologies (`monthly`, `monthly_precip`)
  and `v0.3.0` added typical annual extremes (`extremes`). For planting decisions (Plant Palette),
  growers still need three pieces that monthly means and typical annual extremes do not capture:
  1. **Rarer extremes & realized warming (`extremes_tail`)**: 1-in-10-year freeze/heat tail offsets
     beyond the typical extreme, annual heat-stress days (`Tmax >= 35 °C`), and the warming already
     realized in 2015–2025 vs 1991–2020.
  2. **Frost calendar & false-spring risk (`frost_calendar`)**: median and 1-in-10 late spring frost
     dates, first autumn frost date, frost-free season length, percentage of years with frost, and
     false-spring killing-freeze risk after budbreak GDD accumulation — working uniformly in the
     Northern Hemisphere, Southern Hemisphere, equatorial highlands, frost-free tropics, and polar
     regions.
  3. **2050 Climate Twin (`analogues.bin`)**: fast (< 2 ms) client-side k-NN analogue lookup in pure
     Rust / WebAssembly matching any target climate (e.g., 2050 SSP2-4.5) to historical 1991–2020
     land locations with hemisphere-phase invariance, novel-climate detection, and spatial
     deduplication.
  4. **Manifest decoupling (`climate_tiles_sha256`)**: an optional top-level digest of the base
     climate tiles so downstream consumers (`canopi-website` `consortiumCheck`) do not break every
     time an additive optional layer is added under `format_version: 1`.
- Builds on [0001](0001-tile-format.md), [0003](0003-site-layer-formats.md),
  [0004](0004-monthly-and-relief-layers.md), and [0005](0005-annual-extremes.md).
  Normative contract: [docs/format.md](../format.md).

## Decisions

### 1. `extremes_tail` layer (`extremes_tail/<name>.tif`): static 0.1°, 4 bands

- **Geometry & container:** 0.1° climate grid (100 × 100 cells per 10° tile, internal tile 112),
  static (no `slots` array), 4 `int16` bands, scale `0.1`, offset `0`, `NODATA = -32768`.
- **Bands:**
  - `0`: `tmin_p10_gap` (°C, scale 0.1) — 1-in-10-year coldest-night tail offset below `tmin_extreme`:
    `min(0, P10(TNn_y) − mean(TNn_y))` over 1991–2020 ERA5-Land daily `temperature_2m_min`.
  - `1`: `tmax_p90_gap` (°C, scale 0.1) — 1-in-10-year hottest-day tail offset above `tmax_extreme`:
    `max(0, P90(TXx_y) − mean(TXx_y))` over 1991–2020 ERA5-Land daily `temperature_2m_max`.
  - `2`: `heat_days_35c` (days/year, scale 0.1) — mean annual count of days with daily
    `temperature_2m_max >= 35.0 °C` over 1991–2020.
  - `3`: `recent_trend_c` (°C, scale 0.1) — realized warming shift:
    `mean(Tmean, 2015–2025) − mean(Tmean, 1991–2020)` from ERA5-Land daily `temperature_2m`.
- **Why gaps instead of raw ERA5-Land `tmin_p10` / `tmax_p90`?**
  Storing `tmin_p10_gap` (`<= 0`) and `tmax_p90_gap` (`>= 0`) mirrors ADR 0005 (`extremes`):
  1. It guarantees `tmin_p10[s] = tmin_extreme[s] + tmin_p10_gap <= tmin_extreme[s]` and
     `tmax_p90[s] = tmax_extreme[s] + tmax_p90_gap >= tmax_extreme[s]` on every cell, avoiding
     ERA5-Land vs CHELSA/Beck baseline inversions in mountain cells.
  2. It pairs directly with every future slot `s` of `extremes` without duplicating 18 future bands.
- **Regridding & lookup:** Bilinearly interpolated from ERA5-Land's 0.1° grid to canopi's cell
  centres (half a cell shifted), with nearest-within-5-cells coastal fallback. At lookup time, no
  spatial search of its own: read at the climate answer's `(cell_lat, cell_lon)` in its `tile`
  (`extremes_tail::at_cell`; WASM `lookupExtremesTail(tile, cell_lat, cell_lon)`).

### 2. `frost_calendar` layer (`frost_calendar/<name>.tif`): static 0.1°, 6 bands

- **Geometry & container:** 0.1° climate grid (100 × 100 cells per 10° tile, internal tile 112),
  static (no `slots` array), 6 `int16` bands, scale `1`, offset `0`, `NODATA = -32768`.
- **Per-cell thermal-winter anchoring:**
  Calendar years split Southern Hemisphere winters and wrap frost-free seasons across Dec 31 → Jan 1,
  and latitude sign is a poor proxy for thermal winter in equatorial highlands (Quito, Kiambu).
  Instead, for every pixel we compute its warmest month `m_warm` from 1991–2020 ERA5-Land monthly
  mean `temperature_2m`, set `summer_doy` to `m_warm`'s mid-month DOY, and define the opposite
  `winter_doy = ((summer_doy - 183 + 365) % 365) + 1` (1..365; mid-January in NH, mid-July in SH).
  Leap day (DOY 366) is folded to DOY 365 so all years share a uniform 365-day coordinate.
  Each frost-year `Y` (`1991..=2020`) runs unbroken from `winter_doy` in year `Y` to `winter_doy - 1`
  in year `Y + 1`, with shifted coordinate `fy_doy = (doy - winter_doy + 365) % 365` (`0` = deep
  winter, `182` = midsummer):
  - Spring is `0 <= fy_doy < 182`; `last_spring_fy = max(fy_doy)` of days with `Tmin <= 0 °C`.
  - Autumn is `182 <= fy_doy < 365`; `first_autumn_fy = min(fy_doy)` of days with `Tmin <= 0 °C`.
  - `frost_free_days = clamp(first_autumn_fy.unwrap_or(365) - last_spring_fy.unwrap_or(0), 0, 365)`.
  - Multi-year median and P90 are computed in shifted `fy_doy` coordinates (preventing Dec/Jan wrap
    averaging to July!) and mapped back to calendar DOY `1..=365`.
  - False-spring GDD (`max(0, Tmean - 5 °C)`) accumulates strictly from `winter_doy` (never Jan 1
    in SH); a year counts as a false spring iff at least one day has `Tmin <= -2 °C` on or after
    cumulative GDD reaches `150 °C·day`.
- **Bands & explicit sentinels (distinct from `NODATA = -32768`):**
  - `0`: `last_spring_frost_doy` (`doy`, scale 1) — conditional median over frost years (`1..=365`),
    `0` = frost-free (`frost_year_pct == 0` or no spring frost), `366` = year-round frost
    (`frost_free_days == 0`).
  - `1`: `last_spring_frost_p90_doy` (`doy`, scale 1) — conditional 90th-percentile (1-in-10 late)
    last spring frost (`1..=365`, or `0` / `366`).
  - `2`: `first_autumn_frost_doy` (`doy`, scale 1) — conditional median over frost years (`1..=365`,
    or `0` / `366`).
  - `3`: `frost_free_days` (`days`, scale 1) — unconditional median frost-free season length
    (`0..=365`; `365` when frost-free, `0` when year-round frost).
  - `4`: `frost_year_pct` (`%`, scale 1) — percentage of years in 1991–2020 (`0..=100`) with at
    least one day `Tmin <= 0 °C`. Prevents intermittent-frost climates (e.g., Lisbon, Sydney, 10–40%
    of years) from being mistaken for annual frost.
  - `5`: `false_spring_pct` (`%`, scale 1) — percentage of years (`0..=100`) with a killing freeze
    (`Tmin <= -2 °C`) after `>= 150` GDD (base 5 °C) from `winter_doy`. Guaranteed `0` when
    frost-free or when GDD never reaches 150 (polar/alpine).
- **Regridding & lookup:** Nearest-neighbor from ERA5-Land 0.1° (with nearest-within-5-cells coastal
  fallback) so integer DOYs and `0`/`366` sentinels are never corrupted by interpolation across
  boundaries. No search of its own at lookup: read at the climate answer's `(cell_lat, cell_lon)`
  (`frost_calendar::at_cell`; WASM `lookupFrostCalendar(tile, cell_lat, cell_lon)`).

### 3. `analogues.bin` (**2050 Climate Twin**): compact binary k-NN index

- **Purpose:** Match any target climate (e.g., a site's 2050 SSP2-4.5 projection) against historical
  1991–2020 land cells worldwide in `< 2 ms` in pure Rust / WebAssembly, with zero network calls.
- **Spatial subsampling:** Sample complete 1991–2020 land cells on a regular **0.75°** stride (every
  7th–8th 0.1° cell, or 1.0° / 0.75° grid yielding ~18,000–25,000 non-ocean records, plus all
  validation cities). At 26 bytes per record, 18,000 records = **~468 KB** uncompressed (~200 KB
  gzipped), well below Cloudflare Workers' 25 MiB asset cap and fast to fetch on demand.
- **8 hemisphere-phase-invariant features (`[i16; 8]`):**
  0. `tmean_cold_c` (°C, scale 0.1)
  1. `tmean_warm_c` (°C, scale 0.1) — together with `tmean_cold_c`, captures both MAT and thermal
     seasonality (`tmean_warm - tmean_cold`)
  2. `tmin_cold_c` (°C, scale 0.1)
  3. `tmax_warm_c` (°C, scale 0.1)
  4. `ln_precip` (`ln(1 + precip_mm)`, scale 0.001)
  5. `aridity` (scale 0.01; masked when `flags & 1 == 0`, e.g., polar cells with PET < 1 mm/yr)
  6. `dry_months` (0–12, scale 1)
  7. `summer_rain_frac` (`P_warm_half / P_annual` in `0..=1`, scale 0.001, where `warm_half` is the
     6-month window centered on the cell's warmest month `m_warm` from `monthly` + `monthly_precip`)
     — separates Mediterranean winter-rain from monsoon summer-rain independently of hemisphere.
- **Distance, novel-climate flag, photoperiod delta, and Haversine deduplication:**
  - Standardized weighted RMS distance `sigma = sqrt(sum(w_i * z_i^2) / sum(w_i))` over valid
    features (masking `aridity` if absent on either query or candidate, never dropping polar cells
    or producing `NaN`).
  - Quality bands: `close` (`sigma <= 1.0`), `moderate` (`1.0 < sigma <= 2.0`), `novel`
    (`sigma > 2.0`, sets `novel_climate: true` for 2050 lowland tropics with no historical analogue).
  - Each match embeds `(lat, lon, iso_a2, wgsrpd_l3, koppen, sigma, match_quality, delta_lat_deg)`
    where `delta_lat_deg = | |query_lat| - |cand_lat| |` exposes photoperiod-amplitude mismatch
    (e.g., Quito vs maritime temperate island).
  - Greedy top-`k` selection enforces `min_distance_km` (default `100 km`) between returned twins
    and optional `exclude_radius_km` from the query point using Haversine great-circle distance
    (exact across ±180° antimeridian and high latitudes).
- **Manifest entry:** Listed under an optional top-level `analogues` field (`path`, `bytes`,
  `sha256`, `version`, `records`) and in `SHA256SUMS`. Keeps `format_version: 1`.

### 4. `climate_tiles_sha256` in `manifest.json`

- Added as an optional top-level hex SHA-256 string in `manifest.json` (keeping `format_version: 1`).
- Computed over the UTF-8 string `"canopi-climate-tiles-v1\n"` followed by one line
  `"<name> <bytes> <sha256>\n"` per climate tile in `manifest.tiles` sorted lexicographically by
  `name`.
- Remains invariant across additive layer releases (`v0.4.0` → `v0.5.0` → `v0.6.0` → `v0.7.0`),
  allowing `canopi-website`'s `consortiumCheck` to verify climate-tile compatibility without
  requiring a `canopi-data` re-export for every new optional layer.

## Consequences

- `format_version` stays `1`; all existing `tiles/`, `soil/`, `regions/`, `extremes/`, `monthly/`,
  and `monthly_precip/` tiles remain byte-for-byte identical to `v0.4.0`.
- Total grid size grows by ~9–12 MB (`extremes_tail` ~4 MB, `frost_calendar` ~5 MB, `analogues.bin`
  ~0.5 MB), keeping the total tile count around ~3,400 (well under Cloudflare's 20,000 file cap).
- Earth Engine inputs (`ee/era5land_extremes_tail.tif`, `ee/era5land_frost_calendar.tif`) are
  published as assets of the `v0.5.0` release and pinned in `src/pipeline/sources.rs`, so rebuilds
  require no Earth Engine account.
