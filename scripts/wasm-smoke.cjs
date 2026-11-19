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
assert.equal(cc.textureClass(40, 20), "loam");
assert.equal(cc.textureClass(70, 40), undefined);

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
  };
  got.extremes = got.climate ? cc.lookupExtremes(extBytes, got.climate.cell_lat, got.climate.cell_lon) : null;
  assert.deepStrictEqual(got.extremes, p.expect.extremes, `${p.name}: extremes`);
  assert.deepStrictEqual(got.climate, p.expect.climate, `${p.name}: climate`);
  assert.deepStrictEqual(got.soil, p.expect.soil, `${p.name}: soil`);
  if (own) assert.deepStrictEqual(got.regions, p.expect.regions, `${p.name}: regions`);
  else assert.deepStrictEqual([p.expect.regions.wgsrpd_l3, p.expect.regions.iso_a2], [null, null], `${p.name}: regions`);
  compared++;
}
assert.ok(compared >= 30, `${compared} vector points compared`);

console.log(`wasm smoke ok: ${compared} vector points`, cc.version(), JSON.stringify(r.periods[0]), JSON.stringify(soil), JSON.stringify(fr));
