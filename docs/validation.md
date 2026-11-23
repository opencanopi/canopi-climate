# Validation

Grid v0.6 (`out-v0.6`) against ERA5 1991–2020 normals at 29 cities covering
every Köppen main group (the 22 core cities plus 7 Plant Palette persona
cities: Kiambu, Recife, Oulu, Mendoza, Kumasi, Osaka, Riyadh). Regenerate this
table after a grid change; the commands are in `examples/validation.rs`.

## Method

- **Reference**: ERA5 daily `temperature_2m_min`, `temperature_2m_max` and
  `precipitation_sum`, 1991-01-01 to 2020-12-31, from the Open-Meteo
  archive API (default model, GMT days). Each city was fetched once,
  with requests at least 65 s apart. The raw responses are kept outside the
  repository; the monthly normals are committed as
  `tests/fixtures/era5-normals.json`.
- **ERA5 values**: monthly mean daily minimum and maximum, and monthly
  precipitation totals, averaged over the 30 years.
  - `tmin_cold` is the lowest monthly mean minimum, and `tmax_warm` the highest
    monthly mean maximum.
  - Monthly means are (min + max) / 2.
  - Aridity, dry months and the Köppen class are computed by this crate
    from those normals.
- **Grid values**: `lookup` at the city's coordinates (radius 2). Every city
  falls in a complete cell (distance 0).
- **Tolerances**: ±1.5 °C for the four temperatures, ±40 % for annual
  precipitation. Misses are in bold.
- **The comparison is not like for like.**
  - The grid gives the mean of a 0.1° cell (about 11 × 8 km at 45°).
  - Open-Meteo gives one ERA5 grid point, corrected to the elevation it
    reports (shown in the explanations).
  - In mountains, on coasts and on ice, the two can differ by more than the
    tolerance even when both are right.
  - ERA5 is itself a model: it is known to be wet over tropical mountains and
    to struggle with winter inversions and small coastal features.

## Results

