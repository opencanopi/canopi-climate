//! Interrupted Goode Homolosine (`+proj=igh`), the projection of SoilGrids
//! 2.0 (ESRI:54052 on WGS 84).
//!
//! `proj4rs` 0.3.0 has no `igh`, but it has the two projections Homolosine is
//! made of: sinusoidal (`sinu`) between 40°44'11.8" N and S, and Mollweide
//! (`moll`) beyond. This module composes them exactly as PROJ's `igh.cpp`
//! does: twelve lobes, each a `proj4rs` sinusoidal or Mollweide projection
//! with its own central meridian and false easting (the central meridian
//! times the radius); the Mollweide lobes are shifted north or south so
//! that they meet the sinusoidal ones at 40°44'11.8". Like PROJ, the
//! projection is spherical with the radius of the WGS 84 semi-major axis,
//! and geodetic latitudes are used as spherical ones.
//!
//! Only the forward direction is needed: the pipeline projects each output
//! cell centre and reads the SoilGrids pixel under it. Tests compare with
//! PROJ 9 (GDAL 3.8.4 `gdaltransform`) at points in every lobe.

use proj4rs::proj::Proj;

use super::{PipelineError, Result};

/// Sphere radius used by `+proj=igh +datum=WGS84` (WGS 84 semi-major axis).
pub const RADIUS_M: f64 = 6_378_137.0;

/// Latitude where the sinusoidal and Mollweide lobes meet, 40°44'11.8".
pub const LAT_JOIN_DEG: f64 = 40.0 + 44.0 / 60.0 + 11.8 / 3600.0;

/// The Interrupted Goode Homolosine forward projection.
pub struct Homolosine {
    geographic: Proj,
    /// Lobes 1-12 in PROJ's numbering (index 0 = lobe 1).
    lobes: Vec<Proj>,
}

fn proj(def: &str) -> Result<Proj> {
    Proj::from_proj_string(def).map_err(|e| PipelineError::Data(format!("proj4rs {def}: {e}")))
}

/// Central meridian (degrees), Mollweide or not, and northing shift sign
/// (+1 north lobes, -1 south lobes, 0 sinusoidal) of PROJ's lobes 1-12.
const LOBES: [(f64, bool, f64); 12] = [
    (-100.0, true, 1.0),
    (30.0, true, 1.0),
    (-100.0, false, 0.0),
    (30.0, false, 0.0),
    (-160.0, false, 0.0),
    (-60.0, false, 0.0),
    (20.0, false, 0.0),
    (140.0, false, 0.0),
    (-160.0, true, -1.0),
    (-60.0, true, -1.0),
    (20.0, true, -1.0),
    (140.0, true, -1.0),
];

impl Homolosine {
    /// Build the twelve lobes.
    ///
    /// # Errors
    /// A `proj4rs` definition that does not parse (not expected).
    pub fn new() -> Result<Self> {
        // Same sphere on both sides: no datum shift, geodetic latitude used
        // as spherical latitude, as in PROJ's igh.
        let geographic = proj(&format!("+proj=longlat +R={RADIUS_M} +no_defs"))?;
        let dy0 = Self::mollweide_shift(&geographic)?;
        let lobes = LOBES
            .iter()
            .map(|&(lon0, moll, shift)| {
                let name = if moll { "moll" } else { "sinu" };
                let x0 = lon0.to_radians() * RADIUS_M;
                let y0 = shift * dy0;
                proj(&format!(
                    "+proj={name} +R={RADIUS_M} +lon_0={lon0} +x_0={x0} +y_0={y0} +units=m +no_defs"
                ))
            })
            .collect::<Result<_>>()?;
        Ok(Self { geographic, lobes })
    }

    /// Northing (m) added to the northern Mollweide lobes: the sinusoidal
    /// northing at the join latitude minus the Mollweide one.
    fn mollweide_shift(geographic: &Proj) -> Result<f64> {
        let at = |name: &str| -> Result<f64> {
            let p = proj(&format!("+proj={name} +R={RADIUS_M} +units=m +no_defs"))?;
            let mut pt = (0.0, LAT_JOIN_DEG.to_radians(), 0.0);
            proj4rs::transform::transform(geographic, &p, &mut pt)
                .map_err(|e| PipelineError::Data(format!("proj4rs {name}: {e}")))?;
            Ok(pt.1)
        };
        Ok(at("sinu")? - at("moll")?)
    }

