//! Testy's browser journeys through the real native host and browser carriers.
//!
//! Each journey owns an isolated process fixture (fresh migrated database and
//! `testy-web` host serving freshly built web assets) and a fresh browser
//! context. Browser input goes through the shared [`Session`]/[`Ui`] helpers;
//! sent mutations are never retried. Raw transport and debugger envelopes are
//! built as independent browser-side WebSocket frames, never through the app
//! SDK, and 64-bit values are asserted as exact decimal text on both carriers.
//! Failure cleanup is failure-aware: [`Session::finish`] collects page
//! exceptions, writes failure artifacts and still disposes the context when
//! steps fail, while the fixture [`Process`](support::Process) reaps the host
//! before scratch data is removed.
use anyhow::{Context, Result, anyhow, ensure};
use chromiumoxide::{
    Browser,
    cdp::browser_protocol::emulation::{
        EventVirtualTimeBudgetExpired, SetVirtualTimePolicyParams, VirtualTimePolicy,
    },
};
use futures::{FutureExt, SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::timeout;

use crate::{
    support,
    ui::{Session, Ui, js},
};

/// Run every journey whose name contains `filter` (empty runs all eight).
pub async fn run(browser: &Browser, filter: &str) -> Result<()> {
    let mut selected = 0;
    macro_rules! case {
        ($name:literal, $journey:ident) => {
            if filter.is_empty() || $name.contains(filter) {
                selected += 1;
                let start = std::time::Instant::now();
                $journey(browser)
                    .await
                    .with_context(|| format!("testy journey failed: {name}", name = $name))?;
                println!("PASS testy {} {:?}", $name, start.elapsed());
            }
        };
    }

    case!(
        "login sessions isolate calculators and sign-out closes every connection of that session",
        login_sessions_isolate
    );
    case!(
        "WebSocket envelopes and attachment ownership work without the SDK",
        raw_envelopes
    );
    case!(
        "launcher, health, calculator, reload and explicit close",
        launcher_journey
    );
    case!(
        "agent control steps a live browser request and restores its state",
        agent_control
    );
    case!(
        "execution desk displays and supplies exact i64 dependencies",
        exact_i64_desk
    );
    case!(
        "debugger pushes changes, correlates commands and has no application lifecycle",
        debugger_pushes
    );
    case!(
        "execution desk reconnects independently and is silent while idle",
        idle_silence
    );
    case!(
        "a lost debugger response fails the command without replaying the step",
        lost_response
    );
    ensure!(selected > 0, "no Testy journeys match {filter:?}");
    Ok(())
}

struct Fixture {
    process: support::Process,
    _scratch: tempfile::TempDir,
    url: String,
}

impl Fixture {
    /// Start the real host: migrate a fresh
    /// database through the production CLI, write a disposable deployment and
    /// spawn `testy-web` against freshly built web assets.
    async fn start() -> Result<Self> {
        let root = support::root();
        let assets = root.join("apps/testy/dist/development/clients/web");
        ensure!(
            assets.join("index.html").is_file(),
            "build Testy web assets first (bin/browser-tests builds them)"
        );
        let scratch = support::scratch("testy-web-")?;
        let database = scratch.path().join("identity.sqlite");
        let mut migrate = support::command(root.join("target/debug/snap"));
        migrate
            .args(["migrate", "--database"])
            .arg(&database)
            .arg("--migrations")
            .arg(root.join("crates/identity/migrations"))
            .current_dir(&root);
        support::run(&mut migrate)?;
        let deployment = support::Deployment::create(
            scratch.path(),
            json!({
                "host": {
                    "mode": "development",
                    "listen": "127.0.0.1:0",
                    "data_dir": scratch.path(),
                    "database": "identity.sqlite",
                    "web_dir": assets,
                },
                "app": {},
            }),
            None,
        )?;
        let mut server = support::command(root.join("target/debug/testy-web"));
        deployment.apply(&mut server);
        let mut process = support::Process::start(&mut server)?;
        let url = process.ready("Testy ", "/__dev", 10).await?;
        Ok(Self {
            process,
            _scratch: scratch,
            url,
        })
    }

    fn stop(&mut self) -> Result<()> {
        self.process.stop()
    }
}

/// Run `body` in a fresh fixture and browser context, awaiting failure-aware
/// cleanup (page exceptions, failure artifacts, context disposal, host reap)
/// even when the journey itself fails.
async fn case(
    browser: &Browser,
    body: impl AsyncFnOnce(&Session, &str) -> Result<()>,
) -> Result<()> {
    let mut fixture = Fixture::start().await?;
    let session = Session::new(browser).await?;
    let result = body(&session, &fixture.url).await;
    let finished = session.finish(result).await;
    let stopped = fixture.stop();
    match (finished, stopped) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
    }
}

