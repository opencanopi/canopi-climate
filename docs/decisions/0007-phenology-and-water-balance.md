# 0007: Dynamic Chill Portions (`phenology`), photoperiod, water balance (`water_balance`), and Saxton–Rawls AWC

- Status: accepted (2026-11-23)
- Context: `v0.5.0` added tail extremes (`extremes_tail`), hemisphere-aware frost dates (`frost_calendar`),
  and 2050 Climate Twin search (`analogues.bin`). For orchard, perennial, and drought-resilient planting
  decisions, four horticultural signals remain missing:
  1. **Dynamic Model Chill Portions (`phenology`)**: simple chill-hour thresholds (`0–7 °C`) fail in mild
     maritime and Mediterranean winters because warm spells negate accumulated chill. The Fishman, Erez &
     Couvillon (1987) Dynamic Model tracks reversible intermediate chill products and warm-spell negation,
     and needs future warming sensitivity so Plant Palette can flag 2041–2070 and 2071–2099 SSP2-4.5
     chill collapse for temperate fruit trees.
  2. **Photoperiod (0 bytes, pure Rust)**: day-length extremes, seasonal amplitude, monthly daylength,
     and polar-night/midnight-sun counts govern flowering triggers, bud-set, and latitude-transfer limits.
  3. **Water balance (`water_balance`)**: annual and 1-in-10-year Climatic Water Deficit (CWD),
     peak-quarter Vapor Pressure Deficit (VPD), and maximum consecutive dry days (CDD) capture irrigation
     demand, atmospheric evaporative stress, and dry-spell duration beyond monthly precipitation totals.
  4. **Available Water Capacity (0 bytes, pure Rust)**: translating `soil/` texture (`sand_pct`, `clay_pct`)
     into field capacity, permanent wilting point, and root-zone water storage (mm) via Saxton & Rawls (2006)
     links soil texture directly to CWD/CDD drought buffering.
- Builds on [0001](0001-tile-format.md), [0003](0003-site-layer-formats.md),
  [0004](0004-monthly-and-relief-layers.md), [0005](0005-annual-extremes.md), and
  [0006](0006-tail-extremes-frost-calendar-analogues.md).
  Normative contract: [docs/format.md](../format.md).

## Decisions

### 1. `phenology` layer (`phenology/<name>.tif`): static 0.1°, 5 bands

- **Geometry & container:** 0.1° climate grid (100 × 100 cells per 10° tile, internal tile 112),
  static (no `slots` array), 5 `int16` bands, scale `1`, offset `0`, `NODATA = -32768`.
- **Bands (Fishman Dynamic Model Chill Portions, 1991–2020 mean annual):**
  - `0`: `chill_portions_0c` (CP, scale `1`) — baseline 1991–2020 (`ΔT = 0 °C`).
  - `1`: `chill_portions_1c` (CP, scale `1`) — under uniform `+1 °C` daily warming.
  - `2`: `chill_portions_2c` (CP, scale `1`) — under uniform `+2 °C` daily warming.
  - `3`: `chill_portions_3c` (CP, scale `1`) — under uniform `+3 °C` daily warming.
  - `4`: `chill_portions_4c` (CP, scale `1`) — under uniform `+4 °C` daily warming.
- **Why a 5-band `ΔT ∈ {0, +1, +2, +3, +4 °C}` sensitivity curve instead of pre-baked 2050/2085 bands?**
  1. GEE has no Beck SSP2-4.5 daily rasters; running the Fishman ODE simultaneously across 5 warming
     shifts on ERA5-Land daily preserves each pixel's true daily weather variance and warm-spell chill
     negation in a single compute pass.
  2. At lookup time, Rust computes each future slot's winter warming delta `ΔT_winter` directly from
     `monthly` (`mean(tmean_future - tmean_hist)` over the 6 months centered on thermal winter) and
     piecewise-linearly interpolates `CP(ΔT)` (clamped at `0` for `ΔT > 4 °C` via linear extrapolation
     from the `[+3, +4]` slope, floored at `0`), yielding projected `2041–2070` and `2071–2099` SSP2-4.5
     chill portions with zero extra tile bytes.
  3. Storing the curve directly also lets `v0.7.0`'s microclimate/elevation engine evaluate arbitrary
     topoclimatic `ΔT` offsets (elevation lapse rate, urban heat island, cold-air pooling) without
     introducing another layer.
