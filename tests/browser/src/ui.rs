//! Bounded observable reads and real CDP input. Sent mutations are never retried.
use anyhow::{Context, Result, anyhow, ensure};
use chromiumoxide::{
    Browser, Page,
    cdp::{
        browser_protocol::{
            browser::BrowserContextId,
            input::InsertTextParams,
            page::{
                AddScriptToEvaluateOnNewDocumentParams, GetNavigationHistoryParams,
                NavigateToHistoryEntryParams, ReloadParams,
            },
            target::{CreateBrowserContextParams, CreateTargetParams},
        },
        js_protocol::runtime::{RemoteObjectSubtype, RemoteObjectType},
    },
    error::CdpError,
};
use futures::{FutureExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::{sleep, timeout};

pub fn js(value: &str) -> String {
    serde_json::to_string(value).unwrap()
}
pub fn xpath_literal(value: &str) -> String {
    if !value.contains('\'') {
        format!("'{value}'")
    } else if !value.contains('"') {
        format!("\"{value}\"")
    } else {
        format!(
            "concat({})",
            value
                .split('\'')
                .map(|v| format!("'{v}'"))
                .collect::<Vec<_>>()
                .join(",\"'\",")
        )
    }
}

#[derive(Clone)]
pub struct Ui {
    pub page: Page,
}
fn navigation_race(error: &anyhow::Error) -> bool {
    error.downcast_ref::<CdpError>().is_some_and(|e| matches!(e, CdpError::Chrome(e) if e.message == "Cannot find context with specified id" || e.message == "Inspected target navigated or closed" || e.message.starts_with("Execution context was destroyed")))
}
impl Ui {
    pub fn new(page: Page) -> Self {
        Self { page }
    }
    pub async fn goto(&self, url: &str) -> Result<()> {
        self.page.goto(url).await?;
        Ok(())
    }
    pub async fn eval(&self, expression: &str) -> Result<Value> {
        let result = self
            .page
            .evaluate_expression(expression)
            .await
            .with_context(|| format!("evaluating {expression}"))?;
        if result.object().subtype == Some(RemoteObjectSubtype::Null)
            || result.object().r#type == RemoteObjectType::Undefined
        {
            Ok(Value::Null)
        } else {
            Ok(result.into_value()?)
        }
    }
    pub async fn wait(&self, expression: &str, expected: Value) -> Result<()> {
        self.wait_for(expression, expected, Duration::from_secs(10))
            .await
    }
    pub async fn wait_for(
        &self,
        expression: &str,
        expected: Value,
        duration: Duration,
    ) -> Result<()> {
        let mut last = Value::Null;
        timeout(duration, async {
            loop {
                match self.eval(expression).await {
                    Ok(value) => last = value,
                    Err(error) if navigation_race(&error) => {
                        sleep(Duration::from_millis(10)).await;
                        continue;
                    }
                    Err(error) => return Err(error),
                }
                if last == expected {
                    return Ok(());
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .with_context(|| {
            format!("waiting for {expression}, expected {expected}, observed {last}")
        })?
    }
    pub fn locator(&self, selector: &str) -> Locator {
        Locator {
            ui: self.clone(),
            query: Query::Css(selector.to_owned()),
        }
    }
    pub fn xpath(&self, path: &str) -> Locator {
        Locator {
            ui: self.clone(),
            query: Query::XPath(path.to_owned()),
        }
    }
    pub fn button(&self, name: &str) -> Locator {
        self.xpath(&format!(
            "//button[normalize-space(.) = {}]",
            xpath_literal(name)
        ))
    }
    pub fn label(&self, name: &str) -> Locator {
        let name = xpath_literal(name);
        self.xpath(&format!("//*[@id = //label[normalize-space(.) = {name}]/@for] | //label[normalize-space(.) = {name}]//*[self::input or self::textarea or self::select] | //*[@aria-label = {name}]"))
    }
    pub fn text(&self, text: &str) -> Locator {
        self.xpath(&format!(
            "//*[normalize-space(.) = {} and not(*[normalize-space(.) = {}])]",
            xpath_literal(text),
            xpath_literal(text)
        ))
    }
    pub fn heading(&self, name: &str) -> Locator {
        self.xpath(&format!(
            "//*[self::h1 or self::h2 or self::h3 or self::h4][normalize-space(.) = {}]",
            xpath_literal(name)
        ))
    }
    pub fn link(&self, name: &str) -> Locator {
        self.xpath(&format!(
            "//a[normalize-space(.) = {}]",
            xpath_literal(name)
        ))
    }
    pub async fn init(&self, script: &str) -> Result<()> {
        self.page
            .execute(AddScriptToEvaluateOnNewDocumentParams::new(script))
            .await?;
        Ok(())
    }
    pub async fn reload(&self) -> Result<()> {
        let marker = format!(
            "{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        );
        self.eval(&format!("window.__snapReload = {}", js(&marker)))
            .await?;
        self.page.execute(ReloadParams::default()).await?;
        self.wait(
            &format!("window.__snapReload !== {}", js(&marker)),
            json!(true),
        )
        .await
    }
    pub async fn history(&self, delta: i32) -> Result<()> {
        let h = self
            .page
            .execute(GetNavigationHistoryParams::default())
            .await?
            .result;
        let index = h.current_index + i64::from(delta);
        ensure!(index >= 0, "no history entry");
        let entry = h.entries.get(index as usize).context("no history entry")?;
        self.page
            .execute(NavigateToHistoryEntryParams::new(entry.id))
            .await?;
        self.wait("location.href", json!(entry.url)).await
    }
}

#[derive(Clone)]
enum Query {
    Css(String),
    XPath(String),
}
#[derive(Clone)]
pub struct Locator {
    ui: Ui,
    query: Query,
}
impl Locator {
    fn nodes(&self) -> String {
        match &self.query {
            Query::Css(s) => format!("[...document.querySelectorAll({})]", js(s)),
            Query::XPath(s) => format!(
                "(() => {{const r=document.evaluate({},document,null,XPathResult.ORDERED_NODE_SNAPSHOT_TYPE,null); return Array.from({{length:r.snapshotLength}},(_,i)=>r.snapshotItem(i));}})()",
                js(s)
            ),
        }
    }
    pub async fn count(&self, count: usize) -> Result<()> {
        self.ui
            .wait(&format!("{}.length", self.nodes()), json!(count))
            .await
    }
    pub async fn visible(&self) -> Result<()> {
        self.ui.wait(&format!("(() => {{const n={};return n.length===1 && n[0].getClientRects().length>0 && getComputedStyle(n[0]).visibility!=='hidden';}})()",self.nodes()),json!(true)).await
    }
    pub async fn hidden(&self) -> Result<()> {
        self.ui.wait(&format!("(() => {{const n={};return n.length<=1 && n.every(e=>!e.getClientRects().length || getComputedStyle(e).visibility==='hidden');}})()",self.nodes()),json!(true)).await
    }
    pub async fn enabled(&self, enabled: bool) -> Result<()> {
        self.ui.wait(&format!("(() => {{const n={};return n.length===1 && n[0].matches(':disabled') === {};}})()",self.nodes(),!enabled),json!(true)).await
    }
    pub async fn checked(&self, checked: bool) -> Result<()> {
        self.ui
            .wait(&self.single("e.checked"), json!(checked))
            .await
    }
    pub async fn text(&self, text: &str) -> Result<()> {
        self.ui
            .wait(
                &self.single("e.textContent.replace(/\\s+/g,' ').trim()"),
                json!(text.split_whitespace().collect::<Vec<_>>().join(" ")),
            )
            .await
    }
    pub async fn contains(&self, text: &str) -> Result<()> {
        self.ui
            .wait(
                &self.single(&format!("e.textContent.includes({})", js(text))),
                json!(true),
            )
            .await
    }
    pub async fn value(&self, text: &str) -> Result<()> {
        self.ui.wait(&self.single("e.value"), json!(text)).await
    }
    pub async fn read_text(&self) -> Result<String> {
        Ok(self
            .ui
            .eval(&self.single("e.textContent"))
            .await?
            .as_str()
            .context("text read requires exactly one matching element")?
            .to_owned())
    }
    fn single(&self, expression: &str) -> String {
        format!(
            "(() => {{const n={};if(n.length!==1)return null;const e=n[0];return {expression};}})()",
            self.nodes()
        )
    }
    pub async fn click(&self) -> Result<()> {
        self.ui.page.bring_to_front().await?;
        // Hit-test the center before sending mouse input, so a covered control cannot
        // silently receive a synthetic DOM click. Scroll and re-read only before input.
        self.ui.wait(&format!("(() => {{const n={};return n.length===1&&!n[0].matches(':disabled')&&n[0].getClientRects().length>0;}})()",self.nodes()),json!(true)).await?;
        self.ui
            .wait(
                &self.single("(e.scrollIntoView({block:'center',inline:'center'}),true)"),
                json!(true),
            )
            .await?;
        self.ui.wait(&format!("(() => {{const e={}[0];if(!e)return false;const r=e.getBoundingClientRect(),h=document.elementFromPoint(r.x+r.width/2,r.y+r.height/2);return h===e||e.contains(h);}})()",self.nodes()),json!(true)).await?;
        self.ui.wait(&format!("(async () => {{const e={}[0];if(!e)return false;const box=()=>{{const r=e.getBoundingClientRect();return [r.x,r.y,r.width,r.height].join(',');}};const before=box();await new Promise(requestAnimationFrame);await new Promise(requestAnimationFrame);return e.isConnected && before===box();}})()",self.nodes()),json!(true)).await?;
        // Do not retain a remote element handle across OAuth redirects or dev
        // reloads. Resolve geometry on the current document before input. Once
        // input is dispatched, never retry the action: it may already have run.
        let point=timeout(Duration::from_secs(10),async {
            loop {
                match self.ui.eval(&self.single("(() => {const r=e.getBoundingClientRect();const x=r.x+r.width/2,y=r.y+r.height/2;const h=document.elementFromPoint(x,y);return e.getClientRects().length&&!e.matches(':disabled')&&(h===e||e.contains(h))?{x,y}:null;})()")).await {
                    Ok(value) if value["x"].is_number()&&value["y"].is_number()=>return Ok(chromiumoxide::layout::Point::new(value["x"].as_f64().unwrap(),value["y"].as_f64().unwrap())),
                    Ok(_)=>{},
                    Err(error) if navigation_race(&error)=>{},
                    Err(error)=>return Err(error),
                }
                sleep(Duration::from_millis(10)).await;
            }
        }).await.context("resolving click geometry")??;
        self.ui.page.click(point).await?;
        Ok(())
    }
    pub async fn fill(&self, value: &str) -> Result<()> {
        self.click().await?;
        let e = match &self.query {
            Query::Css(s) => self.ui.page.find_element(s).await?,
            Query::XPath(s) => self.ui.page.find_xpath(s).await?,
        };
        e.call_js_fn("function(){this.select();}", false).await?;
        self.ui.page.execute(InsertTextParams::new(value)).await?;
        self.value(value).await
    }
}

pub struct Session<'a> {
    browser: &'a Browser,
    pub context: BrowserContextId,
    pub ui: Ui,
    observers: std::sync::Mutex<
        Vec<
            chromiumoxide::listeners::EventStream<
                chromiumoxide::cdp::js_protocol::runtime::EventExceptionThrown,
            >,
        >,
    >,
}
impl<'a> Session<'a> {
    pub async fn new(browser: &'a Browser) -> Result<Self> {
        let context = browser
            .create_browser_context(
                CreateBrowserContextParams::builder()
                    .dispose_on_detach(true)
                    .build(),
            )
            .await?;
        let page = browser
            .new_page(
                CreateTargetParams::builder()
                    .url("about:blank")
                    .browser_context_id(context.clone())
                    .build()
                    .map_err(|e| anyhow!(e))?,
            )
            .await?;
        let session = Self {
            browser,
            context,
            ui: Ui::new(page),
            observers: Default::default(),
        };
        session.observe(&session.ui.page).await?;
        Ok(session)
    }
    pub async fn page(&self) -> Result<Ui> {
        let page = self
            .browser
            .new_page(
                CreateTargetParams::builder()
                    .url("about:blank")
                    .browser_context_id(self.context.clone())
                    .build()
                    .map_err(|e| anyhow!(e))?,
            )
            .await?;
        self.observe(&page).await?;
        Ok(Ui::new(page))
    }
    async fn observe(&self, page: &Page) -> Result<()> {
        let events = page
            .event_listener::<chromiumoxide::cdp::js_protocol::runtime::EventExceptionThrown>()
            .await?;
        self.observers.lock().unwrap().push(events);
        Ok(())
    }
    pub async fn finish(self, result: Result<()>) -> Result<()> {
        let _ = self.ui.eval("0").await;
        let mut errors = Vec::new();
        for events in self.observers.lock().unwrap().iter_mut() {
            while let Some(Some(event)) = events.next().now_or_never() {
                errors.push(format!("{:?}", event.exception_details));
            }
        }
        let result = result.and_then(|()| {
            ensure!(errors.is_empty(), "page exceptions: {errors:?}");
            Ok(())
        });
        if result.is_err()
            && let Some(path) = crate::support::artifacts()
        {
            for (index, page) in self
                .browser
                .pages()
                .await
                .unwrap_or_default()
                .iter()
                .enumerate()
            {
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
            let _ = std::fs::write(path.join("failure.txt"), format!("{result:?}"));
        }
        let cleanup = self
            .browser
            .dispose_browser_context(self.context.clone())
            .await;
        result?;
        cleanup?;
        Ok(())
    }
    pub async fn close(self) -> Result<()> {
        self.finish(Ok(())).await
    }
}
