"""Terrain layer (elevation, relief, slope, HAND) for canopi-climate v0.7.0,
computed on Google Earth Engine.

Exports 6 x 30-degree latitude bands (`3600x300` each) on canopi-climate's global 0.1-degree grid
(`3600x1800`, EPSG:4326, `crsTransform = [0.1, 0, -180.0, 0, -0.1, 90.0]`) to the Google Drive
folder `canopi-climate-ee` with `shardSize=32` (3.2x3.2 deg worker shards, 64x smaller than GEE's
default 256x256 shard), then `download` fetches all 6 bands and `combine` stacks them into the
single build input (`ee/terrain_glo30_merit.tif`):

1. `elev_m` (int16, scale 1.0, m, NODATA = -32768):
   Cell mean elevation from `COPERNICUS/DEM/GLO30_2024_1` (`DEM` band, 250 m pyramid, 90S-90N).
   Never masks `DEM <= 0` so below-sea-level land (Netherlands, Dead Sea, Death Valley,
   Caspian basin) is preserved; ocean masking happens in the Rust pipeline against canopi's
   complete-climate land mask.
2. `relief_m` (int16, scale 1.0, m, NODATA = -32768):
   Within-cell topographic relief (`p95 - p05` of GLO-30 `DEM`), clamped to >= 0.
3. `slope_deg` (int16, scale 0.1, tenths of degrees, NODATA = -32768):
   Within-cell median slope in degrees (`ee.Terrain.slope` on the 250 m GLO-30 mosaic,
   reduced to 0.1 degree).
4. `hand_m` (int16, scale 0.1, tenths of meters, clamped to [0, 3000] m, NODATA = -32768):
   Within-cell median Height Above Nearest Drainage (`hnd`) from `MERIT/Hydro/v1_0_1`
   (~90 m, 60S-90N, elected under ODbL 1.0). Remains NODATA outside MERIT-Hydro coverage
   (Antarctica 60S-90S, sub-Antarctic islands, unmapped small islands).

Usage:
    python scripts/ee/v07_terrain.py --project canopi-515022 check
    python scripts/ee/v07_terrain.py --project canopi-515022 export
    python scripts/ee/v07_terrain.py --project canopi-515022 status
    python scripts/ee/v07_terrain.py --project canopi-515022 download --out <dir>
    python scripts/ee/v07_terrain.py --project canopi-515022 combine --out <dir>
"""

import argparse
import io
import json
import sys
from pathlib import Path

import ee

GLO30 = "COPERNICUS/DEM/GLO30_2024_1"
MERIT_HYDRO = "MERIT/Hydro/v1_0_1"
NODATA = -32768
DRIVE_FOLDER = "canopi-climate-ee"
TASK_NAME = "terrain_glo30_merit"
BANDS = ["elev_m", "relief_m", "slope_deg", "hand_m"]
SHARD_SIZE = 32  # 3.2x3.2 deg worker shards (64x lower RAM than default shardSize=256)

# Six 30-degree latitude bands stacking north->south to the exact 3600x1800 0.1-deg global grid.
# Each entry is (suffix, lat_north, include_merit_hydro).
LAT_BANDS = [
    ("n90_n60", 90.0, True),
    ("n60_n30", 60.0, True),
    ("n30_000", 30.0, True),
    ("000_s30", 0.0, True),
    ("s30_s60", -30.0, True),
    ("s60_s90", -60.0, False),  # MERIT-Hydro ends at 60S
]


def band_task_name(suffix: str) -> str:
    return f"{TASK_NAME}_{suffix}"


def all_task_names() -> list[str]:
    return [band_task_name(s) for s, _, _ in LAT_BANDS]


