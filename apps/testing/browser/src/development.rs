//! Development supervisor and origin-policy journeys through real Chromium.
//! Source mutations stay inside SourceCopy; process guards reap supervisors
//! before removing their data. Log-count waits distinguish retained generations
//! from successful rebuilds. The React kit owns its separate rendered-loader case.

use anyhow::{Context, Result, anyhow, ensure};
use chromiumoxide::Browser;
use futures::{FutureExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::{sleep, timeout},
};

use snap_browser_tests::react as react_router;

const DEV_TIMEOUT: u64 = 60;

/// Public entrypoint wired by `main.rs`. `suite` is one of `dev`,
/// `authy-dev`, `chatty-dev`, `dev-origins` or `react`. `filter` selects
/// journeys whose name or suite contains the substring; empty runs all.
pub async fn run(browser: &Browser, suite: &str, filter: &str) -> Result<()> {
    match suite {
        "dev" => {
            if selected(
                "dev publishes real generations, hot-reloads frontend and tooling, and retains invalid changes",
                suite,
                filter,
            ) {
                dev_journey(browser).await?;
            } else {
                anyhow::bail!("no {suite} journeys match {filter:?}");
            }
            Ok(())
        }
        "authy-dev" => {
            if selected(
                "Authy dev retains failed builds and sessions across native/Wasm replacement",
                suite,
                filter,
            ) {
                authy_dev_journey(browser).await?;
            } else {
                anyhow::bail!("no {suite} journeys match {filter:?}");
            }
            Ok(())
        }
        "chatty-dev" => {
            if selected(
                "Chatty dev retains OAuth sessions and threads across failed builds and native/Wasm replacement",
                suite,
                filter,
            ) {
                chatty_dev_journey(browser).await?;
            } else {
                anyhow::bail!("no {suite} journeys match {filter:?}");
            }
            Ok(())
        }
        "dev-origins" => {
            if selected(
                "network dev preserves alias callbacks and logout, blocks spoofed hosts and origins, and serves HMR",
                suite,
                filter,
            ) {
                dev_origins_journey(browser).await?;
            } else {
                anyhow::bail!("no {suite} journeys match {filter:?}");
            }
            Ok(())
        }
        "react" => {
            if selected(
                "an identity change withholds old loader data until new-account onboarding is ready",
                suite,
                filter,
            ) {
                react_router::run(browser).await?;
            } else {
                anyhow::bail!("no {suite} journeys match {filter:?}");
            }
            Ok(())
        }
        _ => Err(anyhow!("unknown development suite {suite}")),
    }
}

