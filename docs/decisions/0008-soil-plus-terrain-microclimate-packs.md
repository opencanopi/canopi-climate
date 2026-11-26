# 0008: Deep soil (`soil_plus`), terrain (`terrain`), WASM microclimate & trajectory engine, and decoupled layer packs

- Status: accepted (2026-11-23)
- Context: Through `v0.6.0`, `canopi-climate` provides Köppen climate, annual and 1-in-10-year extremes,
  frost calendar, monthly temperatures/precipitation, 2050 Climate Twin analogues, Dynamic Model Chill
  Portions, photoperiod, water balance (CWD/CDD/VPD), and 0–30 cm topsoil pH/texture/AWC. Four capabilities
  are needed to complete the Plant Palette engine and keep the grid scalable for `canopi-website`:
  1. **Deep soil & physical properties (`soil_plus/`)**: Saxton–Rawls AWC needs real soil organic carbon
     (replacing the `2.5%` OM default) and coarse-fragment rock discount; deep-rooted trees and shrubs also
     depend on subsoil (30–100 cm) pH (calcareous chlorosis), subsoil clay (claypan drainage), subsoil rock
     content (rooting depth), and topsoil bulk density (compaction/peat).
  2. **Topography & drainage (`terrain/`)**: site elevation, local relief, slope, and Height Above Nearest
     Drainage (HAND) distinguish floodplains, frost hollows, and mountain microclimates from 0.1° cell means.
  3. **Zero-byte WASM `microclimate` & `trajectory` engine**: client-side topoclimatic adjustment
     (elevation lapse rate, katabatic cold-air pooling, tilted-surface solar irradiance) and multi-period
     species suitability trajectory evaluation (`today` → `2041–2070` → `2071–2099`) with typed
     `establishment_vs_maturity_gap` diagnostics.
  4. **Decoupled layer packs (`packs-vX.Y.Z.json`)**: by `v0.7.0`, the full monolithic tar reaches
     ~250–300 MB. Dual-shipping modular per-layer-group `.tar` packs lets `canopi-website` and CLI users
     cache unchanged `core` tiles across releases and opt into layers incrementally under `format_version: 1`.
- Builds on [0001](0001-tile-format.md), [0003](0003-site-layer-formats.md),
  [0004](0004-monthly-and-relief-layers.md), [0005](0005-annual-extremes.md),
  [0006](0006-tail-extremes-frost-calendar-analogues.md), and
  [0007](0007-phenology-and-water-balance.md).
  Normative contract: [docs/format.md](../format.md).

## Decisions

### 1. Split `soil_plus/` (0.05°) from `terrain/` (0.1°)

Packing all 10 new bands at 0.05° would add ~50–60 MB. Splitting along physical resolution and coverage
mirrors ADR 0003 (`soil/` vs `regions/`), stays within a ~24–31 MB budget, and aligns `terrain/` directly
with the 0.1° climate cell whose elevation `microclimate::adjust` lapses from.

#### 1.1 `soil_plus` layer (`soil_plus/<name>.tif`): static 0.05°, 6 bands (~18–24 MB)

- **Geometry & container:** 0.05° grid (200 × 200 cells per 10° tile, internal tile 208), static (no `slots`),
  6 `int16` bands, offset `0`, `NODATA = -32768`.
- **Source:** SoilGrids 2.0 5-km aggregates (ISRIC, Interrupted Goode Homolosine `ESRI:54052`, **CC BY 4.0**),
  15 GeoTIFFs pinned in `src/pipeline/sources.rs` (3 depths `0–5`, `5–15`, `15–30 cm` with weights `5, 10, 15`
  for `soc`, `bdod`, `cfvo`; 2 depths `30–60`, `60–100 cm` with weights `30, 40` for `cfvo`, `phh2o`, `clay`).
- **Bands:**
  - `0`: `soc_0_30` (`g/kg`, scale `0.5`, stored `round(raw_dg_kg / 5.0)`) — feeds Saxton–Rawls AWC via
    `OM% = 0.1724 * soc_g_kg` (0.086% OM resolution).
  - `1`: `bdod_0_30` (`g/cm3`, scale `0.01`, native `cg/cm³`) — flags compacted (`> 1.55`) and organic/peat
    (`< 0.90`) soils.
  - `2`: `cfvo_0_30` (`%`, scale `1.0`, stored `round(raw_cm3_dm3 / 10.0)`) — topsoil coarse fragments;
    discounts topsoil AWC by `(1 - cfvo_0_30 / 100)`.
  - `3`: `cfvo_30_100` (`%`, scale `1.0`, stored `round(raw_cm3_dm3 / 10.0)`) — subsoil coarse fragments /
    skeletal rooting-depth proxy (`> 35%` restricts deep taproots).
  - `4`: `ph_30_100` (`pH`, scale `0.1`, native `pH × 10`) — subsoil pH; calcareous chlorosis proxy
    (`>= 7.5–7.8` triggers active-lime chlorosis even when topsoil is humus-acidified).
  - `5`: `clay_30_100` (`%`, scale `1.0`, stored `round(raw_g_kg / 10.0)`) — subsoil clay; drainage /
    claypan proxy (`>= 40%` or `clay_30_100 - clay_0_30 >= 15%`).