async fn login(ui: &Ui, enroll: bool) -> Result<()> {
    ui.label("Email").fill("alice@example.com").await?;
    ui.label("Password").fill("testy-password1").await?;
    ui.button(if enroll { "Create account" } else { "Sign in" })
        .click()
        .await?;
    ui.text("Connected").visible().await
}

fn accumulator(ui: &Ui) -> crate::ui::Locator {
    ui.locator("[data-testid=\"accumulator\"]")
}

async fn login_sessions_isolate(browser: &Browser) -> Result<()> {
    case(browser, async |session, url| {
        let ui = &session.ui;
        let calc = format!("{url}/calc");
        ui.goto(&calc).await?;
        ui.button("+").enabled(false).await?;
        login(ui, true).await?;
        ui.label("Operand").fill("12").await?;
        ui.button("+").click().await?;
        accumulator(ui).text("12").await?;
        // A second login session gets an isolated calculator.
        let second = session.page().await?;
        second.goto(&calc).await?;
        login(&second, false).await?;
        accumulator(&second).text("0").await?;
        second.label("Operand").fill("7").await?;
        second.button("+").click().await?;
        accumulator(&second).text("7").await?;
        // A sibling tab sharing the first session's bearer reconnects fresh.
        let token = ui
            .eval("sessionStorage.getItem(\"testy.session\")")
            .await?
            .as_str()
            .context("missing session token")?
            .to_owned();
        let sibling = session.page().await?;
        sibling
            .init(&format!(
                "sessionStorage.setItem('testy.session', {})",
                js(&token)
            ))
            .await?;
        sibling.goto(&calc).await?;
        sibling.text("Connected").visible().await?;
        accumulator(&sibling).text("0").await?;
        // Sign-out closes every connection of that session and clears state.
        ui.button("Sign out").click().await?;
        ui.button("Sign in").visible().await?;
        accumulator(ui).text("0").await?;
        sibling.text("Disconnected").visible().await?;
        // The other login session keeps working.
        second.button("Refresh").click().await?;
        accumulator(&second).text("7").await?;
        // Bearer and password material never reaches diagnostics or the wire panel.
        let diagnostics = support::client()?
            .get(format!("{url}/__dev"))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        ensure!(
            !diagnostics.contains(&token),
            "diagnostics leak the session token"
        );
        ensure!(
            !diagnostics.contains("testy-password1"),
            "diagnostics leak the password"
        );
        let wire = ui.locator(".wire").read_text().await?;
        ensure!(!wire.contains(&token), "wire panel leaks the session token");
        ensure!(
            !wire.contains("testy-password1"),
            "wire panel leaks the password"
        );
        Ok(())
    })
    .await
}

