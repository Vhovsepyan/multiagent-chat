//! Anthropic Messages API client — this is the Critic.
//!
//! Endpoint and headers come from the Anthropic docs:
//!   POST https://api.anthropic.com/v1/messages
//!   x-api-key: <key>
//!   anthropic-version: 2023-06-01

// The debate loop wires this up in Phase 3; until then only the tests call it.
#![allow(dead_code)]

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::api::{Message, Role};
use crate::config::Config;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";

/// Upper bound on one reply. Generous enough for a long critique, small enough
/// that a runaway response cannot cost a fortune.
const MAX_TOKENS: u32 = 16_000;

/// Give up on a single call after this long.
const TIMEOUT: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------
// Wire types: exactly the JSON the API sends and receives
// ---------------------------------------------------------------------------

/// How Anthropic spells each role.
fn wire_role(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

#[derive(Debug, Serialize)]
struct WireMessage<'a> {
    role: &'static str,
    content: &'a str,
}

/// The request body. `<'a>` is a lifetime: this struct only *borrows* the model
/// name and the message text, so building it copies nothing.
#[derive(Debug, Serialize)]
struct Request<'a> {
    model: &'a str,
    max_tokens: u32,
    /// The system prompt is a top-level field, not a message.
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    messages: Vec<WireMessage<'a>>,
}

/// The success response. We only name the fields we actually use; serde
/// ignores everything else in the JSON.
#[derive(Debug, Deserialize)]
struct Response {
    content: Vec<ContentBlock>,
    stop_reason: Option<String>,
}

/// `content` is an array of blocks. Text blocks carry `"type": "text"`; other
/// kinds (thinking, tool use) can appear, so we keep the kind and skip them.
#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

/// The error body, e.g.
/// `{"type":"error","error":{"type":"authentication_error","message":"..."}}`
#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: ErrorBody,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    #[serde(rename = "type")]
    kind: String,
    message: String,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Talks to the Anthropic API. Holds one reusable HTTP client, because
/// `reqwest::Client` pools connections internally.
pub struct ClaudeClient {
    http: reqwest::Client,
    api_key: String,
    model: String,
}

impl ClaudeClient {
    pub fn new(config: &Config) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .context("could not build the HTTP client")?;

        Ok(ClaudeClient {
            http,
            api_key: config.anthropic_api_key.clone(),
            model: config.critic_model.clone(),
        })
    }

    /// Send the conversation and return the reply as plain text.
    pub async fn send(&self, system: Option<&str>, messages: &[Message]) -> Result<String> {
        if messages.is_empty() {
            bail!("cannot send an empty conversation");
        }

        let body = Request {
            model: &self.model,
            max_tokens: MAX_TOKENS,
            system,
            messages: messages
                .iter()
                .map(|m| WireMessage {
                    role: wire_role(m.role),
                    content: &m.content,
                })
                .collect(),
        };

        let response = self
            .http
            .post(API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&body)
            .send()
            .await
            .context("request to the Anthropic API failed")?;

        let status = response.status();
        let raw = response
            .text()
            .await
            .context("could not read the Anthropic API response")?;

        if !status.is_success() {
            // Prefer the API's own error message, fall back to the raw body.
            // Never include the request headers here — they hold the key.
            match serde_json::from_str::<ErrorResponse>(&raw) {
                Ok(e) => bail!(
                    "Anthropic API {} ({}): {}",
                    status,
                    e.error.kind,
                    e.error.message
                ),
                Err(_) => bail!("Anthropic API {}: {}", status, raw.trim()),
            };
        }

        let parsed: Response =
            serde_json::from_str(&raw).context("could not parse the Anthropic response as JSON")?;

        text_of(&parsed)
    }
}

/// Join every text block into one string.
fn text_of(response: &Response) -> Result<String> {
    let text = response
        .content
        .iter()
        .filter(|b| b.kind == "text")
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("");

    if text.trim().is_empty() {
        bail!(
            "the model returned no text (stop_reason: {})",
            response.stop_reason.as_deref().unwrap_or("none")
        );
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_for(system: Option<&'static str>, messages: &[Message]) -> serde_json::Value {
        let body = Request {
            model: "claude-sonnet-4-6",
            max_tokens: MAX_TOKENS,
            system,
            messages: messages
                .iter()
                .map(|m| WireMessage {
                    role: wire_role(m.role),
                    content: &m.content,
                })
                .collect(),
        };
        serde_json::to_value(&body).unwrap()
    }

    #[test]
    fn request_matches_the_documented_shape() {
        let json = body_for(Some("be brief"), &[Message::user("hi")]);

        assert_eq!(json["model"], "claude-sonnet-4-6");
        assert_eq!(json["max_tokens"], 16_000);
        assert_eq!(json["system"], "be brief");
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "hi");
    }

    #[test]
    fn assistant_role_is_named_the_anthropic_way() {
        let json = body_for(None, &[Message::assistant("ok")]);
        assert_eq!(json["messages"][0]["role"], "assistant");
    }

    #[test]
    fn system_is_omitted_when_absent() {
        let json = body_for(None, &[Message::user("hi")]);
        assert!(json.get("system").is_none());
    }

    #[test]
    fn parses_text_and_skips_other_blocks() {
        let raw = r#"{
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "hmm"},
                {"type": "text", "text": "pong"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 5, "output_tokens": 2}
        }"#;
        let parsed: Response = serde_json::from_str(raw).unwrap();
        assert_eq!(text_of(&parsed).unwrap(), "pong");
    }

    #[test]
    fn empty_text_is_an_error() {
        let raw = r#"{"content": [], "stop_reason": "max_tokens"}"#;
        let parsed: Response = serde_json::from_str(raw).unwrap();
        let err = text_of(&parsed).unwrap_err().to_string();
        assert!(err.contains("max_tokens"), "unexpected message: {err}");
    }

    /// This is the exact body a real 401 returns — captured from the live API.
    #[test]
    fn parses_an_error_body() {
        let raw = r#"{"type":"error","error":{"type":"authentication_error","message":"API key is invalid."},"request_id":null}"#;
        let parsed: ErrorResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.error.kind, "authentication_error");
        assert_eq!(parsed.error.message, "API key is invalid.");
    }

    /// Hits the real API and costs a fraction of a cent. Not run by default.
    ///
    ///   cargo test -- --ignored live_pong --nocapture
    #[tokio::test]
    #[ignore = "hits the real Anthropic API; run with: cargo test -- --ignored"]
    async fn live_pong() {
        let config = Config::load().expect("config should load from .env");
        let client = ClaudeClient::new(&config).expect("client should build");

        let reply = client
            .send(
                Some("Answer with exactly one word, lowercase, no punctuation."),
                &[Message::user("Say pong")],
            )
            .await
            .expect("the API call should succeed");

        println!("model replied: {reply:?}");
        assert!(
            reply.to_lowercase().contains("pong"),
            "expected 'pong', got: {reply:?}"
        );
    }
}
