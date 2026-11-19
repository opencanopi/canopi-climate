//! Source datasets: pinned download URLs, SHA-256 checksums, citations and
//! licences. Nothing is downloaded by the crate (no HTTP client, no C
//! libraries): `canopi-climate sources` prints the URLs, and the build
//! verifies the files found in the source directory.

use std::path::Path;

use super::{PipelineError, Result, sha256_file};
use crate::manifest::{SourceFile, SourceSpec};

/// A pinned file.
#[derive(Debug, Clone, Copy)]
pub struct PinnedFile {
    /// Path relative to the source directory.
    pub path: &'static str,
    /// Download URL (for an extracted file, its archive).
    pub url: &'static str,
    /// SHA-256 (hex).
    pub sha256: &'static str,
}

/// A source dataset.
#[derive(Debug, Clone, Copy)]
pub struct Source {
    /// Identifier.
    pub id: &'static str,
    /// Dataset title.
    pub title: &'static str,
    /// Citation.
    pub citation: &'static str,
    /// DOI or landing page.
    pub landing: &'static str,
    /// Licence.
    pub licence: &'static str,
    /// Notes.
    pub notes: &'static str,
    /// Files used by the build.
    pub files: &'static [PinnedFile],
    /// Whether the grid needs these files (the rest serve checks only).
    pub required: bool,
    /// What the source feeds: `climate` (the climate tiles), or a layer
    /// id (`soil`, `regions`, `extremes`).
    pub layer: &'static str,
}

const FIGSHARE_KG: &str = "https://ndownloader.figshare.com/files/61012822";
const FIGSHARE_C0P1: &str = "https://ndownloader.figshare.com/files/61012831";
const CHELSA: &str = "https://os.zhdk.cloud.switch.ch/chelsav2/GLOBAL/climatologies";

const fn pin(path: &'static str, url: &'static str, sha256: &'static str) -> PinnedFile {
    PinnedFile { path, url, sha256 }
}

