use crate::{config::Project, process::Runner};
use anyhow::{Context, Result};
use tokio::process::Command;

pub async fn run(project: Project, runner: &Runner, development: bool) -> Result<()> {
    let (name, workflow) = if development {
        ("dev", &project.config.dev)
    } else {
        ("build", &project.config.build)
    };
    let workflow = workflow.as_ref().with_context(|| {
        format!(
            "No {name} workflow declared for {}",
            project.config.application
        )
    })?;
    for args in &workflow.commands {
        eprintln!("{} {name}: {args:?}", project.config.application);
        runner
            .run(
                Command::new(&args[0])
                    .args(&args[1..])
                    .current_dir(&project.root),
                false,
            )
            .await?;
    }
    runner.check()
}
