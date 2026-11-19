"""Mean annual temperature extremes for canopi-climate, computed on Google Earth Engine.

The grid stores the coldest night and the hottest day of a typical year (the 30-year mean of each
year's lowest daily minimum, TNn, and highest daily maximum, TXx) as an offset from the variables
it already has (docs/decisions/0005-annual-extremes.md):

    tmin_extreme[slot] = tmin_cold[slot] + gap_min + dgap_min[slot]
    tmax_extreme[slot] = tmax_warm[slot] + gap_max + dgap_max[slot]

- gap_min, gap_max (this script's `baseline`): ERA5-Land daily aggregates 1991-2020,
  TNn - (coldest month's mean daily minimum) and TXx - (warmest month's mean daily maximum).
- dgap_min, dgap_max (`future`, `combine`): pattern scaling from NASA NEX-GDDP-CMIP6. Earth
  Engine's copy has only SSP2-4.5 and SSP5-8.5, so for each model the change of each gap from
  1991-2020 to 2041-2070 and 2071-2099 under both is regressed (through the origin) on the change
  of the coldest-month (warmest-month) mean temperature: k_min, k_max in °C per °C. The median over
  16 models (MODELS, one per family) is taken locally (`combine`); the build multiplies it by each slot's own warming in Beck et al.:
  dgap_min[slot] = k_min * (tmean_cold[slot] - tmean_cold[1991-2020]), the same for max.
Only the extra change of the extremes comes from NEX-GDDP; the mean warming stays Beck et al.'s.
Nothing is downloaded but the finished maps: the computations run as Earth Engine batch tasks that
write GeoTIFFs to the Google Drive folder `canopi-climate-ee` (int16, -32768 = no data; gaps in
tenths of a degree, k in thousandths), then `download` fetches them (about 20 MB) and `combine`
writes the build's two inputs (float32: °C and °C per °C).

    python scripts/ee/extremes.py --project <id> check
    python scripts/ee/extremes.py --project <id> point 47.2894 3.8186
    python scripts/ee/extremes.py --project <id> baseline
    python scripts/ee/extremes.py --project <id> future [--models A,B,...]
    python scripts/ee/extremes.py --project <id> status
    python scripts/ee/extremes.py --project <id> download --out <dir>   # into <dir>/drive
    python scripts/ee/extremes.py --project <id> combine --out <dir>    # <dir>/era5land_baseline.tif, nexgddp_k.tif

Requires earthengine-api, google-api-python-client, numpy and rasterio, and a one-time
`earthengine authenticate` (Earth Engine and Drive scopes).
"""

import argparse
import json
import sys
import io
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import ee

ERA5_LAND = "ECMWF/ERA5_LAND/DAILY_AGGR"
NEX = "NASA/GDDP-CMIP6"
SCENARIOS = ["ssp245", "ssp585"]  # the only two in Earth Engine's NEX-GDDP-CMIP6
FUTURES = [(p, s) for p in [(2041, 2070), (2071, 2099)] for s in SCENARIOS]
BASE = (1991, 2020)
# NEX-GDDP's historical runs end in 2014; 2015-2020 come from SSP2-4.5 (scenarios barely differ yet).
HIST_END, HIST_FILL = 2014, "ssp245"
NODATA = -32768

# The climate grid (src/grid.rs): 0.1-degree cells, west and north edges inclusive.
# ERA5-Land's own grid: cell centres on multiples of 0.1 degree, half a cell off the canopi grid's
# (the build interpolates bilinearly to its centres; resampling on Earth Engine fails at the poles).
# 3600 columns: ERA5-Land's 3601st repeats the first (180 E = 180 W).
GRID_E5L = dict(crs="EPSG:4326", crsTransform=[0.1, 0, -180.05, 0, -0.1, 90.05], dimensions="3600x1801")
# The scaling factors are computed on a 1-degree grid, 90 N to 60 S (360 x 150): NEX-GDDP's change
# comes from GCMs at about 1 degree or coarser, so its 0.25-degree detail adds nothing to k, and a
# 1-degree export reads Earth Engine's averaged pyramid levels (about 16 times less work). The region
# is a planar rectangle.
GRID_K = dict(crs="EPSG:4326", crsTransform=[1, 0, -180, 0, -1, 90])
REGION_K = [-180, -60, 180, 90]


DRIVE_FOLDER = "canopi-climate-ee"
BASELINE_BANDS = ["gap_min", "gap_max", "tnn", "txx", "tmin_cold", "tmax_warm"]


