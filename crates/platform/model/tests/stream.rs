use serde_json::json;
use snap_http::client::{Body, Client, Incoming, Outgoing};
use snap_model_local::{Config, Event, generate};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
struct Fixture {
    chunks: Vec<Vec<u8>>,
    requests: Arc<Mutex<Vec<Outgoing>>>,
}
struct Chunks(VecDeque<Vec<u8>>);
impl Body for Chunks {
    async fn chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
        Ok(self.0.pop_front())
    }
}
impl Client for Fixture {
    type Body = Chunks;
    async fn send(&self, request: Outgoing) -> Result<Incoming<Chunks>, String> {
        self.requests.lock().unwrap().push(request);
        Ok(Incoming {
            status: 200,
            headers: vec![],
            body: Chunks(self.chunks.clone().into()),
        })
    }
}
fn config() -> Config {
    Config {
        endpoint: "http://fixture/responses".into(),
        model: "fixture".into(),
        key: "fixture-key".into(),
        max_output_tokens: 8192,
    }
}

#[tokio::test]
async fn fragmented_stream_keeps_provider_order_and_only_displays_explicit_text() {
    let output = json!([{"type":"reasoning","encrypted_content":"opaque","summary":[{"text":"Summary"}]},{"type":"message","phase":"final_answer","content":[{"type":"output_text","text":"café"}]},{"type":"function_call","call_id":"call","name":"read_file","arguments":"{}"}]);
    let stream = format!(
        "data: {}\n\ndata: {}\n\ndata: {}\n\ninvalid trailing bytes",
        json!({"type":"response.output_text.delta","delta":"café"}),
        json!({"type":"response.reasoning_summary_text.delta","delta":"Summary"}),
        json!({"type":"response.completed","response":{"status":"completed","output":output,"usage":{"output_tokens":4}}})
    );
    let fixture = Fixture {
        chunks: stream.as_bytes().chunks(1).map(<[u8]>::to_vec).collect(),
        requests: Arc::new(Mutex::new(vec![])),
    };
    let events = Arc::new(Mutex::new(Vec::new()));
    let recorded = events.clone();
    let result = generate(
        &fixture,
        &config(),
        vec![],
        "thread",
        "medium",
        vec![],
        move |event| {
            let recorded = recorded.clone();
            Box::pin(async move {
                recorded.lock().unwrap().push(match event {
                    Event::Text(s) => format!("text:{s}"),
                    Event::Summary(s) => format!("summary:{s}"),
                });
                Ok(())
            })
        },
    )
    .await
    .unwrap();
    assert!(result.complete);
    assert_eq!(result.text, "café");
    assert_eq!(result.summary, "Summary");
    assert_eq!(result.output, output.as_array().unwrap().clone());
    assert_eq!(
        *events.lock().unwrap(),
        vec!["text:café", "summary:Summary"]
    );
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["store"], false);
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
}

#[tokio::test]
async fn missing_terminal_invalid_events_and_observer_failure_never_retry() {
    for stream in [
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n",
        "data: invalid\n\n",
        "data: {\"type\":\"response.failed\"}\n\n",
    ] {
        let fixture = Fixture {
            chunks: vec![stream.as_bytes().to_vec()],
            requests: Arc::new(Mutex::new(vec![])),
        };
        assert!(
            generate(
                &fixture,
                &config(),
                vec![],
                "thread",
                "medium",
                vec![],
                |_| Box::pin(async { Ok(()) })
            )
            .await
            .is_err()
        );
        assert_eq!(fixture.requests.lock().unwrap().len(), 1);
    }
    let fixture = Fixture {
        chunks: vec![
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n".to_vec(),
        ],
        requests: Arc::new(Mutex::new(vec![])),
    };
    assert!(
        generate(
            &fixture,
            &config(),
            vec![],
            "thread",
            "medium",
            vec![],
            |_| Box::pin(async { Err("cancelled".into()) })
        )
        .await
        .is_err()
    );
    assert_eq!(fixture.requests.lock().unwrap().len(), 1);
}