fn selected(name: &str, suite: &str, filter: &str) -> bool {
    filter.is_empty() || name.contains(filter) || suite.contains(filter)
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

fn parse_dev_urls(log: &str, title: &str) -> Result<(String, String)> {
    let frontend_prefix = format!("{title} dev ");
    let backend_prefix = format!("{title} ");
    let mut frontend: Option<String> = None;
    let mut backend: Option<String> = None;
    for line in log.lines() {
        if let Some(rest) = line
            .find(&frontend_prefix)
            .map(|i| &line[i + frontend_prefix.len()..])
        {
            if let Some(url) = rest.split_whitespace().find(|t| t.starts_with("http://")) {
                frontend = Some(url.trim_end_matches([',', ')']).to_owned());
            }
        } else if let Some(rest) = line
            .find(&backend_prefix)
            .map(|i| &line[i + backend_prefix.len()..])
        {
            // Backend lines are "{Title} http://..."; frontend lines contain
            // "dev http" and are handled above, so skip any residual dev token.
            if rest.trim_start().starts_with("dev ") {
                continue;
            }
            if let Some(url) = rest.split_whitespace().find(|t| t.starts_with("http://")) {
                backend.get_or_insert(url.trim_end_matches([',', ')']).to_owned());
            }
        }
    }
    let frontend = frontend.with_context(|| format!("missing {title} dev url in logs:\n{log}"))?;
    let backend =
        backend.with_context(|| format!("missing {title} backend url in logs:\n{log}"))?;
    Ok((frontend, backend))
}

async fn wait_log_count(
    proc_: &mut crate::support::Process,
    needle: &str,
    previous: usize,
    seconds: u64,
) -> Result<()> {
    timeout(Duration::from_secs(seconds), async {
        loop {
            proc_.alive()?;
            if count_occurrences(&proc_.log(), needle) > previous {
                return Ok(());
            }
            sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .with_context(|| format!("waiting for new {needle}: {}", proc_.log()))?
}

async fn wait_http_ok(url: &str, seconds: u64) -> Result<()> {
    let client = crate::support::client()?;
    timeout(Duration::from_secs(seconds), async {
        loop {
            if client
                .get(url)
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                return Ok(());
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .with_context(|| format!("waiting for HTTP ok {url}"))?
}

async fn wait_header(url: &str, header: &str, expected: &str, seconds: u64) -> Result<()> {
    let client = crate::support::client()?;
    timeout(Duration::from_secs(seconds), async {
        loop {
            if let Ok(response) = client.get(url).send().await
                && response.headers().get(header).and_then(|v| v.to_str().ok()) == Some(expected)
            {
                return Ok(());
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .with_context(|| format!("waiting for header {header}={expected} at {url}"))?
}

async fn wait_eval_eq(
    ui: &crate::ui::Ui,
    expression: &str,
    expected: Value,
    seconds: u64,
) -> Result<()> {
    ui.wait_for(expression, expected, Duration::from_secs(seconds))
        .await
}

async fn wait_transcript(ui: &crate::ui::Ui, text: &str, seconds: u64) -> Result<()> {
    let literal = serde_json::to_string(text).unwrap();
    wait_eval_eq(
        ui,
        &format!(
            "(() => {{const n=Array.from(document.querySelectorAll('.transcript .user-message p')).filter(e => e.textContent.includes({literal}));return n.length===1 && n[0].getClientRects().length>0 && getComputedStyle(n[0]).visibility!=='hidden';}})()"
        ),
        json!(true),
        seconds,
    )
    .await
}

fn assert_exit_143(status: std::process::ExitStatus, logs: &str) -> Result<()> {
    use std::os::unix::process::ExitStatusExt;
    ensure!(
        status.code() == Some(143),
        "expected dev exit code 143, got code {:?} signal {:?}:\n{logs}",
        status.code(),
        status.signal(),
    );
    Ok(())
}

async fn fetch_status(
    url: &str,
    headers: &[(&str, &str)],
) -> Result<(u16, reqwest::header::HeaderMap, String)> {
    let client = crate::support::client()?;
    let mut request = client.get(url);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    let map = response.headers().clone();
    let body = response.text().await.unwrap_or_default();
    Ok((status, map, body))
}

/// Raw HTTP status for spoofed Host headers, which reqwest would otherwise
/// normalize from the URL. Preserves the original foreign-Host rejection.
async fn raw_status(host_port: &str, path: &str, host: &str, origin: &str) -> Result<u16> {
    let stream = timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect(host_port),
    )
    .await
    .with_context(|| format!("connecting to {host_port}"))??;
    let (mut reader, mut writer) = stream.into_split();
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nConnection: close\r\n\r\n"
    );
    writer.write_all(request.as_bytes()).await?;
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).await?;
    let head = String::from_utf8_lossy(&buf);
    let line = head.lines().next().unwrap_or("");
    let code: u16 = line
        .split_whitespace()
        .nth(1)
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    Ok(code)
}

/// Raw WebSocket upgrade rejection. Fails if the server completes the upgrade.
async fn ws_rejected(base: &str, host: &str, origin: &str) -> Result<u16> {
    let url = reqwest::Url::parse(base)?;
    let host_port = format!(
        "{}:{}",
        url.host_str().unwrap_or("127.0.0.1"),
        url.port_or_known_default().unwrap_or(80)
    );
    let stream = timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect(&host_port),
    )
    .await
    .with_context(|| format!("connecting to {host_port}"))??;
    let (mut reader, mut writer) = stream.into_split();
    let request = format!(
        "GET /transport HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
    );
    writer.write_all(request.as_bytes()).await?;
    let mut buf = vec![0u8; 4096];
    let n = timeout(Duration::from_secs(5), reader.read(&mut buf))
        .await
        .context("WebSocket rejection stalled")??;
    let head = String::from_utf8_lossy(&buf[..n]);
    if head.contains("101") || head.to_lowercase().contains("upgrade") && head.contains("101") {
        anyhow::bail!("Spoofed WebSocket accepted: {head}");
    }
    let line = head.lines().next().unwrap_or("");
    let code: u16 = line
        .split_whitespace()
        .nth(1)
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    Ok(code)
}

// ---------------------------------------------------------------------------
// dev: Testy generations, HMR, retention, rollback, shutdown.
// ---------------------------------------------------------------------------

async fn dev_journey(browser: &Browser) -> Result<()> {
    // Disposable source copy; the new browser consumer is a workspace member so
    // the full source (including tests) is copied. Declared before processes so
    // Drops reap children before the directory can be removed.
    let source = crate::support::SourceCopy::new()?;
    let fixture = source.root.clone();
    let host_dir = fixture.join("host");
    let deployment = crate::support::Deployment::create(
        &host_dir,
        json!({"host": {"mode": "development", "listen": "127.0.0.1:0", "data_dir": fixture.to_string_lossy(), "database": "identity.sqlite"}, "app": {}}),
        None,
    )?;
    let setup_path = deployment.path.clone();
    let setup_key = deployment.key.clone();
    let setup_text = std::fs::read_to_string(&setup_path)?;
    // Explicit migration before dev startup, matching the original gate.
    {
        let mut cmd = crate::support::command(crate::support::root().join("target/debug/snap"));
        cmd.args([
            "migrate",
            "--database",
            &fixture.join("identity.sqlite").to_string_lossy(),
            "--migrations",
            &fixture.join("crates/identity/migrations").to_string_lossy(),
        ]);
        crate::support::run(&mut cmd)?;
    }
    let mut child = {
        let mut cmd = crate::support::command(crate::support::root().join("target/debug/snap"));
        cmd.args([
            "dev",
            &fixture.join("apps/testy").to_string_lossy(),
            "--config",
            &setup_path.to_string_lossy(),
        ]);
        if let Some(key) = &setup_key {
            cmd.env("SNAP_MASTER_KEY", key);
        }
        crate::support::Process::start(&mut cmd)?
    };

    let session = crate::ui::Session::new(browser).await?;
    // Extra pageerror observer for the mid-journey errors assertion; the final
    // Session::finish assertion subsumes it but the original checks twice.
    let mid_errors = session
        .ui
        .page
        .event_listener::<chromiumoxide::cdp::js_protocol::runtime::EventExceptionThrown>()
        .await?;
    let mid_errors = std::sync::Mutex::new(mid_errors);
    let result: Result<()> = async {
        child.wait_log("Testy dev http", DEV_TIMEOUT).await?;
        let (url, backend_url) = parse_dev_urls(&child.log(), "Testy")?;
        wait_http_ok(&url, DEV_TIMEOUT).await?;
        let ui = &session.ui;
        ui.goto(&format!("{url}/calc")).await?;
        ui.label("Email").fill("dev@example.com").await?;
        ui.label("Password").fill("password123").await?;
        ui.button("Create account").click().await?;
        ui.text("Connected").visible().await?;
        ui.label("Operand").fill("12").await?;
        ui.button("+").click().await?;
        ui.locator("[data-testid=\"accumulator\"]")
            .text("12")
            .await?;

        // CSS hot reload without navigation or state loss.
        let stylesheet = fixture.join("apps/testy/web/style.css");
        let css = std::fs::read_to_string(&stylesheet)?;
        std::fs::write(
            &stylesheet,
            format!("{css}\nbody {{ --dev-probe: active; }}\n"),
        )?;
        wait_eval_eq(
            ui,
            "getComputedStyle(document.body).getPropertyValue('--dev-probe').trim()",
            json!("active"),
            DEV_TIMEOUT,
        )
        .await?;
        ui.locator("[data-testid=\"accumulator\"]")
            .text("12")
            .await?;
        let (dev_status, _, _) = fetch_status(
            &format!("{url}/__dev"),
            &[("Origin", "http://elsewhere.invalid")],
        )
        .await?;
        ensure!(
            dev_status == 403,
            "expected 403 for foreign dev origin, got {dev_status}"
        );

        // Private files are never served through /@fs.
        let private_dir = fixture.join("apps/testy/.deployment/development");
        std::fs::create_dir_all(&private_dir)?;
        std::fs::write(
            private_dir.join("secrets.key"),
            "fixture-private-key-do-not-serve",
        )?;
        std::fs::write(
            private_dir.join("secrets.toml"),
            "fixture-private-key-do-not-serve",
        )?;
        for path in [
            private_dir.join("secrets.key"),
            private_dir.join("secrets.toml"),
            fixture.join("identity.sqlite"),
        ] {
            let (status, _, body) =
                fetch_status(&format!("{url}/@fs{}", path.to_string_lossy()), &[]).await?;
            ensure!(
                status == 403,
                "expected 403 for private file {path:?}, got {status}"
            );
            ensure!(
                !body.contains("fixture-private-key-do-not-serve"),
                "private key leaked for {path:?}"
            );
        }

        // Failed Rust builds retain the previous generation.
        let source_path = fixture.join("apps/testy/src/lib.rs");
        let original = std::fs::read_to_string(&source_path)?;
        std::fs::write(
            &source_path,
            format!("{original}\ncompile_error!(\"dev gate deliberate failure\");\n"),
        )?;
        child
            .wait_log("Rebuild failed; previous generation retained", DEV_TIMEOUT)
            .await?;
        ui.button("Refresh").click().await?;
        ui.locator("[data-testid=\"accumulator\"]")
            .text("12")
            .await?;
        let ready = count_occurrences(&child.log(), "generation ready");
        std::fs::write(&source_path, &original)?;
        wait_log_count(&mut child, "generation ready", ready, DEV_TIMEOUT).await?;
        ui.text("Connected").visible().await?;
        wait_eval_eq(
            ui,
            "document.querySelector('[data-testid=\"accumulator\"]')?.textContent.trim() ?? null",
            json!("0"),
            DEV_TIMEOUT,
        )
        .await?;

        // Component edits are React refresh, not document navigation.
        ui.eval("window.__devDocument = 'same'").await?;
        let layout = fixture.join("apps/testy/web/pages/layout.tsx");
        let layout_text = std::fs::read_to_string(&layout)?;
        ensure!(
            layout_text.contains("SNAP / TESTY"),
            "layout probe text missing"
        );
        std::fs::write(
            &layout,
            layout_text.replace("SNAP / TESTY", "SNAP / RELOADED"),
        )?;
        ui.text("SNAP / RELOADED").visible().await?;
        ensure!(
            ui.eval("window.__devDocument").await? == json!("same"),
            "React edit caused document navigation"
        );
        {
            let mut errors = Vec::new();
            {
                let mut stream = mid_errors.lock().unwrap();
                while let Some(Some(event)) = stream.next().now_or_never() {
                    errors.push(format!("{:?}", event.exception_details));
                }
            }
            ensure!(
                errors.is_empty(),
                "page errors after React refresh: {errors:?}"
            );
        }

        // Adapter restart publishes without losing the session.
        let adapter = fixture.join("tools/cli/web-dev.mjs");
        let adapter_text = std::fs::read_to_string(&adapter)?;
        ensure!(
            adapter_text.contains("const path = req.url"),
            "adapter probe missing"
        );
        std::fs::write(
            &adapter,
            adapter_text.replace(
                "const path = req.url",
                "res.setHeader(\"x-dev-adapter\", \"restarted\"); const path = req.url",
            ),
        )?;
        wait_header(&url, "x-dev-adapter", "restarted", DEV_TIMEOUT).await?;
        ui.text("Connected").visible().await?;

        // Invalid adapter edits are rejected; the previous adapter is retained.
        let known_adapter = std::fs::read_to_string(&adapter)?;
        std::fs::write(
            &adapter,
            format!("{known_adapter}\ninvalid JavaScript syntax;\n"),
        )?;
        child
            .wait_log(
                "Vite adapter rejected; previous adapter retained",
                DEV_TIMEOUT,
            )
            .await?;
        wait_header(&url, "x-dev-adapter", "restarted", DEV_TIMEOUT).await?;
        let restarts = count_occurrences(&child.log(), "Vite adapter restarted");
        std::fs::write(&adapter, &known_adapter)?;
        wait_log_count(&mut child, "Vite adapter restarted", restarts, DEV_TIMEOUT).await?;
        ui.text("Connected").visible().await?;

        // Invalid configuration is rejected; the previous generation is retained.
        let configuration = std::fs::read_to_string(&setup_path)?;
        std::fs::write(&setup_path, "invalid TOML")?;
        child
            .wait_log(
                "Configuration rejected; previous generation retained",
                DEV_TIMEOUT,
            )
            .await?;
        ui.button("Refresh").click().await?;
        ui.locator("[data-testid=\"accumulator\"]")
            .text("0")
            .await?;
        let configured = count_occurrences(&child.log(), "generation ready");
        std::fs::write(&setup_path, &configuration)?;
        wait_log_count(&mut child, "generation ready", configured, DEV_TIMEOUT).await?;
        ui.text("Connected").visible().await?;
        ui.label("Email").count(0).await?;

        // Unmigrated databases fail restart; the previous generation is retained.
        std::fs::write(
            &setup_path,
            configuration.replace(
                "database = \"identity.sqlite\"",
                "database = \"unmigrated.sqlite\"",
            ),
        )?;
        child
            .wait_log("Restart failed; previous generation retained", DEV_TIMEOUT)
            .await?;
        ui.reload().await?;
        ui.text("Connected").visible().await?;
        ui.label("Email").count(0).await?;
        std::fs::write(&setup_path, &configuration)?;
        let recovered = count_occurrences(&child.log(), "generation ready");
        wait_log_count(&mut child, "generation ready", recovered, DEV_TIMEOUT).await?;

        // Occupied frontend listeners fail replacement; the previous generation stays.
        let occupied = std::net::TcpListener::bind("127.0.0.1:0")?;
        let occupied_port = occupied.local_addr()?.port();
        std::fs::write(
            &setup_path,
            format!("{configuration}\n[dev]\nlisten = \"127.0.0.1:{occupied_port}\"\n"),
        )?;
        child
            .wait_log(
                "Frontend replacement failed; previous generation retained",
                DEV_TIMEOUT,
            )
            .await?;
        ui.reload().await?;
        ui.text("Connected").visible().await?;
        ui.label("Email").count(0).await?;

        // A config change and invalid adapter edit in the same debounce batch
        // must use the combined rollback, not the adapter-only path.
        let frontend_failures = count_occurrences(&child.log(), "Frontend replacement failed");
        std::fs::write(
            &adapter,
            format!("{known_adapter}\ninvalid JavaScript syntax;\n"),
        )?;
        std::fs::write(&setup_path, &configuration)?;
        wait_log_count(
            &mut child,
            "Frontend replacement failed",
            frontend_failures,
            DEV_TIMEOUT,
        )
        .await?;
        {
            let client = crate::support::client()?;
            let response = client.get(&url).send().await?;
            ensure!(
                response
                    .headers()
                    .get("x-dev-adapter")
                    .and_then(|v| v.to_str().ok())
                    == Some("restarted"),
                "combined rollback lost the previous adapter"
            );
        }
        ui.reload().await?;
        ui.text("Connected").visible().await?;
        let restored = count_occurrences(&child.log(), "generation ready");
        std::fs::write(&adapter, &known_adapter)?;
        std::fs::write(&setup_path, &configuration)?;
        wait_log_count(&mut child, "generation ready", restored, DEV_TIMEOUT).await?;
        drop(occupied);

        // Loopback-violating frontend configuration is rejected.
        let rejected = count_occurrences(&child.log(), "Configuration rejected");
        std::fs::write(
            &setup_path,
            format!("{configuration}\n[dev]\nlisten = \"0.0.0.0:0\"\n"),
        )?;
        wait_log_count(&mut child, "Configuration rejected", rejected, DEV_TIMEOUT).await?;
        let (dev_ok, _, _) = fetch_status(&format!("{url}/__dev"), &[]).await?;
        ensure!(
            dev_ok == 200,
            "expected 200 from retained dev endpoint, got {dev_ok}"
        );

        // Shutdown closes both listeners and exits with code 143.
        let status = child.terminate().await?;
        assert_exit_143(status, &child.log())?;
        crate::support::poll("frontend shutdown", 15, || async {
            Ok(crate::support::client()?.get(&url).send().await.is_err())
        })
        .await?;
        match crate::support::client()?.get(&backend_url).send().await {
            Ok(_) => anyhow::bail!("Backend listener survived snap dev shutdown"),
            Err(error) => ensure!(
                error.is_connect(),
                "unexpected backend error after shutdown: {error:?}"
            ),
        }
        let _ = setup_text;
        Ok(())
    }
    .await;
    // Ensure the supervisor is reaped even on failure; Drop also stops.
    let _ = child.stop();
    session.finish(result).await
}

// ---------------------------------------------------------------------------
// authy-dev: failed builds, session/profile persistence, CSS/React HMR,
// server-rendered authorization rebuild, shutdown.
// ---------------------------------------------------------------------------

async fn authy_dev_journey(browser: &Browser) -> Result<()> {
    let source = crate::support::SourceCopy::new()?;
    let fixture = source.root.clone();
    let host_dir = fixture.join("host");
    let deployment = crate::support::Deployment::create(
        &host_dir,
        json!({"host": {"mode": "development", "listen": "127.0.0.1:0", "data_dir": fixture.to_string_lossy(), "database": "authy.sqlite"}, "app": {"clients": [], "auto_approve_domain": ""}}),
        None,
    )?;
    let setup_path = deployment.path.clone();
    let setup_key = deployment.key.clone();
    {
        let mut cmd = crate::support::command(crate::support::root().join("target/debug/authy"));
        cmd.args(["--migrate", "--config", &setup_path.to_string_lossy()]);
        if let Some(key) = &setup_key {
            cmd.env("SNAP_MASTER_KEY", key);
        }
        crate::support::run(&mut cmd)?;
    }
    let mut child = {
        let mut cmd = crate::support::command(crate::support::root().join("target/debug/snap"));
        cmd.args([
            "dev",
            &fixture.join("apps/authy").to_string_lossy(),
            "--config",
            &setup_path.to_string_lossy(),
        ]);
        if let Some(key) = &setup_key {
            cmd.env("SNAP_MASTER_KEY", key);
        }
        crate::support::Process::start(&mut cmd)?
    };

    let session = crate::ui::Session::new(browser).await?;
    let result: Result<()> = async {
        child.wait_log("Authy dev http", DEV_TIMEOUT).await?;
        let (url, _) = parse_dev_urls(&child.log(), "Authy")?;
        wait_http_ok(&url, DEV_TIMEOUT).await?;
        let ui = &session.ui;
        ui.goto(&url).await?;
        ui.button("New here? Create account").click().await?;
        ui.label("Email").fill("dev-authy@example.test").await?;
        ui.label("Password").fill("Authy dev password").await?;
        ui.button("Create account").click().await?;
        ui.label("Name").fill("Survives rebuild").await?;
        ui.button("Save profile").click().await?;
        ui.text("Saved revision 2").visible().await?;

        // CSS hot reload retains the session-backed profile.
        let css = fixture.join("apps/authy/web/style.css");
        let css_text = std::fs::read_to_string(&css)?;
        std::fs::write(
            &css,
            format!("{css_text}\nbody {{ --authy-probe: active; }}\n"),
        )?;
        wait_eval_eq(
            ui,
            "getComputedStyle(document.body).getPropertyValue('--authy-probe').trim()",
            json!("active"),
            DEV_TIMEOUT,
        )
        .await?;

        // Failed native builds retain sessions across reload.
        let source_path = fixture.join("apps/authy/src/lib.rs");
        let original = std::fs::read_to_string(&source_path)?;
        std::fs::write(
            &source_path,
            format!("{original}\ncompile_error!(\"deliberate Authy dev failure\");\n"),
        )?;
        child
            .wait_log("Rebuild failed; previous generation retained", DEV_TIMEOUT)
            .await?;
        ui.reload().await?;
        wait_eval_eq(
            ui,
            "document.getElementById('profile-name')?.value ?? null",
            json!("Survives rebuild"),
            DEV_TIMEOUT,
        )
        .await?;

        // Successful rebuilds reload and retain the persisted profile.
        let ready = count_occurrences(&child.log(), "generation ready");
        ui.eval("window.__authyReload = 'waiting'").await?;
        std::fs::write(&source_path, &original)?;
        wait_log_count(&mut child, "generation ready", ready, DEV_TIMEOUT).await?;
        wait_eval_eq(
            ui,
            "window.__authyReload ?? null",
            json!(Value::Null),
            DEV_TIMEOUT,
        )
        .await?;
        ui.label("Name").visible().await?;
        wait_eval_eq(
            ui,
            "document.getElementById('profile-name')?.value ?? null",
            json!("Survives rebuild"),
            DEV_TIMEOUT,
        )
        .await?;

        // Component edits are React refresh, not document navigation.
        ui.eval("window.__authyDev = 'same'").await?;
        let page_path = fixture.join("apps/authy/web/pages/account.tsx");
        let page_text = std::fs::read_to_string(&page_path)?;
        ensure!(
            page_text.contains("Manage your profile and active sessions."),
            "account probe text missing"
        );
        std::fs::write(
            &page_path,
            page_text.replace(
                "Manage your profile and active sessions.",
                "Manage your profile and active sessions after reload.",
            ),
        )?;
        ui.text("Manage your profile and active sessions after reload.")
            .visible()
            .await?;
        ensure!(
            ui.eval("window.__authyDev").await? == json!("same"),
            "Authy UI edit caused document navigation"
        );

        // The no-JS authorization page rebuilds from shared server components.
        let blocked = crate::ui::Session::new(browser).await?;
        let blocked_result: Result<()> = async {
            use chromiumoxide::cdp::browser_protocol::emulation::SetScriptExecutionDisabledParams;
            blocked
                .ui
                .page
                .execute(SetScriptExecutionDisabledParams::new(true))
                .await?;
            let blocked_ui = &blocked.ui;
            blocked_ui
                .goto(&format!("{url}/oauth/authorize?client_id=unknown"))
                .await?;
            blocked_ui
                .text("Return to the application and try again, or check your Authy session.")
                .visible()
                .await?;
            let shared = fixture.join("apps/authy/web/auth-ui.tsx");
            let shared_text = std::fs::read_to_string(&shared)?;
            ensure!(
                shared_text.contains(
                    "Return to the application and try again, or check your Authy session."
                ),
                "shared auth probe missing"
            );
            let pages = count_occurrences(&child.log(), "generation ready");
            std::fs::write(
                &shared,
                shared_text.replace(
                    "Return to the application and try again, or check your Authy session.",
                    "Updated server-rendered authorization guidance.",
                ),
            )?;
            wait_log_count(&mut child, "generation ready", pages, DEV_TIMEOUT).await?;
            blocked_ui.reload().await?;
            blocked_ui
                .text("Updated server-rendered authorization guidance.")
                .visible()
                .await?;
            Ok(())
        }
        .await;
        blocked.finish(blocked_result).await?;

        let status = child.terminate().await?;
        assert_exit_143(status, &child.log())?;
        crate::support::poll("authy dev shutdown", 15, || async {
            Ok(crate::support::client()?.get(&url).send().await.is_err())
        })
        .await?;
        Ok(())
    }
    .await;
    let _ = child.stop();
    session.finish(result).await
}

// ---------------------------------------------------------------------------
// App and development journeys share the same real Authy/Chatty fixtures.
// ---------------------------------------------------------------------------

fn pair_logs(pair: &crate::hosts::ChattyHost) -> String {
    format!(
        "CHATTY:\n{}\nAUTHY:\n{}",
        pair.process.log(),
        pair.authy.process.log()
    )
}

async fn chatty_oauth_login(ui: &crate::ui::Ui, email: &str, password: &str) -> Result<()> {
    // Chatty sign-in starts at the RP link, continues through Authy account
    // creation, then explicit consent. Selectors use accessible names so the
    // real OAuth navigation is preserved.
    ui.xpath("//a[contains(normalize-space(.), 'Continue with Authy')]")
        .click()
        .await?;
    ui.button("New here? Create account").click().await?;
    ui.label("Email").fill(email).await?;
    ui.label("Password").fill(password).await?;
    ui.button("Create account").click().await?;
    Ok(())
}

async fn chatty_dev_journey(browser: &Browser) -> Result<()> {
    let source = crate::support::SourceCopy::new()?;
    let fixture_root = source.root.clone();
    let mut pair = crate::hosts::ChattyHost::start(&fixture_root, true).await?;
    let session = crate::ui::Session::new(browser).await?;
    let result: Result<()> = async {
        let base = pair.base.clone();
        let ui = &session.ui;
        ui.goto(&base).await?;
        chatty_oauth_login(ui, "dev-chatty@example.test", "Chatty dev OAuth password").await?;
        ui.button("Allow").click().await?;
        ui.label("Message Chatty")
            .fill("Persistent dev thread")
            .await?;
        ui.locator("button[aria-label=\"Send message\"]")
            .click()
            .await?;
        wait_transcript(ui, "Persistent dev thread", DEV_TIMEOUT).await?;

        // CSS hot reload retains the OAuth session and thread.
        let css = fixture_root.join("apps/chatty/web/style.css");
        let css_text = std::fs::read_to_string(&css)?;
        std::fs::write(
            &css,
            format!("{css_text}\nbody {{ --chatty-probe: active; }}\n"),
        )?;
        wait_eval_eq(
            ui,
            "getComputedStyle(document.body).getPropertyValue('--chatty-probe').trim()",
            json!("active"),
            DEV_TIMEOUT,
        )
        .await?;

        // Failed Chatty builds retain OAuth sessions and threads across reload.
        let source_path = fixture_root.join("apps/chatty/src/lib.rs");
        let original = std::fs::read_to_string(&source_path)?;
        std::fs::write(
            &source_path,
            format!("{original}\ncompile_error!(\"deliberate Chatty build failure\");\n"),
        )?;
        pair.process
            .wait_log("Rebuild failed; previous generation retained", DEV_TIMEOUT)
            .await?;
        ui.reload().await?;
        wait_transcript(ui, "Persistent dev thread", DEV_TIMEOUT).await?;

        // Successful rebuilds reload and retain the persisted thread.
        let ready = count_occurrences(&pair.process.log(), "generation ready");
        ui.eval("window.__chattyReload = 'waiting'").await?;
        std::fs::write(&source_path, &original)?;
        wait_log_count(&mut pair.process, "generation ready", ready, DEV_TIMEOUT).await?;
        wait_eval_eq(
            ui,
            "window.__chattyReload ?? null",
            json!(Value::Null),
            DEV_TIMEOUT,
        )
        .await?;
        wait_transcript(ui, "Persistent dev thread", DEV_TIMEOUT).await?;

        // Component edits are React refresh, not document navigation.
        ui.eval("window.__chattyDev = 'same'").await?;
        let view = fixture_root.join("apps/chatty/web/pages/conversations.tsx");
        let view_text = std::fs::read_to_string(&view)?;
        ensure!(
            view_text.contains("YOUR CONVERSATIONS"),
            "chatty probe text missing"
        );
        std::fs::write(
            &view,
            view_text.replace("YOUR CONVERSATIONS", "UPDATED CONVERSATIONS"),
        )?;
        ui.text("UPDATED CONVERSATIONS").visible().await?;
        ensure!(
            ui.eval("window.__chattyDev").await? == json!("same"),
            "Chatty UI edit caused document navigation"
        );

        let status = pair.process.terminate().await?;
        assert_exit_143(status, &pair.process.log())?;
        crate::support::poll("chatty dev shutdown", 15, || async {
            Ok(crate::support::client()?.get(&base).send().await.is_err())
        })
        .await?;
        Ok(())
    }
    .await
    .with_context(|| pair_logs(&pair));
    let _ = pair.stop();
    let _ = pair.authy.stop();
    session.finish(result).await
}

// ---------------------------------------------------------------------------
// dev-origins: alias callbacks/logout, spoof rejection, HMR, real WS messages.
// ---------------------------------------------------------------------------

async fn dev_origins_journey(browser: &Browser) -> Result<()> {
    // Isolate the watcher from edits in the working checkout. The same real
    // development host still validates aliases and OAuth against fresh databases.
    let source = crate::support::SourceCopy::new()?;
    let mut pair = crate::hosts::ChattyHost::start(source.path(), true).await?;
    let result: Result<()> = async {
        let port = reqwest::Url::parse(&pair.base)?
            .port_or_known_default()
            .unwrap_or(80);
        let aliases = crate::support::local_origins(port)?;
        let candidates: Vec<_> = aliases.iter().filter(|o| *o != &pair.base).cloned().collect();
        ensure!(!candidates.is_empty(), "expected alias origins besides {}", pair.base);

        // Alias callbacks preserve the checked origin; HMR is served per alias.
        // The evil dev-origin header must not influence the redirect.
        let no_redirect = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()?;
        for origin in &aliases {
            let login = no_redirect
                .get(format!("{origin}/auth/login"))
                .header("x-snap-dev-origin", "http://evil.test")
                .send()
                .await?;
            ensure!(login.status().as_u16() == 303, "expected 303 for {origin} login, got {}", login.status());
            let location = login.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or("");
            let target = reqwest::Url::parse(location).with_context(|| format!("invalid login location {location}"))?;
            ensure!(
                target.origin().ascii_serialization() == pair.authy.base,
                "login for {origin} targeted {}, expected {}",
                target.origin().ascii_serialization(),
                pair.authy.base
            );
            ensure!(
                target.query_pairs().find(|(k, _)| k == "redirect_uri").map(|(_, v)| v.into_owned()).as_deref() == Some(&format!("{origin}/auth/callback")),
                "login redirect_uri for {origin} was not the alias callback: {location}"
            );
            let vite = crate::support::client()?.get(format!("{origin}/@vite/client")).send().await?;
            ensure!(vite.status().is_success(), "expected HMR client for {origin}, got {}", vite.status());
        }

        // Spoofed Host/Origin pairs are rejected on HTTP and WebSockets.
        let base_url = reqwest::Url::parse(&pair.base)?;
        let base_host=base_url.host_str().context("missing fixture hostname")?;
        let base_authority=format!("{base_host}:{port}");
        for (host, origin) in [
            ("evil.test", "http://evil.test"),
            (base_authority.as_str(), "http://evil.test"),
            (base_authority.as_str(), "null"),
        ] {
            let host_port = format!("{}:{}", base_host, port);
            // Use the alias host for the TCP connection when spoofing the Host
            // header would otherwise misroute; the Host header itself is spoofed.
            let status = raw_status(&host_port, "/api/session", host, origin).await?;
            ensure!(status == 403, "expected 403 for spoofed {host}/{origin}, got {status}");
            let rejected = ws_rejected(&pair.base, host, origin).await?;
            ensure!(rejected == 403, "expected 403 for spoofed WS {host}/{origin}, got {rejected}");
        }

        // Each alias exercises host-only cookies, code exchange, authenticated
        // WS mutations and RP-initiated logout in a separate browser session.
        for origin in &aliases {
            let session = crate::ui::Session::new(browser).await?;
            let origin_result: Result<()> = async {
                let ui = &session.ui;
                // Record WebSocket URLs before navigation to capture the HMR
                // connection, independent of the app's JavaScript wrapper.
                ui.init("window.__snapHmr = []; const OrigWS = window.WebSocket; window.WebSocket = function(u, p) { window.__snapHmr.push(u); return new OrigWS(u, p); }; window.WebSocket.prototype = OrigWS.prototype;").await?;
                ui.goto(origin).await?;
                // HMR must connect back to the same alias authority.
                wait_eval_eq(
                    ui,
                    "window.__snapHmr.find(u => u.includes('token=')) ?? null",
                    json!(Value::Null),
                    1,
                )
                .await
                .ok();
                let hmr: Value = {
                    let mut last = Value::Null;
                    timeout(Duration::from_secs(15), async {
                        loop {
                            last = ui.eval("window.__snapHmr.find(u => u.includes('token=')) ?? null").await?;
                            if last.is_string() {
                                return Ok::<_, anyhow::Error>(last.clone());
                            }
                            sleep(Duration::from_millis(50)).await;
                        }
                    })
                    .await
                    .context("waiting for alias HMR connection")??;
                    last
                };
                let hmr_url = reqwest::Url::parse(hmr.as_str().unwrap_or(""))?;
                let origin_url = reqwest::Url::parse(origin)?;
                ensure!(hmr_url.host_str() == origin_url.host_str() && hmr_url.port_or_known_default() == origin_url.port_or_known_default(), "HMR authority {hmr_url} != alias authority {origin_url}");

                let email = format!("network-{}@example.test", uuid_suffix()?);
                chatty_oauth_login(ui, &email, "Network development password").await?;
                // Consent shows the exact alias origin before allowing.
                ui.text(origin).visible().await?;
                ui.button("Allow").click().await?;
                ui.label("Message Chatty").fill("Alias WebSocket works").await?;
                ui.locator("button[aria-label=\"Send message\"]").click().await?;
                wait_transcript(ui, "Alias WebSocket works", 10).await?;
                ensure!(
                    ui.eval("location.origin").await? == json!(origin),
                    "alias navigation left {origin}"
                );
                // RP-initiated logout returns to the same alias origin.
                ui.locator("button[aria-label=\"Sign out\"]").click().await?;
                ui.button("Confirm sign out").click().await?;
                ui.xpath("//a[contains(normalize-space(.), 'Continue with Authy')]")
                    .visible()
                    .await?;
                ensure!(
                    ui.eval("location.origin").await? == json!(origin),
                    "logout left {origin}"
                );
                Ok(())
            }
            .await;
            session.finish(origin_result).await?;
        }
        Ok(())
    }
    .await
    .with_context(|| pair_logs(&pair));
    let _ = pair.stop();
    let _ = pair.authy.stop();
    result
}

fn uuid_suffix() -> Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open("/dev/urandom").context("opening urandom")?;
    let mut bytes = [0u8; 16];
    file.read_exact(&mut bytes).context("reading urandom")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
