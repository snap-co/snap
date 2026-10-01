use anyhow::{Context, Result, anyhow, bail};
use chromiumoxide::{Browser, BrowserConfig, handler::viewport::Viewport};
use clap::Parser;
use futures::{FutureExt, StreamExt};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::time::timeout;

#[derive(Parser)]
#[command(
    about = "Native Rust/CDP browser journeys. Build web assets and application hosts first."
)]
struct Args {
    /// Compile native hosts and frontend assets before executing journeys
    #[arg(long)]
    prepare: bool,
    /// testy, authy, chatty, factorio, client, react, dev, authy-dev, chatty-dev, dev-origins, factorio-dev, or all
    #[arg(default_value = "all")]
    suite: String,
    /// Installed Chromium executable. No browser download or foreign controller.
    #[arg(long, env = "SNAP_BROWSER", default_value = "chromium")]
    browser: PathBuf,
    /// Run only journeys whose names contain this string.
    #[arg(long, default_value = "")]
    filter: String,
    #[arg(long, env = "SNAP_BROWSER_ARTIFACTS")]
    artifacts: Option<PathBuf>,
}

fn browser_executable(path: &Path) -> Result<PathBuf> {
    let candidate = if path.components().count() == 1 {
        std::env::var_os("PATH").and_then(|paths|std::env::split_paths(&paths).map(|directory|directory.join(path)).find(|candidate|candidate.is_file()))
            .with_context(||format!("Chromium executable {} not found on PATH; install Chromium or set SNAP_BROWSER",path.display()))?
    } else {
        path.to_owned()
    };
    candidate
        .canonicalize()
        .with_context(|| format!("Chromium executable {} is unavailable", candidate.display()))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.suite == "all" && !args.filter.is_empty() {
        bail!("select a specific suite when using --filter");
    }
    if args.prepare {
        tokio::select! {
            result = prepare() => result?,
            signal = tokio::signal::ctrl_c() => { signal?; bail!("browser preparation interrupted"); }
        }
    }
    let executable = browser_executable(&args.browser)?;
    if let Some(path) = args.artifacts.as_ref() {
        std::fs::create_dir_all(path)?;
        snap_browser_tests::support::set_artifacts(path.clone());
    }
    let start = Instant::now();
    // Chromium creates a SingletonSocket below TMPDIR. Nesting it below a long
    // checkout/fixture path can exceed Linux's 108-byte Unix socket limit.
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache")
        })
        .join("coding-agents");
    std::fs::create_dir_all(&cache)?;
    let profile = tempfile::Builder::new().prefix("cdp-").tempdir_in(&cache)?;
    let config = BrowserConfig::builder()
        .chrome_executable(executable)
        .user_data_dir(profile.path().join("profile"))
        .env("TMPDIR", profile.path().to_string_lossy().into_owned())
        .env(
            "XDG_CONFIG_HOME",
            profile.path().join("config").to_string_lossy().into_owned(),
        )
        .env(
            "XDG_CACHE_HOME",
            profile.path().join("cache").to_string_lossy().into_owned(),
        )
        .new_headless_mode()
        .no_sandbox()
        .arg("no-startup-window")
        .env("SNAP_MASTER_KEY", "")
        .viewport(Viewport {
            width: 1280,
            height: 720,
            ..Default::default()
        })
        .request_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| anyhow!(e))?;
    let (mut browser, mut handler) = Browser::launch(config).await?;
    let mut pump = tokio::spawn(async move {
        while let Some(event) = handler.next().await {
            event?;
        }
        Ok::<_, chromiumoxide::error::CdpError>(())
    });
    let cases = async {
        println!(
            "Chromium {} launched in {:?}",
            browser.version().await?.product,
            start.elapsed()
        );
        let suites: Vec<&str> = if args.suite == "all" {
            vec![
                "testy",
                "authy",
                "chatty",
                "factorio",
                "react",
                "client",
                "dev",
                "authy-dev",
                "chatty-dev",
                "dev-origins",
                "factorio-dev",
            ]
        } else {
            vec![&args.suite]
        };
        for suite in suites {
            let start = Instant::now();
            let run = async {
                match suite {
                    "client" => snap_browser_tests::client::run(&browser, &args.filter).await,
                    "testy" => snap_browser_tests::testy::run(&browser, &args.filter).await,
                    "authy" => snap_browser_tests::apps::run(&browser, "authy", &args.filter).await,
                    "chatty" => {
                        snap_browser_tests::apps::run(&browser, "chatty", &args.filter).await
                    }
                    "factorio" | "factorio-dev" => {
                        snap_browser_tests::factorio::run(&browser, suite, &args.filter).await
                    }
                    "react" | "dev" | "authy-dev" | "chatty-dev" | "dev-origins" => {
                        snap_browser_tests::development::run(&browser, suite, &args.filter).await
                    }
                    _ => bail!("unknown browser suite {suite}"),
                }
            };
            timeout(Duration::from_secs(900), run)
                .await
                .context("browser suite timed out")?
                .with_context(|| format!("{suite} browser suite"))?;
            println!("PASS {suite} {:?}", start.elapsed());
        }
        Ok::<_, anyhow::Error>(())
    };
    let cases = std::panic::AssertUnwindSafe(cases).catch_unwind();
    let result = tokio::select! {
        result=cases => match result {Ok(result)=>result,Err(payload)=>Err(anyhow!("browser assertion panicked: {}",payload.downcast_ref::<String>().map(String::as_str).or_else(||payload.downcast_ref::<&str>().copied()).unwrap_or("non-string panic")))},
        result=&mut pump => Err(anyhow!("CDP handler stopped: {result:?}")),
        signal=tokio::signal::ctrl_c()=>signal.context("listening for interruption").and_then(|()|Err(anyhow!("browser journeys interrupted"))),
    };
    if result.is_err()
        && let Some(path) = args.artifacts.as_ref()
    {
        for (index, page) in browser.pages().await.unwrap_or_default().iter().enumerate() {
            let _ = timeout(
                Duration::from_secs(2),
                page.save_screenshot(
                    chromiumoxide::page::ScreenshotParams::default(),
                    path.join(format!("failure-{index}.png")),
                ),
            )
            .await;
            if let Ok(Ok(html)) = timeout(Duration::from_secs(2), page.content()).await {
                let _ = std::fs::write(path.join(format!("failure-{index}.html")), html);
            }
        }
        if let Err(error) = std::fs::write(path.join("failure.txt"), format!("{result:?}")) {
            eprintln!("Could not write failure artifact: {error}");
        }
    }
    let close = timeout(Duration::from_secs(5), async {
        browser.close().await?;
        browser.wait().await?;
        Ok::<_, anyhow::Error>(())
    })
    .await;
    let killed = if !matches!(close, Ok(Ok(()))) {
        browser.kill().await
    } else {
        None
    };
    if !pump.is_finished() {
        pump.abort();
        let _ = pump.await;
    }
    result?;
    if let Some(killed) = killed {
        killed?;
    }
    if !matches!(close, Ok(Ok(()))) {
        bail!("browser required forced shutdown: {close:?}");
    }
    Ok(())
}

async fn prepare() -> Result<()> {
    use snap_browser_tests::support;
    let root = support::root();
    let mut command = support::command("cargo");
    command.current_dir(&root).args([
        "build",
        "-p",
        "snap-cli",
        "-p",
        "snap-browser-tests",
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
