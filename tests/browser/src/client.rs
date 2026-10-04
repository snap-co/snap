//! Rust owns the assertions and ordering; Chromium executes the actual frontend
//! runtime and invocation classes. Dependency doubles supply IO, not outcomes.
use anyhow::{Result, ensure};
use chromiumoxide::Browser;
use serde_json::{Value, json};

use crate::{support, ui::Session};

pub async fn run(browser: &Browser, filter: &str) -> Result<()> {
    let entry = support::root().join("kits/browser/tests/fixture.ts");
    let host = support::BundleHost::start(&entry).await?;
    let cases = [
        (
            "anonymous startup and retryable identity failure",
            r#"let failed = true;
const f = fixture(async () => { if (failed) throw new Error('offline'); return null; });
try {
  const error = await f.runtime.resolve().then(() => null, e => e.message);
  const phase = f.runtime.getSnapshot().phase;
  failed = false; await f.runtime.refresh();
  return [error, phase, (await f.runtime.resolve()).phase, f.sockets.length];
} finally { f.runtime.close(); }"#,
            json!(["offline", "error", "anonymous", 0]),
        ),
        (
            "router acquisition uses one binding and waits for empty manifest",
            r#"const held = deferred();
const f = fixture(async () => ({id:'alice'}), () => held.promise);
try {
  const acquire = f.runtime.replace({id:'alice'}), route = f.runtime.resolve();
  await Promise.resolve(); await Promise.resolve();
  const created = f.created;
  held.resolve(binding()); await acquire;
  f.sockets[0].receive({Attached:{resumed:false}});
  const phase = f.runtime.getSnapshot().phase;
  f.sockets[0].receive({manifest:true, documents:[]});
  return [created, phase, (await route).phase, f.sockets.length];
} finally { held.resolve(binding()); f.runtime.close(); }"#,
            json!([1, "loading", "ready", 1]),
        ),
        (
            "late identity cannot restore signed-out account",
            r#"const identity = deferred(), f = fixture(() => identity.promise);
try {
  const first = f.runtime.resolve(); await f.runtime.replace(null);
  identity.resolve({id:'old'});
  return [(await first).phase, f.sockets.length];
} finally { f.runtime.close(); }"#,
            json!(["anonymous", 0]),
        ),
        (
            "replacement fences late binding and frees it",
            r#"const held = deferred(); let freed = 0;
const f = fixture(async () => ({id:'alice'}), () => held.promise);
try {
  const first = f.runtime.refresh(); await Promise.resolve(); await Promise.resolve();
  await f.runtime.replace(null);
  held.resolve(binding(() => {freed++;})); await first;
  return [f.runtime.getSnapshot().phase, freed, f.sockets.length];
} finally { f.runtime.close(); }"#,
            json!(["anonymous", 1, 0]),
        ),
        (
            "physical recovery retains pages and confirmed expiry clears them",
            r#"let account = {id:'alice'}; const f = fixture(async () => account);
try {
  await f.runtime.refresh();
  f.sockets[0].receive({Attached:{resumed:false}}); f.sockets[0].receive({manifest:true});
  const call = f.runtime.invoke('example', null).then(() => null, e => e.message);
  f.sockets[0].receive({Events:[{Accepted:{id:1}}]});
  const epoch = f.runtime.getSnapshot().epoch, publication = f.publications.at(-1);
  f.sockets[0].receive({reset:true}); const retained = f.publications.at(-1) === publication;
  f.sockets[0].close(); const phase = f.runtime.getSnapshot().phase;
  await f.runtime.refresh(); const created = f.created, sameEpoch = epoch === f.runtime.getSnapshot().epoch;
  account = null; await f.runtime.refresh();
  return [retained, phase, created, sameEpoch, f.runtime.getSnapshot().phase, f.publications.at(-1), f.freed, await call, f.sockets[1].sent.length];
} finally { f.runtime.close(); }"#,
            json!([
                true,
                "ready",
                1,
                true,
                "anonymous",
                null,
                1,
                "Physical connection lost; outstanding outcomes are unknown",
                0
            ]),
        ),
        (
            "failed identity recovery never replays an interrupted application call",
            r#"const unavailable = deferred(), recovered = deferred(); let checks = 0;
const f = fixture(async () => {checks++; if (checks === 2) {unavailable.resolve(); throw new Error('backend restarting');} if (checks === 3) recovered.resolve(); return {id:'alice'};});
try {
  await f.runtime.refresh(); f.sockets[0].receive({Attached:{resumed:false}}); f.sockets[0].receive({manifest:true});
  const call = f.runtime.invoke('example.change', {value:'new'}).then(value => ({value}), e => ({error:e.message}));
  const frame = f.sockets[0].sent[0], id = JSON.parse(frame).Invoke.id;
  f.sockets[0].receive({Events:[{Accepted:{id}}]}); f.sockets[0].close();
  await unavailable.promise; const unavailableSockets = f.sockets.length;
  await recovered.promise; await f.runtime.refresh();
  f.sockets[1].receive({Attached:{resumed:true}}); f.sockets[1].receive({manifest:true});
  const original = f.sockets[0].sent, retry = f.sockets[1].sent;
  f.sockets[1].receive({Events:[{Completed:{id, outcome:{Ok:'recovered'}}}]});
  return [unavailableSockets, checks, f.created, original.length, retry.length, await call, f.runtime.getSnapshot().connection, f.runtime.getSnapshot().error];
} finally { f.runtime.close(); }"#,
            json!([1, 3, 1, 1, 0, {"error":"Physical connection lost; outstanding outcomes are unknown"}, "connected", null]),
        ),
        (
            "disposal settles held route readiness",
            r#"const identity = deferred(), f = fixture(() => identity.promise);
const route = f.runtime.resolve().then(() => null, e => e.message);
f.runtime.close(); const error = await route; identity.resolve({id:'alice'});
await Promise.resolve(); await Promise.resolve(); return [error, f.sockets.length];"#,
            json!(["Client closed", 0]),
        ),
        (
            "invocation ACK progress completion and duplicate reply",
            r#"const sent = [], progress = []; let sequence = 0;
const calls = new Invocations((operation,input) => JSON.stringify({Invoke:{id:++sequence,operation,input}}), frame => sent.push(frame));
try {
  const result = calls.invoke('test', {}, value => progress.push(value));
  const accepted = calls.receive(JSON.stringify({Events:[{Accepted:{id:1}}]}));
  calls.receive(JSON.stringify({Events:[{Progress:{id:1,value:'working'}}]}));
  const completed = JSON.stringify({Events:[{Completed:{id:1,outcome:{Ok:42}}}]});
  calls.receive(completed);
  return [accepted, progress, sent.length, await result, calls.receive(completed)];
} finally { calls.close(); }"#,
            json!([true, ["working"], 1, 42, true]),
        ),
        (
            "physical loss rejects unknown outcome without replay on either lifetime",
            r#"const sent = [], calls = new Invocations((operation,input) => JSON.stringify({Invoke:{id:1,operation,input}}), frame => sent.push(frame));
try {
  const result = calls.invoke('test', {}).then(() => null, e => e.message);
  calls.receive(JSON.stringify({Events:[{Accepted:{id:1}}]})); calls.detached();
  calls.receive(JSON.stringify({Attached:{resumed:true}}));
  const resumedSends = sent.length; calls.detached(); calls.receive(JSON.stringify({Attached:{resumed:false}}));
  return [resumedSends, await result, sent.length];
} finally { calls.close(); }"#,
            json!([
                1,
                "Physical connection lost; outstanding outcomes are unknown",
                1
            ]),
        ),
        (
            "acceptance timeout reports unknown outcome instead of retransmitting",
            r#"const sent = [], calls = new Invocations((operation,input) => JSON.stringify({Invoke:{id:1,operation,input}}), frame => sent.push(frame));
let outcome = 'pending';
try {
  void calls.invoke('example.change', {}).then(() => {outcome='resolved';}, e => {outcome=e.message;});
  await new Promise(resolve => setTimeout(resolve, 2100));
  return [sent.length, outcome];
} finally { calls.close(); }"#,
            json!([1, "Invocation acceptance timed out; outcome is unknown"]),
        ),
        (
            "send failure reports unknown outcome without retry",
            r#"let sends = 0;
const calls = new Invocations((operation,input) => JSON.stringify({Invoke:{id:1,operation,input}}), () => {sends++; throw new Error('socket unavailable');});
let outcome = 'pending';
try {
  void calls.invoke('example.change', {}).then(() => {outcome='resolved';}, e => {outcome=e.message;});
  await Promise.resolve();
  return [sends, outcome];
} finally { calls.close(); }"#,
            json!([1, "Invocation delivery failed; outcome is unknown"]),
        ),
    ];
    let mut ran = 0;
    for (name, stimulus, expected) in cases {
        if !filter.is_empty() && !name.contains(filter) {
            continue;
        }
        observation(browser, &host.base, name, stimulus, expected).await?;
        ran += 1;
    }
    for terminal in [
        json!({"Failed":"StaleConnection"}),
        json!({"Failed":"InvalidBearer"}),
        json!("Detached"),
    ] {
        let name = format!("terminal {terminal} rejects accepted call without replay");
        if !filter.is_empty() && !name.contains(filter) {
            continue;
        }
        let stimulus = format!(
            r#"const f = fixture(async () => ({{id:'alice'}}));
try {{
  await f.runtime.refresh(); f.sockets[0].receive({{Attached:{{resumed:false}}}}); f.sockets[0].receive({{manifest:true}});
  let outcome = 'pending'; const call = f.runtime.invoke('example',null).then(() => {{outcome='resolved';}}, () => {{outcome='rejected';}});
  f.sockets[0].receive({{Events:[{{Accepted:{{id:1}}}}]}}); f.sockets[0].close(); await f.runtime.refresh();
  f.sockets[1].receive({terminal}); await Promise.resolve(); await Promise.resolve();
  const observed = [outcome, f.sockets[0].sent.length, f.sockets[1].sent.length]; await call; return observed;
}} finally {{ f.runtime.close(); }}"#
        );
        observation(
            browser,
            &host.base,
            &name,
            &stimulus,
            json!(["rejected", 1, 0]),
        )
        .await?;
        ran += 1;
    }
    ensure!(ran > 0, "no client cases match {filter:?}");
    println!("PASS client {ran} contracts");
    Ok(())
}

async fn observation(
    browser: &Browser,
    base: &str,
    name: &str,
    stimulus: &str,
    expected: Value,
) -> Result<()> {
    let session = Session::new(browser).await?;
    let result = async {
        session.ui.goto(base).await?;
        session.ui.wait("!!window.snapClientFixture", json!(true)).await?;
        let observed = session.ui.eval(&format!("(async () => {{ const {{ fixture, deferred, binding, Invocations }} = window.snapClientFixture; {stimulus} }})()" )).await?;
        ensure!(observed == expected, "{name}: expected {expected}, observed {observed}");
        Ok(())
    }.await;
    session.finish(result).await
}
