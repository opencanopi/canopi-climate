"""Tail extremes and frost calendar layers for canopi-climate v0.5.0, computed on Google Earth Engine.

Exports two static 0.1-degree GeoTIFFs on ERA5-Land's own grid (`GRID_E5L`, 3600x1801) to the
Google Drive folder `canopi-climate-ee`, then `download` fetches them and `combine` writes the
build inputs (`ee/era5land_extremes_tail.tif` and `ee/era5land_frost_calendar.tif`):

1. `era5land_extremes_tail` (4 bands, int16 tenths, NODATA = -32768):
   - `tmin_p10_gap`: P10(annual TNn) - mean(annual TNn), 1991-2020 (<= 0.0 C)
   - `tmax_p90_gap`: P90(annual TXx) - mean(annual TXx), 1991-2020 (>= 0.0 C)
   - `heat_days_35c`: mean annual days with Tmax >= 35.0 C, 1991-2020 (days/yr)
   - `recent_trend_c`: mean(Tmean, 2015-2025) - mean(Tmean, 1991-2020) (C)

2. `era5land_frost_calendar` (6 bands, int16 units, NODATA = -32768, 1991-2020):
   Anchored per pixel to each cell's thermal-winter DOY (`winter_doy`, opposite the warmest month
   `m_warm` from 1991-2020 monthly mean `temperature_2m`) so Northern Hemisphere, Southern
   Hemisphere, and equatorial highlands are handled uniformly without Dec/Jan wrap artifacts:
   - `last_spring_frost_doy`: median calendar DOY (1..365) of last frost (Tmin <= 0 C) in frost
     years; 0 = frost-free (0% of years freeze); 366 = year-round frost (frost_free_days == 0).
   - `last_spring_frost_p90_doy`: 90th-percentile (1-in-10 late) last spring frost calendar DOY
     (1..365, or sentinel 0 / 366).
   - `first_autumn_frost_doy`: median calendar DOY (1..365) of first autumn frost (Tmin <= 0 C)
     in frost years (or sentinel 0 / 366).
   - `frost_free_days`: unconditional median frost-free season length (0..365).
   - `frost_year_pct`: percentage of years (0..100) with at least one day Tmin <= 0 C.
   - `false_spring_pct`: percentage of years (0..100) with a killing freeze (Tmin <= -2 C)
     occurring after >= 150 cumulative GDD (base 5 C) accumulated from `winter_doy`.

Usage:
    python scripts/ee/v05_layers.py --project canopi-515022 check
    python scripts/ee/v05_layers.py --project canopi-515022 export
    python scripts/ee/v05_layers.py --project canopi-515022 status
    python scripts/ee/v05_layers.py --project canopi-515022 download --out <dir>
    python scripts/ee/v05_layers.py --project canopi-515022 frost-fetch --out <dir>
    python scripts/ee/v05_layers.py --project canopi-515022 combine --out <dir>
"""

import argparse
import calendar
import io
import json
import sys
from pathlib import Path

import ee

ERA5_LAND = "ECMWF/ERA5_LAND/DAILY_AGGR"
ERA5_MONTHLY = "ECMWF/ERA5_LAND/MONTHLY_AGGR"
BASE = (1991, 2020)
RECENT = (2015, 2025)
NODATA = -32768
DRIVE_FOLDER = "canopi-climate-ee"

# ERA5-Land native 0.1-degree grid (matching scripts/ee/extremes.py).
GRID_E5L = dict(
    crs="EPSG:4326",
    crsTransform=[0.1, 0, -180.05, 0, -0.1, 90.05],
    dimensions="3600x1801",
)

# 6 latitude strips tiling GRID_E5L (3600x1801) exactly for computePixels (<48 MiB/request).
# Each tuple is (strip_idx, row_start, height, translate_y).
FROST_STRIPS = [
    (0, 0, 300, 90.05),
    (1, 300, 300, 60.05),
    (2, 600, 300, 30.05),
    (3, 900, 300, 0.05),
    (4, 1200, 300, -29.95),
    (5, 1500, 301, -59.95),
]

TAIL_NAME = "era5land_extremes_tail"
FROST_NAME = "era5land_frost_calendar"

TAIL_BANDS = ["tmin_p10_gap", "tmax_p90_gap", "heat_days_35c", "recent_trend_c"]
FROST_BANDS = [
    "last_spring_frost_doy",
    "last_spring_frost_p90_doy",
    "first_autumn_frost_doy",
    "frost_free_days",
    "frost_year_pct",
    "false_spring_pct",
]


