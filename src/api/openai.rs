//! OpenAI Responses API adapter for the proposer and critic roles.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::api::{Failure, MAX_ATTEMPTS, Message, Role, backoff, is_retryable};
use crate::config::Config;
use crate::ui;

const TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Serialize)]
struct Request<'a> {
    model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<&'a str>,
    input: Vec<InputMessage<'a>>,
    // The application stores its own redacted audit evidence. Do not ask the
    // provider to retain a second server-side conversation by default.
    store: bool,
}

#[derive(Debug, Serialize)]
struct InputMessage<'a> {
    role: &'static str,
    content: Vec<InputText<'a>>,
}

#[derive(Debug, Serialize)]
struct InputText<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    text: &'a str,
}

#[derive(Debug, Deserialize)]
struct Response {
    #[serde(default)]
    output: Vec<OutputItem>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    error: Option<ResponseError>,
}

#[derive(Debug, Deserialize)]
struct OutputItem {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    content: Vec<OutputContent>,
}

#[derive(Debug, Deserialize)]
struct OutputContent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    error: ResponseError,
}

#[derive(Debug, Deserialize)]
struct ResponseError {
    #[serde(rename = "type", default)]
    kind: String,
    message: String,
}

pub struct OpenAiClient {
    http: reqwest::Client,
    api_key: String,
    api_url: String,
    model: String,
}

impl OpenAiClient {
    pub fn new(config: &Config, model: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .context("could not build the HTTP client")?;
        let api_key = config.openai_api_key.clone().context(
            "OPENAI_API_KEY is not set — the OpenAI provider is not configured in this installation",
        )?;
        Ok(Self {
            http,
            api_key,
            api_url: format!("{}/responses", config.openai_base_url.trim_end_matches('/')),
            model: model.to_string(),
        })
    }

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
        unreachable!("the retry loop always returns")
    }

    async fn send_once(
        &self,
        system: Option<&str>,
        messages: &[Message],
    ) -> std::result::Result<String, Failure> {
        let body = Request {
            model: &self.model,
            instructions: system,
            input: messages
                .iter()
                .map(|message| InputMessage {
                    role: match message.role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                    },
                    content: vec![InputText {
                        kind: "input_text",
                        text: &message.content,
                    }],
                })
                .collect(),
            store: false,
        };
        let response = self
            .http
            .post(&self.api_url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                Failure::transient(
                    anyhow::Error::new(error).context("request to the OpenAI API failed"),
                )
            })?;
        let status = response.status();
        let raw = response.text().await.map_err(|error| {
            Failure::transient(
                anyhow::Error::new(error).context("could not read the OpenAI API response"),
            )
        })?;
        if !status.is_success() {
            let message = match serde_json::from_str::<ErrorEnvelope>(&raw) {
                Ok(error) => anyhow!(
                    "OpenAI API {} ({}): {}",
                    status,
                    error.error.kind,
                    self.redact_api_key(&error.error.message)
                ),
                Err(_) => anyhow!(
                    "OpenAI API {} returned an unparseable error response",
                    status
                ),
            };
            return Err(if is_retryable(status) {
                Failure::transient(message)
            } else {
                Failure::permanent(message)
            });
        }
        let response = serde_json::from_str::<Response>(&raw).map_err(|error| {
            Failure::permanent(
                anyhow::Error::new(error).context("could not parse the OpenAI response as JSON"),
            )
        })?;
        text_of(&response)
            .map_err(|error| Failure::permanent(anyhow!(self.redact_api_key(&error.to_string()))))
    }

    fn redact_api_key(&self, value: &str) -> String {
        value.replace(&self.api_key, "[REDACTED]")
    }
}

fn text_of(response: &Response) -> Result<String> {
    if let Some(error) = &response.error {
        bail!("OpenAI response failed ({}): {}", error.kind, error.message);
    }
    let text = response
        .output
        .iter()
        .filter(|item| item.kind == "message")
        .flat_map(|item| item.content.iter())
        .filter(|content| content.kind == "output_text")
        .map(|content| content.text.as_str())
        .collect::<Vec<_>>()
        .join("");
    if text.trim().is_empty() {
        bail!(
            "OpenAI returned no text (status: {})",
            response.status.as_deref().unwrap_or("unknown")
        );
    }
    Ok(text)
}

