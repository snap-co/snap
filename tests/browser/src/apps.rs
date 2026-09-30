//! App-owned native browser journeys for Authy and Chatty.
//!
//! Each journey file lives beside its application at
//! `apps/<app>/tests/browser/journeys.rs` so Cargo never discovers it as an
//! integration-test target; it is pulled in here with `#[path]` and stays
//! owned by the host-only `snap-browser-tests` runner, outside the fast
//! portable gates. `run` delegates `authy`/`chatty` suites (honoring
//! `--filter`) to those app cases.

use anyhow::{Context, Result, ensure};
use chromiumoxide::{Browser, Page};

#[path = "../../../apps/authy/tests/browser/journeys.rs"]
mod authy;
#[path = "../../../apps/chatty/tests/browser/journeys.rs"]
mod chatty;

/// Runs the `authy` or `chatty` app journeys whose case names contain
/// `filter` (empty matches all).
pub async fn run(browser: &Browser, suite: &str, filter: &str) -> Result<()> {
    match suite {
        "authy" => authy::run(browser, filter).await,
        "chatty" => chatty::run(browser, filter).await,
        other => anyhow::bail!("unknown app browser suite {other}"),
    }
}

pub(crate) fn matches(filter: &str, name: &str) -> bool {
    filter.is_empty() || name.contains(filter)
}

/// Percent-encodes query pairs without new dependencies.
pub(crate) fn encode_query(pairs: &[(&str, &str)]) -> String {
    fn enc(value: &str) -> String {
        let mut out = String::new();
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                out.push(byte as char);
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
        out
    }
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", enc(key), enc(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Reads a raw query value from a URL without new dependencies.
pub(crate) fn query_value(url: &str, key: &str) -> Option<String> {
    url.split('?').nth(1)?.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == key).then(|| value.to_owned())
    })
}

/// Decodes `application/x-www-form-urlencoded` query values.
pub(crate) fn decode_query(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                match u8::from_str_radix(&value[index + 1..index + 3], 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        index += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) fn unique(prefix: &str) -> String {
    format!(
        "{prefix}-{}@example.test",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("Unix clock")
            .as_nanos()
    )
}

pub(crate) fn http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?)
}

/// Waits until the page URL contains `needle`, polling over CDP so no page
/// JavaScript is required.
pub(crate) async fn wait_url_contains(
    page: &Page,
    needle: &str,
    description: &str,
) -> Result<String> {
    let start = std::time::Instant::now();
    loop {
        let url = page.url().await?.unwrap_or_default();
        if url.contains(needle) {
            return Ok(url);
        }
        anyhow::ensure!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "waiting for URL {description}, observed {url}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Waits until the page URL no longer contains `needle`.
pub(crate) async fn wait_url_absent(
    page: &Page,
    needle: &str,
    description: &str,
) -> Result<String> {
    let start = std::time::Instant::now();
    loop {
        let url = page.url().await?.unwrap_or_default();
        if !url.contains(needle) {
            return Ok(url);
        }
        anyhow::ensure!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "waiting for URL without {description}, observed {url}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Waits until the page body's text contains `needle`.
pub(crate) async fn contains_text(ui: &crate::ui::Ui, needle: &str) -> Result<()> {
    ui.wait(
        &format!(
            "document.body?.textContent.includes({}) ?? false",
            crate::ui::js(needle)
        ),
        serde_json::json!(true),
    )
    .await
}

/// Reads all cookies visible to `url`, including HttpOnly session cookies.
pub(crate) async fn cookies(
    page: &Page,
    url: &str,
) -> Result<Vec<chromiumoxide::cdp::browser_protocol::network::Cookie>> {
    use chromiumoxide::cdp::browser_protocol::network::GetCookiesParams;
    let response = page
        .execute(GetCookiesParams::builder().url(url.to_owned()).build())
        .await?;
    Ok(response.result.cookies)
}

/// Sets a real-device viewport through CDP emulation (no page cooperation).
pub(crate) async fn set_viewport(page: &Page, width: i64, height: i64, mobile: bool) -> Result<()> {
    use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
    page.execute(
        SetDeviceMetricsOverrideParams::builder()
            .width(width)
            .height(height)
            .device_scale_factor(1)
            .mobile(mobile)
            .build()
            .map_err(|error| anyhow::anyhow!(error))?,
    )
    .await?;
    Ok(())
}

/// Saves a full-page snapshot when `--artifacts` is set, otherwise skips.
/// Chromiumoxide clears the emulation override after a full-page capture, so
/// the requested viewport is (re-)applied before and restored after every
/// shot; callers always know the live viewport afterwards.
pub(crate) async fn screenshot(
    page: &Page,
    name: &str,
    width: i64,
    height: i64,
    mobile: bool,
) -> Result<()> {
    set_viewport(page, width, height, mobile).await?;
    if let Some(directory) = crate::support::artifacts() {
        page.save_screenshot(
            chromiumoxide::page::ScreenshotParams::builder()
                .full_page(true)
                .build(),
            directory.join(format!("{name}.png")),
        )
        .await
        .with_context(|| format!("capturing {name}"))?;
        set_viewport(page, width, height, mobile).await?;
    }
    Ok(())
}

/// Clicks an XPath match with real mouse input but without executing any page
/// JavaScript (no scroll or stability reads), for pages running with script
/// execution disabled. Resolution uses the DOM domain and the click point
/// comes from content quads. Layout is polled until the bounding box settles
/// so the click cannot chase a shifting button.
pub(crate) async fn raw_click(page: &Page, xpath: &str) -> Result<()> {
    use chromiumoxide::cdp::browser_protocol::input::{
        DispatchMouseEventParams, DispatchMouseEventType, MouseButton,
    };
    let element = page.find_xpath(xpath.to_owned()).await?;
    let start = std::time::Instant::now();
    let mut settled: Option<(u64, u64, u64, u64)> = None;
    loop {
        let current = match element.bounding_box().await {
            Ok(rect) => Some((
                rect.x.to_bits(),
                rect.y.to_bits(),
                rect.width.to_bits(),
                rect.height.to_bits(),
            )),
            Err(_) => None,
        };
        if current.is_some() && current == settled {
            break;
        }
        settled = current;
        ensure!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "button never settled: {xpath}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let point = element.clickable_point().await?;
    for kind in [
        DispatchMouseEventType::MouseMoved,
        DispatchMouseEventType::MousePressed,
        DispatchMouseEventType::MouseReleased,
    ] {
        page.execute(
            DispatchMouseEventParams::builder()
                .r#type(kind)
                .x(point.x)
                .y(point.y)
                .button(MouseButton::Left)
                .click_count(1)
                .build()
                .map_err(|error| anyhow::anyhow!(error))?,
        )
        .await?;
    }
    Ok(())
}

/// Submits a scriptless form once and waits for a new URL containing `needle`.
/// Never replay a submission whose effect may already have been accepted.
pub(crate) async fn submit_until_navigated(
    page: &Page,
    xpath: &str,
    needle: &str,
    description: &str,
) -> Result<String> {
    let before = page.url().await?.unwrap_or_default();
    ensure!(
        !before.contains(needle),
        "stale URL already matches {description}: {before}"
    );
    let mut observed = before;
    raw_click(page, xpath).await?;
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(15) {
        observed = page.url().await?.unwrap_or_default();
        if observed.contains(needle) {
            return Ok(observed);
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    anyhow::bail!("waiting for {description}, observed {observed}")
}