def extremes_tail_image() -> ee.Image:
    """Compute the 4 static `extremes_tail` bands (float, physical units) from ERA5-Land daily."""
    daily = ee.ImageCollection(ERA5_LAND).select(
        ["temperature_2m", "temperature_2m_min", "temperature_2m_max"]
    )
    base_col = daily.filterDate(f"{BASE[0]}-01-01", f"{BASE[1] + 1}-01-01")
    recent_col = daily.filterDate(f"{RECENT[0]}-01-01", f"{RECENT[1] + 1}-01-01")

    def annual_stats(y):
        yc = base_col.filter(ee.Filter.calendarRange(y, y, "year"))
        tnn = yc.select("temperature_2m_min").min().rename("tnn")
        txx = yc.select("temperature_2m_max").max().rename("txx")
        # 35.0 C = 308.15 K
        hd35 = yc.select("temperature_2m_max").map(lambda i: i.gte(308.15)).sum().rename("hd35")
        return tnn.addBands(txx).addBands(hd35)

    yearly = ee.ImageCollection(ee.List.sequence(BASE[0], BASE[1]).map(annual_stats))
    tnn_mean = yearly.select("tnn").mean()
    txx_mean = yearly.select("txx").mean()
    tnn_p10 = yearly.select("tnn").reduce(ee.Reducer.percentile([10]))
    txx_p90 = yearly.select("txx").reduce(ee.Reducer.percentile([90]))

    # Gaps are clamped so tmin_p10_gap <= 0 and tmax_p90_gap >= 0.
    tmin_p10_gap = tnn_p10.subtract(tnn_mean).min(0).rename("tmin_p10_gap")
    tmax_p90_gap = txx_p90.subtract(txx_mean).max(0).rename("tmax_p90_gap")
    heat_days_35c = yearly.select("hd35").mean().rename("heat_days_35c")

    base_tmean = base_col.select("temperature_2m").mean()
    recent_tmean = recent_col.select("temperature_2m").mean()
    recent_trend_c = recent_tmean.subtract(base_tmean).rename("recent_trend_c")

    return (
        tmin_p10_gap.addBands(tmax_p90_gap)
        .addBands(heat_days_35c)
        .addBands(recent_trend_c)
        .select(TAIL_BANDS)
    )


def thermal_winter_doy(base_col: ee.ImageCollection) -> ee.Image:
    """Per-pixel thermal-winter anchor DOY (1..365), opposite the warmest month of 1991-2020."""
    # Mid-month calendar DOYs (1-based) for months 1..12: [15, 45, 74, ..., 349].
    mid_doys = [15, 45, 74, 105, 135, 166, 196, 227, 258, 288, 319, 349]

    def month_img(m_idx):
        m = ee.Number(m_idx).add(1)
        doy = ee.Number(ee.List(mid_doys).get(m_idx))
        tmean = base_col.filter(ee.Filter.calendarRange(m, m, "month")).select("temperature_2m").mean()
        return tmean.rename("t").addBands(ee.Image.constant(doy).toInt16().rename("doy"))

    # qualityMosaic picks the month with highest mean temperature -> summer_doy.
    summer_doy = (
        ee.ImageCollection(ee.List.sequence(0, 11).map(month_img))
        .qualityMosaic("t")
        .select("doy")
    )
    # Opposite side of the 365-day year (1..365).
    return summer_doy.subtract(182).subtract(1).add(365).mod(365).add(1).toInt16().rename("winter_doy")


