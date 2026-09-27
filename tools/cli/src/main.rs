//! Local developer tooling. Application execution stays in the selected host executable.
mod application;
mod architecture;
mod cargo;
mod check;
mod config;
mod migrate;
mod process;
mod test;

use clap::{Parser, Subcommand};
use std::{path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(
    name = "snap",
    version,
    about = "Build, develop and check Snap applications"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build the application's configured artifacts
    Build { project: Option<PathBuf> },
    /// Run the application's development server and reload workflow
    Dev { project: Option<PathBuf> },
    /// Create or apply explicit Store schema migrations
    Migrate(migrate::Args),
    /// Run a project's tests; defaults to the memory platform
    Test {
        /// Project directory or platform (memory, native, workers, browser, full)
        project_or_platform: Option<String>,
        /// Platform when a project directory is supplied
        platform: Option<String>,
    },
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
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = async {
        let runner = process::Runner::new()?;
        match cli.command {
            Command::Build { project } => {
                application::run(config::Project::discover(project)?, &runner, false).await
            }
            Command::Dev { project } => {
                application::run(config::Project::discover(project)?, &runner, true).await
            }
            Command::Migrate(args) => migrate::run(args),
            Command::Test {
                project_or_platform,
                platform,
            } => {
                let (project, platform) = test::selection(project_or_platform, platform)?;
                test::run(config::Project::discover(project)?, &runner, &platform).await
            }
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