def terrain_image(include_merit: bool = True) -> ee.Image:
    """Build 4-band float terrain image (elev_m, relief_m, slope_deg, hand_m) on 0.1-deg grid."""
    # Read GLO-30 once at 250 m default projection (~45x45 = ~2,000 samples per 0.1-deg cell)
    # and compute both DEM stats and slope from that single mosaic so GEE does not read the
    # 26k-tile ImageCollection twice. Never mask DEM <= 0.
    glo30 = ee.ImageCollection(GLO30).select("DEM")
    dem = glo30.mosaic().setDefaultProjection("EPSG:4326", None, 250)
    slope = ee.Terrain.slope(dem).rename("slope_deg")

    # Mean elevation and p5/p95 relief from GLO-30.
    dem_stats = dem.reduceResolution(
        reducer=ee.Reducer.mean().combine(
            ee.Reducer.percentile([5, 95]), sharedInputs=True
        ),
        bestEffort=True,
        maxPixels=4096,
    )

    elev_m = dem_stats.select("DEM_mean").rename("elev_m")
    relief_m = (
        dem_stats.select("DEM_p95")
        .subtract(dem_stats.select("DEM_p5"))
        .max(0.0)
        .rename("relief_m")
    )

    slope_deg = (
        slope.reduceResolution(
            reducer=ee.Reducer.median(),
            bestEffort=True,
            maxPixels=4096,
        )
        .clamp(0.0, 90.0)
        .rename("slope_deg")
    )

    # MERIT-Hydro HAND (90 m, 60S-90N, ODbL 1.0). Keep unmasked only where valid (no 0-fill).
    if include_merit:
        merit_hnd = ee.Image(MERIT_HYDRO).select("hnd")
        hand_m = (
            merit_hnd.reduceResolution(
                reducer=ee.Reducer.median(),
                bestEffort=True,
                maxPixels=4096,
            )
            .clamp(0.0, 3000.0)
            .rename("hand_m")
        )
    else:
        hand_m = ee.Image.constant(0.0).updateMask(ee.Image.constant(0)).rename("hand_m")

    return ee.Image.cat([elev_m, relief_m, slope_deg, hand_m])


def terrain_export_image(include_merit: bool = True) -> ee.Image:
    """Quantize terrain bands to int16 with NODATA = -32768 for GeoTIFF export."""
    img = terrain_image(include_merit=include_merit)
    elev = img.select("elev_m").round().clamp(-32767, 32767).unmask(NODATA).toInt16()
    relief = img.select("relief_m").round().clamp(0, 32767).unmask(NODATA).toInt16()
    slope = (
        img.select("slope_deg").multiply(10.0).round().clamp(0, 900).unmask(NODATA).toInt16()
    )
    hand = (
        img.select("hand_m").multiply(10.0).round().clamp(0, 30000).unmask(NODATA).toInt16()
    )
    return ee.Image.cat([elev, relief, slope, hand]).rename(BANDS)


def cmd_check(_args):
    """Spot-check terrain_image() on 6 diverse points including below-sea-level and high relief."""
    img = terrain_image()
    cities = [
        ("Paris", 48.85, 2.35),
        ("Amsterdam (below sea level)", 52.35, 4.95),
        ("Dead Sea basin", 31.55, 35.45),
        ("Death Valley (below sea level)", 36.25, -116.85),
        ("Quito (Andes)", -0.15, -78.45),
        ("Chamonix (Alps)", 45.95, 6.85),
    ]
    out = []
    for name, lat, lon in cities:
        vals = img.reduceRegion(
            reducer=ee.Reducer.first(),
            geometry=ee.Geometry.Point([lon, lat]),
            crs="EPSG:4326",
            crsTransform=[0.1, 0, -180.0, 0, -0.1, 90.0],
        ).getInfo()
        out.append({"city": name, "lat": lat, "lon": lon, **vals})
    print(json.dumps(out, indent=2))


def cmd_export(_args):
    """Launch 6 x 30-degree latitude-band 0.1-degree terrain exports to Google Drive."""
    for suffix, lat_north, include_merit in LAT_BANDS:
        name = band_task_name(suffix)
        img = terrain_export_image(include_merit=include_merit)
        task = ee.batch.Export.image.toDrive(
            image=img,
            description=name,
            folder=DRIVE_FOLDER,
            fileNamePrefix=name,
            fileFormat="GeoTIFF",
            formatOptions={"cloudOptimized": False, "noData": NODATA},
            crs="EPSG:4326",
            crsTransform=[0.1, 0, -180.0, 0, -0.1, lat_north],
            dimensions="3600x300",
            shardSize=SHARD_SIZE,
            maxPixels=1e9,
        )
        task.start()
        print(f"STARTED  {task.id}  {name}")


def cmd_status(_args):
    """Show status of v0.7.0 terrain GEE export tasks."""
    want = set(all_task_names()) | {TASK_NAME}
    tasks = ee.batch.Task.list()
    found = False
    for t in tasks:
        desc = t.config.get("description", "")
        if desc in want:
            found = True
            err = f"  {t.status().get('error_message', '')}" if t.state == "FAILED" else ""
            print(f"{t.state:10s} {t.id:26s} {desc}{err}")
    if not found:
        print(f"No GEE tasks found for {TASK_NAME}.")