def frost_calendar_image() -> ee.Image:
    """Compute the 6 static `frost_calendar` bands (int16) from ERA5-Land daily 1991-2020."""
    daily = ee.ImageCollection(ERA5_LAND).select(["temperature_2m", "temperature_2m_min"])
    base_col = daily.filterDate(f"{BASE[0]}-01-01", f"{BASE[1] + 1}-01-01")
    # Need 1991-01-01 through 2021-12-31 so frost-year 2020 (which crosses into 2021 in SH) is complete.
    ext_col = daily.filterDate(f"{BASE[0]}-01-01", f"{BASE[1] + 2}-01-01")

    winter_doy = thermal_winter_doy(base_col)
    land_mask = winter_doy.mask()

    def frost_year_stats(y):
        y_num = ee.Number(y)
        start = ee.Date.fromYMD(y_num, 1, 1)
        end = ee.Date.fromYMD(y_num.add(2), 1, 1)
        two_yr = ext_col.filterDate(start, end)

        def tag_day(img):
            date = ee.Date(img.get("system:time_start"))
            yr = date.get("year")
            # Clamp leap day 366 -> 365 so every year uses a uniform 1..365 DOY axis.
            doy = date.getRelative("day", "year").add(1).min(365)
            doy_img = ee.Image.constant(doy).toInt16()
            in_y0 = ee.Image.constant(yr.eq(y_num)).And(doy_img.gte(winter_doy))
            in_y1 = ee.Image.constant(yr.eq(y_num.add(1))).And(doy_img.lt(winter_doy))
            in_fy = in_y0.Or(in_y1)

            # Shifted frost-year DOY: 0 = winter_doy, 182 = midsummer, 364 = day before next winter.
            fy_doy = doy_img.subtract(winter_doy).add(365).mod(365).toInt16()

            tmin_c = img.select("temperature_2m_min").subtract(273.15)
            tmean_c = img.select("temperature_2m").subtract(273.15)

            is_frost = tmin_c.lte(0.0).And(in_fy)
            is_kill_spring = tmin_c.lte(-2.0).And(in_fy).And(fy_doy.lt(182))
            # Spring: 0 <= fy_doy < 182; Autumn: 182 <= fy_doy < 365
            spring_doy = fy_doy.updateMask(is_frost.And(fy_doy.lt(182))).rename("spring_fy")
            autumn_doy = fy_doy.updateMask(is_frost.And(fy_doy.gte(182))).rename("autumn_fy")
            kill_spring_doy = fy_doy.updateMask(is_kill_spring).rename("kill_spring_fy")
            gdd_day = tmean_c.subtract(5.0).max(0.0).updateMask(in_fy.And(fy_doy.lt(182))).toFloat().rename("gdd")
            return (
                spring_doy.addBands(autumn_doy)
                .addBands(kill_spring_doy)
                .addBands(is_frost.toInt16().rename("has_frost"))
                .addBands(gdd_day)
                .addBands(fy_doy.rename("fy_doy"))
            )

        tagged = two_yr.map(tag_day)

        last_spring_fy = tagged.select("spring_fy").max()  # masked if no spring frost
        first_autumn_fy = tagged.select("autumn_fy").min()  # masked if no autumn frost
        has_frost = tagged.select("has_frost").max().unmask(0).updateMask(land_mask).rename("has_frost")

        ff_start = last_spring_fy.add(1).unmask(0)
        ff_end = first_autumn_fy.unmask(365)
        ff_days = ff_end.subtract(ff_start).max(0).min(365).updateMask(land_mask).toInt16().rename("ff_days")

        # False spring (exact, O(1) streaming): because cumulative GDD is monotonic in fy_doy,
        # a spring killing freeze (Tmin <= -2 C, fy_doy < 182) occurs after >= 150 GDD iff
        # cumulative GDD from winter_doy up to the LAST spring killing freeze is >= 150.
        last_kill_fy = tagged.select("kill_spring_fy").max()
        gdd_to_last_kill = (
            tagged.map(lambda i: i.select("gdd").updateMask(i.select("fy_doy").lte(last_kill_fy)))
            .sum()
        )
        had_fs = (
            gdd_to_last_kill.gte(150.0)
            .unmask(0)
            .updateMask(land_mask)
            .toInt16()
            .rename("had_fs")
        )

        return (
            last_spring_fy.rename("spring_fy")
            .addBands(first_autumn_fy.rename("autumn_fy"))
            .addBands(ff_days)
            .addBands(has_frost)
            .addBands(had_fs)
        )

    years = ee.ImageCollection(ee.List.sequence(BASE[0], BASE[1]).map(frost_year_stats))

    # Conditional statistics in shifted frost-year coordinates (only over years with frost in that half-year).
    spring_med_fy = years.select("spring_fy").reduce(ee.Reducer.percentile([50]))
    spring_p90_fy = years.select("spring_fy").reduce(ee.Reducer.percentile([90]))
    autumn_med_fy = years.select("autumn_fy").reduce(ee.Reducer.percentile([50]))

    # Map shifted fy_doy back to calendar DOY 1..365.
    def to_cal_doy(fy_img):
        return fy_img.round().toInt16().add(winter_doy).subtract(1).mod(365).add(1).toInt16()

    ff_med = years.select("ff_days").reduce(ee.Reducer.percentile([50])).round().toInt16()
    frost_pct = years.select("has_frost").mean().multiply(100).round().toInt16()
    fs_pct = years.select("had_fs").mean().multiply(100).round().toInt16()

    # Apply sentinels:
    #   0   = frost-free (no frost in that half-year / 0% frost years)
    #   366 = year-round frost (median frost_free_days == 0)
    year_round = ff_med.eq(0)

    def apply_sentinels(fy_img, name):
        cal = to_cal_doy(fy_img).unmask(0)
        cal = cal.where(frost_pct.eq(0), 0)
        cal = cal.where(year_round, 366)
        return cal.updateMask(land_mask).toInt16().rename(name)

    last_spring = apply_sentinels(spring_med_fy, "last_spring_frost_doy")
    last_spring_p90 = apply_sentinels(spring_p90_fy, "last_spring_frost_p90_doy")
    first_autumn = apply_sentinels(autumn_med_fy, "first_autumn_frost_doy")
    ff_out = ff_med.where(frost_pct.eq(0), 365).updateMask(land_mask).toInt16().rename("frost_free_days")
    frost_pct_out = frost_pct.updateMask(land_mask).toInt16().rename("frost_year_pct")
    fs_pct_out = fs_pct.where(year_round, 0).where(frost_pct.eq(0), 0).updateMask(land_mask).toInt16().rename("false_spring_pct")

    return (
        last_spring.addBands(last_spring_p90)
        .addBands(first_autumn)
        .addBands(ff_out)
        .addBands(frost_pct_out)
        .addBands(fs_pct_out)
        .select(FROST_BANDS)
    )