async fn raw_envelopes(browser: &Browser) -> Result<()> {
    case(browser, async |session, url| {
        let ui = &session.ui;
        ui.goto(url).await?;
        let observations = ui
            .eval(
                r#"(async () => {
  async function open() {
    const ws = new WebSocket(`${location.origin.replace("http", "ws")}/transport`);
    await new Promise((resolve, reject) => {
      ws.onopen = () => resolve();
      ws.onerror = () => reject(new Error("connect"));
    });
    return ws;
  }
  function exchange(ws, command) {
    return new Promise((resolve, reject) => {
      const frames = [];
      ws.onclose = () => reject(new Error("unexpected close"));
      ws.onmessage = ({ data }) => {
        const response = JSON.parse(data);
        frames.push(response);
        if (!response.Events || response.Events.some((event) => event.Completed))
          resolve(frames);
      };
      ws.send(JSON.stringify(command));
    });
  }
  const first = await open();
  const second = await open();
  const health = await exchange(first, {
    Request: {
      bearer: null,
      invocation: { id: 1, operation: "health.up", input: null },
    },
  });
  const enrollment = await exchange(first, { Request: { bearer: null,
    invocation: { id: 2, operation: "identity.enroll", input: { email: "raw@example.com", password: "password1" } } } });
  const events = enrollment.flatMap(frame => frame.Events ?? []);
  const token = events.find(event => event.Bearer)?.Bearer.change.Set;
  const completion = events.find(event => event.Completed).Completed.outcome.Ok;
  if (!token || completion.bearer || completion.session) throw new Error("private bearer handoff");
  const attach = {
    Connect: {
      bearer: token,
      client_id: "raw-frame-client",
    },
  };
  const attached = await exchange(first, attach);
  const occupied = await exchange(second, attach);
  await new Promise((resolve) => {
    first.onclose = () => resolve();
    first.close();
  });
  const resumed = await exchange(second, attach);
  await exchange(second, "Close");
  second.close();
  return { health, attached, occupied, resumed };
})()"#,
            )
            .await?;
        ensure!(
            observations
                == json!({
                    "health": [
                        { "Events": [{ "Accepted": { "id": 1 } }] },
                        { "Events": [{ "Completed": { "id": 1, "outcome": { "Ok": { "status": "OK" } } } }] },
                    ],
                    "attached": [{ "Attached": { "resumed": false } }],
                    "occupied": [{ "Failed": "Occupied" }],
                    "resumed": [{ "Attached": { "resumed": false } }],
                }),
            "unexpected wire observations: {observations}"
        );
        Ok(())
    })
    .await
}

async fn launcher_journey(browser: &Browser) -> Result<()> {
    case(browser, async |session, url| {
        let ui = &session.ui;
        ui.goto(url).await?;
        ui.eval("window.__testyDocument = 'same'").await?;
        ui.locator("a[href=\"/healthy\"]").click().await?;
        ui.button("Check health").click().await?;
        ui.locator(".health-result output").text("OK").await?;
        ui.locator("nav a[href=\"/\"]").click().await?;
        ui.locator("a[href=\"/calc\"]").click().await?;
        login(ui, true).await?;
        ui.text("Connected").visible().await?;
        ui.label("Operand").fill("12").await?;
        ui.button("+").click().await?;
        accumulator(ui).text("12").await?;
        // Router navigation keeps the document but retires the page's calculator.
        ui.locator("nav a[href=\"/\"]").click().await?;
        ui.locator("a[href=\"/healthy\"]").click().await?;
        ui.button("Check health").click().await?;
        ui.locator(".health-result output").text("OK").await?;
        ui.history(-1).await?;
        ui.wait("location.pathname", json!("/")).await?;
        ui.heading("Your testing ground.").visible().await?;
        ui.history(-1).await?;
        ui.wait("location.pathname", json!("/calc")).await?;
        ui.text("Connected").visible().await?;
        accumulator(ui).text("0").await?;
        ensure!(
            ui.eval("window.__testyDocument").await? == json!("same"),
            "router navigation reloaded the document"
        );
        ui.reload().await?;
        ui.text("Connected").visible().await?;
        accumulator(ui).text("0").await?;
        ui.button("Disconnect").click().await?;
        ui.button("Reconnect").click().await?;
        ui.text("Connected").visible().await?;
        ui.label("Operand").fill("3").await?;
        ui.button("×").click().await?;
        accumulator(ui).text("0").await?;
        ui.button("Close calculator").click().await?;
        ui.button("Reconnect").click().await?;
        accumulator(ui).text("0").await?;
        // Values above JavaScript's exact integer range stay exact on both carriers.
        ui.label("Operand").fill("9007199254740993").await?;
        ui.button("+").click().await?;
        accumulator(ui).text("9007199254740993").await?;
        // Leaving while an async Wasm method borrows the SDK still unmounts cleanly.
        support::post(
            &format!("{url}/__dev"),
            json!({"action": "breakpoint", "enabled": true}),
        )
        .await?;
        ui.label("Operand").fill("0").await?;
        ui.button("Checked +").click().await?;
        ui.locator(".execution-status")
            .contains("ready for attempt")
            .await?;
        ui.locator("nav a[href=\"/\"]").click().await?;
        ui.heading("Your testing ground.").visible().await?;
        support::post(
            &format!("{url}/__dev"),
            json!({"action": "breakpoint", "enabled": false}),
        )
        .await?;
        support::post(
            &format!("{url}/__dev"),
            json!({"action": "mode", "manual": false}),
        )
        .await?;
        ui.history(-1).await?;
        ui.wait("location.pathname", json!("/calc")).await?;
        ui.text("Connected").visible().await?;
        accumulator(ui).text("0").await?;
        Ok(())
    })
    .await
}