| City | Köppen grid / ERA5 | tmin_cold | tmax_warm | tmean_cold | tmean_warm | precip mm | aridity | dry months | misses |
|---|---|---|---|---|---|---|---|---|---|
| Paris | Cfb / Cfb | 1.6 / 1.4 (+0.2) | 24.0 / 24.4 (-0.4) | 4.8 / 4.4 (+0.4) | 20.1 / 19.5 (+0.6) | 644 / 670 (-4%) | 0.92 / 0.98 | 0 / 0 | - |
| Lyon | Cfa / Cfb | 0.1 / 0.3 (-0.2) | 26.6 / 26.6 (+0.0) | 4.3 / 3.6 (+0.6) | 22.4 / 21.4 (+1.0) | 809 / 945 (-14%) | 1.10 / 1.34 | 0 / 0 | - |
| Seville | Csa / Csa | 5.8 / 6.0 (-0.2) | 34.8 / 35.4 (-0.6) | 11.0 / 10.7 (+0.3) | 27.8 / 28.3 (-0.5) | 566 / 479 (+18%) | 0.58 / 0.49 | 5 / 5 | - |
| Montpellier | Csa / Csa | 4.0 / 3.8 (+0.2) | 28.5 / 28.5 (+0.0) | 7.8 / 7.3 (+0.5) | 24.1 / 23.6 (+0.5) | 781 / 838 (-7%) | 0.97 / 1.07 | 2 / 3 | - |
| Berlin | Cfb / Cfb | -1.5 / -1.8 (+0.3) | 23.7 / 23.9 (-0.2) | 1.0 / 0.7 (+0.3) | 19.7 / 19.3 (+0.4) | 584 / 605 (-3%) | 0.88 / 0.92 | 0 / 0 | - |
| Nairobi | Aw / Csb | 12.3 / 12.3 (+0.0) | 26.5 / 27.0 (-0.5) | 17.6 / 17.1 (+0.5) | 21.1 / 21.1 (+0.0) | 889 / 656 (+36%) | 1.02 / 0.76 | 5 / 5 | - |
| São Paulo | Cfa / Cfa | 12.4 / 12.2 (+0.2) | 27.0 / 26.8 (+0.2) | 16.5 / 16.9 (-0.4) | 22.8 / 23.0 (-0.2) | 1438 / 1288 (+12%) | 1.57 / 1.39 | 0 / 1 | - |
| Manaus | Af / Af | 24.0 / 23.5 (+0.5) | 31.2 / 31.1 (+0.1) | 26.7 / 26.3 (+0.4) | 28.3 / 27.9 (+0.4) | 2270 / 2694 (-16%) | 1.30 / 1.61 | 0 / 0 | - |
| Mumbai | Am / Aw | 21.0 / 18.7 (+2.3) **miss** | 30.4 / 32.7 (-2.3) **miss** | 24.3 / 24.0 (+0.3) | 29.9 / 29.5 (+0.4) | 2513 / 1924 (+31%) | 1.48 / 1.17 | 7 / 7 | tmin_cold, tmax_warm |
| Tokyo | Cfa / Cfa | 0.2 / -0.3 (+0.5) | 29.3 / 29.7 (-0.4) | 5.0 / 4.0 (+1.0) | 27.2 / 26.6 (+0.6) | 1566 / 1465 (+7%) | 1.84 / 1.80 | 0 / 0 | - |
| Phoenix | BWh / BWh | 5.2 / 4.8 (+0.4) | 41.6 / 41.5 (+0.1) | 12.6 / 11.7 (+0.9) | 35.0 / 34.8 (+0.2) | 189 / 249 (-24%) | 0.14 / 0.20 | 12 / 9 | - |
| Hobart | Cfb / Cfb | 4.6 / 5.0 (-0.4) | 21.3 / 22.2 (-0.9) | 8.2 / 8.3 (-0.1) | 16.8 / 17.6 (-0.8) | 583 / 611 (-5%) | 0.87 / 0.90 | 0 / 0 | - |
| Reykjavik | Cfc / Dfc | -4.4 / -3.5 (-0.9) | 14.3 / 14.5 (-0.2) | 0.6 / -1.0 (+1.6) **miss** | 12.0 / 11.6 (+0.4) | 1101 / 1354 (-19%) | 1.93 / 2.54 | 0 / 0 | tmean_cold |
| Yakutsk | Dfc / Dwc | -39.6 / -39.3 (-0.3) | 24.4 / 24.9 (-0.5) | -37.6 / -36.1 (-1.5) | 19.4 / 19.7 (-0.3) | 252 / 232 (+9%) | 0.48 / 0.44 | 1 / 0 | - |
| Cairo | BWh / BWh | 8.2 / 7.9 (+0.3) | 36.2 / 36.8 (-0.6) | 13.8 / 13.5 (+0.3) | 29.2 / 29.2 (+0.0) | 23 / 18 (+29%) | 0.02 / 0.02 | 12 / 12 | - |
| Lima | BWh / BWh | 14.9 / 15.1 (-0.2) | 24.9 / 26.2 (-1.3) | 17.9 / 17.8 (+0.1) | 23.4 / 23.6 (-0.2) | 32 / 85 (-62%) **miss** | 0.03 / 0.09 | 12 / 12 | precip |
| Singapore | Af / Af | 24.7 / 23.9 (+0.8) | 29.2 / 29.2 (-0.0) | 26.4 / 25.9 (+0.5) | 27.9 / 27.2 (+0.7) | 2404 / 2641 (-9%) | 1.39 / 1.60 | 0 / 0 | - |
| Ulaanbaatar | Dwc / Dwc | -28.9 / -25.7 (-3.2) **miss** | 20.7 / 23.6 (-2.9) **miss** | -23.0 / -19.2 (-3.8) **miss** | 16.6 / 17.8 (-1.2) | 300 / 321 (-6%) | 0.68 / 0.66 | 0 / 0 | tmin_cold, tmax_warm, tmean_cold |
| Cape Town | Csb / Csb | 10.5 / 9.2 (+1.3) | 22.1 / 25.9 (-3.8) **miss** | 12.9 / 12.8 (+0.1) | 20.4 / 21.7 (-1.3) | 703 / 495 (+42%) **miss** | 0.90 / 0.61 | 5 / 6 | tmax_warm, precip |
| Quito | Csb / Cfb | 9.7 / 8.2 (+1.5) **miss** | 18.8 / 17.3 (+1.5) | 13.6 / 12.2 (+1.4) | 14.1 / 12.8 (+1.3) | 1152 / 2876 (-60%) **miss** | 1.71 / 4.48 | 1 / 0 | tmin_cold, precip |
| Nuuk | ET / ET | -16.2 / -17.4 (+1.2) | 9.6 / 12.1 (-2.5) **miss** | -10.9 / -14.3 (+3.4) **miss** | 7.3 / 9.0 (-1.7) **miss** | 996 / 809 (+23%) | 2.47 / 2.07 | 0 / 0 | tmax_warm, tmean_cold, tmean_warm |
| Longyearbyen | ET / ET | -17.3 / -16.5 (-0.8) | 6.0 / 8.0 (-2.0) **miss** | -13.9 / -14.1 (+0.2) | 5.1 / 6.5 (-1.4) | 365 / 432 (-16%) | 0.76 / 0.93 | 0 / 0 | tmax_warm |
| Kiambu | Cfb / Csb | 12.0 / 12.1 (-0.1) | 26.1 / 26.1 (-0.0) | 17.2 / 16.6 (+0.6) | 20.8 / 20.5 (+0.3) | 986 / 693 (+42%) **miss** | 1.15 / 0.83 | 3 / 4 | precip |
| Recife | Am / Aw | 22.7 / 22.3 (+0.4) | 29.4 / 29.9 (-0.5) | 24.6 / 24.5 (+0.1) | 27.3 / 27.3 (-0.0) | 1770 / 938 (+89%) **miss** | 1.14 / 0.60 | 3 / 4 | precip |
| Oulu | Dfc / Dfc | -11.2 / -10.9 (-0.3) | 19.5 / 19.6 (-0.1) | -8.5 / -8.4 (-0.1) | 16.6 / 16.3 (+0.3) | 549 / 586 (-6%) | 0.97 / 1.06 | 0 / 0 | - |
| Mendoza | BWk / BSk | 3.1 / 2.3 (+0.8) | 29.3 / 29.3 (-0.0) | 7.6 / 7.6 (+0.0) | 24.3 / 23.9 (+0.4) | 233 / 451 (-48%) **miss** | 0.28 / 0.56 | 12 / 0 | precip |
| Kumasi | Aw / Aw | 21.8 / 21.6 (+0.2) | 33.7 / 33.7 (+0.0) | 24.7 / 24.6 (+0.1) | 28.1 / 28.6 (-0.5) | 1362 / 1080 (+26%) | 0.86 / 0.69 | 2 / 3 | - |
| Osaka | Cfa / Cfa | 0.9 / 1.2 (-0.3) | 32.3 / 31.6 (+0.7) | 5.9 / 4.8 (+1.2) | 29.1 / 28.0 (+1.1) | 1414 / 1455 (-3%) | 1.53 / 1.70 | 0 / 0 | - |
| Riyadh | BWh / BWh | 6.8 / 6.9 (-0.1) | 42.3 / 42.4 (-0.1) | 14.4 / 13.3 (+1.1) | 36.6 / 34.9 (+1.8) **miss** | 108 / 67 (+61%) **miss** | 0.07 / 0.05 | 12 / 12 | tmean_warm, precip |

