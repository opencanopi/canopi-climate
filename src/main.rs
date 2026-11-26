//! `canopi-climate` command line: build the grid, sample points, look up one
//! point.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use canopi_climate::lookup::DEFAULT_RADIUS;
use canopi_climate::pipeline::build::{self, BuildConfig, Stage};
use canopi_climate::pipeline::sample::{Grid, SampleOptions, sample_csv_with};
use canopi_climate::pipeline::{PipelineError, Result, sources, vectors};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "canopi-climate",
    version,
    about = "Global 0.1-degree climate grid for Canopi"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build the grid from the pinned sources, stage by stage.
    BuildGrid {
        /// Directory holding the source files (`canopi-climate sources`);
        /// required, from the flag or the environment variable.
        #[arg(long, env = "CANOPI_CLIMATE_SOURCES")]
        sources: PathBuf,
        /// Release directory (tiles, manifest).
        #[arg(long)]
        out: PathBuf,
        /// Work directory for intermediates (default: `<out>/../work-<out name>`).
        #[arg(long)]
        work: Option<PathBuf>,
        /// Stages to run (repeatable); default: verify, koppen, chelsa,
        /// chelsa-monthly, beck, tiles, regions, soil, extremes, monthly,
        /// monthly-precip.
        #[arg(long = "stage", value_enum)]
        stages: Vec<Stage>,
        /// Only these Beck periods in the `beck` stage (e.g. 1991_2020, 2041_2070_ssp245).
        #[arg(long = "beck-period")]
        beck_only: Vec<String>,
        /// Redo stages whose outputs exist.
        #[arg(long)]
        force: bool,
        /// Skip the source verification check (development only).
        #[arg(long)]
        skip_verify: bool,
    },
    /// List the source files with their URLs and SHA-256; with --verify,
    /// check a local copy.
    Sources {
        /// Check the files in this directory.
        #[arg(long)]
        verify: Option<PathBuf>,
    },
    /// Sample a CSV of points (columns lat, lon, optional id) and write a
    /// long CSV: one row per point and period/scenario.
    Sample {
        /// Input CSV (`-` for standard input).
        points: PathBuf,
        /// Grid directory.
        #[arg(long)]
        grid: PathBuf,
        /// Output CSV (default: standard output).
        #[arg(long, short)]
        output: Option<PathBuf>,
        /// Search radius in cells for the nearest complete cell.
        #[arg(long, default_value_t = DEFAULT_RADIUS)]
        radius: usize,
        /// Append the columns wgsrpd_l3 and iso_a2: each point's WGSRPD
        /// level-3 area and country, as `lookup` gives them (needs the
        /// regions layer). Without it the output is unchanged.
        #[arg(long)]
        regions: bool,
        /// Append tmin_extreme_c, tmax_extreme_c, usda_zone (needs the
        /// extremes layer). Without it the output is unchanged.
        #[arg(long)]
        extremes: bool,
        /// Append monthly tmean, tmin, dry_mask, gdd_base_10, chill_months
        /// and monthly precip columns for the 3 monthly slots (needs the
        /// monthly layer). Without it the output is unchanged.
        #[arg(long)]
        monthly: bool,
    },
    /// Look one point up and print JSON: `climate`, `extremes`,
    /// `extremes_tail`, `frost_calendar`, `monthly`, `monthly_precip`,
    /// `phenology`, `water_balance`, `soil`, `soil_plus`, `terrain` and
    /// `regions` (each `null` when absent; exit code 2 when all are).
    Lookup {
        /// Latitude (degrees north).
        #[arg(allow_hyphen_values = true)]
        lat: f64,
        /// Longitude (degrees east).
        #[arg(allow_hyphen_values = true)]
        lon: f64,
        /// Grid directory.
        #[arg(long)]
        grid: PathBuf,
        /// Search radius in climate cells (soil and regions search 2 layer
        /// cells).
        #[arg(long, default_value_t = DEFAULT_RADIUS)]
        radius: usize,
    },
    /// Write the lookup vectors of a grid (`vectors/lookup.json`): each
    /// named point's `lookup` JSON, whether a single-tile reader agrees,
    /// and the rule tables (docs/format.md).
    Vectors {
        /// Grid directory.
        #[arg(long)]
        grid: PathBuf,
        /// CSV of name,lat,lon.
        #[arg(long)]
        points: PathBuf,
        /// Output file (default: standard output).
        #[arg(long, short)]
        output: Option<PathBuf>,
        /// Compare with this committed file instead of writing (the `cli`
        /// field aside); exit 1 when it is stale.
        #[arg(long, conflicts_with = "output")]
        check: Option<PathBuf>,
    },
    /// Check every tile of a grid against its manifest.
    VerifyGrid {
        /// Grid directory.
        grid: PathBuf,
    },
    /// Find historical (1991-2020) climate twins matching a point's climate in
    /// the given slot (default: 2041-2070 ssp245) using `analogues.bin`.
    Analogues {
        /// Latitude (degrees north).
        #[arg(allow_hyphen_values = true)]
        lat: f64,
        /// Longitude (degrees east).
        #[arg(allow_hyphen_values = true)]
        lon: f64,
        /// Grid directory containing `analogues.bin`.
        #[arg(long)]
        grid: PathBuf,
        /// Climate slot index (0 = 1991-2020, 2 = 2041-2070 ssp245, 6 = 2071-2099 ssp245).
        #[arg(long, default_value_t = 2)]
        slot: usize,
        /// Number of spatially deduplicated twins to return.
        #[arg(long, default_value_t = 5)]
        k: usize,
        /// Minimum Haversine separation between returned twins (km).
        #[arg(long, default_value_t = 100.0)]
        min_distance_km: f64,
        /// Exclude historical twins closer than this distance to the query point (km).
        #[arg(long, default_value_t = 0.0)]
        exclude_radius_km: f64,
    },
    /// Package a built grid into decoupled modular `.tar` layer packs plus
    /// `packs-vX.Y.Z.json` (+ `.sha256`) under `format_version: 1`.
    PackGrid {
        /// Built grid directory (containing `manifest.json` and tile directories).
        grid: PathBuf,
        /// Output directory for pack `.tar` archives and `packs-vX.Y.Z.json`.
        #[arg(long)]
        out: PathBuf,
        /// Release version string (e.g. `0.7.0`, defaults to crate version).
        #[arg(long, default_value = env!("CARGO_PKG_VERSION"))]
        version: String,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Command::BuildGrid {
            sources,
            out,
            work,
            stages,
            beck_only,
            force,
            skip_verify,
        } => build_grid_cmd(sources, out, work, stages, beck_only, force, skip_verify),
        Command::Sources { verify } => list_sources(verify),
        Command::Sample {
            points,
            grid,
            output,
            radius,
            regions,
            extremes,
            monthly,
        } => sample(
            &points,
            &grid,
            output.as_deref(),
            &SampleOptions {
                radius,
                regions,
                extremes,
                monthly,
            },
        ),
        Command::Lookup {
            lat,
            lon,
            grid,
            radius,
        } => {
            let site = Grid::open(&grid)?.site(lat, lon, radius, false)?;
            let text = serde_json::to_string_pretty(&site.json())
                .map_err(|e| PipelineError::Data(e.to_string()))?;
            println!("{text}");
            Ok(if site.found() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            })
        }
        Command::Vectors {
            grid,
            points,
            output,
            check,
        } => vectors_cmd(&grid, &points, output.as_deref(), check.as_deref()),
        Command::VerifyGrid { grid } => {
            let g = Grid::open(&grid)?;
            let problems = g.verify()?;
            for p in &problems {
                eprintln!("{p}");
            }
            let m = g.manifest();
            let layer_tiles: usize = m.layers.iter().map(|l| l.tiles.len()).sum();
            eprintln!(
                "{} climate tiles, {layer_tiles} layer tiles, {} problems",
                m.tiles.len(),
                problems.len()
            );
            Ok(if problems.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Command::Analogues {
            lat,
            lon,
            grid,
            slot,
            k,
            min_distance_km,
            exclude_radius_km,
        } => analogues_cmd(
            lat,
            lon,
            &grid,
            slot,
            canopi_climate::analogues::SearchOptions {
                k,
                min_distance_km,
                exclude_radius_km,
            },
        ),
        Command::PackGrid { grid, out, version } => pack_grid_cmd(&grid, &out, &version),
    }
}

