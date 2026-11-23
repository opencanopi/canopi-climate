// Smoke test of the Node build: `node scripts/wasm-smoke.cjs` after
// `wasm-bindgen --target nodejs --out-dir pkg-node target/wasm32-unknown-unknown/wasm/canopi_climate.wasm`.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const cc = require(path.join(__dirname, "..", "pkg-node", "canopi_climate.js"));

assert.equal(cc.tileName(45.95, 5.05), "n50e000.tif");
assert.equal(cc.tileName(-90, 180), "s80w180.tif");
assert.equal(cc.tileName(91, 0), undefined);

const bytes = fs.readFileSync(path.join(__dirname, "..", "tests", "fixtures", "synthetic-n50e000.tif"));
const r = cc.lookup(bytes, 45.95, 5.05);
assert.equal(r.tile, "n50e000");
assert.equal(r.periods.length, 9);
assert.equal(r.periods[0].period, "1991-2020");
assert.equal(typeof r.periods[0].tmin_cold_c, "number");
// Decimal values come out as short decimals, not float noise.
for (const p of r.periods) {
  for (const k of ["tmin_cold_c", "tmax_warm_c", "tmean_cold_c", "tmean_warm_c", "aridity_index"]) {
    assert.ok(String(p[k]).length <= 8, `${k}=${p[k]}`);
  }
}
assert.equal(cc.lookup(bytes, 49.5, 9.5), null); // ocean, too far from land
assert.throws(() => cc.lookup(bytes, 10, 10)); // wrong tile
assert.throws(() => cc.lookup(new Uint8Array([1, 2, 3]), 45, 5));