Columns give grid / ERA5 (grid − ERA5). The Köppen class of the grid is
Beck et al.'s published class (`docs/decisions/0002-koppen-class-vs-monthly-means.md`);
the ERA5 class is computed from the ERA5 monthly normals.

### Summary

| | within tolerance | mean grid − ERA5 | mean absolute difference |
|---|---|---|---|
| `tmin_cold` | 19 / 22 | +0.16 °C | 0.73 °C |
| `tmax_warm` | 17 / 22 | −0.77 °C | 0.95 °C |
| `tmean_cold` | 19 / 22 | +0.33 °C | 0.86 °C |
| `tmean_warm` | 21 / 22 | −0.05 °C | 0.64 °C |
| `precip` | 19 / 22 | median −4 % | |
| Köppen class | same at 16 / 22 | | |

14 of the 22 cities have no miss. The baseline shift (CHELSA 1981–2010 moved
to 1991–2020) removed the cold bias the prototype showed: Paris and Berlin
`tmin_cold` are now within 0.3 °C of ERA5.

### Misses explained

- **Mumbai** (`tmin_cold` +2.3, `tmax_warm` −2.3): the monthly means agree
  (+0.3 / +0.4), so the grid's diurnal range is too small.
  - The cell (19.05 N, 72.85 E) is mostly sea and peninsula.
  - CHELSA's minimum and maximum over its land pixels are damped by the sea,
    while the ERA5 point (elevation 6 m) is a land point.
  - Use the grid's extremes with care on narrow coasts.
