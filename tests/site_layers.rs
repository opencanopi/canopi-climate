//! Site layers against a built grid (ignored; needs `CANOPI_CLIMATE_GRID`
//! and the `pipeline` feature):
//!
//! ```text
//! CANOPI_CLIMATE_GRID=out-v0.3 cargo test --release --features pipeline --test site_layers -- --ignored
//! ```
//!
//! Soil values are SoilGrids' own: the expected numbers are the
//! depth-weighted 0-30 cm means of `gdallocationinfo -wgs84` (GDAL 3.8.4,
//! PROJ 9) on the nine source files at the layer cell centre, rounded like
//! the tiles (`docs/validation.md`, "Site layers").

#[cfg(test)]
#[cfg(feature = "pipeline")]
mod site_layers {
    use std::path::PathBuf;

    use canopi_climate::layer::LAYER_LOOKUP_RADIUS;
    use canopi_climate::pipeline::sample::Grid;

    fn grid() -> Grid {
        let dir = std::env::var_os("CANOPI_CLIMATE_GRID").map(PathBuf::from);
        let dir = dir.expect("set CANOPI_CLIMATE_GRID to a built grid directory");
        Grid::open(&dir).unwrap()
    }

    /// (name, lat, lon, pH, sand %, clay %, fill distance).
    const SOIL: [(&str, f64, f64, f64, f64, f64, u8); 7] = [
        // Rural points: measured, equal to GDAL at the cell centre.
        ("Beauce", 48.30, 1.75, 7.0, 7.5, 30.5, 0),
        ("Herault", 43.65, 3.55, 7.9, 32.4, 29.2, 0),
        ("Kenya highlands", -0.5, 37.0, 6.0, 24.8, 43.7, 0),
        ("Sao Paulo state", -22.0, -48.0, 5.3, 37.4, 45.6, 0),
        // Cities: SoilGrids has no value; filled from nearby cells.
        ("Paris", 48.8566, 2.3522, 6.0, 28.3, 25.2, 3),
        ("Montpellier", 43.6108, 3.8767, 7.7, 31.3, 25.7, 1),
        ("Sao Paulo", -23.55, -46.63, 5.3, 48.3, 32.3, 2),
    ];

    #[test]
    #[ignore = "needs CANOPI_CLIMATE_GRID"]
    fn soil_at_known_points() {
        let mut g = grid();
        for (name, lat, lon, ph, sand, clay, fill) in SOIL {
            let s = g.soil(lat, lon, LAYER_LOOKUP_RADIUS).unwrap().unwrap();
            assert_eq!(
                (s.ph_h2o, s.sand_pct, s.clay_pct, s.fill_distance_cells),
                (ph, sand, clay, fill),
                "{name}"
            );
            assert_eq!(s.filled, fill > 0, "{name}");
            assert_eq!(s.distance_cells, 0.0, "{name}");
            assert!(s.texture.is_some(), "{name}");
        }
    }

    #[test]
    #[ignore = "needs CANOPI_CLIMATE_GRID"]
    fn no_soil_on_ice_sheets_and_at_sea() {
        let mut g = grid();
        for (lat, lon) in [(72.0, -40.0), (-80.0, 0.0), (45.0, -30.0)] {
            assert_eq!(
                g.soil(lat, lon, LAYER_LOOKUP_RADIUS).unwrap(),
                None,
                "{lat} {lon}"
            );
        }
    }

    /// (lat, lon, WGSRPD level 3, ISO alpha-2).
    const REGIONS: [(f64, f64, &str, &str); 14] = [
        (48.8566, 2.3522, "FRA", "FR"),
        (-23.55, -46.63, "BZL", "BR"),
        (-1.29, 36.82, "KEN", "KE"),
        (1.29, 103.85, "MLY", "SG"),
        (43.7384, 7.4246, "FRA", "MC"),
        (35.18, 33.36, "CYP", "CY"), // Nicosia, north of the line: Northern Cyprus -> CY
        (9.56, 44.06, "SOM", "SO"),  // Hargeisa, Somaliland -> SO
        (59.91, 10.75, "NOR", "NO"), // Natural Earth ISO_A2 -99, ISO_A2_EH NO
        (25.03, 121.56, "TAI", "TW"), // ISO_A2 CN-TW, ISO_A2_EH TW
        // Antimeridian: Fiji on both sides, Chukotka.
        (-17.8, 178.0, "FIJ", "FJ"),
        (-17.2, -178.95, "FIJ", "FJ"),
        (65.0, -175.0, "MAG", "RU"),
        (65.0, 178.0, "MAG", "RU"),
        (64.15, -21.94, "ICE", "IS"),
    ];