def tenths(img: ee.Image) -> ee.Image:
    return img.multiply(10).round().toInt16().unmask(NODATA, False)


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
    """Spot-check tail extremes and frost calendar at 5 contrasting cities (NH, SH, tropics, polar)."""
    pts = [
        ("Paris", 48.8566, 2.3522),
        ("Canberra", -35.2809, 149.1300),
        ("Quito", -0.1807, -78.4678),
        ("Singapore", 1.3521, 103.8198),
        ("Longyearbyen", 78.2232, 15.6267),
    ]
    fc = ee.FeatureCollection([ee.Feature(ee.Geometry.Point([lon, lat]), {"name": n}) for n, lat, lon in pts])
    tail = extremes_tail_image()
    frost = frost_calendar_image()
    combined = tail.addBands(frost)
    res = combined.sampleRegions(collection=fc, scale=11132, geometries=False).getInfo()
    for feat in res["features"]:
        props = feat["properties"]
        name = props.pop("name")
        rounded = {k: (round(v, 2) if isinstance(v, float) else v) for k, v in sorted(props.items())}
        print(f"{name:14} {json.dumps(rounded)}")


def cmd_export(args) -> None:
    which = set(args.only.split(",")) if args.only else {"tail", "frost"}
    if "tail" in which:
        export_img(tenths(extremes_tail_image()), TAIL_NAME)
    if "frost" in which:
        export_img(frost_calendar_image().toInt16().unmask(NODATA, False), FROST_NAME)


def cmd_status(args) -> None:
    for t in ee.data.getTaskList()[:50]:
        desc = t.get("description", "")
        if desc in (TAIL_NAME, FROST_NAME):
            print(f"{t['state']:10} {t['id']:26} {desc:28} {t.get('error_message', '')}")


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
    want = {f"{TAIL_NAME}.tif", f"{FROST_NAME}.tif"}
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


def _frost_strip_grid(strip_idx: int) -> dict:
    _, _r0, h, ty = FROST_STRIPS[strip_idx]
    return {
        "dimensions": {"width": 3600, "height": h},
        "affineTransform": {
            "scaleX": 0.1,
            "shearX": 0,
            "translateX": -180.05,
            "shearY": 0,
            "scaleY": -0.1,
            "translateY": ty,
        },
        "crsCode": "EPSG:4326",
    }


def _frost_fetch_jobs() -> list[tuple[int, int, int]]:
    """All (strip_idx, year, month) jobs for 1991-01 through 2021-12."""
    return [
        (s, y, m)
        for y in range(BASE[0], BASE[1] + 2)
        for m in range(1, 13)
        for s, _, _, _ in FROST_STRIPS
    ]