fn pack_grid_cmd(grid: &Path, out: &Path, version: &str) -> Result<ExitCode> {
    let index = canopi_climate::pipeline::packs::pack_grid(grid, out, version)?;
    eprintln!(
        "packed {} modular pack(s) into {} (manifest_sha256={})",
        index.packs.len(),
        out.display(),
        index.manifest_sha256
    );
    Ok(ExitCode::SUCCESS)
}

fn build_grid_cmd(
    sources: PathBuf,
    out: PathBuf,
    work: Option<PathBuf>,
    stages: Vec<Stage>,
    beck_only: Vec<String>,
    force: bool,
    skip_verify: bool,
) -> Result<ExitCode> {
    let work = work.unwrap_or_else(|| {
        let name = out
            .file_name()
            .map_or_else(|| "grid".into(), |n| n.to_string_lossy().into_owned());
        out.with_file_name(format!("work-{name}"))
    });
    let cfg = BuildConfig {
        sources,
        out,
        work,
        force,
        skip_verify,
        beck_only,
    };
    let stages = if stages.is_empty() {
        build::DEFAULT_STAGES.to_vec()
    } else {
        stages
    };
    build::run(&cfg, &stages)?;
    Ok(ExitCode::SUCCESS)
}

fn analogues_cmd(
    lat: f64,
    lon: f64,
    grid: &Path,
    slot: usize,
    opts: canopi_climate::analogues::SearchOptions,
) -> Result<ExitCode> {
    let mut g = Grid::open(grid)?;
    let site = g.site(lat, lon, DEFAULT_RADIUS, false)?;
    let Some(climate) = site.climate else {
        eprintln!("no climate data near ({lat}, {lon})");
        return Ok(ExitCode::from(2));
    };
    let Some(period) = climate.periods.get(slot) else {
        return Err(PipelineError::Data(format!(
            "slot {slot} out of range (0..{})",
            climate.periods.len()
        )));
    };
    let srf = match (&site.monthly, &site.monthly_precip) {
        (Some(m), Some(p)) => {
            let m_slot = canopi_climate::monthly::MONTHLY_SLOT_INDICES
                .iter()
                .position(|&idx| idx == slot)
                .unwrap_or(0);
            f64::from(canopi_climate::pipeline::analogues::summer_rain_fraction(
                &m.periods[m_slot].tmean_c,
                &p.periods[m_slot].precip_mm,
            ))
        }
        _ => 0.5,
    };
    let query = canopi_climate::analogues::AnalogueQuery {
        lat,
        lon,
        tmean_cold_c: period.tmean_cold_c,
        tmean_warm_c: period.tmean_warm_c,
        tmin_cold_c: period.tmin_cold_c,
        tmax_warm_c: period.tmax_warm_c,
        precip_mm: period.precip_mm,
        aridity_index: period.aridity_index,
        dry_months: period.dry_months,
        summer_rain_frac: srf,
    };
    let bin_path = grid.join(canopi_climate::analogues::FILE_NAME);
    let bytes = std::fs::read(&bin_path).map_err(|e| PipelineError::io(&bin_path, e))?;
    let index = canopi_climate::analogues::AnalogueIndex::from_bytes(&bytes)
        .map_err(|e| PipelineError::Data(e.to_string()))?;
    let res = index.search(&query, &opts);
    let text =
        serde_json::to_string_pretty(&res).map_err(|e| PipelineError::Data(e.to_string()))?;
    println!("{text}");
    Ok(ExitCode::SUCCESS)
}