const BECK_FILES: &[PinnedFile] = &[
    pin(
        "beck2023/koppen_geiger_tif.zip",
        FIGSHARE_KG,
        "bb84453d4541f1a0bc5a804ead83f483c19ce70f16a5197f6d3a7b6a63e65562",
    ),
    pin(
        "beck2023/climate_data_0p1.zip",
        FIGSHARE_C0P1,
        "22e900d114ff23a008ebb94286d114e6b50d3cc95769f5cbe735009b2afc6cc5",
    ),
    pin(
        "beck2023/kg/1991_2020/koppen_geiger_0p00833333.tif",
        FIGSHARE_KG,
        "2130f0071dfb2904947d8ec3a0d807fac71004df76e769262004f1602e4d6a13",
    ),
    pin(
        "beck2023/kg/1991_2020/koppen_geiger_0p1.tif",
        FIGSHARE_KG,
        "59b749fe6711e72c720ff903044eb0efb4373524ee434740ed7384a0284c155b",
    ),
    pin(
        "beck2023/kg/2041_2070/ssp126/koppen_geiger_0p00833333.tif",
        FIGSHARE_KG,
        "ad9d9d12d64fd605a367a65513527c7dfcd764d8bd6ad205dca2d75bf3cd53ac",
    ),
    pin(
        "beck2023/kg/2041_2070/ssp245/koppen_geiger_0p00833333.tif",
        FIGSHARE_KG,
        "ff9b272de9ac8553847561c66d8b0ff163e45b9712bf13d05e70b8406e8e63a8",
    ),
    pin(
        "beck2023/kg/2041_2070/ssp370/koppen_geiger_0p00833333.tif",
        FIGSHARE_KG,
        "17a9febce547caa67ecc5d0e417516f543745fe1044be9490e36aa8b42762c87",
    ),
    pin(
        "beck2023/kg/2041_2070/ssp585/koppen_geiger_0p00833333.tif",
        FIGSHARE_KG,
        "d74b7851cdc3c7f124a2f9425e0fe7c98f2cfe7a82c78f4eab6c3600be1a90d7",
    ),
    pin(
        "beck2023/kg/2071_2099/ssp126/koppen_geiger_0p00833333.tif",
        FIGSHARE_KG,
        "d99fc9c845752076df5f55068126ff3d5b955b4d53422699a85f856a98d377c5",
    ),
    pin(
        "beck2023/kg/2071_2099/ssp245/koppen_geiger_0p00833333.tif",
        FIGSHARE_KG,
        "115bbb0fec0e670ce8a5fa150bc8cea067b8bfb3246f904625b0dd050c3b8f03",
    ),
    pin(
        "beck2023/kg/2071_2099/ssp370/koppen_geiger_0p00833333.tif",
        FIGSHARE_KG,
        "21beb01a0c5b695ce4fab918610142f0d306056f6c9b0c02c57ef902e43401cd",
    ),
    pin(
        "beck2023/kg/2071_2099/ssp585/koppen_geiger_0p00833333.tif",
        FIGSHARE_KG,
        "9dc796accd0e8f734feccd142b0ee7308bd3a0d13e200997348319389145e7dd",
    ),
    pin(
        "beck2023/c0p1/1961_1990/ensemble_mean_0p1.nc",
        FIGSHARE_C0P1,
        "8d7847ae56178d7a0797048412af0aa7345449bff6540afcbd774647f53b537f",
    ),
    pin(
        "beck2023/c0p1/1991_2020/ensemble_mean_0p1.nc",
        FIGSHARE_C0P1,
        "d57d9e6ea3e9bc4fb0864753ae96d4ccb38233b4f55c26c9712df2d8b77c0d76",
    ),
    pin(
        "beck2023/c0p1/2041_2070/ssp126/ensemble_mean_0p1.nc",
        FIGSHARE_C0P1,
        "5feb803566be01bbca9581d5115e48ba5e2148580f136026b8812c626214fdb5",
    ),
    pin(
        "beck2023/c0p1/2041_2070/ssp245/ensemble_mean_0p1.nc",
        FIGSHARE_C0P1,
        "bdf1a593c211955faf3c535b4898433972c4ca2d393ef803d4c68f457f16cf59",
    ),
    pin(
        "beck2023/c0p1/2041_2070/ssp370/ensemble_mean_0p1.nc",
        FIGSHARE_C0P1,
        "fc8c7ec687388037abfbd20e253c3c8a538046e344313102616bced1a58b32f4",
    ),
    pin(
        "beck2023/c0p1/2041_2070/ssp585/ensemble_mean_0p1.nc",
        FIGSHARE_C0P1,
        "cfcc38ee2c571729c5965d52b733f87dd4b531a1a455f9f93e9d1722757e4cca",
    ),
    pin(
        "beck2023/c0p1/2071_2099/ssp126/ensemble_mean_0p1.nc",
        FIGSHARE_C0P1,
        "5a15145b1428346039c45025b89278af37c2b7741ae733b5795de77ea6d3b567",
    ),
    pin(
        "beck2023/c0p1/2071_2099/ssp245/ensemble_mean_0p1.nc",
        FIGSHARE_C0P1,
        "d820ecd7c07eea816f305df8a4850f59a8e5b48f5619a60531af46cda24b1759",
    ),
    pin(
        "beck2023/c0p1/2071_2099/ssp370/ensemble_mean_0p1.nc",
        FIGSHARE_C0P1,
        "4fc868bdedb93fb790e4d9fa5ab1ead41cc970c8e71e40f44be9c2a8bc52d994",
    ),
    pin(
        "beck2023/c0p1/2071_2099/ssp585/ensemble_mean_0p1.nc",
        FIGSHARE_C0P1,
        "26c4abea72409c9067dc15c7ede6c5669b7c8761be7d98b43b45288199efd277",
    ),
];

const CHELSA_FILES: &[PinnedFile] = &[
    pin(
        "chelsa/CHELSA_bio5_1981-2010_V.2.1.tif",
        concat!(
            "https://os.zhdk.cloud.switch.ch/chelsav2/GLOBAL/climatologies",
            "/1981-2010/bio/CHELSA_bio5_1981-2010_V.2.1.tif"
        ),
        "9f88f80555cebeb35ae277cfdd1b5bbb11de71050bdc44e1ee0e980f9c478a8d",
    ),
    pin(
        "chelsa/CHELSA_bio6_1981-2010_V.2.1.tif",
        concat!(
            "https://os.zhdk.cloud.switch.ch/chelsav2/GLOBAL/climatologies",
            "/1981-2010/bio/CHELSA_bio6_1981-2010_V.2.1.tif"
        ),
        "fffa9e9948933a0e9b5fd4fb3e299e310e7e52e56785c7c493e1a543b44acfc1",
    ),
    pin(
        "chelsa/CHELSA_bio12_1981-2010_V.2.1.tif",
        concat!(
            "https://os.zhdk.cloud.switch.ch/chelsav2/GLOBAL/climatologies",
            "/1981-2010/bio/CHELSA_bio12_1981-2010_V.2.1.tif"
        ),
        "070c2309ead1ae703652e87f1ca51ba390c9ddfd9a236944528ae8b1c53a4e9a",
    ),
];

