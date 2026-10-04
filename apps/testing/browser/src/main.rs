use anyhow::{Result, bail};
use chromiumoxide::Browser;
use clap::Parser;
use futures::future::LocalBoxFuture;
use snap_browser_tests::runner;

mod apps;
mod development;
mod factorio;
mod hosts;
use snap_app_browser_tests::support;
#[path = "../../../testy/tests/browser/journeys.rs"]
mod testy;
use snap_browser_tests::ui;

fn dispatch<'a>(
    browser: &'a Browser,
    suite: &'a str,
    filter: &'a str,
) -> LocalBoxFuture<'a, Result<()>> {
    Box::pin(async move {
        match suite {
            "testy" => testy::run(browser, filter).await,
            "authy" | "chatty" => apps::run(browser, suite, filter).await,
            "factorio" | "factorio-dev" => factorio::run(browser, suite, filter).await,
            "dev" | "authy-dev" | "chatty-dev" | "dev-origins" => {
                development::run(browser, suite, filter).await
            }
            _ => bail!("unknown application browser suite {suite}"),
        }
    })
}

async fn prepare() -> Result<()> {
    let root = support::root();
    let mut command = support::command("cargo");
    command.current_dir(&root).args([
        "build",
        "-p",
        "snap-cli",
        "-p",
        "snap-app-browser-tests",
        "-p",
        "authy-server",
        "-p",
        "chatty-server",
        "-p",
        "factorio-server",
        "-p",
        "testy-server",
        "--features",
        "testy-server/web",
    ]);
    support::checked(&mut command, 120).await?;
    for app in ["testy", "authy", "chatty", "factorio"] {
        let mut command = support::command(root.join("target/debug/snap"));
        command
            .current_dir(&root)
            .args(["build", "--project"])
            .arg(format!("apps/{app}"))
            .args(["--web-only", "--output"])
            .arg(root.join(format!("apps/{app}/dist/development/clients/web")));
        support::checked(&mut command, 120).await?;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    runner::run(
        runner::Args::parse(),
        &[
            "testy",
            "authy",
            "chatty",
            "factorio",
            "dev",
            "authy-dev",
            "chatty-dev",
            "dev-origins",
            "factorio-dev",
        ],
        prepare(),
        dispatch,
    )
    .await
}