- **Reykjavik** (`tmean_cold` +1.6): just outside the tolerance.
  - The cell lies on the coast and is mild in winter.
  - ERA5 classifies the city as Dfc, the grid (Beck) as Cfc: the coldest
    month sits near the 0 °C C/D boundary in both sources.
- **Lima** (precipitation −62 %): 32 mm against 85 mm a year.
  - In a hyper-arid climate a relative tolerance is meaningless; both say
    desert (BWh, 12 dry months).
  - ERA5 is known to overestimate drizzle there.
- **Ulaanbaatar** (`tmin_cold` −3.2, `tmax_warm` −2.9, `tmean_cold` −3.8):
  the grid is colder all year.
  - The cell includes the mountains around the valley (the ERA5 point is
    corrected to 1,300 m).
  - The winter inversion over the Mongolian plateau is hard for both
    sources: cold air pools in the valley, but ERA5 tends to be too warm in
    strong inversions.
  - Precipitation agrees (−6 %).
- **Cape Town** (`tmax_warm` −3.8, precipitation +42 %): the cell
  (33.95 S, 18.45 E) includes Table Mountain and the Atlantic shore.
  - Both are cooler in summer and the mountain is much wetter than the ERA5
    point on the Cape Flats (17 m).
- **Quito** (`tmin_cold` +1.5, precipitation −60 %):
  - Temperatures: the city lies on a slope at 2,800–2,900 m (ERA5 corrected
    to 2,920 m), and the cell averages lower and warmer terrain, so all four
    temperatures run 1.3–1.5 °C warmer.
  - Precipitation: ERA5 (2,876 mm) is known to be too wet over the tropical
    Andes; the grid's 1,152 mm is the more plausible value for the city.
- **Nuuk** (`tmax_warm` −2.5, `tmean_cold` +3.4, `tmean_warm` −1.7): the cell
  mixes fjord water, coast and mountains.
  - Its winter is milder (sea influence) and its summer cooler than the ERA5
    land point (7 m).
  - Both sources are coarse for Greenland's coast, but agree on ET.
- **Longyearbyen** (`tmax_warm` −2.0): the cell includes glaciers and
  mountains above the town, which are cooler in summer.
- **Kiambu** (precipitation +42 %): 986 mm against ERA5's 693 mm. Kiambu sits
  on the wet windward slope north of Nairobi (observed normals ~1,000–1,100 mm);
  all four temperatures agree within 0.6 °C.
- **Recife** (precipitation +89 %): 1,770 mm against ERA5's 938 mm. Recife's
  observed coastal rainfall is ~2,200 mm/year; ERA5 underestimates narrow
  tropical coastal rain while temperatures agree within 0.5 °C.
- **Mendoza** (precipitation −48 %): 233 mm against ERA5's 451 mm. Mendoza
  lies in the Andean rain shadow (observed ~220 mm/year); ERA5 overestimates
  foothill precipitation, while all four temperatures agree within 0.8 °C.
- **Riyadh** (`tmean_warm` +1.8, precipitation +61 %): 108 mm against 67 mm in
  a hyper-arid desert (both BWh, 12 dry months); `tmin_cold` and `tmax_warm`
  agree within 0.1 °C.

