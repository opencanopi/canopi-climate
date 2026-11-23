"""Phenology (Dynamic Chill Portions) and water balance layers for canopi-climate v0.6.0,
computed on Google Earth Engine.

Exports static 0.1-degree GeoTIFFs on ERA5-Land's own grid (`GRID_E5L`, 3600x1801) to the
Google Drive folder `canopi-climate-ee`, then `download` fetches them and `combine` writes
the build inputs (`ee/era5land_phenology.tif`, `ee/terraclimate_water_balance.tif`,
`ee/era5land_cdd.tif`):

1. `terraclimate_water_balance` (3 bands, int16, NODATA = -32768, 1991-2020):
   From `IDAHO_EPSCOR/TERRACLIMATE` (4 km, CC0 / Public Domain):
   - `cwd_mm`: mean annual Climatic Water Deficit (mm/yr, scale 1)
   - `cwd_p90_mm`: 90th-percentile (1-in-10 drought year) annual CWD (mm/yr, scale 1)
   - `vpd_summer_kpa`: peak-quarter (max circular 3-month rolling mean) climatological
     monthly-mean VPD (hundredths of kPa, scale 0.01)

2. `era5land_cdd_<y0>_<y1>` (3 x 10-year chunks: 1991-2000, 2001-2010, 2011-2020; 1 band,
   int16 tenths of days, NODATA = -32768):
   From `ECMWF/ERA5_LAND/DAILY_AGGR` (`total_precipitation_sum` in meters; dry day < 0.001 m):
   - `cdd_max_days`: mean annual maximum consecutive dry days, computed via a monthly
     `(len, prefix_dry, suffix_dry, max_dry)` monoid merge anchored per pixel to each cell's
     wettest month (from `ECMWF/ERA5_LAND/MONTHLY_AGGR`, 1991-2020) so SH/tropical dry seasons
     crossing Dec 31 -> Jan 1 are never split.

3. `era5land_phenology` (5 bands, int16 CP, NODATA = -32768, 1991-2020):
   From `ECMWF/ERA5_LAND/DAILY_AGGR` (`temperature_2m_min`, `temperature_2m_max`):
   Downloaded directly via GEE `getDownloadURL` as monthly 0.25-deg (`1440x360` per hemisphere,
   `int16` tenths of degC) daily stacks (zero Drive quota / zero batch-queue stall), then
   integrated locally in vectorized float32 using the **exact continuous 7-month hourly
   (`DT_HOURS = 1.0`, 24 steps/day)** Fishman (1987) Dynamic Model (Oct-Apr for NH `0..90N`,
   Apr-Oct for SH `90S..0`) across dT in {0, +1, +2, +3, +4 C}, averaged over 1991-2020, and
   bilinearly resampled (with nearest coastal fallback) to `GRID_E5L` 0.1-deg:
   - `chill_portions_0c`, `chill_portions_1c`, `chill_portions_2c`,
     `chill_portions_3c`, `chill_portions_4c`

Usage:
    python scripts/ee/v06_layers.py --project canopi-515022 check
    python scripts/ee/v06_layers.py --project canopi-515022 export --only tc,cdd
    python scripts/ee/v06_layers.py --project canopi-515022 status
    python scripts/ee/v06_layers.py --project canopi-515022 download --out <dir>
    python scripts/ee/v06_layers.py --project canopi-515022 pheno-fetch --out <dir>
    python scripts/ee/v06_layers.py --project canopi-515022 combine --out <dir>
"""

import argparse
import io
import json
import math
import sys
from pathlib import Path

import ee

ERA5_DAILY = "ECMWF/ERA5_LAND/DAILY_AGGR"
ERA5_MONTHLY = "ECMWF/ERA5_LAND/MONTHLY_AGGR"
TERRACLIMATE = "IDAHO_EPSCOR/TERRACLIMATE"
BASE = (1991, 2020)
NODATA = -32768
DRIVE_FOLDER = "canopi-climate-ee"

# ERA5-Land native 0.1-degree grid (matching scripts/ee/extremes.py and v05_layers.py).
GRID_E5L = dict(
    crs="EPSG:4326",
    crsTransform=[0.1, 0, -180.05, 0, -0.1, 90.05],
    dimensions="3600x1801",
)

TC_NAME = "terraclimate_water_balance"
CDD_CHUNKS = [(1991, 2000), (2001, 2010), (2011, 2020)]
# 15 x 2-year chunks x 2 hemispheres (NH 0..90N, SH 90S..0) at 0.25-deg with DT_HOURS=4.0
# and shardSize=32: ~50x fewer pixel-ODE-steps/yr and 40x lower worker tile RAM than 5-yr shardSize=128.
PHENO_CHUNKS = [(y, y + 1) for y in range(BASE[0], BASE[1] + 1, 2)]
PHENO_HEMIS = ("nh", "sh")
PHENO_SHARD_SIZE = 32

GRID_PHENO_NH = dict(
    crs="EPSG:4326",
    crsTransform=[0.25, 0, -180.0, 0, -0.25, 90.0],
    dimensions="1440x360",
)
GRID_PHENO_SH = dict(
    crs="EPSG:4326",
    crsTransform=[0.25, 0, -180.0, 0, -0.25, 0.0],
    dimensions="1440x360",
)

TC_BANDS = ["cwd_mm", "cwd_p90_mm", "vpd_summer_kpa"]
PHENO_BANDS = [
    "chill_portions_0c",
    "chill_portions_1c",
    "chill_portions_2c",
    "chill_portions_3c",
    "chill_portions_4c",
]

