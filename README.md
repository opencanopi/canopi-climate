# canopi-climate

Current and future climate for any land location on Earth, for planting decisions.

Given a latitude and longitude, `canopi-climate` returns the climate of
1991–2020 and its projections for 2041–2070 and 2071–2099 under four emission
scenarios (SSP1-2.6, SSP2-4.5, SSP3-7.0, SSP5-8.5):

| Variable | Unit | Meaning |
|---|---|---|
| `koppen` | class | Köppen-Geiger class (most frequent class of Beck et al.'s 1-km map in the cell) |
| `tmin_cold` | °C | mean daily minimum of the coldest month |
| `tmax_warm` | °C | mean daily maximum of the warmest month |
| `tmean_cold`, `tmean_warm` | °C | mean of the coldest and warmest months |
| `precip` | mm/year | annual precipitation |
| `aridity` | ratio | annual precipitation / Thornthwaite potential evapotranspiration (capped at 300; no data where PET is below 1 mm/year, e.g. ice sheets) |
| `dry_months` | months | months with precipitation (mm) below twice the temperature (°C) (Gaussen) |

The `extremes` layer (0.3.0), on the same 0.1° cells, adds the annual
extremes that monthly means hide, for the same 9 periods and scenarios:

| Field | Unit | Meaning |
|---|---|---|
| `tmin_extreme_c` | °C | coldest night of a typical year (mean annual lowest daily minimum) |
| `tmax_extreme_c` | °C | hottest day of a typical year (mean annual highest daily maximum) |
| `usda_zone` | zone | USDA hardiness zone of `tmin_extreme_c` (`1a` … `13b`), derived at lookup |

`tmin_cold` (about 0 °C in central France) is a monthly mean of nightly
lows; `tmin_extreme` (about −8 °C there) is the night that decides whether a
plant survives the winter.

Two monthly climatology layers (0.4.0), for three slots (`1991-2020`, `2041-2070` SSP2-4.5, `2071-2099` SSP2-4.5), add the seasonal cycle:

| Layer | Resolution | Field | Unit | Meaning |
|---|---|---|---|---|
| `monthly` | 0.1° | `tmean_c`, `tmin_c` | °C | 12 monthly mean and mean-daily-minimum temperatures (0.5 °C step) |
| `monthly` | 0.1° | `dry_mask` | bits | 12-bit Gaussen dry-month mask (bit `m` = month `m+1` has `p < 2t`) |
| `monthly` | 0.1° | `gdd_base_10`, `chill_months` | °C·day, months | annual growing degree-days (base 10 °C) and chill-month count (`0 < tmean <= 7.2 °C`), derived at lookup |
| `monthly_precip` | 0.25° | `precip_mm` | mm/month | 12 monthly precipitation totals (1 mm step, land-cell mean) |

Two static agroecological risk layers and a compact Climate Twin index (0.5.0), built from ERA5-Land daily (1991–2020 / 2015–2025) and the historical climate grid:

| Layer / Artifact | Resolution | Fields | Meaning |
|---|---|---|---|
| `extremes_tail` | 0.1° | `tmin_p10_gap_c`, `tmax_p90_gap_c`, `heat_days_35c`, `recent_trend_c` | 1-in-10-yr cold/heat tail offsets, mean annual days ≥ 35 °C, and 2015–2025 vs 1991–2020 warming trend |
| `frost_calendar` | 0.1° | `last_spring_frost_doy`, `last_spring_frost_p90_doy`, `first_autumn_frost_doy`, `frost_free_days`, `frost_year_pct`, `false_spring_pct`, `regime` | Hemisphere-aware frost calendar anchored to thermal winter, plus false-spring killing-freeze risk (Tmin ≤ −2 °C after ≥ 150 GDD₅) |
| `analogues.bin` | 1.0° (~22k cells, 553 KB) | `findAnalogues` / `canopi-climate analogues` | Phase-invariant 8D z-score k-NN index for finding historical Climate Twins of future (2050/2085) climates in Rust & WASM |

Two site layers, on a finer 0.05° grid (about 5.5 km) in the same 10° tiles,
describe the place itself:

| Layer | Field | Unit | Meaning |
|---|---|---|---|
| soil | `ph_h2o` | pH | pH in water, 0–30 cm (SoilGrids 2.0, depth-weighted 0–5, 5–15, 15–30 cm) |
| soil | `sand_pct`, `clay_pct`, `silt_pct` | % | particle-size fractions, 0–30 cm (silt = 100 − sand − clay) |
| soil | `texture` | class | USDA texture class (`loam`, `silty_clay_loam`, …), derived at lookup |
| soil | `filled`, `fill_distance_cells` | –, cells | SoilGrids has no value on towns, water and ice; land cells within 8 cells of a measured cell take its values, and say how far it was |
| regions | `wgsrpd_l3` | code | WGSRPD level-3 botanical country (`FRA`, `BZL`, …; 369 areas), the scheme of WCVP native ranges |
| regions | `iso_a2` | code | country or territory, ISO 3166-1 alpha-2 (Natural Earth 1:50m map units: Réunion is RE, Svalbard SJ, Puerto Rico PR) |

Regions carry codes only, no names.

The crate is written in Rust. It reads a tiled grid of 0.1° cells (about 11 km
at the equator): one Cloud Optimized GeoTIFF per 10° × 10° tile, and a
`manifest.json` that describes the variables, scale/offset, periods,
scenarios, sources, and every tile's size and SHA-256. It runs natively and as
WebAssembly.

Part of [Canopi](https://github.com/opencanopi/canopi), the agroecological
design app. Status: early development; the API and the grid format may still
change (the manifest carries a format version).

## Use

### Rust

```rust
use canopi_climate::grid::TileId;
use canopi_climate::lookup::lookup;
use canopi_climate::tile::Tile;

let (lat, lon) = (48.8566, 2.3522);
let id = TileId::of(lat, lon).expect("valid coordinate");          // n50e000
let bytes = std::fs::read(format!("grid/tiles/{}", id.file_name()))?;
let tile = Tile::decode(&bytes)?;
if let Some(found) = lookup(&tile, lat, lon)? {
    let now = &found.periods[0];                                    // 1991-2020
    println!("{} {:.1} °C {:.0} mm", now.koppen, now.tmin_cold_c, now.precip_mm);
}
```

`lookup` uses the point's cell when it has every value (aridity may be
missing, see above), else the nearest
complete cell within 2 cells inside the same tile (the CLI and `sample`
also search the neighbouring tiles). It returns `None` at sea or
when no cell nearby has data. A missing tile file means the tile has no land.

The site layers work the same way, with their own tile files of the same
name:

```rust
use canopi_climate::{regions, soil};

let soil_tile = soil::decode(&std::fs::read(format!("grid/soil/{}", id.file_name()))?)?;
if let Some(s) = soil::lookup(&soil_tile, lat, lon)? {     // None: no soil within 2 cells
    println!("pH {} {:?} filled={}", s.ph_h2o, s.texture, s.filled);
}
let regions_tile = regions::decode(&std::fs::read(format!("grid/regions/{}", id.file_name()))?)?;
let r = regions::lookup(&regions_tile, lat, lon)?;          // fields None at sea
println!("{:?} {:?}", r.wgsrpd_l3, r.iso_a2);              // Some("FRA") Some("FR")
```

`soil::texture_class(sand_pct, clay_pct)` classifies any fractions. The
`lookup_across` variants search across tile edges.

### JavaScript (WebAssembly)

```js
import init, {
  tileName, lookup, lookupSoil, lookupRegions, lookupExtremes,
  lookupMonthly, lookupMonthlyPrecip, textureClass,
} from "./pkg/canopi_climate.js";
await init();
const name = tileName(lat, lon);                    // "n50e000.tif", or undefined
const get = async (dir) => {                        // 404: the layer has no data in the tile
  const res = await fetch(`/climate/${dir}/${name}`);
  return res.ok ? new Uint8Array(await res.arrayBuffer()) : null;
};
const tiles = await get("tiles"), soilTile = await get("soil"), regionsTile = await get("regions");
const climate = tiles ? lookup(tiles, lat, lon) : null;
const soil = soilTile ? lookupSoil(soilTile, lat, lon) : null;
const regions = regionsTile ? lookupRegions(regionsTile, lat, lon) : null;  // { wgsrpd_l3, iso_a2, ... }
// Extremes and monthly have no search of their own: read them at the climate answer's cell.
const extremesTile = climate ? await get("extremes") : null;
const extremes = extremesTile ? lookupExtremes(extremesTile, climate.cell_lat, climate.cell_lon) : null;
const monthlyTile = climate ? await get("monthly") : null;
const monthly = monthlyTile ? lookupMonthly(monthlyTile, climate.cell_lat, climate.cell_lon) : null;
const precipTile = await get("monthly_precip");
const monthlyPrecip = precipTile ? lookupMonthlyPrecip(precipTile, lat, lon) : null;
```

The results are plain objects with the fields of `Lookup` (`koppen`,
`tmin_cold_c`, …, one entry per period and scenario), `SoilLookup`,
`RegionsLookup`, `ExtremesLookup`, `MonthlyLookup` and `MonthlyPrecipLookup`, or `null`.
In the v0.4 build the median climate tile is 126 KB, monthly 80 KB, extremes 53 KB,
soil 45 KB, monthly_precip 14 KB and regions 1.4 KB.

### Command line

```sh
canopi-climate lookup 48.8566 2.3522 --grid out-v0.5          # JSON: climate, soil, regions, extremes, monthly, monthly_precip, extremes_tail, frost_calendar
canopi-climate analogues 45.7640 4.8357 --grid out-v0.5       # JSON: 2041-2070 SSP2-4.5 historical Climate Twins
canopi-climate sample points.csv --grid out-v0.5 -o climate.csv
canopi-climate sample points.csv --grid out-v0.5 --regions --extremes --monthly
canopi-climate verify-grid out-v0.5                           # sizes, SHA-256, every tile decoded
canopi-climate sources                                        # source files, URLs, SHA-256
```

`lookup` prints `{"climate": …, "extremes": …, "monthly": …, "monthly_precip": …, "regions": …, "soil": …}`; each part is
`null` when absent (exit code 2 when all are). For Paris:

```json
"soil": { "ph_h2o": 6.0, "sand_pct": 28.3, "clay_pct": 25.2, "silt_pct": 46.5, "texture": "loam",
          "filled": true, "fill_distance_cells": 3, "depth": "0-30 cm", "cell_lat": 48.875, ... },
"regions": { "wgsrpd_l3": "FRA", "iso_a2": "FR", "wgsrpd_l3_distance_cells": 0.0, ... }
```

`sample` reads a CSV with `lat`/`latitude` and `lon`/`lng`/`longitude`
columns (and optionally `id`). It writes one row per point and
period/scenario; points without data get one row with their status. It
samples climate only unless opt-in flags are passed: `--regions` (0.2.1)
appends `wgsrpd_l3` and `iso_a2`; `--extremes` (0.4.0) appends
`tmin_extreme_c`, `tmax_extreme_c` and `usda_zone`; `--monthly` (0.4.0)
appends `gdd_base_10`, `chill_months` and `dry_mask` (blank on slots not
present in `monthly`). Without flags the output is byte-identical to 0.2.0.

## Features

| Feature | Contents |
|---|---|
| default | grid geometry, tile decoding (`tiff` crate, Deflate only), lookup, Köppen classifier, derived measures (Thornthwaite PET, Gaussen dry mask, GDD base 10 °C, chill months), extremes, monthly, monthly_precip, soil and regions lookups, texture classes, manifest types |
| `pipeline` | grid builder and CLI: windowed GeoTIFF reading on `wbgeotiff`, NetCDF/HDF5 on `wbhdf`, Homolosine on `proj4rs`, polygons on `geo`/`geojson`, pinned sources, aggregation, COG writer, sampling (`--regions`, `--extremes`, `--monthly`) |
| `wasm` | `wasm-bindgen` exports: `tileName`, `lookup`, `lookupSoil`, `lookupRegions`, `lookupExtremes`, `lookupMonthly`, `lookupMonthlyPrecip`, `usdaZone`, `textureClass`, `version` |

There are no C libraries, no GDAL and no HTTP client: the crate does not
download anything.

## Building the grid

1. Download the source files listed by `canopi-climate sources` into one
   directory, keeping the listed relative paths (the two Beck et al. zip
   archives are extracted in place). Each file is checked against its pinned
   SHA-256.
2. Build stage by stage (each stage writes to a work directory and is skipped
   when its outputs exist; every stage stays under a few minutes and 3 GB of
   memory):

```sh
cargo build --release --features pipeline
B=target/release/canopi-climate
$B build-grid --sources /data/climate --out out-v0.4 --stage verify
$B build-grid --sources /data/climate --out out-v0.4 --stage koppen
$B build-grid --sources /data/climate --out out-v0.4 --stage chelsa
$B build-grid --sources /data/climate --out out-v0.4 --stage beck    # or --beck-period 2041_2070_ssp245 ...
$B build-grid --sources /data/climate --out out-v0.4 --stage check-future   # optional, needs the 5 CHELSA GCM files
$B build-grid --sources /data/climate --out out-v0.4 --stage tiles
$B build-grid --sources /data/climate --out out-v0.4 --stage regions        # 25 s
$B build-grid --sources /data/climate --out out-v0.4 --stage soil           # 10 s; needs regions (land mask)
$B build-grid --sources /data/climate --out out-v0.4 --stage extremes       # needs the two Earth Engine rasters below
$B build-grid --sources /data/climate --out out-v0.4 --stage chelsa-monthly # 12 CHELSA V2.1 tasmin rasters
$B build-grid --sources /data/climate --out out-v0.4 --stage monthly        # 0.1° monthly tmean, tmin, dry_mask
$B build-grid --sources /data/climate --out out-v0.4 --stage monthly-precip # 0.25° monthly precip
$B verify-grid out-v0.4
```

The `extremes` stage reads two rasters computed on Google Earth Engine by
`scripts/ee/extremes.py`. Both are assets of the v0.3.0 release (the pinned
URLs), so a rebuild needs no Earth Engine account. Recomputing them needs an
Earth Engine project and Google Drive (about 20 MB downloaded; each model
export takes about an hour of Earth Engine time):

```sh
python scripts/ee/extremes.py --project <id> baseline           # ERA5-Land 1991-2020 gaps, 0.1°
python scripts/ee/extremes.py --project <id> future             # one export per NEX-GDDP-CMIP6 model
python scripts/ee/extremes.py --project <id> status
python scripts/ee/extremes.py --project <id> download --out /data/climate/ee   # into ee/drive
python scripts/ee/extremes.py --project <id> combine --out /data/climate/ee    # the 2 build inputs
```

The release directory holds `tiles/*.tif`, `soil/*.tif`, `regions/*.tif`,
`manifest.json`, `SHA256SUMS`, `ATTRIBUTION.md` and `build-report.json`.
The report counts land cells, the cells where the Köppen class contradicts
the cell's own monthly means, clamped extremes, and source comparisons; its
`layers` part counts the soil fill by distance, the land cells without soil
per country, and region codes present, missing and set at an interior point.
The `regions` and `soil` stages add their layer to the release written by
`tiles` (and rewrite `manifest.json`, `ATTRIBUTION.md` and `SHA256SUMS`);
`--force` rebuilds one. Re-running `tiles` keeps the layers. Rebuilding
`regions` removes the `soil` layer, whose fill uses the regions as land
mask; the next `soil` stage rebuilds it.

### Method

- **Köppen class**: most frequent class among the 12 × 12 one-kilometre
  cells of Beck et al. (2023), per period and scenario. For 1991–2020 this
  equals Beck's own 0.1° map in every cell.
- **Monthly means, precipitation, aridity, dry months**: from Beck et al.'s
  0.1° monthly temperature and precipitation climatologies. Future slots use
  the ensemble mean of their constrained CMIP6 models.
- **Coldest-month minimum, warmest-month maximum**: CHELSA V2.1 bio6/bio5
  (1981–2010), averaged over the cell. They are moved to 1991–2020 by a third
  of Beck's 1961–1990 → 1991–2020 change. Future values add Beck's change of the
  coldest/warmest monthly mean (delta method). They are clamped so that the
  minimum never exceeds the coldest-month mean and the maximum never falls
  below the warmest-month mean.

- **Coldest night, hottest day** (`docs/decisions/0005-annual-extremes.md`):
  the grid's `tmin_cold` (`tmax_warm`) plus the ERA5-Land 1991–2020 gap
  between the mean annual extreme and that monthly value, plus a change with
  warming: the median over 16 NEX-GDDP-CMIP6 models (one per model family) of how much the gap
  moves per degree of coldest-month (warmest-month) warming, times each
  slot's warming in Beck et al. ERA5-Land (about 9 km) misses cold-air
  pools: in hilly country the coldest night can be 2–3 °C milder than at a
  station in the valley.

- **Soil**: each 0.05° cell centre is projected to Interrupted Goode
  Homolosine (twelve `proj4rs` sinusoidal/Mollweide lobes, equal to PROJ's
  `igh` within 1 mm) and takes the SoilGrids 5-km pixel under it, for the
  nine depth/variable files; then the 0–30 cm depth-weighted mean. Land
  (any region code) without a value takes the nearest measured cell within
  8 cells (about 44 km at the equator).
- **Regions**: each cell takes the polygon that contains its centre. Areas
  too small for any centre (islets, Monaco, Vatican, …) take the cell of
  their interior point, so all 369 WGSRPD areas and all 247 Natural Earth
  codes are present. Countries come from Natural Earth's map units, which
  split overseas departments and territories from their sovereign, so they
  get their own ISO code (Réunion RE, French Guiana GF, Svalbard and Jan
  Mayen SJ, Bonaire BQ, Christmas Island CX, Hong Kong HK, Greenland GL).
  Natural Earth's -99 codes are resolved from `ISO_A2_EH` and the unit code
  (Norway NO, England GB, Kosovo XK, Somaliland SO, Northern Cyprus CY;
  Taiwan TW; Siachen has no code). Gibraltar, Bouvet Island and the US
  Minor Outlying Islands have no unit at 1:50m.

See `docs/validation.md` for a comparison with ERA5 at 29 cities and the
site-layer checks, and `docs/decisions/` for design decisions (0003: site
layers, 0004: monthly layers, 0005: annual extremes).

## Data sources and licences

The grid is derived from the datasets below. Their licences apply to the grid,
and their citations go with any use of it (`ATTRIBUTION.md` in each release).

- **Beck et al. (2023)**: Köppen-Geiger maps (1 km) and 0.1° monthly
  climatologies, 1961–2099. Beck, H. E., McVicar, T. R., Vergopolan, N.,
  Berg, A., Lutsko, N. J., Dufour, A., Zeng, Z., Jiang, X., van Dijk, A. I. J. M.,
  Miralles, D. G. (2023). High-resolution (1 km) Köppen-Geiger maps for
  1901–2099 based on constrained CMIP6 projections. *Scientific Data* 10, 724.
  <https://doi.org/10.1038/s41597-023-02549-6>. Data: figshare,
  <https://doi.org/10.6084/m9.figshare.21789074.v2>, licensed **CC BY 4.0**
  there. The authors' GloH2O page links CC BY-NC 4.0 instead. The Canopi
  project has the authors' permission to use the data.
- **CHELSA V2.1**: bio5, bio6, bio12 and monthly `tasmin` (months 01–12) for 1981–2010, licensed **CC0 1.0**.
  Karger, D. N., Conrad, O., Böhner, J., Kawohl, T., Kreft, H., Soria-Auza, R. W.,
  Zimmermann, N. E., Linder, H. P., Kessler, M. (2017). Climatologies at high
  resolution for the earth's land surface areas. *Scientific Data* 4, 170122.
  <https://doi.org/10.1038/sdata.2017.122>. Karger, D. N., et al. (2021). CHELSA
  V2.1, EnviDat, <https://doi.org/10.16904/envidat.228>. Brun, P.,
  Zimmermann, N. E., Hari, C., Pellissier, L., Karger, D. N. (2022). Global
  climate-related predictors at kilometre resolution for the past and future.
  *Earth System Science Data* 14, 5573–5603.
  <https://doi.org/10.5194/essd-14-5573-2022>.

- **ERA5-Land** daily aggregates (`ECMWF/ERA5_LAND/DAILY_AGGR` on Google
  Earth Engine), 1991–2020 daily minimum and maximum temperature, licensed
  **CC BY 4.0** (Copernicus). Muñoz-Sabater, J., et al. (2021). ERA5-Land: a
  state-of-the-art global reanalysis dataset for land applications. *Earth
  System Science Data* 13, 4349–4383. <https://doi.org/10.5194/essd-13-4349-2021>.
  Contains modified Copernicus Climate Change Service information.
- **NEX-GDDP-CMIP6** (`NASA/GDDP-CMIP6` on Google Earth Engine), daily
  `tas`, `tasmin`, `tasmax` of 16 models (one per model family), historical, SSP2-4.5 and SSP5-8.5.
  Thrasher, B., Wang, W., Michaelis, A., Melton, F., Lee, T., Nemani, R.
  (2022). NASA Global Daily Downscaled Projections, CMIP6. *Scientific Data*
  9, 262. <https://doi.org/10.1038/s41597-022-01393-4>. Made available by
  NASA under a blanket **CC0 1.0** licence (since September 2022).

- **SoilGrids 2.0**: pH (water), sand and clay, 0–5, 5–15 and 15–30 cm,
  5-km aggregates of the mean prediction, licensed **CC BY 4.0**. Poggio, L.,
  de Sousa, L. M., Batjes, N. H., Heuvelink, G. B. M., Kempen, B., Ribeiro, E.,
  Rossiter, D. (2021). SoilGrids 2.0: producing soil information for the globe
  with quantified spatial uncertainty. *SOIL* 7, 217–240.
  <https://doi.org/10.5194/soil-7-217-2021>. Data: ISRIC,
  <https://files.isric.org/soilgrids/latest/data_aggregated/5000m/>.
- **Natural Earth** 1:50m Admin 0 – Map Units, v5.1.2, **public domain**
  ("Made with Natural Earth").
- **WGSRPD** level 3: Brummitt, R. K. (2001). *World Geographical Scheme for
  Recording Plant Distributions*, Edition 2. TDWG / Hunt Institute.
  Geometry from <https://github.com/tdwg/wgsrpd> (commit f68afd9). The
  repository declares no licence; the standard is published by TDWG for
  reuse and is cited here. A licence statement from TDWG is a follow-up.

The validation also uses ERA5 through the Open-Meteo archive API (Open-Meteo,
CC BY 4.0; Hersbach et al. 2020, Copernicus Climate Change Service). It is not
part of the grid.

## Development

The tile format is specified in [docs/format.md](docs/format.md) (normative); a change to it
follows its compatibility rule.

One-time setup per checkout, for the tracked git hooks (pre-commit: `cargo fmt --check`; pre-push:
clippy and the tests, the pipeline tests when the push touches them). Cargo in the hooks runs in a
memory-capped `systemd-run` scope when one is available:

```sh
git config core.hooksPath .githooks
```

```sh
cargo fmt --all --check
cargo clippy --all-targets --features pipeline -- -D warnings   # covers the default build
cargo clippy --all-targets --features wasm -- -D warnings
cargo test && cargo test --features pipeline                     # includes the synthetic-vectors staleness check
UPDATE_FIXTURES=1 cargo test                                     # regenerate tests/fixtures/synthetic-expected.json
cargo build --target wasm32-unknown-unknown --profile wasm --features wasm
wasm-bindgen --target nodejs --out-dir pkg-node target/wasm32-unknown-unknown/wasm/canopi_climate.wasm
node scripts/wasm-smoke.cjs                                      # includes the single-tile synthetic vectors
cargo deny check bans licenses sources                           # cargo-deny 0.20
CANOPI_CLIMATE_GRID=out-v0.4 cargo test --release --features pipeline -- --ignored   # against a built grid
canopi-climate vectors --grid out-v0.4 --points vectors/points.csv --check vectors/lookup.json
```

CI tiers:

| Where | When | What |
|---|---|---|
| `.githooks` | commit, push | fmt; clippy and tests |
| `ci.yml` | every push and pull request | fmt, clippy (pipeline, wasm), tests, cargo-deny, wasm build and smoke |
| `ci.yml` grid job | every push to main, by hand | the `#[ignore]` tests, `verify-grid` and the `vectors/lookup.json` check, on the grid pinned in `release/grid.json` (cached by SHA-256) |
| `advisories.yml` | push to main, weekly, by hand | osv-scanner over `Cargo.lock`, exceptions in `osv-scanner.toml` |
| `release.yml` | a `v*` tag, by hand | grid tests, vectors regenerated and checked, size report (read-only token); a separate job uploads `lookup.json`, its SHA-256 and the size report to the release (the only write token; `--clobber`, safe to re-run) |

To release, bump `version` in `Cargo.toml`, merge to main, then create the release with
`gh release create vX.Y.Z --target main`: it creates the tag and the release together, and the tag
push starts `release.yml` with the release already there. Pushing a tag first starts the run before
the release exists and its upload job fails; then create the release and run `release.yml` by hand
with `tag=vX.Y.Z`. The grid job downloads the pinned grid from this repository's release, or from
the `url` in `release/grid.json` when this repository does not have it (a fork).

Dependency updates come from Renovate (`renovate.json`) once its GitHub app is installed.

## Licence

AGPL-3.0-or-later (see `LICENSE`). Climate, soil and boundary datasets keep their own licences
and citations (above, and `ATTRIBUTION.md` in each release).
