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
    fn appends_to_an_empty_conversation() {
        let mut messages = Vec::new();
        push_user(&mut messages, "hello");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::User);
    }
}
