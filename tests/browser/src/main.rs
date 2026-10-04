use anyhow::{Result, bail};
use chromiumoxide::Browser;
use clap::Parser;
use futures::future::LocalBoxFuture;
use snap_browser_tests::{client, react, runner};

fn dispatch<'a>(
    browser: &'a Browser,
    suite: &'a str,
    filter: &'a str,
) -> LocalBoxFuture<'a, Result<()>> {
    Box::pin(async move {
        match suite {
            "client" => client::run(browser, filter).await,
            "react" => {
                let name = "an identity change withholds old loader data until new-account onboarding is ready";
                if !filter.is_empty() && !name.contains(filter) && !suite.contains(filter) {
                    bail!("no {suite} journeys match {filter:?}");
                }
                react::run(browser).await
            }
            _ => bail!("unknown framework browser suite {suite}"),
        }
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    // BundleHost compiles the framework fixtures themselves. There are no app
    // assets or application hosts to prepare for these adapter contracts.
    runner::run(
        runner::Args::parse(),
        &["client", "react"],
        async { Ok(()) },
        dispatch,
    )
    .await
}
