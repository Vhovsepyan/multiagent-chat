//! Google Gemini client — this is the Proposer.
//!
//! Endpoint and headers come from the Gemini API docs:
//!   POST https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent
//!   x-goog-api-key: <key>
//!
//! The docs also allow `?key=<key>` in the URL, but a header keeps the key out
//! of proxy logs and crash dumps, so we use the header.
//!
//! Two differences from the Anthropic shape are worth knowing:
//!   - a turn is a `Content` holding `parts`, not a single `content` string;
//!   - the model's own turn is called "model", not "assistant".

// The debate loop wires this up in Phase 3; until then only the tests call it.
#![allow(dead_code)]

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::api::{Failure, MAX_ATTEMPTS, Message, Role, backoff, is_retryable};
use crate::config::Config;
use crate::ui;

const API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";

/// Upper bound on one reply. On thinking-capable Gemini models this budget
/// covers reasoning as well as the visible answer, so keep it generous.
const MAX_OUTPUT_TOKENS: u32 = 16_000;

/// Give up on a single call after this long.
const TIMEOUT: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// How Google spells each role.
fn wire_role(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "model",
    }
}

/// A chunk of text. Gemini allows images and other kinds here too; we only
/// ever send and read text.
#[derive(Debug, Serialize, Deserialize)]
struct Part<'a> {
    #[serde(borrow)]
    text: std::borrow::Cow<'a, str>,
}

/// One turn. `role` is absent on the system instruction, hence the `Option`.
#[derive(Debug, Serialize)]
struct Content<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<&'static str>,
    parts: Vec<Part<'a>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationConfig {
    max_output_tokens: u32,
}

/// The request body. Note `camelCase`: Google uses `systemInstruction`, not
/// `system_instruction`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Request<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<Content<'a>>,
    contents: Vec<Content<'a>>,
    generation_config: GenerationConfig,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Response {
    #[serde(default)]
    candidates: Vec<Candidate>,
    #[serde(default)]
    prompt_feedback: Option<PromptFeedback>,
}

#[derive(Debug, Deserialize)]
struct Candidate {
    #[serde(default)]
    content: Option<CandidateContent>,
    #[serde(default, rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CandidateContent {
    #[serde(default)]
    parts: Vec<ResponsePart>,
}

/// Owned twin of `Part`, used when reading a response.
#[derive(Debug, Deserialize)]
struct ResponsePart {
    #[serde(default)]
    text: String,
}

/// Present when the prompt itself was refused.
#[derive(Debug, Deserialize)]
struct PromptFeedback {
    #[serde(rename = "blockReason")]
    block_reason: Option<String>,
}

/// The error body, e.g.
/// `{"error":{"code":400,"message":"...","status":"INVALID_ARGUMENT"}}`
#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: ErrorBody,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    #[serde(default)]
    status: String,
    message: String,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Talks to the Gemini API.
pub struct GeminiClient {
    http: reqwest::Client,
    api_key: String,
    model: String,
}

impl GeminiClient {
    pub fn new(config: &Config) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .context("could not build the HTTP client")?;

        Ok(GeminiClient {
            http,
            api_key: config.gemini_api_key.clone(),
            model: config.gemini_model.clone(),
        })
    }

    /// Send the conversation and return the reply as plain text.
    ///
    /// Retries rate limits, provider errors and timeouts with backoff (DP-4);
    /// anything else fails on the first attempt.
    pub async fn send(&self, system: Option<&str>, messages: &[Message]) -> Result<String> {
        if messages.is_empty() {
            bail!("cannot send an empty conversation");
        }

        for attempt in 1..=MAX_ATTEMPTS {
            match self.send_once(system, messages).await {
                Ok(text) => return Ok(text),
                Err(failure) if failure.retryable && attempt < MAX_ATTEMPTS => {
                    let wait = backoff(attempt);
                    ui::warn(&format!(
                        "{} — retrying in {}s ({}/{})",
                        failure.error,
                        wait.as_secs(),
                        attempt,
                        MAX_ATTEMPTS - 1
                    ));
                    tokio::time::sleep(wait).await;
                }
                Err(failure) => return Err(failure.error),
            }
        }
        unreachable!("the loop either returns or exhausts MAX_ATTEMPTS")
    }

    /// One attempt, with no retry logic.
    async fn send_once(
        &self,
        system: Option<&str>,
        messages: &[Message],
    ) -> std::result::Result<String, Failure> {
        let body = Request {
            system_instruction: system.map(|text| Content {
                role: None,
                parts: vec![Part { text: text.into() }],
            }),
            contents: messages
                .iter()
                .map(|m| Content {
                    role: Some(wire_role(m.role)),
                    parts: vec![Part {
                        text: m.content.as_str().into(),
                    }],
                })
                .collect(),
            generation_config: GenerationConfig {
                max_output_tokens: MAX_OUTPUT_TOKENS,
            },
        };

        let url = format!("{API_BASE}/{}:generateContent", self.model);

        // A transport failure (timeout, DNS, connection reset) is worth another
        // attempt.
        let response = self
            .http
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                Failure::transient(
                    anyhow::Error::new(e).context("request to the Gemini API failed"),
                )
            })?;

        let status = response.status();
        let raw = response.text().await.map_err(|e| {
            Failure::transient(
                anyhow::Error::new(e).context("could not read the Gemini API response"),
            )
        })?;

        if !status.is_success() {
            // Never include the request headers here — they hold the key.
            let message = match serde_json::from_str::<ErrorResponse>(&raw) {
                Ok(e) => anyhow!(
                    "Gemini API {} ({}): {}",
                    status,
                    e.error.status,
                    e.error.message
                ),
                Err(_) => anyhow!("Gemini API {}: {}", status, raw.trim()),
            };
            return Err(if is_retryable(status) {
                Failure::transient(message)
            } else {
                Failure::permanent(message)
            });
        }

        let parsed: Response = serde_json::from_str(&raw).map_err(|e| {
            Failure::permanent(
                anyhow::Error::new(e).context("could not parse the Gemini response as JSON"),
            )
        })?;

        text_of(&parsed).map_err(Failure::permanent)
    }
}