- **No duplicate `fill_distance` band:** reuses `soil/`'s exact 8-cell land-only fill geometry. If a measured
  0–30 cm cell lacks a 30–100 cm depth value, fall back to its 0–30 cm counterpart (for `cfvo`, `ph`, `clay`)
  or fill from the nearest measured cell within 8 cells so `soil_plus` maintains mask parity with `soil/`.
  Quantizing `cfvo_*` and `clay_30_100` to whole `%` (well inside SoilGrids' 5–12% RMSE) trims noisy LSBs
  and saves ~35–45% Deflate bytes.
- **Lookup:** 0.05° nearest search within radius 2 cells (same as `soil/`), or read at the `soil/` answer's
  cell when both are present.

#### 1.2 `terrain` layer (`terrain/<name>.tif`): static 0.1°, 4 bands (~5–7 MB)

- **Geometry & container:** 0.1° climate grid (100 × 100 cells per 10° tile, internal tile 112), static
  (no `slots`), 4 `int16` bands, offset `0`, `NODATA = -32768`.
- **Sources & licences:**
  - `COPERNICUS/DEM/GLO30_2024_1` (~30 m, 90°S–90°N, Copernicus Data Policy — commercial use permitted with
    EU/ESA/DLR/Airbus attribution). Because GLO-30 is an `ee.ImageCollection` with tile-varying projections,
    `scripts/ee/v07_terrain.py` MUST call `.select('DEM').mosaic().setDefaultProjection('EPSG:4326', None, 30)`
    before `ee.Terrain.slope()` or `reduceResolution`. **Never mask `DEM <= 0`** as ocean (preserves the
    Netherlands, Dead Sea, Death Valley, Caspian basin); ocean masking happens at tile-build time against
    canopi's complete-climate land mask.
  - `MERIT/Hydro/v1_0_1` (~90 m, 60°S–90°N, band `hnd`) — dual-licensed CC BY-NC 4.0 or **ODbL 1.0**.
    `canopi-climate` explicitly elects and declares **ODbL 1.0** in `src/pipeline/sources.rs` and
    `ATTRIBUTION.md` for `MERIT/Hydro/v1_0_1` (open share-alike database licence, compatible with AGPL-3.0
    open tile publication). Outside MERIT-Hydro's 60°S–90°N coverage (Antarctica, sub-Antarctic islands) and
    unmapped small islands, `hand_m` stays `NODATA` (at most a 1-cell coastal fallback on land; never fill
    across sea or default to `0 m`).
- **Bands (exported directly on canopi's 0.1° cell grid `3600 × 1800` via `reduceResolution`):**
  - `0`: `elev_m` (`m`, scale `1.0`, signed `int16`) — cell mean elevation from GLO-30 (`-430 m` Dead Sea to
    `8848 m` Everest).
  - `1`: `relief_m` (`m`, scale `1.0`, `>= 0`) — within-cell elevation relief (`p95 - p05` of GLO-30 `DEM`),
    bounding lapse-rate clamping and characterizing topographic complexity.
  - `2`: `slope_deg` (`deg`, scale `0.1`, `0..=90`) — median terrain slope from GLO-30.
  - `3`: `hand_m` (`m`, scale `0.1`, clamped to `[0, 3000]` m; MAY be `NODATA` outside 60°S–90°N) — median
    Height Above Nearest Drainage from MERIT-Hydro (`< 2 m` floodplain/wetland, `2–5 m` moist toe-slope,
    `> 15 m` upland).
- **Lookup:** no spatial search; read at the climate answer's `(cell_lat, cell_lon)` in its `tile`
  (`terrain::at_cell`; WASM `lookupTerrain(tile, cell_lat, cell_lon)`). A cell is valid whenever `elev_m`,
  `relief_m`, and `slope_deg` are set (`hand_m` is optional `Option<f64>`).

### 2. Zero-byte WASM `microclimate` & `trajectory` engine

#### 2.1 `microclimate::adjust(site, site_elev_m, slope_deg, aspect_deg, topo_position)`

- **Input guards:** reject non-finite inputs; clamp `slope_deg` to `[0.0, 60.0]`; normalize `aspect_deg` via
  `.rem_euclid(360.0)`. Gracefully degrades when optional layers (`extremes`, `extremes_tail`,
  `frost_calendar`, `monthly`, `phenology`, `water_balance`, `terrain`) are absent.
- **Elevation lapse rate (`Γ = -0.0065 °C/m`):**
  - `delta_elev_m = site_elev_m - cell_elev_m` (`0` if `site_elev_m` or `terrain` is absent).
  - Clamp `effective_delta_elev_m` to `±max(cell_relief_m / 2.0, 150.0)` with a hard cap of `±1500.0 m`,
    reporting `elev_clamped: bool` so user typos (e.g. `4000 m` in Paris) cannot produce unphysical climates.
  - Apply `ΔT_lapse = -0.0065 * effective_delta_elev_m` to all temperature fields (`tmean*`, `tmin*`,
    `tmax*`, monthly `tmean[12]`/`tmin[12]`) and re-derive `usda_zone`, `gdd_base_10`, `chill_months`, and
    `chill_portions` (evaluating `v0.6.0`'s `ΔT` sensitivity curve at `ΔT_future + ΔT_lapse`).
- **Katabatic cold-air pooling (nighttime minimums only):**
  - `TopoPosition`: `Ridge`, `UpperSlope`, `MidSlope` (neutral default), `LowerSlope`, `ValleyBottom`,
    `FrostHollow`.
  - Offsets applied **only** to nighttime minimums (`tmin_extreme`, `tmin_p10`, `tmin_cold`, monthly
    `tmin[12]`, and `frost_calendar`; never daytime `tmax`):
    - `Ridge`: `+1.0 °C` extreme / `+0.5 °C` monthly `tmin`
    - `UpperSlope`: `+0.5 °C` / `+0.25 °C`
    - `MidSlope`: `0.0 °C` / `0.0 °C`
    - `LowerSlope`: `-1.5 °C` / `-0.8 °C`
    - `ValleyBottom`: `-3.0 °C` / `-1.5 °C`
    - `FrostHollow`: `-5.0 °C` / `-2.5 °C`
  - Enforce `tmin[m] <= tmean[m]` after adjustment.
  - Shift `frost_calendar` spring/autumn DOYs by `3.5 days / °C` of total nighttime delta
    (`ΔT_lapse + ΔT_pool`), preserving sentinels `0` (frost-free) and `366` (year-round frost).
- **Tilted-surface monthly solar irradiance ratio (48-step ENU vector integration):**
  - For each `MID_MONTH_DOY[m]` (reusing `src/derived.rs` declination `δ = 0.409 * sin(2π * doy / 365 - 1.39)`),
    integrate 48 half-hour steps `ω ∈ [-π, π]` using ENU unit vectors (NH, SH, E/W aspect, and polar safe):
    - Sun: `s_E = -cos δ sin ω`, `s_N = sin δ cos φ - cos δ sin φ cos ω`,
      `s_U = sin δ sin φ + cos δ cos φ cos ω`.
    - Slope normal (`β` = slope, `A` = compass aspect `0°=N, 90°=E, 180°=S, 270°=W`):
      `n_E = sin β sin A`, `n_N = sin β cos A`, `n_U = cos β`.
    - `cos θ_i = s_E n_E + s_N n_N + s_U n_U` (beam active when `s_U > 0 && cos θ_i > 0`).
    - Isotropic diffuse fraction `k_d = 0.25` with sky-view factor `(1 + cos β) / 2`:
      `I_tilted = (1 - k_d) * max(0, cos θ_i) + k_d * max(0, s_U) * (1 + cos β) / 2`,
      `I_horiz = max(0, s_U)`.
    - Polar night guard: when `Σ I_horiz < 1e-6`, ratio = `1.0`.
  - Outputs nested `solar_ratio: { monthly: [f64; 12], annual: f64, growing_season: f64 }` and
    optional `cwd_adjusted_mm = round(cwd_mm * solar_ratio.growing_season.clamp(0.7, 1.3))`.

#### 2.2 `evaluate_trajectory(site, species_envelope)`

- **Partial-profile safety:** all envelope bounds are `Option<f64>`; only supplied constraints are evaluated.
- **6 canonical limiting-factor margins** (positive = headroom, negative = deficit):
  1. `cold_extreme` (`°C`): `site.tmin_extreme - env.min_tmin_extreme` (uses `tmin_p10` when available).
  2. `heat_extreme` (`°C`): `env.max_tmax_extreme - site.tmax_extreme` (uses `tmax_p90` when available).
  3. `water_balance` (`mm` or `months`): `env.max_cwd_mm - site.cwd_mm` (falls back to `precip` / `dry_months`
     when `water_balance` is absent).
  4. `chill` (`CP`): `site.chill_portions - env.min_chill_portions` (projected per period).
  5. `soil_ph` (`pH`): `min(ph_eff - env.min_ph, env.max_ph - ph_eff)` checking both topsoil `ph_h2o` and
     subsoil `ph_30_100` for calcareous chlorosis.
  6. `drainage` (`m` HAND): `site.hand_m - env.min_hand_m` (plus claypan flag when `clay_30_100 >= 40%`).
  *(Optional 7th: `false_spring` risk flag when `frost_calendar.false_spring_pct` exceeds tolerance.)*
- **Outputs (`TrajectoryResult`):**
  - `periods`: `{ today, mid_century, late_century }` (default SSP2-4.5, configurable), each with
    `suitability` (`Optimal | Suitable | Marginal | Unsuitable`), bottleneck `score` (`0.0..=1.0`),
    `margins: Vec<FactorMargin>`, and `limiting_factor: Option<FactorId>`.
  - `trajectory_class`: `Stable | Improving | Declining | Transitional | Unsuitable`.
  - `establishment_vs_maturity_gap`: typed enum `None | ColdEstablishmentBarrier | FutureMaturityRisk | Both`
    (plus `has_gap: bool`).
  - `evaluated_layers`: list of layer IDs present at evaluation time.

### 3. Decoupled layer packs (`packs-vX.Y.Z.json`, `format_version: 1`)

- **Dual-shipping layout:** continue publishing the monolithic `canopi-climate-grid-vX.Y.Z.tar` alongside
  five uncompressed modular `.tar` packs and a top-level `packs-vX.Y.Z.json` index (+ `.sha256`):
  - `canopi-climate-pack-core-vX.Y.Z.tar`: `manifest.json`, `ATTRIBUTION.md`, `build-report.json`,
    `SHA256SUMS.pack`, `tiles/`, `soil/`, `regions/`, `extremes/` (~105 MB).
  - `canopi-climate-pack-monthly-vX.Y.Z.tar`: `SHA256SUMS.pack`, `monthly/`, `monthly_precip/` (~75 MB).
  - `canopi-climate-pack-v05-vX.Y.Z.tar`: `SHA256SUMS.pack`, `extremes_tail/`, `frost_calendar/`,
    `analogues.bin` (~20 MB).
  - `canopi-climate-pack-v06-vX.Y.Z.tar`: `SHA256SUMS.pack`, `phenology/`, `water_balance/` (~10–25 MB).
  - `canopi-climate-pack-v07-vX.Y.Z.tar`: `SHA256SUMS.pack`, `soil_plus/`, `terrain/` (~24–31 MB).
- **No SHA-256 cycle:** pack hashes live ONLY in the top-level `packs-vX.Y.Z.json` release asset (never
  inside `manifest.json`, which lives inside `pack-core`). Consequently, `manifest.json` is byte-for-byte
  identical whether installed from the monolithic tar or assembled from packs, `format_version` remains `1`,
  and `climate_tiles_sha256` keeps downstream indexes (`consortium-index.json`) decoupled from additive packs.
- **`canopi-website` handoff:** `install-climate.mjs` should switch to streaming ustar extraction (cutting
  peak RAM from ~300 MB to < 10 MB), derive its allowed tile prefixes dynamically from `manifest.json`, and
  accept either monolithic `file` or modular `packs[]` in `climate-release.json`.

## Consequences

- `format_version` stays `1`; `climate_tiles_sha256` and all existing tiles remain unchanged.
- Total files across all 10 layers stay around ~5,100–5,600 (well below Cloudflare Workers' 20,000-file cap).
- `MERIT/Hydro/v1_0_1` introduces **ODbL 1.0** share-alike attribution for the `hand_m` band in `terrain/`;
  this is explicitly recorded in `src/pipeline/sources.rs` and `ATTRIBUTION.md`.
- The 15 SoilGrids 2.0 5-km rasters are pinned directly from ISRIC (CC BY 4.0); `ee/terrain_glo30_merit.tif`
  will be published as a `v0.7.0` release asset and pinned in `src/pipeline/sources.rs` once exported.