Cities without misses still show the usual offsets: Lyon, Phoenix and Tokyo
are within tolerance but 0.9–1.0 °C warmer in their coldest or warmest month
than ERA5. Precipitation is the weakest variable: products disagree by tens
of percent at a point. `build-report.json` compares Beck with CHELSA bio12
over all land: median −14 %, 5th–95th percentile −50 % to +21 %. Treat
`precip` and `aridity` as estimates.

## Future periods

The Open-Meteo CMIP6 API was used for 8 cities and one model
(EC_Earth3P_HR, high-resolution HighResMIP run). Its free hourly limit did not
allow more. The model covers 1991–2050 only, so its 2021–2050 change is
compared with the grid's 2041–2070 change. This is a check of sign and order
of magnitude only, not a tolerance test.

| City | EC_Earth3P_HR 2021-2050 minus 1991-2020: coldest / warmest month (°C) | grid 2041-2070 SSP2-4.5 | grid 2041-2070 SSP5-8.5 |
|---|---|---|---|
| paris | +0.8 / +1.5 | +1.1 / +1.8 | +1.6 / +2.5 |
| lyon | +0.7 / +1.8 | +1.2 / +2.2 | +1.8 / +3.1 |
| seville | +0.8 / +1.3 | +1.1 / +1.8 | +1.5 / +2.5 |
| montpellier | +0.5 / +1.8 | +1.2 / +2.2 | +1.7 / +3.1 |
| berlin | +1.0 / +1.4 | +1.6 / +1.9 | +2.1 / +2.6 |
| nairobi | +1.1 / +0.6 | +1.3 / +1.3 | +1.8 / +1.7 |
| sao-paulo | +0.8 / +0.7 | +1.4 / +1.1 | +1.9 / +1.7 |
| manaus | +0.8 / +1.2 | +1.4 / +1.9 | +1.9 / +2.8 |

The model's earlier period (centred 2035 vs 2055) warms less than the grid,
as expected. Sign and seasonality agree, for example Nairobi's coldest month
warming more than its warmest month.

Over all land, the `check-future` stage compares the grid's 2041–2070
SSP3-7.0 `tmin_cold` with the mean of CHELSA V2.1's own bio6 projections for
five GCMs (GFDL-ESM4, IPSL-CM6A-LR, MPI-ESM1-2-HR, MRI-ESM2-0, UKESM1-0-LL).
Measured from the same 1981–2010 baseline (CHELSA bio6), the grid's change
is 0.45 °C smaller on average (median 0.32 °C; grid − CHELSA from −1.85 to
+0.33 °C at the 5th and 95th percentiles). With one baseline, the difference
of the changes equals the difference of the 2041–2070 values. The grid's own
change from 1991–2020 is not comparable to CHELSA's change, which starts 10
years earlier. Beck et al.'s ensemble is constrained to models
that reproduce observed warming, which excludes the hottest models, and two
of CHELSA's five GCMs (UKESM1-0-LL, IPSL-CM6A-LR) are among the
warmest. The grid's futures are therefore on the moderate side of CMIP6.

## Extremes, monthly, and monthly_precip layers

### Extremes (`v0.3.0`)

Across the 29 validation cities, `extremes` produces consistent annual extreme
minimums (`tmin_extreme_c`), annual extreme maximums (`tmax_extreme_c`), and
USDA hardiness zones (`1a` at Yakutsk −50.5 °C through `13b` at Manaus
22.5 °C, Singapore 23.7 °C, Recife 21.5 °C, Kumasi 19.2 °C; `8b` at Paris
−6.7 °C and Lyon −7.4 °C; `5b` at Oulu −25.3 °C; `9b` at Mendoza −2.9 °C and
Osaka −3.3 °C; `10a` at Riyadh −0.5 °C). Globally across 2,171,102 cells,
`tmin_extreme` is strictly below `min_m(monthly.tmin)` at 100 % of cells
(0 cells above; mean gap −9.62 °C, 5th–95th percentile −17.1 to −2.3 °C).

### Monthly (`v0.4.0`)

Global build-report invariants across all 2,171,421 complete climate cells:

- `dry_mask` popcount matches `climate.dry_months` with **0 mismatches**
  across all 3 slots (hard-fail gate).