def gaps(daily: ee.ImageCollection, tmin: str, tmax: str, y0: int, y1: int, tmean: str | None = None) -> ee.Image:
    """TNn - coldest-month mean minimum and TXx - warmest-month mean maximum over [y0, y1] (K = °C);
    with `tmean`, also the coldest- and warmest-month mean temperatures (`tc`, `tw`)."""
    c = daily.filterDate(f"{y0}-01-01", f"{y1 + 1}-01-01")

    def annual(y):
        # A year without data (a model's missing band) is left out: List.map drops nulls.
        year = c.filter(ee.Filter.calendarRange(y, y, "year"))
        return ee.Algorithms.If(year.size().gt(0), year.select(tmin).min().addBands(year.select(tmax).max()), None)

    def monthly(m):
        return c.filter(ee.Filter.calendarRange(m, m, "month")).select([tmin, tmax] + ([tmean] if tmean else [])).mean()

    ext = ee.ImageCollection(ee.List.sequence(y0, y1).map(annual)).mean()
    mon = ee.ImageCollection(ee.List.sequence(1, 12).map(monthly))
    cold, warm = mon.select(tmin).min(), mon.select(tmax).max()
    if tmean:
        return (
            ext.select(tmin).subtract(cold).rename("gap_min")
            .addBands(ext.select(tmax).subtract(warm).rename("gap_max"))
            .addBands(mon.select(tmean).min().rename("tc"))
            .addBands(mon.select(tmean).max().rename("tw"))
        )
    return (
        ext.select(tmin).subtract(cold).rename("gap_min")
        .addBands(ext.select(tmax).subtract(warm).rename("gap_max"))
        .addBands(ext.select(tmin).subtract(273.15).rename("tnn"))
        .addBands(ext.select(tmax).subtract(273.15).rename("txx"))
        .addBands(cold.subtract(273.15).rename("tmin_cold"))
        .addBands(warm.subtract(273.15).rename("tmax_warm"))
    )


def baseline_image() -> ee.Image:
    daily = ee.ImageCollection(ERA5_LAND).select(["temperature_2m_min", "temperature_2m_max"])
    return gaps(daily, "temperature_2m_min", "temperature_2m_max", *BASE)


def tenths(img: ee.Image) -> ee.Image:
    """°C as int16 tenths, masked cells as NODATA."""
    return img.multiply(10).round().toInt16().unmask(NODATA, False)


def nex(model: str, scenario: str) -> ee.ImageCollection:
    return (
        ee.ImageCollection(NEX)
        .filter(ee.Filter.eq("model", model))
        .filter(ee.Filter.eq("scenario", scenario))
        # A few days lack a temperature band (NorESM2-LM ssp585 2096: tas; TaiESM1 ssp585 2093:
        # tasmin): leave those days out.
        .filter(ee.Filter.listContains("system:band_names", "tas"))
        .filter(ee.Filter.listContains("system:band_names", "tasmin"))
        .filter(ee.Filter.listContains("system:band_names", "tasmax"))
        .select(["tasmin", "tasmax", "tas"])
        .map(unbound)
    )


def unbound(img: ee.Image) -> ee.Image:
    """Drop a NEX-GDDP image's footprint (not copied back), keeping its mask (kelvin: valid > 0).

    Exports of composites of the bounded daily images fail with "Unable to transform edge" at
    internal tile corners; the same composite of unbounded images exports.
    """
    u = img.unmask(0, False)
    return u.updateMask(u.gt(0)).copyProperties(img, ["system:time_start", "system:time_end"])


def model_image(model: str) -> ee.Image:
    """k_min, k_max (°C of gap change per °C of coldest/warmest-month warming) for one model, and
    the four samples they are fitted on (dmin_*, dmax_*, dtc_*, dtw_*)."""
    hist = nex(model, "historical").filterDate(f"{BASE[0]}-01-01", f"{HIST_END + 1}-01-01").merge(
        nex(model, HIST_FILL).filterDate(f"{HIST_END + 1}-01-01", f"{BASE[1] + 1}-01-01")
    )
    base = gaps(hist, "tasmin", "tasmax", *BASE, tmean="tas")
    samples, bands = [], []
    for (y0, y1), s in FUTURES:
        d = gaps(nex(model, s), "tasmin", "tasmax", y0, y1, tmean="tas").subtract(base)
        samples.append(d)
        key = f"{y0}_{y1}_{s}"
        bands += [d.select(b).rename(f"d{b.replace('gap_', '')}_{key}") for b in ["gap_min", "gap_max", "tc", "tw"]]
    fit = lambda gap, t: (
        ee.Image.cat([d.select(gap).multiply(d.select(t)) for d in samples]).reduce(ee.Reducer.sum())
        .divide(ee.Image.cat([d.select(t).pow(2) for d in samples]).reduce(ee.Reducer.sum()))
    )
    k = fit("gap_min", "tc").rename("k_min").addBands(fit("gap_max", "tw").rename("k_max"))
    return k.addBands(ee.Image.cat(bands)).toFloat().set("model", model)