DT_HOURS = 4.0  # 6 four-hourly closed-form Fishman ODE steps per day


def cdd_chunk_name(y0: int, y1: int) -> str:
    return f"era5land_cdd_{y0}_{y1}"


def pheno_chunk_name(hemi: str, y0: int, y1: int) -> str:
    return f"era5land_phenology_{hemi}_{y0}_{y1}"


def all_task_names() -> list[str]:
    return [TC_NAME] + [cdd_chunk_name(a, b) for a, b in CDD_CHUNKS]


def _pheno_month_jobs() -> list[tuple[str, str, int, int]]:
    """All (hemi, var, year, month) monthly daily-temperature stacks needed for 1991-2020 winters."""
    jobs: list[tuple[str, str, int, int]] = []
    # NH winters: Oct..Dec of 1991..2020 and Jan..Apr of 1992..2021
    nh_ym = [(y, m) for y in range(BASE[0], BASE[1] + 1) for m in (10, 11, 12)] + [
        (y + 1, m) for y in range(BASE[0], BASE[1] + 1) for m in (1, 2, 3, 4)
    ]
    # SH winters: Apr..Oct of 1991..2020
    sh_ym = [(y, m) for y in range(BASE[0], BASE[1] + 1) for m in (4, 5, 6, 7, 8, 9, 10)]
    for hemi, ym_list in (("nh", nh_ym), ("sh", sh_ym)):
        for var in ("tmin", "tmax"):
            for y, m in sorted(set(ym_list)):
                jobs.append((hemi, var, y, m))
    return jobs


def cmd_pheno_fetch(args) -> None:
    """Download monthly 0.25-deg ERA5-Land daily Tmin/Tmax stacks directly via GEE getDownloadURL."""
    import concurrent.futures
    import time
    import urllib.request

    dst_dir = Path(args.out) / "pheno_daily"
    dst_dir.mkdir(parents=True, exist_ok=True)
    jobs = _pheno_month_jobs()
    pending = [j for j in jobs if not (dst_dir / f"{j[0]}_{j[1]}_{j[2]}_{j[3]:02d}.tif").exists()]
    print(f"pheno-fetch: {len(jobs) - len(pending)}/{len(jobs)} cached, {len(pending)} to download")
    if not pending:
        return

    var_band = {"tmin": "temperature_2m_min", "tmax": "temperature_2m_max"}

    def fetch_one(job: tuple[str, str, int, int]) -> str:
        hemi, var, y, m = job
        path = dst_dir / f"{hemi}_{var}_{y}_{m:02d}.tif"
        tmp = dst_dir / f".{hemi}_{var}_{y}_{m:02d}.tif.part"
        start = f"{y}-{m:02d}-01"
        ny, nm = (y + 1, 1) if m == 12 else (y, m + 1)
        end = f"{ny}-{nm:02d}-01"
        grid = GRID_PHENO_NH if hemi == "nh" else GRID_PHENO_SH
        for attempt in range(4):
            try:
                col = (
                    ee.ImageCollection(ERA5_DAILY)
                    .select(var_band[var])
                    .filterDate(start, end)
                    .sort("system:time_start")
                    .map(
                        lambda img: img.subtract(273.15)
                        .multiply(10.0)
                        .round()
                        .toInt16()
                        .unmask(NODATA, False)
                    )
                )
                url = col.toBands().getDownloadURL({**grid, "format": "GEO_TIFF"})
                data = urllib.request.urlopen(url, timeout=180).read()
                tmp.write_bytes(data)
                tmp.replace(path)
                return f"{path.name} ({len(data) // 1024} KB)"
            except Exception as exc:
                if attempt == 3:
                    raise RuntimeError(f"failed {path.name}: {exc}") from exc
                time.sleep(2.0 * (attempt + 1))
        raise RuntimeError(f"unreachable {path.name}")

    done = 0
    t0 = time.time()
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
        futs = {pool.submit(fetch_one, j): j for j in pending}
        for fut in concurrent.futures.as_completed(futs):
            fut.result()
            done += 1
            if done % 50 == 0 or done == len(pending):
                elapsed = time.time() - t0
                rate = done / max(elapsed, 0.1)
                eta = (len(pending) - done) / max(rate, 1e-6)
                print(f"  downloaded {done}/{len(pending)} ({elapsed:.0f}s elapsed, ETA {eta:.0f}s)", flush=True)


