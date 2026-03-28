use anyhow::{anyhow, Result};
use eventsource_stream::Eventsource;
use futures_util::{Stream, StreamExt};
use reqwest::Client;
use serde::Deserialize;

/// Events emitted by the krabs-server SSE stream.
#[derive(Debug)]
pub enum GatewayEvent {
    Delta { text: String },
    Status { text: String },
    Done,
}

/// Typed HTTP client for the krabs-server REST + SSE API.
#[derive(Clone)]
pub struct KrabsServerClient {
    http: Client,
    server_url: String,
    secret_key: Option<String>,
}

impl KrabsServerClient {
    pub fn new(server_url: impl Into<String>, secret_key: Option<String>) -> Self {
        Self {
            http: Client::new(),
            server_url: server_url.into(),
            secret_key,
        }
    }

    fn authed(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.secret_key {
            Some(k) => req.header("X-Secret-Key", k),
            None => req,
        }
    }

    /// Create a new agent. Returns the server-assigned `agent_id`.
    pub async fn create_agent(&self, name: Option<&str>) -> Result<String> {
        let url = format!("{}/api/v1/agents", self.server_url);
        let body = serde_json::json!({ "name": name });
        let resp = self.authed(self.http.post(&url)).json(&body).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("create_agent failed {}: {}", status, text));
        }

        #[derive(Deserialize)]
        struct CreateResponse {
            agent_id: String,
        }
        let r: CreateResponse = resp.json().await?;
        Ok(r.agent_id)
    }

    /// POST a message to an agent; returns an SSE event stream.
    pub async fn chat(
        &self,
        agent_id: &str,
        message: &str,
    ) -> Result<impl Stream<Item = Result<GatewayEvent>>> {
        let url = format!("{}/api/v1/agents/{}/chat", self.server_url, agent_id);
        let resp = self
            .authed(self.http.post(&url))
            .json(&serde_json::json!({ "message": message }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("chat failed {}: {}", status, text));
        }

        let stream = resp.bytes_stream().eventsource().map(|result| {
            let event = result.map_err(|e| anyhow!("SSE stream error: {}", e))?;
            parse_event(&event.event, &event.data)
        });

        Ok(stream)
    }

    /// Cancel an in-flight chat request.
    pub async fn cancel(&self, agent_id: &str) -> Result<()> {
        let url = format!("{}/api/v1/agents/{}/chat", self.server_url, agent_id);
        self.http.delete(&url).send().await?;
        Ok(())
    }
}

fn parse_event(event_type: &str, data: &str) -> Result<GatewayEvent> {
    match event_type {
        "delta" => {
            #[derive(Deserialize)]
            struct D {
                text: String,
            }
            let d: D = serde_json::from_str(data)?;
            Ok(GatewayEvent::Delta { text: d.text })
        }
        "status" => {
            #[derive(Deserialize)]
            struct S {
                text: String,
            }
            let s: S = serde_json::from_str(data)?;
            Ok(GatewayEvent::Status { text: s.text })
        }
        // "usage" and "done" both signal end-of-stream
        _ => Ok(GatewayEvent::Done),
    }
}