    #[test]
    #[ignore = "needs CANOPI_CLIMATE_GRID"]
    fn regions_at_known_points() {
        let mut g = grid();
        for (lat, lon, l3, a2) in REGIONS {
            let r = g.regions(lat, lon, LAYER_LOOKUP_RADIUS).unwrap().unwrap();
            assert_eq!(
                (r.wgsrpd_l3.as_deref(), r.iso_a2.as_deref()),
                (Some(l3), Some(a2)),
                "{lat} {lon}"
            );
        }
        let sea = g
            .regions(45.0, -30.0, LAYER_LOOKUP_RADIUS)
            .unwrap()
            .unwrap();
        assert_eq!((sea.wgsrpd_l3, sea.iso_a2), (None, None));
    }

    /// Overseas departments and territories with their own ISO 3166-1
    /// code (not their sovereign's): (lat, lon, iso_a2).
    const OVERSEAS: [(f64, f64, &str); 22] = [
        (-21.12, 55.53, "RE"),  // Réunion
        (4.93, -52.33, "GF"),   // Cayenne, French Guiana
        (16.25, -61.55, "GP"),  // Guadeloupe
        (14.64, -61.02, "MQ"),  // Martinique
        (-12.82, 45.15, "YT"),  // Mayotte
        (-21.3, 165.5, "NC"),   // New Caledonia
        (-17.6, -149.45, "PF"), // Tahiti
        (78.22, 15.65, "SJ"),   // Longyearbyen, Svalbard
        (71.0, -8.3, "SJ"),     // Jan Mayen
        (12.2, -68.26, "BQ"),   // Bonaire
        (12.12, -68.93, "CW"),  // Curaçao
        (12.52, -69.97, "AW"),  // Aruba
        (64.18, -51.72, "GL"),  // Nuuk, Greenland
        (62.01, -6.77, "FO"),   // Tórshavn, Faroe
        (18.4, -66.06, "PR"),   // San Juan, Puerto Rico
        (13.47, 144.75, "GU"),  // Guam
        (-10.49, 105.62, "CX"), // Christmas Island
        (-12.19, 96.83, "CC"),  // Cocos (Keeling) Islands
        (22.32, 114.17, "HK"),  // Hong Kong
        (22.19, 113.54, "MO"),  // Macao
        (32.3, -64.78, "BM"),   // Bermuda
        (-51.7, -57.85, "FK"),  // Stanley, Falklands
    ];

    #[test]
    #[ignore = "needs CANOPI_CLIMATE_GRID"]
    fn overseas_territories_have_their_own_codes() {
        let mut g = grid();
        let mut wrong = Vec::new();
        for (lat, lon, a2) in OVERSEAS {
            let r = g.regions(lat, lon, LAYER_LOOKUP_RADIUS).unwrap().unwrap();
            if r.iso_a2.as_deref() != Some(a2) {
                wrong.push(format!("{lat} {lon}: {:?} (want {a2})", r.iso_a2));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[test]
    #[ignore = "needs CANOPI_CLIMATE_GRID"]
    fn coastal_points_find_land() {
        let mut g = grid();
        // Points just offshore of a coast: the regions search reaches land
        // within 2 cells, and soil is there (measured or filled).
        for (lat, lon, a2) in [
            (43.52, 3.93, "FR"),
            (50.72, 1.50, "FR"), // 2 cells off Boulogne-sur-Mer
            (-33.92, 18.38, "ZA"),
        ] {
            let r = g.regions(lat, lon, LAYER_LOOKUP_RADIUS).unwrap().unwrap();
            assert_eq!(r.iso_a2.as_deref(), Some(a2), "{lat} {lon}");
            assert!(
                r.iso_a2_distance_cells > Some(0.0),
                "{lat} {lon} is offshore"
            );
            assert!(
                g.soil(lat, lon, LAYER_LOOKUP_RADIUS).unwrap().is_some(),
                "{lat} {lon}"
            );
        }
    }

    #[test]
    #[ignore = "needs CANOPI_CLIMATE_GRID"]
    fn release_verifies() {
        let g = grid();
        assert!(g.has_layer("soil") && g.has_layer("regions"));
        assert_eq!(g.verify().unwrap(), Vec::<String>::new());
        let ids: Vec<&str> = g.manifest().sources.iter().map(|s| s.id.as_str()).collect();
        for id in [
            "soilgrids-2.0",
            "natural-earth-map-units-50m",
            "wgsrpd-level3",
        ] {
            assert!(ids.contains(&id), "{id}");
        }
    }
}