/// CHELSA 2041-2070 SSP3-7.0 bio6 of five GCMs: only for the delta-method
/// check (`build-grid --stage check-future`).
const CHELSA_FUTURE_FILES: &[PinnedFile] = &[
    pin(
        "chelsa/CHELSA_bio6_2041-2070_gfdl-esm4_ssp370_V.2.1.tif",
        concat!(
            "https://os.zhdk.cloud.switch.ch/chelsav2/GLOBAL/climatologies",
            "/2041-2070/GFDL-ESM4/ssp370/bio/CHELSA_bio6_2041-2070_gfdl-esm4_ssp370_V.2.1.tif"
        ),
        "9473388ee472322b75744455747815cd0d6e806914dca62708420872f96eb1f5",
    ),
    pin(
        "chelsa/CHELSA_bio6_2041-2070_ipsl-cm6a-lr_ssp370_V.2.1.tif",
        concat!(
            "https://os.zhdk.cloud.switch.ch/chelsav2/GLOBAL/climatologies",
            "/2041-2070/IPSL-CM6A-LR/ssp370/bio/CHELSA_bio6_2041-2070_ipsl-cm6a-lr_ssp370_V.2.1.tif"
        ),
        "c1726f03df7b1cc1a85805179dbe5aed3a196c92d1c74bdcd5ca3b8f09f4264c",
    ),
    pin(
        "chelsa/CHELSA_bio6_2041-2070_mpi-esm1-2-hr_ssp370_V.2.1.tif",
        concat!(
            "https://os.zhdk.cloud.switch.ch/chelsav2/GLOBAL/climatologies",
            "/2041-2070/MPI-ESM1-2-HR/ssp370/bio/CHELSA_bio6_2041-2070_mpi-esm1-2-hr_ssp370_V.2.1.tif"
        ),
        "b2bee2ef391e082c26ae1cacc7a897f9e44a47fe7a9b27d968fb77f91b62a3ef",
    ),
    pin(
        "chelsa/CHELSA_bio6_2041-2070_mri-esm2-0_ssp370_V.2.1.tif",
        concat!(
            "https://os.zhdk.cloud.switch.ch/chelsav2/GLOBAL/climatologies",
            "/2041-2070/MRI-ESM2-0/ssp370/bio/CHELSA_bio6_2041-2070_mri-esm2-0_ssp370_V.2.1.tif"
        ),
        "bd2007f47aa5f62b0a0056ac9608ecc78d1a82764bab524fcd62201a211a0f5d",
    ),
    pin(
        "chelsa/CHELSA_bio6_2041-2070_ukesm1-0-ll_ssp370_V.2.1.tif",
        concat!(
            "https://os.zhdk.cloud.switch.ch/chelsav2/GLOBAL/climatologies",
            "/2041-2070/UKESM1-0-LL/ssp370/bio/CHELSA_bio6_2041-2070_ukesm1-0-ll_ssp370_V.2.1.tif"
        ),
        "8fab7544072f0c7b8055b6a4395b05e5fb5acb7c6c95356d8ec85a097e03aba6",
    ),
];

/// Base URL of the SoilGrids 5 km aggregates (for documentation).
pub const SOILGRIDS_BASE: &str = "https://files.isric.org/soilgrids/latest/data_aggregated/5000m";