# The models whose k enter the median: one per model family (modelling centre or shared code),
# among those with all three temperatures in every scenario. Families share biases, so a median
# over all 30 would over-weight them, and the median's sampling error at 16 is a few tenths of a
# degree at most, below the method's known errors.
MODELS = [
    "ACCESS-CM2", "BCC-CSM2-MR", "CanESM5", "CMCC-ESM2", "CNRM-CM6-1", "EC-Earth3", "FGOALS-g3",
    "GFDL-ESM4", "GISS-E2-1-G", "INM-CM5-0", "IPSL-CM6A-LR", "MIROC6", "MPI-ESM1-2-HR",
    "MRI-ESM2-0", "NorESM2-MM", "UKESM1-0-LL",
]


def models_with_all_scenarios() -> list[str]:
    """Models with historical, SSP2-4.5 and SSP5-8.5 runs that carry tas, tasmin and tasmax (CESM2,
    CESM2-WACCM and IITM-ESM lack a temperature band in Earth Engine's copy)."""
    need = ee.List(["tas", "tasmin", "tasmax"])

    def models(s, d):
        day = ee.ImageCollection(NEX).filter(ee.Filter.eq("scenario", s)).filterDate(d, ee.Date(d).advance(1, "day"))
        ok = day.map(lambda i: i.set("ok", i.bandNames().containsAll(need))).filter(ee.Filter.eq("ok", True))
        return set(ok.aggregate_array("model").getInfo())

    sets = [models("historical", "2000-01-01")] + [models(s, "2050-01-01") for s in SCENARIOS]
    return sorted(set.intersection(*sets))


def cmd_check(args) -> None:
    print("ERA5-Land:", ee.ImageCollection(ERA5_LAND).first().bandNames().getInfo()[:4], "...")
    sample = ee.ImageCollection(NEX).filterDate("2050-01-01", "2050-01-02").first()
    print("NEX-GDDP sample properties:", sample.propertyNames().getInfo())
    models = models_with_all_scenarios()
    print(f"{len(models)} models with historical, SSP2-4.5 and SSP5-8.5:", ", ".join(models))


def cmd_point(args) -> None:
    """The baseline at one point, from the daily series (an image reduction exceeds the interactive
    memory limit)."""
    pt = ee.Geometry.Point([args.lon, args.lat])
    bands = ["temperature_2m_min", "temperature_2m_max"]

    def year(y):
        c = ee.ImageCollection(ERA5_LAND).filterDate(f"{y}-01-01", f"{y + 1}-01-01").select(bands)
        return c.getRegion(pt, 11132).getInfo()[1:]

    with ThreadPoolExecutor(10) as ex:
        rows = [r for part in ex.map(year, range(BASE[0], BASE[1] + 1)) for r in part]
    lows, highs, mon = {}, {}, {m: ([], []) for m in range(1, 13)}
    for r in rows:
        y, m, tn, tx = int(r[0][:4]), int(r[0][4:6]), r[4] - 273.15, r[5] - 273.15
        lows[y], highs[y] = min(lows.get(y, 99), tn), max(highs.get(y, -99), tx)
        mon[m][0].append(tn)
        mon[m][1].append(tx)
    mean = lambda v: sum(v) / len(v)
    tnn, txx = mean(list(lows.values())), mean(list(highs.values()))
    cold, warm = min(mean(v[0]) for v in mon.values()), max(mean(v[1]) for v in mon.values())
    print(json.dumps({"days": len(rows), "tnn": round(tnn, 1), "txx": round(txx, 1), "tmin_cold": round(cold, 1),
                      "tmax_warm": round(warm, 1), "gap_min": round(tnn - cold, 1), "gap_max": round(txx - warm, 1)}))


def export(img: ee.Image, name: str, grid: dict, region: list | None = None) -> None:
    if region:
        grid = {**grid, "region": ee.Geometry.Rectangle(region, "EPSG:4326", False)}
    task = ee.batch.Export.image.toDrive(
        image=img, description=name, folder=DRIVE_FOLDER, fileNamePrefix=name, maxPixels=1e10, **grid)
    task.start()
    print(f"started {name}: {task.id}")