_FISHMAN_C_SRC = r"""
#include <math.h>
#include <stddef.h>
#include <stdint.h>

#define LUT_N 2401 /* 220.0 K .. 340.0 K at 0.05 K resolution */

void fishman_season(
    const float *tmid,    /* (n_land, n_days) contiguous per pixel */
    const float *trange,  /* (n_land, n_days) contiguous per pixel */
    const float *cos_w,   /* (24,) */
    int n_days,
    int n_land,
    float *cp_out         /* (5, n_land) */
) {
    const double ln_a0_div_a1 = -30.54354837558665;
    const double ln_a1_dt     =  42.38945182297704; /* dt = 1.0 h */
    float xs_lut[LUT_N];
    float decay_lut[LUT_N];
    float xi_lut[LUT_N];
    for (int k = 0; k < LUT_N; ++k) {
        double tk = 220.0 + 0.05 * (double)k;
        double inv_tk = 1.0 / tk;
        xs_lut[k] = (float)exp(8735.3 * inv_tk + ln_a0_div_a1);
        double k1_dt = exp(ln_a1_dt - 12888.8 * inv_tk);
        decay_lut[k] = (float)exp(-k1_dt);
        double arg = 443.2 * (tk - 277.0) * inv_tk;
        if (arg < -50.0) arg = -50.0;
        if (arg >  50.0) arg =  50.0;
        xi_lut[k] = (float)(1.0 / (1.0 + exp(-arg)));
    }

    for (int i = 0; i < n_land; ++i) {
        const float *tm_row = tmid + (size_t)i * (size_t)n_days;
        const float *tr_row = trange + (size_t)i * (size_t)n_days;
        for (int s = 0; s < 5; ++s) {
            float dt = (float)s;
            float x = 0.0f;
            float cp = 0.0f;
            for (int d = 0; d < n_days; ++d) {
                float tm = tm_row[d] + dt;
                float tr = tr_row[d];
                for (int h = 0; h < 24; ++h) {
                    float tk = tm + tr * cos_w[h];
                    int k = (int)((tk - 220.0f) * 20.0f + 0.5f);
                    if (k < 0) k = 0;
                    if (k >= LUT_N) k = LUT_N - 1;
                    float xs = xs_lut[k];
                    float x_inter = xs - (xs - x) * decay_lut[k];
                    if (x_inter >= 1.0f) {
                        float delta = x_inter * xi_lut[k];
                        cp += delta;
                        x = x_inter - delta;
                    } else {
                        x = x_inter;
                    }
                }
            }
            cp_out[(size_t)s * (size_t)n_land + (size_t)i] = cp;
        }
    }
}
"""


def _compile_fishman_so(cache_dir: Path) -> str | None:
    import shutil
    import subprocess

    cc = shutil.which("gcc") or shutil.which("cc")
    if not cc:
        return None
    so_path = cache_dir / ".fishman_kernel.so"
    c_path = cache_dir / ".fishman_kernel.c"
    c_path.write_text(_FISHMAN_C_SRC)
    subprocess.run([cc, "-O3", "-shared", "-fPIC", str(c_path), "-lm", "-o", str(so_path)], check=True)
    return str(so_path)


def _fishman_season_worker(task: tuple[str, int, str, str | None]) -> tuple[str, int, object, float]:
    """Compute exact 1-hour (24 steps/day) continuous 7-month Fishman CP for one (hemi, year)."""
    import ctypes
    import numpy as np
    import rasterio

    hemi, y, daily_dir_s, so_path = task
    daily_dir = Path(daily_dir_s)
    months = (
        [(y, 10), (y, 11), (y, 12), (y + 1, 1), (y + 1, 2), (y + 1, 3), (y + 1, 4)]
        if hemi == "nh"
        else [(y, 4), (y, 5), (y, 6), (y, 7), (y, 8), (y, 9), (y, 10)]
    )
    tmin_list = []
    tmax_list = []
    dy = -0.5
    for yy, mm in months:
        with rasterio.open(daily_dir / f"{hemi}_tmin_{yy}_{mm:02d}.tif") as s:
            dy = float(s.transform.e)
            arr = s.read()
        # getDownloadURL without explicit region returns full-globe 1440x360 (dy=-0.50);
        # slice to the target hemisphere so NH uses rows 0..179 (90N..0) and SH uses 180..359 (0..90S).
        if abs(dy + 0.5) < 0.05 and arr.shape[1] == 360:
            arr = arr[:, :180, :] if hemi == "nh" else arr[:, 180:, :]
        tmin_list.append(arr)
        with rasterio.open(daily_dir / f"{hemi}_tmax_{yy}_{mm:02d}.tif") as s:
            arr = s.read()
        if abs(dy + 0.5) < 0.05 and arr.shape[1] == 360:
            arr = arr[:, :180, :] if hemi == "nh" else arr[:, 180:, :]
        tmax_list.append(arr)

    tmin_all = np.concatenate(tmin_list, axis=0)  # (D, H, W) int16 tenths of C
    tmax_all = np.concatenate(tmax_list, axis=0)
    n_days, h_out, w_out = tmin_all.shape
    valid = (tmin_all[0] != NODATA) & (tmax_all[0] != NODATA)
    idx = np.flatnonzero(valid)
    n_land = int(idx.size)

    # Transpose to (N_land, D) so each pixel's time series is contiguous in L1 cache.
    tmin_k = tmin_all.reshape(n_days, -1)[:, idx].T.astype(np.float32) * 0.1 + 273.15
    tmax_k = tmax_all.reshape(n_days, -1)[:, idx].T.astype(np.float32) * 0.1 + 273.15
    tmid = np.ascontiguousarray(0.5 * (tmin_k + tmax_k), dtype=np.float32)
    trange = np.ascontiguousarray(np.maximum(0.0, tmax_k - tmin_k), dtype=np.float32)
    cos_w = np.ascontiguousarray(
        [-0.5 * math.cos(2.0 * math.pi * (h - 3.0) / 24.0) for h in range(24)], dtype=np.float32
    )
    cp = np.zeros((5, n_land), dtype=np.float32)

    if so_path:
        lib = ctypes.CDLL(so_path)
        fp = ctypes.POINTER(ctypes.c_float)
        lib.fishman_season.argtypes = [fp, fp, fp, ctypes.c_int, ctypes.c_int, fp]
        lib.fishman_season(
            tmid.ctypes.data_as(fp),
            trange.ctypes.data_as(fp),
            cos_w.ctypes.data_as(fp),
            n_days,
            n_land,
            cp.ctypes.data_as(fp),
        )
    else:
        dt_shifts = np.array([0.0, 1.0, 2.0, 3.0, 4.0], dtype=np.float32)[:, None]
        ln_a0_div_a1 = np.float32(math.log(5.434359174e-14))
        ln_a1_dt = np.float32(math.log(2.567e18 * 1.0))
        x = np.zeros((5, n_land), dtype=np.float32)
        for d in range(n_days):
            tm_d = tmid[:, d][None, :]
            tr_d = trange[:, d][None, :]
            for cw in cos_w:
                tk = np.clip(tm_d + tr_d * cw + dt_shifts, 220.0, 340.0)
                inv_tk = np.float32(1.0) / tk
                xs = np.exp(np.float32(8735.3) * inv_tk + ln_a0_div_a1)
                k1_dt = np.exp(ln_a1_dt - np.float32(12888.8) * inv_tk)
                x_inter = xs - (xs - x) * np.exp(-k1_dt)
                trig = x_inter >= 1.0
                if np.any(trig):
                    arg = np.clip(np.float32(443.2) * (tk[trig] - np.float32(277.0)) * inv_tk[trig], -50.0, 50.0)
                    xi = np.float32(1.0) / (np.float32(1.0) + np.exp(-arg))
                    delta = x_inter[trig] * xi
                    cp[trig] += delta
                    x_inter[trig] -= delta
                x = x_inter

    out = np.full((5, h_out * w_out), np.nan, dtype=np.float32)
    out[:, idx] = cp
    return hemi, y, out.reshape(5, h_out, w_out), dy