/// SoilGrids 2.0 5-km aggregates (Homolosine, `int16`), checked against
/// ISRIC's `checksum.sha256.txt` of each variable on 2026-11-11.
const SOILGRIDS_FILES: &[PinnedFile] = &[
    pin(
        "soilgrids/phh2o_0-5cm_mean_5000.tif",
        concat!(
            "https://files.isric.org/soilgrids/latest/data_aggregated/5000m",
            "/phh2o/phh2o_0-5cm_mean_5000.tif"
        ),
        "a74bc7e8551e8e281f7abb9cfc4fe3d60a72717c8cb131f7feb51b43a893c8c9",
    ),
    pin(
        "soilgrids/phh2o_5-15cm_mean_5000.tif",
        concat!(
            "https://files.isric.org/soilgrids/latest/data_aggregated/5000m",
            "/phh2o/phh2o_5-15cm_mean_5000.tif"
        ),
        "23816f452e812142d1f91bbc1c8d25026f05c131d7175a375030da8bf37ea8d0",
    ),
    pin(
        "soilgrids/phh2o_15-30cm_mean_5000.tif",
        concat!(
            "https://files.isric.org/soilgrids/latest/data_aggregated/5000m",
            "/phh2o/phh2o_15-30cm_mean_5000.tif"
        ),
        "16faa5e308dfb60681bbad24bc6b41df69cb6a507506ac8683e22d98c0c27f01",
    ),
    pin(
        "soilgrids/sand_0-5cm_mean_5000.tif",
        concat!(
            "https://files.isric.org/soilgrids/latest/data_aggregated/5000m",
            "/sand/sand_0-5cm_mean_5000.tif"
        ),
        "af70445daad0ff93830e81805e55c2b84ff6bf4f4ac2d62e0ab6bc1648e7708f",
    ),
    pin(
        "soilgrids/sand_5-15cm_mean_5000.tif",
        concat!(
            "https://files.isric.org/soilgrids/latest/data_aggregated/5000m",
            "/sand/sand_5-15cm_mean_5000.tif"
        ),
        "8cfb0cc06ddd6c6557e6bbe9c85b502adfa31270e92ad45bfd6255c14d343610",
    ),
    pin(
        "soilgrids/sand_15-30cm_mean_5000.tif",
        concat!(
            "https://files.isric.org/soilgrids/latest/data_aggregated/5000m",
            "/sand/sand_15-30cm_mean_5000.tif"
        ),
        "e89d0475f6fcd0651fb90046811af54c229a5aa84d046af3c17a9eceeabcc44c",
    ),
    pin(
        "soilgrids/clay_0-5cm_mean_5000.tif",
        concat!(
            "https://files.isric.org/soilgrids/latest/data_aggregated/5000m",
            "/clay/clay_0-5cm_mean_5000.tif"
        ),
        "8d75b5db5bb295b4f54e64068d5cdbae8283ae7d6fab5278b9225face1b7020a",
    ),
    pin(
        "soilgrids/clay_5-15cm_mean_5000.tif",
        concat!(
            "https://files.isric.org/soilgrids/latest/data_aggregated/5000m",
            "/clay/clay_5-15cm_mean_5000.tif"
        ),
        "75d8cbd87391814fd400edeb1bd970ea458a1dde2640b2fafd905f84f814e0fb",
    ),
    pin(
        "soilgrids/clay_15-30cm_mean_5000.tif",
        concat!(
            "https://files.isric.org/soilgrids/latest/data_aggregated/5000m",
            "/clay/clay_15-30cm_mean_5000.tif"
        ),
        "d42c0067eccea56ed3272608149c510e5fc1aa0c555cc6782c972cc1b768f72b",
    ),
];

const NATURAL_EARTH_FILES: &[PinnedFile] = &[pin(
    "naturalearth/ne_50m_admin_0_map_units.geojson",
    "https://raw.githubusercontent.com/nvkelso/natural-earth-vector/v5.1.2/geojson/ne_50m_admin_0_map_units.geojson",
    "b8d421aca6e9e08e8cdf09cc26af111cc3e0deba4fe915611d58ade71e8a4db0",
)];

const WGSRPD_FILES: &[PinnedFile] = &[pin(
    "wgsrpd/level3.geojson",
    "https://raw.githubusercontent.com/tdwg/wgsrpd/f68afd99aa94adadaa3e0328e4aadda75ecc4215/geojson/level3.geojson",
    "c172bcf6aba20e19477adc60aebf0023068f0175dca8480f0760b090dcf64840",
)];

// The Earth Engine rasters are computed by `scripts/ee/extremes.py` and
// published with the grid release that first used them.
const ERA5_LAND_FILES: &[PinnedFile] = &[pin(
    "ee/era5land_baseline.tif",
    "https://github.com/opencanopi/canopi-climate/releases/download/v0.3.0/era5land_baseline.tif",
    "295f703f7ceb4df31dce38d2201b09fff08ae693d60e3a507c4d4f083890cb55",
)];

