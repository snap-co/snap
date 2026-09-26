//! Local developer tooling. Application execution stays in the selected host executable.
mod architecture;
mod build;
mod cargo;
mod check;
mod config;
mod dev;
mod migrate;
mod process;
mod watch;

use clap::{Parser, Subcommand};
use std::{path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(
    name = "snap",
    version,
    about = "Build and run local Snap applications"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create or apply explicit Store schema migrations
    Migrate(migrate::Args),
    /// Verify the selected project's Rust packages and declared consumer checks
    Check {
        /// Start discovery here instead of the current directory
        project: Option<PathBuf>,
        /// Only dependency-direction and portable-target checks
        #[arg(long)]
        structure_only: bool,
        /// Include all workspace packages in structural checks (repository gate)
        #[arg(long, requires = "structure_only")]
        workspace: bool,
    },
    /// Build a runnable package for the nearest snap.toml project
    Build {
        /// Start discovery here instead of the current directory
        project: Option<PathBuf>,
        /// Optimize native, WASM, and browser code
        #[arg(long)]
        release: bool,
    },
    /// Build, watch, and run the nearest snap.toml project
    Dev {
        /// Start discovery here instead of the current directory
        project: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = async {
        let runner = process::Runner::new()?;
        match cli.command {
            Command::Migrate(args) => migrate::run(args),
            Command::Check {
                project,
                structure_only,
                workspace,
            } => {
                check::run(
                    config::Project::discover(project)?,
                    &runner,
                    structure_only,
                    workspace,
                )
                .await
            }
            Command::Build { project, release } => {
                let project = config::Project::discover(project)?;
                let profile = if release {
                    build::Profile::Release
                } else {
                    build::Profile::Debug
                };
                let artifacts = build::run(&project, &runner, profile).await?;
                println!("Package: {}", artifacts.directory.display());
                println!("Executable: {}", artifacts.executable.display());
                if let Some(web) = artifacts.web {
                    println!("Web assets: {}", web.display());
                }
                Ok(())
            }
            Command::Dev { project } => {
                dev::run(config::Project::discover(project)?, &runner).await
            }
        }
    }
    .await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("snap: {error:#}");
            ExitCode::from(
                error
                    .downcast_ref::<process::Failed>()
                    .map_or(1, |error| error.0),
            )
        }
    }
}