// Site layers: synthetic soil and regions tiles of n50e000.
const fixture = (name) => fs.readFileSync(path.join(__dirname, "..", "tests", "fixtures", name));
const soilBytes = fixture("synthetic-soil-n50e000.tif");
const soil = cc.lookupSoil(soilBytes, 45.25, 3.25);
assert.equal(soil.tile, "n50e000");
assert.equal(soil.ph_h2o, 6);
assert.equal(soil.sand_pct, 57.5);
assert.equal(soil.clay_pct, 24.5);
assert.equal(soil.silt_pct, 18);
assert.equal(soil.texture, "sandy_clay_loam");
assert.equal(soil.filled, true);
assert.equal(soil.fill_distance_cells, 2);
assert.equal(soil.depth, "0-30 cm");
assert.equal(cc.lookupSoil(soilBytes, 45.0, 1.0).filled, false);
assert.equal(cc.lookupSoil(soilBytes, 48.0, 1.0), null); // sea
assert.throws(() => cc.lookupSoil(soilBytes, 10, 10)); // wrong tile
const regionsBytes = fixture("synthetic-regions-n50e000.tif");
assert.throws(() => cc.lookupSoil(regionsBytes, 45, 1)); // not a soil tile
const fr = cc.lookupRegions(regionsBytes, 45.0, 1.0);
assert.equal(fr.wgsrpd_l3, "FRA");
assert.equal(fr.iso_a2, "FR");
assert.equal(cc.lookupRegions(regionsBytes, 45.25, 3.25).iso_a2, "CH");
const coast = cc.lookupRegions(regionsBytes, 46.05, 1.0); // one cell off the coast
assert.equal(coast.iso_a2, "FR");
assert.equal(coast.iso_a2_distance_cells, 1);
const sea = cc.lookupRegions(regionsBytes, 48.0, 1.0);
assert.equal(sea.wgsrpd_l3, null);
assert.equal(sea.iso_a2, null);
const extBytes = fixture("synthetic-extremes-n50e000.tif");
const ext = cc.lookupExtremes(extBytes, r.cell_lat, r.cell_lon);
assert.equal(ext.tile, "n50e000");
assert.equal(ext.periods.length, 9);
assert.equal(typeof ext.periods[0].tmin_extreme_c, "number");
assert.ok(ext.periods[0].tmin_extreme_c < r.periods[0].tmin_cold_c);
assert.equal(ext.periods[0].usda_zone, cc.usdaZone(ext.periods[0].tmin_extreme_c));
assert.equal(cc.usdaZone(-8.4), "8b");
assert.throws(() => cc.lookupExtremes(extBytes, 10, 10)); // wrong tile
assert.throws(() => cc.lookupExtremes(bytes, r.cell_lat, r.cell_lon)); // a climate tile
const monBytes = fixture("synthetic-monthly-n50e000.tif");
const mon = cc.lookupMonthly(monBytes, r.cell_lat, r.cell_lon);
assert.equal(mon.tile, "n50e000");
assert.equal(mon.periods.length, 3);
assert.equal(mon.periods[0].tmean_c.length, 12);
assert.equal(mon.periods[0].tmin_c.length, 12);
assert.equal(typeof mon.periods[0].gdd_base_10, "number");
assert.equal(typeof mon.periods[0].chill_months, "number");
assert.throws(() => cc.lookupMonthly(monBytes, 10, 10)); // wrong tile
assert.throws(() => cc.lookupMonthly(bytes, r.cell_lat, r.cell_lon)); // climate tile
const monPBytes = fixture("synthetic-monthly-precip-n50e000.tif");
const monP = cc.lookupMonthlyPrecip(monPBytes, 45.95, 5.05);
assert.equal(monP.tile, "n50e000");
assert.equal(monP.distance_cells, 0);
assert.equal(monP.periods.length, 3);
assert.equal(monP.periods[0].precip_mm.length, 12);
const monPCoast = cc.lookupMonthlyPrecip(monPBytes, 47.125, 5.125);
assert.equal(monPCoast.distance_cells, 1);
assert.equal(cc.lookupMonthlyPrecip(monPBytes, 49.5, 9.5), null); // far ocean
assert.throws(() => cc.lookupMonthlyPrecip(monPBytes, 10, 10)); // wrong tile
const extTailBytes = fixture("synthetic-extremes-tail-n50e000.tif");
const extTail = cc.lookupExtremesTail(extTailBytes, r.cell_lat, r.cell_lon);
assert.equal(extTail.tile, "n50e000");
assert.equal(extTail.tmin_p10_gap_c, -3.5);
assert.equal(extTail.tmax_p90_gap_c, 2.8);
assert.equal(extTail.heat_days_35c, 4.2);
assert.equal(extTail.recent_trend_c, 0.9);
assert.throws(() => cc.lookupExtremesTail(extTailBytes, 10, 10)); // wrong tile
const frostBytes = fixture("synthetic-frost-calendar-n50e000.tif");
const frost = cc.lookupFrostCalendar(frostBytes, r.cell_lat, r.cell_lon);
assert.equal(frost.tile, "n50e000");
assert.equal(frost.regime, "seasonal");
assert.equal(frost.last_spring_frost_doy, 105);
assert.equal(frost.frost_free_days, 200);
assert.equal(frost.false_spring_pct, 23);
const frostFree = cc.lookupFrostCalendar(frostBytes, 45.95, 5.15);
assert.equal(frostFree.regime, "frost_free");
assert.equal(frostFree.last_spring_frost_doy, null);
assert.equal(frostFree.frost_free_days, 365);
const frostYr = cc.lookupFrostCalendar(frostBytes, 45.95, 5.25);
assert.equal(frostYr.regime, "year_round");
assert.equal(frostYr.last_spring_frost_doy, null);
assert.equal(frostYr.frost_free_days, 0);
assert.throws(() => cc.lookupFrostCalendar(frostBytes, 10, 10)); // wrong tile
assert.equal(cc.textureClass(40, 20), "loam");
assert.equal(cc.textureClass(70, 40), undefined);