async fn agent_control(browser: &Browser) -> Result<()> {
    case(browser, async |session, url| {
        let ui = &session.ui;
        let calc = format!("{url}/calc");
        let dev = format!("{url}/__dev");
        let raw = support::client()?;
        let landing = raw.get(&calc).send().await?;
        ensure!(
            landing.status() == reqwest::StatusCode::OK,
            "calc landing status {}",
            landing.status()
        );
        ui.goto(&calc).await?;
        login(ui, true).await?;
        ui.text("Connected").visible().await?;
        support::post(&dev, json!({"action": "snapshot"})).await?;
        support::post(&dev, json!({"action": "breakpoint", "enabled": true})).await?;
        ui.button("Checked +").click().await?;
        support::poll("browser request accepted", 10, || async {
            Ok(support::get(&dev).await?["active"]["accepted"] == json!(true))
        })
        .await?;
        let state = support::post(&dev, json!({"action": "step"})).await?;
        ensure!(
            state["active"]["waiting"] == json!("testy.calculator.ceiling"),
            "unexpected held dependency: {state}"
        );
        ensure!(
            state["states"][0]["state"]["accumulator"] == json!(0),
            "committed state leaked into the held attempt: {state}"
        );
        let denied = raw
            .post(&dev)
            .json(&json!({"action": "restore"}))
            .send()
            .await?;
        ensure!(
            denied.status() == reqwest::StatusCode::CONFLICT,
            "restore with outstanding work returned {}",
            denied.status()
        );
        support::post(
            &dev,
            json!({
                "action": "supply",
                "ticket": state["active"]["ticket"],
                "key": state["active"]["waiting"],
                "value": 1000,
            }),
        )
        .await?;
        support::post(&dev, json!({"action": "breakpoint", "enabled": false})).await?;
        support::post(&dev, json!({"action": "step"})).await?;
        support::post(&dev, json!({"action": "mode", "manual": false})).await?;
        accumulator(ui).text("10").await?;
        ui.button("Refresh").enabled(true).await?;
        support::post(&dev, json!({"action": "restore"})).await?;
        support::post(&dev, json!({"action": "replace", "program": "double-add"})).await?;
        ui.button("+").click().await?;
        accumulator(ui).text("20").await?;
        let foreign = raw
            .post(&dev)
            .header("origin", "https://other.example")
            .json(&json!({"action": "step"}))
            .send()
            .await?;
        ensure!(
            foreign.status() == reqwest::StatusCode::FORBIDDEN,
            "foreign origin returned {}",
            foreign.status()
        );
        Ok(())
    })
    .await
}

