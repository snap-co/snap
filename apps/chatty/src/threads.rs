//! Private durable threads and retained generation workflows. Every read/write
//! checks owner authority. Runtime interruption is recorded and never replays tools.
use crate::{Config, Host, session::Actor, storage::*, tools};
use alloc::{
    boxed::Box,
    collections::BTreeSet,
    format,
    rc::Rc,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::cell::{Cell, RefCell};
use serde_json::{Value, json};
use snap_http::Response;
use snap_store::{Predicate as P, Query, Row, Statement as S, Store};

const MAX_TURNS: u32 = 200;
const CONTEXT_BYTES: usize = 192 * 1024;
#[derive(Clone)]
pub struct Threads<S, H> {
    pub store: S,
    pub host: H,
    pub config: Config,
    running: Rc<Cell<usize>>,
    accepting: Rc<RefCell<BTreeSet<String>>>,
}
/// Serializes only a thread's short acceptance phase. The set borrow is released
/// before every suspension; the owned permit is independent of generation lifetime.
struct Acceptance(Rc<RefCell<BTreeSet<String>>>, String);
impl Drop for Acceptance {
    fn drop(&mut self) {
        self.0.borrow_mut().remove(&self.1);
    }
}
struct Permit(Rc<Cell<usize>>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}
#[derive(Clone, Default)]
struct Progress {
    text: String,
    summary: String,
    output: Vec<Value>,
    tools: Vec<Value>,
    usage: Value,
    last_flush: u64,
    dirty: usize,
}
impl<D: Store, H: Host> Threads<D, H> {
    pub fn new(store: D, host: H, config: Config) -> Self {
        Self {
            store,
            host,
            config,
            running: Rc::new(Cell::new(0)),
            accepting: Rc::new(RefCell::new(BTreeSet::new())),
        }
    }
    async fn thread(&self, actor: &Actor, id: &str) -> Result<Row, Response> {
        let rows = tx(
            &self.store,
            vec![actor.lease(self.host.now())],
            vec![S::Select(owned(id, &actor.owner))],
        )
        .await?;
        rows[0]
            .first()
            .cloned()
            .ok_or_else(|| Response::error(404, "not_found", "Thread not found"))
    }
    pub async fn list(&self, actor: &Actor) -> Result<Value, Response> {
        let mut rows = tx(
            &self.store,
            vec![actor.lease(self.host.now())],
            vec![S::Select(
                Query::new(THREADS)
                    .matching(vec![P::eq("owner", actor.owner.clone())])
                    .ordered(&["updated", "id"])
                    .limit(201),
            )],
        )
        .await?
        .remove(0);
        rows.reverse();
        let threads = rows
            .iter()
            .map(thread_public)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(json!({"threads":threads}))
    }
    pub async fn create(&self, actor: &Actor, input: &Value) -> Result<Value, Response> {
        let quota = read(&self.store, id(OWNERS, &actor.owner)).await?;
        let count = quota
            .as_ref()
            .map(|r| number(r, "threads"))
            .transpose()?
            .unwrap_or(0);
        if count >= 200 {
            return Err(Response::error(
                409,
                "limit",
                "Delete a thread before creating more than 200",
            ));
        }
        let title = input["title"].as_str().unwrap_or("New thread").trim();
        if title.is_empty() || title.chars().count() > 100 {
            return Err(bad("Invalid thread title"));
        }
        let effort = input["effort"].as_str().unwrap_or("medium");
        if !valid_effort(effort) {
            return Err(bad("Invalid reasoning effort"));
        }
        let id = self.host.random()?;
        let now = self.host.now();
        let record = row(&[
            ("id", id.clone().into()),
            ("owner", actor.owner.clone().into()),
            ("title", title.into()),
            ("effort", effort.into()),
            ("created", (now as i64).into()),
            ("updated", (now as i64).into()),
            ("revision", 1i64.into()),
            ("active", "".into()),
        ]);
        let mut guards = vec![actor.lease(now)];
        let mut writes = vec![S::Insert {
            table: THREADS,
            row: record.clone(),
        }];
        if quota.is_some() {
            let mut q = crate::storage::id(OWNERS, &actor.owner);
            q.filter.push(P::eq("threads", count));
            guards.push(guard(q));
            writes.push(S::Update {
                table: OWNERS,
                filter: vec![P::eq("id", actor.owner.clone())],
                changes: row(&[("threads", (count + 1).into())]),
            });
        } else {
            guards.push(snap_store::Guard {
                query: crate::storage::id(OWNERS, &actor.owner),
                exists: false,
            });
            writes.push(S::Insert {
                table: OWNERS,
                row: row(&[("id", actor.owner.clone().into()), ("threads", 1i64.into())]),
            });
        }
        tx(&self.store, guards, writes).await?;
        thread_public(&record)
    }
    pub async fn view(&self, actor: &Actor, thread: &str) -> Result<Value, Response> {
        let record = self.thread(actor, thread).await?;
        let rows = tx(
            &self.store,
            vec![
                actor.lease(self.host.now()),
                guard(owned(thread, &actor.owner)),
            ],
            vec![S::Select(
                Query::new(TURNS)
                    .matching(vec![P::eq("thread_id", thread)])
                    .ordered(&["seq"])
                    .limit(MAX_TURNS + 1),
            )],
        )
        .await?
        .remove(0);
        let turns = rows
            .iter()
            .map(turn_public)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(json!({"thread":thread_public(&record)?,"turns":turns}))
    }
    pub async fn rename(&self, actor: &Actor, input: &Value) -> Result<Value, Response> {
        let thread = field(input, "thread_id")?;
        let record = self.thread(actor, thread).await?;
        let title = field(input, "title")?.trim();
        if title.is_empty() || title.chars().count() > 100 {
            return Err(bad("Invalid thread title"));
        }
        let effort = input["effort"].as_str().unwrap_or("medium");
        if !valid_effort(effort) {
            return Err(bad("Invalid reasoning effort"));
        }
        let revision = number(&record, "revision")?;
        let mut q = owned(thread, &actor.owner);
        q.filter.push(P::eq("revision", revision));
        tx(
            &self.store,
            vec![actor.lease(self.host.now()), guard(q)],
            vec![S::Update {
                table: THREADS,
                filter: vec![P::eq("id", thread)],
                changes: row(&[
                    ("title", title.into()),
                    ("effort", effort.into()),
                    ("revision", (revision + 1).into()),
                    ("updated", (self.host.now() as i64).into()),
                ]),
            }],
        )
        .await?;
        self.view(actor, thread).await
    }
    pub async fn delete(&self, actor: &Actor, thread: &str) -> Result<Value, Response> {
        self.thread(actor, thread).await?;
        let quota = read(&self.store, id(OWNERS, &actor.owner))
            .await?
            .ok_or_else(unavailable)?;
        let count = number(&quota, "threads")?;
        if count <= 0 {
            return Err(unavailable());
        }
        let mut q = id(OWNERS, &actor.owner);
        q.filter.push(P::eq("threads", count));
        tx(
            &self.store,
            vec![
                actor.lease(self.host.now()),
                guard(owned(thread, &actor.owner)),
                guard(q),
            ],
            vec![
                S::Delete {
                    table: TURNS,
                    filter: vec![P::eq("thread_id", thread)],
                },
                delete(THREADS, thread),
                S::Update {
                    table: OWNERS,
                    filter: vec![P::eq("id", actor.owner.clone())],
                    changes: row(&[("threads", (count - 1).into())]),
                },
            ],
        )
        .await?;
        Ok(json!({"deleted":true}))
    }
    pub async fn send(&self, actor: Actor, input: &Value) -> Result<Value, Response> {
        let thread = field(input, "thread_id")?.to_string();
        let prompt = field(input, "message")?.trim().to_string();
        let request_id = field(input, "request_id")?.to_string();
        if prompt.is_empty() || prompt.len() > 32 * 1024 {
            return Err(bad("Message must be between 1 byte and 32 KiB"));
        }
        if request_id.is_empty()
            || request_id.len() > 100
            || !request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            return Err(bad("Invalid request identifier"));
        }
        // A duplicate can arrive while its first receipt is still committing.
        // Wait for that acceptance before receipt lookup/capacity reservation.
        // Distinct threads keep independent admission, and Store guards still
        // fence all persisted writes. This never retries model or tool work.
        let mut acceptance = None;
        for _ in 0..750 {
            if self.accepting.borrow_mut().insert(thread.clone()) {
                acceptance = Some(Acceptance(self.accepting.clone(), thread.clone()));
                break;
            }
            self.host.sleep(20).await;
        }
        let _acceptance = acceptance
            .ok_or_else(|| Response::error(503, "busy", "Thread acceptance is in progress"))?;
        let record = self.thread(&actor, &thread).await?;
        if let Some(existing) = read(
            &self.store,
            Query::new(TURNS)
                .matching(vec![
                    P::eq("thread_id", thread.clone()),
                    P::eq("request_id", request_id.clone()),
                ])
                .limit(1),
        )
        .await?
        {
            if text(&existing, "user")? != prompt {
                return Err(conflict());
            }
            return Ok(json!({"turn_id":text(&existing,"id")?,"accepted":true}));
        }
        if !text(&record, "active")?.is_empty() {
            return Err(Response::error(
                409,
                "busy",
                "This thread already has a reply in progress",
            ));
        }
        if self.running.get() >= 4 {
            return Err(Response::error(
                503,
                "busy",
                "Four replies are already in progress",
            ));
        }
        if self.config.model.key.is_empty() {
            return Err(Response::error(
                503,
                "configuration",
                "OPENCODE_API_KEY is not configured",
            ));
        }
        let history = tx(
            &self.store,
            vec![
                actor.lease(self.host.now()),
                guard(owned(&thread, &actor.owner)),
            ],
            vec![S::Select(
                Query::new(TURNS)
                    .matching(vec![P::eq("thread_id", thread.clone())])
                    .ordered(&["seq"])
                    .limit(MAX_TURNS + 1),
            )],
        )
        .await?
        .remove(0);
        if history.len() >= MAX_TURNS as usize {
            return Err(Response::error(
                409,
                "limit",
                "This thread has reached 200 turns; start another thread",
            ));
        }
        let (context, omitted) = context(&history, &prompt)?;
        let turn = self.host.random()?;
        let now = self.host.now();
        let revision = number(&record, "revision")?;
        let effort = text(&record, "effort")?;
        // History loading above suspends. Reserve synchronously with the final
        // capacity check so distinct threads cannot all spend the same free slot.
        if self.running.get() >= 4 {
            return Err(Response::error(
                503,
                "busy",
                "Four replies are already in progress",
            ));
        }
        self.running.set(self.running.get() + 1);
        let permit = Permit(self.running.clone());
        let mut q = owned(&thread, &actor.owner);
        q.filter
            .extend([P::eq("active", ""), P::eq("revision", revision)]);
        let title = if text(&record, "title")? == "New thread" {
            prompt.chars().take(60).collect::<String>()
        } else {
            text(&record, "title")?
        };
        tx(
            &self.store,
            vec![actor.lease(now), guard(q)],
            vec![
                S::Insert {
                    table: TURNS,
                    row: row(&[
                        ("id", turn.clone().into()),
                        ("thread_id", thread.clone().into()),
                        ("request_id", request_id.into()),
                        ("seq", revision.into()),
                        ("user", prompt.into()),
                        ("output", "[]".into()),
                        ("text", "".into()),
                        ("summary", "".into()),
                        ("tools", "[]".into()),
                        (
                            "usage",
                            json!({"context_omitted":omitted}).to_string().into(),
                        ),
                        ("status", "running".into()),
                        ("error", "".into()),
                        ("created", (now as i64).into()),
                        ("updated", (now as i64).into()),
                    ]),
                },
                S::Update {
                    table: THREADS,
                    filter: vec![P::eq("id", thread.clone())],
                    changes: row(&[
                        ("active", turn.clone().into()),
                        ("revision", (revision + 1).into()),
                        ("title", title.into()),
                        ("updated", (now as i64).into()),
                    ]),
                },
            ],
        )
        .await?;
        let app = self.clone();
        let id = turn.clone();
        self.host.spawn(Box::pin(async move {
            let _permit = permit;
            let progress = Rc::new(RefCell::new(Progress {
                usage: json!({"context_omitted":omitted}),
                ..Progress::default()
            }));
            let result = app
                .generate(&actor, &thread, &id, context, &effort, progress.clone())
                .await;
            let status = if result.is_ok() { "complete" } else { "failed" };
            let error = result.err().unwrap_or_default();
            let snapshot = progress.borrow().clone();
            if app
                .finish(&actor, &thread, &id, &snapshot, status, &error)
                .await
                .is_err()
            {
                app.abandon(&actor, &thread, &id).await;
            }
        }));
        Ok(json!({"turn_id":turn,"accepted":true}))
    }
    pub async fn cancel(&self, actor: &Actor, input: &Value) -> Result<Value, Response> {
        let thread = field(input, "thread_id")?;
        let turn = field(input, "turn_id")?;
        let mut q = owned(thread, &actor.owner);
        q.filter.push(P::eq("active", turn));
        tx(
            &self.store,
            vec![actor.lease(self.host.now()), guard(q)],
            vec![
                S::Update {
                    table: TURNS,
                    filter: vec![
                        P::eq("id", turn),
                        P::eq("thread_id", thread),
                        P::eq("status", "running"),
                    ],
                    changes: row(&[
                        ("status", "cancelled".into()),
                        (
                            "error",
                            "Stopped by you. In-flight remote work may still finish.".into(),
                        ),
                        ("updated", (self.host.now() as i64).into()),
                    ]),
                },
                S::Update {
                    table: THREADS,
                    filter: vec![P::eq("id", thread)],
                    changes: row(&[("active", "".into())]),
                },
            ],
        )
        .await?;
        Ok(json!({"cancelled":true}))
    }
    fn turn_guards(&self, actor: &Actor, thread: &str, turn: &str) -> Vec<snap_store::Guard> {
        let mut q = owned(thread, &actor.owner);
        q.filter.push(P::eq("active", turn));
        let mut tq = id(TURNS, turn);
        tq.filter
            .extend([P::eq("thread_id", thread), P::eq("status", "running")]);
        vec![actor.lease(self.host.now()), guard(q), guard(tq)]
    }
    async fn save(
        &self,
        actor: &Actor,
        thread: &str,
        turn: &str,
        progress: &Progress,
    ) -> Result<(), String> {
        tx(
            &self.store,
            self.turn_guards(actor, thread, turn),
            vec![S::Update {
                table: TURNS,
                filter: vec![P::eq("id", turn)],
                changes: progress_row(progress, self.host.now()),
            }],
        )
        .await
        .map_err(|_| "Reply was stopped or session ended".to_string())?;
        Ok(())
    }
    async fn finish(
        &self,
        actor: &Actor,
        thread: &str,
        turn: &str,
        progress: &Progress,
        status: &str,
        error: &str,
    ) -> Result<(), Response> {
        let mut changes = progress_row(progress, self.host.now());
        changes.insert("status".into(), status.into());
        changes.insert("error".into(), error.into());
        tx(
            &self.store,
            self.turn_guards(actor, thread, turn),
            vec![
                S::Update {
                    table: TURNS,
                    filter: vec![P::eq("id", turn)],
                    changes,
                },
                S::Update {
                    table: THREADS,
                    filter: vec![P::eq("id", thread)],
                    changes: row(&[
                        ("active", "".into()),
                        ("updated", (self.host.now() as i64).into()),
                    ]),
                },
            ],
        )
        .await?;
        Ok(())
    }
    /// Retained work may release its own slot after session loss, without granting
    /// new user authority or writing a late model result. Cancel/delete fences win.
    async fn abandon(&self, actor: &Actor, thread: &str, turn: &str) {
        let mut q = owned(thread, &actor.owner);
        q.filter.push(P::eq("active", turn));
        let mut tq = id(TURNS, turn);
        tq.filter
            .extend([P::eq("thread_id", thread), P::eq("status", "running")]);
        let _ = tx(
            &self.store,
            vec![guard(q), guard(tq)],
            vec![
                S::Update {
                    table: TURNS,
                    filter: vec![P::eq("id", turn)],
                    changes: row(&[
                        ("status", "interrupted".into()),
                        ("error", "The session ended during this reply.".into()),
                        ("updated", (self.host.now() as i64).into()),
                    ]),
                },
                S::Update {
                    table: THREADS,
                    filter: vec![P::eq("id", thread)],
                    changes: row(&[("active", "".into())]),
                },
            ],
        )
        .await;
    }
    async fn generate(
        &self,
        actor: &Actor,
        thread: &str,
        turn: &str,
        mut input: Vec<Value>,
        effort: &str,
        progress: Rc<RefCell<Progress>>,
    ) -> Result<(), String> {
        let mut calls = 0;
        for _step in 0..5 {
            if serde_json::to_vec(&input)
                .map_err(|_| "Invalid model context")?
                .len()
                > CONTEXT_BYTES + 128 * 1024
            {
                return Err("This reply exceeded the context budget".into());
            }
            let app = self.clone();
            let observer = progress.clone();
            let actor_copy = actor.clone();
            let thread_copy = thread.to_string();
            let turn_copy = turn.to_string();
            let prefix_text = progress.borrow().text.clone();
            let prefix_summary = progress.borrow().summary.clone();
            let result = snap_llm::generate(
                &self.host,
                &self.config.model,
                input.clone(),
                thread,
                effort,
                tools::definitions(&self.config),
                move |event| {
                    let app = app.clone();
                    let p = observer.clone();
                    let actor = actor_copy.clone();
                    let thread = thread_copy.clone();
                    let turn = turn_copy.clone();
                    Box::pin(async move {
                        let now = app.host.now();
                        let flush = {
                            let mut p = p.borrow_mut();
                            match event {
                                snap_llm::Event::Text(s) => {
                                    p.dirty += s.len();
                                    p.text.push_str(&s);
                                }
                                snap_llm::Event::Summary(s) => {
                                    p.dirty += s.len();
                                    p.summary.push_str(&s);
                                }
                            }
                            if p.text.len() + p.summary.len() > 512 * 1024 {
                                return Err("Reply exceeded its display limit".into());
                            }
                            let flush = now.saturating_sub(p.last_flush) >= 250 || p.dirty >= 1024;
                            if flush {
                                p.last_flush = now;
                                p.dirty = 0;
                            }
                            flush
                        };
                        if flush {
                            let snapshot = p.borrow().clone();
                            app.save(&actor, &thread, &turn, &snapshot).await?;
                        }
                        Ok(())
                    })
                },
            )
            .await?;
            {
                let mut p = progress.borrow_mut();
                p.text = format!("{prefix_text}{}", result.text);
                p.summary = format!("{prefix_summary}{}", result.summary);
                p.output.extend(result.output.clone());
                add_usage(&mut p.usage, &result.usage);
            }
            let snapshot = progress.borrow().clone();
            self.save(actor, thread, turn, &snapshot).await?;
            if !result.complete {
                return Err(
                    "The model reached its output limit. Send a follow-up to continue.".into(),
                );
            }
            input.extend(snap_llm::replay(&result.output));
            let functions: Vec<_> = result
                .output
                .iter()
                .filter(|v| v["type"] == "function_call")
                .collect();
            if functions.is_empty() {
                return Ok(());
            }
            for function in functions {
                calls += 1;
                if calls > 8 {
                    return Err("This reply reached its eight-tool-call limit".into());
                }
                let call = function["call_id"]
                    .as_str()
                    .ok_or("Model tool call has no ID")?;
                let name = function["name"]
                    .as_str()
                    .ok_or("Model tool call has no name")?;
                let args: Value = serde_json::from_str(
                    function["arguments"]
                        .as_str()
                        .ok_or("Missing tool arguments")?,
                )
                .map_err(|_| "Invalid tool arguments")?;
                {
                    progress.borrow_mut().tools.push(
                        json!({"call_id":call,"name":name,"arguments":args,"status":"running"}),
                    );
                }
                let snapshot = progress.borrow().clone();
                self.save(actor, thread, turn, &snapshot).await?;
                let value = match tools::execute(&self.host, &self.config, &actor.owner, name, args)
                    .await
                {
                    Ok(value) => json!({"ok":true,"result":value}),
                    Err(error) => json!({"ok":false,"error":error}),
                };
                let output = json!({"type":"function_call_output","call_id":call,"output":value.to_string()});
                {
                    let mut p = progress.borrow_mut();
                    let tool = p.tools.last_mut().ok_or("Missing tool record")?;
                    tool["status"] = "complete".into();
                    tool["result"] = value;
                    p.output.push(output.clone());
                }
                input.push(output);
                let snapshot = progress.borrow().clone();
                self.save(actor, thread, turn, &snapshot).await?;
            }
        }
        Err("This reply reached its five-model-step limit".into())
    }
}
fn owned(thread: &str, owner: &str) -> Query {
    Query::new(THREADS)
        .matching(vec![P::eq("id", thread), P::eq("owner", owner)])
        .limit(1)
}
fn thread_public(r: &Row) -> Result<Value, Response> {
    Ok(
        json!({"id":text(r,"id")?,"title":text(r,"title")?,"effort":text(r,"effort")?,"active_turn":text(r,"active")?,"created":number(r,"created")?,"updated":number(r,"updated")?}),
    )
}
fn turn_public(r: &Row) -> Result<Value, Response> {
    Ok(
        json!({"id":text(r,"id")?,"request_id":text(r,"request_id")?,"user":text(r,"user")?,"text":text(r,"text")?,"summary":text(r,"summary")?,"tools":json(r,"tools")?,"usage":json(r,"usage")?,"status":text(r,"status")?,"error":text(r,"error")?,"created":number(r,"created")?}),
    )
}
fn progress_row(p: &Progress, now: u64) -> Row {
    row(&[
        ("text", p.text.clone().into()),
        ("summary", p.summary.clone().into()),
        (
            "output",
            serde_json::to_string(&p.output)
                .unwrap_or_else(|_| "[]".into())
                .into(),
        ),
        (
            "tools",
            serde_json::to_string(&p.tools)
                .unwrap_or_else(|_| "[]".into())
                .into(),
        ),
        ("usage", stringify(&p.usage)),
        ("updated", (now as i64).into()),
    ])
}
fn add_usage(total: &mut Value, step: &Value) {
    for (name, value) in [
        ("input_tokens", step["input_tokens"].as_u64().unwrap_or(0)),
        ("output_tokens", step["output_tokens"].as_u64().unwrap_or(0)),
        (
            "reasoning_tokens",
            step["output_tokens_details"]["reasoning_tokens"]
                .as_u64()
                .unwrap_or(0),
        ),
        (
            "cached_tokens",
            step["input_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap_or(0),
        ),
        ("model_steps", 1),
    ] {
        total[name] = total[name]
            .as_u64()
            .unwrap_or(0)
            .saturating_add(value)
            .into();
    }
}
fn context(history: &[Row], prompt: &str) -> Result<(Vec<Value>, usize), Response> {
    let mut turns = Vec::new();
    let mut size = prompt.len();
    let mut omitted = 0;
    let mut cutoff = false;
    for turn in history.iter().rev() {
        if cutoff || text(turn, "status")? != "complete" {
            omitted += 1;
            continue;
        }
        let output = json(turn, "output")?;
        let Some(output) = output.as_array() else {
            return Err(unavailable());
        };
        let mut items = vec![json!({"role":"user","content":text(turn,"user")?})];
        items.extend(snap_llm::replay(output));
        let bytes = serde_json::to_vec(&items).map_err(|_| unavailable())?.len();
        if size + bytes > CONTEXT_BYTES {
            cutoff = true;
            omitted += 1;
            continue;
        }
        size += bytes;
        turns.push(items);
    }
    turns.reverse();
    let mut input = turns.into_iter().flatten().collect::<Vec<_>>();
    input.push(json!({"role":"user","content":prompt}));
    Ok((input, omitted))
}
pub fn field<'a>(v: &'a Value, key: &str) -> Result<&'a str, Response> {
    v[key]
        .as_str()
        .ok_or_else(|| bad(&format!("Missing {key}")))
}
fn valid_effort(s: &str) -> bool {
    matches!(s, "minimal" | "low" | "medium" | "high" | "xhigh")
}
fn bad(message: &str) -> Response {
    Response::error(400, "invalid_request", message)
}
