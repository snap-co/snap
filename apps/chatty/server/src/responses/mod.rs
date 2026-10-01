//! Host-owned Responses conversations. Reasoning items are opaque provider
//! data, preserved with their assistant/tool items rather than decoded or invented.
extern crate alloc;
mod http;
use alloc::{
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
pub use http::Http;
use serde_json::{Value, json};
use snap_http::{
    FutureValue,
    client::{Body, Client, Outgoing, collect},
};

#[derive(Clone)]
pub struct Config {
    pub endpoint: String,
    pub model: String,
    pub key: String,
    pub max_output_tokens: u32,
}
#[derive(Clone)]
pub enum Event {
    Text(String),
    Summary(String),
}
pub struct Completion {
    pub output: Vec<Value>,
    pub text: String,
    pub summary: String,
    pub usage: Value,
    pub complete: bool,
}

/// Stream one model step. Callers own tool execution and append tool results to
/// the next step. Unknown outcomes are errors, never automatic generation retries.
pub async fn generate<C: Client>(
    client: &C,
    config: &Config,
    input: Vec<Value>,
    session: &str,
    effort: &str,
    tools: Vec<Value>,
    mut observe: impl FnMut(Event) -> FutureValue<Result<(), String>>,
) -> Result<Completion, String> {
    if config.key.is_empty() {
        return Err("Configure OPENCODE_API_KEY to enable the model".into());
    }
    if !matches!(effort, "minimal" | "low" | "medium" | "high" | "xhigh") {
        return Err("Unsupported reasoning effort".into());
    }
    let body=json!({"model":config.model,"input":input,"stream":true,"store":false,"include":["reasoning.encrypted_content"],"reasoning":{"effort":effort,"summary":"auto"},"max_output_tokens":config.max_output_tokens,"tools":tools,"parallel_tool_calls":false,"instructions":include_str!("../../../prompts/assistant.md").trim()}).to_string().into_bytes();
    let mut response = client
        .send(Outgoing {
            method: "POST",
            url: config.endpoint.clone(),
            headers: vec![
                ("authorization".into(), format!("Bearer {}", config.key)),
                ("content-type".into(), "application/json".into()),
                ("accept".into(), "text/event-stream".into()),
                ("user-agent".into(), "chatty/0.1".into()),
                ("x-opencode-session".into(), session.into()),
            ],
            body,
            max_bytes: 8 * 1024 * 1024,
            timeout_ms: 150_000,
        })
        .await?;
    if response.status != 200 {
        let bytes = collect(&mut response.body, 32 * 1024)
            .await
            .unwrap_or_default();
        let error = serde_json::from_slice::<Value>(&bytes).ok();
        let message = error
            .as_ref()
            .and_then(|e| e["error"]["message"].as_str())
            .unwrap_or("Model request failed");
        return Err(format!(
            "Model HTTP {}: {}",
            response.status,
            message.chars().take(400).collect::<String>()
        ));
    }
    let mut pending = Vec::new();
    let mut data = String::new();
    let mut terminal = None;
    'stream: while let Some(chunk) = response.body.chunk().await? {
        pending.extend(chunk);
        if pending.len() > 2 * 1024 * 1024 {
            return Err("Model event exceeded the size limit".into());
        }
        while let Some(end) = pending.iter().position(|b| *b == b'\n') {
            let line = pending.drain(..=end).collect::<Vec<_>>();
            let line = core::str::from_utf8(&line)
                .map_err(|_| "Model sent invalid UTF-8")?
                .trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if !data.is_empty() {
                    if data.trim() != "[DONE]" {
                        let event: Value = serde_json::from_str(&data)
                            .map_err(|_| "Model sent an invalid event")?;
                        match event["type"].as_str().unwrap_or("") {
                            "response.output_text.delta" => {
                                if let Some(text) = event["delta"].as_str() {
                                    observe(Event::Text(text.into())).await?;
                                }
                            }
                            "response.reasoning_summary_text.delta" => {
                                if let Some(text) = event["delta"].as_str() {
                                    observe(Event::Summary(text.into())).await?;
                                }
                            }
                            "response.completed" | "response.incomplete" => {
                                terminal = Some(event["response"].clone());
                                // The terminal event owns the result. A lingering
                                // HTTP body cannot turn a completed reply into a timeout.
                                break 'stream;
                            }
                            "response.failed" | "error" => {
                                return Err("The model could not complete this response".into());
                            }
                            _ => {}
                        }
                    }
                    data.clear();
                }
            } else if let Some(value) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value.strip_prefix(' ').unwrap_or(value));
                if data.len() > 2 * 1024 * 1024 {
                    return Err("Model event exceeded the size limit".into());
                }
            }
        }
    }
    let response = terminal.ok_or("Model stream ended without a completed response")?;
    let output = response["output"]
        .as_array()
        .ok_or("Model returned no output list")?
        .clone();
    let mut text = String::new();
    let mut summary = String::new();
    for item in &output {
        if item["type"] == "message"
            && let Some(parts) = item["content"].as_array()
        {
            for part in parts {
                if let Some(t) = part["text"].as_str() {
                    text.push_str(t);
                } else if let Some(t) = part["refusal"].as_str() {
                    text.push_str(t);
                }
            }
        }
        if item["type"] == "reasoning"
            && let Some(parts) = item["summary"].as_array()
        {
            for part in parts {
                if let Some(t) = part["text"].as_str() {
                    if !summary.is_empty() {
                        summary.push('\n');
                    }
                    summary.push_str(t);
                }
            }
        }
    }
    Ok(Completion {
        output,
        text,
        summary,
        usage: response["usage"].clone(),
        complete: response["status"] == "completed",
    })
}

/// Preserve provider output ordering and opaque reasoning content. This merely
/// supplies the required empty summary when the provider omitted the optional one.
pub fn replay(items: &[Value]) -> Vec<Value> {
    items
        .iter()
        .cloned()
        .map(|mut item| {
            if item["type"] == "reasoning" && item.get("summary").is_none() {
                item["summary"] = json!([]);
            }
            item
        })
        .collect()
}