async fn exact_i64_desk(browser: &Browser) -> Result<()> {
    case(browser, async |session, url| {
        let ui = &session.ui;
        ui.init(
            r#"(() => {
  window.__supplyFrames = [];
  const Native = window.WebSocket;
  window.WebSocket = class extends Native {
    constructor(url, protocols) {
      super(url, protocols);
      if (String(url).endsWith("/__dev/ws")) {
        const send = this.send.bind(this);
        this.send = (data) => {
          try { window.__supplyFrames.push(String(data)); } catch (e) {}
          return send(data);
        };
      }
    }
  };
})()"#,
        )
        .await?;
        ui.goto(&format!("{url}/calc")).await?;
        login(ui, true).await?;
        ui.text("Connected").visible().await?;
        ui.xpath("//summary[contains(., \"Committed records\")]")
            .click()
            .await?;
        ui.xpath("//summary[contains(., \"Execution trace\")]")
            .click()
            .await?;
        for (index, value) in [
            "9007199254740993",
            "9223372036854775807",
            "-9223372036854775808",
        ]
        .into_iter()
        .enumerate()
        {
            if index > 0 {
                ui.button("Close calculator").click().await?;
                ui.button("Reconnect").click().await?;
                ui.text("Connected").visible().await?;
            }
            ui.label("Operand").fill(value).await?;
            ui.button("+").click().await?;
            accumulator(ui).text(value).await?;
            ui.locator("[data-testid=\"host-state\"]")
                .contains(&format!("\"accumulator\": {value}"))
                .await?;
            ui.label("Break after acceptance").click().await?;
            ui.label("Break after acceptance").checked(true).await?;
            ui.label("Operand").fill("0").await?;
            ui.button("Checked +").click().await?;
            ui.locator(".execution-status")
                .contains("ready for attempt")
                .await?;
            ui.button("Step once").click().await?;
            // Invalid JSON is rejected locally, leaving the dependency intact.
            ui.label("Dependency value")
                .fill(&format!("{value} trailing"))
                .await?;
            ui.button("Supply input").click().await?;
            ui.xpath("//*[@role=\"alert\"]").visible().await?;
            ui.label("Dependency value").fill(value).await?;
            ui.button("Supply input").click().await?;
            ui.label("Dependency value").count(0).await?;
            let frames = ui.eval("window.__supplyFrames").await?;
            let supplied = frames
                .as_array()
                .context("missing recorded debugger frames")?
                .iter()
                .rev()
                .find(|frame| {
                    frame
                        .as_str()
                        .is_some_and(|text| text.contains("\"action\":\"supply\""))
                })
                .context("no supply command reached the debugger socket")?;
            ensure!(
                supplied
                    .as_str()
                    .is_some_and(|text| text.contains(&format!("\"value\":{value}"))),
                "supply frame lost exact i64 digits: {supplied}"
            );
            ui.label("Break after acceptance").click().await?;
            ui.label("Break after acceptance").checked(false).await?;
            ui.button("Step once").click().await?;
            ui.button("Run").click().await?;
            ui.button("Refresh").enabled(true).await?;
            ui.xpath("//*[@role=\"alert\"]").count(0).await?;
            ui.heading("History 2").visible().await?;
            accumulator(ui).text(value).await?;
            ui.locator("[data-testid=\"host-trace\"]")
                .contains(&format!("\"Ok\": {value}"))
                .await?;
        }
        Ok(())
    })
    .await
}