const NEX_GDDP_FILES: &[PinnedFile] = &[pin(
    "ee/nexgddp_k.tif",
    "https://github.com/opencanopi/canopi-climate/releases/download/v0.3.0/nexgddp_k.tif",
    "8820706498ee7c6dbd188a28aeac39f22eb93a9f29a130f100a46ef0af1bed67",
)];

/// Every source, grid inputs first.
pub const SOURCES: &[Source] = &[
    Source {
        id: "beck2023",
        title: "High-resolution (1 km) Köppen-Geiger maps for 1901-2099 based on constrained CMIP6 projections (figshare v2, January 2026 correction)",
        citation: "Beck, H. E., McVicar, T. R., Vergopolan, N., Berg, A., Lutsko, N. J., Dufour, A., Zeng, Z., Jiang, X., van Dijk, A. I. J. M., Miralles, D. G. (2023). High-resolution (1 km) Köppen-Geiger maps for 1901-2099 based on constrained CMIP6 projections. Scientific Data 10, 724. https://doi.org/10.1038/s41597-023-02549-6",
        landing: "https://doi.org/10.6084/m9.figshare.21789074.v2",
        licence: "CC BY 4.0 as published on figshare (https://creativecommons.org/licenses/by/4.0/)",
        notes: "The authors' GloH2O page links CC BY-NC 4.0 instead; the Canopi project has the authors' permission for its use. Köppen-Geiger classes (1-km maps) and the 0.1-degree monthly temperature and precipitation climatologies (ensemble mean of the constrained CMIP6 models; 1961-1990 and 1991-2020 from observations).",
        files: BECK_FILES,
        required: true,
        layer: "climate",
    },
    Source {
        id: "chelsa-v2.1",
        title: "CHELSA V2.1 bioclimatic variables, 1981-2010 (bio5, bio6, bio12)",
        citation: "Karger, D. N., Conrad, O., Böhner, J., Kawohl, T., Kreft, H., Soria-Auza, R. W., Zimmermann, N. E., Linder, H. P., Kessler, M. (2017). Climatologies at high resolution for the earth's land surface areas. Scientific Data 4, 170122. https://doi.org/10.1038/sdata.2017.122 -- Karger, D. N., et al. (2021). Climatologies at high resolution for the earth's land surface areas (CHELSA V2.1). EnviDat. https://doi.org/10.16904/envidat.228 -- Brun, P., Zimmermann, N. E., Hari, C., Pellissier, L., Karger, D. N. (2022). Global climate-related predictors at kilometre resolution for the past and future. Earth System Science Data 14, 5573-5603. https://doi.org/10.5194/essd-14-5573-2022",
        landing: "https://doi.org/10.16904/envidat.228",
        licence: "CC0 1.0 (https://creativecommons.org/publicdomain/zero/1.0/)",
        notes: "bio6 (mean daily minimum of the coldest month) and bio5 (mean daily maximum of the warmest month) give the baseline of tmin_cold and tmax_warm; bio12 serves only as a check on precipitation.",
        files: CHELSA_FILES,
        required: true,
        layer: "climate",
    },
    Source {
        id: "chelsa-v2.1-future-check",
        title: "CHELSA V2.1 bio6, 2041-2070, SSP3-7.0, five GCMs (check only, not in the grid)",
        citation: "Karger et al. (2021), CHELSA V2.1, EnviDat, https://doi.org/10.16904/envidat.228; Brun et al. (2022), https://doi.org/10.5194/essd-14-5573-2022",
        landing: "https://doi.org/10.16904/envidat.228",
        licence: "CC0 1.0 (https://creativecommons.org/publicdomain/zero/1.0/)",
        notes: "Compares the grid's delta-method 2041-2070 SSP3-7.0 tmin_cold with CHELSA's own downscaled projections.",
        files: CHELSA_FUTURE_FILES,
        required: false,
        layer: "climate",
    },
    Source {
        id: "soilgrids-2.0",
        title: "SoilGrids 2.0, 5 km aggregates: pH (water), sand and clay at 0-5, 5-15 and 15-30 cm",
        citation: "Poggio, L., de Sousa, L. M., Batjes, N. H., Heuvelink, G. B. M., Kempen, B., Ribeiro, E., Rossiter, D. (2021). SoilGrids 2.0: producing soil information for the globe with quantified spatial uncertainty. SOIL 7, 217-240. https://doi.org/10.5194/soil-7-217-2021",
        landing: "https://www.isric.org/explore/soilgrids",
        licence: "CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/)",
        notes: "Mean predictions, Interrupted Goode Homolosine (ESRI:54052), 5 km. Reprojected to latitude/longitude by nearest pixel at 0.05-degree cell centres and combined into a depth-weighted 0-30 cm mean. SoilGrids has no value on built-up areas, water and permanent ice; nearby land cells take the nearest measured value (fill_distance band).",
        files: SOILGRIDS_FILES,
        required: true,
        layer: "soil",
    },
    Source {
        id: "natural-earth-map-units-50m",
        title: "Natural Earth 1:50m Cultural Vectors, Admin 0 - Map Units, version 5.1.2",
        citation: "Made with Natural Earth. Free vector and raster map data @ naturalearthdata.com. Admin 0 - Details, Map Units, 1:50m, version 5.1.2.",
        landing: "https://www.naturalearthdata.com/downloads/50m-cultural-vectors/50m-admin-0-details/",
        licence: "Public domain (https://www.naturalearthdata.com/about/terms-of-use/)",
        notes: "Country of each cell as ISO 3166-1 alpha-2. Map units split overseas departments and territories from their sovereign (Reunion RE, French Guiana GF, Svalbard SJ, Bonaire BQ, Christmas Island CX, ...). Code: Jan Mayen (GU_A3 NJM) -> SJ as in ISO 3166-1; else ISO_A2_EH when it is a two-letter code; else ISO_A2; else (Natural Earth gives -99) the de jure country of Somaliland (SOL -> SO) and Northern Cyprus (CYN -> CY); the Siachen Glacier (KAS) gets none. Gibraltar, Bouvet Island and the US Minor Outlying Islands have no map unit at 1:50m. Natural Earth shows de facto boundaries.",
        files: NATURAL_EARTH_FILES,
        required: true,
        layer: "regions",
    },
    Source {
        id: "wgsrpd-level3",
        title: "World Geographical Scheme for Recording Plant Distributions (WGSRPD), level 3 botanical countries",
        citation: "Brummitt, R. K. (2001). World Geographical Scheme for Recording Plant Distributions, Edition 2. Plant Taxonomic Database Standards No. 2. Hunt Institute for Botanical Documentation, Carnegie Mellon University, Pittsburgh, for the International Working Group on Taxonomic Databases for Plant Sciences (TDWG). Polygons: TDWG, https://github.com/tdwg/wgsrpd (geojson/level3.geojson at commit f68afd9).",
        landing: "https://github.com/tdwg/wgsrpd",
        licence: "Not declared: the tdwg/wgsrpd repository carries no licence (checked 2026-11-11).",
        notes: "369 level-3 areas, the scheme WCVP uses for native ranges. The licence is not declared upstream; the file is used as the published TDWG standard's reference geometry, with this attribution.",
        files: WGSRPD_FILES,
        required: true,
        layer: "regions",
    },
    Source {
        id: "era5-land",
        title: "ERA5-Land daily aggregates (ECMWF/ERA5_LAND/DAILY_AGGR on Google Earth Engine), daily minimum and maximum 2 m temperature, 1991-2020",
        citation: "Muñoz-Sabater, J., et al. (2021). ERA5-Land: a state-of-the-art global reanalysis dataset for land applications. Earth System Science Data 13, 4349-4383. https://doi.org/10.5194/essd-13-4349-2021. Contains modified Copernicus Climate Change Service information.",
        landing: "https://developers.google.com/earth-engine/datasets/catalog/ECMWF_ERA5_LAND_DAILY_AGGR",
        licence: "CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/)",
        notes: "Reduced on Earth Engine by scripts/ee/extremes.py (baseline): per cell, the mean over 1991-2020 of each year's lowest daily minimum and highest daily maximum, minus the coldest month's mean daily minimum and the warmest month's mean daily maximum, on ERA5-Land's own 0.1-degree grid (the build interpolates bilinearly to its cell centres); float32 °C.",
        files: ERA5_LAND_FILES,
        required: true,
        layer: "extremes",
    },
    Source {
        id: "nex-gddp-cmip6",
        title: "NASA Earth Exchange Global Daily Downscaled Projections, CMIP6 (NASA/GDDP-CMIP6 on Google Earth Engine), 16 models (one per model family), historical, SSP2-4.5, SSP5-8.5",
        citation: "Thrasher, B., Wang, W., Michaelis, A., Melton, F., Lee, T., Nemani, R. (2022). NASA Global Daily Downscaled Projections, CMIP6. Scientific Data 9, 262. https://doi.org/10.1038/s41597-022-01393-4",
        landing: "https://developers.google.com/earth-engine/datasets/catalog/NASA_GDDP-CMIP6",
        licence: "CC0 1.0 (blanket licence for all NEX-GDDP-CMIP6 outputs since September 2022)",
        notes: "Reduced on Earth Engine by scripts/ee/extremes.py (future, median): per model, the change of the extreme-minus-monthly gaps against coldest/warmest-month warming, fitted through the origin over 2041-2070 and 2071-2099 under SSP2-4.5 and SSP5-8.5 (1991-2020 = historical 1991-2014 + SSP2-4.5 2015-2020); the median over the models, 1 degree, float32.",
        files: NEX_GDDP_FILES,
        required: true,
        layer: "extremes",
    },
];

