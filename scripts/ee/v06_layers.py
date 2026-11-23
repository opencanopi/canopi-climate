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

3. `era5land_phenology_<y0>_<y1>` (6 x 5-year chunks: 1991-1995 .. 2016-2020; 5 bands,
   int16 tenths of CP, NODATA = -32768):
   From `ECMWF/ERA5_LAND/DAILY_AGGR` (`temperature_2m_min`, `temperature_2m_max`):
   Fishman (1987) Dynamic Model Chill Portions accumulated over a 7-month (~212-day) winter
   window (Oct-Apr for thermal-NH pixels where `winter_month` is Oct-Mar, Apr-Oct for
   thermal-SH pixels where `winter_month` is Apr-Sep, derived from `MONTHLY_AGGR` 1991-2020).
   Uses 7 parallel monthly `iterate()` chunks (each with a 2-day warm-up overlap to initialize
   the intermediate product `x`) and 12 two-hourly closed-form ODE steps per day across
   dT in {0, +1, +2, +3, +4 C}:
   - `chill_portions_0c`, `chill_portions_1c`, `chill_portions_2c`,
     `chill_portions_3c`, `chill_portions_4c`

Usage:
    python scripts/ee/v06_layers.py --project canopi-515022 check
    python scripts/ee/v06_layers.py --project canopi-515022 export
    python scripts/ee/v06_layers.py --project canopi-515022 status
    python scripts/ee/v06_layers.py --project canopi-515022 download --out <dir>
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
PHENO_CHUNKS = [
    (1991, 1995),
    (1996, 2000),
    (2001, 2005),
    (2006, 2010),
    (2011, 2015),
    (2016, 2020),
]

TC_BANDS = ["cwd_mm", "cwd_p90_mm", "vpd_summer_kpa"]
PHENO_BANDS = [
    "chill_portions_0c",
    "chill_portions_1c",
    "chill_portions_2c",
    "chill_portions_3c",
    "chill_portions_4c",
]

DT_HOURS = 2.0  # 12 two-hourly closed-form Fishman ODE steps per day


def cdd_chunk_name(y0: int, y1: int) -> str:
    return f"era5land_cdd_{y0}_{y1}"


def pheno_chunk_name(y0: int, y1: int) -> str:
    return f"era5land_phenology_{y0}_{y1}"


def all_task_names() -> list[str]:
    return (
        [TC_NAME]
        + [cdd_chunk_name(a, b) for a, b in CDD_CHUNKS]
        + [pheno_chunk_name(a, b) for a, b in PHENO_CHUNKS]
    )


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


def phenology_chunk_image(y0: int, y1: int) -> ee.Image:
    """Compute 5-band Fishman Dynamic Chill Portions (dT = 0..+4 C) mean for years [y0..=y1].

    Uses 7 parallel monthly `iterate()` chunks per winter season (each with a 2-day warm-up
    overlap to initialize intermediate chill `x`) and 12 two-hourly closed-form ODE steps per day.
    Thermal-NH pixels (`winter_m` in Oct-Mar) sum Oct(y)-Apr(y+1); thermal-SH pixels
    (`winter_m` in Apr-Sep) sum Apr(y)-Oct(y).
    """
    winter_m = thermal_winter_month_image()
    land_mask = winter_m.mask()
    is_nh_winter = winter_m.lte(3).Or(winter_m.gte(10))

    daily_t = ee.ImageCollection(ERA5_DAILY).select(["temperature_2m_min", "temperature_2m_max"])
    cos_w = [-0.5 * math.cos(2.0 * math.pi * (h - 3.0) / 24.0) for h in range(0, 24, int(DT_HOURS))]
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
            return x.rename(x_bands).addBands(cp.rename(cp_bands))

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
        nh = ee.ImageCollection(nh_starts.map(fishman_month)).sum()
        sh = ee.ImageCollection(sh_starts.map(fishman_month)).sum()
        return sh.where(is_nh_winter, nh).updateMask(land_mask).toFloat()

    yearly = ee.ImageCollection(ee.List.sequence(y0, y1).map(season_cp))
    return yearly.mean().updateMask(land_mask).select(PHENO_BANDS)


def export_img(img: ee.Image, name: str) -> str:
    task = ee.batch.Export.image.toDrive(
        image=img,
        description=name,
        folder=DRIVE_FOLDER,
        fileNamePrefix=name,
        maxPixels=1e10,
        **GRID_E5L,
    )
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
        for y0, y1 in PHENO_CHUNKS:
            img = phenology_chunk_image(y0, y1).multiply(10).round().toInt16().unmask(NODATA, False)
            export_img(img, pheno_chunk_name(y0, y1))


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

    # 3. ERA5-Land Phenology (average 6 x 5-year chunks stored in tenths -> int16 CP)
    ph_paths = [raw / f"{pheno_chunk_name(a, b)}.tif" for a, b in PHENO_CHUNKS]
    if all(p.exists() for p in ph_paths):
        arrays = []
        prof = None
        for p in ph_paths:
            with rasterio.open(p) as src:
                arr = src.read().astype(np.float32)
                arr[arr == NODATA] = np.nan
                arrays.append(arr / 10.0)
                if prof is None:
                    prof = src.profile
        mean_ph = np.nanmean(np.stack(arrays, axis=0), axis=0)
        out_arr = np.where(np.isnan(mean_ph), NODATA, np.rint(np.clip(mean_ph, 0.0, 250.0))).astype(np.int16)
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
    cb = sub.add_parser("combine", help="combine chunked Drive GeoTIFFs into build inputs")
    cb.add_argument("--out", required=True, help="same directory passed to download")
    args = p.parse_args()

    ee.Initialize(project=args.project)
    {
        "check": cmd_check,
        "export": cmd_export,
        "status": cmd_status,
        "download": cmd_download,
        "combine": cmd_combine,
    }[args.cmd](args)


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        print(f"error: {exc}", file=sys.stderr)
        sys.exit(1)