    /// PROJ's lobe (1-12) for a longitude/latitude in degrees.
    #[must_use]
    pub fn lobe(lon: f64, lat: f64) -> usize {
        let south = |lon: f64| {
            if lon <= -100.0 {
                0
            } else if lon <= -20.0 {
                1
            } else if lon <= 80.0 {
                2
            } else {
                3
            }
        };
        if lat >= LAT_JOIN_DEG {
            if lon <= -40.0 { 1 } else { 2 }
        } else if lat >= 0.0 {
            if lon <= -40.0 { 3 } else { 4 }
        } else if lat >= -LAT_JOIN_DEG {
            5 + south(lon)
        } else {
            9 + south(lon)
        }
    }

    /// Project a longitude/latitude (degrees, longitude in [-180, 180]) to
    /// Homolosine metres. `None` for a non-finite or out-of-range input.
    #[must_use]
    pub fn forward(&self, lon: f64, lat: f64) -> Option<(f64, f64)> {
        if !lon.is_finite() || !(-180.0..=180.0).contains(&lon) || !(-90.0..=90.0).contains(&lat) {
            return None;
        }
        let lobe = self.lobes.get(Self::lobe(lon, lat) - 1)?;
        let mut pt = (lon.to_radians(), lat.to_radians(), 0.0);
        proj4rs::transform::transform(&self.geographic, lobe, &mut pt).ok()?;
        (pt.0.is_finite() && pt.1.is_finite()).then_some((pt.0, pt.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `gdaltransform -s_srs "+proj=longlat +datum=WGS84" -t_srs
    /// "+proj=igh +datum=WGS84 +units=m"` (GDAL 3.8.4, PROJ 9).
    const PROJ_REFERENCE: [(f64, f64, f64, f64); 26] = [
        (1.75, 48.30, 1143793.54973498, 5357513.37749651),
        (3.55, 43.65, 1172118.55238739, 4856366.07764581),
        (37.0, -0.5, 4118749.1013643, -55659.7453966368),
        (-48.0, -22.0, -5440605.83285098, -2449028.79745202),
        (-100.0, 60.0, -11131949.0793274, 6539970.86168982),
        (-120.0, 45.0, -12747354.0465757, 5003456.77990526),
        (-41.0, 41.0, -6167753.58475491, 4564077.37058701),
        (-39.0, 41.0, -2465999.83765093, 4564077.37058701),
        (-60.0, 10.0, -6746817.17554388, 1113194.90793274),
        (120.0, 30.0, 12016080.3504973, 3339584.72379821),
        (-170.0, -10.0, -18907401.5028696, -1113194.90793274),
        (-150.0, -20.0, -16765057.4864429, -2226389.81586547),
        (-30.0, -30.0, -3787004.23869673, -3339584.72379821),
        (50.0, -35.0, 4962017.46944106, -3896182.17776457),
        (150.0, -25.0, 16593625.9246073, -2782987.26983184),
        (-170.0, -60.0, -18459682.4574391, -6539970.86168982),
        (-80.0, -50.0, -8200435.82001744, -5536683.82457167),
        (0.0, -45.0, 610984.848617146, -5003456.77990526),
        (100.0, -70.0, 13551965.6824757, -7437681.47625251),
        (170.0, -45.0, 18007836.1619308, -5003456.77990526),
        (179.9, 65.0, 12069850.6331747, 7003942.53489363),
        (-179.9, 65.0, -15785373.0030043, 7003942.53489363),
        (-30.0, 75.0, 794974.098208057, 7835874.58627853),
        (0.0, 0.0, 0.0, 0.0),
        (10.0, 40.7366, 1652609.91512721, 4534777.56864927),
        (10.0, 40.7367, 1652612.09236137, 4534788.70059872),
    ];

    #[test]
    fn matches_proj_in_every_lobe() {
        let h = Homolosine::new().unwrap();
        let mut lobes = std::collections::BTreeSet::new();
        for (lon, lat, x, y) in PROJ_REFERENCE {
            lobes.insert(Homolosine::lobe(lon, lat));
            let (px, py) = h.forward(lon, lat).unwrap();
            // Millimetres: well below the 5 km SoilGrids pixel.
            assert!(
                (px - x).abs() < 1e-3 && (py - y).abs() < 1e-3,
                "{lon} {lat}: {px} {py}, PROJ {x} {y}"
            );
        }
        assert_eq!(lobes.len(), 12, "every lobe tested");
    }

    #[test]
    fn rejects_bad_input() {
        let h = Homolosine::new().unwrap();
        assert_eq!(h.forward(f64::NAN, 0.0), None);
        assert_eq!(h.forward(0.0, 91.0), None);
        assert_eq!(h.forward(181.0, 0.0), None);
        assert!(h.forward(180.0, 0.0).is_some());
        assert!(h.forward(-180.0, -90.0).is_some());
    }
}