/// Base URL of CHELSA climatologies (for documentation).
pub const CHELSA_BASE: &str = CHELSA;

/// Verify the files of the sources (`required_only` skips check-only ones).
/// Returns the problems found (empty when every file matches).
///
/// # Errors
/// I/O errors other than a missing file.
pub fn verify(dir: &Path, required_only: bool) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    for s in SOURCES.iter().filter(|s| s.required || !required_only) {
        for f in s.files {
            let p = dir.join(f.path);
            if !p.exists() {
                problems.push(format!("missing {} (download {})", f.path, f.url));
                continue;
            }
            let got = sha256_file(&p)?;
            if got != f.sha256 {
                problems.push(format!("{}: sha256 {got}, expected {}", f.path, f.sha256));
            }
        }
    }
    Ok(problems)
}

/// Manifest entries of the sources that `layer` (`climate`, `soil`,
/// `regions`) uses.
#[must_use]
pub fn manifest_sources(layer: &str) -> Vec<SourceSpec> {
    SOURCES
        .iter()
        .filter(|s| s.required && s.layer == layer)
        .map(|s| SourceSpec {
            id: s.id.into(),
            title: s.title.into(),
            citation: s.citation.into(),
            landing: s.landing.into(),
            licence: s.licence.into(),
            notes: s.notes.into(),
            files: s
                .files
                .iter()
                .map(|f| SourceFile {
                    path: f.path.into(),
                    url: f.url.into(),
                    sha256: f.sha256.into(),
                })
                .collect(),
        })
        .collect()
}