def cmd_frost_fetch(args) -> None:
    """Fetch 0.1-deg packed daily frost/GDD strips (1991-2021) + winter_doy via computePixels."""
    import concurrent.futures
    import time

    out_dir = Path(args.out) / "frost_daily"
    out_dir.mkdir(parents=True, exist_ok=True)

    # 1. Fetch 1991-2020 thermal_winter_doy (6 strips, int16) from ERA5-Land monthly.
    monthly = ee.ImageCollection(ERA5_MONTHLY).filterDate(f"{BASE[0]}-01-01", f"{BASE[1] + 1}-01-01")
    wd_img = thermal_winter_doy(monthly).unmask(NODATA, False).toInt16()
    for s, _, _, _ in FROST_STRIPS:
        wd_path = out_dir / f"winter_doy_s{s}.tif"
        if wd_path.exists() and wd_path.stat().st_size > 1000:
            continue
        raw = ee.data.computePixels({"expression": wd_img, "fileFormat": "GEO_TIFF", "grid": _frost_strip_grid(s)})
        tmp = wd_path.with_suffix(".tif.tmp")
        tmp.write_bytes(raw)
        tmp.rename(wd_path)
        print(f"downloaded {wd_path.name} ({len(raw) // 1024} KB)", flush=True)

    # 2. Fetch packed int32 daily frost/GDD strips (4 days/band, 1 byte/day):
    #    bit 7 = (Tmin <= 0 C), bit 6 = (Tmin <= -2 C), bits 0..5 = round(clamp(max(0, Tmean-5), 0, 31) * 2).
    #    255 = NODATA.
    jobs = _frost_fetch_jobs()
    pending = [j for j in jobs if not (out_dir / f"s{j[0]}_{j[1]}_{j[2]:02d}.tif").exists()]
    print(f"frost-fetch: {len(jobs) - len(pending)}/{len(jobs)} cached, {len(pending)} to fetch", flush=True)
    if not pending:
        return

    def fetch_one(job: tuple[int, int, int]) -> tuple[str, int]:
        s, y, m = job
        dst = out_dir / f"s{s}_{y}_{m:02d}.tif"
        d0 = ee.Date.fromYMD(y, m, 1)
        d1 = d0.advance(1, "month")
        col = ee.ImageCollection(ERA5_LAND).filterDate(d0, d1).sort("system:time_start")

        def pack_day(img):
            tmin_c = img.select("temperature_2m_min").subtract(273.15)
            tmean_c = img.select("temperature_2m").subtract(273.15)
            is_frost = tmin_c.lte(0.0).toUint32().multiply(128)
            is_kill = tmin_c.lte(-2.0).toUint32().multiply(64)
            gdd_q = tmean_c.subtract(5.0).max(0.0).multiply(2.0).round().min(62).toUint32()
            return is_frost.add(is_kill).add(gdd_q).unmask(255, False).toUint32()

        lst = col.map(pack_day).toList(31)
        n = lst.size()

        def pack4(g):
            g = ee.Number(g)
            b0 = ee.Image(lst.get(g.multiply(4)))
            b1 = ee.Image(
                ee.Algorithms.If(g.multiply(4).add(1).lt(n), lst.get(g.multiply(4).add(1)), ee.Image.constant(255).toUint32())
            )
            b2 = ee.Image(
                ee.Algorithms.If(g.multiply(4).add(2).lt(n), lst.get(g.multiply(4).add(2)), ee.Image.constant(255).toUint32())
            )
            b3 = ee.Image(
                ee.Algorithms.If(g.multiply(4).add(3).lt(n), lst.get(g.multiply(4).add(3)), ee.Image.constant(255).toUint32())
            )
            return b0.bitwiseOr(b1.leftShift(8)).bitwiseOr(b2.leftShift(16)).bitwiseOr(b3.leftShift(24)).toUint32()

        n_groups = ee.Number(n).add(3).divide(4).floor()
        packed = ee.ImageCollection(ee.List.sequence(0, n_groups.subtract(1)).map(pack4)).toBands().toUint32()
        req = {"expression": packed, "fileFormat": "GEO_TIFF", "grid": _frost_strip_grid(s)}

        last_err = None
        for attempt in range(6):
            try:
                raw = ee.data.computePixels(req)
                if len(raw) < 1000:
                    raise RuntimeError(f"short response ({len(raw)} B)")
                tmp = dst.with_suffix(".tif.tmp")
                tmp.write_bytes(raw)
                tmp.rename(dst)
                return dst.name, len(raw)
            except Exception as exc:
                last_err = exc
                time.sleep(min(30.0, 2.0 * (attempt + 1)))
        raise RuntimeError(f"failed {dst.name}: {last_err}")

    t0 = time.time()
    done = 0
    total_bytes = 0
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
        futs = [pool.submit(fetch_one, j) for j in pending]
        for fut in concurrent.futures.as_completed(futs):
            _name, sz = fut.result()
            done += 1
            total_bytes += sz
            if done % 50 == 0 or done == len(pending):
                el = time.time() - t0
                rate = done / max(el, 0.1)
                eta = (len(pending) - done) / max(rate, 1e-6)
                print(
                    f"frost-fetch: {done}/{len(pending)} ({total_bytes / 1e6:.1f} MB, "
                    f"{rate:.2f} req/s, elapsed {el:.0f}s, ETA {eta:.0f}s)",
                    flush=True,
                )


