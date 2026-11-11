# 0002: The Köppen class is Beck's published class, and contradictions are reported

- Status: accepted (2026-11-11)

## Context

Each cell has two possible Köppen classes:

1. the published class: the most frequent class among the 144 one-kilometre
   cells of Beck et al. (2023) inside the 0.1° cell;
2. the class of the cell's own 0.1° monthly means, computed with the same
   rules (Beck et al. 2018, Table 1) from Beck's 0.1° temperature and
   precipitation climatologies.

The two can disagree. The 1-km maps resolve terrain, and the class of the
most common 1-km cell is not the class of the cell's average climate. For
1991–2020, the 1-km maps also come from a different set of observational
datasets than the 0.1° climatologies. Nairobi is the known case: the
published class is **Aw**, while the cell's coldest month averages 17.6 °C,
0.4 °C below the 18 °C that group A requires.

## Counts (grid v0.1, 2,171,421 land cells with complete data)

| Slot | class ≠ class of means | group ≠ group of means | class outside its own temperature bounds by > 0 °C / > 1 °C / > 2 °C |
|---|---|---|---|
| 1991–2020 | 89,211 (4.1 %) | 29,344 (1.4 %) | 20,555 / 1,214 / 203 |
| 2041–2070 SSP1-2.6 | 38,927 | 14,543 | 13,362 / 373 / 37 |
| 2041–2070 SSP2-4.5 | 39,056 | 14,212 | 12,270 / 366 / 45 |
| 2041–2070 SSP3-7.0 | 40,775 | 13,602 | 12,388 / 342 / 37 |
| 2041–2070 SSP5-8.5 | 44,890 | 14,676 | 14,320 / 359 / 38 |
| 2071–2099 SSP1-2.6 | 43,917 | 16,458 | 16,002 / 403 / 48 |
| 2071–2099 SSP2-4.5 | 49,909 | 16,870 | 18,984 / 392 / 42 |
| 2071–2099 SSP3-7.0 | 60,304 | 17,295 | 22,560 / 441 / 32 |
| 2071–2099 SSP5-8.5 | 63,896 | 18,481 | 23,892 / 522 / 30 |

"Temperature bounds" counts only the temperature criteria of the class (A:
coldest month ≥ 18 °C; C: 0–18 °C; D: ≤ 0 °C; a/b/c/d summer thresholds; ET/EF).
The B (arid) criterion depends on precipitation and is not counted. In
1991–2020, the 1,214 cells beyond 1 °C split into A 144, C 232, D 175 and
E 663. `build-report.json` holds these numbers for every build.

## Decision

- `koppen` is the published Beck class (option 1). It is the cited, reviewed
  product, and for 1991–2020 it equals Beck's own 0.1° map in every cell.
- No class is recomputed or substituted. A future class is never replaced by
  the current one.
- Every lookup reports `koppen_temperature_margin_c`: by how many °C the
  cell's own monthly means fall outside the temperature bounds of its class
  (0 when they fit). An application can show the class with a caveat when
  the margin is large. Nairobi gets Aw with a margin of 0.4 °C.

## Consequences

- At about 1 % of cells, a user can see a class whose temperature rule the
  displayed monthly means do not meet. Near class boundaries this is expected
  and is disclosed by the margin field.
- If a later version prefers the class of the means, it can be derived from
  the stored variables, except where the B criterion needs the monthly
  precipitation distribution, which the grid does not store.
