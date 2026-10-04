//! Factorio browser adapter. The journey lives beside the application tests
//! and is compiled here so it stays out of the portable fast gates.

#[path = "../../../factorio/tests/browser/journeys.rs"]
mod journeys;

pub async fn run(
    browser: &chromiumoxide::Browser,
    suite: &str,
    filter: &str,
) -> anyhow::Result<()> {
    journeys::run(browser, suite, filter).await
}