// Synthetic analogues.bin k-NN check (build a tiny valid CNPA buffer in JS to verify WASM findAnalogues).
{
  const hdr = Buffer.alloc(120 + 2 * 26);
  hdr.write("CNPA", 0, "ascii");
  hdr.writeUInt16LE(1, 4);
  hdr.writeUInt16LE(120, 6);
  hdr.writeUInt32LE(2, 8);
  hdr.writeUInt8(8, 12);
  const mean = [5, 20, 0, 26, 6.5, 1, 2, 0.5];
  const invStd = [0.1, 0.125, 0.1, 0.125, 1, 1.5, 0.25, 4];
  const weight = [1.4, 1.4, 1, 1, 1.2, 0.8, 1, 1.2];
  for (let i = 0; i < 8; i++) {
    hdr.writeFloatLE(mean[i], 16 + i * 4);
    hdr.writeFloatLE(invStd[i], 48 + i * 4);
    hdr.writeFloatLE(weight[i], 80 + i * 4);
  }
  hdr.writeFloatLE(1.0, 112);
  hdr.writeFloatLE(2.0, 116);
  const writeRec = (off, lat, lon, iso, wgs, koppen, flags, feats) => {
    hdr.writeInt16LE(Math.round(lat * 100), off);
    hdr.writeInt16LE(Math.round(lon * 100), off + 2);
    hdr.writeUInt16LE(iso, off + 4);
    hdr.writeUInt16LE(wgs, off + 6);
    hdr.writeUInt8(koppen, off + 8);
    hdr.writeUInt8(flags, off + 9);
    for (let i = 0; i < 8; i++) hdr.writeInt16LE(feats[i], off + 10 + i * 2);
  };
  // Record 0: Toulouse-like (FR = 5*26+17 = 147, FRA = 5*676+17*26+0 = 3822)
  writeRec(120, 43.6, 1.44, 147, 3822, 14, 1, [60, 225, 25, 285, 6486, 90, 1, 480]);
  // Record 1: Svalbard-like (aridity invalid -> flags=0)
  writeRec(146, 78.22, 15.63, 0xffff, 0xffff, 29, 0, [-140, 50, -173, 60, 5900, 0, 0, 500]);
  const res = cc.findAnalogues(
    new Uint8Array(hdr),
    {
      lat: 48.86, lon: 2.35, tmean_cold_c: 6.0, tmean_warm_c: 22.5,
      tmin_cold_c: 2.5, tmax_warm_c: 28.5, precip_mm: 655, aridity_index: 0.9,
      dry_months: 1, summer_rain_frac: 0.48,
    },
    { k: 2, min_distance_km: 100, exclude_radius_km: 0 },
  );
  assert.equal(res.novel_climate, false);
  assert.equal(res.matches[0].iso_a2, "FR");
  assert.equal(res.matches[0].match_quality, "close");
}

// v0.6.0: phenology, water_balance, photoperiod, and Saxton-Rawls AWC.
const phenoBytes = fixture("synthetic-phenology-n50e000.tif");
const pheno = cc.lookupPhenology(phenoBytes, 45.95, 5.05);
assert.equal(pheno.tile, "n50e000");
assert.equal(pheno.chill_portions_0c, 96);
assert.equal(pheno.chill_portions_4c, 46);
assert.throws(() => cc.lookupPhenology(phenoBytes, 10, 10));

const wbBytes = fixture("synthetic-water-balance-n50e000.tif");
const wb = cc.lookupWaterBalance(wbBytes, 45.95, 5.05);
assert.equal(wb.tile, "n50e000");
assert.equal(wb.cwd_mm, 340);
assert.equal(wb.cwd_p90_mm, 465);
assert.equal(wb.cdd_max_days, 22);
assert.equal(wb.vpd_summer_kpa, 1.05);
assert.throws(() => cc.lookupWaterBalance(wbBytes, 10, 10));

const ppEq = cc.photoperiod(0);
assert.equal(ppEq.min_hours, 12);
assert.equal(ppEq.max_hours, 12);
const ppPole = cc.photoperiod(90);
assert.equal(ppPole.min_hours, 0);
assert.equal(ppPole.max_hours, 24);
assert.equal(cc.photoperiod(91), null);