#[async_trait::async_trait]
impl crate::agent::ChatAgent for OpenAiClient {
    fn provider(&self) -> crate::agent::ChatProvider {
        crate::agent::ChatProvider::OpenAI
    }
    fn model(&self) -> &str {
        &self.model
    }
    async fn complete(
        &self,
        request: crate::agent::ChatRequest<'_>,
    ) -> Result<crate::agent::ChatResponse> {
        Ok(crate::agent::ChatResponse {
            text: self.send(request.system, request.messages).await?,
            provider: crate::agent::ChatProvider::OpenAI,
            model: self.model.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc::{self, Receiver};
    use std::thread;

    use super::*;
    use crate::agent::{ChatAgent, ChatProvider, ChatRequest};

    const TEST_API_KEY: &str = "test-openai-api-key";

    #[derive(Debug)]
    struct CapturedRequest {
        method: String,
        target: String,
        has_bearer_authorization: bool,
        body: serde_json::Value,
    }

    fn mock_server(status: u16, response: &'static str) -> (String, Receiver<CapturedRequest>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut headers = Vec::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                headers.push(line);
            }
            let content_length = headers
                .iter()
                .find_map(|line| line.strip_prefix("content-length: "))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap();
            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).unwrap();
            let mut parts = request_line.split_whitespace();
            sender
                .send(CapturedRequest {
                    method: parts.next().unwrap().to_string(),
                    target: parts.next().unwrap().to_string(),
                    has_bearer_authorization: headers.iter().any(|line| {
                        line.starts_with("authorization: Bearer ")
                            && line.trim().len() > "authorization: Bearer".len()
                    }),
                    body: serde_json::from_slice(&body).unwrap(),
                })
                .unwrap();
            let response = format!(
                "HTTP/1.1 {status} test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            );
            reader.get_mut().write_all(response.as_bytes()).unwrap();
        });
        (format!("http://{address}/v1"), receiver)
    }

    fn client_for(base_url: String) -> OpenAiClient {
        let mut config = crate::agent::test_config();
        config.openai_api_key = Some(TEST_API_KEY.into());
        config.openai_base_url = base_url;
        OpenAiClient::new(&config, "gpt-5.6-sol").unwrap()
    }

    #[test]
    fn request_uses_responses_input_and_never_requests_provider_storage() {
        let body = Request {
            model: "gpt-5.6-sol",
            instructions: Some("be concise"),
            input: vec![InputMessage {
                role: "user",
                content: vec![InputText {
                    kind: "input_text",
                    text: "hello",
                }],
            }],
            store: false,
        };
        let json = serde_json::to_value(body).unwrap();
        assert_eq!(json["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(json["store"], false);
    }

    #[test]
    fn parses_output_text_and_refuses_empty_or_error_responses() {
        let response: Response = serde_json::from_str(r#"{"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"pong"}]}]}"#).unwrap();
        assert_eq!(text_of(&response).unwrap(), "pong");
        let error: Response = serde_json::from_str(
            r#"{"status":"failed","error":{"type":"invalid_request_error","message":"bad model"}}"#,
        )
        .unwrap();
        assert!(
            text_of(&error)
                .unwrap_err()
                .to_string()
                .contains("bad model")
        );
    }

    #[test]
    fn provider_errors_cannot_echo_the_configured_api_key() {
        let client = OpenAiClient {
            http: reqwest::Client::new(),
            api_key: "openai-secret".into(),
            api_url: "https://api.openai.com/v1/responses".into(),
            model: "gpt-5.6-sol".into(),
        };
        assert_eq!(
            client.redact_api_key("request rejected: openai-secret"),
            "request rejected: [REDACTED]"
        );
    }

    #[tokio::test]
    async fn responses_api_request_and_chat_response_use_the_local_server_contract() {
        let (base_url, requests) = mock_server(
            200,
            r#"{"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"local response"}]}]}"#,
        );
        let client = client_for(base_url);
        let messages = [Message::user("first"), Message::assistant("second")];

        let response = client
            .complete(ChatRequest::new(
                Some("follow the system instruction"),
                &messages,
            ))
            .await
            .unwrap();

        assert_eq!(response.text, "local response");
        assert_eq!(response.provider, ChatProvider::OpenAI);
        assert_eq!(response.model, "gpt-5.6-sol");
        let request = requests.recv().unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.target, "/v1/responses");
        assert!(request.has_bearer_authorization);
        assert_eq!(request.body["model"], "gpt-5.6-sol");
        assert_eq!(
            request.body["instructions"],
            "follow the system instruction"
        );
        assert_eq!(request.body["store"], false);
        assert_eq!(request.body["input"][0]["role"], "user");
        assert_eq!(request.body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(request.body["input"][0]["content"][0]["text"], "first");
        assert_eq!(request.body["input"][1]["role"], "assistant");
        assert_eq!(request.body["input"][1]["content"][0]["text"], "second");
    }

    #[tokio::test]
    async fn local_api_failures_are_safely_reported_without_the_api_key() {
        let (base_url, requests) = mock_server(
            400,
            r#"{"error":{"type":"invalid_request_error","message":"model unavailable for test-openai-api-key"}}"#,
        );
        let client = client_for(base_url);

        let error = client
            .complete_text(None, &[Message::user("hello")])
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("OpenAI API 400"));
        assert!(error.contains("invalid_request_error"));
        assert!(error.contains("[REDACTED]"));
        assert!(!error.contains(TEST_API_KEY));
        assert!(requests.recv().unwrap().has_bearer_authorization);
    }
}