_FROST_C_SRC = r"""
#include <math.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>

static int cmp_i16(const void *a, const void *b) {
    int16_t va = *(const int16_t *)a;
    int16_t vb = *(const int16_t *)b;
    return (va > vb) - (va < vb);
}

static float pct_sorted(const int16_t *arr, int n, float p) {
    if (n <= 0) return -1.0f;
    if (n == 1) return (float)arr[0];
    float pos = (p / 100.0f) * (float)(n - 1);
    int lo = (int)floorf(pos);
    int hi = (int)ceilf(pos);
    if (lo < 0) lo = 0;
    if (hi >= n) hi = n - 1;
    return (float)arr[lo] + (pos - (float)lo) * (float)(arr[hi] - arr[lo]);
}

/* Update active frost-year state for one month of packed int32 bands (4 days/band).
   Active slots: slot 0 = frost year (cy - 1), slot 1 = frost year cy.
   Arrays have shape (2, n_land) or (2, n_land, 182). */
void frost_update_month(
    const uint32_t *packed,     /* (n_bands, n_px) */
    const int32_t *land_idx,    /* (n_land,) */
    const int16_t *winter_doy,  /* (n_land,) in 1..365 */
    int n_px,
    int n_land,
    int n_days,
    int doy_start,              /* 1-based DOY of day 0 in this month */
    int has_slot0,              /* 1 if (cy - 1) in [1991..2020] */
    int has_slot1,              /* 1 if cy in [1991..2020] */
    int16_t *last_spring,       /* (2, n_land), init -1 */
    int16_t *first_autumn,      /* (2, n_land), init 365 */
    int16_t *last_kill,         /* (2, n_land), init -1 */
    uint8_t *has_frost,         /* (2, n_land), init 0 */
    uint16_t *gdd2_spring       /* (2, n_land, 182), init 0 */
) {
    for (int d = 0; d < n_days; d++) {
        int raw_doy = doy_start + d;
        int doy = raw_doy > 365 ? 365 : raw_doy;
        int g = d >> 2;
        int shift = (d & 3) << 3;
        const uint32_t *band = packed + (size_t)g * (size_t)n_px;
        for (int p = 0; p < n_land; p++) {
            uint8_t byte = (uint8_t)(band[land_idx[p]] >> shift);
            if (byte == 255) continue;
            int wd = (int)winter_doy[p];
            int slot;
            if (doy >= wd) {
                if (!has_slot1) continue;
                slot = 1;
            } else {
                if (!has_slot0) continue;
                slot = 0;
            }
            int fy_doy = doy - wd;
            if (fy_doy < 0) fy_doy += 365;
            size_t sp = (size_t)slot * (size_t)n_land + (size_t)p;
            if (byte & 128) {
                has_frost[sp] = 1;
                if (fy_doy < 182) {
                    if (fy_doy > last_spring[sp]) last_spring[sp] = (int16_t)fy_doy;
                } else {
                    if (fy_doy < first_autumn[sp]) first_autumn[sp] = (int16_t)fy_doy;
                }
            }
            if (fy_doy < 182) {
                if (byte & 64) {
                    if (fy_doy > last_kill[sp]) last_kill[sp] = (int16_t)fy_doy;
                }
                uint16_t g2 = (uint16_t)(byte & 63);
                if (g2) {
                    gdd2_spring[sp * 182 + (size_t)fy_doy] += g2;
                }
            }
        }
    }
}

/* Finalize 30 frost years into the 6 output int16 bands for n_land pixels. */
void frost_finalize(
    const int16_t *winter_doy,  /* (n_land,) */
    const int16_t *yr_spring,   /* (30, n_land), -1 = none */
    const int16_t *yr_autumn,   /* (30, n_land), 365 = none */
    const int16_t *yr_ff,       /* (30, n_land) */
    const uint8_t *yr_frost,    /* (30, n_land) */
    const uint8_t *yr_fs,       /* (30, n_land) */
    int n_land,
    int16_t *out6               /* (6, n_land) */
) {
    int16_t sbuf[30], abuf[30], fbuf[30];
    for (int p = 0; p < n_land; p++) {
        int ns = 0, na = 0, frost_cnt = 0, fs_cnt = 0;
        for (int y = 0; y < 30; y++) {
            size_t idx = (size_t)y * (size_t)n_land + (size_t)p;
            int16_t sv = yr_spring[idx];
            if (sv >= 0) sbuf[ns++] = sv;
            int16_t av = yr_autumn[idx];
            if (av < 365) abuf[na++] = av;
            fbuf[y] = yr_ff[idx];
            frost_cnt += yr_frost[idx];
            fs_cnt += yr_fs[idx];
        }
        qsort(sbuf, (size_t)ns, sizeof(int16_t), cmp_i16);
        qsort(abuf, (size_t)na, sizeof(int16_t), cmp_i16);
        qsort(fbuf, 30, sizeof(int16_t), cmp_i16);

        int ff_med = (int)lroundf(pct_sorted(fbuf, 30, 50.0f));
        int frost_pct = (int)lroundf((float)frost_cnt * 100.0f / 30.0f);
        int fs_pct = (int)lroundf((float)fs_cnt * 100.0f / 30.0f);
        int year_round = (ff_med == 0);
        int wd = (int)winter_doy[p];

        int16_t b0, b1, b2;
        if (year_round) {
            b0 = b1 = b2 = 366;
        } else if (frost_pct == 0) {
            b0 = b1 = b2 = 0;
        } else {
            if (ns > 0) {
                int s50 = (int)lroundf(pct_sorted(sbuf, ns, 50.0f));
                int s90 = (int)lroundf(pct_sorted(sbuf, ns, 90.0f));
                b0 = (int16_t)(((s50 + wd - 1) % 365) + 1);
                b1 = (int16_t)(((s90 + wd - 1) % 365) + 1);
            } else {
                b0 = b1 = 0;
            }
            if (na > 0) {
                int a50 = (int)lroundf(pct_sorted(abuf, na, 50.0f));
                b2 = (int16_t)(((a50 + wd - 1) % 365) + 1);
            } else {
                b2 = 0;
            }
        }
        out6[0 * (size_t)n_land + (size_t)p] = b0;
        out6[1 * (size_t)n_land + (size_t)p] = b1;
        out6[2 * (size_t)n_land + (size_t)p] = b2;
        out6[3 * (size_t)n_land + (size_t)p] = (int16_t)(frost_pct == 0 ? 365 : ff_med);
        out6[4 * (size_t)n_land + (size_t)p] = (int16_t)frost_pct;
        out6[5 * (size_t)n_land + (size_t)p] = (int16_t)((year_round || frost_pct == 0) ? 0 : fs_pct);
    }
}
"""


def _compile_frost_so(cache_dir: Path) -> str:
    import shutil
    import subprocess

    cc = shutil.which("gcc") or shutil.which("cc")
    if not cc:
        raise RuntimeError("gcc/cc required for local frost calendar computation")
    c_path = cache_dir / ".frost_kernel.c"
    so_path = cache_dir / ".frost_kernel.so"
    c_path.write_text(_FROST_C_SRC)
    subprocess.run([cc, "-O3", "-shared", "-fPIC", str(c_path), "-lm", "-o", str(so_path)], check=True)
    return str(so_path)