- **Hemisphere-aware chill window (7 months / ~212 days centered on thermal winter):**
  - Reuses ADR 0006's per-pixel thermal winter (derived from 1991–2020 `ECMWF/ERA5_LAND/MONTHLY_AGGR`
    warmest month `m_warm`, opposite `winter_m = ((m_warm + 5) % 12) + 1`).
  - Accumulates Chill Portions over a **7-month (~212-day) winter window**: **Oct–Apr** for thermal-NH
    pixels (`winter_m` in Oct–Mar) and **Apr–Oct** for thermal-SH pixels (`winter_m` in Apr–Sep).
    The 7-month span (~±105 days around midwinter) captures autumn/spring shoulder chill in cold
    continental and boreal sites where midwinter temperatures below `0 °C` contribute almost zero chill.
  - Tropics with `T > 13 °C` year-round yield valid `0` CP (never `NODATA`). Equatorial highlands (e.g.,
    Quito, Kiambu) accumulate cool-night chill year-round; `phenology` reports the single-season 7-month
    accumulation centered on their thermal winter.
- **Two-hourly closed-form Fishman (1987) ODE in GEE (monthly-chunked with 2-day warm-up):**
  - Hourly ERA5-Land (`262,800` images/pixel) and full-season 212-day daily `iterate()` exceed GEE's
    expression-graph complexity limits. Instead, we partition each 7-month winter into **7 parallel
    monthly `iterate()` chunks** (each preceded by a 2-day warm-up overlap to initialize the short-memory
    intermediate product `x`, discarding warm-up CP), running 12 two-hourly cosine diurnal steps per day
    (`Δt = 2 h`, minimum at 03:00, maximum at 15:00) from `ECMWF/ERA5_LAND/DAILY_AGGR`
    (`temperature_2m_min`, `temperature_2m_max`):
    - $E_0 = 4153.5\text{ K}$, $E_1 = 12888.8\text{ K}$, $A_0 = 139500\text{ h}^{-1}$,
      $A_1 = 2.567 \times 10^{18}\text{ h}^{-1}$, $\text{slp} = 1.6\text{ K}^{-1}$, $T_{\text{elt}} = 277.0\text{ K}$.
    - $x_s = (A_0 / A_1) \exp((E_1 - E_0) / T_K)$, $k_1 = A_1 \exp(-E_1 / T_K)$,
      $x_{\text{inter}} = x_s - (x_s - x) \exp(-k_1 \Delta t)$.
    - $\text{ftmprt} = \text{clamp}(\text{slp} \cdot T_{\text{elt}} \cdot (T_K - T_{\text{elt}}) / T_K, -50, 50)$,
      $\xi = 1 / (1 + \exp(-\text{ftmprt}))$.
    - When $x_{\text{inter}} \ge 1$: $\text{CP} \mathrel{+}= \xi \cdot x_{\text{inter}}$ and
      $x \leftarrow x_{\text{inter}} (1 - \xi)$; else $x \leftarrow x_{\text{inter}}$.
  - Exported in **6 × 5-year chunks** (`1991–1995` … `2016–2020`), then averaged across the 6 chunks
    locally in `cmd_combine()`.
- **Regridding & lookup:** Bilinearly interpolated from ERA5-Land's 0.1° grid to canopi's cell centres
  (nearest-within-5-cells coastal fallback), clamped to non-negative integers. No spatial search at lookup:
  read at the climate answer's `(cell_lat, cell_lon)` in its `tile` (`phenology::at_cell`; WASM
  `lookupPhenology(tile, cell_lat, cell_lon)`).

### 2. Photoperiod (0 bytes, pure Rust / WASM)

- Built on FAO-56 geometric daylength (`src/derived.rs::day_length_hours`, consistent with
  `thornthwaite_pet`): $\cos \omega_s = -\tan\phi \tan\delta$, clamped to $[-1, 1]$.
- **Pole hardening & exact symmetry:**
  - Reject non-finite or `|lat_deg| > 90.0` (`None`).
  - Handle `|lat_deg| >= 90.0` explicitly without `tan(±90°)` overflow (`24.0` when `lat * δ > 0`,
    `0.0` when `< 0`, `12.0` when `δ == 0`).
  - Compute `min_hours` and `max_hours` analytically at exact `|δ| = 0.409` rad so
    `min_hours(lat) == min_hours(-lat)` and `min_hours + max_hours == 24.0` identically.
- Exposes `photoperiod(lat_deg)` → `{ min_hours, max_hours, amplitude_hours, monthly_hours: [f64; 12],
  polar_night_days, midnight_sun_days }` (and WASM `photoperiod(lat)`).

### 3. `water_balance` layer (`water_balance/<name>.tif`): static 0.1°, 4 bands

- **Geometry & container:** 0.1° climate grid (100 × 100 cells per 10° tile, internal tile 112),
  static (no `slots` array), 4 `int16` bands, `NODATA = -32768`.