def terraclimate_water_balance_image() -> ee.Image:
    """Compute TerraClimate CWD mean, CWD P90 (mm/yr), and peak-quarter VPD (kPa) for 1991-2020."""
    tc = ee.ImageCollection(TERRACLIMATE).filterDate(f"{BASE[0]}-01-01", f"{BASE[1] + 1}-01-01")

    def annual_cwd(y):
        # TerraClimate `def` is in tenths of mm (scale 0.1).
        yc = tc.filter(ee.Filter.calendarRange(y, y, "year")).select("def")
        return yc.sum().multiply(0.1).rename("cwd")

    yearly_cwd = ee.ImageCollection(ee.List.sequence(BASE[0], BASE[1]).map(annual_cwd))
    cwd_mm = yearly_cwd.mean().rename("cwd_mm")
    cwd_p90_mm = yearly_cwd.reduce(ee.Reducer.percentile([90])).rename("cwd_p90_mm")

    # 12 climatological monthly mean VPD images (TerraClimate `vpd` is in 0.01 kPa).
    monthly_vpd = [
        tc.filter(ee.Filter.calendarRange(m, m, "month")).select("vpd").mean().multiply(0.01)
        for m in range(1, 13)
    ]
    # Peak-quarter VPD: max over all 12 circular 3-month rolling windows.
    quarters = [
        monthly_vpd[i].add(monthly_vpd[(i + 1) % 12]).add(monthly_vpd[(i + 2) % 12]).divide(3.0).rename("vpd_q")
        for i in range(12)
    ]
    vpd_summer_kpa = ee.ImageCollection(quarters).max().rename("vpd_summer_kpa")

    return cwd_mm.addBands(cwd_p90_mm).addBands(vpd_summer_kpa).select(TC_BANDS)


def wettest_month_image() -> ee.Image:
    """Per-pixel wettest month (1..12) from 1991-2020 ERA5-Land monthly precipitation."""
    monthly = ee.ImageCollection(ERA5_MONTHLY).select("total_precipitation_sum").filterDate(
        f"{BASE[0]}-01-01", f"{BASE[1] + 1}-01-01"
    )

    def month_p(m):
        m = ee.Number(m)
        return (
            monthly.filter(ee.Filter.calendarRange(m, m, "month"))
            .mean()
            .rename("p")
            .addBands(ee.Image.constant(m).toInt16().rename("m"))
        )

    return ee.ImageCollection(ee.List.sequence(1, 12).map(month_p)).qualityMosaic("p").select("m").toInt16()


def thermal_winter_month_image() -> ee.Image:
    """Per-pixel thermal-winter month (1..12), opposite the warmest month of 1991-2020."""
    monthly = ee.ImageCollection(ERA5_MONTHLY).select("temperature_2m").filterDate(
        f"{BASE[0]}-01-01", f"{BASE[1] + 1}-01-01"
    )

    def month_t(m):
        m = ee.Number(m)
        return (
            monthly.filter(ee.Filter.calendarRange(m, m, "month"))
            .mean()
            .rename("t")
            .addBands(ee.Image.constant(m).toInt16().rename("m"))
        )

    warm_m = ee.ImageCollection(ee.List.sequence(1, 12).map(month_t)).qualityMosaic("t").select("m")
    # Opposite month in 1..12: ((warm_m + 5) % 12) + 1
    return warm_m.add(5).mod(12).add(1).toInt16().rename("winter_m")