def _compute_frost_strip(task: tuple[int, str, str]) -> tuple[int, object]:
    import ctypes
    import numpy as np
    import rasterio

    s, daily_dir_s, so_path = task
    daily_dir = Path(daily_dir_s)
    _, _, h, _ = FROST_STRIPS[s]
    n_px = h * 3600

    with rasterio.open(daily_dir / f"winter_doy_s{s}.tif") as src:
        wd_full = src.read(1).reshape(-1)
    land_idx = np.flatnonzero(wd_full != NODATA).astype(np.int32)
    n_land = int(land_idx.size)
    wd = np.ascontiguousarray(wd_full[land_idx], dtype=np.int16)

    lib = ctypes.CDLL(so_path)
    u32p = ctypes.POINTER(ctypes.c_uint32)
    i32p = ctypes.POINTER(ctypes.c_int32)
    i16p = ctypes.POINTER(ctypes.c_int16)
    u8p = ctypes.POINTER(ctypes.c_uint8)
    u16p = ctypes.POINTER(ctypes.c_uint16)
    lib.frost_update_month.argtypes = [
        u32p, i32p, i16p,
        ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int,
        i16p, i16p, i16p, u8p, u16p,
    ]
    lib.frost_finalize.argtypes = [i16p, i16p, i16p, i16p, u8p, u8p, ctypes.c_int, i16p]

    last_spring = np.full((2, n_land), -1, dtype=np.int16)
    first_autumn = np.full((2, n_land), 365, dtype=np.int16)
    last_kill = np.full((2, n_land), -1, dtype=np.int16)
    has_frost = np.zeros((2, n_land), dtype=np.uint8)
    gdd2_spring = np.zeros((2, n_land, 182), dtype=np.uint16)

    yr_spring = np.full((30, n_land), -1, dtype=np.int16)
    yr_autumn = np.full((30, n_land), 365, dtype=np.int16)
    yr_ff = np.zeros((30, n_land), dtype=np.int16)
    yr_frost = np.zeros((30, n_land), dtype=np.uint8)
    yr_fs = np.zeros((30, n_land), dtype=np.uint8)

    for cy in range(BASE[0], BASE[1] + 2):
        has_slot0 = 1 if BASE[0] <= cy - 1 <= BASE[1] else 0
        has_slot1 = 1 if BASE[0] <= cy <= BASE[1] else 0
        doy_start = 1
        for m in range(1, 13):
            n_days = calendar.monthrange(cy, m)[1]
            with rasterio.open(daily_dir / f"s{s}_{cy}_{m:02d}.tif") as src:
                packed = np.ascontiguousarray(src.read(), dtype=np.uint32)
            lib.frost_update_month(
                packed.ctypes.data_as(u32p),
                land_idx.ctypes.data_as(i32p),
                wd.ctypes.data_as(i16p),
                n_px, n_land, n_days, doy_start, has_slot0, has_slot1,
                last_spring.ctypes.data_as(i16p),
                first_autumn.ctypes.data_as(i16p),
                last_kill.ctypes.data_as(i16p),
                has_frost.ctypes.data_as(u8p),
                gdd2_spring.ctypes.data_as(u16p),
            )
            doy_start += n_days

        # At the end of calendar year cy, frost year (cy - 1) in slot 0 is complete.
        if has_slot0:
            yi = (cy - 1) - BASE[0]
            ls = last_spring[0]
            fa = first_autumn[0]
            yr_spring[yi] = ls
            yr_autumn[yi] = fa
            ff_start = np.where(ls >= 0, ls + 1, 0)
            yr_ff[yi] = np.clip(fa - ff_start, 0, 365).astype(np.int16)
            yr_frost[yi] = has_frost[0]
            # False spring: cumulative GDD (in 0.5 C units) up to last_kill >= 300 (150.0 GDD).
            lk = last_kill[0]
            had_kill = lk >= 0
            if np.any(had_kill):
                # Vectorized cumulative sum only for pixels with a spring killing freeze.
                kp = np.flatnonzero(had_kill)
                g_sub = gdd2_spring[0, kp]  # (K, 182)
                lk_sub = lk[kp]
                # Mask columns <= lk_sub and sum
                cols = np.arange(182, dtype=np.int16)[None, :]
                tot = np.where(cols <= lk_sub[:, None], g_sub, 0).sum(axis=1)
                yr_fs[yi, kp] = (tot >= 300).astype(np.uint8)

        # Shift slot 1 -> slot 0 and reset slot 1 for next calendar year.
        last_spring[0] = last_spring[1]
        first_autumn[0] = first_autumn[1]
        last_kill[0] = last_kill[1]
        has_frost[0] = has_frost[1]
        gdd2_spring[0] = gdd2_spring[1]
        last_spring[1] = -1
        first_autumn[1] = 365
        last_kill[1] = -1
        has_frost[1] = 0
        gdd2_spring[1] = 0

    out6 = np.empty((6, n_land), dtype=np.int16)
    lib.frost_finalize(
        wd.ctypes.data_as(i16p),
        yr_spring.ctypes.data_as(i16p),
        yr_autumn.ctypes.data_as(i16p),
        yr_ff.ctypes.data_as(i16p),
        yr_frost.ctypes.data_as(u8p),
        yr_fs.ctypes.data_as(u8p),
        n_land,
        out6.ctypes.data_as(i16p),
    )

    strip_out = np.full((6, n_px), NODATA, dtype=np.int16)
    strip_out[:, land_idx] = out6
    return s, strip_out.reshape(6, h, 3600)


