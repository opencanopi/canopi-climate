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
    /// Look one point up and print JSON: `climate`, `extremes`, `monthly`,
    /// `monthly_precip`, `soil` and `regions` (each `null` when absent;
    /// exit code 2 when all are).
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
        } => {
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
    }
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
