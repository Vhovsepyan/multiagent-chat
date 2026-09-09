//! The conversational agent abstraction, used by the Proposer and the Critic.
//!
//! A `ChatAgent` is anything that can be handed a conversation and return text.
//! Retries, wire formats, role names and error parsing stay inside the provider
//! adapters in `api/`; this trait is all the debate loop is allowed to know.

use anyhow::Result;
use async_trait::async_trait;

use crate::agent::ChatProvider;
use crate::api::Message;

/// One request to a chat agent.
///
/// `system` is the role instruction; `messages` is the conversation so far,
/// already alternating user/assistant as both APIs require.
#[derive(Debug, Clone, Copy)]
pub struct ChatRequest<'a> {
    pub system: Option<&'a str>,
    pub messages: &'a [Message],
}

impl<'a> ChatRequest<'a> {
    pub fn new(system: Option<&'a str>, messages: &'a [Message]) -> Self {
        ChatRequest { system, messages }
    }
}

/// What an agent replied, plus who replied. The provenance fields matter for
/// later evidence/audit work; today only `text` is read.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub text: String,
    pub provider: ChatProvider,
    pub model: String,
}

/// A conversational agent.
///
/// `#[async_trait]` rewrites the `async fn` into one returning a boxed future,
/// which is what makes `&dyn ChatAgent` legal — a plain `async fn` in a trait
/// cannot be called through a trait object. One heap allocation per API call is
/// nothing next to the HTTP round trip it wraps.
#[async_trait]
pub trait ChatAgent: Send + Sync {
    /// Who is behind this agent.
    fn provider(&self) -> ChatProvider;

    /// The model this agent will answer with.
    fn model(&self) -> &str;

    /// Send the conversation and return the reply.
    async fn complete(&self, request: ChatRequest<'_>) -> Result<ChatResponse>;

    /// Convenience for callers that want only the text, which is every caller
    /// today. Provided here so no stage has to build a `ChatRequest` by hand.
    async fn complete_text(&self, system: Option<&str>, messages: &[Message]) -> Result<String> {
        Ok(self
            .complete(ChatRequest::new(system, messages))
            .await?
            .text)
    }
}

// ---------------------------------------------------------------------------
// Test double
// ---------------------------------------------------------------------------

/// A `ChatAgent` that replays canned replies and records what it was asked.
///
/// This is the point of the abstraction: `debate.rs` and `spec.rs` can now be
/// tested end to end with no API key and no network.
#[cfg(test)]
pub struct ScriptedAgent {
    provider: ChatProvider,
    model: String,
    replies: std::sync::Mutex<std::collections::VecDeque<String>>,
    seen: std::sync::Mutex<Vec<(Option<String>, Vec<Message>)>>,
}

#[cfg(test)]
impl ScriptedAgent {
    pub fn new(provider: ChatProvider, replies: &[&str]) -> Self {
        ScriptedAgent {
            provider,
            model: format!("{provider}-test"),
            replies: std::sync::Mutex::new(replies.iter().map(|r| r.to_string()).collect()),
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// How many times this agent was called.
    pub fn calls(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    /// The system prompt and conversation of one recorded call.
    pub fn call(&self, index: usize) -> (Option<String>, Vec<Message>) {
        self.seen.lock().unwrap()[index].clone()
    }
}

#[cfg(test)]
#[async_trait]
impl ChatAgent for ScriptedAgent {
    fn provider(&self) -> ChatProvider {
        self.provider
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(&self, request: ChatRequest<'_>) -> Result<ChatResponse> {
        self.seen.lock().unwrap().push((
            request.system.map(|s| s.to_string()),
            request.messages.to_vec(),
        ));
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("the scripted agent ran out of replies"))?;
        Ok(ChatResponse {
            text: reply,
            provider: self.provider,
            model: self.model.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_scripted_agent_answers_in_order_and_reports_itself() {
        let agent = ScriptedAgent::new(ChatProvider::Gemini, &["first", "second"]);
        let messages = vec![Message::user("hello")];

        let response = agent
            .complete(ChatRequest::new(Some("be brief"), &messages))
            .await
            .unwrap();

        assert_eq!(response.text, "first");
        assert_eq!(response.provider, ChatProvider::Gemini);
        assert_eq!(response.model, agent.model());

        assert_eq!(
            agent.complete_text(None, &messages).await.unwrap(),
            "second"
        );
        assert_eq!(agent.calls(), 2);
        assert_eq!(agent.call(0).0.as_deref(), Some("be brief"));
    }

    /// `complete_text` must go through `complete`, not around it.
    #[tokio::test]
    async fn the_text_helper_records_the_same_call() {
        let agent = ScriptedAgent::new(ChatProvider::Anthropic, &["ok"]);
        let messages = vec![Message::user("q")];

        agent
            .complete_text(Some("system"), &messages)
            .await
            .unwrap();

        let (system, seen) = agent.call(0);
        assert_eq!(system.as_deref(), Some("system"));
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].content, "q");
    }

    #[tokio::test]
    async fn an_exhausted_script_fails_rather_than_inventing_an_answer() {
        let agent = ScriptedAgent::new(ChatProvider::Gemini, &[]);

        let error = agent
            .complete_text(None, &[Message::user("q")])
            .await
            .unwrap_err();

        assert!(error.to_string().contains("ran out of replies"));
    }
}