async fn debugger_pushes(browser: &Browser) -> Result<()> {
    case(browser, async |session, url| {
        let ui = &session.ui;
        ui.goto(url).await?;
        let result = ui
            .eval(
                r#"(async () => {
  async function debuggerSocket() {
    const socket = new WebSocket(`${location.origin.replace("http", "ws")}/__dev/ws`);
    const responses = new Map();
    const reports = [];
    let wake;
    socket.onmessage = ({ data }) => {
      const frame = JSON.parse(data);
      if (frame.type === "state") {
        reports.push(frame);
        if (wake) wake();
      } else {
        const resolve = responses.get(frame.id);
        responses.delete(frame.id);
        if (resolve) resolve(frame);
      }
    };
    async function state(predicate) {
      while (!reports.length || !predicate(reports[reports.length - 1].state))
        await new Promise((resolve) => {
          wake = resolve;
        });
      wake = undefined;
      return reports[reports.length - 1];
    }
    await state(() => true);
    return {
      socket,
      reports,
      state,
      command(id, control) {
        return new Promise((resolve) => {
          responses.set(id, resolve);
          socket.send(JSON.stringify({ id, control }));
        });
      },
    };
  }
  const first = await debuggerSocket();
  const second = await debuggerSocket();
  const initial = first.reports[0];
  const commands = await Promise.all([
    first.command("hold", { action: "mode", manual: true }),
    first.command("invalid", { action: "does_not_exist" }),
    first.command("save", { action: "snapshot" }),
  ]);
  const observed = await second.state((state) => state.manual && state.snapshot);
  await new Promise((resolve) => {
    first.socket.onclose = () => resolve();
    first.socket.close();
  });
  // An HTTP change also reaches subscribers, without a socket command or poll.
  await fetch("/__dev", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ action: "breakpoint", enabled: true }),
  });
  await second.state((state) => state.breakpoint);
  const reconnected = await debuggerSocket();
  const resumed = reconnected.reports[0];
  await reconnected.command("run", { action: "mode", manual: false });
  await second.state((state) => !state.manual);
  second.socket.close();
  reconnected.socket.close();
  return { initial, commands, observed, resumed };
})()"#,
            )
            .await?;
        ensure!(
            result["initial"]["type"] == json!("state"),
            "unexpected initial debugger frame: {result}"
        );
        ensure!(
            result["initial"]["state"]["peers"] == json!([]),
            "debugger allocated an application peer: {result}"
        );
        let commands = result["commands"]
            .as_array()
            .context("missing correlated debugger commands")?;
        let ids: Vec<&str> = commands
            .iter()
            .filter_map(|frame| frame.get("id").and_then(Value::as_str))
            .collect();
        ensure!(
            ids == ["hold", "invalid", "save"],
            "debugger commands mis-correlated: {result}"
        );
        ensure!(
            commands[0]["result"]["manual"] == json!(true),
            "unexpected hold result: {result}"
        );
        ensure!(
            commands[1]["error"]
                .as_str()
                .is_some_and(|error| error.contains("unknown variant")),
            "unexpected invalid-action error: {result}"
        );
        ensure!(
            commands[2]["result"]["snapshot"] == json!(true),
            "unexpected snapshot result: {result}"
        );
        let initial_revision: u64 = result["initial"]["revision"]
            .as_str()
            .context("missing initial revision")?
            .parse()
            .context("non-numeric debugger revision")?;
        let observed_revision: u64 = result["observed"]["revision"]
            .as_str()
            .context("missing observed revision")?
            .parse()
            .context("non-numeric debugger revision")?;
        ensure!(
            observed_revision > initial_revision,
            "debugger never pushed the HTTP change: {result}"
        );
        for (path, expected) in [
            ("manual", json!(true)),
            ("snapshot", json!(true)),
            ("breakpoint", json!(true)),
            ("peers", json!([])),
            ("states", json!([])),
        ] {
            ensure!(
                result["resumed"]["state"][path] == expected,
                "reconnect lost debugger state at {path}: {result}"
            );
        }
        Ok(())
    })
    .await
}

