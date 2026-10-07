use crate::{config::Project, process::Runner};
use anyhow::{Result, bail};
use std::path::PathBuf;
use tokio::process::Command;

const PLATFORMS: &[&str] = &["memory", "native", "workers", "browser", "full"];

/// A single positional selector is a platform or a project; two are project/platform.
pub fn selection(
    first: Option<String>,
    second: Option<String>,
) -> Result<(Option<PathBuf>, String)> {
    match (first, second) {
        (None, None) => Ok((None, "memory".into())),
        (Some(first), None) if PLATFORMS.contains(&first.as_str()) || first == "wasm" => {
            Ok((None, first))
        }
        (Some(first), platform) => Ok((Some(first.into()), platform.unwrap_or("memory".into()))),
        _ => unreachable!(),
    }
}

pub async fn run(project: Project, runner: &Runner, platform: &str) -> Result<()> {
    if platform == "wasm" {
        bail!("Wasm is not a host: select workers or browser explicitly");
    }
    let selected: Vec<_> = if platform == "full" {
        if project.config.test.is_empty() {
            bail!("No test suites declared for {}", project.config.application);
        }
        PLATFORMS
            .iter()
            .filter(|name| project.config.test.contains_key(**name))
            .copied()
            .collect()
    } else {
        if !project.config.test.contains_key(platform) {
            bail!(
                "No {platform} test suite declared for {}; available: {}",
                project.config.application,
                project
                    .config
                    .test
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        vec![platform]
    };
    if platform == "full" {
        crate::check::run(Some(project.root.clone()), runner, false, false, false).await?;
    }
    for name in selected {
        let suite = &project.config.test[name];
        for args in &suite.commands {
            eprintln!("Testing {} ({name}): {args:?}", project.config.application);
            let mut command = Command::new(&args[0]);
            command.args(&args[1..]).current_dir(&project.root);
            command.env("SNAP_TEST_PLATFORM", name);
            for key in [
                "SNAP_CHECK_EXECUTABLE",
                "SNAP_CHECK_PACKAGE",
                "SNAP_CHECK_WEB_DIR",
            ] {
                command.env_remove(key);
            }
            runner.run(&mut command, false).await?;
        }
    }
    runner.check()?;
    println!("Tests passed: {} ({platform})", project.config.application);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn positional_selection_defaults_to_memory() {
        assert_eq!(selection(None, None).unwrap(), (None, "memory".into()));
        assert_eq!(
            selection(Some("native".into()), None).unwrap(),
            (None, "native".into())
        );
        assert_eq!(
            selection(Some("apps/testy".into()), None).unwrap(),
            (Some("apps/testy".into()), "memory".into())
        );
        assert_eq!(
            selection(Some("apps/testy".into()), Some("full".into())).unwrap(),
            (Some("apps/testy".into()), "full".into())
        );
    }
}