- `tmin <= tmean` at 100 % of cells/slots/months (3,381,444 values clamped
  where Beck `tmean` and CHELSA `tasmin` diverge slightly; 0 CHELSA `tasmin`
  missing or gap-filled cells).
- `|min_m(monthly.tmean) - climate.tmean_cold|` and
  `|max_m(monthly.tmean) - climate.tmean_warm|` have max absolute difference
  **0.20 °C** (MAE 0.12 °C, p05/p50/p95 = −0.2 / 0.0 / +0.2 °C), inside the
  0.30 °C quantization bound of 0.5 °C `tmean` vs 0.1 °C climate summaries.

Against ERA5 1991–2020 monthly normals across all 29 cities × 12 months
(348 city-months):

| Metric | Mean (grid − ERA5) | MAE | Median \|err\| | p90 \|err\| |
|---|---|---|---|---|
| `monthly.tmean` vs ERA5 `(tmin + tmax) / 2` | +0.31 °C | 0.72 °C | 0.53 °C | 1.49 °C |
| `monthly.tmin` vs ERA5 `tmin` | +0.02 °C | 0.55 °C | 0.34 °C | 1.27 °C |
| `2 * tmean - tmin` vs ERA5 `tmax` | +0.59 °C | 1.22 °C | 0.90 °C | 2.61 °C |

The symmetric approximation `2 * tmean - tmin` tracks ERA5 monthly `tmax` with
0.90 °C median error (1.22 °C MAE), confirming that omitting a separate
`monthly.tmax` band saves ~20 MB with acceptable accuracy (larger errors occur
where Beck `tmean` and CHELSA `tasmin` diverge in coastal/high-latitude cells
such as Reykjavik, Nuuk, Osaka, and Riyadh).

### Monthly precipitation (`v0.4.0`)

Global build-report invariants across 2,171,421 climate cells (358,004
0.25°-grid cells in 438 tiles, 9.04 MB total):

- Wettest-month agreement with 0.1° Beck precipitation: **99.66 %** (gate ≥ 95 %).
- Driest-month agreement with 0.1° Beck precipitation: **99.93 %** (gate ≥ 95 %).
- Against ERA5 across the 29 validation cities: **29 / 29** wettest-month and
  **29 / 29** driest-month agreement (within 15 mm for near-ties).

### Tail extremes, frost calendar, and climate analogues (`v0.5.0`)

Global `out-v0.5` build-report checks (0 `SHA256SUMS` diffs on all prior `v0.4.0`
layers; `verify-grid`: 464 climate tiles, 2,920 layer tiles, 0 problems):

- `extremes_tail`: 417 tiles (4.38 MB), 2,171,102 cells (4,347 coastal-filled, 319 missing).
- `frost_calendar`: 417 tiles (3.00 MB), 2,171,102 cells (443,895 frost-free,
  1,050,740 seasonal, 676,467 year-round). Verified bit-exact (6/6 bands) against
  the GEE `GRID_E5L` expression on Paris, Davis (CA), Quito, and Singapore, and
  within 1 day P90 tie-rounding on Canberra.
- `analogues.bin`: 21,767 reference cells (552.8 KB, `sha256=038e4e6f...`),
  phase-invariant 8D z-score k-NN index for 2050/2085 Climate Twin search.

Sanity check across 8 contrasting cities (`extremes_tail`, `frost_calendar`, and
top 2041–2070 SSP2-4.5 historical Climate Twin):