async fn idle_silence(browser: &Browser) -> Result<()> {
    case(browser, async |session, url| {
        let ui = &session.ui;
        ui.init(
            r#"(() => {
  window.__debuggerFrames = 0;
  window.__appSockets = 0;
  window.debuggers = [];
  const Native = window.WebSocket;
  window.WebSocket = class extends Native {
    constructor(url, protocols) {
      super(url, protocols);
      const seen = String(url);
      if (seen.endsWith("/__dev/ws")) {
        window.debuggers.push(this);
        this.addEventListener("message", () => { window.__debuggerFrames += 1; });
      } else if (seen.endsWith("/transport")) { window.__appSockets += 1; }
    }
  };
})()"#,
        )
        .await?;
        let calc = format!("{url}/calc");
        let dev = format!("{url}/__dev");
        let mut requests=ui.page.event_listener::<chromiumoxide::cdp::browser_protocol::network::EventRequestWillBeSent>().await?;
        ui.goto(&calc).await?;
        login(ui, true).await?;
        ui.text("Connected").visible().await?;
        ui.text("Debugger: Live").visible().await?;
        ui.button("+").click().await?;
        accumulator(ui).text("10").await?;
        ui.xpath("//summary[contains(., \"Committed records\")]")
            .click()
            .await?;
        ui.locator("[data-testid=\"host-state\"]")
            .contains("\"accumulator\": 10")
            .await?;
        ui.button("Hold").click().await?;
        ui.button("Run").visible().await?;
        let before = support::get(&dev).await?;
        ui.eval("window.debuggers.at(-1).close()").await?;
        ui.wait("window.debuggers.length", json!(2)).await?;
        ui.text("Debugger: Live").visible().await?;
        // Reconnection observes current state without touching the application.
        ensure!(
            support::get(&dev).await? == before,
            "debugger reconnect changed host inspection"
        );
        ensure!(
            ui.eval("window.__appSockets").await? == json!(2),
            "unexpected application socket count"
        );
        // Advance virtual time past the old 250ms HTTP poll interval so any
        // pre-existing polling timer fires deterministically instead of
        // sleeping through it.
        let frames_before = ui.eval("window.__debuggerFrames").await?;
        let mut expired = ui
            .page
            .event_listener::<EventVirtualTimeBudgetExpired>()
            .await?;
        ui.page
            .execute(
                SetVirtualTimePolicyParams::builder()
                    .policy(VirtualTimePolicy::PauseIfNetworkFetchesPending)
                    .budget(1100.0)
                    .build()
                    .map_err(|error| anyhow!(error))?,
            )
            .await?;
        timeout(Duration::from_secs(30), expired.next())
            .await
            .context("virtual time budget never expired")?
            .context("virtual time event stream ended")?;
        ui.eval("0").await?;
        let mut polls=Vec::new();
        while let Some(Some(request))=requests.next().now_or_never() {
            if request.request.url.ends_with("/__dev") {polls.push((request.request.method.clone(),request.request.url.clone()));}
        }
        ensure!(polls.is_empty(),"execution desk polled: {polls:?}");
        ensure!(
            ui.eval("window.__debuggerFrames").await? == frames_before,
            "debugger pushed while idle"
        );
        // Resume advancement before RAF-gated input: click stability needs frames.
        ui.page
            .execute(
                SetVirtualTimePolicyParams::builder()
                    .policy(VirtualTimePolicy::Advance)
                    .build()
                    .map_err(|error| anyhow!(error))?,
            )
            .await?;
        ui.button("Run").click().await?;
        ui.button("+").click().await?;
        accumulator(ui).text("20").await?;
        Ok(())
    })
    .await
}

#[derive(Default)]
struct RelayState {
    steps: usize,
    step_id: Option<String>,
    dropped: bool,
}

