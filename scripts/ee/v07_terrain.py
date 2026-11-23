"""Terrain layer (elevation, relief, slope, HAND) for canopi-climate v0.7.0,
computed on Google Earth Engine.

Exports a single static 0.1-degree GeoTIFF directly on canopi-climate's global 0.1-degree grid
(`3600x1800`, EPSG:4326, `crsTransform = [0.1, 0, -180.0, 0, -0.1, 90.0]`) to the Google Drive
folder `canopi-climate-ee`, then `download` fetches it and `combine` writes the build input
(`ee/terrain_glo30_merit.tif`):

1. `elev_m` (int16, scale 1.0, m, NODATA = -32768):
   Cell mean elevation from `COPERNICUS/DEM/GLO30` (`DEM` band, ~30 m, 90S-90N).
   Never masks `DEM <= 0` so below-sea-level land (Netherlands, Dead Sea, Death Valley,
   Caspian basin) is preserved; ocean masking happens in the Rust pipeline against canopi's
   complete-climate land mask.
2. `relief_m` (int16, scale 1.0, m, NODATA = -32768):
   Within-cell topographic relief (`p95 - p05` of GLO-30 `DEM`), clamped to >= 0.
3. `slope_deg` (int16, scale 0.1, tenths of degrees, NODATA = -32768):
   Within-cell median slope in degrees (`ee.Terrain.slope` on GLO-30 at native 30 m
   default projection, then reduced to 0.1 degree).
4. `hand_m` (int16, scale 0.1, tenths of meters, clamped to [0, 3000] m, NODATA = -32768):
   Within-cell median Height Above Nearest Drainage (`hnd`) from `MERIT/Hydro/v1_0_1`
   (~90 m, 60S-90N, elected under ODbL 1.0). Remains NODATA outside MERIT-Hydro coverage
   (Antarctica, sub-Antarctic islands, unmapped small islands).

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

# Exact canopi-climate 0.1-degree global grid: 3600x1800, cell centres at -179.95..179.95, 89.95..-89.95.
GRID_CANOPI_0P1 = dict(
    crs="EPSG:4326",
    crsTransform=[0.1, 0, -180.0, 0, -0.1, 90.0],
    dimensions="3600x1800",
)


def terrain_image() -> ee.Image:
    """Build 4-band float terrain image (elev_m, relief_m, slope_deg, hand_m) on 0.1-deg grid."""
    # GLO-30 is an ImageCollection with tile-varying projections; compute slope per native tile
    # (preserving 30 m terrain gradients and GEE pyramids), then mosaic and set a 250 m default
    # projection (~45x45 = ~2,000 samples per 0.1-deg cell) so reduceResolution stays well inside
    # Earth Engine's tile memory budget globally. Never mask DEM <= 0.
    glo30 = ee.ImageCollection(GLO30).select("DEM")
    dem = glo30.mosaic().setDefaultProjection("EPSG:4326", None, 250)
    slope = (
        glo30.map(lambda img: ee.Terrain.slope(img))
        .mosaic()
        .setDefaultProjection("EPSG:4326", None, 250)
        .rename("slope_deg")
    )

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

    return ee.Image.cat([elev_m, relief_m, slope_deg, hand_m])


def terrain_export_image() -> ee.Image:
    """Quantize terrain bands to int16 with NODATA = -32768 for GeoTIFF export."""
    img = terrain_image()
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
    """Spot-check terrain_image() on 5 diverse cities including below-sea-level and high relief."""
    img = terrain_image()
    cities = [
        ("Paris", 48.85, 2.35),
        ("Amsterdam (below sea level)", 52.35, 4.95),
        ("Dead Sea basin", 31.55, 35.45),
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
    """Launch the single global 0.1-degree terrain export to Google Drive."""
    img = terrain_export_image()
    task = ee.batch.Export.image.toDrive(
        image=img,
        description=TASK_NAME,
        folder=DRIVE_FOLDER,
        fileNamePrefix=TASK_NAME,
        fileFormat="GeoTIFF",
        formatOptions={"cloudOptimized": False, "noData": NODATA},
        maxPixels=1e9,
        **GRID_CANOPI_0P1,
    )
    task.start()
    print(f"STARTED  {task.id}  {TASK_NAME}")


def cmd_status(_args):
    """Show status of v0.7.0 terrain GEE export tasks."""
    tasks = ee.batch.Task.list()
    found = False
    for t in tasks:
        desc = t.config.get("description", "")
        if desc == TASK_NAME:
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
    """Download terrain_glo30_merit.tif from Google Drive folder canopi-climate-ee."""
    from googleapiclient.http import MediaIoBaseDownload

    out_dir = Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)
    svc = _drive_service()
    fq = f"name = '{DRIVE_FOLDER}' and mimeType = 'application/vnd.google-apps.folder' and trashed = false"
    folders = svc.files().list(q=fq, fields="files(id)").execute().get("files", [])
    files = [
        f
        for fo in folders
        for f in svc.files()
        .list(
            q=f"'{fo['id']}' in parents and name = '{TASK_NAME}.tif' and trashed = false",
            fields="files(id, name, size, modifiedTime)",
            orderBy="modifiedTime desc",
        )
        .execute()
        .get("files", [])
    ]
    if not files:
        sys.exit(f"File {TASK_NAME}.tif not found in Google Drive folder '{DRIVE_FOLDER}'.")
    f = files[0]
    dst = out_dir / f"{TASK_NAME}.raw.tif"
    req = svc.files().get_media(fileId=f["id"])
    with open(dst, "wb") as fh:
        dl = MediaIoBaseDownload(fh, req, chunksize=8 * 1024 * 1024)
        done = False
        while not done:
            _, done = dl.next_chunk()
    print(f"Downloaded {dst} ({dst.stat().st_size} bytes)")


def cmd_combine(args):
    """Validate and copy/repack the downloaded terrain raster to ee/terrain_glo30_merit.tif."""
    import hashlib
    import numpy as np
    import rasterio

    out_dir = Path(args.out)
    src_path = out_dir / f"{TASK_NAME}.raw.tif"
    dst_path = out_dir / f"{TASK_NAME}.tif"
    if not src_path.exists():
        sys.exit(f"Missing {src_path}; run download first.")

    with rasterio.open(src_path) as src:
        assert src.width == 3600 and src.height == 1800, f"unexpected shape {src.width}x{src.height}"
        assert src.count == 4, f"unexpected band count {src.count}"
        data = src.read()
        profile = src.profile.copy()

    profile.update(
        driver="GTiff",
        dtype="int16",
        count=4,
        nodata=NODATA,
        compress="deflate",
        tiled=True,
        blockxsize=256,
        blockysize=256,
    )
    with rasterio.open(dst_path, "w", **profile) as dst:
        for i, name in enumerate(BANDS, start=1):
            dst.write(data[i - 1].astype(np.int16), i)
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