def cdd_chunk_image(y0: int, y1: int) -> ee.Image:
    """Compute mean annual max consecutive dry days (< 0.001 m) for years [y0..=y1].

    Uses a monthly `(len, prefix_dry, suffix_dry, max_dry)` monoid so `iterate()` depth
    is at most 31 days per month + 24 monthly merges, anchored at each cell's wettest month.
    """
    wet_m = wettest_month_image()
    land_mask = wet_m.mask()
    daily_p = ee.ImageCollection(ERA5_DAILY).select("total_precipitation_sum")

    def month_cdd(y, m):
        start = ee.Date.fromYMD(y, m, 1)
        days = daily_p.filterDate(start, start.advance(1, "month")).sort("system:time_start")
        init = ee.Image.constant([0, 0, 0, 0]).toInt16().rename(["len", "pref", "suff", "mx"])

        def step(img, acc):
            acc = ee.Image(acc)
            # CRITICAL UNIT: ERA5-Land total_precipitation_sum is in METERS (< 0.001 m = < 1 mm).
            dry = img.lt(0.001).toInt16()
            ln = acc.select("len")
            pref = acc.select("pref")
            suff = acc.select("suff")
            mx = acc.select("mx")
            new_pref = pref.add(pref.eq(ln).And(dry))
            new_suff = suff.add(1).multiply(dry)
            new_mx = mx.max(new_suff)
            return (
                ln.add(1)
                .rename("len")
                .addBands(new_pref.rename("pref"))
                .addBands(new_suff.rename("suff"))
                .addBands(new_mx.rename("mx"))
            )

        return ee.Image(days.iterate(step, init))

    def year_cdd(y):
        y = ee.Number(y)
        m0 = ee.List.sequence(1, 12).map(lambda m: month_cdd(y, m).set("idx", ee.Number(m)))
        m1 = ee.List.sequence(1, 12).map(lambda m: month_cdd(y.add(1), m).set("idx", ee.Number(m).add(12)))
        blocks = ee.ImageCollection(m0.cat(m1)).sort("idx")
        init = ee.Image.constant([0, 0, 0, 0]).toInt16().rename(["len", "pref", "suff", "mx"])

        def merge(blk, acc):
            blk = ee.Image(blk)
            acc = ee.Image(acc)
            idx = ee.Image.constant(ee.Number(blk.get("idx"))).toInt16()
            in_wy = idx.gte(wet_m).And(idx.lt(wet_m.add(12)))
            l_len, l_pref, l_suff, l_mx = (
                acc.select("len"),
                acc.select("pref"),
                acc.select("suff"),
                acc.select("mx"),
            )
            r_len, r_pref, r_suff, r_mx = (
                blk.select("len"),
                blk.select("pref"),
                blk.select("suff"),
                blk.select("mx"),
            )
            merged = (
                l_len.add(r_len)
                .rename("len")
                .addBands(l_pref.add(r_pref.multiply(l_pref.eq(l_len))).rename("pref"))
                .addBands(r_suff.add(l_suff.multiply(r_suff.eq(r_len))).rename("suff"))
                .addBands(l_mx.max(r_mx).max(l_suff.add(r_pref)).rename("mx"))
            )
            return acc.where(in_wy, merged).toInt16()

        return ee.Image(blocks.iterate(merge, init)).select("mx").min(365).updateMask(land_mask).toFloat().rename("cdd")

    yearly = ee.ImageCollection(ee.List.sequence(y0, y1).map(year_cdd))
    return yearly.mean().updateMask(land_mask).rename("cdd_max_days")


