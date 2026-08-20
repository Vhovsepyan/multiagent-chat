//! Clients for the two model APIs.
//!
//! Both clients accept the same `Message` type and convert it to their own wire
//! format, because the two APIs disagree on names: Anthropic calls the model's
//! turn "assistant", Google calls it "model". Keeping one shared type here
//! means the debate loop never has to care which model it is talking to.

// The debate loop starts using these in Phase 3.
#![allow(dead_code)]

pub mod claude;
pub mod gemini;

use std::time::Duration;

/// Who said a given message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// One turn of a conversation, in a form both clients understand.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    /// `impl Into<String>` accepts both `&str` and `String`, so callers can
    /// pass a literal without writing `.to_string()` every time.
    pub fn user(content: impl Into<String>) -> Self {
        Message {
            role: Role::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Message {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Retry policy (DP-4)
// ---------------------------------------------------------------------------

/// How many times one API call is attempted in total.
///
/// Five rather than three because live runs showed Gemini returning 503
/// "experiencing high demand" in bursts, and its own 429 bodies ask for a
/// ~9 second wait — a 1s/2s pair of retries gives up well before the provider
/// expects you to.
pub const MAX_ATTEMPTS: u32 = 5;

/// How long to wait before the next attempt: 1s, 2s, 4s, then 8s.
///
/// `attempt` is 1-based, so this doubles each time. Backing off matters for a
/// 429: hammering a rate limit immediately just earns another one.
pub fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(1u64 << (attempt - 1))
}

/// Which HTTP failures are worth trying again.
///
/// 429 (rate limited) and 5xx (the provider is having a bad time) usually clear
/// on their own. A 400 or 401 means the request or the key is wrong, and will
/// fail identically forever — retrying only wastes time.
pub fn is_retryable(status: reqwest::StatusCode) -> bool {
    status.as_u16() == 429 || status.is_server_error()
}

/// One failed API attempt, tagged with whether trying again could help.
pub struct Failure {
    pub error: anyhow::Error,
    pub retryable: bool,
}

impl Failure {
    /// Worth another attempt: rate limits, provider outages, timeouts.
    pub fn transient(error: anyhow::Error) -> Self {
        Failure {
            error,
            retryable: true,
        }
    }

    /// Will fail the same way every time: bad key, bad request, bad model name.
    pub fn permanent(error: anyhow::Error) -> Self {
        Failure {
            error,
            retryable: false,
        }
    }
}

/// Add a user message, merging into the previous one if it was also a user
/// message.
///
/// Both APIs require the conversation to alternate user/assistant. A debate
/// transcript ends on the Critic's review, which is a *user* message from the
/// Proposer's point of view, so asking it one more question would otherwise
/// produce two user messages in a row and a 400 from the API.
pub fn push_user(messages: &mut Vec<Message>, text: impl Into<String>) {
    let text = text.into();
    match messages.last_mut() {
        Some(last) if last.role == Role::User => {
            last.content.push_str("\n\n");
            last.content.push_str(&text);
        }
        _ => messages.push(Message::user(text)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_into_a_trailing_user_message() {
        let mut messages = vec![Message::user("first")];
        push_user(&mut messages, "second");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "first\n\nsecond");
    }

    #[test]
    fn appends_after_an_assistant_message() {
        let mut messages = vec![Message::user("q"), Message::assistant("a")];
        push_user(&mut messages, "next");

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2].role, Role::User);
        assert_eq!(messages[2].content, "next");
    }

    #[test]
    fn backoff_doubles_each_attempt() {
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(2), Duration::from_secs(2));
        assert_eq!(backoff(3), Duration::from_secs(4));
        assert_eq!(backoff(4), Duration::from_secs(8));
    }

    /// The provider's own 429 bodies ask for roughly nine seconds. Whatever
    /// MAX_ATTEMPTS is, the total wait has to comfortably exceed that.
    #[test]
    fn total_backoff_outlasts_a_rate_limit_window() {
        let total: u64 = (1..MAX_ATTEMPTS).map(|a| backoff(a).as_secs()).sum();
        assert!(total >= 10, "total backoff was only {total}s");
    }

    #[test]
    fn rate_limits_and_server_errors_are_retryable() {
        use reqwest::StatusCode;

        assert!(is_retryable(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(is_retryable(StatusCode::SERVICE_UNAVAILABLE));
        assert!(is_retryable(StatusCode::BAD_GATEWAY));
    }

    /// A bad key or a malformed request fails identically forever — waiting
    /// four seconds to find that out twice more helps nobody.
    #[test]
    fn client_errors_are_not_retryable() {
        use reqwest::StatusCode;

        assert!(!is_retryable(StatusCode::UNAUTHORIZED));
        assert!(!is_retryable(StatusCode::BAD_REQUEST));
        assert!(!is_retryable(StatusCode::FORBIDDEN));
        assert!(!is_retryable(StatusCode::NOT_FOUND));
    }

    #[test]
    fn appends_to_an_empty_conversation() {
        let mut messages = Vec::new();
        push_user(&mut messages, "hello");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::User);
    }
}
