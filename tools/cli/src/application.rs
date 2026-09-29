use crate::{config::Project, process::Runner};
use anyhow::{Context, Result};
use tokio::process::Command;

pub async fn run(
    project: Project,
    runner: &Runner,
    config: Option<std::path::PathBuf>,
) -> Result<()> {
    let (name, workflow) = ("dev", &project.config.dev);
    let workflow = workflow.as_ref().with_context(|| {
        format!(
            "No {name} workflow declared for {}",
            project.config.application
        )
    })?;
    for args in &workflow.commands {
        eprintln!("{} {name}: {args:?}", project.config.application);
        let mut command = Command::new(&args[0]);
        command.args(&args[1..]).current_dir(&project.root);
        command.env("SNAP_CLI", std::env::current_exe()?);
        if let Some(path) = &config {
            command.env("SNAP_CONFIG", path.canonicalize()?);
        }
        runner.run(&mut command, false).await?;
    }
    runner.check()
}