def phenology_chunk_image(y0: int, y1: int, hemi: str | None = None) -> ee.Image:
    """Compute 5-band Fishman Dynamic Chill Portions (dT = 0..+4 C) mean for years [y0..=y1].

    Uses 7 parallel monthly `iterate()` chunks per winter season (each with a 2-day warm-up
    overlap to initialize intermediate chill `x`, `float32` accumulator) and 6 four-hourly
    closed-form ODE steps per day (`DT_HOURS = 4.0`).
    When `hemi` is `"nh"` or `"sh"`, evaluates only that hemisphere's 7-month winter window
    (`Oct(y)-Apr(y+1)` for NH, `Apr(y)-Oct(y)` for SH) without reading ERA5 monthly data.
    When `hemi` is `None` (used by `check`), blends NH/SH at the equator (`lat >= 0`).
    """
    daily_t = ee.ImageCollection(ERA5_DAILY).select(["temperature_2m_min", "temperature_2m_max"])
    land_mask = daily_t.first().select("temperature_2m_min").mask()
    n_steps = int(round(24.0 / DT_HOURS))
    cos_w = [-0.5 * math.cos(2.0 * math.pi * (h * DT_HOURS - 3.0) / 24.0) for h in range(n_steps)]
    dt_shifts = ee.Image.constant([0.0, 1.0, 2.0, 3.0, 4.0]).toFloat()
    x_bands = [f"x_{i}" for i in range(5)]
    cp_bands = PHENO_BANDS

    def fishman_month(start_date):
        start_date = ee.Date(start_date)
        days = daily_t.filterDate(start_date.advance(-2, "day"), start_date.advance(1, "month")).sort(
            "system:time_start"
        )
        init = (
            ee.Image.constant([0.0] * 5)
            .toFloat()
            .rename(x_bands)
            .addBands(ee.Image.constant([0.0] * 5).toFloat().rename(cp_bands))
        )
        start_ms = start_date.millis()

        def step(img, acc):
            acc = ee.Image(acc)
            x = acc.select(x_bands)
            cp = acc.select(cp_bands)
            count_it = ee.Image.constant(ee.Number(img.get("system:time_start")).gte(start_ms)).toFloat()
            tmin = img.select("temperature_2m_min")
            tmax = img.select("temperature_2m_max")
            tmid = tmin.add(tmax).multiply(0.5)
            trange = tmax.subtract(tmin).max(0.0)
            for cw in cos_w:
                tk = tmid.add(trange.multiply(cw)).add(dt_shifts).max(220.0).min(340.0)
                xs = tk.expression("5.434359174e-14 * exp(8735.3 / tk)", {"tk": tk})
                k1 = tk.expression("2.567e18 * exp(-12888.8 / tk)", {"tk": tk})
                x_inter = xs.expression("xs - (xs - x) * exp(-k1 * dt)", {"xs": xs, "x": x, "k1": k1, "dt": DT_HOURS})
                xi = tk.expression(
                    "1.0 / (1.0 + exp(-max(-50.0, min(50.0, 443.2 * (tk - 277.0) / tk))))",
                    {"tk": tk},
                )
                trig = x_inter.gte(1.0)
                cp = cp.add(x_inter.multiply(xi).multiply(trig).multiply(count_it))
                x = x_inter.where(trig, x_inter.multiply(ee.Image.constant(1.0).subtract(xi)))
            return x.toFloat().rename(x_bands).addBands(cp.toFloat().rename(cp_bands))

        return ee.Image(days.iterate(step, init)).select(cp_bands)

    def season_cp(y):
        y = ee.Number(y)
        nh_starts = ee.List([
            ee.Date.fromYMD(y, 10, 1),
            ee.Date.fromYMD(y, 11, 1),
            ee.Date.fromYMD(y, 12, 1),
            ee.Date.fromYMD(y.add(1), 1, 1),
            ee.Date.fromYMD(y.add(1), 2, 1),
            ee.Date.fromYMD(y.add(1), 3, 1),
            ee.Date.fromYMD(y.add(1), 4, 1),
        ])
        sh_starts = ee.List([
            ee.Date.fromYMD(y, 4, 1),
            ee.Date.fromYMD(y, 5, 1),
            ee.Date.fromYMD(y, 6, 1),
            ee.Date.fromYMD(y, 7, 1),
            ee.Date.fromYMD(y, 8, 1),
            ee.Date.fromYMD(y, 9, 1),
            ee.Date.fromYMD(y, 10, 1),
        ])
        if hemi == "nh":
            return ee.ImageCollection(nh_starts.map(fishman_month)).sum().updateMask(land_mask).toFloat()
        if hemi == "sh":
            return ee.ImageCollection(sh_starts.map(fishman_month)).sum().updateMask(land_mask).toFloat()
        is_nh = ee.Image.pixelLonLat().select("latitude").gte(0.0)
        nh = ee.ImageCollection(nh_starts.map(fishman_month)).sum()
        sh = ee.ImageCollection(sh_starts.map(fishman_month)).sum()
        return sh.where(is_nh, nh).updateMask(land_mask).toFloat()

    if y0 == y1:
        return season_cp(y0).select(PHENO_BANDS)
    yearly = ee.ImageCollection(ee.List.sequence(y0, y1).map(season_cp))
    return yearly.mean().updateMask(land_mask).select(PHENO_BANDS)


def export_img(img: ee.Image, name: str, grid: dict | None = None, shard_size: int | None = None) -> str:
    kwargs = dict(
        image=img,
        description=name,
        folder=DRIVE_FOLDER,
        fileNamePrefix=name,
        maxPixels=1e10,
        **(grid or GRID_E5L),
    )
    if shard_size is not None:
        kwargs["shardSize"] = shard_size
    task = ee.batch.Export.image.toDrive(**kwargs)
    task.start()
    print(f"started {name}: {task.id}")
    return task.id


def cmd_check(args) -> None:
    """Spot-check TerraClimate water balance (30y), CDD (1y), and Dynamic Chill Portions (1y) at 5 cities."""
    pts = [
        ("Paris", 48.8566, 2.3522),
        ("Canberra", -35.2809, 149.1300),
        ("Davis_CA", 38.5449, -121.7405),
        ("Quito", -0.1807, -78.4678),
        ("Singapore", 1.3521, 103.8198),
    ]
    fc = ee.FeatureCollection([ee.Feature(ee.Geometry.Point([lon, lat]), {"name": n}) for n, lat, lon in pts])

    print("--- TerraClimate (1991-2020) ---")
    res_tc = terraclimate_water_balance_image().sampleRegions(collection=fc, scale=11132, geometries=False).getInfo()
    for feat in res_tc["features"]:
        props = feat["properties"]
        name = props.pop("name")
        print(f"{name:12} {json.dumps({k: round(v, 2) for k, v in sorted(props.items())})}")

    print("--- ERA5-Land CDD (2019 check) ---")
    res_cdd = cdd_chunk_image(2019, 2019).sampleRegions(collection=fc, scale=11132, geometries=False).getInfo()
    for feat in res_cdd["features"]:
        props = feat["properties"]
        name = props.pop("name")
        print(f"{name:12} {json.dumps({k: round(v, 2) for k, v in sorted(props.items())})}")

    print("--- ERA5-Land Phenology Fishman CP (2019 check) ---")
    res_ph = phenology_chunk_image(2019, 2019).sampleRegions(collection=fc, scale=11132, geometries=False).getInfo()
    for feat in res_ph["features"]:
        props = feat["properties"]
        name = props.pop("name")
        print(f"{name:12} {json.dumps({k: round(v, 1) for k, v in sorted(props.items())})}")