def cmd_baseline(args) -> None:
    export(tenths(baseline_image().select(BASELINE_BANDS)), "era5land_baseline", GRID_E5L)


def cmd_future(args) -> None:
    models = args.models.split(",") if args.models else MODELS
    missing = set(models) - set(models_with_all_scenarios())
    if missing:
        raise SystemExit(f"no complete runs for {sorted(missing)}")
    for m in models:
        k = model_image(m).select(["k_min", "k_max"])
        # k is a ratio (°C per °C): int16 thousandths.
        export(k.multiply(1000).round().toInt16().unmask(NODATA, False), f"nexgddp_k_{m}", GRID_K, REGION_K)


def cmd_status(args) -> None:
    for t in ee.data.getTaskList()[:100]:
        if t.get("description", "").startswith(("era5land_", "nexgddp_")):
            print(f"{t['state']:10} {t['description']:32} {t.get('error_message', '')}")


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
    files = [f for fo in folders for f in d.files().list(
        q=f"'{fo['id']}' in parents and trashed = false", fields="files(id,name,size)", pageSize=200).execute()["files"]]
    for f in sorted(files, key=lambda f: f["name"]):
        path = out / f["name"]
        if path.exists() and path.stat().st_size == int(f["size"]):
            continue
        buf = io.BytesIO()
        dl = MediaIoBaseDownload(buf, d.files().get_media(fileId=f["id"]), chunksize=1 << 24)
        done = False
        while not done:
            _, done = dl.next_chunk()
        path.write_bytes(buf.getvalue())
        print(f"{path} ({path.stat().st_size // 1024} KB)")


def cmd_combine(args) -> None:
    """From the downloads in <out>/drive, write the build's two inputs to <out> (float32, deflate,
    NaN = no data): era5land_baseline.tif (°C) and nexgddp_k.tif (the median over the models)."""
    import numpy as np
    import rasterio

    out = Path(args.out)
    src = out / "drive"

    def read(p, scale):
        with rasterio.open(p) as r:
            a = r.read().astype("float32")
            a[a == NODATA] = np.nan
            return a * scale, r.profile, r.descriptions

    def write(path, a, profile, names, **tags):
        profile = {**profile, "dtype": "float32", "nodata": float("nan"), "compress": "deflate", "tiled": False}
        profile.pop("blockxsize", None), profile.pop("blockysize", None)
        with rasterio.open(path, "w", **profile) as w:
            w.write(a)
            for i, n in enumerate(names):
                w.set_band_description(i + 1, n)
            w.update_tags(**tags)
        print(f"{path} ({path.stat().st_size // 1024} KB)")

    base, profile, names = read(src / "era5land_baseline.tif", 0.1)
    write(out / "era5land_baseline.tif", base, profile, names, source=ERA5_LAND, period=f"{BASE[0]}-{BASE[1]}")
    paths = [src / f"nexgddp_k_{m}.tif" for m in MODELS]
    if absent := [p.name for p in paths if not p.exists()]:
        raise SystemExit(f"missing {absent}")
    # The export region's far edges are inclusive (361 x 151): keep the 360 x 150 cells of GRID_K.
    w, h = 360, 150
    stack = [read(p, 0.001)[0][:, :h, :w] for p in paths]
    _, profile, names = read(paths[0], 0.001)
    profile = {**profile, "width": w, "height": h}
    models = [p.stem.removeprefix("nexgddp_k_") for p in paths]
    med = np.nanmedian(np.stack(stack), axis=0).astype("float32")
    write(out / "nexgddp_k.tif", med, profile, names, source=NEX, models=",".join(models))
    print(f"median of {len(models)} models")


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--project", required=True, help="Google Cloud project registered for Earth Engine")
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("check")
    pt = sub.add_parser("point")
    pt.add_argument("lat", type=float)
    pt.add_argument("lon", type=float)
    sub.add_parser("baseline")
    f = sub.add_parser("future")
    f.add_argument("--models", help="comma-separated model names (default: every model with both SSPs)")
    sub.add_parser("status")
    for name in ["download", "combine"]:
        sub.add_parser(name).add_argument("--out", required=True)
    args = p.parse_args()
    ee.Initialize(project=args.project)
    globals()[f"cmd_{args.cmd}"](args)


if __name__ == "__main__":
    sys.exit(main())