/// Pull the text out of the first candidate.
fn text_of(response: &Response) -> Result<String> {
    if let Some(feedback) = &response.prompt_feedback
        && let Some(reason) = &feedback.block_reason
    {
        bail!("Gemini refused the prompt (blockReason: {reason})");
    }

    let Some(candidate) = response.candidates.first() else {
        bail!("Gemini returned no candidates");
    };

    let text = candidate
        .content
        .as_ref()
        .map(|c| {
            c.parts
                .iter()
                .map(|p| p.text.as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();

    if text.trim().is_empty() {
        bail!(
            "Gemini returned no text (finishReason: {})",
            candidate.finish_reason.as_deref().unwrap_or("none")
        );
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_for(system: Option<&str>, messages: &[Message]) -> serde_json::Value {
        let body = Request {
            system_instruction: system.map(|text| Content {
                role: None,
                parts: vec![Part { text: text.into() }],
            }),
            contents: messages
                .iter()
                .map(|m| Content {
                    role: Some(wire_role(m.role)),
                    parts: vec![Part {
                        text: m.content.as_str().into(),
                    }],
                })
                .collect(),
            generation_config: GenerationConfig {
                max_output_tokens: MAX_OUTPUT_TOKENS,
            },
        };
        serde_json::to_value(&body).unwrap()
    }

    #[test]
    fn request_matches_the_documented_shape() {
        let json = body_for(Some("be brief"), &[Message::user("hi")]);

        assert_eq!(json["systemInstruction"]["parts"][0]["text"], "be brief");
        assert!(json["systemInstruction"].get("role").is_none());
        assert_eq!(json["contents"][0]["role"], "user");
        assert_eq!(json["contents"][0]["parts"][0]["text"], "hi");
        assert_eq!(json["generationConfig"]["maxOutputTokens"], 16_000);
    }

    /// The whole reason `Message` lives in `api/mod.rs`: Google says "model"
    /// where Anthropic says "assistant".
    #[test]
    fn assistant_role_is_named_the_google_way() {
        let json = body_for(None, &[Message::assistant("ok")]);
        assert_eq!(json["contents"][0]["role"], "model");
        assert!(json.get("systemInstruction").is_none());
    }

    #[test]
    fn parses_a_normal_response() {
        let raw = r#"{
            "candidates": [{
                "content": {"parts": [{"text": "here is "}, {"text": "a plan"}], "role": "model"},
                "finishReason": "STOP"
            }],
            "usageMetadata": {"promptTokenCount": 5, "candidatesTokenCount": 3}
        }"#;
        let parsed: Response = serde_json::from_str(raw).unwrap();
        assert_eq!(text_of(&parsed).unwrap(), "here is a plan");
    }

    #[test]
    fn reports_a_blocked_prompt() {
        let raw = r#"{"promptFeedback": {"blockReason": "SAFETY"}, "candidates": []}"#;
        let parsed: Response = serde_json::from_str(raw).unwrap();
        let err = text_of(&parsed).unwrap_err().to_string();
        assert!(err.contains("SAFETY"), "unexpected message: {err}");
    }

    /// A thinking model can spend the whole budget before writing an answer.
    #[test]
    fn reports_a_truncated_answer() {
        let raw = r#"{"candidates": [{"content": {"parts": []}, "finishReason": "MAX_TOKENS"}]}"#;
        let parsed: Response = serde_json::from_str(raw).unwrap();
        let err = text_of(&parsed).unwrap_err().to_string();
        assert!(err.contains("MAX_TOKENS"), "unexpected message: {err}");
    }

    #[test]
    fn parses_an_error_body() {
        let raw =
            r#"{"error":{"code":400,"message":"API key not valid.","status":"INVALID_ARGUMENT"}}"#;
        let parsed: ErrorResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.error.status, "INVALID_ARGUMENT");
        assert_eq!(parsed.error.message, "API key not valid.");
    }

    /// Hits the real API. Not run by default.
    #[tokio::test]
    #[ignore = "hits the real Gemini API; run with: cargo test -- --ignored"]
    async fn live_pong() {
        let config = Config::load().expect("config should load from .env");
        let client = GeminiClient::new(&config).expect("client should build");

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