def cmd_export(args) -> None:
    which = set(args.only.split(",")) if args.only else {"tc", "cdd", "pheno"}

    if "tc" in which:
        tc = terraclimate_water_balance_image()
        out_tc = (
            tc.select("cwd_mm")
            .round()
            .toInt16()
            .addBands(tc.select("cwd_p90_mm").round().toInt16())
            .addBands(tc.select("vpd_summer_kpa").multiply(100).round().toInt16())
            .unmask(NODATA, False)
        )
        export_img(out_tc, TC_NAME)

    if "cdd" in which:
        for y0, y1 in CDD_CHUNKS:
            img = cdd_chunk_image(y0, y1).multiply(10).round().toInt16().unmask(NODATA, False)
            export_img(img, cdd_chunk_name(y0, y1))

    if "pheno" in which:
        for hemi in PHENO_HEMIS:
            grid = GRID_PHENO_NH if hemi == "nh" else GRID_PHENO_SH
            for y0, y1 in PHENO_CHUNKS:
                img = phenology_chunk_image(y0, y1, hemi=hemi).multiply(10).round().toInt16().unmask(NODATA, False)
                export_img(img, pheno_chunk_name(hemi, y0, y1), grid=grid, shard_size=PHENO_SHARD_SIZE)


def cmd_status(args) -> None:
    want = set(all_task_names())
    for t in ee.data.getTaskList()[:100]:
        desc = t.get("description", "")
        if desc in want:
            print(f"{t['state']:10} {t['id']:26} {desc:32} {t.get('error_message', '')}")


def drive():
    from googleapiclient.discovery import build

    return build("drive", "v3", credentials=ee.data.get_persistent_credentials(), cache_discovery=False)


def cmd_download(args) -> None:
    from googleapiclient.http import MediaIoBaseDownload

    out = Path(args.out) / "drive"
    out.mkdir(parents=True, exist_ok=True)
    d = drive()
    q = f"name = '{DRIVE_FOLDER}' and mimeType = 'application/vnd.google-apps.folder' and trashed = false"
    folders = d.files().list(q=q, fields="files(id)").execute()["files"]
    want = {f"{n}.tif" for n in all_task_names()}
    files = [
        f
        for fo in folders
        for f in d.files()
        .list(q=f"'{fo['id']}' in parents and trashed = false", fields="files(id,name,size)", pageSize=200)
        .execute()["files"]
        if f["name"] in want
    ]
    for f in sorted(files, key=lambda f: f["name"]):
        path = out / f["name"]
        if path.exists() and path.stat().st_size == int(f["size"]):
            print(f"cached {path} ({path.stat().st_size // 1024} KB)")
            continue
        buf = io.BytesIO()
        dl = MediaIoBaseDownload(buf, d.files().get_media(fileId=f["id"]), chunksize=1 << 24)
        done = False
        while not done:
            _, done = dl.next_chunk()
        path.write_bytes(buf.getvalue())
        print(f"downloaded {path} ({path.stat().st_size // 1024} KB)")


def _write_gtiff(dst_path: Path, data, profile, descriptions: list[str]) -> None:
    import rasterio

    prof = profile.copy()
    prof.update(
        count=data.shape[0],
        dtype="int16",
        nodata=NODATA,
        compress="deflate",
        predictor=2,
        tiled=True,
        blockxsize=256,
        blockysize=256,
    )
    with rasterio.open(dst_path, "w", **prof) as dst:
        dst.write(data)
        dst.descriptions = tuple(descriptions)
    print(f"wrote {dst_path} ({dst_path.stat().st_size // 1024} KB)")


