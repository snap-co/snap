//! Local developer tooling. Application execution stays in the selected host executable.
mod build;
mod config;
mod dev;
mod process;

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
    /// Build a runnable package for the nearest snap.toml project
    Build {
        /// Start discovery here instead of the current directory
        project: Option<PathBuf>,
        /// Optimize native, WASM, and browser code
        #[arg(long)]
        release: bool,
    },
    /// Build and run the nearest snap.toml project (no file watching yet)
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
            Command::Build { project, release } => {
                let project = config::Project::discover(project)?;
                let profile = if release {
                    build::Profile::Release
                } else {
                    build::Profile::Debug
                };
                let artifacts = build::run(&project, &runner, build::Mode::Build(profile)).await?;
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
