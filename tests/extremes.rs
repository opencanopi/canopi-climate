//! The `extremes` layer against a built grid (ignored; needs
//! `CANOPI_CLIMATE_GRID` and the `pipeline` feature):
//!
//! ```text
//! CANOPI_CLIMATE_GRID=out-v0.3 cargo test --release --features pipeline --test extremes -- --ignored
//! ```
//!
//! Ranges, not exact values: they hold for any sound build, and catch a
//! wrong band, scale or offset (docs/decisions/0005-annual-extremes.md).

#[cfg(test)]
#[cfg(feature = "pipeline")]
mod extremes {
    use std::path::PathBuf;

    use canopi_climate::pipeline::sample::Grid;

    fn grid() -> Grid {
        let dir = std::env::var_os("CANOPI_CLIMATE_GRID").map(PathBuf::from);
        let dir = dir.expect("set CANOPI_CLIMATE_GRID to a built grid directory");
        Grid::open(&dir).unwrap()
    }

    /// A range of °C, inclusive.
    type Range = (f64, f64);

    /// (name, lat, lon, 1991-2020 coldest night range, hottest day range), °C.
    const POINTS: [(&str, f64, f64, Range, Range); 6] = [
        // ERA5-Land -8.3 and 33.1; stations nearby -10 to -12 and 33.
        ("Lormes", 47.2894, 3.8186, (-11.0, -6.0), (31.0, 35.0)),
        ("Moscow", 55.75, 37.62, (-32.0, -20.0), (29.0, 35.0)),
        // Inherits the grid's low tmax_warm here (29.2 against about 32 at
        // Changi): about 31, where stations record about 34.
        ("Singapore", 1.35, 103.82, (20.0, 25.0), (29.5, 36.0)),
        ("Phoenix", 33.45, -112.07, (-4.0, 6.0), (43.0, 48.0)),
        ("Montpellier", 43.61, 3.88, (-9.0, -2.0), (33.0, 39.0)),
        ("Nairobi", -1.29, 36.82, (5.0, 13.0), (26.0, 32.0)),
    ];

    #[test]
    #[ignore = "needs CANOPI_CLIMATE_GRID"]
    fn extremes_at_known_points() {
        let mut g = grid();
        for (name, lat, lon, (n0, n1), (x0, x1)) in POINTS {
            let site = g.site(lat, lon, 2, false).unwrap();
            let (c, e) = (site.climate.unwrap(), site.extremes.expect(name));
            assert_eq!((c.cell_lat, c.cell_lon), (e.cell_lat, e.cell_lon), "{name}");
            let (tn, tx) = (e.periods[0].tmin_extreme_c, e.periods[0].tmax_extreme_c);
            assert!((n0..=n1).contains(&tn), "{name}: coldest night {tn}");
            assert!((x0..=x1).contains(&tx), "{name}: hottest day {tx}");
            for (p, q) in c.periods.iter().zip(&e.periods) {
                assert!(q.tmin_extreme_c <= p.tmin_cold_c, "{name} {}", p.scenario);
                assert!(q.tmax_extreme_c >= p.tmax_warm_c, "{name} {}", p.scenario);
            }
            // Warming raises both by 2071-2099 under SSP5-8.5.
            assert!(
                e.periods[8].tmin_extreme_c > tn && e.periods[8].tmax_extreme_c > tx,
                "{name}"
            );
        }
    }

    #[test]
    #[ignore = "needs CANOPI_CLIMATE_GRID"]
    fn usda_zone_of_lormes() {
        let mut g = grid();
        let e = g.site(47.2894, 3.8186, 2, false).unwrap().extremes.unwrap();
        assert!(
            ["8a", "8b"].contains(&e.periods[0].usda_zone.as_str()),
            "{}",
            e.periods[0].usda_zone
        );
    }
}
