//! Browser client host. Rust owns requests, timers and cancellation; JS supplies browser facilities.
use futures_util::{
    future::{AbortHandle, Abortable, Either, select},
    pin_mut,
};
use gloo_timers::future::TimeoutFuture;
use snap_client::application::{Application, Input, Step};
use snap_protocol::{Error, Invocation};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

#[derive(Clone)]
pub struct Http {
    base: String,
    build: String,
}

impl Http {
    pub fn new(base: String, build: String) -> Self {
        Self { base, build }
    }

    pub async fn query(&self, invocation: &Invocation) -> Result<String, Error> {
        let path = invocation
            .key
            .split('.')
            .map(|part| String::from(js_sys::encode_uri_component(part)))
            .collect::<Vec<_>>()
            .join("/");
        let url =
            web_sys::Url::new_with_base(&format!("/{path}"), &self.base).map_err(unavailable)?;
        let controller = web_sys::AbortController::new().map_err(unavailable)?;
        let _cancel_on_drop = Cancel(controller.clone());
        let options = web_sys::RequestInit::new();
        options.set_credentials(web_sys::RequestCredentials::Include);
        options.set_signal(Some(&controller.signal()));
        let request =
            web_sys::Request::new_with_str_and_init(&url.href(), &options).map_err(unavailable)?;
        request
            .headers()
            .set("x-snap-operation-id", &invocation.operation_id)
            .map_err(unavailable)?;
        request
            .headers()
            .set("x-snap-build", &self.build)
            .map_err(unavailable)?;
        let work = async {
            let global = js_sys::global();
            let fetch: js_sys::Function = js_sys::Reflect::get(&global, &"fetch".into())
                .map_err(unavailable)?
                .dyn_into()
                .map_err(unavailable)?;
            let promise: js_sys::Promise = fetch
                .call1(&global, &request)
                .map_err(unavailable)?
                .dyn_into()
                .map_err(unavailable)?;
            let response: web_sys::Response = JsFuture::from(promise)
                .await
                .map_err(unavailable)?
                .dyn_into()
                .map_err(unavailable)?;
            let text = JsFuture::from(response.text().map_err(unavailable)?)
                .await
                .map_err(unavailable)?;
            text.as_string()
                .ok_or_else(|| failure("Response is not text"))
        };
        let timeout = TimeoutFuture::new(5_000);
        pin_mut!(work, timeout);
        match select(work, timeout).await {
            Either::Left((result, _)) => result,
            Either::Right(_) => Err(failure("Query timed out")),
        }
    }
}

struct Cancel(web_sys::AbortController);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub struct Running<S> {
    snapshot: Rc<RefCell<S>>,
    abort: AbortHandle,
    finished: Option<futures_channel::oneshot::Receiver<()>>,
}

impl<S: Clone> Running<S> {
    pub fn snapshot(&self) -> S {
        self.snapshot.borrow().clone()
    }
    pub async fn close(&mut self) {
        self.abort.abort();
        if let Some(finished) = self.finished.take() {
            let _ = finished.await;
        }
    }
}
impl<S> Drop for Running<S> {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

pub fn start<A>(
    mut app: A,
    http: Http,
    notify: impl Fn(&A::Snapshot) + 'static,
) -> Running<A::Snapshot>
where
    A: Application + 'static,
    A::Snapshot: 'static,
{
    let snapshot = Rc::new(RefCell::new(app.snapshot()));
    let observed = snapshot.clone();
    let (abort, registration) = AbortHandle::new_pair();
    let (done, finished) = futures_channel::oneshot::channel();
    wasm_bindgen_futures::spawn_local(async move {
        let work = async move {
            let mut input = Input::Start;
            loop {
                let step = app.update(input);
                let next = app.snapshot();
                *observed.borrow_mut() = next.clone();
                notify(&next);
                input = match step {
                    Step::Query(invocation) => Input::Completed {
                        result: http.query(&invocation).await,
                        at: js_sys::Date::now() as u64,
                    },
                    Step::Wait { milliseconds } => {
                        TimeoutFuture::new(milliseconds).await;
                        Input::Wake
                    }
                    Step::Stop => break,
                };
            }
        };
        let _ = Abortable::new(work, registration).await;
        let _ = done.send(());
    });
    Running {
        snapshot,
        abort,
        finished: Some(finished),
    }
}

pub fn failure(message: &str) -> Error {
    Error::UnavailableError {
        message: message.into(),
    }
}
fn unavailable(value: JsValue) -> Error {
    failure(&format!("{value:?}"))
}