def cmd_combine(args) -> None:
    """Convert Drive GeoTIFFs (and/or local frost_daily strips) to pipeline inputs in <out>."""
    import concurrent.futures
    import time
    import numpy as np
    import rasterio
    from rasterio.transform import Affine

    out = Path(args.out)
    src = out / "drive"

    def convert(name, scale, bands, dtype_out):
        p = src / f"{name}.tif"
        if not p.exists():
            return False
        with rasterio.open(p) as r:
            a = r.read()
            profile = r.profile
        if dtype_out == "float32":
            arr = a.astype("float32")
            arr[a == NODATA] = np.nan
            arr = arr * scale
            profile = {**profile, "dtype": "float32", "nodata": float("nan"), "compress": "deflate", "tiled": False}
        else:
            arr = a.astype("int16")
            profile = {**profile, "dtype": "int16", "nodata": NODATA, "compress": "deflate", "tiled": False}
        profile.pop("blockxsize", None)
        profile.pop("blockysize", None)
        dst = out / f"{name}.tif"
        with rasterio.open(dst, "w", **profile) as w:
            w.write(arr)
            for i, b in enumerate(bands):
                w.set_band_description(i + 1, b)
            w.update_tags(source=ERA5_LAND, period=f"{BASE[0]}-{BASE[1]}")
        print(f"{dst} ({dst.stat().st_size // 1024} KB)")
        return True

    convert(TAIL_NAME, 0.1, TAIL_BANDS, "float32")
    if not convert(FROST_NAME, 1, FROST_BANDS, "int16"):
        daily_dir = out / "frost_daily"
        all_wd = [daily_dir / f"winter_doy_s{s}.tif" for s, _, _, _ in FROST_STRIPS]
        all_daily = [daily_dir / f"s{s}_{y}_{m:02d}.tif" for s, y, m in _frost_fetch_jobs()]
        if all(p.exists() for p in all_wd) and all(p.exists() for p in all_daily):
            t0 = time.time()
            so_path = _compile_frost_so(daily_dir)
            tasks = [(s, str(daily_dir), so_path) for s, _, _, _ in FROST_STRIPS]
            strips = [None] * len(FROST_STRIPS)
            print(f"computing exact 0.1-deg frost calendar across {len(tasks)} strips...", flush=True)
            with concurrent.futures.ProcessPoolExecutor(max_workers=len(tasks)) as pool:
                for s, arr in pool.map(_compute_frost_strip, tasks):
                    strips[s] = arr
            full = np.concatenate(strips, axis=1)  # (6, 1801, 3600)
            dst = out / f"{FROST_NAME}.tif"
            prof = {
                "driver": "GTiff",
                "crs": "EPSG:4326",
                "transform": Affine(0.1, 0.0, -180.05, 0.0, -0.1, 90.05),
                "width": 3600,
                "height": 1801,
                "count": len(FROST_BANDS),
                "dtype": "int16",
                "nodata": NODATA,
                "compress": "deflate",
                "tiled": False,
            }
            with rasterio.open(dst, "w", **prof) as w:
                w.write(full)
                for i, b in enumerate(FROST_BANDS):
                    w.set_band_description(i + 1, b)
                w.update_tags(source=ERA5_LAND, period=f"{BASE[0]}-{BASE[1]}")
            print(f"{dst} ({dst.stat().st_size // 1024} KB) in {time.time() - t0:.1f}s")
        else:
            print(f"skipping {FROST_NAME}: neither drive/{FROST_NAME}.tif nor complete frost_daily/ present")


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--project", required=True, help="Google Cloud project registered for Earth Engine")
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("check")
    exp = sub.add_parser("export")
    exp.add_argument("--only", help="comma-separated subset: tail,frost")
    sub.add_parser("status")
    for name in ["download", "combine"]:
        sub.add_parser(name).add_argument("--out", required=True)
    ff = sub.add_parser("frost-fetch")
    ff.add_argument("--out", required=True)
    ff.add_argument("--workers", type=int, default=16)
    args = p.parse_args()
    if args.cmd != "combine":
        ee.Initialize(project=args.project)
    {
        "check": cmd_check,
        "export": cmd_export,
        "status": cmd_status,
        "download": cmd_download,
        "frost-fetch": cmd_frost_fetch,
        "combine": cmd_combine,
    }[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())