def cmd_combine(args) -> None:
    """Combine chunked Drive GeoTIFFs into final v0.6.0 pipeline inputs in <out>."""
    import numpy as np
    import rasterio

    raw = Path(args.out) / "drive"
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)

    # 1. TerraClimate water balance (3 bands: cwd_mm, cwd_p90_mm, vpd_summer_kpa_x100)
    tc_src = raw / f"{TC_NAME}.tif"
    if tc_src.exists():
        with rasterio.open(tc_src) as src:
            data = src.read()
            prof = src.profile
        _write_gtiff(out / f"{TC_NAME}.tif", data, prof, TC_BANDS)

    # 2. ERA5-Land CDD (average 3 x 10-year chunks stored in tenths -> int16 days)
    cdd_paths = [raw / f"{cdd_chunk_name(a, b)}.tif" for a, b in CDD_CHUNKS]
    if all(p.exists() for p in cdd_paths):
        arrays = []
        prof = None
        for p in cdd_paths:
            with rasterio.open(p) as src:
                arr = src.read(1).astype(np.float32)
                arr[arr == NODATA] = np.nan
                arrays.append(arr / 10.0)
                if prof is None:
                    prof = src.profile
        mean_cdd = np.nanmean(np.stack(arrays, axis=0), axis=0)
        out_arr = np.where(np.isnan(mean_cdd), NODATA, np.rint(mean_cdd)).astype(np.int16)[None, :, :]
        _write_gtiff(out / "era5land_cdd.tif", out_arr, prof, ["cdd_max_days"])

    # 3. ERA5-Land Phenology (local exact 1-hour continuous 7-month Fishman ODE from pheno_daily/
    #    at 0.25-deg, averaged over 1991-2020, stacked NH+SH, and bilinearly resampled to GRID_E5L)
    daily_dir = out / "pheno_daily"
    all_daily = [daily_dir / f"{h}_{v}_{y}_{m:02d}.tif" for h, v, y, m in _pheno_month_jobs()]
    if all(p.exists() for p in all_daily):
        import concurrent.futures
        import os
        import time
        from rasterio.transform import Affine
        from rasterio.warp import Resampling, reproject

        t0 = time.time()
        so_path = _compile_fishman_so(daily_dir)
        tasks = [(hemi, y, str(daily_dir), so_path) for hemi in ("nh", "sh") for y in range(BASE[0], BASE[1] + 1)]
        nh_sum = sh_sum = nh_cnt = sh_cnt = None
        src_dy = -0.5

        workers = min(os.cpu_count() or 4, 8)
        print(f"computing exact 1-hour Fishman CP for {len(tasks)} hemisphere-seasons on {workers} cores...")
        with concurrent.futures.ProcessPoolExecutor(max_workers=workers) as pool:
            for hemi, _y, arr, dy in pool.map(_fishman_season_worker, tasks):
                src_dy = dy
                if nh_sum is None:
                    nh_sum = np.zeros_like(arr, dtype=np.float64)
                    sh_sum = np.zeros_like(arr, dtype=np.float64)
                    nh_cnt = np.zeros(arr.shape, dtype=np.int32)
                    sh_cnt = np.zeros(arr.shape, dtype=np.int32)
                ok = ~np.isnan(arr)
                if hemi == "nh":
                    nh_sum[ok] += arr[ok]
                    nh_cnt[ok] += 1
                else:
                    sh_sum[ok] += arr[ok]
                    sh_cnt[ok] += 1

        nh_mean = np.where(nh_cnt > 0, nh_sum / np.maximum(nh_cnt, 1), np.nan).astype(np.float32)
        sh_mean = np.where(sh_cnt > 0, sh_sum / np.maximum(sh_cnt, 1), np.nan).astype(np.float32)
        print(f"computed 30-yr hourly Fishman CP in {time.time() - t0:.1f}s")

        global_grid = np.concatenate([nh_mean, sh_mean], axis=1)
        src_transform = Affine(0.25, 0.0, -180.0, 0.0, src_dy, 90.0)
        dst_transform = Affine(0.1, 0.0, -180.05, 0.0, -0.1, 90.05)
        dst_shape = (len(PHENO_BANDS), 1801, 3600)

        dst_bilin = np.full(dst_shape, np.nan, dtype=np.float32)
        dst_near = np.full(dst_shape, np.nan, dtype=np.float32)
        for b in range(len(PHENO_BANDS)):
            reproject(
                source=global_grid[b],
                destination=dst_bilin[b],
                src_transform=src_transform,
                src_crs="EPSG:4326",
                dst_transform=dst_transform,
                dst_crs="EPSG:4326",
                src_nodata=np.nan,
                dst_nodata=np.nan,
                resampling=Resampling.bilinear,
            )
            reproject(
                source=global_grid[b],
                destination=dst_near[b],
                src_transform=src_transform,
                src_crs="EPSG:4326",
                dst_transform=dst_transform,
                dst_crs="EPSG:4326",
                src_nodata=np.nan,
                dst_nodata=np.nan,
                resampling=Resampling.nearest,
            )
        mean_ph = np.where(np.isnan(dst_bilin), dst_near, dst_bilin)
        out_arr = np.where(np.isnan(mean_ph), NODATA, np.rint(np.clip(mean_ph, 0.0, 250.0))).astype(np.int16)
        prof = {
            "driver": "GTiff",
            "crs": "EPSG:4326",
            "transform": dst_transform,
            "width": 3600,
            "height": 1801,
        }
        _write_gtiff(out / "era5land_phenology.tif", out_arr, prof, PHENO_BANDS)


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--project", required=True, help="Google Cloud project registered for Earth Engine")
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("check", help="spot-check 5 cities in GEE before launching global exports")
    exp = sub.add_parser("export", help="start Earth Engine exports to Google Drive")
    exp.add_argument("--only", help="comma-separated subset of: tc,cdd,pheno")
    sub.add_parser("status", help="list Earth Engine export task status")
    dl = sub.add_parser("download", help="fetch exported GeoTIFFs from Google Drive")
    dl.add_argument("--out", required=True, help="directory under which drive/ is stored")
    pf = sub.add_parser("pheno-fetch", help="download monthly 0.25-deg ERA5-Land daily Tmin/Tmax via getDownloadURL")
    pf.add_argument("--out", required=True, help="directory under which pheno_daily/ is stored")
    pf.add_argument("--workers", type=int, default=6, help="parallel GEE download workers (default: 6)")
    cb = sub.add_parser("combine", help="combine Drive GeoTIFFs + pheno_daily into build inputs")
    cb.add_argument("--out", required=True, help="same directory passed to download / pheno-fetch")
    args = p.parse_args()

    if args.cmd != "combine":
        ee.Initialize(project=args.project)
    {
        "check": cmd_check,
        "export": cmd_export,
        "status": cmd_status,
        "download": cmd_download,
        "pheno-fetch": cmd_pheno_fetch,
        "combine": cmd_combine,
    }[args.cmd](args)


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        print(f"error: {exc}", file=sys.stderr)
        sys.exit(1)
