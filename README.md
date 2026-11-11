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

No hardiness zone is derived: `tmin_cold` is a monthly mean, not the annual
extreme minimum that USDA zones need.

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
complete cell within 2 cells inside the same tile. It returns `None` at sea or
when no cell nearby has data. A missing tile file means the tile has no land.

### JavaScript (WebAssembly)

```js
import init, { tileName, lookup } from "./pkg/canopi_climate.js";
await init();
const name = tileName(lat, lon);                    // "n50e000.tif", or undefined
const res = await fetch(`/climate/tiles/${name}`);  // 404: no land in the tile
const climate = res.ok ? lookup(new Uint8Array(await res.arrayBuffer()), lat, lon) : null;
```

The result is a plain object with the fields of `Lookup`
(`koppen`, `tmin_cold_c`, …, one entry per period and scenario), or `null`.

### Command line

```sh
canopi-climate lookup 48.8566 2.3522 --grid out-v0.1          # JSON
canopi-climate sample points.csv --grid out-v0.1 -o climate.csv
canopi-climate verify-grid out-v0.1                           # sizes and SHA-256
canopi-climate sources                                        # source files, URLs, SHA-256
```

`sample` reads a CSV with `lat`/`latitude` and `lon`/`lng`/`longitude`
columns (and optionally `id`). It writes one row per point and
period/scenario; points without data get one row with their status.

## Features

| Feature | Contents |
|---|---|
| default | grid geometry, tile decoding (`tiff` crate, Deflate only), lookup, Köppen classifier, derived measures, manifest types |
| `pipeline` | grid builder and CLI: windowed GeoTIFF reading on `wbgeotiff`, NetCDF/HDF5 on `wbhdf`, pinned sources, aggregation, COG writer, sampling |
| `wasm` | `wasm-bindgen` exports: `tileName`, `lookup`, `version` |

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
$B build-grid --sources /data/climate --out out-v0.1 --stage verify
$B build-grid --sources /data/climate --out out-v0.1 --stage koppen
$B build-grid --sources /data/climate --out out-v0.1 --stage chelsa
$B build-grid --sources /data/climate --out out-v0.1 --stage beck    # or --beck-period 2041_2070_ssp245 ...
$B build-grid --sources /data/climate --out out-v0.1 --stage check-future   # optional, needs the 5 CHELSA GCM files
$B build-grid --sources /data/climate --out out-v0.1 --stage tiles
$B verify-grid out-v0.1
```

The release directory holds `tiles/*.tif`, `manifest.json`, `SHA256SUMS`,
`ATTRIBUTION.md` and `build-report.json`. The report counts land cells,
the cells where the Köppen class contradicts the cell's own monthly means,
clamped extremes, and source comparisons.

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

See `docs/validation.md` for a comparison with ERA5 at 22 cities, and
`docs/decisions/` for design decisions.

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
- **CHELSA V2.1**: bio5, bio6 and bio12 for 1981–2010, licensed **CC0 1.0**.
  Karger, D. N., Conrad, O., Böhner, J., Kawohl, T., Kreft, H., Soria-Auza, R. W.,
  Zimmermann, N. E., Linder, H. P., Kessler, M. (2017). Climatologies at high
  resolution for the earth's land surface areas. *Scientific Data* 4, 170122.
  <https://doi.org/10.1038/sdata.2017.122>. Karger, D. N., et al. (2021). CHELSA
  V2.1, EnviDat, <https://doi.org/10.16904/envidat.228>. Brun, P.,
  Zimmermann, N. E., Hari, C., Pellissier, L., Karger, D. N. (2022). Global
  climate-related predictors at kilometre resolution for the past and future.
  *Earth System Science Data* 14, 5573–5603.
  <https://doi.org/10.5194/essd-14-5573-2022>.

The validation also uses ERA5 through the Open-Meteo archive API (Open-Meteo,
CC BY 4.0; Hersbach et al. 2020, Copernicus Climate Change Service). It is not
part of the grid.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings                     # also --features pipeline, --features wasm
cargo test && cargo test --features pipeline
cargo build --target wasm32-unknown-unknown --profile wasm --features wasm
wasm-bindgen --target nodejs --out-dir pkg-node target/wasm32-unknown-unknown/wasm/canopi_climate.wasm
node scripts/wasm-smoke.cjs
CANOPI_CLIMATE_GRID=out-v0.1 cargo test --release --features pipeline -- --ignored   # against a built grid
```

## Licence

AGPL-3.0-or-later (see `LICENSE`). Climate datasets keep their own licences
and citations (above, and `ATTRIBUTION.md` in each release).
