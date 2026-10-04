//! A held account loader must not expose the preceding account's rendered data.
//! After publication, physical disconnect must retain the selected repository.

use anyhow::{Context, Result};
use chromiumoxide::Browser;
use std::time::Duration;
use tokio::time::{sleep, timeout};

/// Run the held-loader journey through the framework browser consumer.
pub async fn run(browser: &Browser) -> Result<()> {
    let fixture = crate::support::root().join("kits/react/tests/session-fixture.tsx");
    let host = crate::support::BundleHost::start(&fixture).await?;

    let session = crate::ui::Session::new(browser).await?;
    let result: Result<()> = async {
        let ui = &session.ui;
        ui.goto(&host.base).await?;
        // Owner A is the initial converged view.
        ui.text("Owner A").visible().await?;
        ui.eval("window.switchStart()").await?;
        ui.heading("Opening your workspace").visible().await?;
        ui.eval("window.switchReady()").await?;
        // The new account's loader holds; the old owner's data must stay hidden
        // until onboarding can mount. Wait for the held marker with a bounded
        // timeout rather than the shared 10s UI wait.
        timeout(Duration::from_secs(10), async {
            loop {
                let value = ui.eval("document.documentElement.dataset.loader ?? null").await?;
                if value == serde_json::json!("held") {
                    return Ok::<_, anyhow::Error>(());
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("waiting for held loader marker")??;
        // Observe a rendered frame, not just the synchronous state transition.
        // Two requestAnimationFrames guarantee the withheld view has painted.
        ui.eval("new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve(1))))")
            .await?;
        // Old-owner secrecy: Owner A must not be visible while held, and the
        // loading placeholder must render exactly once.
        ui.text("Owner A").hidden().await?;
        ui.heading("Opening your workspace").count(1).await?;
        ui.eval("window.release()").await?;
        // New-account onboarding mounts with its repository choices.
        ui.xpath("//option[normalize-space(.) = 'repository']").count(1).await?;
        ui.text("Owner B").count(1).await?;
        // Disconnect retention: the selected repository survives connection loss.
        ui.eval("window.disconnect()").await?;
        timeout(Duration::from_secs(10), async {
            loop {
                let value = ui
                    .eval("document.querySelector('select[aria-label=\"Repository\"]')?.value ?? null")
                    .await?;
                if value == serde_json::json!("repository") {
                    return Ok::<_, anyhow::Error>(());
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("waiting for retained repository selection")??;
        Ok(())
    }
    .await;
    session.finish(result).await
}
