//! Validation fixtures (see `docs/validation.md`).
//!
//! - `era5_classes_are_reproducible`: the Köppen class and summaries derived
//!   from the committed ERA5 normals match `validation-expected.json`.
//! - `grid_matches_expected` (ignored; needs `CANOPI_CLIMATE_GRID` and the
//!   `pipeline` feature): lookups in a built grid equal the committed ones,
//!   and the tolerance misses are exactly the documented ones.

#[cfg(test)]
mod validation {
    use serde_json::Value;

    const NORMALS: &str = include_str!("fixtures/era5-normals.json");
    const EXPECTED: &str = include_str!("fixtures/validation-expected.json");

    fn cities(doc: &str) -> Vec<Value> {
        let v: Value = serde_json::from_str(doc).unwrap();
        v["cities"].as_array().unwrap().clone()
    }

    fn arr(v: &Value) -> [f32; 12] {
        let a = v.as_array().unwrap();
        std::array::from_fn(|m| a[m].as_f64().unwrap() as f32)
    }

    #[test]
    fn era5_classes_are_reproducible() {
        let normals = cities(NORMALS);
        let expected = cities(EXPECTED);
        assert_eq!(normals.len(), expected.len());
        assert!(normals.len() >= 20);
        for (n, e) in normals.iter().zip(&expected) {
            assert_eq!(n["slug"], e["slug"]);
            let (tmin, tmax, p) = (arr(&n["tmin"]), arr(&n["tmax"]), arr(&n["precip"]));
            let t: [f32; 12] = std::array::from_fn(|m| f32::midpoint(tmin[m], tmax[m]));
            let lat = n["lat"].as_f64().unwrap();
            let s = canopi_climate::derived::MonthlySummary::new(&t, &p, lat).unwrap();
            let era = &e["era5"];
            let code = s.koppen_of_means.unwrap();
            assert_eq!(
                canopi_climate::koppen::symbol(code),
                era["koppen"].as_str().unwrap(),
                "{}",
                n["slug"]
            );
            assert!((f64::from(s.tmean_cold) - era["tmean_cold_c"].as_f64().unwrap()).abs() < 0.02);
            assert!((f64::from(s.precip) - era["precip_mm"].as_f64().unwrap()).abs() < 0.5);
            assert_eq!(u64::from(s.dry_months), era["dry_months"].as_u64().unwrap());
        }
    }

    /// Covers every Köppen main group (A, B, C, D, E) in ERA5's own classes.
    #[test]
    fn cities_cover_every_main_group() {
        let groups: std::collections::BTreeSet<char> = cities(EXPECTED)
            .iter()
            .filter_map(|e| e["era5"]["koppen"].as_str().and_then(|s| s.chars().next()))
            .collect();
        assert_eq!(groups.into_iter().collect::<String>(), "ABCDE");
    }

    #[cfg(feature = "pipeline")]
    #[test]
    #[ignore = "needs a built grid in CANOPI_CLIMATE_GRID"]
    fn grid_matches_expected() {
        let dir = std::env::var("CANOPI_CLIMATE_GRID")
            .expect("set CANOPI_CLIMATE_GRID to a grid directory");
        let mut grid =
            canopi_climate::pipeline::sample::Grid::open(std::path::Path::new(&dir)).unwrap();
        let doc: Value = serde_json::from_str(EXPECTED).unwrap();
        let (tol_t, tol_p) = (
            doc["tolerance_temperature_c"].as_f64().unwrap(),
            doc["tolerance_precip_fraction"].as_f64().unwrap(),
        );
        for e in doc["cities"].as_array().unwrap() {
            let lk = &e["lookup"];
            let found = grid
                .lookup(lk["lat"].as_f64().unwrap(), lk["lon"].as_f64().unwrap(), 2)
                .unwrap()
                .unwrap();
            assert_eq!(&serde_json::to_value(&found).unwrap(), lk, "{}", e["slug"]);
            let g = &found.periods[0];
            let era = &e["era5"];
            let t = |id: &str, v: f64| {
                ((v - era[format!("{id}_c")].as_f64().unwrap()).abs() > tol_t)
                    .then(|| id.to_string())
            };
            let mut misses: Vec<String> = [
                t("tmin_cold", g.tmin_cold_c),
                t("tmax_warm", g.tmax_warm_c),
                t("tmean_cold", g.tmean_cold_c),
                t("tmean_warm", g.tmean_warm_c),
            ]
            .into_iter()
            .flatten()
            .collect();
            let p = era["precip_mm"].as_f64().unwrap();
            if ((g.precip_mm - p) / p).abs() > tol_p {
                misses.push("precip".into());
            }
            let documented: Vec<String> = e["misses"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m.as_str().unwrap().to_string())
                .collect();
            assert_eq!(misses, documented, "{}", e["slug"]);
        }
    }
}
