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
    python scripts/ee/v05_layers.py --project canopi-515022 combine --out <dir>
"""

import argparse
import io
import json
import sys
from pathlib import Path

import ee

ERA5_LAND = "ECMWF/ERA5_LAND/DAILY_AGGR"
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

        ff_start = last_spring_fy.unmask(0)
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


def cmd_combine(args) -> None:
    """Convert Drive int16 GeoTIFFs to pipeline inputs in <out>."""
    import numpy as np
    import rasterio

    out = Path(args.out)
    src = out / "drive"

    def convert(name, scale, bands, dtype_out):
        p = src / f"{name}.tif"
        if not p.exists():
            raise SystemExit(f"missing {p}")
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

    convert(TAIL_NAME, 0.1, TAIL_BANDS, "float32")
    convert(FROST_NAME, 1, FROST_BANDS, "int16")


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
    args = p.parse_args()
    ee.Initialize(project=args.project)
    globals()[f"cmd_{args.cmd}"](args)


if __name__ == "__main__":
    sys.exit(main())
