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
console.log("wasm smoke ok", cc.version(), JSON.stringify(r.periods[0]));