/// Path of a pinned file, erroring if the id is unknown.
///
/// # Errors
/// [`PipelineError::Source`] for a path not in the table.
pub fn pinned(dir: &Path, rel: &str) -> Result<std::path::PathBuf> {
    SOURCES
        .iter()
        .flat_map(|s| s.files.iter())
        .find(|f| f.path == rel)
        .map(|f| dir.join(f.path))
        .ok_or_else(|| PipelineError::Source(format!("{rel} is not a pinned source file")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_are_well_formed_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for s in SOURCES {
            assert!(!s.citation.is_empty() && !s.licence.is_empty());
            for f in s.files {
                assert_eq!(f.sha256.len(), 64, "{}", f.path);
                assert!(f.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
                assert!(f.url.starts_with("https://"), "{}", f.url);
                assert!(seen.insert(f.path), "duplicate {}", f.path);
            }
        }
        assert!(CHELSA_FILES.iter().all(|f| f.url.starts_with(CHELSA_BASE)));
        assert!(
            SOILGRIDS_FILES
                .iter()
                .all(|f| f.url.starts_with(SOILGRIDS_BASE))
        );
        for layer in ["climate", "soil", "regions", "extremes"] {
            assert!(!manifest_sources(layer).is_empty(), "{layer}");
        }
    }

    #[test]
    fn verify_reports_missing_files() {
        let dir = std::env::temp_dir().join("canopi-climate-no-sources");
        let problems = verify(&dir, true).unwrap();
        assert_eq!(
            problems.len(),
            BECK_FILES.len()
                + CHELSA_FILES.len()
                + SOILGRIDS_FILES.len()
                + NATURAL_EARTH_FILES.len()
                + WGSRPD_FILES.len()
                + ERA5_LAND_FILES.len()
                + NEX_GDDP_FILES.len()
        );
        assert!(problems[0].starts_with("missing beck2023/"));
        assert!(pinned(&dir, "chelsa/CHELSA_bio6_1981-2010_V.2.1.tif").is_ok());
        assert!(pinned(&dir, "nope").is_err());
    }
}
