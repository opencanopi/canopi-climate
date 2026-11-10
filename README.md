# canopi-climate

Current and future climate for any location on Earth, for planting decisions.

Given a latitude and longitude, `canopi-climate` returns the climate of 1991–2020
and its projections for 2041–2070 and 2071–2099 under several emission scenarios:
coldest and warmest months, precipitation and aridity, the Köppen-Geiger class
and a hardiness zone. It is written in Rust, runs natively and as WebAssembly,
and reads a compact tiled grid built from published, openly licensed climate
datasets (sources and their citations are listed with each data release).

Part of [Canopi](https://github.com/opencanopi/canopi), the agroecological design
app. Status: early development; the API and the grid format will change.

## Licence

AGPL-3.0-or-later (see `LICENSE`). Climate datasets keep their own licences and
citations, listed in each data release.