struct Relay {
    address: std::net::SocketAddr,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl Relay {
    async fn start(upstream: String, state: Arc<Mutex<RelayState>>) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (shutdown, mut stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut peers = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _=&mut stopped=>break,
                    accepted=listener.accept()=>{
                        let Ok((stream,_))=accepted else {break;};
                        let upstream=upstream.clone();let state=state.clone();
                        peers.spawn(async move {proxy_connection(stream,upstream,state).await});
                    },
                    _=peers.join_next(),if !peers.is_empty()=>{},
                }
            }
            peers.abort_all();
            while peers.join_next().await.is_some() {}
        });
        Ok(Self {
            address,
            shutdown: Some(shutdown),
            task: Some(task),
        })
    }
    async fn stop(&mut self) -> Result<()> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.as_mut() {
            task.await?;
        }
        self.task.take();
        Ok(())
    }
}
impl Drop for Relay {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Proxy one browser debugger socket to the real host, dropping the first
/// `step` result at the carrier and closing both sides. The host has already
/// executed that step; the desk must fail the command without replaying it.
async fn proxy_connection(
    stream: tokio::net::TcpStream,
    upstream_url: String,
    state: Arc<Mutex<RelayState>>,
) -> Result<()> {
    use tokio_tungstenite::{accept_async, connect_async, tungstenite::Message};
    let downstream = accept_async(stream).await.context("relay accept")?;
    let (upstream, _) = connect_async(&upstream_url)
        .await
        .context("relay upstream")?;
    let (mut down_sink, mut down_stream) = downstream.split();
    let (mut up_sink, mut up_stream) = upstream.split();
    loop {
        tokio::select! {
            message = down_stream.next() => {
                let Some(message) = message else { break };
                let message = message.context("browser debugger frame")?;
                if let Message::Text(text) = &message
                    && let Ok(frame) = serde_json::from_str::<Value>(text)
                    && frame.pointer("/control/action") == Some(&json!("step"))
                {
                    let mut state = state.lock().unwrap();
                    state.steps += 1;
                    state.step_id = frame.get("id").and_then(Value::as_str).map(str::to_owned);
                }
                if up_sink.send(message).await.is_err() {
                    break;
                }
            }
            message = up_stream.next() => {
                let Some(message) = message else { break };
                let message = message.context("host debugger frame")?;
                let lost = if let Message::Text(text) = &message {
                    serde_json::from_str::<Value>(text).ok().is_some_and(|frame| {
                        let state = state.lock().unwrap();
                        state.step_id.is_some()
                            && !state.dropped
                            && frame.get("type") == Some(&json!("result"))
                            && frame.get("id").and_then(Value::as_str) == state.step_id.as_deref()
                    })
                } else {
                    false
                };
                if lost {
                    state.lock().unwrap().dropped = true;
                    break;
                }
                if down_sink.send(message).await.is_err() {
                    break;
                }
            }
        }
    }
    let _ = down_sink.close().await;
    let _ = up_sink.close().await;
    Ok(())
}

async fn lost_response(browser: &Browser) -> Result<()> {
    let mut fixture = Fixture::start().await?;
    let relay_state = Arc::new(Mutex::new(RelayState::default()));
    let upstream_url = format!("{}/__dev/ws", fixture.url.replace("http", "ws"));
    let mut relay = Relay::start(upstream_url, relay_state.clone()).await?;
    let relay_addr = relay.address;
    let session = Session::new(browser).await?;
    let result = async {
        // Route the desk through the relay at the browser boundary; the
        // application carrier is untouched.
        session
            .ui
            .init(&format!(
                "(() => {{ const Native = window.WebSocket; window.WebSocket = class extends Native {{ constructor(url, protocols) {{ if (String(url).endsWith(\"/__dev/ws\")) url = {}; super(url, protocols); }} }}; }})()",
                js(&format!("ws://{relay_addr}/__dev/ws"))
            ))
            .await?;
        let ui = &session.ui;
        let calc = format!("{}/calc", fixture.url);
        let dev = format!("{}/__dev", fixture.url);
        ui.goto(&calc).await?;
        login(ui, true).await?;
        ui.text("Connected").visible().await?;
        ui.button("Hold").click().await?;
        ui.button("Run").visible().await?;
        ui.button("+").click().await?;
        ui.locator(".execution-status")
            .contains("1 queued operations")
            .await?;
        ui.button("Step once").click().await?;
        ui.xpath("//*[@role=\"alert\"]")
            .contains("command outcome may be unknown")
            .await?;
        ui.text("Debugger: Live").visible().await?;
        ui.locator(".execution-status")
            .contains("ready for attempt")
            .await?;
        let state = support::get(&dev).await?;
        ensure!(
            state["states"][0]["state"]["accumulator"] == json!(0),
            "lost step mutated committed state: {state}"
        );
        ensure!(
            state["active"]["accepted"] == json!(true),
            "lost step was not accepted before the response was lost: {state}"
        );
        ensure!(
            relay_state.lock().unwrap().steps == 1,
            "the desk replayed the step after losing its response"
        );
        ensure!(relay_state.lock().unwrap().dropped,"the relay did not drop the step result");
        ui.button("Step once").click().await?;
        ui.button("Run").click().await?;
        accumulator(ui).text("10").await?;
        Ok(())
    }
    .await;
    let finished = session.finish(result).await;
    let relayed = relay.stop().await;
    let stopped = fixture.stop();
    if finished.is_ok() {
        relayed?;
    }
    match (finished, stopped) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
    }
}