def _drive_service():
    from googleapiclient.discovery import build

    return build(
        "drive",
        "v3",
        credentials=ee.data.get_persistent_credentials(),
        cache_discovery=False,
    )


def cmd_download(args):
    """Download all 6 terrain latitude-band GeoTIFFs from Google Drive folder canopi-climate-ee."""
    from googleapiclient.http import MediaIoBaseDownload

    out_dir = Path(args.out) / "drive"
    out_dir.mkdir(parents=True, exist_ok=True)
    svc = _drive_service()
    fq = f"name = '{DRIVE_FOLDER}' and mimeType = 'application/vnd.google-apps.folder' and trashed = false"
    folders = svc.files().list(q=fq, fields="files(id)").execute().get("files", [])
    want = {f"{n}.tif" for n in all_task_names()}
    by_name: dict[str, dict] = {}
    for fo in folders:
        files = (
            svc.files()
            .list(
                q=f"'{fo['id']}' in parents and trashed = false",
                fields="files(id, name, size, modifiedTime)",
                orderBy="modifiedTime desc",
                pageSize=200,
            )
            .execute()
            .get("files", [])
        )
        for f in files:
            if f["name"] in want and f["name"] not in by_name:
                by_name[f["name"]] = f

    missing = sorted(want - set(by_name))
    if missing:
        sys.exit(f"Missing files in Google Drive folder '{DRIVE_FOLDER}': {missing}")

    for name in sorted(by_name):
        f = by_name[name]
        dst = out_dir / name
        if dst.exists() and dst.stat().st_size == int(f["size"]):
            print(f"Cached     {dst} ({dst.stat().st_size} bytes)")
            continue
        req = svc.files().get_media(fileId=f["id"])
        with open(dst, "wb") as fh:
            dl = MediaIoBaseDownload(fh, req, chunksize=8 * 1024 * 1024)
            done = False
            while not done:
                _, done = dl.next_chunk()
        print(f"Downloaded {dst} ({dst.stat().st_size} bytes)")


def cmd_combine(args):
    """Stack the 6 latitude-band GeoTIFFs into the global 3600x1800 ee/terrain_glo30_merit.tif."""
    import hashlib
    import numpy as np
    import rasterio
    from rasterio.transform import Affine

    out_dir = Path(args.out)
    raw_dir = out_dir / "drive"
    dst_path = out_dir / f"{TASK_NAME}.tif"

    strips = []
    profile = None
    for suffix, _, _ in LAT_BANDS:
        src_path = raw_dir / f"{band_task_name(suffix)}.tif"
        if not src_path.exists():
            sys.exit(f"Missing {src_path}; run download first.")
        with rasterio.open(src_path) as src:
            assert src.width == 3600 and src.height == 300, (
                f"{src_path.name}: unexpected shape {src.width}x{src.height}"
            )
            assert src.count == 4, f"{src_path.name}: unexpected band count {src.count}"
            strips.append(src.read().astype(np.int16))
            if profile is None:
                profile = src.profile.copy()

    data = np.concatenate(strips, axis=1)
    assert data.shape == (4, 1800, 3600), f"unexpected combined shape {data.shape}"

    profile.update(
        driver="GTiff",
        dtype="int16",
        width=3600,
        height=1800,
        count=4,
        crs="EPSG:4326",
        transform=Affine(0.1, 0.0, -180.0, 0.0, -0.1, 90.0),
        nodata=NODATA,
        compress="deflate",
        tiled=True,
        blockxsize=256,
        blockysize=256,
    )
    with rasterio.open(dst_path, "w", **profile) as dst:
        for i, name in enumerate(BANDS, start=1):
            dst.write(data[i - 1], i)
            dst.set_band_description(i, name)

    h = hashlib.sha256(dst_path.read_bytes()).hexdigest()
    print(f"Wrote {dst_path} ({dst_path.stat().st_size} bytes) sha256={h}")


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--project", required=True, help="Google Cloud / Earth Engine project ID")
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("check")
    sub.add_parser("export")
    sub.add_parser("status")
    dl = sub.add_parser("download")
    dl.add_argument("--out", required=True)
    cb = sub.add_parser("combine")
    cb.add_argument("--out", required=True)

    args = p.parse_args()
    if args.cmd in ("check", "export", "status"):
        ee.Initialize(project=args.project)
    {"check": cmd_check, "export": cmd_export, "status": cmd_status, "download": cmd_download, "combine": cmd_combine}[args.cmd](args)


if __name__ == "__main__":
    main()