- **Bands (1991–2020 historical climatology):**
  - `0`: `cwd_mm` (mm/yr, scale `1`) — mean annual Climatic Water Deficit from **TerraClimate**
    (`IDAHO_EPSCOR/TERRACLIMATE` band `def`, 4 km, CC0 / Public Domain; native scale `0.1 mm`, summed
    over 12 months per year, 30-year mean).
  - `1`: `cwd_p90_mm` (mm/yr, scale `1`) — 90th-percentile (1-in-10 drought year) annual CWD from
    TerraClimate `def` (1991–2020).
  - `2`: `cdd_max_days` (days, scale `1`) — mean annual maximum consecutive dry days
    (`total_precipitation_sum < 0.001 m`, i.e. `< 1 mm/day`) from **ERA5-Land Daily**
    (`ECMWF/ERA5_LAND/DAILY_AGGR`, 0.1°, 1991–2020). Each pixel's CDD year is anchored at the
    **first day of its wettest month** (1991–2020) so Southern Hemisphere and tropical dry seasons
    crossing Dec 31 → Jan 1 are never split across calendar years. Exported in 3 × 10-year chunks
    (`1991–2000`, `2001–2010`, `2011–2020`) and averaged locally.
  - `3`: `vpd_summer_kpa` (kPa, scale `0.01`, stored as hundredths of kPa) — **peak-quarter monthly-mean
    VPD**: maximum over all 12 circular 3-month rolling windows of 1991–2020 climatological monthly-mean
    TerraClimate `vpd` (native scale `0.01 kPa`). Using the 3-month circular rolling mean is
    hemisphere-agnostic (captures DJF in SH, JJA in NH, and dry-season peaks in the tropics). Note:
    monthly-mean VPD (~1.5–2.5 kPa in Mediterranean summers) is roughly half of afternoon-peak VPD
    (`es(Tmax) - ea(Tdew)`); both scales are well-established, and TerraClimate's monthly-mean VPD pairs
    consistently with `def` in the same CC0 source.
- **Regridding & lookup:** Bilinearly interpolated to canopi's 0.1° cell centres (nearest-within-5-cells
  coastal fallback). No spatial search at lookup: read at the climate answer's `(cell_lat, cell_lon)` in
  its `tile` (`water_balance::at_cell`; WASM `lookupWaterBalance(tile, cell_lat, cell_lon)`).

### 4. Saxton & Rawls (2006) Available Water Capacity (0 bytes, pure Rust / WASM)

- Implements Saxton & Rawls (2006, SSSAJ 70:1569–1578, Table 1) from `sand_pct`, `clay_pct`, and
  optional organic matter `om_pct` (default `2.5%` in `v0.6.0`; overrideable in `v0.7.0` from `soil_plus`
  SOC as `soc_pct * 1.724`, with optional coarse-fragment rock discount `*(1 - cfvo_frac)`):
  - Reject non-finite inputs, negative fractions, or `sand_pct + clay_pct > 100.0 + 1e-9` (`None`).
  - Calibration-domain clamps: $S = \text{sand}/100$, $C = \min(\text{clay}/100, 0.60)$,
    $OM = \text{clamp}(\text{om\_pct}, 0.0, 8.0)$.
  - $\theta_{1500t} = -0.024 S + 0.487 C + 0.006 OM + 0.005 S \cdot OM - 0.013 C \cdot OM + 0.068 S \cdot C + 0.031$,
    $\theta_{1500} = \text{clamp}(\theta_{1500t} + 0.14\theta_{1500t} - 0.02,\; 0.01,\; 0.50)$.
  - $\theta_{33t} = -0.251 S + 0.195 C + 0.011 OM + 0.006 S \cdot OM - 0.027 C \cdot OM + 0.452 S \cdot C + 0.299$,
    $\theta_{33} = \text{clamp}(\theta_{33t} + 1.283\theta_{33t}^2 - 0.374\theta_{33t} - 0.015,\; \theta_{1500} + 0.03,\; 0.60)$.
  - $\text{AWC}_v = \text{clamp}(\theta_{33} - \theta_{1500},\; 0.03,\; 0.30)\;\text{m}^3/\text{m}^3$.
- Outputs: `field_capacity` ($\theta_{33}$), `wilting_point` ($\theta_{1500}$), `awc_mm_per_m`
  (`awc_v * 1000`), `awc_30cm_mm` (`awc_v * 300`, matching `soil/` 0–30 cm depth), `awc_100cm_mm`
  (`awc_v * 1000`, extrapolated tree root zone).

## Consequences

- `format_version` stays `1`; `climate_tiles_sha256` and all existing `v0.5.0` tiles remain byte-for-byte
  identical.
- Total grid size grows by ~8–10 MB (`phenology/` ~4.5 MB, `water_balance/` ~4.5 MB), bringing the
  cumulative tar to ~198–200 MB and ~4,500 files (well under Cloudflare Workers' 20,000-file and 25 MiB
  asset caps).
- Earth Engine inputs (`ee/era5land_phenology.tif`, `ee/terraclimate_water_balance.tif`,
  `ee/era5land_cdd.tif`) will be published as `v0.6.0` release assets and pinned in `src/pipeline/sources.rs`
  so rebuilds require no Earth Engine account.