| City | `tmin_p10` / `tmax_p90` (°C) | Heat ≥35 °C (d/yr) | Trend (°C) | Last spring (P90) / First autumn DOY | Frost-free (d) | False spring % | Top 2050 SSP2-4.5 twin (σ) |
|---|---|---|---|---|---|---|---|
| Lyon | −3.7 / +3.7 | 1.4 | +1.0 | 87 (98) / 320 | 234 | 0 | Po Valley, IT `Cfa` (0.064) |
| Lormes | −4.3 / +2.9 | 0.8 | +0.9 | 97 (112) / 318 | 218 | 3 | Tarn, FR `Cfb` (0.054) |
| Seville | −2.1 / +2.2 | 42.2 | +0.7 | 28 (55) / 9 | 365 (43% yrs) | 0 | Andalusia / N. Tunisia `Csa` (0.080) |
| Oulu | −5.9 / +2.5 | 0.0 | +0.8 | 129 (139) / 287 | 153 | 0 | Vaasa, FI `Dfb` (0.057) |
| Mendoza | −3.7 / +2.2 | 2.5 | +0.5 | 234 (257) / 169 | 297 | 10 | Chihuahua, MX `BWh` (0.087) |
| Kiambu | −0.7 / +1.3 | 0.0 | +0.5 | frost-free | 365 (0% yrs) | 0 | E. Cape, ZA `Cfa` / S. Uganda `Aw` (0.073) |
| Paris | −3.3 / +3.3 | 0.8 | +0.8 | 91 (110) / 325 | 233 | 7 | Loire Valley / Toulouse, FR `Cfb` (0.067) |
| Canberra | −0.8 / +2.3 | 1.8 | +0.3 | 256 (275) / 135 | 247 | 0 | Mudgee, NSW AU `Cfa` (0.048) |

### Phenology, photoperiod, and water balance (`v0.6.0`)

Global `out-v0.6` build-report checks (0 `SHA256SUMS` diffs on all prior `v0.5.0`
layers; `climate_tiles_sha256` unchanged; `verify-grid`: 464 climate tiles,
3,748 layer tiles, 0 problems):

- `phenology`: 411 tiles (2.14 MB, median 1.6 KB), 2,170,983 cells (4,113
  coastal-filled, 438 missing). 5-band Fishman Dynamic Chill Portions across
  warming levels `+0 °C .. +4 °C` (ERA5-Land hourly 1991–2020); `photoperiod`
  (FAO-56 day-length & solstice range, pole-hardened) is derived at 0 bytes.
- `water_balance`: 417 tiles (7.65 MB, median 9.6 KB), 2,171,102 cells (4,357
  coastal-filled, 319 missing). TerraClimate 1991–2020 CWD mean/P90 and summer
  VPD + ERA5-Land 1991–2020 max consecutive dry days (CDD); Saxton–Rawls AWC
  is derived at 0 bytes from `soil`.

Sanity check across 8 contrasting cities (`phenology` chill trajectory and `water_balance`):

| City | Chill Portions `+0 / +1 / +2 / +3 / +4 °C` | CWD mean / P90 (mm) | Max CDD (days) | Summer VPD (kPa) |
|---|---|---|---|---|
| Paris | `136 / 130 / 121 / 110 / 97` | 338 / 457 | 20 | 1.00 |
| Lyon | `132 / 127 / 120 / 111 / 101` | 297 / 423 | 19 | 1.18 |
| Seville | `62 / 47 / 33 / 21 / 12` | 946 / 1094 | 82 | 2.00 |
| Davis, CA | `74 / 60 / 46 / 32 / 21` | 1021 / 1126 | 116 | 1.99 |
| Canberra | `130 / 121 / 111 / 99 / 86` | 637 / 851 | 25 | 1.16 |
| Oulu | `76 / 90 / 102 / 112 / 119` | 70 / 135 | 19 | 0.56 |
| Mendoza | `117 / 108 / 99 / 88 / 77` | 1181 / 1330 | 28 | 1.89 |
| Singapore | `0 / 0 / 0 / 0 / 0` | 29 / 91 | 10 | 0.74 |

Note the expected Mediterranean/Central Valley chill collapse at Seville
(62 → 12 CP) and Davis (74 → 21 CP), Southern Hemisphere winter alignment at
Canberra and Mendoza, and boreal warming chill gain at Oulu (76 → 119 CP as
sub-freezing winter days warm into the 4–10 °C Fishman sweet spot).

## Site layers

### Soil

Reference: `gdallocationinfo -wgs84` (GDAL 3.8.4, PROJ 9) on the nine
SoilGrids files, at the layer cell centre, combined 5/10/15 over 0–30 cm
and rounded like the tiles. The grid equals the reference in every value:

| Point | Cell centre | pH (water) | Sand % | Clay % | Texture |
|---|---|---|---|---|---|
| Beauce (48.30, 1.75) | 48.275, 1.775 | 7.0 | 7.5 | 30.5 | silty clay loam |
| Hérault (43.65, 3.55) | 43.625, 3.575 | 7.9 | 32.4 | 29.2 | clay loam |
| Kenya highlands (-0.5, 37.0) | -0.525, 37.025 | 6.0 | 24.8 | 43.7 | clay |
| São Paulo state (-22.0, -48.0) | -22.025, -47.975 | 5.3 | 37.4 | 45.6 | clay |

Fiji and Chukotka (both sides of the antimeridian) were checked the same
way. The prototype's reference values (Beauce ≈ 6.5, Hérault 7.1, Kenya
5.6, São Paulo state 5.2) came from points whose coordinates were not kept;
the grid's values at nearby rural points are in the same range (0.1–0.8
higher), as expected for different 5-km pixels.

Cities, which SoilGrids leaves empty, are filled from the nearest measured
cell: Paris pH 6.0 (3 cells), Montpellier 7.7 (1), São Paulo 5.3 (2),
Tokyo (4), Shanghai, Beijing and Buenos Aires (3). The Greenland and
Antarctic ice sheets and the sea return no soil.

### Regions

| Point | `wgsrpd_l3` | `iso_a2` | Note |
|---|---|---|---|
| Paris | FRA | FR | Natural Earth ISO_A2 is -99; ISO_A2_EH is FR |
| Oslo | NOR | NO | same |
| São Paulo | BZL | BR | |
| Nairobi | KEN | KE | |
| Singapore | MLY | SG | WGSRPD puts Singapore in Malaya |
| Monaco | FRA | MC | microstate smaller than a cell: interior-point cell |
| Nicosia | CYP | CY | Northern Cyprus resolved to CY |
| Hargeisa | SOM | SO | Somaliland resolved to SO |
| Taipei | TAI | TW | ISO_A2 CN-TW, ISO_A2_EH TW |
| Fiji, 178.0 E and 178.95 W | FIJ | FJ | antimeridian |
| Chukotka, 178 E and 175 W | MAG | RU | antimeridian |
| Reykjavik | ICE | IS | |
| Réunion, Cayenne, Guadeloupe, Martinique, Mayotte | | RE, GF, GP, MQ, YT | French overseas departments: own ISO codes (map units) |
| Longyearbyen, Jan Mayen | | SJ | Jan Mayen overridden from NO to SJ as in ISO 3166-1 |
| Bonaire, Curaçao, Aruba | | BQ, CW, AW | |
| Nuuk, Tórshavn, San Juan, Guam, Christmas I., Cocos, Hong Kong, Macao, Bermuda, Stanley, Nouméa area, Tahiti | | GL, FO, PR, GU, CX, CC, HK, MO, BM, FK, NC, PF | |
| 2 cells off Boulogne-sur-Mer, Palavas, Cape Town | FRA/FRA/CPP | FR/FR/ZA | coastal points reach land within 2 cells |

Vatican: Natural Earth 1:50m draws it about 1.5 km west of its real
position, so a lookup at St Peter's returns IT; its own cell (at the
polygon's interior point) holds VA.

## Tests

- `tests/validation.rs::era5_classes_are_reproducible` recomputes the ERA5
  classes and summaries from the committed normals. It runs in CI.
- `tests/validation.rs::grid_matches_expected` (ignored) checks a built grid.
  It requires every city's lookup to equal `tests/fixtures/validation-expected.json`
  and the set of misses to equal the one documented here:

```sh
CANOPI_CLIMATE_GRID=/path/to/extracted/canopi-climate-grid-v0.2.0 \
  cargo test --release --features pipeline --test validation -- --ignored
```

  CI's `grid` job runs every ignored test on each push to `main`, against
  the release tar pinned in `release/grid.json`.

- `tests/site_layers.rs` (ignored) checks the soil and regions points
  above, the empty ice sheets, the coastal points and the release
  checksums against a built grid (`--test site_layers`).
