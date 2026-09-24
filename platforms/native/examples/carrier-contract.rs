//! Independent Protocol provider used by the carrier and execution contracts.
use snap_native::{Lease, Reply};
use snap_protocol::{Invocation, Operation, Provider, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
#[derive(Default)]
struct App {
    gate: Arc<tokio::sync::Notify>,
    started: Arc<AtomicBool>,
    completed: Arc<AtomicUsize>,
}
impl Provider for App {
    type Context = Option<String>;
    type Output = Reply;
    fn operations(&self) -> impl Iterator<Item = Operation> {
        ["echo", "lease.resolve", "hold", "release", "status"]
            .into_iter()
            .map(|key| Operation { key })
    }
    fn invoke(
        &mut self,
        invocation: Invocation,
        token: Option<String>,
    ) -> impl core::future::Future<Output = Reply> + Send + 'static {
        let gate = self.gate.clone();
        let started = self.started.clone();
        let completed = self.completed.clone();
        async move {
            match invocation.key.as_str() {
                "lease.resolve" => {
                    let mut reply = Reply::new(Ok(json!({})));
                    if token.as_deref() == Some("fixture") {
                        reply.lease = Some(Lease {
                            id: "fixture".into(),
                            expires_at: snap_native::now() + 60_000,
                        });
                    }
                    reply
                }
                "hold" => {
                    started.store(true, Ordering::SeqCst);
                    gate.notified().await;
                    completed.fetch_add(1, Ordering::SeqCst);
                    Reply::new(Ok(json!("completed")))
                }
                "release" => {
                    gate.notify_one();
                    Reply::new(Ok(json!("released")))
                }
                "status" => Reply::new(Ok(
                    json!({"started":started.load(Ordering::SeqCst),"completed":completed.load(Ordering::SeqCst)}),
                )),
                _ => Reply::new(Ok(invocation.payload.unwrap_or_default())),
            }
        }
    }
}
fn main() -> std::io::Result<()> {
    let config = snap_native::Config::from_env("carrier-contract")?;
    let origin = format!("http://{}", config.address);
    let cookie = snap_native::cookie::Cookie::new(vec![7; 32], "fixture", false, 60)?;
    let mut bindings = vec![snap_native::web::Binding {
        key: "echo",
        http: Some(snap_native::web::Method::Post),
        socket: true,
    }];
    bindings.extend(["hold", "release", "status"].into_iter().map(|key| {
        snap_native::web::Binding {
            key,
            http: Some(snap_native::web::Method::Post),
            socket: false,
        }
    }));
    snap_native::run_application(
        App::default(),
        config,
        snap_native::Web {
            bindings,
            session: Some(snap_native::SessionCarrier {
                origin,
                cookie,
                identify: "lease.resolve",
            }),
        },
    )
}