fn vectors_cmd(
    grid: &Path,
    points: &Path,
    output: Option<&Path>,
    check: Option<&Path>,
) -> Result<ExitCode> {
    let text = std::fs::read_to_string(points).map_err(|e| PipelineError::io(points, e))?;
    let pts = vectors::read_points(&text)?;
    let fresh = vectors::vectors(&mut Grid::open(grid)?, &pts, DEFAULT_RADIUS)?;
    let Some(problem) = vectors::write_or_check(&fresh, output, check)? else {
        return Ok(ExitCode::SUCCESS);
    };
    let committed = check.unwrap_or(Path::new("?")).display();
    eprintln!(
        "{committed} is stale ({problem}); regenerate: canopi-climate vectors --grid {} --points {} -o {committed}",
        grid.display(),
        points.display(),
    );
    Ok(ExitCode::FAILURE)
}

fn list_sources(verify: Option<PathBuf>) -> Result<ExitCode> {
    for s in sources::SOURCES {
        let optional = if s.required { "" } else { " [optional check]" };
        println!("# {} ({}){optional}", s.title, s.licence);
        for f in s.files {
            println!("{}  {}  {}", f.sha256, f.path, f.url);
        }
    }
    if let Some(dir) = verify {
        let problems = sources::verify(&dir, false)?;
        for p in &problems {
            eprintln!("{p}");
        }
        if !problems.is_empty() {
            return Ok(ExitCode::FAILURE);
        }
        eprintln!("all source files match");
    }
    Ok(ExitCode::SUCCESS)
}

fn sample(
    points: &Path,
    grid: &Path,
    output: Option<&Path>,
    opts: &SampleOptions,
) -> Result<ExitCode> {
    let mut g = Grid::open(grid)?;
    let input: Box<dyn std::io::Read> = if points.as_os_str() == "-" {
        Box::new(std::io::stdin().lock())
    } else {
        Box::new(std::fs::File::open(points).map_err(|e| PipelineError::io(points, e))?)
    };
    let output: Box<dyn std::io::Write> = match output {
        Some(p) => Box::new(std::io::BufWriter::new(
            std::fs::File::create(p).map_err(|e| PipelineError::io(p, e))?,
        )),
        None => Box::new(std::io::stdout().lock()),
    };
    let s = sample_csv_with(&mut g, input, output, opts)?;
    eprintln!(
        "{} points: {} with data, {} without data nearby, {} invalid",
        s.points, s.ok, s.no_data, s.invalid
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::Parser;

    #[test]
    fn build_grid_needs_sources() {
        let parsed = Cli::try_parse_from(["canopi-climate", "build-grid", "--out", "o"]);
        if std::env::var_os("CANOPI_CLIMATE_SOURCES").is_some() {
            assert!(parsed.is_ok(), "the environment variable stands in");
            return;
        }
        let Err(e) = parsed else {
            panic!("build-grid without --sources must be refused");
        };
        let text = e.to_string();
        assert!(text.contains("--sources"), "{text}");
        let help = Cli::try_parse_from(["canopi-climate", "build-grid", "--help"])
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(help.contains("CANOPI_CLIMATE_SOURCES"), "{help}");
        assert!(!help.contains("/media/"), "no default path: {help}");
        let ok = Cli::try_parse_from([
            "canopi-climate",
            "build-grid",
            "--sources",
            "s",
            "--out",
            "o",
        ]);
        assert!(ok.is_ok());
    }
}