const awcLoam = cc.soilAwc(40, 20, undefined);
assert.ok(awcLoam.awc_30cm_mm >= 35 && awcLoam.awc_30cm_mm <= 60);
const awcSand = cc.soilAwc(100, 0, 0);
assert.ok(awcSand.wilting_point >= 0.01);
assert.equal(cc.soilAwc(70, 40, undefined), null);

// The synthetic vectors (tests/fixtures/synthetic-expected.json, written by the crate's own
// lookups): every point a single-tile reader can answer gives exactly the native answer. The
// host fetches the point's own tile only (none outside n50e000 here).
const vectors = JSON.parse(fs.readFileSync(path.join(__dirname, "..", "tests", "fixtures", "synthetic-expected.json"), "utf8"));
let compared = 0;
for (const p of vectors.points) {
  if (p.expect === "invalid") {
    assert.equal(cc.tileName(p.lat, p.lon), undefined, p.name);
    assert.throws(() => cc.lookup(bytes, p.lat, p.lon), p.name);
    continue;
  }
  if (!p.own_tile) continue; // needs a neighbouring tile: the cross-tile readers only
  const own = cc.tileName(p.lat, p.lon) === "n50e000.tif";
  const got = {
    climate: own ? cc.lookup(bytes, p.lat, p.lon) : null,
    soil: own ? cc.lookupSoil(soilBytes, p.lat, p.lon) : null,
    regions: own ? cc.lookupRegions(regionsBytes, p.lat, p.lon) : null,
    monthly_precip: own ? cc.lookupMonthlyPrecip(monPBytes, p.lat, p.lon) : null,
  };
  got.extremes = got.climate ? cc.lookupExtremes(extBytes, got.climate.cell_lat, got.climate.cell_lon) : null;
  got.extremes_tail = got.climate ? cc.lookupExtremesTail(extTailBytes, got.climate.cell_lat, got.climate.cell_lon) : null;
  got.frost_calendar = got.climate ? cc.lookupFrostCalendar(frostBytes, got.climate.cell_lat, got.climate.cell_lon) : null;
  got.monthly = got.climate ? cc.lookupMonthly(monBytes, got.climate.cell_lat, got.climate.cell_lon) : null;
  got.phenology = got.climate ? cc.lookupPhenology(phenoBytes, got.climate.cell_lat, got.climate.cell_lon) : null;
  got.water_balance = got.climate ? cc.lookupWaterBalance(wbBytes, got.climate.cell_lat, got.climate.cell_lon) : null;
  assert.deepStrictEqual(got.extremes, p.expect.extremes, `${p.name}: extremes`);
  assert.deepStrictEqual(got.extremes_tail, p.expect.extremes_tail, `${p.name}: extremes_tail`);
  assert.deepStrictEqual(got.frost_calendar, p.expect.frost_calendar, `${p.name}: frost_calendar`);
  assert.deepStrictEqual(got.monthly, p.expect.monthly, `${p.name}: monthly`);
  assert.deepStrictEqual(got.monthly_precip, p.expect.monthly_precip, `${p.name}: monthly_precip`);
  assert.deepStrictEqual(got.phenology, p.expect.phenology, `${p.name}: phenology`);
  assert.deepStrictEqual(got.water_balance, p.expect.water_balance, `${p.name}: water_balance`);
  assert.deepStrictEqual(got.climate, p.expect.climate, `${p.name}: climate`);
  assert.deepStrictEqual(got.soil, p.expect.soil, `${p.name}: soil`);
  if (own) assert.deepStrictEqual(got.regions, p.expect.regions, `${p.name}: regions`);
  else assert.deepStrictEqual([p.expect.regions.wgsrpd_l3, p.expect.regions.iso_a2], [null, null], `${p.name}: regions`);
  compared++;
}
assert.ok(compared >= 30, `${compared} vector points compared`);

console.log(`wasm smoke ok: ${compared} vector points`, cc.version(), JSON.stringify(r.periods[0]), JSON.stringify(soil), JSON.stringify(fr));
