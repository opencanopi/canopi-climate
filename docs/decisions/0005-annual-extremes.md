# 0005: Coldest night and hottest day as an `extremes` layer (ERA5-Land offsets, NEX-GDDP scaling)

- Status: accepted (2026-11-18)
- Context: the Canopi plant palette showed the coldest-month mean daily minimum (`tmin_cold`, 0.3 °C at
  Lormes, France) where growers expect the coldest night of the year (about −10 °C there). Hardiness
  (USDA zones) and heat tolerance need annual extremes, which monthly means hide. Builds on
  [0001](0001-tile-format.md) and [0003](0003-site-layer-formats.md). Contract: [docs/format.md](../format.md).
  (0004 is the monthly-layer draft on `plan/monthly`; its CLIM-6 method for extremes is replaced by this one.)

## Question

Where do the mean annual minimum of the daily minimum (TNn) and the mean annual maximum of the daily
maximum (TXx) come from, for 1991-2020 and the 8 future slots, without downloading decades of daily
global data and without changing the climate tiles?

## Decision

1. **Offsets from the monthly values the grid already has.** For every slot `s`:

   ```text
   tmin_extreme[s] = tmin_cold[s] + gap_min + k_min × (tmean_cold[s] − tmean_cold[1991-2020])
   tmax_extreme[s] = tmax_warm[s] + gap_max + k_max × (tmean_warm[s] − tmean_warm[1991-2020])
   ```

   clamped so that `tmin_extreme ≤ tmin_cold` and `tmax_extreme ≥ tmax_warm`. The extremes stay
   consistent with the grid's own monthly climate (CHELSA and Beck et al. 2023) for every slot.

2. **Gaps from ERA5-Land daily aggregates, 1991-2020** (`ECMWF/ERA5_LAND/DAILY_AGGR`, about 9 km):
   `gap_min` = TNn − the coldest month's mean daily minimum, `gap_max` = TXx − the warmest month's
   mean daily maximum, interpolated bilinearly to the grid's cell centres. Coastal cells without an
   ERA5-Land value take the nearest within 5 cells.

3. **Change from NEX-GDDP-CMIP6 by pattern scaling.** Earth Engine's `NASA/GDDP-CMIP6` has SSP2-4.5
   and SSP5-8.5 only. For 16 models, one per model family (modelling centre or shared code: a median over all 30
   with daily `tas`, `tasmin` and `tasmax` in both would over-weight the families), the
   change of each gap (2041-2070 and 2071-2099 against 1991-2020, which is 1991-2014 historical plus
   2015-2020 SSP2-4.5) is regressed through the origin on the change of the coldest-month
   (warmest-month) mean temperature over the 4 samples. The slope is `k`. The median over the models
   (on a 1° grid: the change comes from GCMs at about 1° or coarser, so NEX-GDDP's 0.25° detail adds
   nothing to it) is applied to each slot's own warming in Beck et al. 2023, so SSP1-2.6 and SSP3-7.0 are
   scaled, not simulated. South of 60 S (no NEX-GDDP data) `k` is 0.

4. **Computed on Earth Engine, built offline.** `scripts/ee/extremes.py` runs the reductions as
   batch exports to Google Drive (one baseline GeoTIFF on ERA5-Land's own 0.1° grid and one 1°
   file per model, about 20 MB in all), downloads them and takes the median. The build
   interpolates the gaps bilinearly to its cell centres (half a cell off ERA5-Land's). The build reads the two rasters
   (`ee/era5land_baseline.tif`, `ee/nexgddp_k.tif`), pinned by SHA-256 like every source.

5. **A layer, not new climate bands.** `extremes/<name>.tif`: 0.1° cells in the climate tiles,
   18 int16 bands (2 × 9 slots, band = `band_index × 9 + slot`), scale 0.1 °C, the climate tiles'
   container (internal tile 112). The manifest lists it under `layers` with a `slots` list. Adding a
   layer keeps `format_version` 1 (format.md, compatibility rule); 72-band readers are untouched.

6. **No search of its own.** A reader reads the extremes at the cell the climate lookup chose, so
   both answers describe the same place. The USDA zone is derived at lookup (5 °F half-zones from
   −60 °F, `1a` … `13b`).

## Alternatives rejected

- **Daily CMIP6 or ERA5 downloads** (terabytes) for an exact TNn per slot: the offset method needs
  two small rasters.
- **IPCC Interactive Atlas TNn/TXx** (CMIP6, about 1°): no 1991-2020 observational baseline at a
  useful resolution, and a second, coarser climate beside the grid's.
- **New bands in the climate tiles**: `format_version` 2 and a coordinated reader change for every
  host.
- **Station-based validation or correction** (Météo-France, GHCN-D): useful, but a project of its
  own; the known bias is documented instead.

## Consequences

- ERA5-Land smooths cold-air pools and valley inversions: in hilly country `tmin_extreme` can be
  2–3 °C too mild. At Lormes (Morvan) it gives −8.3 °C, where nearby Météo-France stations suggest
  about −10 to −12 °C (1.3–3.1 nights a year at or below −10 °C). The hottest day (33.1 °C) agrees
  with the stations (7–18 days a year at or above 30 °C). The manifest notes say this.
- The values are typical extremes (means of each year's extreme), not worst cases: a heatwave
  year such as 2026 in France (40 °C or more over about 40% of the country) goes several degrees
  beyond them. A rarer-event band (for example the 1-in-10-year extreme, from the same ERA5-Land
  yearly series) is a possible follow-up.
- In Western Europe, observed hot extremes have risen faster than CMIP6 models simulate
  (Vautard et al. 2023, Nature Communications), so the future hottest days there are a lower bound.
  ERA5-Land at points (1991-2025, linear trend): the hottest day rose 1.15 °C per decade at Lormes,
  1.34 at Paris and 1.37 at Montpellier (±0.3), so its 2025 trend level at Lormes (35.1 °C) has
  already reached the 2041-2070 SSP2-4.5 value (35.1 °C). No future value accounts for regime
  shifts or tipping points. A follow-up anchors the extremes on the
  observed recent level and adds the 1-in-10-year values (detrended ERA5-Land, 1991-2025).
- The extremes are offsets from `tmin_cold` and `tmax_warm`, so they inherit those values' errors:
  in Singapore the grid's warmest-month maximum is 29.2 °C (Changi about 32), and the hottest day
  comes out near 31 °C where stations record about 34.
- The Earth Engine outputs are published as assets of the v0.3.0 release, where the source pins
  point, so a rebuild needs no Earth Engine account; the script reproduces them.
- Earth Engine refuses to export composites of the bounded NEX-GDDP daily images ("Unable to
  transform edge"); the script drops each image's footprint first and skips the few model-years
  that lack a temperature band (NorESM2-LM SSP5-8.5 2096).
- A host must learn the `extremes` directory before it installs a grid that has it (the website's
  installer refuses unknown directories).
